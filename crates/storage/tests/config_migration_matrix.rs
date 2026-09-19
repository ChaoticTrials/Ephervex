//! Every configuration shape Ephervex has written, read by the current build.
//!
//! Upgrading must never cost a user a setting. This file is the explicit matrix
//! that proves it: one representative document per shape the launcher has
//! actually produced, each read by today's code and checked for what it is
//! supposed to keep.
//!
//! # What "round trip" means here, and what it does not
//!
//! A byte-identical round trip is **not** guaranteed and is deliberately not
//! asserted. Unknown keys live in flattened `toml::Table` fields, so a rewrite
//! re-emits them after the named fields of their table; comments, blank lines,
//! quoting style, and key order are not retained; and credential-shaped keys
//! are removed on load by design (ADR 0004). What is guaranteed is *semantic*:
//! the loaded value, a load/save/load fixed point, and the specific
//! preservation the architecture promises, each checked by name.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ephervex_storage::config::{
    ApplicationDisplayConfig, CONFIG_SCHEMA_VERSION, CacheConfig, ModsConfig,
};
use ephervex_storage::{Config, Paths};
use tempfile::TempDir;

/// The oldest shape: before application display settings, generic Minecraft
/// preferences, the mod catalog preference, and the artifact cache budget.
const BEFORE_GENERIC_PREFERENCES: &str = r#"
schema_version = 1
language = "en-US"
persistent_instance_root = ""

[java]
auto_detect = true
auto_download = true
min_memory_mib = 512
max_memory_mib = 4096
jvm_args = []

[server_defaults]
online_mode = false
network_access = "localhost"
port = 25565

[eula]
accepted = false
accepted_at = ""
reference = ""
"#;

/// The shape written once managed preferences existed, holding exactly the
/// seven that predate the generated product catalog.
const ORIGINAL_SEVEN_PREFERENCES: &str = r#"
schema_version = 1
language = "en-US"
persistent_instance_root = ""

[java]
auto_detect = true

[minecraft_preferences.gui_scale]
enabled = true
value = "2"

[minecraft_preferences.language]
enabled = true
value = "en_us"

[minecraft_preferences.fullscreen]
enabled = false
value = "false"

[minecraft_preferences.fov]
enabled = true
value = "90"

[minecraft_preferences.render_distance]
enabled = true
value = "12"

[minecraft_preferences.simulation_distance]
enabled = false
value = "10"

[minecraft_preferences.window_size]
enabled = true
value = "1280x720"
"#;

/// The current shape, with every table this build knows about.
const CURRENT: &str = r#"
schema_version = 1
language = "en-US"
persistent_instance_root = "/mnt/big/instances"
active_account = ""

[display]
text_scale = "large"
technical_text_size = "compact"
show_linked_servers = true

[java]
auto_detect = true
auto_download = false
min_memory_mib = 1024
max_memory_mib = 8192
jvm_args = ["-XX:+UseG1GC"]

[mods]
default_catalog = "curseforge"

[server_defaults]
online_mode = false
network_access = "lan"
port = 25566
make_default_account_operator = true
operator_permission_level = 4

[eula]
accepted = true
accepted_at = "2026-01-01T00:00:00Z"
reference = "https://aka.ms/MinecraftEULA"

[cache]
artifact_limit_mode = "limited"
artifact_limit_bytes = 21474836480

[minecraft_preferences.gui_scale]
enabled = true
value = "3"
"#;

fn paths(base: &TempDir) -> Paths {
    let paths = Paths::rooted_at(base.path());
    paths.ensure_base_directories().expect("create roots");
    paths
}

/// Reads a document, saves it, and reads it back.
///
/// The returned pair is the value the file meant and the value a rewrite
/// preserves. They must be equal: a load/save/load cycle is a fixed point.
fn round_trip(document: &str) -> (Config, Config) {
    let base = tempfile::tempdir().expect("temp dir");
    let paths = paths(&base);
    std::fs::write(paths.config_file(), document).expect("write the historical file");

    let loaded = Config::load_or_default(&paths).expect("an older shape still loads");
    loaded.save(&paths).expect("save");
    let reloaded = Config::load_or_default(&paths).expect("reload");
    (loaded, reloaded)
}

