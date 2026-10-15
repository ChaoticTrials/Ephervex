#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use ephervex_download::{
    CacheLayout, ContentStore, DownloadManager, DownloaderConfig, FileExtension, OperationReporter,
};
use ephervex_model::{
    Hash, HashAlgorithm, ImportSourceKind, LoaderKind, LoaderSelection, ModOrigin, ModProvider,
};
use ephervex_pack::{
    ArtifactFetch, OpenArtifacts, OptionalSelection, OverrideLayer, OverrideScope, PackError,
    PackFileRecipe, PackRecipe, ProviderFileIdentity, ProviderRecipeMetadata, RECIPE_VERSION,
    ServerMaterializeRequest, SideRequirement, SourceStore, UnknownServerFiles,
    materialize_server_recipe,
};
use tokio_util::sync::CancellationToken;

fn sha1(bytes: &[u8]) -> Hash {
    use sha1::Digest as _;
    let mut digest = sha1::Sha1::new();
    digest.update(bytes);
    let encoded = hex::encode(digest.finalize());
    Hash::new(HashAlgorithm::Sha1, &encoded).expect("sha1")
}

fn file(
    path: &str,
    body: &[u8],
    client: SideRequirement,
    server: SideRequirement,
    selected: Option<bool>,
) -> PackFileRecipe {
    PackFileRecipe {
        logical_path: PathBuf::from(path),
        hashes: vec![sha1(body)],
        downloads: vec![format!("https://cdn.modrinth.com/data/test/{path}")],
        size: Some(body.len() as u64),
        client,
        server,
        selected,
        origin: ModOrigin::PackDeclared,
        identity: None,
        original_identity: None,
        installed_despite_unsupported: None,
        relations: None,
    }
}

fn cache_file(cache: &CacheLayout, entry: &PackFileRecipe, body: &[u8]) {
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
    std::fs::create_dir_all(path.parent().expect("cache parent")).expect("cache directory");
    std::fs::write(path, body).expect("cached artifact");
}

fn write(path: &Path, body: &[u8]) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("directory");
    std::fs::write(path, body).expect("write");
}

fn recipe() -> (PackRecipe, Vec<Vec<u8>>) {
    let bodies = vec![
        b"required".to_vec(),
        b"client-only".to_vec(),
        b"selected optional".to_vec(),
        b"unselected optional".to_vec(),
        b"unknown".to_vec(),
        b"browser managed".to_vec(),
    ];
    let mut files = vec![
        file(
            "mods/required.jar",
            &bodies[0],
            SideRequirement::Unsupported,
            SideRequirement::Required,
            None,
        ),
        file(
            "mods/client-only.jar",
            &bodies[1],
            SideRequirement::Required,
            SideRequirement::Unsupported,
            None,
        ),
        file(
            "mods/selected-optional.jar",
            &bodies[2],
            SideRequirement::Optional,
            SideRequirement::Optional,
            Some(true),
        ),
        file(
            "mods/unselected-optional.jar",
            &bodies[3],
            SideRequirement::Optional,
            SideRequirement::Optional,
            Some(false),
        ),
        file(
            "mods/unknown.jar",
            &bodies[4],
            SideRequirement::Required,
            SideRequirement::Unknown,
            Some(true),
        ),
        file(
            "mods/browser-managed.jar",
            &bodies[5],
            SideRequirement::Required,
            SideRequirement::Required,
            None,
        ),
    ];
    files[5].origin = ModOrigin::UserSelected;
    files[5].identity = Some(ProviderFileIdentity {
        provider: Some(ModProvider::Modrinth),
        project_id: Some("browser-project".to_owned()),
        version_id: Some("exact-version".to_owned()),
        file_id: None,
    });

    (
        PackRecipe {
            recipe_version: RECIPE_VERSION,
            source_kind: ImportSourceKind::ModrinthPack,
            original_name: "Retained Test Pack".to_owned(),
            original_version: Some("exact-pack-version".to_owned()),
            author: None,
            summary: None,
            archive_name: Some("deleted.mrpack".to_owned()),
            minecraft_version: "1.20.1".to_owned(),
            loader: LoaderSelection::exact(LoaderKind::Forge, "47.2.0"),
            files,
            override_layers: vec![
                OverrideLayer {
                    scope: OverrideScope::Client,
                    archive_prefix: "client-overrides".to_owned(),
                    retained_at: None,
                    files: 1,
                },
                OverrideLayer {
                    scope: OverrideScope::Server,
                    archive_prefix: "server-overrides".to_owned(),
                    retained_at: Some(PathBuf::from("modrinth/server-overrides")),
                    files: 3,
                },
                OverrideLayer {
                    scope: OverrideScope::Common,
                    archive_prefix: "overrides".to_owned(),
                    retained_at: Some(PathBuf::from("modrinth/overrides")),
                    files: 1,
                },
            ],
            provider_metadata: ProviderRecipeMetadata::new(
                ImportSourceKind::ModrinthPack,
                Some(PathBuf::from("modrinth/modrinth.index.json")),
            ),
        },
        bodies,
    )
}

