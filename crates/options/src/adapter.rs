//! Version-aware `options.txt` adapters.
//!
//! # Why a preference cannot simply be written
//!
//! Ephervex does not assume every key has identical semantics across Minecraft
//! 1.12.2 and 26.x. The supported range contains three concrete
//! counter-examples:
//!
//! * **A key that only exists in the old scheme.** 1.12.x stores the game
//!   window size in `overrideWidth`/`overrideHeight`. The 1.13 move to LWJGL 3
//!   removed them, so writing them into a modern instance adds two keys the
//!   game will never read.
//! * **A key that only exists in the new scheme.** `simulationDistance`
//!   arrived with 1.18. Writing it into a 1.12.2 instance is the same mistake
//!   pointing the other way.
//! * **A value that is not what the settings screen shows.** `fov` is stored
//!   normalized, not in degrees: the game's own codec is
//!   `degrees = 70 + 40 × stored`, so 70° is `fov:0.0` and 110° ("Quake Pro")
//!   is `fov:1.0`. A preference expressed in degrees has to be encoded, and
//!   copying a stored value between releases without understanding it is
//!   exactly the corruption Ephervex does not allow.
//!
//! # Tested compatibility ranges
//!
//! Each adapter declares the release range it has been tested against, and
//! [`AdapterSet`] picks by version. The ranges are covered by the fixtures in
//! `fixtures/options/`, which are real files from the eras they name.
//!
//! # And what happens outside them
//!
//! An id in a numbering scheme this build cannot parse — the thing Ephervex requires to
//! expect, because Mojang has changed schemes once already — gets
//! [`CONSERVATIVE`], which applies only the three keys that have been spelled
//! and encoded identically for the whole supported range. The conservative
//! behavior is to apply the known-safe keys, warn in the developer log,
//! and never rewrite a file this build cannot reason about.

use std::fmt;

use ephervex_minecraft::MinecraftVersionId;

use crate::error::OptionsError;
use crate::file::ParsedOptions;
use crate::preferences::{GlobalMinecraftPreferences, PreferenceKey};

/// The `options.txt` key holding the release's own data version.
///
/// Never written by Ephervex and never copied between releases: it tells the
/// game which historical fixes to apply to the rest of the file, so a value
/// from another release is worse than no value at all.
pub const VERSION_KEY: &str = "version";

/// Applies global preferences to one release's `options.txt`.
pub trait OptionsAdapter: fmt::Debug + Send + Sync {
    /// A stable identifier for logs, reports, and tests.
    fn name(&self) -> &'static str;

    /// Whether this adapter has been tested against a Minecraft release.
    fn supports(&self, minecraft_version: &str) -> bool;

    /// The preferences this release can express.
    ///
    /// Part of the trait rather than derived from a failed [`Self::apply`], so
    /// a caller can report "this version has no simulation distance" *before*
    /// writing anything, and a settings screen can gray the field out.
    fn expressible_keys(&self) -> &'static [PreferenceKey];

    /// Applies every managed, expressible preference to `existing`.
    ///
    /// Takes the parsed file and returns it: only the managed keys change, and
    /// everything else — unknown keys, comments, `version:` — comes back
    /// untouched.
    fn apply(
        &self,
        existing: ParsedOptions,
        prefs: &GlobalMinecraftPreferences,
    ) -> Result<ParsedOptions, OptionsError>;
}

/// A release range, as a half-open interval of Minecraft version ids.
#[derive(Debug, Clone, Copy)]
struct ReleaseRange {
    /// Inclusive lower bound.
    from: &'static str,
    /// Exclusive upper bound, or `None` for "everything newer".
    below: Option<&'static str>,
}

impl ReleaseRange {
    fn contains(self, version: &MinecraftVersionId) -> bool {
        // An id in an unknown scheme is never claimed by a range: guessing
        // which era it belongs to would risk selecting an incompatible adapter.
        // `CONSERVATIVE` picks it up instead.
        if !version.is_recognized() {
            return false;
        }
        if *version < MinecraftVersionId::parse(self.from) {
            return false;
        }
        self.below
            .is_none_or(|below| *version < MinecraftVersionId::parse(below))
    }
}

