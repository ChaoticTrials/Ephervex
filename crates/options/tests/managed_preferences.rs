//! The generic managed-preference engine against four exact Minecraft eras.
//!
//! Everything here is driven by the bundled product catalog and the bundled
//! compatibility dataset. Nothing reads a real installation and nothing
//! touches the network.
//!
//! What these tests defend is the boundary the whole feature rests on: a
//! preference is written only where an exact release was *proven* to store it
//! that way, a value the release cannot express is reported rather than moved
//! to one it can, and a release nobody has analyzed receives nothing at all.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

use ephervex_options::{
    ConfiguredPreference, ConfiguredPreferences, InputBinding, LanguageAvailability,
    MinecraftPreferenceCatalog, MinecraftPreferenceValue, MouseButton, OPTIONS_FILE_NAME,
    OptionsCompatibility, ParsedOptions, PhysicalKey, PreferenceDisposition, PreferenceId,
    ReleaseSupport, SkipReason, apply_managed_preferences, plan_managed_preferences,
};

/// The releases every cross-era assertion runs against: the pre-1.13 storage
/// era, the era that introduced serialized key names, the era simulation
/// distance exists in, and the current calendar-versioned canonical release.
const RELEASES: &[&str] = &["1.12.2", "1.16.5", "1.20.1", "26.2"];

fn catalog() -> &'static MinecraftPreferenceCatalog {
    MinecraftPreferenceCatalog::bundled_shared().expect("the bundled catalog is valid")
}

fn id(text: &str) -> PreferenceId {
    PreferenceId::parse(text).expect("a well-formed id")
}

fn managed(entries: &[(&str, MinecraftPreferenceValue)]) -> ConfiguredPreferences {
    let mut preferences = ConfiguredPreferences::new();
    for (name, value) in entries {
        preferences.insert(id(name), ConfiguredPreference::managed(value.clone()));
    }
    preferences
}

fn plan(
    version: &str,
    preferences: &ConfiguredPreferences,
    language: LanguageAvailability,
) -> ephervex_options::PreferencePlan {
    plan_managed_preferences(
        version,
        preferences,
        catalog(),
        &OptionsCompatibility::bundled(),
        language,
    )
}

fn disposition(
    version: &str,
    name: &str,
    value: MinecraftPreferenceValue,
) -> PreferenceDisposition {
    let preferences = managed(&[(name, value)]);
    plan(version, &preferences, LanguageAvailability::Verified)
        .entry(&id(name))
        .expect("the preference is configured")
        .disposition
        .clone()
}

fn apply(
    game: &Path,
    version: &str,
    preferences: &ConfiguredPreferences,
    language: LanguageAvailability,
) -> ephervex_options::PreferenceApplication {
    apply_managed_preferences(
        game,
        version,
        preferences,
        catalog(),
        &OptionsCompatibility::bundled(),
        language,
    )
    .expect("apply")
}

fn applied_text(version: &str, preferences: &ConfiguredPreferences) -> String {
    let base = tempfile::tempdir().expect("temp dir");
    apply(
        base.path(),
        version,
        preferences,
        LanguageAvailability::Verified,
    );
    std::fs::read_to_string(base.path().join(OPTIONS_FILE_NAME)).unwrap_or_default()
}

// --------------------------------------------------------------- compatibility

#[test]
fn every_shipped_preference_resolves_to_one_of_exactly_three_answers() {
    let compatibility = OptionsCompatibility::bundled();
    for version in RELEASES {
        let support = compatibility.catalog_release_support(catalog(), version);
        assert_eq!(
            support.len(),
            catalog().descriptors.len(),
            "{version} must answer for every shipped preference"
        );
        assert!(
            support
                .values()
                .any(|answer| *answer == ReleaseSupport::Authoritative),
            "{version} proves nothing at all"
        );
    }
}

#[test]
fn a_setting_a_release_predates_is_unsupported_rather_than_unknown() {
    // Unsupported is a proven absence. Reporting it as unknown would turn a
    // finished answer into an open question.
    let compatibility = OptionsCompatibility::bundled();
    for (version, expected) in [
        ("1.12.2", ReleaseSupport::Unsupported),
        ("1.16.5", ReleaseSupport::Unsupported),
        ("1.20.1", ReleaseSupport::Authoritative),
        ("26.2", ReleaseSupport::Authoritative),
    ] {
        let support = compatibility.catalog_release_support(catalog(), version);
        assert_eq!(support["simulation_distance"], expected, "{version}");
    }
}