async fn reconstruct(
    recipe: &PackRecipe,
    unknown_server_files: &UnknownServerFiles,
    game: &Path,
    client_source: &Path,
    server_source: &Path,
    cache: &CacheLayout,
) -> Result<ephervex_pack::MaterializedServerRecipe, PackError> {
    reconstruct_reserving(
        recipe,
        unknown_server_files,
        game,
        client_source,
        server_source,
        cache,
        &[],
    )
    .await
}

async fn reconstruct_reserving(
    recipe: &PackRecipe,
    unknown_server_files: &UnknownServerFiles,
    game: &Path,
    client_source: &Path,
    server_source: &Path,
    cache: &CacheLayout,
    reserved: &[PathBuf],
) -> Result<ephervex_pack::MaterializedServerRecipe, PackError> {
    let manager = DownloadManager::new(DownloaderConfig::default()).expect("manager");
    materialize_server_recipe(ServerMaterializeRequest {
        recipe,
        unknown_server_files,
        game_dir: game,
        // These reconstruct from the addresses the recipe records, so the
        // client's own game directory is never read. It still has to be named:
        // it is where a file with no address would be copied from.
        client_game_dir: &client_source
            .parent()
            .and_then(Path::parent)
            .unwrap_or(client_source)
            .join("minecraft"),
        client_source_dir: client_source,
        server_source_dir: server_source,
        reserved,
        fetch: ArtifactFetch {
            manager: &manager,
            cache,
            credentials: &OpenArtifacts,
            reporter: &OperationReporter::silent("server-reconstruction-test"),
            cancel: &CancellationToken::new(),
        },
    })
    .await
}