/// The built-in adapter: one release range, one set of expressible keys.
///
/// The eras Ephervex supports differ in *which* preferences they can express,
/// not in how the shared ones are encoded, so one implementation covers them
/// and adding an era is a table entry. A future release that changes an
/// encoding gets its own implementation of [`OptionsAdapter`] rather than a
/// flag here — which is what the trait is for.
#[derive(Debug)]
pub struct EraAdapter {
    name: &'static str,
    range: ReleaseRange,
    keys: &'static [PreferenceKey],
}

impl OptionsAdapter for EraAdapter {
    fn name(&self) -> &'static str {
        self.name
    }

    fn supports(&self, minecraft_version: &str) -> bool {
        self.range
            .contains(&MinecraftVersionId::parse(minecraft_version))
    }

    fn expressible_keys(&self) -> &'static [PreferenceKey] {
        self.keys
    }

    fn apply(
        &self,
        mut existing: ParsedOptions,
        prefs: &GlobalMinecraftPreferences,
    ) -> Result<ParsedOptions, OptionsError> {
        for key in self.keys {
            write_preference(&mut existing, *key, prefs);
        }
        Ok(existing)
    }
}

/// Writes one preference, if it is managed, in this release's own spelling.
fn write_preference(
    options: &mut ParsedOptions,
    key: PreferenceKey,
    prefs: &GlobalMinecraftPreferences,
) {
    match key {
        PreferenceKey::GuiScale => {
            if let Some(scale) = prefs.gui_scale.get() {
                options.set("guiScale", scale.option_value());
            }
        }
        PreferenceKey::Language => {
            if let Some(code) = prefs.language.get() {
                options.set("lang", code.clone());
            }
        }
        PreferenceKey::Fullscreen => {
            if let Some(fullscreen) = prefs.fullscreen.get() {
                options.set("fullscreen", fullscreen.to_string());
            }
        }
        PreferenceKey::WindowSize => {
            if let Some(size) = prefs.window_size.get() {
                options.set("overrideWidth", size.width.to_string());
                options.set("overrideHeight", size.height.to_string());
            }
        }
        PreferenceKey::Fov => {
            if let Some(degrees) = prefs.fov.get() {
                options.set("fov", format_option_double(encode_fov(*degrees)));
            }
        }
        PreferenceKey::RenderDistance => {
            if let Some(chunks) = prefs.render_distance.get() {
                options.set("renderDistance", chunks.to_string());
            }
        }
        PreferenceKey::SimulationDistance => {
            if let Some(chunks) = prefs.simulation_distance.get() {
                options.set("simulationDistance", chunks.to_string());
            }
        }
    }
}

/// Degrees to the value Minecraft stores: `degrees = 70 + 40 × stored`.
#[must_use]
pub fn encode_fov(degrees: f64) -> f64 {
    (degrees - 70.0) / 40.0
}

/// The inverse of [`encode_fov`], for reading a file back.
#[must_use]
pub fn decode_fov(stored: f64) -> f64 {
    stored.mul_add(40.0, 70.0)
}

/// Formats a floating-point option the way the game writes one: always with a
/// fractional part, so `0` is written `0.0`.
fn format_option_double(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.1}")
    } else {
        format!("{value}")
    }
}

/// Every preference that has been spelled and encoded identically across the
/// whole supported range, and is therefore safe to apply to a release this
/// build knows nothing about.
const KNOWN_SAFE_KEYS: &[PreferenceKey] = &[
    PreferenceKey::GuiScale,
    PreferenceKey::Language,
    PreferenceKey::Fullscreen,
];

/// 1.12.x: the oldest supported era. Window size lives in the options file.
pub static LEGACY_1_12: EraAdapter = EraAdapter {
    name: "minecraft-1.12",
    range: ReleaseRange {
        from: "1.12",
        below: Some("1.13"),
    },
    keys: &[
        PreferenceKey::GuiScale,
        PreferenceKey::Language,
        PreferenceKey::Fullscreen,
        PreferenceKey::WindowSize,
        PreferenceKey::Fov,
        PreferenceKey::RenderDistance,
    ],
};

