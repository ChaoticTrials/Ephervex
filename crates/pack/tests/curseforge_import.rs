//! A CurseForge export through the shared archive and materialization path.
//!
//! Everything here is offline. A CurseForge pack that declares no files needs
//! no provider at all, which is what lets the *archive* half of the import -
//! overrides, path safety, source retention - be exercised without a key, a
//! network, or a mock API. The provider half is covered where it lives, in
//! `ephervex-core`.
//!
//! The archive is assembled at test time from
//! `fixtures/curseforge/export/`, so every part of it stays reviewable as text;
//! see that directory's `SOURCES.md` for what each file proves.
//!
//! The last test is the one that matters most for what comes after: a
//! normalized CurseForge recipe entering the generic server-reconstruction path
//! with nothing CurseForge-specific anywhere in it.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Write as _;
use std::path::{Path, PathBuf};

use ephervex_download::{
    CacheLayout, ContentStore, DownloadManager, DownloaderConfig, FileExtension, OperationReporter,
};
use ephervex_model::{
    Hash, HashAlgorithm, ImportSourceKind, LoaderKind, LoaderSelection, ModOrigin, ModProvider,
};
use ephervex_pack::{
    ArtifactFetch, ExtractionLimits, MaterializeRequest, OpenArtifacts, OptionalSelection,
    OverrideScope, PackError, PackFileRecipe, PackFormat, PackPolicy, PackRecipe, PackSide,
    ProviderFileIdentity, ProviderRecipeMetadata, RECIPE_VERSION, ServerMaterializeRequest,
    SideRequirement, SourceStore, UnknownServerFiles, analyze, materialize,
    materialize_server_recipe,
};
use tokio_util::sync::CancellationToken;

fn zip_file(path: &Path, entries: &[(&str, &[u8])]) {
    let file = std::fs::File::create(path).expect("create archive");
    let mut writer = zip::ZipWriter::new(file);
    let options: zip::write::FileOptions<'_, ()> =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, body) in entries {
        writer.start_file(*name, options).expect("start entry");
        writer.write_all(body).expect("write entry");
    }
    writer.finish().expect("finish");
}

/// `<repo>/fixtures/curseforge/export`.
fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/curseforge/export")
        .canonicalize()
        .expect("the fixtures directory exists")
}

fn read_fixture(relative: &str) -> Vec<u8> {
    std::fs::read(fixtures().join(relative))
        .unwrap_or_else(|error| panic!("fixture {relative} is readable: {error}"))
}

/// The fixture export's manifest, which declares no files - so the whole import
/// is local.
fn manifest() -> Vec<u8> {
    read_fixture("manifest.json")
}

/// The fixture export as an archive, plus whatever a test adds to it.
fn build_archive(destination: &Path, extra: &[(&str, &[u8])]) {
    let mut entries: Vec<(String, Vec<u8>)> = vec![
        ("manifest.json".to_owned(), manifest()),
        (
            "overrides/config/repro.toml".to_owned(),
            read_fixture("overrides/config/repro.toml"),
        ),
        (
            "overrides/options.txt".to_owned(),
            read_fixture("overrides/options.txt"),
        ),
    ];
    entries.extend(
        extra
            .iter()
            .map(|(name, body)| ((*name).to_owned(), (*body).to_vec())),
    );
    let borrowed: Vec<(&str, &[u8])> = entries
        .iter()
        .map(|(name, body)| (name.as_str(), body.as_slice()))
        .collect();
    zip_file(destination, &borrowed);
}

fn manager() -> DownloadManager {
    DownloadManager::new(DownloaderConfig::default()).expect("manager")
}

async fn import_into(
    archive_path: &Path,
    game: &Path,
    source: &Path,
    cache: &Path,
) -> Result<PackRecipe, PackError> {
    let (archive, analysis) = analyze(archive_path, &PackPolicy::curseforge())?;
    assert_eq!(analysis.format, PackFormat::CurseForgePack);
    let manager = manager();
    let cache = CacheLayout::new(cache);
    let materialized = materialize(MaterializeRequest {
        archive,
        analysis: &analysis,
        side: PackSide::Client,
        selection: &OptionalSelection::none(),
        game_dir: game,
        source_dir: source,
        manager: &manager,
        cache: &cache,
        credentials: &OpenArtifacts,
        reporter: &OperationReporter::silent("curseforge-pack-import"),
        cancel: &CancellationToken::new(),
    })
    .await?;
    Ok(materialized.recipe)
}