#[tokio::test]
async fn retained_desired_state_reconstructs_a_server_without_the_source_archive() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (recipe, bodies) = recipe();
    let client_source = temp.path().join("client/.launcher/source");
    let client_game = temp.path().join("client/minecraft");
    let cache = CacheLayout::new(temp.path().join("cache"));
    SourceStore::new(&client_source)
        .write_recipe(&recipe)
        .expect("client recipe");
    write(
        &client_source.join("modrinth/modrinth.index.json"),
        b"retained manifest",
    );
    write(
        &client_source.join("modrinth/overrides/config/precedence.txt"),
        b"common",
    );
    write(
        &client_source.join("modrinth/server-overrides/config/precedence.txt"),
        b"server",
    );
    write(
        &client_source.join("modrinth/server-overrides/server.properties"),
        b"motd=retained\n",
    );
    write(
        &client_source.join("modrinth/server-overrides/eula.txt"),
        b"eula=true\n",
    );
    write(&client_game.join("mods/manual-unmanaged.jar"), b"manual");
    for (entry, body) in recipe.files.iter().zip(&bodies) {
        cache_file(&cache, entry, body);
    }
    assert!(!temp.path().join("deleted.mrpack").exists());
    let before = std::fs::read(SourceStore::new(&client_source).recipe_path()).expect("before");

    for server_name in ["server-a", "server-b"] {
        let game = temp.path().join(server_name).join("server");
        let server_source = temp.path().join(server_name).join(".launcher/source");
        let materialized = reconstruct(
            &recipe,
            &UnknownServerFiles::only(OptionalSelection::none()),
            &game,
            &client_source,
            &server_source,
            &cache,
        )
        .await
        .expect("reconstruct from retained state");

        assert_eq!(materialized.files.installed.len(), 3);
        assert_eq!(materialized.files.reused_from_cache, 3);
        assert!(game.join("mods/required.jar").is_file());
        assert!(game.join("mods/selected-optional.jar").is_file());
        assert!(game.join("mods/browser-managed.jar").is_file());
        assert!(!game.join("mods/client-only.jar").exists());
        assert!(!game.join("mods/unselected-optional.jar").exists());
        assert!(!game.join("mods/unknown.jar").exists());
        assert!(!game.join("mods/manual-unmanaged.jar").exists());
        assert_eq!(
            materialized.unconfirmed_unknown,
            vec![PathBuf::from("mods/unknown.jar")]
        );
        assert_eq!(
            std::fs::read(game.join("config/precedence.txt")).expect("precedence"),
            b"server"
        );
        // The server's own copy describes the server, not the client it was
        // copied from. An undeclared server side this creation did not confirm
        // is recorded as answered "no", so the file the server never received
        // cannot later look like one it has.
        let mut expected = recipe.clone();
        for file in &mut expected.files {
            if file.logical_path == Path::new("mods/unknown.jar") {
                file.selected = Some(false);
            }
        }
        assert_eq!(
            SourceStore::new(&server_source)
                .read_recipe()
                .expect("server recipe"),
            expected
        );
        assert!(!server_source.join("modrinth/client-overrides").exists());
        assert!(!game.join("eula.txt").exists());
    }

    assert_eq!(
        std::fs::read(SourceStore::new(&client_source).recipe_path()).expect("after"),
        before,
        "repeated server creation must not mutate client desired state"
    );
}

/// A pack format that declares no server side leaves every one of its files
/// undeclared. Skipping those handed back a linked server holding none of the
/// pack it was created from, so reconstructing them is the default.
#[tokio::test]
async fn undeclared_server_content_is_reconstructed_by_default() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (recipe, bodies) = recipe();
    let client_source = temp.path().join("client-source");
    let cache = CacheLayout::new(temp.path().join("cache"));
    write(
        &client_source.join("modrinth/modrinth.index.json"),
        b"manifest",
    );
    write(&client_source.join("modrinth/overrides/empty"), b"");
    write(&client_source.join("modrinth/server-overrides/empty"), b"");
    for (entry, body) in recipe.files.iter().zip(&bodies) {
        cache_file(&cache, entry, body);
    }

    let game = temp.path().join("server");
    let server_source = temp.path().join("server-source");
    let materialized = reconstruct(
        &recipe,
        &UnknownServerFiles::default(),
        &game,
        &client_source,
        &server_source,
        &cache,
    )
    .await
    .expect("reconstruct with the default policy");

    assert!(game.join("mods/unknown.jar").is_file());
    assert!(materialized.unconfirmed_unknown.is_empty());
    // A declared client-only file stays excluded. Only the undeclared side
    // changed meaning; a provider that answered the question is still obeyed.
    assert!(!game.join("mods/client-only.jar").exists());
    // An optional the client declined is still declined: an undeclared side and
    // an unselected optional are different facts.
    assert!(!game.join("mods/unselected-optional.jar").exists());

    // The server's own recipe records the file as one it has, so a later mod
    // operation does not treat it as pack content the server never received.
    let stored = SourceStore::new(&server_source)
        .read_recipe()
        .expect("server recipe");
    let unknown = stored
        .files
        .iter()
        .find(|file| file.logical_path == Path::new("mods/unknown.jar"))
        .expect("undeclared entry");
    assert_eq!(unknown.selected, Some(true));
}

