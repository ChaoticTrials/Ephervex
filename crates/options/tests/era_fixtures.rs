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
    OPTIONS_FILE_NAME, OptionCodec, OptionsCompatibility, OptionsDataset, ParsedOptions,
    PreferenceApplication, PreferenceId, PreferenceKey, PreferenceSupport, VERSION_KEY, WindowSize,
    apply_to_game_directory,
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
fn touched_keys(era: &Era, id: &PreferenceId) -> Vec<String> {
    let key = PreferenceKey::ALL
        .iter()
        .copied()
        .find(|key| key.id() == *id)
        .expect("the report only names the seven facade preferences");
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

/// What a fixture should read like after its own values are applied back.
///
/// Unchanged, except that a release applying a startup preset must name the
/// preset that replaces nothing, because the preset would otherwise replace the
/// very values being written.
fn expected_after_applying(era: &Era) -> String {
    let mut parsed = ParsedOptions::parse(era.text);
    if let Some(guard) = OptionsCompatibility::bundled().preset_guard(era.version) {
        parsed.set(&guard.key, guard.neutral_value);
    }
    parsed.render()
}

/// Writing back exactly what a fixture already holds must not change a byte,
/// apart from the startup preset line a release needs for the values to hold.
///
/// This is the assertion that catches an encoding the dataset has wrong. Each
/// fixture is a file in the shape its release writes; asking this build to
/// apply the very values that file already expresses must therefore reproduce
/// it. When the two disagree, one of them is wrong about the release, and it
/// is not the game.
///
/// The field of view is the case that made this test necessary: the file holds
/// `fov:0.0`, which is the default seventy degrees through the transform every
/// release stores it with. A build that thought a modern release wrote degrees
/// would put `fov:70.0` here and this test would say so.
#[test]
fn applying_what_a_fixture_already_says_rewrites_none_of_it() {
    for era in ERAS {
        let modern = era.version != "1.12.2" && era.version != "1.16.5";
        let preferences = GlobalMinecraftPreferences {
            fov: Managed::apply(70.0),
            gui_scale: Managed::apply(GuiScalePreference::Auto),
            language: Managed::apply("en_us".to_owned()),
            fullscreen: Managed::apply(false),
            render_distance: Managed::apply(if era.version == "1.12.2" { 8 } else { 12 }),
            // A key an era does not have is left unmanaged rather than
            // written and refused: this test is about what the file already
            // says, and these two are absent from the eras that skip them.
            simulation_distance: if modern {
                Managed::apply(12)
            } else {
                Managed::ignore(12)
            },
            window_size: {
                let none = WindowSize {
                    width: 0,
                    height: 0,
                };
                if era.version == "1.12.2" {
                    Managed::apply(none)
                } else {
                    Managed::ignore(none)
                }
            },
        };

        let base = tempfile::tempdir().expect("temp dir");
        let (_, report) = apply_in(base.path(), era, &preferences);
        let text = std::fs::read_to_string(base.path().join(OPTIONS_FILE_NAME)).expect("read back");
        assert!(
            !report.applied.is_empty(),
            "{} wrote nothing, so this proves nothing",
            era.version
        );
        assert_eq!(
            text,
            expected_after_applying(era),
            "{} was rewritten by values it already held",
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

/// What is wrong with `value` as the text a release stores for `codec`, if
/// anything. This reads a stored value back; the codec itself only encodes.
fn stored_value_problem(codec: &OptionCodec, value: &str) -> Option<String> {
    let outside = |number: f64, min: Option<f64>, max: Option<f64>| {
        const TOLERANCE: f64 = 1e-9;
        min.is_some_and(|low| number < low - TOLERANCE)
            || max.is_some_and(|high| number > high + TOLERANCE)
    };
    let problem = match codec {
        OptionCodec::Boolean => !matches!(value, "true" | "false"),
        OptionCodec::Text => false,
        OptionCodec::LanguageCode => value.is_empty(),
        OptionCodec::KeyCodeLwjgl2 => value.parse::<i64>().is_err(),
        OptionCodec::KeyNameGlfw => !value.starts_with("key."),
        OptionCodec::Integer { min, max } => value.parse::<i64>().map_or(true, |number| {
            min.is_some_and(|low| number < low) || max.is_some_and(|high| number > high)
        }),
        OptionCodec::Decimal { min, max } => value.parse::<f64>().map_or(true, |number| {
            !number.is_finite() || outside(number, *min, *max)
        }),
        OptionCodec::ScaledDecimal {
            offset,
            scale,
            min,
            max,
        } => value.parse::<f64>().map_or(true, |stored| {
            !stored.is_finite() || outside(stored * scale + offset, *min, *max)
        }),
        OptionCodec::EnumId { values } => value
            .parse::<i64>()
            .map_or(true, |id| !values.contains(&id)),
        OptionCodec::EnumToken { tokens, quoted } => {
            let token = if *quoted {
                serde_json::from_str::<String>(value).ok()
            } else {
                Some(value.to_owned())
            };
            token.is_none_or(|token| !tokens.contains(&token))
        }
    };
    problem.then(|| format!("`{value}` does not fit {codec:?}"))
}

#[test]
fn every_fixture_line_is_a_key_and_value_its_release_writes() {
    let dataset = OptionsDataset::bundled();
    let mut problems = Vec::new();
    for era in ERAS {
        let schema = dataset
            .schema_for(era.version)
            .unwrap_or_else(|| panic!("{} is not mapped", era.version));
        let writes: std::collections::BTreeMap<&str, &OptionCodec> = schema
            .supported
            .keys()
            .filter_map(|setting| dataset.representation(schema, setting))
            .map(|representation| (representation.key.as_str(), &representation.codec))
            .collect();
        // Only a proven absence counts: a key the dataset has no verdict on
        // is a gap in what it knows, not evidence the release lacks it.
        let proven_absent: std::collections::BTreeSet<&str> = schema
            .unsupported
            .iter()
            .filter_map(|id| {
                dataset
                    .setting_registry
                    .iter()
                    .find(|entry| entry.id == *id)
            })
            .map(|entry| entry.key.as_str())
            .collect();

        let parsed = ParsedOptions::parse(era.text);
        for key in parsed.keys() {
            let value = parsed.get(key).expect("a listed key has a value");
            match writes.get(key) {
                Some(codec) => {
                    if let Some(problem) = stored_value_problem(codec, value) {
                        problems.push(format!("{} writes `{key}`, but {problem}", era.version));
                    }
                }
                None if proven_absent.contains(key) => problems.push(format!(
                    "{} contains `{key}`, which this release provably does not write",
                    era.version
                )),
                None => {}
            }
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn applying_preferences_changes_only_the_lines_it_names() {
    for era in ERAS {
        let before = ParsedOptions::parse(era.text);
        let (after, report) = apply(era, &everything_managed());

        let mut touched: Vec<String> = Vec::new();
        for id in &report.applied {
            touched.extend(touched_keys(era, id));
        }
        // A release that applies a startup preset also needs the preset that
        // replaces nothing, whenever a setting that preset controls is written.
        if let Some(guard) = OptionsCompatibility::bundled().preset_guard(era.version)
            && touched
                .iter()
                .any(|key| guard.controlled_keys.contains(key))
        {
            touched.push(guard.key);
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
fn the_field_of_view_is_stored_scaled_rather_than_in_degrees() {
    // No release has ever put degrees in the file: every one of them stores
    // `(degrees - 70) / 40`, which is why each fixture's own default reads
    // `fov:0.0` rather than `fov:70.0`. Releases from 1.19 reach that
    // arithmetic through the option's codec instead of a field transform,
    // which changes where it is written down and not what lands in the file.
    for era in ERAS {
        let (applied, _) = apply(era, &everything_managed());
        assert_eq!(applied.get("fov"), Some("0.5"), "{}", era.version);
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
                        .contains(&PreferenceKey::SimulationDistance.id()),
                    "{} predates simulation distance",
                    era.version
                );
                assert!(!applied.contains("simulationDistance"));
                assert!(report.unknown.is_empty(), "{}", era.version);
            }
            _ => assert!(
                !report
                    .unsupported
                    .contains(&PreferenceKey::SimulationDistance.id()),
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