#[test]
fn a_configuration_written_before_generic_preferences_loads_with_current_defaults() {
    let (loaded, reloaded) = round_trip(BEFORE_GENERIC_PREFERENCES);

    assert_eq!(loaded.schema_version, CONFIG_SCHEMA_VERSION);
    assert_eq!(loaded.language, "en-US");
    assert!(loaded.minecraft_preferences.is_empty());
    // Absent tables take this build's defaults rather than failing the file.
    assert_eq!(loaded.display, ApplicationDisplayConfig::default());
    assert_eq!(loaded.mods, ModsConfig::default());
    assert_eq!(loaded.cache, CacheConfig::default());
    // The settings the file *did* carry are exactly what it said.
    assert!(loaded.java.auto_detect);
    assert_eq!(loaded.java.max_memory_mib, 4096);
    assert_eq!(loaded.server_defaults.port, 25565);
    assert!(!loaded.eula.accepted);

    assert_eq!(loaded, reloaded, "a rewrite is a fixed point");
}

#[test]
fn the_seven_preferences_that_predate_the_catalog_survive_unchanged() {
    let (loaded, reloaded) = round_trip(ORIGINAL_SEVEN_PREFERENCES);

    for id in [
        "gui_scale",
        "language",
        "fullscreen",
        "fov",
        "render_distance",
        "simulation_distance",
        "window_size",
    ] {
        let setting = loaded
            .minecraft_preferences
            .get(id)
            .unwrap_or_else(|| panic!("{id} survived the upgrade"));
        assert_eq!(
            setting,
            reloaded.minecraft_preferences.get(id).expect("reloaded"),
            "{id} changed across a rewrite"
        );
    }
    // Managed and unmanaged are different states, and both are preserved.
    assert!(loaded.minecraft_preferences["gui_scale"].enabled);
    assert_eq!(loaded.minecraft_preferences["gui_scale"].value, "2");
    assert!(!loaded.minecraft_preferences["fullscreen"].enabled);
    assert_eq!(loaded.minecraft_preferences["fullscreen"].value, "false");

    assert_eq!(loaded, reloaded);
}

#[test]
fn the_current_configuration_round_trips_with_every_setting_intact() {
    let (loaded, reloaded) = round_trip(CURRENT);

    assert_eq!(loaded.persistent_instance_root, "/mnt/big/instances");
    assert_eq!(loaded.java.jvm_args, vec!["-XX:+UseG1GC".to_owned()]);
    assert_eq!(loaded.mods.default_catalog, "curseforge");
    assert_eq!(loaded.server_defaults.port, 25566);
    assert!(loaded.eula.accepted);
    assert_eq!(loaded, reloaded);
}

#[test]
fn a_preference_id_a_newer_build_wrote_survives_an_unrelated_save() {
    // The downgrade case: an older binary must carry a newer one's settings
    // through untouched rather than dropping what it cannot present.
    let document = format!(
        "{CURRENT}\n\
         [minecraft_preferences.some_setting_from_the_future]\n\
         enabled = true\nvalue = \"17\"\n"
    );
    let (loaded, reloaded) = round_trip(&document);

    let future = loaded
        .minecraft_preferences
        .get("some_setting_from_the_future")
        .expect("an unknown preference id is carried, not dropped");
    assert!(future.enabled);
    assert_eq!(future.value, "17");
    assert_eq!(loaded, reloaded);
}