#[tokio::test]
async fn a_narrowed_policy_reconstructs_only_the_undeclared_files_it_names() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (recipe, bodies) = recipe();
    let client_source = temp.path().join("client-source");
    let cache = CacheLayout::new(temp.path().join("cache"));
    write(
        &client_source.join("modrinth/modrinth.index.json"),
        b"manifest",
    );
    write(&client_source.join("modrinth/overrides/empty"), b"");
    write(&client_source.join("modrinth/server-overrides/empty"), b"");
    for (entry, body) in recipe.files.iter().zip(&bodies) {
        cache_file(&cache, entry, body);
    }

    let materialized = reconstruct(
        &recipe,
        &UnknownServerFiles::only(OptionalSelection::of(["mods/unknown.jar"])),
        &temp.path().join("server"),
        &client_source,
        &temp.path().join("server-source"),
        &cache,
    )
    .await
    .expect("explicit confirmation");
    assert!(temp.path().join("server/mods/unknown.jar").is_file());
    assert!(materialized.unconfirmed_unknown.is_empty());

    let error = recipe
        .server_reconstruction_plan(&UnknownServerFiles::only(OptionalSelection::of([
            "mods/not-unknown.jar",
        ])))
        .expect_err("typos are not silently ignored");
    assert!(matches!(error, PackError::UnknownServerSelection { .. }));
}

#[test]
fn the_server_preview_lists_what_server_reconstruction_installs() {
    let (mut recipe, _) = recipe();
    // No recorded choice, so only the unknown-file policy can include it.
    for file in &mut recipe.files {
        if file.server == SideRequirement::Unknown {
            file.selected = None;
        }
    }
    let paths = |files: &[&PackFileRecipe]| -> Vec<PathBuf> {
        files.iter().map(|file| file.logical_path.clone()).collect()
    };

    let include_all = UnknownServerFiles::default();
    let created = recipe
        .server_reconstruction_plan(&include_all)
        .expect("reconstruction plan");
    let preview = recipe
        .server_preview_plan(&OptionalSelection::none(), &include_all)
        .expect("preview plan");
    assert_eq!(paths(&preview.files), paths(&created.files));
    assert!(paths(&preview.files).contains(&PathBuf::from("mods/unknown.jar")));
    assert!(!paths(&preview.files).contains(&PathBuf::from("mods/unselected-optional.jar")));

    let none = UnknownServerFiles::only(OptionalSelection::none());
    let narrowed = recipe
        .server_preview_plan(&OptionalSelection::none(), &none)
        .expect("narrowed preview");
    assert!(!paths(&narrowed.files).contains(&PathBuf::from("mods/unknown.jar")));

    let everything = recipe
        .server_preview_plan(&OptionalSelection::all(), &include_all)
        .expect("preview with every optional");
    assert!(paths(&everything.files).contains(&PathBuf::from("mods/unselected-optional.jar")));
}

