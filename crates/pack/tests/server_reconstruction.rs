#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use ephervex_download::{
    CacheLayout, ContentStore, DownloadManager, DownloaderConfig, FileExtension, OperationReporter,
};
use ephervex_model::{
    Hash, HashAlgorithm, ImportSourceKind, LoaderKind, LoaderSelection, ModOrigin, ModProvider,
};
use ephervex_pack::{
    OptionalSelection, OverrideLayer, OverrideScope, PackError, PackFileRecipe, PackRecipe,
    ProviderFileIdentity, ProviderRecipeMetadata, RECIPE_VERSION, ServerMaterializeRequest,
    SideRequirement, SourceStore, materialize_server_recipe,
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
    confirmed_unknown: &OptionalSelection,
    game: &Path,
    client_source: &Path,
    server_source: &Path,
    cache: &CacheLayout,
) -> Result<ephervex_pack::MaterializedServerRecipe, PackError> {
    let manager = DownloadManager::new(DownloaderConfig::default()).expect("manager");
    materialize_server_recipe(ServerMaterializeRequest {
        recipe,
        confirmed_unknown,
        game_dir: game,
        client_source_dir: client_source,
        server_source_dir: server_source,
        manager: &manager,
        cache,
        reporter: &OperationReporter::silent("server-reconstruction-test"),
        cancel: &CancellationToken::new(),
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
            &OptionalSelection::none(),
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
        assert_eq!(
            SourceStore::new(&server_source)
                .read_recipe()
                .expect("server recipe"),
            recipe
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

#[tokio::test]
async fn unknown_server_content_requires_an_explicit_creation_confirmation() {
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
        &OptionalSelection::of(["mods/unknown.jar"]),
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
        .server_reconstruction_plan(&OptionalSelection::of(["mods/not-unknown.jar"]))
        .expect_err("typos are not silently ignored");
    assert!(matches!(error, PackError::UnknownServerSelection { .. }));
}

#[tokio::test]
async fn missing_layers_and_path_collisions_fail_before_overwriting_server_content() {
    let temp = tempfile::tempdir().expect("tempdir");
    let cache = CacheLayout::new(temp.path().join("cache"));
    let (mut missing, _) = recipe();
    missing.provider_metadata.manifest_file = None;
    let error = reconstruct(
        &missing,
        &OptionalSelection::none(),
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
        &OptionalSelection::none(),
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
        &OptionalSelection::none(),
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