#[test]
fn a_release_nobody_has_analyzed_is_unknown_for_every_preference() {
    // The day after a Minecraft release. The launcher stays usable and writes
    // nothing it cannot vouch for, and nothing is inferred from 26.2.
    let compatibility = OptionsCompatibility::bundled();
    let support = compatibility.catalog_release_support(catalog(), "26.99");
    assert!(
        support
            .values()
            .all(|answer| *answer == ReleaseSupport::Unknown),
        "an unmapped release must not borrow a neighbor's answers"
    );

    let preferences = managed(&[("fullscreen", MinecraftPreferenceValue::Boolean(true))]);
    let base = tempfile::tempdir().expect("temp dir");
    let report = apply(
        base.path(),
        "26.99",
        &preferences,
        LanguageAvailability::Verified,
    );
    assert_eq!(report.unknown, vec![id("fullscreen")]);
    assert!(report.applied.is_empty());
    assert!(!base.path().join(OPTIONS_FILE_NAME).exists());
}

// ------------------------------------------------------------ representability

#[test]
fn a_number_no_release_bounded_is_written_as_chosen_rather_than_clamped() {
    // A clamp writes a value the user never chose into every instance created
    // afterwards. Where a release proved no bound there is nothing to refuse
    // against, so the number is written exactly as chosen. A render distance
    // maximum is chosen by runtime branches, so no release proves one.
    assert_eq!(
        disposition(
            "1.20.1",
            "render_distance",
            MinecraftPreferenceValue::Integer(999)
        ),
        PreferenceDisposition::Writable,
        "no release proves a render distance domain, so the codec has nothing to refuse"
    );
    // An older release proves the field of view transform but never proved its
    // endpoints, so a number outside them is written rather than refused
    // against a domain nobody established.
    assert_eq!(
        disposition(
            "1.12.2",
            "fov",
            MinecraftPreferenceValue::decimal(110.0).expect("a number")
        ),
        PreferenceDisposition::Writable,
        "1.12.2 proved no field of view domain"
    );
}

#[test]
fn an_enumeration_is_written_in_the_form_the_exact_release_stores() {
    let fast = MinecraftPreferenceValue::Enum("fast".to_owned());
    for (version, expected) in [
        ("1.12.2", "renderClouds:fast"),
        ("1.16.5", "renderClouds:fast"),
        ("26.2", "renderClouds:\"fast\""),
    ] {
        assert_eq!(
            disposition(version, "cloud_status", fast.clone()),
            PreferenceDisposition::Writable,
            "{version}"
        );
        assert!(
            applied_text(version, &managed(&[("cloud_status", fast.clone())])).contains(expected),
            "{version} stores the token as `{expected}`"
        );
    }
}

#[test]
fn a_value_outside_the_product_domain_is_reported_as_out_of_range_not_a_type_mismatch() {
    // The value has the right shape; the product descriptor refuses it.
    for version in RELEASES {
        assert_eq!(
            disposition(version, "gui_scale", MinecraftPreferenceValue::Integer(-1)),
            PreferenceDisposition::NotRepresentable(SkipReason::OutOfRange),
            "{version}: a negative interface scale is outside the proven domain"
        );
    }
}

#[test]
fn a_value_the_product_does_not_enumerate_is_reported_as_not_in_domain() {
    for version in RELEASES {
        assert_eq!(
            disposition(
                version,
                "cloud_status",
                MinecraftPreferenceValue::Enum("7".to_owned())
            ),
            PreferenceDisposition::NotRepresentable(SkipReason::NotInDomain),
            "{version}: 7 is not a cloud status choice"
        );
    }
}

#[test]
fn a_value_that_cannot_be_one_options_line_is_reported_as_unwritable() {
    for version in RELEASES {
        assert_eq!(
            disposition(
                version,
                "language",
                MinecraftPreferenceValue::LanguageCode("en_us\nlang:xx".to_owned())
            ),
            PreferenceDisposition::NotRepresentable(SkipReason::Unwritable),
            "{version}: a code with a line break cannot be written"
        );
    }
}