#[test]
fn an_unknown_key_in_every_open_table_survives_an_unrelated_save() {
    // Each open table is listed by name: a table that gains a `flatten` and
    // does not get its own row here would pass by accident.
    // A root key has to precede the first table header, or TOML puts it inside
    // that table instead of at the root.
    let document = format!(
        "{}\n\
         [accounts.2f8a1c40-0000-4000-8000-000000000001]\n\
         name = \"Steve\"\nkind = \"microsoft\"\n\
         added_at = \"2026-01-01T00:00:00Z\"\n\
         last_authenticated_at = \"2026-01-02T00:00:00Z\"\n\
         future_account_key = \"account\"\n",
        CURRENT.replace(
            "active_account = \"\"",
            "active_account = \"\"\nfuture_root_key = \"root\""
        )
    );
    let base = tempfile::tempdir().expect("temp dir");
    let paths = paths(&base);
    // The nested keys go into their tables through a second document so the
    // table headers above stay valid TOML.
    let document = document
        .replace("[java]\n", "[java]\nfuture_java_key = \"java\"\n")
        .replace(
            "[display]\n",
            "[display]\nfuture_display_key = \"display\"\n",
        )
        .replace("[mods]\n", "[mods]\nfuture_mods_key = \"mods\"\n")
        .replace("[cache]\n", "[cache]\nfuture_cache_key = \"cache\"\n")
        .replace("[eula]\n", "[eula]\nfuture_eula_key = \"eula\"\n")
        .replace(
            "[server_defaults]\n",
            "[server_defaults]\nfuture_server_key = \"server\"\n",
        );
    std::fs::write(paths.config_file(), &document).expect("write");

    let loaded = Config::load_or_default(&paths).expect("load");
    loaded.save(&paths).expect("save");
    let reloaded = Config::load_or_default(&paths).expect("reload");

    for (table, key, expected) in [
        (loaded.unknown.clone(), "future_root_key", "root"),
        (loaded.java.unknown.clone(), "future_java_key", "java"),
        (
            loaded.display.unknown.clone(),
            "future_display_key",
            "display",
        ),
        (loaded.mods.unknown.clone(), "future_mods_key", "mods"),
        (loaded.cache.unknown.clone(), "future_cache_key", "cache"),
        (loaded.eula.unknown.clone(), "future_eula_key", "eula"),
        (
            loaded.server_defaults.unknown.clone(),
            "future_server_key",
            "server",
        ),
    ] {
        assert_eq!(
            table.get(key).and_then(toml::Value::as_str),
            Some(expected),
            "{key} was dropped"
        );
    }
    assert_eq!(
        loaded.accounts["2f8a1c40-0000-4000-8000-000000000001"]
            .unknown
            .get("future_account_key")
            .and_then(toml::Value::as_str),
        Some("account")
    );
    assert_eq!(loaded, reloaded, "a rewrite preserves every unknown key");
}

#[test]
fn a_catalog_name_this_build_cannot_resolve_is_kept_verbatim() {
    // Three states stay distinguishable: nothing recorded, a value this build
    // knows, and a value only a newer build knows.
    let document = CURRENT.replace(
        "default_catalog = \"curseforge\"",
        "default_catalog = \"a-catalog-from-the-future\"",
    );
    let (loaded, reloaded) = round_trip(&document);

    assert_eq!(loaded.mods.default_catalog, "a-catalog-from-the-future");
    assert_eq!(
        reloaded.mods.default_catalog, "a-catalog-from-the-future",
        "an unresolvable catalog is written back exactly as found"
    );
}

#[test]
fn an_unresolvable_typed_setting_fails_the_document_rather_than_being_coerced() {
    // Where preservation is impossible -- a closed enum this build cannot
    // represent -- the file is refused rather than silently read as one of the
    // values this build does know.
    let base = tempfile::tempdir().expect("temp dir");
    let paths = paths(&base);
    for (field, replacement) in [
        ("text_scale = \"large\"", "text_scale = \"colossal\""),
        (
            "technical_text_size = \"compact\"",
            "technical_text_size = \"microscopic\"",
        ),
        (
            "network_access = \"lan\"",
            "network_access = \"whole-internet\"",
        ),
        (
            "artifact_limit_mode = \"limited\"",
            "artifact_limit_mode = \"elastic\"",
        ),
    ] {
        let document = CURRENT.replace(field, replacement);
        std::fs::write(paths.config_file(), &document).expect("write");
        assert!(
            Config::load_or_default(&paths).is_err(),
            "{replacement} must fail the document rather than be guessed at"
        );
    }
}

