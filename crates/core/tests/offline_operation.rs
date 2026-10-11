//! What Ephervex can still do with no network at all.
//!
//! A bug-reproduction tool is used on trains, in hotels, and on machines that
//! have been deliberately cut off. The rule is that a missing network costs a
//! *feature*, never the application: nothing crashes, nothing silently
//! substitutes, and everything answerable from local state answers.
//!
//! Determinism note: none of these cases proves a *failure* mode by leaving the
//! network out, because a developer machine has one and CI does not, and a test
//! whose outcome depends on which is running is worthless. Every case here
//! instead exercises a path that must reach its answer from local state alone,
//! against roots where nothing has ever been fetched. Network failure
//! classification itself is covered against loopback servers in
//! `ephervex-download`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use ephervex_core::{App, Paths};
use ephervex_model::{ArtifactCacheLimit, LoaderSelection, ModProvider, StorageClass};
use ephervex_options::PreferenceKey;
use ephervex_storage::NewClient;
use tempfile::TempDir;

fn roots(base: &TempDir) -> Paths {
    let paths = Paths::rooted_at(base.path());
    paths.ensure_base_directories().expect("create roots");
    paths
}

fn app(paths: &Paths) -> App {
    App::open_at(paths.clone()).expect("open the application")
}

#[tokio::test]
async fn browsing_local_instances_and_servers_needs_nothing_remote() {
    let base = tempfile::tempdir().expect("temp dir");
    let paths = roots(&base);

    let registry = ephervex_storage::Registry::open(paths.clone()).expect("registry");
    for name in ["First repro", "Second repro"] {
        let record = registry
            .create_client(
                NewClient::new(name, "1.20.1", LoaderSelection::vanilla())
                    .with_storage(StorageClass::Persistent),
            )
            .expect("create");
        registry.publish_client(&record).expect("publish");
    }
    drop(registry);

    let app = app(&paths);
    assert_eq!(app.list_clients().expect("clients").len(), 2);
    assert!(app.list_servers().expect("servers").is_empty());
    // Reconciliation is a startup step and must not need a provider either.
    let report = app.reconcile().expect("reconcile");
    assert!(report.is_empty());
}

#[tokio::test]
async fn global_preferences_are_editable_without_a_provider() {
    let base = tempfile::tempdir().expect("temp dir");
    let paths = roots(&base);
    let mut app = app(&paths);

    for (key, value) in [
        (PreferenceKey::GuiScale, "2"),
        (PreferenceKey::RenderDistance, "8"),
        (PreferenceKey::Fov, "80"),
        (PreferenceKey::Fullscreen, "false"),
    ] {
        app.set_minecraft_preference(key, value)
            .unwrap_or_else(|error| panic!("{key:?} must be settable offline: {error}"));
    }

    let loaded = app.minecraft_preferences();
    assert_eq!(loaded.preferences.managed_keys().len(), 4);
}

#[tokio::test]
async fn preference_compatibility_answers_from_the_bundled_baseline() {
    // The reason a baseline is compiled in at all: a machine that has never
    // reached the feed still knows what each release stores.
    let base = tempfile::tempdir().expect("temp dir");
    let paths = roots(&base);
    let app = app(&paths);

    for release in ["1.12.2", "1.20.1"] {
        let support = app
            .minecraft_preference_compatibility(release)
            .unwrap_or_else(|error| panic!("{release} must resolve offline: {error}"));
        assert!(
            !support.is_empty(),
            "{release} resolved to nothing from the bundled dataset"
        );
    }

    // A release the dataset does not name stays fully usable and simply
    // receives no managed writes, rather than failing or being guessed at.
    let unknown = app
        .minecraft_preference_compatibility("1.99.99")
        .expect("an unnamed release is not an error");
    assert!(
        unknown
            .values()
            .all(|support| *support == ephervex_options::ReleaseSupport::Unknown),
        "an unnamed release must not claim support for anything"
    );
}

#[tokio::test]
async fn a_corrupt_cached_options_feed_falls_back_to_the_bundled_baseline() {
    // The cache is a disposable optimization. Bytes that no longer parse are
    // discarded rather than trusted, and the launcher keeps working.
    let base = tempfile::tempdir().expect("temp dir");
    let paths = roots(&base);

    let metadata_root = paths.cache_dir().join("metadata");
    std::fs::create_dir_all(&metadata_root).expect("create the metadata store");
    // Whatever shape the store uses, a file of rubbish anywhere inside it must
    // not become compatibility knowledge.
    std::fs::write(metadata_root.join("minecraft-options.json"), b"{not json")
        .expect("write rubbish");

    let app = app(&paths);
    let compatibility = app.cached_minecraft_options().await;
    assert!(
        compatibility.schema_id("1.20.1").is_some(),
        "the bundled baseline still answers when the cache cannot"
    );
}

#[tokio::test]
async fn rereading_the_cached_options_feed_replaces_what_an_app_loaded_earlier() {
    let base = tempfile::tempdir().expect("temp dir");
    let paths = roots(&base);
    let app = app(&paths);
    let loaded = app.cached_minecraft_options().await;

    let metadata_root = paths.cache_dir().join("metadata");
    std::fs::create_dir_all(&metadata_root).expect("create the metadata store");
    std::fs::write(metadata_root.join("minecraft-options.json"), b"{not json")
        .expect("write rubbish");
    let reloaded = app.reload_cached_minecraft_options().await;

    assert!(
        !Arc::ptr_eq(&loaded, &reloaded),
        "the document is read again rather than the loaded one returned"
    );
    assert!(
        Arc::ptr_eq(&reloaded, &app.minecraft_options()),
        "later answers use the reloaded data"
    );
}

#[tokio::test]
async fn the_artifact_cache_is_measurable_and_configurable_offline() {
    let base = tempfile::tempdir().expect("temp dir");
    let paths = roots(&base);
    let mut app = app(&paths);

    let status = app
        .artifact_cache_status()
        .await
        .expect("measuring local files needs no network");
    assert_eq!(status.used_bytes, 0);
    assert_eq!(status.entries, 0);

    let status = app
        .set_artifact_cache_limit(ArtifactCacheLimit::Limited(1024 * 1024 * 1024))
        .await
        .expect("changing a local budget needs no network");
    assert_eq!(
        status.limit,
        ArtifactCacheLimit::Limited(1024 * 1024 * 1024)
    );
}

#[tokio::test]
async fn the_preferred_catalog_is_answerable_with_no_credential_and_no_request() {
    // Naming CurseForge says which catalog to browse first, not that it can be
    // reached. Nothing about answering this question touches a provider.
    let base = tempfile::tempdir().expect("temp dir");
    let paths = roots(&base);
    let mut app = app(&paths);

    app.set_default_mod_catalog(Some(ModProvider::CurseForge))
        .expect("prefer curseforge offline");
    assert_eq!(
        app.default_mod_catalog().resolved(),
        Some(ModProvider::CurseForge)
    );
}
