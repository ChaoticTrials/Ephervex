//! Raw `options.txt` lines applied after the managed preferences.
//!
//! The lines are the user's own text. What these tests defend is the order they
//! are applied in, the file format's minimum they must meet, and that a line
//! which cannot be written never reaches a file.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

use ephervex_options::{
    ConfiguredPreference, ConfiguredPreferences, LanguageAvailability, MinecraftPreferenceCatalog,
    MinecraftPreferenceValue, OPTIONS_FILE_NAME, OptionsCompatibility, PreferenceApplication,
    PreferenceId, RawLineProblem, RawOptionLines, apply_plan, plan_managed_preferences,
};

fn managed(entries: &[(&str, MinecraftPreferenceValue)]) -> ConfiguredPreferences {
    let mut preferences = ConfiguredPreferences::new();
    for (name, value) in entries {
        preferences.insert(
            PreferenceId::parse(name).expect("a well-formed id"),
            ConfiguredPreference::managed(value.clone()),
        );
    }
    preferences
}

fn apply_with_raw(
    game: &Path,
    version: &str,
    preferences: &ConfiguredPreferences,
    raw: &str,
) -> PreferenceApplication {
    let plan = plan_managed_preferences(
        version,
        preferences,
        MinecraftPreferenceCatalog::bundled_shared().expect("the bundled catalog is valid"),
        &OptionsCompatibility::bundled(),
        LanguageAvailability::Verified,
    )
    .with_raw_lines(&RawOptionLines::parse(raw));
    apply_plan(game, &plan).expect("apply")
}

fn read(game: &Path) -> String {
    std::fs::read_to_string(game.join(OPTIONS_FILE_NAME)).expect("read")
}

#[test]
fn a_raw_line_replaces_the_line_for_a_key_the_file_already_has() {
    let base = tempfile::tempdir().expect("temp dir");
    std::fs::write(
        base.path().join(OPTIONS_FILE_NAME),
        "version:3465\nautoJump:false\nmodSetting:1\n",
    )
    .expect("write");

    let report = apply_with_raw(
        base.path(),
        "1.20.1",
        &ConfiguredPreferences::new(),
        "autoJump:true\n",
    );

    assert_eq!(report.raw_lines_applied, 1);
    assert!(report.changed_anything());
    assert_eq!(
        read(base.path()),
        "version:3465\nautoJump:true\nmodSetting:1\n"
    );
}

#[test]
fn a_raw_line_for_a_new_key_is_appended() {
    let base = tempfile::tempdir().expect("temp dir");
    std::fs::write(
        base.path().join(OPTIONS_FILE_NAME),
        "version:3465\nguiScale:0\n",
    )
    .expect("write");

    apply_with_raw(
        base.path(),
        "1.20.1",
        &ConfiguredPreferences::new(),
        "key_key.modmenu.open:key.keyboard.m\nsomeModOption:{\"a\":1}",
    );

    assert_eq!(
        read(base.path()),
        "version:3465\nguiScale:0\nkey_key.modmenu.open:key.keyboard.m\nsomeModOption:{\"a\":1}\n"
    );
}

#[test]
fn blank_lines_write_nothing() {
    let base = tempfile::tempdir().expect("temp dir");
    let original = "version:3465\nguiScale:0\n";
    std::fs::write(base.path().join(OPTIONS_FILE_NAME), original).expect("write");

    let report = apply_with_raw(
        base.path(),
        "1.20.1",
        &ConfiguredPreferences::new(),
        "\n   \n\r\n",
    );

    assert_eq!(report.raw_lines_applied, 0);
    assert!(report.raw_lines_rejected.is_empty());
    assert!(!report.changed_anything());
    assert_eq!(read(base.path()), original);
}