#[tokio::test]
async fn missing_layers_and_path_collisions_fail_before_overwriting_server_content() {
    let temp = tempfile::tempdir().expect("tempdir");
    let cache = CacheLayout::new(temp.path().join("cache"));
    let (mut missing, _) = recipe();
    missing.provider_metadata.manifest_file = None;
    let error = reconstruct(
        &missing,
        &UnknownServerFiles::default(),
        &temp.path().join("missing-game"),
        &temp.path().join("missing-source"),
        &temp.path().join("missing-server-source"),
        &cache,
    )
    .await
    .expect_err("missing retained layer");
    assert!(matches!(error, PackError::RetainedLayerUnavailable { .. }));

    let body = b"same";
    let mut collision =
        PackRecipe::for_manual_instance("Collision", "1.20.1", LoaderSelection::vanilla());
    collision.files = vec![
        file(
            "mods/same.jar",
            body,
            SideRequirement::Required,
            SideRequirement::Required,
            None,
        ),
        file(
            "mods/same.jar",
            body,
            SideRequirement::Required,
            SideRequirement::Required,
            None,
        ),
    ];
    let error = reconstruct(
        &collision,
        &UnknownServerFiles::default(),
        &temp.path().join("collision-game"),
        &temp.path().join("manual-source"),
        &temp.path().join("collision-server-source"),
        &cache,
    )
    .await
    .expect_err("duplicate destination");
    assert!(matches!(
        error,
        PackError::RecipeDestinationCollision { .. }
    ));

    collision.files.truncate(1);
    let occupied_game = temp.path().join("occupied-game");
    write(&occupied_game.join("mods/same.jar"), b"base server file");
    let error = reconstruct(
        &collision,
        &UnknownServerFiles::default(),
        &occupied_game,
        &temp.path().join("manual-source"),
        &temp.path().join("occupied-server-source"),
        &cache,
    )
    .await
    .expect_err("occupied destination");
    assert!(matches!(error, PackError::RecipeDestinationOccupied { .. }));
    assert_eq!(
        std::fs::read(occupied_game.join("mods/same.jar")).expect("preserved"),
        b"base server file"
    );
}

/// A jar the user dropped into the client themselves is recorded with no
/// address to fetch it from, because there is none: what a digest lookup
/// learned is what the jar *is*, not where it came from. The client has the
/// file, and a linked server exists to run what the client runs, so the server
/// takes its copy from the client rather than the build failing on a pack file
/// that supposedly lost its source.
#[tokio::test]
async fn a_hand_placed_jar_reaches_the_server_from_the_client_that_holds_it() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (mut recipe, bodies) = recipe();
    let client_source = temp.path().join("client/.launcher/source");
    let client_game = temp.path().join("client/minecraft");
    let cache = CacheLayout::new(temp.path().join("cache"));
    write(
        &client_source.join("modrinth/modrinth.index.json"),
        b"manifest",
    );
    write(&client_source.join("modrinth/overrides/empty"), b"");
    write(&client_source.join("modrinth/server-overrides/empty"), b"");
    for (entry, body) in recipe.files.iter().zip(&bodies) {
        cache_file(&cache, entry, body);
    }

    let hand_placed = b"a jar from a forum post";
    let mut entry = file(
        "mods/from-a-forum-post.jar",
        hand_placed,
        SideRequirement::Required,
        SideRequirement::Required,
        None,
    );
    entry.downloads.clear();
    entry.origin = ModOrigin::HandPlaced;
    recipe.files.push(entry);
    write(&client_game.join("mods/from-a-forum-post.jar"), hand_placed);

    let game = temp.path().join("server/minecraft");
    let materialized = reconstruct(
        &recipe,
        &UnknownServerFiles::default(),
        &game,
        &client_source,
        &temp.path().join("server/.launcher/source"),
        &cache,
    )
    .await
    .expect("a jar the client holds is one the server can have");

    let installed = game.join("mods/from-a-forum-post.jar");
    assert!(
        installed.is_file(),
        "the server has to end up with the jar the client has"
    );
    assert_eq!(
        std::fs::read(&installed).expect("read back"),
        hand_placed,
        "and with its bytes, not an empty file standing in for it"
    );
    assert!(
        materialized
            .files
            .installed
            .contains(&PathBuf::from("mods/from-a-forum-post.jar")),
        "{:?}",
        materialized.files.installed
    );
}

