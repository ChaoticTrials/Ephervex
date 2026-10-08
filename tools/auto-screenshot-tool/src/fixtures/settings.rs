//! Synthetic Settings pages: General, Minecraft defaults, Java, Providers & APIs,
//! Storage, Disk usage, and About & Licenses.

use ephervex_gui::minecraft_defaults::{CATEGORIES, WINDOW_ASPECTS, category_label};
use ephervex_gui::presentation::{cache_limit_choices, cache_limit_units};
use ephervex_gui::ui::{AppWindow, DiskUsageRow, JavaRow, LicenseRow, MinecraftPreferenceRow};
use ephervex_i18n::{Arg, lookup, lookup_args};
use slint::{ModelRc, SharedString, VecModel};

use super::rows;

fn strings(values: &[&str]) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(
        values
            .iter()
            .map(|value| SharedString::from(*value))
            .collect::<Vec<_>>(),
    ))
}

fn open_settings(ui: &AppWindow, tab: &str) {
    ui.set_workspace_open(false);
    ui.set_page(4);
    ui.set_settings_tab(tab.into());
}

pub(crate) fn general(ui: &AppWindow) {
    open_settings(ui, "General");
    ui.set_show_linked_servers(true);
    ui.set_make_default_account_operator(true);
    ui.set_operator_permission_level(4);
    ui.set_application_text_scale(100);
    ui.set_technical_text_size(12);
    ui.set_theme_labels(strings(&[
        &lookup("gui-color-scheme-dark"),
        &lookup("gui-color-scheme-light"),
        "Midnight Blueprint",
    ]));
    ui.set_theme_ids(strings(&["dark", "light", "user:midnight-blueprint"]));
    ui.set_themes_folder("/demo/config/ephervex/themes".into());
    ui.set_theme_problems(SharedString::default());
    ui.set_follow_mod_dependencies(true);
    ui.set_warn_on_dependency_impact(true);
    ui.set_trusted_pack_hosts(strings(&[
        "packs.example.invalid",
        "mirror.example.invalid",
    ]));
    ui.set_favorite_minecraft_versions(strings(&["1.21.1"]));
}

const GENERAL_RELEASES: [&str; 7] = [
    "26.2", "1.21.1", "1.20.1", "1.19.4", "1.18.2", "1.16.5", "1.12.2",
];

/// The common shape of a Minecraft defaults row, before an editor fills it in.
fn preference(id: &str, label: &str, editor: &str) -> MinecraftPreferenceRow {
    MinecraftPreferenceRow {
        id: id.into(),
        label: label.into(),
        technical_id: id.into(),
        editor: editor.into(),
        overridden: true,
        can_reset: true,
        enum_index: -1,
        enum_labels: strings(&[]),
        status: lookup("gui-minecraft-compat-supported").into(),
        status_kind: "supported".into(),
        ..MinecraftPreferenceRow::default()
    }
}

fn minecraft_rows() -> Vec<MinecraftPreferenceRow> {
    let degrees = |value: &str| lookup_args("gui-minecraft-degrees", [("value", Arg::from(value))]);
    vec![
        MinecraftPreferenceRow {
            section_heading: lookup("gui-minecraft-section-general").into(),
            number_min: 30.0,
            number_max: 110.0,
            number_step: 1.0,
            number_value: 90.0,
            number_readout: degrees("90").into(),
            ..preference("fov", &lookup("gui-minecraft-preference-fov"), "slider")
        },
        MinecraftPreferenceRow {
            bool_value: true,
            dirty: true,
            ..preference("fullscreen", "Fullscreen", "toggle")
        },
        MinecraftPreferenceRow {
            enum_labels: strings(&["Fast", "Fancy", "Fabulous", "Custom"]),
            enum_index: 2,
            status: lookup("gui-minecraft-compat-unavailable").into(),
            status_kind: "unavailable".into(),
            status_detail: lookup_args(
                "gui-minecraft-compat-unavailable-detail",
                [("value", Arg::from("1.20.1"))],
            )
            .into(),
            ..preference("graphics_preset", "Graphics preset", "enum")
        },
        MinecraftPreferenceRow {
            text_value: "1600".into(),
            height_text: "900".into(),
            aspect_index: 1,
            aspect_locked: true,
            dirty: true,
            ..preference("window_size", "Window size", "window")
        },
    ]
}

