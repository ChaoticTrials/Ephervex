//! Durable intent survives the process that recorded it.
//!
//! Every case here mutates through the application services, drops the `App`
//! entirely, opens a new one over the same roots, and checks the exact value
//! came back. Dropping matters: a setting that only ever round-trips through a
//! live process proves nothing about what the next launch reads, and a cached
//! `OnceLock` would hide a write that never reached disk.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ephervex_core::{App, Paths};
use ephervex_model::DefaultModCatalog;
use ephervex_model::{ArtifactCacheLimit, LoaderSelection, ModProvider, StorageClass};
use ephervex_options::PreferenceKey;
use ephervex_storage::NewClient;
use tempfile::TempDir;

fn roots(base: &TempDir) -> Paths {
    let paths = Paths::rooted_at(base.path());
    paths.ensure_base_directories().expect("create roots");
    paths
}

fn reopen(paths: &Paths) -> App {
    App::open_at(paths.clone()).expect("reopen the application over the same roots")
}

#[tokio::test]
async fn global_minecraft_preferences_survive_a_restart_exactly() {
    let base = tempfile::tempdir().expect("temp dir");
    let paths = roots(&base);

    {
        let mut app = reopen(&paths);
        app.set_minecraft_preference(PreferenceKey::GuiScale, "3")
            .expect("set gui scale");
        app.set_minecraft_preference(PreferenceKey::RenderDistance, "16")
            .expect("set render distance");
        app.set_minecraft_preference(PreferenceKey::Fullscreen, "true")
            .expect("set fullscreen");
        // Managed-then-unmanaged keeps the value: turning management back on
        // must restore what the user chose, not a default.
        app.unmanage_minecraft_preference(PreferenceKey::Fullscreen)
            .expect("unmanage fullscreen");
    }

    let app = reopen(&paths);
    let loaded = app.minecraft_preferences();
    let preferences = &loaded.preferences;
    assert_eq!(
        preferences
            .value(PreferenceKey::GuiScale)
            .to_setting_value(),
        "3"
    );
    assert_eq!(
        preferences
            .value(PreferenceKey::RenderDistance)
            .to_setting_value(),
        "16"
    );
    let managed = preferences.managed_keys();
    assert!(managed.contains(&PreferenceKey::GuiScale));
    assert!(managed.contains(&PreferenceKey::RenderDistance));
    assert!(
        !managed.contains(&PreferenceKey::Fullscreen),
        "an unmanaged preference stays unmanaged across a restart"
    );
    assert_eq!(
        preferences
            .value(PreferenceKey::Fullscreen)
            .to_setting_value(),
        "true",
        "unmanaging keeps the value it was last given"
    );
}

#[tokio::test]
async fn the_preferred_mod_catalog_survives_a_restart() {
    let base = tempfile::tempdir().expect("temp dir");
    let paths = roots(&base);

    {
        let mut app = reopen(&paths);
        assert_eq!(app.default_mod_catalog(), DefaultModCatalog::BuiltIn);
        app.set_default_mod_catalog(Some(ModProvider::CurseForge))
            .expect("prefer curseforge");
    }

    assert_eq!(
        reopen(&paths).default_mod_catalog(),
        DefaultModCatalog::Selected(ModProvider::CurseForge)
    );

    {
        let mut app = reopen(&paths);
        app.set_default_mod_catalog(None).expect("clear");
    }
    assert_eq!(
        reopen(&paths).default_mod_catalog(),
        DefaultModCatalog::BuiltIn,
        "clearing the preference is itself durable"
    );
}

#[tokio::test]
async fn java_settings_survive_a_restart() {
    let base = tempfile::tempdir().expect("temp dir");
    let paths = roots(&base);

    let intended = {
        let mut app = reopen(&paths);
        let mut settings = app.java_settings();
        settings.auto_download = false;
        settings.min_memory_mib = 1024;
        settings.max_memory_mib = 6144;
        settings.jvm_args = vec!["-XX:+UseZGC".to_owned()];
        app.set_java_settings(&settings).expect("set java settings");
        settings
    };

    let restored = reopen(&paths).java_settings();
    assert_eq!(restored.auto_download, intended.auto_download);
    assert_eq!(restored.min_memory_mib, intended.min_memory_mib);
    assert_eq!(restored.max_memory_mib, intended.max_memory_mib);
    assert_eq!(restored.jvm_args, intended.jvm_args);
}

#[tokio::test]
async fn the_artifact_cache_budget_survives_a_restart() {
    let base = tempfile::tempdir().expect("temp dir");
    let paths = roots(&base);

    {
        let mut app = reopen(&paths);
        app.set_artifact_cache_limit(ArtifactCacheLimit::Unlimited)
            .await
            .expect("set unlimited");
    }
    assert_eq!(
        reopen(&paths).artifact_cache_limit(),
        ArtifactCacheLimit::Unlimited
    );

    {
        let mut app = reopen(&paths);
        app.set_artifact_cache_limit(ArtifactCacheLimit::Limited(2 * 1024 * 1024 * 1024))
            .await
            .expect("set a byte budget");
    }
    assert_eq!(
        reopen(&paths).artifact_cache_limit(),
        ArtifactCacheLimit::Limited(2 * 1024 * 1024 * 1024)
    );
}

#[tokio::test]
async fn a_published_instance_record_survives_a_restart_exactly() {
    let base = tempfile::tempdir().expect("temp dir");
    let paths = roots(&base);

    let created = {
        let registry = ephervex_storage::Registry::open(paths.clone()).expect("registry");
        let record = registry
            .create_client(
                NewClient::new("Repro", "1.12.2", LoaderSelection::vanilla())
                    .with_storage(StorageClass::Persistent),
            )
            .expect("create");
        registry.publish_client(&record).expect("publish");
        record
    };

    let app = reopen(&paths);
    let restored = app.client(created.id).expect("the record survives");
    assert_eq!(restored.id, created.id);
    assert_eq!(restored.name, created.name);
    assert_eq!(restored.minecraft_version, created.minecraft_version);
    assert_eq!(restored.loader, created.loader);
    assert_eq!(restored.storage, created.storage);
    assert_eq!(restored.root, created.root);
    assert_eq!(app.list_clients().expect("list").len(), 1);
}

#[tokio::test]
async fn an_unpublished_instance_does_not_come_back_after_a_restart() {
    // The other half of the invariant: a creation interrupted before
    // publication must still be invisible to the next process, and the next
    // reconciliation must report it rather than adopt it.
    let base = tempfile::tempdir().expect("temp dir");
    let paths = roots(&base);

    let root = {
        let registry = ephervex_storage::Registry::open(paths.clone()).expect("registry");
        registry
            .create_client(
                NewClient::new("Interrupted", "1.20.1", LoaderSelection::vanilla())
                    .with_storage(StorageClass::Persistent),
            )
            .expect("create")
            .root
    };

    let app = reopen(&paths);
    assert!(app.list_clients().expect("list").is_empty());
    let report = app.reconcile().expect("reconcile");
    assert_eq!(report.unfinished_instances.len(), 1);
    assert!(app.list_clients().expect("list").is_empty());
    assert!(
        root.is_dir(),
        "the interrupted work is reported, not deleted"
    );
}