/// The same entry when the client does not have the file either: there is
/// nowhere left to get it from, and that is worth failing over rather than
/// quietly building a server missing a mod.
#[tokio::test]
async fn an_addressless_file_the_client_does_not_have_either_still_fails() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (mut recipe, bodies) = recipe();
    let client_source = temp.path().join("client/.launcher/source");
    let cache = CacheLayout::new(temp.path().join("cache"));
    write(
        &client_source.join("modrinth/modrinth.index.json"),
        b"manifest",
    );
    write(&client_source.join("modrinth/overrides/empty"), b"");
    write(&client_source.join("modrinth/server-overrides/empty"), b"");
    for (entry, body) in recipe.files.iter().zip(&bodies) {
        cache_file(&cache, entry, body);
    }

    let mut entry = file(
        "mods/vanished.jar",
        b"gone",
        SideRequirement::Required,
        SideRequirement::Required,
        None,
    );
    entry.downloads.clear();
    entry.origin = ModOrigin::HandPlaced;
    recipe.files.push(entry);

    let error = reconstruct(
        &recipe,
        &UnknownServerFiles::default(),
        &temp.path().join("server/minecraft"),
        &client_source,
        &temp.path().join("server/.launcher/source"),
        &cache,
    )
    .await
    .expect_err("nothing to copy and nowhere to fetch from");
    assert!(
        matches!(error, PackError::FileDownloadsMissing { .. }),
        "{error:?}"
    );
}

/// Sets up a client holding `client_bytes` at the path of an addressless entry
/// that records the digests of `recorded`, and returns what is needed to
/// reconstruct a server from it.
fn addressless_entry_with_client_copy(
    temp: &tempfile::TempDir,
    recorded: &[u8],
    client_bytes: &[u8],
) -> (PackRecipe, PathBuf, CacheLayout) {
    let (mut recipe, bodies) = recipe();
    let client_source = temp.path().join("client/.launcher/source");
    let client_game = temp.path().join("client/minecraft");
    let cache = CacheLayout::new(temp.path().join("cache"));
    write(
        &client_source.join("modrinth/modrinth.index.json"),
        b"manifest",
    );
    write(&client_source.join("modrinth/overrides/empty"), b"");
    write(&client_source.join("modrinth/server-overrides/empty"), b"");
    for (entry, body) in recipe.files.iter().zip(&bodies) {
        cache_file(&cache, entry, body);
    }
    let mut entry = file(
        "mods/a.jar",
        recorded,
        SideRequirement::Required,
        SideRequirement::Required,
        None,
    );
    entry.downloads.clear();
    entry.origin = ModOrigin::HandPlaced;
    recipe.files.push(entry);
    write(&client_game.join("mods/a.jar"), client_bytes);
    (recipe, client_source, cache)
}

/// The client's copy is mutable and the server recipe keeps the recorded
/// digests, so bytes that no longer match them must not be installed under
/// that identity.
#[tokio::test]
async fn a_changed_client_copy_of_an_addressless_file_is_not_installed() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (recipe, client_source, cache) =
        addressless_entry_with_client_copy(&temp, b"abc", b"changed");
    let game = temp.path().join("server/minecraft");

    let error = reconstruct(
        &recipe,
        &UnknownServerFiles::default(),
        &game,
        &client_source,
        &temp.path().join("server/.launcher/source"),
        &cache,
    )
    .await
    .expect_err("the client bytes do not match the recorded digests");

    assert!(
        matches!(error, PackError::FileDownloadsMissing { .. }),
        "{error:?}"
    );
    assert!(
        !game.join("mods/a.jar").exists(),
        "the mismatching bytes must not stay installed"
    );
}