#[tokio::test]
async fn a_curseforge_export_lays_its_overrides_down_and_keeps_its_manifest_verbatim() {
    let workspace = tempfile::tempdir().expect("temp dir");
    let archive = workspace.path().join("repro.zip");
    build_archive(&archive, &[]);

    let game = workspace.path().join("instance/minecraft");
    let source = workspace.path().join("instance/.launcher/source");
    let recipe = import_into(&archive, &game, &source, &workspace.path().join("cache"))
        .await
        .expect("import");

    assert_eq!(
        std::fs::read_to_string(game.join("config/repro.toml")).expect("read"),
        "debug = true\n"
    );
    assert!(game.join("options.txt").is_file());

    // The one override layer a CurseForge export has is the common one, so it
    // is retained for a server built later rather than left in the client only.
    assert_eq!(recipe.override_layers.len(), 1);
    let layer = &recipe.override_layers[0];
    assert_eq!(layer.scope, OverrideScope::Common);
    assert_eq!(
        layer.retained_at,
        Some(PathBuf::from("curseforge/overrides"))
    );
    assert!(
        source
            .join("curseforge/overrides/config/repro.toml")
            .is_file(),
        "the layer a server needs survives the archive"
    );

    // The user's own manifest, byte for byte, under its provider's directory.
    assert_eq!(
        std::fs::read(source.join("curseforge/manifest.json")).expect("read"),
        manifest(),
    );
    assert_eq!(
        recipe.provider_metadata.manifest_file,
        Some(PathBuf::from("curseforge/manifest.json"))
    );
    assert_eq!(
        recipe.provider_metadata.kind,
        ImportSourceKind::CurseForgePack
    );
    assert_eq!(recipe.author.as_deref(), Some("A Reporter"));
    assert_eq!(
        recipe.loader,
        LoaderSelection::exact(LoaderKind::NeoForge, "20.4.190")
    );

    // The recipe Ephervex owns is compact machine-owned JSON.
    let stored = std::fs::read_to_string(source.join("recipe.json")).expect("read");
    assert!(!stored.contains('\n'), "{stored}");
    assert_eq!(
        SourceStore::new(&source).read_recipe().expect("reopen"),
        recipe
    );
}

#[tokio::test]
async fn an_override_that_climbs_out_of_the_instance_is_refused_before_anything_is_written() {
    let workspace = tempfile::tempdir().expect("temp dir");
    let archive = workspace.path().join("hostile.zip");
    build_archive(
        &archive,
        &[(
            "overrides/../../../../etc/cron.d/evil",
            b"* * * * * root sh\n",
        )],
    );

    let game = workspace.path().join("instance/minecraft");
    let error = import_into(
        &archive,
        &game,
        &workspace.path().join("instance/.launcher/source"),
        &workspace.path().join("cache"),
    )
    .await
    .expect_err("must refuse");
    assert!(
        matches!(error, PackError::EntryPathUnsafe { .. }),
        "{error:?}"
    );
    assert!(!workspace.path().join("etc").exists());
}

#[tokio::test]
async fn an_override_aimed_at_launcher_metadata_is_refused() {
    let workspace = tempfile::tempdir().expect("temp dir");
    let archive = workspace.path().join("hostile.zip");
    build_archive(&archive, &[("overrides/.launcher/instance.json", b"{}")]);

    let game = workspace.path().join("instance/minecraft");
    let error = import_into(
        &archive,
        &game,
        &workspace.path().join("instance/.launcher/source"),
        &workspace.path().join("cache"),
    )
    .await
    .expect_err("must refuse");
    assert!(
        matches!(error, PackError::EntryReserved { .. }),
        "{error:?}"
    );
}

#[tokio::test]
async fn an_unrelated_zip_carrying_a_manifest_json_is_not_read_as_a_curseforge_pack() {
    let workspace = tempfile::tempdir().expect("temp dir");
    let archive = workspace.path().join("node-thing.zip");
    zip_file(
        &archive,
        &[(
            "manifest.json",
            br#"{"name":"something-else","version":"1.0.0","dependencies":{}}"# as &[u8],
        )],
    );

    let error = analyze(&archive, &PackPolicy::curseforge()).expect_err("must refuse");
    assert!(
        matches!(error, PackError::ManifestInvalid { ref entry, .. } if entry == "manifest.json"),
        "{error:?}"
    );
}