pub(crate) fn minecraft_defaults(ui: &AppWindow) {
    open_settings(ui, "Minecraft");
    ui.set_minecraft_categories(rows(
        CATEGORIES
            .iter()
            .map(|category| SharedString::from(category_label(*category)))
            .collect(),
    ));
    ui.set_minecraft_aspect_labels(rows(
        WINDOW_ASPECTS
            .iter()
            .map(|aspect| SharedString::from(aspect.label()))
            .collect(),
    ));
    ui.set_minecraft_category_index(1);
    ui.set_minecraft_only_overridden(true);
    ui.set_minecraft_releases(strings(&GENERAL_RELEASES));
    ui.set_minecraft_preview_release("1.20.1".into());
    ui.set_minecraft_preference_rows(rows(minecraft_rows()));
    ui.set_minecraft_dirty(true);
    ui.set_minecraft_can_save(true);
    ui.set_minecraft_dirty_count(2);
    ui.set_minecraft_total_count(181);
}

fn runtime(version: &str, vendor: &str, path: &str, origin: &str, major: u8) -> JavaRow {
    JavaRow {
        path: path.into(),
        version: version.into(),
        vendor: vendor.into(),
        origin: origin.into(),
        compatible: format!("Java {major}").into(),
    }
}

pub(crate) fn java(ui: &AppWindow) {
    open_settings(ui, "Java");
    ui.set_java_auto_detect(true);
    ui.set_java_auto_download(true);
    ui.set_java_memory_min("1024".into());
    ui.set_java_memory_max("6144".into());
    ui.set_java_preferred_path("/demo/jvm/temurin-17/bin/java".into());
    ui.set_java_runtimes(rows(vec![
        runtime(
            "1.8.0_432",
            "Azul Systems, Inc.",
            "/demo/usr/lib/jvm/zulu-8/bin/java",
            "system",
            8,
        ),
        runtime(
            "17.0.13",
            "Eclipse Adoptium",
            "/demo/jvm/temurin-17/bin/java",
            "java-home",
            17,
        ),
        runtime(
            "21.0.5",
            "Eclipse Adoptium",
            "/demo/ephervex/runtimes/temurin-21/bin/java",
            "managed",
            21,
        ),
        runtime(
            "25.0.1",
            "Eclipse Adoptium",
            "/demo/home/.sdkman/candidates/java/25-tem/bin/java",
            "sdkman",
            25,
        ),
    ]));
}

pub(crate) fn providers(ui: &AppWindow) {
    open_settings(ui, "Providers & APIs");
    ui.set_default_mod_catalog("curseforge".into());
    ui.set_configured_mod_catalog("curseforge".into());
    ui.set_default_mod_catalog_is_custom(true);
    ui.set_microsoft_client_id(SharedString::default());
    ui.set_effective_microsoft_client_id("00000000-0000-4000-8000-0000000000a1".into());
    ui.set_microsoft_client_id_is_custom(false);
    ui.set_user_agent("DemoQA/1.0 (+https://example.invalid/contact)".into());
    ui.set_effective_user_agent("DemoQA/1.0 (+https://example.invalid/contact)".into());
    ui.set_user_agent_is_custom(true);
    ui.set_curseforge_key_configured(true);
    ui.set_curseforge_key_default_in_use(false);
    ui.set_curseforge_build_key_available(false);
    ui.set_curseforge_api_key(SharedString::default());
}

const CUSTOM_STORAGE_ROOT: &str = "/demo/mnt/fast-ssd/ephervex";
const DEFAULT_STORAGE_ROOT: &str = "/demo/data/ephervex";