#[test]
fn a_line_that_cannot_be_one_is_reported_and_not_written() {
    let base = tempfile::tempdir().expect("temp dir");
    let original = "version:3465\nguiScale:0\n";
    std::fs::write(base.path().join(OPTIONS_FILE_NAME), original).expect("write");

    let report = apply_with_raw(
        base.path(),
        "1.20.1",
        &ConfiguredPreferences::new(),
        "no separator here\n:valueWithoutKey\nbad\x07key:1\nfine:yes\n",
    );

    assert_eq!(report.raw_lines_applied, 1);
    assert_eq!(
        report
            .raw_lines_rejected
            .iter()
            .map(|rejected| (rejected.line_number, rejected.problem))
            .collect::<Vec<_>>(),
        vec![
            (1, RawLineProblem::MissingSeparator),
            (2, RawLineProblem::EmptyKey),
            (3, RawLineProblem::ControlCharacter),
        ]
    );
    assert_eq!(read(base.path()), "version:3465\nguiScale:0\nfine:yes\n");
}

#[test]
fn only_refused_lines_leave_the_file_untouched() {
    let base = tempfile::tempdir().expect("temp dir");
    let original = "version:3465\nguiScale:0\n";
    std::fs::write(base.path().join(OPTIONS_FILE_NAME), original).expect("write");

    let report = apply_with_raw(
        base.path(),
        "1.20.1",
        &ConfiguredPreferences::new(),
        "not a line",
    );

    assert_eq!(report.raw_lines_applied, 0);
    assert_eq!(report.raw_lines_rejected.len(), 1);
    assert_eq!(read(base.path()), original);
}

#[test]
fn the_data_version_line_is_never_taken_from_the_text() {
    let base = tempfile::tempdir().expect("temp dir");

    let report = apply_with_raw(
        base.path(),
        "1.20.1",
        &ConfiguredPreferences::new(),
        "version:1\nautoJump:true\n",
    );

    assert_eq!(
        report.raw_lines_rejected[0].problem,
        RawLineProblem::DataVersionMarker
    );
    assert_eq!(
        read(base.path()),
        "version:3465\nautoJump:true\n",
        "a file Ephervex creates carries the release's own marker"
    );

    let existing = tempfile::tempdir().expect("temp dir");
    std::fs::write(existing.path().join(OPTIONS_FILE_NAME), "version:2975\n").expect("write");
    apply_with_raw(
        existing.path(),
        "1.20.1",
        &ConfiguredPreferences::new(),
        "version:3465",
    );
    assert_eq!(read(existing.path()), "version:2975\n");
}

#[test]
fn a_raw_line_wins_over_a_managed_preference_for_the_same_key() {
    let base = tempfile::tempdir().expect("temp dir");

    apply_with_raw(
        base.path(),
        "1.20.1",
        &managed(&[("gui_scale", MinecraftPreferenceValue::Integer(3))]),
        "guiScale:2\n",
    );

    assert_eq!(read(base.path()), "version:3465\nguiScale:2\n");
}

#[test]
fn a_raw_line_wins_over_the_preset_line_added_for_a_managed_setting() {
    let base = tempfile::tempdir().expect("temp dir");

    apply_with_raw(
        base.path(),
        "26.2",
        &managed(&[(
            "cloud_status",
            MinecraftPreferenceValue::Enum("false".to_owned()),
        )]),
        "graphicsPreset:\"fancy\"\n",
    );

    let text = read(base.path());
    assert!(text.contains("renderClouds:\"false\""), "{text}");
    assert!(text.contains("graphicsPreset:\"fancy\""), "{text}");
    assert!(!text.contains("custom"), "{text}");
}

#[test]
fn raw_lines_are_written_even_when_no_preference_is_managed() {
    let base = tempfile::tempdir().expect("temp dir");

    let report = apply_with_raw(
        base.path(),
        "1.20.1",
        &ConfiguredPreferences::new(),
        "autoJump:true",
    );

    assert!(report.created_file);
    assert_eq!(read(base.path()), "version:3465\nautoJump:true\n");
}

#[test]
fn applying_the_same_raw_lines_twice_changes_nothing_the_second_time() {
    let base = tempfile::tempdir().expect("temp dir");
    let preferences = ConfiguredPreferences::new();
    apply_with_raw(base.path(), "1.20.1", &preferences, "a:1\nb:2\na:3\n");
    let first = read(base.path());

    apply_with_raw(base.path(), "1.20.1", &preferences, "a:1\nb:2\na:3\n");

    assert_eq!(read(base.path()), first);
    assert_eq!(first, "version:3465\na:3\nb:2\n");
}
