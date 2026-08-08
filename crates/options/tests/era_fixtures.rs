//! The `options.txt` fixtures are driven through the adapters.
//!
//! Every file is embedded at compile time; nothing here reads a real
//! installation and nothing here touches the network. What each fixture is for
//! is documented in `fixtures/options/SOURCES.md`.
//!
//! These tests defend the compatibility boundary between Minecraft releases:
//! applying a global preference must be an *edit*, not a
//! replacement. A user's four hundred settings, a mod's key, and the release's
//! own data-version marker all have to come out the other side untouched.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ephervex_options::{
    AdapterSet, GlobalMinecraftPreferences, GuiScalePreference, Managed, ParsedOptions,
    PreferenceKey, VERSION_KEY, WindowSize, decode_fov,
};

/// One era fixture and the adapter it must select.
struct Era {
    version: &'static str,
    adapter: &'static str,
    text: &'static str,
}

const ERAS: &[Era] = &[
    Era {
        version: "1.12.2",
        adapter: "minecraft-1.12",
        text: include_str!("../../../fixtures/options/options-1.12.2.txt"),
    },
    Era {
        version: "1.16.5",
        adapter: "minecraft-1.13",
        text: include_str!("../../../fixtures/options/options-1.16.5.txt"),
    },
    Era {
        version: "1.20.1",
        adapter: "minecraft-1.18",
        text: include_str!("../../../fixtures/options/options-1.20.1.txt"),
    },
    Era {
        version: "26.1",
        adapter: "minecraft-1.18",
        text: include_str!("../../../fixtures/options/options-26.1.txt"),
    },
];

/// Every preference managed, so each fixture is asked for everything and only
/// the era decides what it can answer.
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

fn apply(era: &Era, preferences: &GlobalMinecraftPreferences) -> ParsedOptions {
    let adapter = AdapterSet::default().adapter_for(era.version);
    assert_eq!(adapter.name(), era.adapter, "{}", era.version);
    adapter
        .apply(ParsedOptions::parse(era.text), preferences)
        .expect("apply")
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
fn applying_preferences_changes_only_the_lines_it_names() {
    for era in ERAS {
        let before = ParsedOptions::parse(era.text);
        let after = apply(era, &everything_managed());

        let expressible = AdapterSet::default()
            .adapter_for(era.version)
            .expressible_keys();
        let mut touched: Vec<&str> = Vec::new();
        for key in expressible {
            match key {
                PreferenceKey::GuiScale => touched.push("guiScale"),
                PreferenceKey::Language => touched.push("lang"),
                PreferenceKey::Fullscreen => touched.push("fullscreen"),
                PreferenceKey::WindowSize => {
                    touched.push("overrideWidth");
                    touched.push("overrideHeight");
                }
                PreferenceKey::Fov => touched.push("fov"),
                PreferenceKey::RenderDistance => touched.push("renderDistance"),
                PreferenceKey::SimulationDistance => touched.push("simulationDistance"),
            }
        }

        // Every key the file had is still there, and every one the adapter did
        // not name still has the value the user's own installation gave it.
        for key in before.keys() {
            assert!(after.contains(key), "{} lost the key `{key}`", era.version);
            if !touched.contains(&key) {
                assert_eq!(
                    after.get(key),
                    before.get(key),
                    "{} rewrote `{key}`, which no preference names",
                    era.version
                );
            }
        }

        //...and no key was invented beyond the ones the adapter writes.
        for key in after.keys() {
            assert!(
                before.contains(key) || touched.contains(&key),
                "{} gained the unexpected key `{key}`",
                era.version
            );
        }
    }
}

#[test]
fn the_same_normalized_preference_lands_correctly_in_every_era() {
    // the project's whole point: one normalized value, several releases, and
    // no file copied between them.
    for era in ERAS {
        let applied = apply(era, &everything_managed());
        assert_eq!(applied.get("guiScale"), Some("2"), "{}", era.version);
        assert_eq!(applied.get("lang"), Some("de_de"), "{}", era.version);
        assert_eq!(applied.get("fullscreen"), Some("true"), "{}", era.version);
        assert_eq!(applied.get("renderDistance"), Some("16"), "{}", era.version);

        // FOV is stored normalized in every supported era, so the round trip
        // through the encoding has to land back on the degrees asked for.
        let stored: f64 = applied
            .get("fov")
            .expect("every era has a field of view")
            .parse()
            .expect("a number");
        assert!(
            (decode_fov(stored) - 90.0).abs() < 1e-9,
            "{} stored fov {stored}",
            era.version
        );
    }
}

#[test]
fn window_size_reaches_only_the_release_that_can_express_it() {
    for era in ERAS {
        let applied = apply(era, &everything_managed());
        if era.version == "1.12.2" {
            assert_eq!(applied.get("overrideWidth"), Some("1920"));
            assert_eq!(applied.get("overrideHeight"), Some("1080"));
        } else {
            assert!(
                !applied.contains("overrideWidth"),
                "{} has no window-size keys since the LWJGL 3 move",
                era.version
            );
        }
    }
}

#[test]
fn simulation_distance_reaches_only_the_releases_that_have_it() {
    for era in ERAS {
        let applied = apply(era, &everything_managed());
        match era.version {
            "1.12.2" | "1.16.5" => assert!(
                !applied.contains("simulationDistance"),
                "{} predates simulation distance",
                era.version
            ),
            _ => assert_eq!(
                applied.get("simulationDistance"),
                Some("10"),
                "{}",
                era.version
            ),
        }
    }
}

#[test]
fn each_releases_own_version_marker_survives_untouched() {
    // `version:` says which historical fixes the game applies to the
    // rest of the file. Copying one release's value into another is how an
    // options file gets corrupted rather than configured.
    for era in ERAS {
        let before = ParsedOptions::parse(era.text);
        let after = apply(era, &everything_managed());
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
    let applied = apply(modern, &everything_managed());
    assert_eq!(applied.get("jei"), Some("configVersion:2"));
    assert!(applied.render().contains("jei:configVersion:2"));
}

#[test]
fn a_key_this_build_has_never_seen_survives_an_edit() {
    let calendar = ERAS
        .iter()
        .find(|era| era.version == "26.1")
        .expect("fixture");
    let applied = apply(calendar, &everything_managed());
    assert_eq!(applied.get("aKeyThisBuildHasNeverSeen"), Some("17"));
}

#[test]
fn an_unmanaged_preference_leaves_the_instances_own_value_in_place() {
    // only explicitly managed keys override what the file already had.
    for era in ERAS {
        let before = ParsedOptions::parse(era.text);
        let applied = apply(era, &GlobalMinecraftPreferences::default());
        assert_eq!(applied, before, "{} was rewritten for nothing", era.version);
    }
}
