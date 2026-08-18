//! Installed workspaces are independent of the reusable artifact cache.
//!
//! The artifact-cache budget only makes sense if evicting an entry costs a
//! re-download and nothing else. That is a property of the *materialization*
//! mechanism, not of the collector, so these tests exercise the real production
//! paths — `install_files` for a client instance and `materialize_server_recipe`
//! for a server — and then run the real collector against the same cache root.
//!
//! Offline throughout. Every artifact is placed at its content-addressed cache
//! path first, which is the cache-hit path a second import of the same pack
//! takes: the installer verifies the published hashes against what is there and
//! never reaches the network.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use ephervex_download::{
    ArtifactCache, CacheLayout, ContentStore, DownloadManager, DownloaderConfig, FileExtension,
    OperationReporter,
};
use ephervex_model::{
    ArtifactCacheLimit, Hash, HashAlgorithm, LoaderKind, LoaderSelection, ModOrigin,
};
use ephervex_pack::{
    ArtifactFetch, FileInstall, OpenArtifacts, OptionalSelection, PackFileRecipe, PackRecipe,
    PackSide, ServerMaterializeRequest, SideRequirement, SourceStore, install_files,
    materialize_server_recipe,
};
use tokio_util::sync::CancellationToken;

/// A body large enough that a real budget can be exceeded by it.
///
/// The smallest budget the settings model accepts is one mebibyte, so proving
/// that collection actually triggers needs an artifact bigger than that rather
/// than a token few bytes.
fn mod_body() -> Vec<u8> {
    b"a jar that an instance has to keep owning\n".repeat(51_200)
}

fn sha1(bytes: &[u8]) -> Hash {
    use sha1::Digest as _;
    let mut digest = sha1::Sha1::new();
    digest.update(bytes);
    Hash::new(HashAlgorithm::Sha1, &hex::encode(digest.finalize())).expect("sha1")
}

fn recipe(body: &[u8]) -> PackRecipe {
    let mut recipe = PackRecipe::for_manual_instance(
        "cache independence".to_owned(),
        "1.21.1".to_owned(),
        LoaderSelection::exact(LoaderKind::NeoForge, "21.1.100".to_owned()),
    );
    recipe.files.push(PackFileRecipe {
        logical_path: PathBuf::from("mods/kept.jar"),
        hashes: vec![sha1(body)],
        downloads: vec!["https://cdn.modrinth.com/data/test/mods/kept.jar".to_owned()],
        size: Some(body.len() as u64),
        client: SideRequirement::Required,
        server: SideRequirement::Required,
        selected: None,
        origin: ModOrigin::PackDeclared,
        identity: None,
    });
    recipe
}

/// Places the artifact at the cache path the installer will resolve for it.
fn publish_to_cache(cache: &CacheLayout, entry: &PackFileRecipe, body: &[u8]) -> PathBuf {
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
    std::fs::create_dir_all(path.parent().expect("shard")).expect("shard directory");
    std::fs::write(&path, body).expect("cached artifact");
    path
}

fn manager() -> DownloadManager {
    DownloadManager::new(DownloaderConfig::default()).expect("download manager")
}

fn reporter() -> OperationReporter {
    OperationReporter::silent("artifact-cache-independence")
}

#[tokio::test]
async fn a_mod_installed_into_a_client_instance_survives_eviction_of_its_cache_entry() {
    let temp = tempfile::tempdir().expect("tempdir");
    let layout = CacheLayout::new(temp.path().join("cache"));
    let game_dir = temp.path().join("client/minecraft");
    let body = mod_body();
    let recipe = recipe(&body);
    let cached = publish_to_cache(&layout, &recipe.files[0], &body);

    let installed = install_files(FileInstall {
        recipe: &recipe,
        side: PackSide::Client,
        selection: &OptionalSelection::default(),
        game_dir: &game_dir,
        fetch: ArtifactFetch {
            manager: &manager(),
            cache: &layout,
            credentials: &OpenArtifacts,
            reporter: &reporter(),
            cancel: &CancellationToken::new(),
        },
    })
    .await
    .expect("the cached artifact installs without a network");
    assert_eq!(installed.installed, vec![PathBuf::from("mods/kept.jar")]);

    let in_instance = game_dir.join("mods/kept.jar");
    assert!(in_instance.is_file());

    // A budget no single artifact could fit, so the collector has to take it.
    let report = ArtifactCache::new(layout.clone())
        .collect(ArtifactCacheLimit::Limited(1024 * 1024))
        .await
        .expect("collection runs");
    assert_eq!(report.removed_entries, 1, "the shared copy is collectable");
    assert!(!cached.exists());

    assert_eq!(
        std::fs::read(&in_instance).expect("the installed mod is still readable"),
        body,
        "an instance owns its own copy, byte for byte"
    );
}