#[test]
fn a_value_of_the_wrong_shape_is_still_reported_as_a_type_mismatch() {
    for version in RELEASES {
        assert_eq!(
            disposition(
                version,
                "gui_scale",
                MinecraftPreferenceValue::Boolean(true)
            ),
            PreferenceDisposition::NotRepresentable(SkipReason::TypeMismatch),
            "{version}: a toggle is not an interface scale"
        );
    }
}

#[test]
fn a_window_size_is_written_whole_or_not_at_all() {
    let size = MinecraftPreferenceValue::WindowSize(ephervex_options::WindowSize {
        width: 1920,
        height: 1080,
    });
    for version in RELEASES {
        let preferences = managed(&[("window_size", size.clone())]);
        let entry_plan = plan(version, &preferences, LanguageAvailability::Verified);
        let entry = entry_plan.entry(&id("window_size")).expect("configured");
        assert_eq!(
            entry.disposition,
            PreferenceDisposition::Writable,
            "{version} keeps the window-size keys"
        );
        assert_eq!(
            entry.writes().len(),
            2,
            "{version} writes both edges or neither"
        );

        let text = applied_text(version, &preferences);
        assert!(text.contains("overrideWidth:1920"), "{version}");
        assert!(text.contains("overrideHeight:1080"), "{version}");
    }
}

#[test]
fn a_binding_reaches_each_era_in_that_eras_own_spelling() {
    // One release-neutral binding, two incompatible storage forms, and no
    // file copied between them.
    let binding =
        MinecraftPreferenceValue::KeyBinding(InputBinding::Keyboard(PhysicalKey::LeftShift));
    let preferences = managed(&[("keybind_inventory", binding)]);

    assert!(
        applied_text("1.12.2", &preferences).contains("key_key.inventory:42"),
        "1.12.2 stores the LWJGL 2 scan code"
    );
    for version in ["1.16.5", "1.20.1", "26.2"] {
        assert!(
            applied_text(version, &preferences)
                .contains("key_key.inventory:key.keyboard.left.shift"),
            "{version} stores the serialized key name"
        );
    }
}

#[test]
fn a_mouse_binding_and_an_unbound_key_reach_the_releases_that_establish_them() {
    for (value, expected) in [
        (
            MinecraftPreferenceValue::KeyBinding(InputBinding::Mouse(MouseButton::Middle)),
            "key_key.inventory:key.mouse.middle",
        ),
        (
            MinecraftPreferenceValue::KeyBinding(InputBinding::Unbound),
            "key_key.inventory:key.keyboard.unknown",
        ),
    ] {
        let preferences = managed(&[("keybind_inventory", value.clone())]);
        assert!(
            applied_text("1.20.1", &preferences).contains(expected),
            "1.20.1 {expected}"
        );

        // The pre-1.13 era stores mouse buttons and an unbound marker in the
        // same field, but Ephervex's compatibility research establishes
        // neither, so nothing is written rather than something plausible.
        assert_eq!(
            disposition("1.12.2", "keybind_inventory", value),
            PreferenceDisposition::NotRepresentable(SkipReason::InputUnavailable)
        );
    }
}

#[test]
fn a_key_the_pre_1_13_era_has_no_code_for_is_refused_rather_than_approximated() {
    // `keypad.equal` exists in the modern name space and has no scan code in
    // the pre-1.13 one. Binding an action to a neighboring key would be worse
    // than not binding it.
    let value =
        MinecraftPreferenceValue::KeyBinding(InputBinding::Keyboard(PhysicalKey::KeypadEqual));
    assert_eq!(
        disposition("1.12.2", "keybind_inventory", value.clone()),
        PreferenceDisposition::NotRepresentable(SkipReason::InputUnavailable)
    );
    assert!(
        !applied_text("1.12.2", &managed(&[("keybind_inventory", value.clone())]))
            .contains("key_key.inventory"),
    );
    assert_eq!(
        disposition("1.20.1", "keybind_inventory", value),
        PreferenceDisposition::Writable,
        "the modern era names it"
    );
}

// ----------------------------------------------------------------- application