/// A verified shared-cache copy is the recorded artifact, so it stands in for a
/// client file that has since changed.
#[tokio::test]
async fn a_verified_cache_copy_replaces_a_changed_client_copy() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (recipe, client_source, cache) =
        addressless_entry_with_client_copy(&temp, b"abc", b"changed");
    let recorded = recipe.files.last().expect("addressless entry");
    cache_file(&cache, recorded, b"abc");
    let game = temp.path().join("server/minecraft");

    reconstruct(
        &recipe,
        &UnknownServerFiles::default(),
        &game,
        &client_source,
        &temp.path().join("server/.launcher/source"),
        &cache,
    )
    .await
    .expect("the cache holds the recorded bytes");

    assert_eq!(
        std::fs::read(game.join("mods/a.jar")).expect("installed"),
        b"abc"
    );
}

/// An entry that records no digest cannot vouch for any bytes, so the client's
/// copy is not trusted for it either.
#[tokio::test]
async fn an_addressless_file_without_digests_is_not_copied_from_the_client() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (mut recipe, client_source, cache) =
        addressless_entry_with_client_copy(&temp, b"abc", b"abc");
    recipe.files.last_mut().expect("entry").hashes.clear();
    let game = temp.path().join("server/minecraft");

    let error = reconstruct(
        &recipe,
        &UnknownServerFiles::default(),
        &game,
        &client_source,
        &temp.path().join("server/.launcher/source"),
        &cache,
    )
    .await
    .expect_err("nothing identifies the bytes");

    assert!(
        matches!(error, PackError::FileDownloadsMissing { .. }),
        "{error:?}"
    );
    assert!(!game.join("mods/a.jar").exists());
}

/// A loader installation replaces `<server>/libraries` with a link into the
/// shared libraries store, and reconstruction runs after it. Retained pack
/// content must not be written through that link into storage every other
/// server shares.
#[cfg(unix)]
#[tokio::test]
async fn retained_overrides_never_write_through_the_shared_library_link() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (recipe, bodies) = recipe();
    let client_source = temp.path().join("client/.launcher/source");
    let cache = CacheLayout::new(temp.path().join("cache"));
    SourceStore::new(&client_source)
        .write_recipe(&recipe)
        .expect("client recipe");
    write(
        &client_source.join("modrinth/modrinth.index.json"),
        b"manifest",
    );
    write(&client_source.join("modrinth/overrides/empty"), b"");
    let shipped =
        "modrinth/server-overrides/libraries/net/minecraftforge/forge/1.20.1-47.2.0/unix_args.txt";
    write(&client_source.join(shipped), b"from the pack");
    write(
        &client_source.join("modrinth/server-overrides/server.properties"),
        b"motd=retained\n",
    );
    for (entry, body) in recipe.files.iter().zip(&bodies) {
        cache_file(&cache, entry, body);
    }

    let shared = temp.path().join("shared-libraries");
    let shared_args = shared.join("net/minecraftforge/forge/1.20.1-47.2.0/unix_args.txt");
    write(&shared_args, b"installed by the loader");
    let game = temp.path().join("server/server");
    std::fs::create_dir_all(&game).expect("server directory");
    std::os::unix::fs::symlink(&shared, game.join("libraries")).expect("libraries link");
    let server_source = temp.path().join("server/.launcher/source");

    reconstruct(
        &recipe,
        &UnknownServerFiles::only(OptionalSelection::none()),
        &game,
        &client_source,
        &server_source,
        &cache,
    )
    .await
    .expect("reconstruct beside a linked libraries directory");

    assert_eq!(
        std::fs::read(&shared_args).expect("shared file"),
        b"installed by the loader",
        "the shared store belongs to every server, not to one pack"
    );
    assert!(
        std::fs::symlink_metadata(game.join("libraries"))
            .expect("libraries")
            .is_symlink()
    );
    assert_eq!(
        std::fs::read(game.join("server.properties")).expect("properties"),
        b"motd=retained\n",
        "the rest of the layer is still applied"
    );
    assert_eq!(
        std::fs::read(server_source.join(shipped)).expect("retained copy"),
        b"from the pack",
        "the server's own source keeps the layer exactly as shipped"
    );
}