#[tokio::test]
async fn a_reconstructed_server_survives_a_full_clear_of_the_reusable_cache() {
    let temp = tempfile::tempdir().expect("tempdir");
    let layout = CacheLayout::new(temp.path().join("cache"));
    let client_source = temp.path().join("client/.launcher/source");
    let server_source = temp.path().join("server/.launcher/source");
    let server_game = temp.path().join("server/minecraft");
    let body = mod_body();
    let recipe = recipe(&body);
    let cached = publish_to_cache(&layout, &recipe.files[0], &body);
    SourceStore::new(&client_source)
        .write_recipe(&recipe)
        .expect("retained recipe");

    let materialized = materialize_server_recipe(ServerMaterializeRequest {
        recipe: &recipe,
        confirmed_unknown: &OptionalSelection::default(),
        game_dir: &server_game,
        client_source_dir: &client_source,
        server_source_dir: &server_source,
        fetch: ArtifactFetch {
            manager: &manager(),
            cache: &layout,
            credentials: &OpenArtifacts,
            reporter: &reporter(),
            cancel: &CancellationToken::new(),
        },
    })
    .await
    .expect("the server reconstructs from the cache alone");
    assert_eq!(
        materialized.files.installed,
        vec![PathBuf::from("mods/kept.jar")]
    );

    let in_server = server_game.join("mods/kept.jar");
    let retained_recipe = materialized.recipe_path.clone();
    assert!(in_server.is_file() && retained_recipe.is_file());

    let report = ArtifactCache::new(layout.clone())
        .clear()
        .await
        .expect("manual clear runs");
    assert_eq!(report.removed_entries, 1);
    assert_eq!(report.usage_after, 0);
    assert!(!cached.exists());

    assert_eq!(
        std::fs::read(&in_server).expect("the server's own mod is still readable"),
        body
    );
    assert!(
        retained_recipe.is_file(),
        "a clear must not touch retained desired state"
    );
}

#[tokio::test]
async fn clearing_the_reusable_cache_leaves_every_other_cache_store_alone() {
    let temp = tempfile::tempdir().expect("tempdir");
    let layout = CacheLayout::new(temp.path().join("cache"));
    let body = mod_body();
    let recipe = recipe(&body);
    let cached = publish_to_cache(&layout, &recipe.files[0], &body);

    // Things that live under the same root and are not reusable artifacts: a
    // library a server's own `libraries` link resolves through, an asset object a
    // running game opens, and a provider metadata document.
    let library = layout.library(
        &ephervex_download::MavenCoordinate::parse("net.neoforged:neoforge:21.1.100")
            .expect("coordinate"),
    );
    let asset = layout
        .asset_object(&sha1(b"asset"))
        .expect("asset object path");
    let metadata = layout.content(ContentStore::Metadata, &sha1(b"metadata"), None);
    for path in [&library, &asset, &metadata] {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("directory");
        std::fs::write(path, b"not a reusable artifact").expect("write");
    }

    let report = ArtifactCache::new(layout).clear().await.expect("clear");
    assert_eq!(report.removed_entries, 1);
    assert!(!cached.exists());
    for path in [&library, &asset, &metadata] {
        assert!(
            path.is_file(),
            "{} is outside the reusable artifact store",
            path.display()
        );
    }
}