#[test]
fn a_managed_value_writes_the_exact_text_the_release_stores() {
    let preferences = managed(&[
        (
            "fov",
            MinecraftPreferenceValue::decimal(90.0).expect("finite"),
        ),
        ("gui_scale", MinecraftPreferenceValue::Integer(3)),
    ]);
    // Every release stores `(degrees - 70) / 40`. None of them has ever put
    // degrees in the file.
    for version in RELEASES {
        assert!(
            applied_text(version, &preferences).contains("fov:0.5"),
            "{version} stores (degrees - 70) / 40"
        );
    }
    for version in RELEASES {
        assert!(
            applied_text(version, &preferences).contains("guiScale:3"),
            "{version}"
        );
    }
}

#[test]
fn an_unmanaged_preference_writes_nothing_at_all() {
    let mut preferences = ConfiguredPreferences::new();
    preferences.insert(
        id("fullscreen"),
        ConfiguredPreference::remembered(MinecraftPreferenceValue::Boolean(true)),
    );

    let base = tempfile::tempdir().expect("temp dir");
    let report = apply(
        base.path(),
        "26.2",
        &preferences,
        LanguageAvailability::Verified,
    );
    assert!(!report.changed_anything());
    assert!(
        !base.path().join(OPTIONS_FILE_NAME).exists(),
        "an empty options.txt is worse than none"
    );

    let entry_plan = plan("26.2", &preferences, LanguageAvailability::Verified);
    assert_eq!(
        entry_plan
            .entry(&id("fullscreen"))
            .expect("configured")
            .disposition,
        PreferenceDisposition::Unmanaged
    );
}

#[test]
fn an_unreadable_options_path_does_not_fail_an_application_with_nothing_to_write() {
    let base = tempfile::tempdir().expect("temp dir");
    let options = base.path().join(OPTIONS_FILE_NAME);
    std::fs::create_dir(&options).expect("a directory where the file would be");

    let report = apply(
        base.path(),
        "1.20.1",
        &ConfiguredPreferences::new(),
        LanguageAvailability::Verified,
    );

    assert!(!report.changed_anything());
    assert!(!report.created_file, "nothing was created");
    assert!(options.is_dir(), "the path is left exactly as found");
}

#[test]
fn an_unreadable_options_path_still_fails_an_application_that_has_something_to_write() {
    let base = tempfile::tempdir().expect("temp dir");
    std::fs::create_dir(base.path().join(OPTIONS_FILE_NAME)).expect("a directory");

    let result = apply_managed_preferences(
        base.path(),
        "1.20.1",
        &managed(&[("gui_scale", MinecraftPreferenceValue::Integer(3))]),
        catalog(),
        &OptionsCompatibility::bundled(),
        LanguageAvailability::Verified,
    );

    assert!(
        matches!(
            result,
            Err(ephervex_options::OptionsError::Unreadable { .. })
        ),
        "{result:?}"
    );
}

#[test]
fn a_files_unrelated_and_mod_added_lines_survive_an_edit() {
    let base = tempfile::tempdir().expect("temp dir");
    let original = "version:3465\nguiScale:0\njei:configVersion:2\naKeyNobodyKnows:{\"a\":1}\n";
    std::fs::write(base.path().join(OPTIONS_FILE_NAME), original).expect("write");

    let report = apply(
        base.path(),
        "1.20.1",
        &managed(&[("gui_scale", MinecraftPreferenceValue::Integer(3))]),
        LanguageAvailability::Verified,
    );
    assert!(!report.created_file);
    assert_eq!(
        std::fs::read_to_string(base.path().join(OPTIONS_FILE_NAME)).expect("read"),
        "version:3465\nguiScale:3\njei:configVersion:2\naKeyNobodyKnows:{\"a\":1}\n"
    );
}

