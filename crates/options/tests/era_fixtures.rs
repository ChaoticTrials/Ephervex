//! Real `options.txt` files from four eras, driven through the generated
//! compatibility dataset.
//!
//! Every file is embedded at compile time; nothing here reads a real
//! installation and nothing here touches the network. What each fixture is for
//! is documented in `fixtures/options/SOURCES.md`.
//!
//! These tests defend the compatibility boundary between Minecraft releases:
//! applying a global preference must be an *edit*, not a replacement. A user's
//! four hundred settings, a mod's key, and the release's own data-version
//! marker all have to come out the other side untouched.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

use ephervex_options::{
    GlobalMinecraftPreferences, GuiScalePreference, LanguageAvailability, Managed,
    OPTIONS_FILE_NAME, OptionsCompatibility, ParsedOptions, PreferenceApplication, PreferenceKey,
    PreferenceSupport, VERSION_KEY, WindowSize, apply_to_game_directory,
};

/// One era fixture: a release and a real file from it.
struct Era {
    version: &'static str,
    text: &'static str,
}

const ERAS: &[Era] = &[
    Era {
        version: "1.12.2",
        text: include_str!("../../../fixtures/options/options-1.12.2.txt"),
    },
    Era {
        version: "1.16.5",
        text: include_str!("../../../fixtures/options/options-1.16.5.txt"),
    },
    Era {
        version: "1.20.1",
        text: include_str!("../../../fixtures/options/options-1.20.1.txt"),
    },
    Era {
        version: "26.1",
        text: include_str!("../../../fixtures/options/options-26.1.txt"),
    },
];

/// Every preference managed, so each fixture is asked for everything and only
/// the release decides what it can answer.
fn everything_managed() -> GlobalMinecraftPreferences {
    GlobalMinecraftPreferences {
        gui_scale: Managed::apply(GuiScalePreference::Fixed(2)),
        language: Managed::apply("de_de".to_owned()),
        fullscreen: Managed::apply(true),
        window_size: Managed::apply(WindowSize {
            width: 1920,
            height: 1080,
        }),
        fov: Managed::apply(90.0),
        render_distance: Managed::apply(16),
        simulation_distance: Managed::apply(10),
    }
}

/// Lays the fixture down in a temporary game directory and applies preferences
/// to it, exactly as instance creation does.
fn apply(
    era: &Era,
    preferences: &GlobalMinecraftPreferences,
) -> (ParsedOptions, PreferenceApplication) {
    let base = tempfile::tempdir().expect("temp dir");
    apply_in(base.path(), era, preferences)
}

fn apply_in(
    game: &Path,
    era: &Era,
    preferences: &GlobalMinecraftPreferences,
) -> (ParsedOptions, PreferenceApplication) {
    std::fs::write(game.join(OPTIONS_FILE_NAME), era.text).expect("lay down the fixture");
    let report = apply_to_game_directory(
        game,
        era.version,
        preferences,
        &OptionsCompatibility::bundled(),
        // The language catalog is Mojang's, resolved separately; these tests
        // are about options.txt, so the verdict is supplied.
        LanguageAvailability::Verified,
    )
    .expect("apply");
    let text = std::fs::read_to_string(game.join(OPTIONS_FILE_NAME)).expect("read back");
    (ParsedOptions::parse(&text), report)
}

/// The options keys one preference resolves to for one release.
fn touched_keys(era: &Era, key: PreferenceKey) -> Vec<String> {
    match OptionsCompatibility::bundled().support(key, era.version) {
        PreferenceSupport::Authoritative(fields) => fields
            .iter()
            .map(|field| field.representation.key.clone())
            .collect(),
        _ => Vec::new(),
    }
}

#[test]
fn every_fixture_round_trips_byte_for_byte_before_anything_is_applied() {
    // If this fails, no other assertion in this file means anything: the
    // parser is losing data before a preference is even considered.
    for era in ERAS {
        assert_eq!(
            ParsedOptions::parse(era.text).render(),
            era.text,
            "{} is not preserved verbatim",
            era.version
        );
    }
}

#[test]
fn every_era_fixture_belongs_to_a_release_the_dataset_can_answer_for() {
    let compatibility = OptionsCompatibility::bundled();
    for era in ERAS {
        assert!(
            compatibility.schema_id(era.version).is_some(),
            "{} is not mapped by the bundled dataset",
            era.version
        );
    }
}

#[test]
fn applying_preferences_changes_only_the_lines_it_names() {
    for era in ERAS {
        let before = ParsedOptions::parse(era.text);
        let (after, report) = apply(era, &everything_managed());

        let mut touched: Vec<String> = Vec::new();
        for key in &report.applied {
            touched.extend(touched_keys(era, *key));
        }

        // Every key the file had is still there, and every one no preference
        // named still has the value the user's own installation gave it.
        for key in before.keys() {
            assert!(after.contains(key), "{} lost the key `{key}`", era.version);
            if !touched.iter().any(|written| written == key) {
                assert_eq!(
                    after.get(key),
                    before.get(key),
                    "{} rewrote `{key}`, which no preference names",
                    era.version
                );
            }
        }

        //...and no key was invented beyond the ones that were applied.
        for key in after.keys() {
            assert!(
                before.contains(key) || touched.iter().any(|written| written == key),
                "{} gained the unexpected key `{key}`",
                era.version
            );
        }
    }
}