pub(crate) fn storage(ui: &AppWindow) {
    open_settings(ui, "Storage");
    ui.set_storage_root(CUSTOM_STORAGE_ROOT.into());
    ui.set_persistent_root(CUSTOM_STORAGE_ROOT.into());
    ui.set_default_persistent_root(DEFAULT_STORAGE_ROOT.into());
    ui.set_persistent_root_is_custom(true);
    ui.set_watched_download_folders(strings(&[
        "/demo/home/Downloads",
        "/demo/home/Downloads/modpacks",
    ]));
    let choices = cache_limit_choices();
    let limit = choices
        .iter()
        .position(|choice| choice.label == "10 GiB")
        .unwrap_or_default();
    ui.set_artifact_cache_limit_labels(rows(
        choices
            .iter()
            .map(|choice| SharedString::from(choice.label.as_str()))
            .collect(),
    ));
    ui.set_artifact_cache_limit_index(i32::try_from(limit).unwrap_or_default());
    ui.set_artifact_cache_custom_index(
        i32::try_from(choices.len().saturating_sub(2)).unwrap_or(-1),
    );
    ui.set_artifact_cache_units(rows(
        cache_limit_units()
            .iter()
            .map(|unit| SharedString::from(unit.as_str()))
            .collect(),
    ));
    ui.set_artifact_cache_custom_value("10".into());
    ui.set_artifact_cache_custom_unit("GiB".into());
    ui.set_artifact_cache_used("6.3 GiB".into());
    ui.set_artifact_cache_total("10 GiB".into());
    ui.set_artifact_cache_usage_known(true);
    ui.set_artifact_cache_usage_exact(true);
    ui.set_artifact_cache_over_budget(false);
}

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;

/// Mirrors the launcher's binary size reading: whole units bare, others to one decimal.
fn size(bytes: u64) -> String {
    for (scale, label) in [(GIB, "GiB"), (MIB, "MiB")] {
        if bytes >= scale {
            if bytes % scale == 0 {
                return format!("{} {label}", bytes / scale);
            }
            #[allow(clippy::cast_precision_loss)]
            let value = bytes as f64 / scale as f64;
            return format!("{value:.1} {label}");
        }
    }
    format!("{bytes} B")
}

/// One category: id, name key, detail key, size in MiB, and its folders.
type Category = (
    &'static str,
    &'static str,
    &'static str,
    u64,
    &'static [&'static str],
);

const CATEGORIES_MEASURED: [Category; 10] = [
    (
        "shared_game_files",
        "gui-disk-usage-shared-game-files",
        "gui-disk-usage-shared-game-files-detail",
        6246,
        &["/demo/ephervex/shared"],
    ),
    (
        "client_instances",
        "gui-disk-usage-client-instances",
        "gui-disk-usage-client-instances-detail",
        4710,
        &["/demo/ephervex/instances"],
    ),
    (
        "download_cache",
        "gui-disk-usage-download-cache",
        "gui-disk-usage-download-cache-detail",
        2970,
        &[
            "/demo/cache/ephervex/downloads",
            "/demo/cache/ephervex/metadata",
        ],
    ),
    (
        "java_runtimes",
        "gui-disk-usage-java-runtimes",
        "gui-disk-usage-java-runtimes-detail",
        1830,
        &["/demo/ephervex/runtimes"],
    ),
    (
        "mods",
        "gui-disk-usage-mods",
        "gui-disk-usage-mods-detail",
        1320,
        &[],
    ),
    (
        "local_servers",
        "gui-disk-usage-local-servers",
        "gui-disk-usage-local-servers-detail",
        870,
        &["/demo/ephervex/servers"],
    ),
    (
        "snapshots",
        "gui-disk-usage-snapshots",
        "gui-disk-usage-snapshots-detail",
        410,
        &[],
    ),
    (
        "logs",
        "gui-disk-usage-logs",
        "gui-disk-usage-logs-detail",
        138,
        &["/demo/ephervex/logs"],
    ),
    (
        "recovery",
        "gui-disk-usage-recovery",
        "gui-disk-usage-recovery-detail",
        64,
        &[],
    ),
    (
        "launcher_state",
        "gui-disk-usage-launcher-state",
        "gui-disk-usage-launcher-state-detail",
        12,
        &["/demo/config/ephervex"],
    ),
];