#[test]
fn nothing_is_reported_as_applied_that_was_not_written() {
    // The report is what a user is shown, so an unknown or unrepresentable
    // preference must never appear as a success.
    let preferences = managed(&[
        ("simulation_distance", MinecraftPreferenceValue::Integer(10)),
        (
            "sound_volume_master",
            MinecraftPreferenceValue::decimal(1.5).expect("a number"),
        ),
        ("gui_scale", MinecraftPreferenceValue::Integer(2)),
    ]);
    let base = tempfile::tempdir().expect("temp dir");
    let report = apply(
        base.path(),
        "1.20.5",
        &preferences,
        LanguageAvailability::Verified,
    );

    assert_eq!(
        report.applied,
        vec![id("gui_scale"), id("simulation_distance")]
    );
    assert!(report.unsupported.is_empty());
    assert!(report.unknown.is_empty());
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(report.skipped[0].id, id("sound_volume_master"));
    assert_eq!(report.skipped[0].reason, SkipReason::OutOfRange);

    let text = std::fs::read_to_string(base.path().join(OPTIONS_FILE_NAME)).expect("read");
    assert!(text.contains("guiScale:2"));
    assert!(text.contains("simulationDistance:10"));
    assert!(
        !text.contains("soundCategory_master"),
        "a refused value leaves no trace"
    );
}

#[test]
fn a_preference_the_release_predates_is_reported_and_not_written() {
    let base = tempfile::tempdir().expect("temp dir");
    let report = apply(
        base.path(),
        "1.12.2",
        &managed(&[("simulation_distance", MinecraftPreferenceValue::Integer(10))]),
        LanguageAvailability::Verified,
    );
    assert_eq!(report.unsupported, vec![id("simulation_distance")]);
    assert!(report.applied.is_empty());
    assert!(!base.path().join(OPTIONS_FILE_NAME).exists());
}

// -------------------------------------------------------------------- language

#[test]
fn the_language_preference_waits_for_the_target_releases_own_catalog() {
    let preferences = managed(&[(
        "language",
        MinecraftPreferenceValue::LanguageCode("de_de".to_owned()),
    )]);
    for (availability, expected) in [
        (
            LanguageAvailability::Unverified,
            SkipReason::LanguageUnverified,
        ),
        (LanguageAvailability::Absent, SkipReason::LanguageAbsent),
    ] {
        let base = tempfile::tempdir().expect("temp dir");
        let report = apply(base.path(), "1.12.2", &preferences, availability);
        assert_eq!(report.skipped.len(), 1);
        assert_eq!(report.skipped[0].reason, expected);
        assert!(!base.path().join(OPTIONS_FILE_NAME).exists());
    }

    assert!(
        applied_text("1.12.2", &preferences).contains("lang:de_de"),
        "a verified code is written"
    );
}

#[test]
fn an_unverifiable_language_never_blocks_an_unrelated_preference() {
    // A missing language catalog is a fact about one preference. Letting it
    // stop a GUI scale would make an offline creation strictly worse.
    let preferences = managed(&[
        (
            "language",
            MinecraftPreferenceValue::LanguageCode("de_de".to_owned()),
        ),
        ("gui_scale", MinecraftPreferenceValue::Integer(3)),
    ]);
    let base = tempfile::tempdir().expect("temp dir");
    let report = apply(
        base.path(),
        "1.20.1",
        &preferences,
        LanguageAvailability::Unverified,
    );

    assert_eq!(report.applied, vec![id("gui_scale")]);
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(report.skipped[0].id, id("language"));

    let text = std::fs::read_to_string(base.path().join(OPTIONS_FILE_NAME)).expect("read");
    assert!(text.contains("guiScale:3"));
    assert!(!text.contains("lang:"));
}

// ----------------------------------------------------------------- idempotence

#[test]
fn applying_the_same_preferences_twice_changes_nothing_the_second_time() {
    // Import layering applies preferences after the pack's own overrides, so
    // this has to be idempotent.
    let base = tempfile::tempdir().expect("temp dir");
    let preferences = managed(&[
        ("gui_scale", MinecraftPreferenceValue::Integer(3)),
        (
            "fov",
            MinecraftPreferenceValue::decimal(90.0).expect("finite"),
        ),
    ]);
    apply(
        base.path(),
        "1.20.1",
        &preferences,
        LanguageAvailability::Verified,
    );
    let once = std::fs::read_to_string(base.path().join(OPTIONS_FILE_NAME)).expect("read");
    apply(
        base.path(),
        "1.20.1",
        &preferences,
        LanguageAvailability::Verified,
    );
    let twice = std::fs::read_to_string(base.path().join(OPTIONS_FILE_NAME)).expect("read");
    assert_eq!(once, twice);
    assert_eq!(ParsedOptions::parse(&once).render(), once);
}

// ------------------------------------------------------- startup graphics preset