#[test]
fn the_same_normalized_preference_lands_correctly_in_every_era() {
    // One normalized value, several releases, and no file copied between them.
    for era in ERAS {
        let (applied, _) = apply(era, &everything_managed());
        assert_eq!(applied.get("guiScale"), Some("2"), "{}", era.version);
        assert_eq!(applied.get("lang"), Some("de_de"), "{}", era.version);
        assert_eq!(applied.get("fullscreen"), Some("true"), "{}", era.version);
        assert_eq!(applied.get("renderDistance"), Some("16"), "{}", era.version);
    }
}

#[test]
fn the_field_of_view_is_written_in_each_eras_own_encoding() {
    // 1.12.2 through 1.18.2 store `(degrees - 70) / 40`; 1.19 onward store the
    // degrees. Writing one era's number into another is exactly the corruption
    // the generated dataset exists to prevent.
    let expected = [
        ("1.12.2", "0.5"),
        ("1.16.5", "0.5"),
        ("1.20.1", "90.0"),
        ("26.1", "90.0"),
    ];
    for (version, stored) in expected {
        let era = ERAS.iter().find(|era| era.version == version).expect("era");
        let (applied, _) = apply(era, &everything_managed());
        assert_eq!(applied.get("fov"), Some(stored), "{version}");
    }
}

#[test]
fn simulation_distance_is_unsupported_before_the_release_that_introduced_it() {
    // Unsupported is a proven absence, and it is reported as such rather than
    // as "we do not know".
    for era in ERAS {
        let (applied, report) = apply(era, &everything_managed());
        match era.version {
            "1.12.2" | "1.16.5" => {
                assert!(
                    report
                        .unsupported
                        .contains(&PreferenceKey::SimulationDistance),
                    "{} predates simulation distance",
                    era.version
                );
                assert!(!applied.contains("simulationDistance"));
                assert!(report.unknown.is_empty(), "{}", era.version);
            }
            _ => assert!(
                !report
                    .unsupported
                    .contains(&PreferenceKey::SimulationDistance),
                "{} has a simulation distance",
                era.version
            ),
        }
    }
}

#[test]
fn each_releases_own_version_marker_survives_untouched() {
    // `version:` says which historical fixes the game applies to the rest of
    // the file. Copying one release's value into another is how an options
    // file gets corrupted rather than configured.
    for era in ERAS {
        let before = ParsedOptions::parse(era.text);
        let (after, _) = apply(era, &everything_managed());
        assert_eq!(
            after.get(VERSION_KEY),
            before.get(VERSION_KEY),
            "{}",
            era.version
        );
    }

    // No two fixtures share a marker, so a test that accidentally compared the
    // wrong file would fail rather than pass by coincidence.
    let markers: Vec<_> = ERAS
        .iter()
        .map(|era| {
            ParsedOptions::parse(era.text)
                .get(VERSION_KEY)
                .expect("every fixture has one")
                .to_owned()
        })
        .collect();
    let mut unique = markers.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), markers.len(), "{markers:?}");
}

#[test]
fn a_mod_key_carrying_a_second_colon_is_not_corrupted() {
    // `jei:configVersion:2` splits at the first colon, exactly as the game
    // does; a naive `split(':')` would turn it into a key of `jei` and lose
    // the rest.
    let modern = ERAS
        .iter()
        .find(|era| era.version == "1.20.1")
        .expect("fixture");
    let (applied, _) = apply(modern, &everything_managed());
    assert_eq!(applied.get("jei"), Some("configVersion:2"));
    assert!(applied.render().contains("jei:configVersion:2"));
}

#[test]
fn a_key_this_build_has_never_seen_survives_an_edit() {
    let calendar = ERAS
        .iter()
        .find(|era| era.version == "26.1")
        .expect("fixture");
    let (applied, _) = apply(calendar, &everything_managed());
    assert_eq!(applied.get("aKeyThisBuildHasNeverSeen"), Some("17"));
}

#[test]
fn an_unmanaged_preference_leaves_the_instances_own_value_in_place() {
    // Only explicitly managed keys override what the file already had.
    for era in ERAS {
        let before = ParsedOptions::parse(era.text);
        let (applied, report) = apply(era, &GlobalMinecraftPreferences::default());
        assert!(!report.changed_anything(), "{}", era.version);
        assert_eq!(applied, before, "{} was rewritten for nothing", era.version);
    }
}

#[test]
fn a_fixture_applied_against_an_unmapped_release_is_returned_untouched() {
    // The day after a Minecraft release. Nothing is written, nothing is
    // guessed, and the user's file is exactly as it was.
    let base = tempfile::tempdir().expect("temp dir");
    let future = Era {
        version: "26.99",
        text: ERAS[3].text,
    };
    let (applied, report) = apply_in(base.path(), &future, &everything_managed());
    assert_eq!(report.schema, None);
    assert_eq!(report.unknown.len(), PreferenceKey::ALL.len());
    assert!(report.applied.is_empty());
    assert_eq!(applied, ParsedOptions::parse(future.text));
}