/// The retention split of the measurement above: id, name key, detail key, MiB.
/// Persistent takes whatever the other three leave.
fn retention_parts(total: u64) -> [(&'static str, &'static str, &'static str, u64); 4] {
    let regenerable = (6246 + 2970 + 1830) * MIB;
    let temporary = 1900 * MIB;
    let recovery = 64 * MIB;
    let persistent = total - regenerable - temporary - recovery;
    [
        (
            "persistent",
            "gui-disk-usage-retention-persistent",
            "gui-disk-usage-retention-persistent-detail",
            persistent,
        ),
        (
            "temporary",
            "gui-disk-usage-retention-temporary",
            "gui-disk-usage-retention-temporary-detail",
            temporary,
        ),
        (
            "regenerable",
            "gui-disk-usage-retention-regenerable",
            "gui-disk-usage-retention-regenerable-detail",
            regenerable,
        ),
        (
            "recovery",
            "gui-disk-usage-retention-recovery",
            "gui-disk-usage-retention-recovery-detail",
            recovery,
        ),
    ]
}

#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn share(bytes: u64, total: u64) -> f32 {
    (bytes as f64 / total as f64) as f32
}

fn usage_total() -> u64 {
    CATEGORIES_MEASURED
        .iter()
        .map(|category| category.3 * MIB)
        .sum()
}

pub(crate) fn disk_usage(ui: &AppWindow) {
    open_settings(ui, "Disk Usage");
    let total = usage_total();
    ui.set_disk_usage_state("measured".into());
    ui.set_disk_usage_total(size(total).into());
    ui.set_disk_usage_measured_at(
        lookup_args(
            "gui-disk-usage-measured-at",
            [("value", Arg::from("2026-10-08 14:32"))],
        )
        .into(),
    );
    ui.set_disk_usage_retention(rows(
        retention_parts(total)
            .into_iter()
            .map(|(id, label, detail, bytes)| DiskUsageRow {
                id: id.into(),
                label: lookup(label).into(),
                detail: lookup(detail).into(),
                size: size(bytes).into(),
                fraction: share(bytes, total),
                locations: strings(&[]),
            })
            .collect(),
    ));
    ui.set_disk_usage_categories(rows(
        CATEGORIES_MEASURED
            .iter()
            .map(|(id, label, detail, mib, locations)| DiskUsageRow {
                id: (*id).into(),
                label: lookup(label).into(),
                detail: lookup(detail).into(),
                size: size(mib * MIB).into(),
                fraction: share(mib * MIB, total),
                locations: strings(locations),
            })
            .collect(),
    ));
    ui.set_disk_usage_incomplete(true);
    ui.set_disk_usage_incomplete_text(
        lookup_args("gui-disk-usage-incomplete", [("count", Arg::from(1_i64))]).into(),
    );
    ui.set_disk_usage_unreadable(strings(&[
        "/demo/ephervex/instances/herobrine-sighting/.minecraft/saves/backup-2",
    ]));
}

pub(crate) fn about(ui: &AppWindow) {
    open_settings(ui, "About & Licenses");
}