/// 1.13 through 1.17.x: LWJGL 3 removed the window-size keys, and simulation
/// distance does not exist yet.
pub static MODERN_1_13: EraAdapter = EraAdapter {
    name: "minecraft-1.13",
    range: ReleaseRange {
        from: "1.13",
        below: Some("1.18"),
    },
    keys: &[
        PreferenceKey::GuiScale,
        PreferenceKey::Language,
        PreferenceKey::Fullscreen,
        PreferenceKey::Fov,
        PreferenceKey::RenderDistance,
    ],
};

/// 1.18 and everything newer, including the calendar-versioned releases:
/// simulation distance is split out from render distance.
pub static MODERN_1_18: EraAdapter = EraAdapter {
    name: "minecraft-1.18",
    range: ReleaseRange {
        from: "1.18",
        below: None,
    },
    keys: &[
        PreferenceKey::GuiScale,
        PreferenceKey::Language,
        PreferenceKey::Fullscreen,
        PreferenceKey::Fov,
        PreferenceKey::RenderDistance,
        PreferenceKey::SimulationDistance,
    ],
};

/// The fallback for a release in a numbering scheme this build cannot parse.
///
/// Claims nothing — [`OptionsAdapter::supports`] is always `false` — so it is
/// only ever reached through [`AdapterSet::adapter_for`] having found nothing
/// else. Applies the three keys of [`KNOWN_SAFE_KEYS`] and leaves the rest of
/// the file, including `version:`, exactly as it found it.
pub static CONSERVATIVE: EraAdapter = EraAdapter {
    name: "minecraft-unknown",
    range: ReleaseRange {
        from: "0",
        below: Some("0"),
    },
    keys: KNOWN_SAFE_KEYS,
};

/// The adapters this build ships, newest era first.
///
/// Order matters only for determinism: the ranges do not overlap.
pub static ADAPTERS: &[&dyn OptionsAdapter] = &[&MODERN_1_18, &MODERN_1_13, &LEGACY_1_12];

/// The adapters available to a caller, and the rule for choosing one.
#[derive(Debug, Clone, Copy)]
pub struct AdapterSet {
    adapters: &'static [&'static dyn OptionsAdapter],
    fallback: &'static dyn OptionsAdapter,
}

impl Default for AdapterSet {
    fn default() -> Self {
        Self {
            adapters: ADAPTERS,
            fallback: &CONSERVATIVE,
        }
    }
}

impl AdapterSet {
    /// The adapter for one Minecraft release, never `None`.
    ///
    /// An unsupported release gets [`CONSERVATIVE`] plus a warning rather than
    /// an error: refusing to create an instance because this build has not been
    /// taught a version's option keys would make the launcher useless the day
    /// after a Minecraft release, and The test suite covers the opposite trade.
    #[must_use]
    pub fn adapter_for(&self, minecraft_version: &str) -> &'static dyn OptionsAdapter {
        if let Some(adapter) = self
            .adapters
            .iter()
            .copied()
            .find(|adapter| adapter.supports(minecraft_version))
        {
            return adapter;
        }

        tracing::warn!(
            version = minecraft_version,
            adapter = self.fallback.name(),
            "no tested options.txt adapter for this Minecraft version; \
             applying only the preferences that are safe for every supported release"
        );
        self.fallback
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::preferences::{GuiScalePreference, Managed, WindowSize};

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

    fn apply(version: &str, existing: &str) -> ParsedOptions {
        AdapterSet::default()
            .adapter_for(version)
            .apply(ParsedOptions::parse(existing), &everything_managed())
            .expect("apply")
    }

    #[test]
    fn each_supported_era_is_matched_by_exactly_one_adapter() {
        let set = AdapterSet::default();
        for (version, expected) in [
            ("1.12.2", "minecraft-1.12"),
            ("1.13", "minecraft-1.13"),
            ("1.16.5", "minecraft-1.13"),
            ("1.17.1", "minecraft-1.13"),
            ("1.18", "minecraft-1.18"),
            ("1.20.1", "minecraft-1.18"),
            ("1.21.11", "minecraft-1.18"),
            ("26.1", "minecraft-1.18"),
            ("26.1.2", "minecraft-1.18"),
        ] {
            assert_eq!(set.adapter_for(version).name(), expected, "{version}");
            let claimed = ADAPTERS
                .iter()
                .filter(|adapter| adapter.supports(version))
                .count();
            assert_eq!(claimed, 1, "{version} must belong to exactly one era");
        }
    }