#[test]
fn a_compact_manifest_declaring_more_files_than_the_default_ceiling_is_refused_at_analysis() {
    let ceiling = ExtractionLimits::default().max_declared_files;
    let workspace = tempfile::tempdir().expect("temp dir");
    let archive = workspace.path().join("declares-everything.zip");
    let files = (1..=ceiling + 5)
        .map(|id| format!(r#"{{"projectID":{id},"fileID":{id},"required":true}}"#))
        .collect::<Vec<_>>()
        .join(",");
    let manifest = format!(
        r#"{{"manifestType":"minecraftModpack","manifestVersion":1,"name":"Wide",
            "minecraft":{{"version":"1.20.1","modLoaders":[{{"id":"forge-47.1.0","primary":true}}]}},
            "files":[{files}],"overrides":"overrides"}}"#
    );
    zip_file(&archive, &[("manifest.json", manifest.as_bytes())]);

    let error = analyze(&archive, &PackPolicy::curseforge()).expect_err("must refuse");
    assert!(
        matches!(
            error,
            PackError::LimitExceeded {
                limit: "declared-files",
                reached,
                ..
            } if reached == ceiling + 5
        ),
        "{error:?}"
    );
}

// -- the generic server path ------------------------------------------------

const MOD_BODY: &[u8] = b"a curseforge mod jar";

fn sha1_of(body: &[u8]) -> String {
    use sha1::{Digest as _, Sha1};
    let mut hasher = Sha1::new();
    hasher.update(body);
    hex::encode(hasher.finalize())
}

/// The normalized recipe a CurseForge import produces, written by hand so the
/// generic path is exercised without a provider anywhere in the test.
fn curseforge_recipe() -> PackRecipe {
    PackRecipe {
        recipe_version: RECIPE_VERSION,
        source_kind: ImportSourceKind::CurseForgePack,
        original_name: "Repro Pack".to_owned(),
        original_version: Some("3.1.0".to_owned()),
        author: Some("A Reporter".to_owned()),
        summary: None,
        archive_name: Some("repro.zip".to_owned()),
        minecraft_version: "1.20.1".to_owned(),
        loader: LoaderSelection::exact(LoaderKind::Forge, "47.4.10"),
        files: vec![PackFileRecipe {
            logical_path: PathBuf::from("mods/required-1.0.0.jar"),
            hashes: vec![Hash::new(HashAlgorithm::Sha1, &sha1_of(MOD_BODY)).expect("a digest")],
            downloads: vec![
                "https://mediafilez.forgecdn.net/files/4593/548/required-1.0.0.jar".to_owned(),
            ],
            size: Some(MOD_BODY.len() as u64),
            client: SideRequirement::Required,
            server: SideRequirement::Unknown,
            selected: None,
            origin: ModOrigin::PackDeclared,
            identity: Some(ProviderFileIdentity {
                provider: Some(ModProvider::CurseForge),
                project_id: Some("238222".to_owned()),
                version_id: Some("4593548".to_owned()),
                file_id: Some("4593548".to_owned()),
            }),
            original_identity: None,
            relations: None,
        }],
        override_layers: vec![ephervex_pack::OverrideLayer {
            scope: OverrideScope::Common,
            archive_prefix: "overrides".to_owned(),
            retained_at: Some(PathBuf::from("curseforge/overrides")),
            files: 1,
        }],
        provider_metadata: ProviderRecipeMetadata::new(
            ImportSourceKind::CurseForgePack,
            Some(PathBuf::from("curseforge/manifest.json")),
        ),
    }
}

/// Publishes the artifact into the shared cache so reconstruction is offline.
fn publish_to_cache(cache: &CacheLayout, entry: &PackFileRecipe, body: &[u8]) {
    let extension = entry
        .logical_path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| FileExtension::new(value).expect("extension"));
    let path = cache.content(
        ContentStore::Downloads,
        &entry.hashes[0],
        extension.as_ref(),
    );
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
    std::fs::write(path, body).expect("publish");
}