#[test]
fn a_newer_schema_version_is_refused_without_touching_the_file() {
    let base = tempfile::tempdir().expect("temp dir");
    let paths = paths(&base);
    let document = CURRENT.replace("schema_version = 1", "schema_version = 2");
    std::fs::write(paths.config_file(), &document).expect("write");

    assert!(Config::load_or_default(&paths).is_err());
    assert_eq!(
        std::fs::read_to_string(paths.config_file()).expect("read"),
        document,
        "a refused load leaves the original bytes exactly as they were"
    );
}

#[test]
fn reading_a_configuration_never_rewrites_it() {
    // Startup loads the configuration and writes nothing. A build that
    // normalized on read would rewrite a newer build's file every time the
    // launcher started, which is how forward-compatible keys get lost.
    let base = tempfile::tempdir().expect("temp dir");
    let paths = paths(&base);
    let document = format!("{CURRENT}\nfuture_root_key = \"root\"\n");
    std::fs::write(paths.config_file(), &document).expect("write");
    let before = std::fs::metadata(paths.config_file())
        .expect("metadata")
        .modified()
        .expect("mtime");

    for _ in 0..3 {
        let _ = Config::load_or_default(&paths).expect("load");
    }

    assert_eq!(
        std::fs::read_to_string(paths.config_file()).expect("read"),
        document,
        "loading must not rewrite the file"
    );
    assert_eq!(
        std::fs::metadata(paths.config_file())
            .expect("metadata")
            .modified()
            .expect("mtime"),
        before
    );
}

#[test]
fn a_file_without_the_dependency_warning_key_reads_as_warning() {
    // Every configuration written before the setting existed, which is all of
    // them. Silence has to mean "warn me", or upgrading would quietly stop
    // telling a user that turning one mod off takes three more with it.
    let base = TempDir::new().expect("temp dir");
    let paths = paths(&base);
    std::fs::write(paths.config_file(), CURRENT).expect("write");

    let loaded = Config::load_or_default(&paths).expect("load");
    assert!(loaded.mods.warn_on_dependency_impact);
}

#[test]
fn a_file_without_the_dependency_following_key_reads_as_following() {
    // The same silence rule, for the setting that decides whether a change
    // acts on other mods at all: an upgrade must not quietly stop carrying a
    // library along with the mods that need it.
    let base = TempDir::new().expect("temp dir");
    let paths = paths(&base);
    std::fs::write(paths.config_file(), CURRENT).expect("write");

    let loaded = Config::load_or_default(&paths).expect("load");
    assert!(loaded.mods.follow_mod_dependencies);
}

#[test]
fn the_two_dependency_settings_are_recorded_independently() {
    let base = TempDir::new().expect("temp dir");
    let paths = paths(&base);
    std::fs::write(paths.config_file(), CURRENT).expect("write");

    let mut loaded = Config::load_or_default(&paths).expect("load");
    loaded.mods.follow_mod_dependencies = false;
    loaded.save(&paths).expect("save");

    let reloaded = Config::load_or_default(&paths).expect("reload");
    assert!(!reloaded.mods.follow_mod_dependencies);
    assert!(
        reloaded.mods.warn_on_dependency_impact,
        "turning the cascade off must not rewrite the answer to the other question"
    );
}

#[test]
fn turning_the_dependency_warning_off_survives_a_save_and_reload() {
    let base = TempDir::new().expect("temp dir");
    let paths = paths(&base);
    // The unknown key goes in the very table this setting lives in, which is
    // the one a save of it rewrites.
    let document = CURRENT.replace("[mods]\n", "[mods]\nfuture_mods_key = \"mods\"\n");
    std::fs::write(paths.config_file(), document).expect("write");

    let mut loaded = Config::load_or_default(&paths).expect("load");
    assert!(loaded.mods.warn_on_dependency_impact);
    loaded.mods.warn_on_dependency_impact = false;
    loaded.save(&paths).expect("save");

    let reloaded = Config::load_or_default(&paths).expect("reload");
    assert!(!reloaded.mods.warn_on_dependency_impact);
    assert_eq!(
        reloaded.mods.default_catalog, "curseforge",
        "the neighbouring setting is untouched"
    );
    assert!(
        reloaded.mods.unknown.contains_key("future_mods_key"),
        "a newer build's key in this table survives a change to it"
    );
}