/// Rows from the head of the alphabetical dependency list: name, version, SPDX.
const PACKAGES: [(&str, &str, &str); 24] = [
    ("ab_glyph", "0.2.32", "Apache-2.0"),
    ("ab_glyph_rasterizer", "0.1.10", "Apache-2.0"),
    ("accesskit", "0.24.1", "MIT OR Apache-2.0"),
    ("accesskit_atspi_common", "0.19.1", "MIT OR Apache-2.0"),
    ("accesskit_consumer", "0.38.0", "MIT OR Apache-2.0"),
    ("accesskit_unix", "0.22.1", "MIT OR Apache-2.0"),
    ("accesskit_winit", "0.33.2", "Apache-2.0"),
    ("adler2", "2.0.1", "0BSD OR MIT OR Apache-2.0"),
    ("aes", "0.8.4", "MIT OR Apache-2.0"),
    ("ahash", "0.8.12", "MIT OR Apache-2.0"),
    ("allocator-api2", "0.2.21", "MIT OR Apache-2.0"),
    ("anstream", "1.0.0", "MIT OR Apache-2.0"),
    ("anstyle", "1.0.14", "MIT OR Apache-2.0"),
    ("anyhow", "1.0.104", "MIT OR Apache-2.0"),
    ("arc-swap", "1.9.2", "MIT OR Apache-2.0"),
    ("arrayref", "0.3.9", "BSD-2-Clause"),
    ("arrayvec", "0.7.8", "MIT OR Apache-2.0"),
    ("async-channel", "2.5.0", "Apache-2.0 OR MIT"),
    ("async-executor", "1.14.0", "Apache-2.0 OR MIT"),
    ("async-io", "2.6.0", "Apache-2.0 OR MIT"),
    ("async-lock", "3.4.2", "Apache-2.0 OR MIT"),
    ("async-process", "2.5.0", "Apache-2.0 OR MIT"),
    ("async-task", "4.7.1", "Apache-2.0 OR MIT"),
    ("atspi", "0.29.0", "Apache-2.0 OR MIT"),
];

pub(crate) fn third_party_licenses(ui: &AppWindow) {
    about(ui);
    ui.set_third_party_packages(rows(
        PACKAGES
            .iter()
            .map(|(name, version, license)| LicenseRow {
                name: (*name).into(),
                version: (*version).into(),
                license: (*license).into(),
                text: SharedString::default(),
            })
            .collect(),
    ));
    ui.set_third_party_summary(
        lookup_args(
            "gui-third-party-summary",
            [("crates", Arg::from(524)), ("licenses", Arg::from(140))],
        )
        .into(),
    );
    ui.set_third_party_error(SharedString::default());
    ui.set_third_party_open(-1);
    ui.set_modal("third-party".into());
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use crate::capture::{CaptureOptions, Dimensions, ThemeName, generate};
    use crate::catalog::STATES;

    #[test]
    fn measured_usage_adds_up_and_every_text_resolves() {
        let _session = crate::capture::render_session();
        let total = usage_total();
        let retention: u64 = retention_parts(total).iter().map(|part| part.3).sum();
        assert_eq!(retention, total);
        for (_, label, detail, _, _) in CATEGORIES_MEASURED {
            assert!(ephervex_i18n::has(label) && ephervex_i18n::has(detail));
        }
        assert!(
            cache_limit_choices()
                .iter()
                .any(|choice| choice.label == "10 GiB")
        );
    }

    #[test]
    fn every_settings_state_renders_at_full_hd() {
        let _session = crate::capture::render_session();
        let wanted = [
            "settings-general",
            "settings-minecraft-defaults",
            "settings-java",
            "settings-providers",
            "settings-storage",
            "settings-disk-usage",
            "about-licenses",
            "third-party-licenses-dialog",
        ];
        let states: Vec<_> = STATES
            .iter()
            .filter(|state| wanted.contains(&state.name))
            .copied()
            .collect();
        assert_eq!(states.len(), wanted.len());
        let output = tempfile::tempdir().expect("temporary output");
        let files = generate(&CaptureOptions {
            states: &states,
            themes: &[ThemeName::Light],
            sizes: &[Dimensions::HD],
            output: output.path(),
            derive_1080: false,
        })
        .expect("capture the settings states");
        assert_eq!(files.len(), wanted.len());
        assert!(files.iter().all(|file| file.is_file()));
    }
}