/// The loader installation puts the jar the server starts from at the server
/// root. A retained override layer, common or server, must never replace it
/// with unverified pack bytes, while the rest of the layer still applies.
#[tokio::test]
async fn retained_overrides_never_replace_the_launch_descriptor() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (recipe, bodies) = recipe();
    let client_source = temp.path().join("client/.launcher/source");
    let cache = CacheLayout::new(temp.path().join("cache"));
    SourceStore::new(&client_source)
        .write_recipe(&recipe)
        .expect("client recipe");
    write(
        &client_source.join("modrinth/modrinth.index.json"),
        b"manifest",
    );
    let launch = "forge-1.20.1-47.2.0-server.jar";
    write(
        &client_source.join("modrinth/overrides").join(launch),
        b"common pack bytes",
    );
    write(
        &client_source.join("modrinth/server-overrides").join(launch),
        b"server pack bytes",
    );
    write(
        &client_source.join("modrinth/server-overrides/server.properties"),
        b"motd=retained\n",
    );
    for (entry, body) in recipe.files.iter().zip(&bodies) {
        cache_file(&cache, entry, body);
    }

    let game = temp.path().join("server/server");
    write(&game.join(launch), b"verified by the loader");
    let server_source = temp.path().join("server/.launcher/source");

    reconstruct_reserving(
        &recipe,
        &UnknownServerFiles::only(OptionalSelection::none()),
        &game,
        &client_source,
        &server_source,
        &cache,
        &[PathBuf::from(launch)],
    )
    .await
    .expect("reconstruct beside an installed launch jar");

    assert_eq!(
        std::fs::read(game.join(launch)).expect("launch jar"),
        b"verified by the loader",
        "the server starts from the loader's artifact, never from pack bytes"
    );
    assert_eq!(
        std::fs::read(game.join("server.properties")).expect("properties"),
        b"motd=retained\n",
        "the rest of the layer is still applied"
    );
    assert_eq!(
        std::fs::read(server_source.join("modrinth/server-overrides").join(launch))
            .expect("retained copy"),
        b"server pack bytes",
        "the server's own source keeps the layer exactly as shipped"
    );
}

/// A declared pack file at the launch path is refused outright, so the
/// installed loader artifact survives even when the recipe names it.
#[tokio::test]
async fn a_declared_file_never_replaces_the_launch_descriptor() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (mut recipe, bodies) = recipe();
    let launch = "forge-1.20.1-47.2.0-server.jar";
    let declared_body = b"declared pack bytes";
    let declared = file(
        launch,
        declared_body,
        SideRequirement::Required,
        SideRequirement::Required,
        None,
    );
    let client_source = temp.path().join("client/.launcher/source");
    let cache = CacheLayout::new(temp.path().join("cache"));
    write(
        &client_source.join("modrinth/modrinth.index.json"),
        b"manifest",
    );
    write(&client_source.join("modrinth/overrides/empty"), b"");
    write(&client_source.join("modrinth/server-overrides/empty"), b"");
    for (entry, body) in recipe.files.iter().zip(&bodies) {
        cache_file(&cache, entry, body);
    }
    cache_file(&cache, &declared, declared_body);
    recipe.files.push(declared);

    let game = temp.path().join("server/server");
    write(&game.join(launch), b"verified by the loader");

    let error = reconstruct_reserving(
        &recipe,
        &UnknownServerFiles::only(OptionalSelection::none()),
        &game,
        &client_source,
        &temp.path().join("server/.launcher/source"),
        &cache,
        &[PathBuf::from(launch)],
    )
    .await
    .expect_err("a declared file cannot take the launch path");

    assert!(
        matches!(error, PackError::RecipeDestinationOccupied { .. }),
        "unexpected error: {error:?}"
    );
    assert_eq!(
        std::fs::read(game.join(launch)).expect("launch jar"),
        b"verified by the loader"
    );
}
