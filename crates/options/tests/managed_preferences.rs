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
fn an_authoritative_release_that_cannot_express_the_value_stays_its_own_answer() {
    // 1.20.1 stores ambient occlusion as a boolean and 1.16.5 as an
    // enumeration. Both releases have the setting; only one of them can hold
    // a toggle, and the difference must not be reported as "unsupported".
    let on = MinecraftPreferenceValue::Boolean(true);
    assert_eq!(
        disposition("1.20.1", "ambient_occlusion", on.clone()),
        PreferenceDisposition::Writable
    );
    assert_eq!(
        disposition("1.16.5", "ambient_occlusion", on),
        PreferenceDisposition::NotRepresentable(SkipReason::TypeMismatch),
        "the release has the setting; it just does not store a boolean there"
    );
}

#[test]
fn a_number_outside_a_releases_proven_domain_is_refused_rather_than_clamped() {
    // A clamp writes a render distance the user never chose into every
    // instance created afterwards.
    let far = MinecraftPreferenceValue::Integer(999);
    assert_eq!(
        disposition("1.20.1", "render_distance", far.clone()),
        PreferenceDisposition::NotRepresentable(SkipReason::OutOfRange),
        "1.20.1 proved a 2..32 domain"
    );
    assert_eq!(
        disposition("26.2", "render_distance", far),
        PreferenceDisposition::Writable,
        "26.2 proved no domain, so the codec has nothing to refuse"
    );
    assert!(
        !applied_text(
            "1.20.1",
            &managed(&[("render_distance", MinecraftPreferenceValue::Integer(999))])
        )
        .contains("renderDistance"),
        "nothing at all is written for a value the release refuses"
    );
}

#[test]
fn an_enumeration_accepts_only_what_the_exact_release_enumerates() {
    let crosshair = MinecraftPreferenceValue::Enum("1".to_owned());
    assert_eq!(
        disposition("1.20.1", "attack_indicator", crosshair.clone()),
        PreferenceDisposition::Writable
    );
    assert!(
        applied_text("1.20.1", &managed(&[("attack_indicator", crosshair)]))
            .contains("attackIndicator:1")
    );
    assert_eq!(
        disposition(
            "1.12.2",
            "attack_indicator",
            MinecraftPreferenceValue::Enum("1".to_owned())
        ),
        PreferenceDisposition::NotRepresentable(SkipReason::TypeMismatch),
        "1.12.2 stores a plain number there, not a proven enumeration"
    );
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
    let preferences = managed(&[("keybind_advancements", binding)]);

    assert!(
        applied_text("1.12.2", &preferences).contains("key_key.advancements:42"),
        "1.12.2 stores the LWJGL 2 scan code"
    );
    for version in ["1.16.5", "1.20.1", "26.2"] {
        assert!(
            applied_text(version, &preferences)
                .contains("key_key.advancements:key.keyboard.left.shift"),
            "{version} stores the serialized key name"
        );
    }
}

#[test]
fn a_mouse_binding_and_an_unbound_key_reach_the_releases_that_establish_them() {
    for (value, expected) in [
        (
            MinecraftPreferenceValue::KeyBinding(InputBinding::Mouse(MouseButton::Middle)),
            "key_key.advancements:key.mouse.middle",
        ),
        (
            MinecraftPreferenceValue::KeyBinding(InputBinding::Unbound),
            "key_key.advancements:key.keyboard.unknown",
        ),
    ] {
        let preferences = managed(&[("keybind_advancements", value.clone())]);
        assert!(
            applied_text("1.20.1", &preferences).contains(expected),
            "1.20.1 {expected}"
        );

        // The pre-1.13 era stores mouse buttons and an unbound marker in the
        // same field, but Ephervex's compatibility research establishes
        // neither, so nothing is written rather than something plausible.
        assert_eq!(
            disposition("1.12.2", "keybind_advancements", value),
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
        disposition("1.12.2", "keybind_advancements", value.clone()),
        PreferenceDisposition::NotRepresentable(SkipReason::InputUnavailable)
    );
    assert!(
        !applied_text(
            "1.12.2",
            &managed(&[("keybind_advancements", value.clone())])
        )
        .contains("key_key.advancements"),
    );
    assert_eq!(
        disposition("1.20.1", "keybind_advancements", value),
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
    assert!(
        applied_text("1.12.2", &preferences).contains("fov:0.5"),
        "1.12.2 stores (degrees - 70) / 40"
    );
    assert!(
        applied_text("1.20.1", &preferences).contains("fov:90.0"),
        "1.20.1 stores degrees"
    );
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
        ("render_distance", MinecraftPreferenceValue::Integer(999)),
        ("gui_scale", MinecraftPreferenceValue::Integer(2)),
    ]);
    let base = tempfile::tempdir().expect("temp dir");
    let report = apply(
        base.path(),
        "1.20.1",
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
    assert_eq!(report.skipped[0].id, id("render_distance"));
    assert_eq!(report.skipped[0].reason, SkipReason::OutOfRange);

    let text = std::fs::read_to_string(base.path().join(OPTIONS_FILE_NAME)).expect("read");
    assert!(text.contains("guiScale:2"));
    assert!(text.contains("simulationDistance:10"));
    assert!(
        !text.contains("renderDistance"),
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