/// Releases from 1.21.11 apply a graphics preset right after loading the file.
const PRESET_RELEASES: &[&str] = &["1.21.11", "26.1.2", "26.2", "26.3"];

fn clouds_off() -> ConfiguredPreferences {
    managed(&[(
        "cloud_status",
        MinecraftPreferenceValue::Enum("false".to_owned()),
    )])
}

#[test]
fn a_setting_the_startup_preset_controls_is_written_with_the_preset_that_replaces_nothing() {
    for version in PRESET_RELEASES {
        let text = applied_text(version, &clouds_off());

        assert!(text.contains("renderClouds:\"false\""), "{version}: {text}");
        assert!(
            text.contains("graphicsPreset:\"custom\""),
            "{version} would replace the chosen clouds with its default preset's: {text}"
        );
    }
}

#[test]
fn a_setting_the_startup_preset_does_not_control_leaves_the_preset_unwritten() {
    let preferences = managed(&[
        ("gui_scale", MinecraftPreferenceValue::Integer(3)),
        ("fov", MinecraftPreferenceValue::Integer(90)),
    ]);
    for version in PRESET_RELEASES {
        let text = applied_text(version, &preferences);

        assert!(text.contains("guiScale:3"), "{version}: {text}");
        assert!(!text.contains("graphicsPreset"), "{version}: {text}");
    }
}

#[test]
fn a_release_without_a_startup_preset_never_gets_a_preset_line() {
    for version in ["1.12.2", "1.16.5"] {
        let text = applied_text(version, &clouds_off());

        assert!(text.contains("renderClouds:"), "{version}: {text}");
        assert!(!text.contains("graphicsPreset"), "{version}: {text}");
    }
}

#[test]
fn a_preset_the_user_manages_is_written_as_chosen() {
    let preferences = managed(&[
        (
            "cloud_status",
            MinecraftPreferenceValue::Enum("false".to_owned()),
        ),
        (
            "graphics_preset",
            MinecraftPreferenceValue::Enum("fast".to_owned()),
        ),
    ]);
    for version in PRESET_RELEASES {
        let text = applied_text(version, &preferences);

        assert!(
            text.contains("graphicsPreset:\"fast\""),
            "{version}: {text}"
        );
        assert!(!text.contains("custom"), "{version}: {text}");
    }
}

#[test]
fn an_existing_preset_is_replaced_only_when_a_controlled_setting_is_written() {
    let base = tempfile::tempdir().expect("temp dir");
    let path = base.path().join(OPTIONS_FILE_NAME);
    std::fs::write(&path, "version:4903\ngraphicsPreset:\"fast\"\nguiScale:2\n").expect("write");

    apply(
        base.path(),
        "26.2",
        &managed(&[("gui_scale", MinecraftPreferenceValue::Integer(3))]),
        LanguageAvailability::Verified,
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        "version:4903\ngraphicsPreset:\"fast\"\nguiScale:3\n",
        "an unrelated setting must not touch the instance's own preset"
    );

    apply(
        base.path(),
        "26.2",
        &clouds_off(),
        LanguageAvailability::Verified,
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        "version:4903\ngraphicsPreset:\"custom\"\nguiScale:3\nrenderClouds:\"false\"\n"
    );
}

#[test]
fn applying_the_same_controlled_setting_twice_changes_nothing_the_second_time() {
    let base = tempfile::tempdir().expect("temp dir");
    apply(
        base.path(),
        "26.2",
        &clouds_off(),
        LanguageAvailability::Verified,
    );
    let first = std::fs::read_to_string(base.path().join(OPTIONS_FILE_NAME)).expect("read");

    apply(
        base.path(),
        "26.2",
        &clouds_off(),
        LanguageAvailability::Verified,
    );

    assert_eq!(
        std::fs::read_to_string(base.path().join(OPTIONS_FILE_NAME)).expect("read"),
        first
    );
}

#[test]
fn a_controlled_setting_the_release_cannot_express_does_not_write_the_preset() {
    let preferences = managed(&[(
        "cloud_status",
        MinecraftPreferenceValue::Enum("not-a-cloud-status".to_owned()),
    )]);
    for version in PRESET_RELEASES {
        let text = applied_text(version, &preferences);

        assert!(!text.contains("graphicsPreset"), "{version}: {text}");
    }
}