#[tokio::test]
async fn a_normalized_curseforge_recipe_reconstructs_a_server_through_the_generic_path() {
    let workspace = tempfile::tempdir().expect("temp dir");
    let cache = CacheLayout::new(workspace.path().join("cache"));
    let recipe = curseforge_recipe();
    publish_to_cache(&cache, &recipe.files[0], MOD_BODY);

    let client_source = workspace.path().join("client/.launcher/source");
    let client_game = workspace.path().join("client/minecraft");
    std::fs::create_dir_all(client_source.join("curseforge/overrides/config")).expect("mkdir");
    std::fs::write(
        client_source.join("curseforge/overrides/config/repro.toml"),
        "debug = true\n",
    )
    .expect("write");
    std::fs::write(client_source.join("curseforge/manifest.json"), manifest()).expect("write");
    SourceStore::new(&client_source)
        .write_recipe(&recipe)
        .expect("retain");

    let server_game = workspace.path().join("server/minecraft");
    let server_source = workspace.path().join("server/.launcher/source");
    let manager = manager();

    // Nothing declares this file server-applicable, so an unconfirmed
    // reconstruction leaves it out and says so.
    let unconfirmed = materialize_server_recipe(ServerMaterializeRequest {
        recipe: &recipe,
        unknown_server_files: &UnknownServerFiles::only(OptionalSelection::none()),
        game_dir: &server_game,
        client_game_dir: &client_game,
        client_source_dir: &client_source,
        server_source_dir: &server_source,
        reserved: &[],
        fetch: ArtifactFetch {
            manager: &manager,
            cache: &cache,
            // Reconstruction needs no provider credential of any kind.
            credentials: &OpenArtifacts,
            reporter: &OperationReporter::silent("server-content"),
            cancel: &CancellationToken::new(),
        },
    })
    .await
    .expect("reconstruct");
    assert!(unconfirmed.files.installed.is_empty());
    assert_eq!(
        unconfirmed.unconfirmed_unknown,
        vec![PathBuf::from("mods/required-1.0.0.jar")]
    );
    assert!(
        server_game.join("config/repro.toml").is_file(),
        "the pack's common overrides still reach the server"
    );

    // Confirming it explicitly installs it, from the shared cache alone.
    let server_game = workspace.path().join("server-2/minecraft");
    let server_source = workspace.path().join("server-2/.launcher/source");
    let confirmed = materialize_server_recipe(ServerMaterializeRequest {
        recipe: &recipe,
        unknown_server_files: &UnknownServerFiles::only(OptionalSelection::of([
            "mods/required-1.0.0.jar",
        ])),
        game_dir: &server_game,
        client_game_dir: &client_game,
        client_source_dir: &client_source,
        server_source_dir: &server_source,
        reserved: &[],
        fetch: ArtifactFetch {
            manager: &manager,
            cache: &cache,
            credentials: &OpenArtifacts,
            reporter: &OperationReporter::silent("server-content"),
            cancel: &CancellationToken::new(),
        },
    })
    .await
    .expect("reconstruct");
    assert_eq!(
        confirmed.files.installed,
        vec![PathBuf::from("mods/required-1.0.0.jar")]
    );
    assert_eq!(
        std::fs::read(server_game.join("mods/required-1.0.0.jar")).expect("read"),
        MOD_BODY
    );

    // The server's own record keeps the exact provider identity, the pack's
    // ownership, and the undeclared side as undeclared.
    let server_recipe = SourceStore::new(&server_source)
        .read_recipe()
        .expect("the server's own recipe");
    assert_eq!(server_recipe.source_kind, ImportSourceKind::CurseForgePack);
    assert_eq!(server_recipe.minecraft_version, "1.20.1");
    assert_eq!(server_recipe.loader.version.as_deref(), Some("47.4.10"));
    let entry = &server_recipe.files[0];
    assert_eq!(entry.origin, ModOrigin::PackDeclared);
    assert_eq!(
        entry.server,
        SideRequirement::Unknown,
        "confirming one reconstruction does not turn an undeclared side into a declaration"
    );
    assert_eq!(
        entry.selected,
        Some(true),
        "what this server actually installed is recorded"
    );
    let identity = entry.identity.as_ref().expect("an identity");
    assert_eq!(identity.provider, Some(ModProvider::CurseForge));
    assert_eq!(identity.project_id.as_deref(), Some("238222"));
    assert_eq!(identity.file_id.as_deref(), Some("4593548"));
}