    #[test]
    fn an_unparseable_version_falls_back_to_the_keys_that_are_safe_everywhere() {
        // apply what is known-safe, warn, and corrupt nothing.
        let set = AdapterSet::default();
        let adapter = set.adapter_for("27w03a-something");
        assert_eq!(adapter.name(), "minecraft-unknown");
        assert_eq!(adapter.expressible_keys(), KNOWN_SAFE_KEYS);

        let applied = apply("27w03a-something", "version:9999\nfov:0.25\n");
        assert_eq!(
            applied.get("fov"),
            Some("0.25"),
            "an encoding this build cannot vouch for is left alone"
        );
        assert_eq!(applied.get("version"), Some("9999"));
        assert_eq!(applied.get("guiScale"), Some("2"));
        assert_eq!(applied.get("lang"), Some("de_de"));
    }

    #[test]
    fn the_window_size_keys_are_written_only_for_the_release_that_has_them() {
        // 1.13 moved to LWJGL 3 and dropped them; writing them into a modern
        // instance would add two keys the game never reads.
        let legacy = apply("1.12.2", "");
        assert_eq!(legacy.get("overrideWidth"), Some("1920"));
        assert_eq!(legacy.get("overrideHeight"), Some("1080"));

        for version in ["1.16.5", "1.20.1", "26.1"] {
            let modern = apply(version, "");
            assert!(!modern.contains("overrideWidth"), "{version}");
            assert!(!modern.contains("overrideHeight"), "{version}");
        }
    }

    #[test]
    fn simulation_distance_is_written_only_from_the_release_that_introduced_it() {
        assert!(!apply("1.12.2", "").contains("simulationDistance"));
        assert!(!apply("1.17.1", "").contains("simulationDistance"));
        assert_eq!(apply("1.18", "").get("simulationDistance"), Some("10"));
        assert_eq!(apply("1.20.1", "").get("simulationDistance"), Some("10"));
    }

    #[test]
    fn field_of_view_is_stored_normalized_rather_than_in_degrees() {
        // The game's own codec: degrees = 70 + 40 × stored.
        assert!((encode_fov(70.0) - 0.0).abs() < f64::EPSILON);
        assert!((encode_fov(90.0) - 0.5).abs() < f64::EPSILON);
        assert!((encode_fov(110.0) - 1.0).abs() < f64::EPSILON);
        assert!((encode_fov(30.0) + 1.0).abs() < f64::EPSILON);
        assert!((decode_fov(encode_fov(82.5)) - 82.5).abs() < 1e-9);

        assert_eq!(apply("1.20.1", "").get("fov"), Some("0.5"));
        assert_eq!(format_option_double(0.0), "0.0");
        assert_eq!(format_option_double(-1.0), "-1.0");
    }

    #[test]
    fn the_version_marker_is_never_written_and_never_copied() {
        // `version:` describes which historical fixes the game should
        // apply to the rest of the file. A value from another release is worse
        // than none.
        let created = apply("1.20.1", "");
        assert!(
            !created.contains(VERSION_KEY),
            "a file Ephervex creates claims no data version"
        );

        let existing = apply("1.20.1", "version:3465\nguiScale:0\n");
        assert_eq!(existing.get(VERSION_KEY), Some("3465"));
    }

    #[test]
    fn an_unmanaged_preference_leaves_the_instances_own_value_alone() {
        // only explicitly managed keys override what is already there.
        let mut prefs = everything_managed();
        prefs.render_distance.enabled = false;

        let applied = AdapterSet::default()
            .adapter_for("1.20.1")
            .apply(
                ParsedOptions::parse("renderDistance:32\nguiScale:0\n"),
                &prefs,
            )
            .expect("apply");

        assert_eq!(
            applied.get("renderDistance"),
            Some("32"),
            "the pack's value"
        );
        assert_eq!(applied.get("guiScale"), Some("2"), "the managed one");
    }

    #[test]
    fn applying_the_same_preferences_twice_changes_nothing_the_second_time() {
        // Import layering re-applies preferences after overrides, so
        // this has to be idempotent.
        let once = apply("1.20.1", "guiScale:0\n");
        let twice = AdapterSet::default()
            .adapter_for("1.20.1")
            .apply(once.clone(), &everything_managed())
            .expect("apply");
        assert_eq!(once, twice);
    }
}
