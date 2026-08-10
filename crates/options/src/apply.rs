//! Applying global preferences to an instance's own `options.txt`
//!
//! # Once, at creation
//!
//! Preferences are applied *once*, when a client instance is
//! created or imported, and afterwards the instance owns its file. There
//! is deliberately no "sync preferences to all instances" operation anywhere in
//! this crate: a user who turns their own render distance down inside the game
//! is making a decision about that instance, and a launcher that reverts it on
//! the next launch is a launcher fighting its user.
//!
//! The one exception is an import, which applies preferences after the pack's
//! own overrides have been laid down — still once, just later in the same
//! creation.
//!
//! # Only what was proven gets written
//!
//! Every managed preference goes through
//! [`OptionsCompatibility::support`](crate::OptionsCompatibility::support) for
//! the exact target release. An unmapped release, an unproven representation,
//! or a value the release's own domain does not accept all end the same way:
//! the preference is skipped, the reason is reported, and the file keeps
//! whatever it already had. Creating the instance is never blocked by any of
//! them.

use std::path::{Path, PathBuf};

use crate::codec::Unrepresentable;
use crate::compatibility::{
    CompatibilitySource, OptionsCompatibility, PreferenceSupport, component_value,
    needs_language_catalog,
};
use crate::error::OptionsError;
use crate::file::ParsedOptions;
use crate::preferences::{GlobalMinecraftPreferences, PreferenceKey};

/// The name of Minecraft's own settings file inside a game directory.
pub const OPTIONS_FILE_NAME: &str = "options.txt";

/// Whether the target release is known to offer the configured language.
///
/// A language code is the one preference whose validity is not a property of
/// `options.txt` at all: the key and its encoding are stable everywhere, but
/// the set of codes a release accepts comes from that release's own language
/// catalog. Ephervex resolves that separately and passes the verdict in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LanguageAvailability {
    /// The target release's catalog was read and contains the code.
    Verified,
    /// The catalog was read and does not contain the code.
    Absent,
    /// No catalog for that release was available, so nothing is claimed.
    #[default]
    Unverified,
}

/// Why one managed preference was not written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// Outside the domain this release was proven to accept.
    OutOfRange,
    /// Not one of the values this release's setting enumerates.
    NotInDomain,
    /// Could not be written as one options file line.
    Unwritable,
    /// The value is not the shape this release stores.
    TypeMismatch,
    /// The release's language catalog does not list the configured code.
    LanguageAbsent,
    /// No language catalog for the release was available to check against.
    LanguageUnverified,
}

impl SkipReason {
    /// A short stable name, for developer logs and reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OutOfRange => "out-of-range",
            Self::NotInDomain => "not-in-domain",
            Self::Unwritable => "unwritable",
            Self::TypeMismatch => "type-mismatch",
            Self::LanguageAbsent => "language-absent",
            Self::LanguageUnverified => "language-unverified",
        }
    }
}

impl From<Unrepresentable> for SkipReason {
    fn from(error: Unrepresentable) -> Self {
        match error {
            Unrepresentable::OutOfRange => Self::OutOfRange,
            Unrepresentable::NotInDomain => Self::NotInDomain,
            Unrepresentable::Unwritable => Self::Unwritable,
            Unrepresentable::TypeMismatch => Self::TypeMismatch,
        }
    }
}

/// One managed preference that was not written, and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkippedPreference {
    pub key: PreferenceKey,
    pub reason: SkipReason,
}

/// What applying global preferences to one instance did.
///
/// Reported rather than logged and forgotten, because "why is my GUI scale not
/// set" has a small number of answers — the preference is not managed, this
/// release has no such setting, nobody has established what this release does,
/// or the value does not fit what the release accepts — and all of them are
/// here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreferenceApplication {
    /// The file that was written, or would have been.
    pub path: PathBuf,
    /// The release the file was written for.
    pub minecraft_version: String,
    /// The schema family this release resolved to, or `None` when the release
    /// is not in the compatibility dataset at all.
    pub schema: Option<String>,
    /// Whether the knowledge used was the bundled baseline or the remote feed.
    pub source: CompatibilitySource,
    /// Whether the instance had no `options.txt` before this.
    pub created_file: bool,
    /// Preferences that were managed and written.
    pub applied: Vec<PreferenceKey>,
    /// Preferences that were managed but which this Minecraft release provably
    /// does not have.
    pub unsupported: Vec<PreferenceKey>,
    /// Preferences whose representation has not been established for this
    /// release. Never written, and deliberately not reported as unsupported.
    pub unknown: Vec<PreferenceKey>,
    /// Preferences the release does have, but whose configured value it cannot
    /// express.
    pub skipped: Vec<SkippedPreference>,
}

impl PreferenceApplication {
    /// Whether anything at all was written to the instance.
    #[must_use]
    pub fn changed_anything(&self) -> bool {
        !self.applied.is_empty()
    }
}

/// Applies the managed global preferences to `<game_dir>/options.txt`.
///
/// Reads the existing file when there is one — an imported pack may ship its
/// own — merges only the managed keys into it, and writes it back atomically.
/// A pack's file with nothing to apply is not rewritten, so an import that
/// changes nothing leaves the bytes it was given.
pub fn apply_to_game_directory(
    game_dir: &Path,
    minecraft_version: &str,
    preferences: &GlobalMinecraftPreferences,
    compatibility: &OptionsCompatibility,
    language: LanguageAvailability,
) -> Result<PreferenceApplication, OptionsError> {
    let path = game_dir.join(OPTIONS_FILE_NAME);
    let mut report = PreferenceApplication {
        path: path.clone(),
        minecraft_version: minecraft_version.to_owned(),
        schema: compatibility
            .schema_id(minecraft_version)
            .map(str::to_owned),
        source: compatibility.source(),
        created_file: false,
        applied: Vec::new(),
        unsupported: Vec::new(),
        unknown: Vec::new(),
        skipped: Vec::new(),
    };

    let mut writes: Vec<(String, String)> = Vec::new();
    for key in preferences.managed_keys() {
        match compatibility.support(key, minecraft_version) {
            PreferenceSupport::Unsupported => {
                tracing::debug!(
                    preference = %key,
                    version = minecraft_version,
                    "skipping a managed preference; this Minecraft release has no setting for it"
                );
                report.unsupported.push(key);
            }
            PreferenceSupport::Unknown => {
                tracing::info!(
                    preference = %key,
                    version = minecraft_version,
                    "skipping a managed preference; how this Minecraft release stores it has not been established"
                );
                report.unknown.push(key);
            }
            PreferenceSupport::Authoritative(fields) => {
                if needs_language_catalog(&fields) && language != LanguageAvailability::Verified {
                    let reason = match language {
                        LanguageAvailability::Absent => SkipReason::LanguageAbsent,
                        _ => SkipReason::LanguageUnverified,
                    };
                    tracing::info!(
                        preference = %key,
                        version = minecraft_version,
                        reason = reason.as_str(),
                        "skipping the managed language preference; support could not be verified for this release"
                    );
                    report.skipped.push(SkippedPreference { key, reason });
                    continue;
                }

                match encode_fields(preferences, key, &fields) {
                    Ok(encoded) => {
                        writes.extend(encoded);
                        report.applied.push(key);
                    }
                    Err(reason) => {
                        tracing::info!(
                            preference = %key,
                            version = minecraft_version,
                            reason = reason.as_str(),
                            "skipping a managed preference; this Minecraft release cannot express the configured value"
                        );
                        report.skipped.push(SkippedPreference { key, reason });
                    }
                }
            }
        }
    }

    let existing = read_options(&path)?;
    report.created_file = existing.is_none();

    if writes.is_empty() {
        // Writing the file back would rewrite an imported pack's own options
        // for no reason.
        tracing::debug!(
            version = minecraft_version,
            "no global Minecraft preferences to apply; leaving options.txt alone"
        );
        report.applied.clear();
        return Ok(report);
    }

    let mut updated = existing.unwrap_or_else(ParsedOptions::empty);
    for (key, value) in writes {
        updated.set(&key, value);
    }
    write_options(&path, &updated.render())?;

    tracing::info!(
        path = %path.display(),
        version = minecraft_version,
        schema = report.schema.as_deref().unwrap_or("unmapped"),
        source = report.source.as_str(),
        created = report.created_file,
        applied = report.applied.len(),
        "applied global Minecraft preferences"
    );
    Ok(report)
}

/// Encodes every component of one preference, or none of them.
///
/// All or nothing on purpose: a window size whose width was writable and whose
/// height was not would configure half a window.
fn encode_fields(
    preferences: &GlobalMinecraftPreferences,
    key: PreferenceKey,
    fields: &[crate::compatibility::ResolvedField<'_>],
) -> Result<Vec<(String, String)>, SkipReason> {
    let mut encoded = Vec::with_capacity(fields.len());
    for field in fields {
        let Some(value) = component_value(preferences, key, field.component) else {
            // Only reachable if a managed preference has no value for a
            // component the dataset expects, which is a build-level mismatch
            // rather than anything the user did.
            return Err(SkipReason::TypeMismatch);
        };
        let text = field.representation.codec.encode(&value)?;
        encoded.push((field.representation.key.clone(), text));
    }
    Ok(encoded)
}

/// Reads an instance's options file, or `None` when it has none yet.
///
/// A file that exists but cannot be read is an error rather than an empty file:
/// treating "permission denied" as "no settings" would replace a user's whole
/// configuration with a handful of managed keys.
fn read_options(path: &Path) -> Result<Option<ParsedOptions>, OptionsError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(ParsedOptions::parse(&text))),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(OptionsError::Unreadable {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Writes the options file through a temporary sibling, as everything else in
/// this workspace that a user would hate to lose half of.
fn write_options(path: &Path, text: &str) -> Result<(), OptionsError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| OptionsError::Unwritable {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    let temporary = path.with_extension("txt.tmp");
    std::fs::write(&temporary, text).map_err(|source| OptionsError::Unwritable {
        path: temporary.clone(),
        source,
    })?;
    std::fs::rename(&temporary, path).map_err(|source| OptionsError::Unwritable {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::preferences::{GuiScalePreference, Managed, WindowSize};

    fn gui_scale_only() -> GlobalMinecraftPreferences {
        GlobalMinecraftPreferences {
            gui_scale: Managed::apply(GuiScalePreference::Fixed(3)),
            ..GlobalMinecraftPreferences::default()
        }
    }

    fn apply(
        game: &Path,
        version: &str,
        preferences: &GlobalMinecraftPreferences,
    ) -> PreferenceApplication {
        apply_to_game_directory(
            game,
            version,
            preferences,
            &OptionsCompatibility::bundled(),
            LanguageAvailability::Verified,
        )
        .expect("apply")
    }

    #[test]
    fn one_normalized_preference_reaches_two_different_releases_correctly() {
        // The same normalized preference, two Minecraft versions, no file
        // copied between them.
        let base = tempfile::tempdir().expect("temp dir");
        let old = base.path().join("old");
        let new = base.path().join("new");
        std::fs::create_dir_all(&old).expect("mkdir");
        std::fs::create_dir_all(&new).expect("mkdir");

        let mut preferences = gui_scale_only();
        preferences.fov = Managed::apply(90.0);

        let legacy = apply(&old, "1.12.2", &preferences);
        let modern = apply(&new, "1.20.1", &preferences);

        assert!(legacy.created_file && modern.created_file);
        assert_eq!(
            legacy.applied,
            vec![PreferenceKey::GuiScale, PreferenceKey::Fov]
        );

        let legacy_text = std::fs::read_to_string(old.join(OPTIONS_FILE_NAME)).expect("read");
        let modern_text = std::fs::read_to_string(new.join(OPTIONS_FILE_NAME)).expect("read");
        assert!(legacy_text.contains("guiScale:3"));
        assert!(modern_text.contains("guiScale:3"));
        assert!(
            legacy_text.contains("fov:0.5"),
            "1.12.2 stores (degrees - 70) / 40, got {legacy_text:?}"
        );
        assert!(
            modern_text.contains("fov:90.0"),
            "1.20.1 stores degrees, got {modern_text:?}"
        );
        assert!(!legacy_text.contains("version:"));
        assert!(!modern_text.contains("version:"));
    }

    #[test]
    fn an_existing_file_keeps_everything_the_preferences_do_not_name() {
        let base = tempfile::tempdir().expect("temp dir");
        let game = base.path().to_path_buf();
        let original = "version:3465\nguiScale:0\nrenderDistance:32\nmodSetting:{\"a\":1}\n";
        std::fs::write(game.join(OPTIONS_FILE_NAME), original).expect("write");

        let report = apply(&game, "1.20.1", &gui_scale_only());
        assert!(!report.created_file);
        assert_eq!(report.applied, vec![PreferenceKey::GuiScale]);

        let text = std::fs::read_to_string(game.join(OPTIONS_FILE_NAME)).expect("read");
        assert_eq!(
            text,
            "version:3465\nguiScale:3\nrenderDistance:32\nmodSetting:{\"a\":1}\n"
        );
    }

    #[test]
    fn nothing_managed_means_the_file_is_not_touched_at_all() {
        let base = tempfile::tempdir().expect("temp dir");
        let game = base.path().to_path_buf();
        let original = "version:3465\nguiScale:0\n";
        std::fs::write(game.join(OPTIONS_FILE_NAME), original).expect("write");

        let report = apply(&game, "1.20.1", &GlobalMinecraftPreferences::default());
        assert!(!report.changed_anything());
        assert_eq!(
            std::fs::read_to_string(game.join(OPTIONS_FILE_NAME)).expect("read"),
            original
        );
    }

    #[test]
    fn a_managed_preference_the_release_cannot_express_creates_no_file() {
        let base = tempfile::tempdir().expect("temp dir");
        let game = base.path().to_path_buf();
        let preferences = GlobalMinecraftPreferences {
            simulation_distance: Managed::apply(8),
            ..GlobalMinecraftPreferences::default()
        };

        let report = apply(&game, "1.12.2", &preferences);
        assert_eq!(report.unsupported, vec![PreferenceKey::SimulationDistance]);
        assert!(report.applied.is_empty());
        assert!(
            !game.join(OPTIONS_FILE_NAME).exists(),
            "an empty options.txt is worse than none"
        );
    }

    #[test]
    fn a_release_with_no_compatibility_entry_is_reported_unknown_and_written_to_not_at_all() {
        // The day after a Minecraft release: the instance is usable, and no
        // preference is written on a guess that it matches the previous one.
        let base = tempfile::tempdir().expect("temp dir");
        let game = base.path().to_path_buf();

        let report = apply(&game, "26.99", &gui_scale_only());
        assert_eq!(report.schema, None);
        assert_eq!(report.unknown, vec![PreferenceKey::GuiScale]);
        assert!(report.applied.is_empty());
        assert!(!game.join(OPTIONS_FILE_NAME).exists());
    }

    #[test]
    fn the_language_preference_waits_for_the_target_releases_own_catalog() {
        let base = tempfile::tempdir().expect("temp dir");
        let preferences = GlobalMinecraftPreferences {
            language: Managed::apply("de_de".to_owned()),
            ..GlobalMinecraftPreferences::default()
        };

        for (availability, expected) in [
            (
                LanguageAvailability::Unverified,
                SkipReason::LanguageUnverified,
            ),
            (LanguageAvailability::Absent, SkipReason::LanguageAbsent),
        ] {
            let game = base.path().join(expected.as_str());
            std::fs::create_dir_all(&game).expect("mkdir");
            let report = apply_to_game_directory(
                &game,
                "1.12.2",
                &preferences,
                &OptionsCompatibility::bundled(),
                availability,
            )
            .expect("apply");
            assert_eq!(
                report.skipped,
                vec![SkippedPreference {
                    key: PreferenceKey::Language,
                    reason: expected
                }]
            );
            assert!(!game.join(OPTIONS_FILE_NAME).exists());
        }

        let game = base.path().join("verified");
        std::fs::create_dir_all(&game).expect("mkdir");
        let report = apply(&game, "1.12.2", &preferences);
        assert_eq!(report.applied, vec![PreferenceKey::Language]);
        assert!(
            std::fs::read_to_string(game.join(OPTIONS_FILE_NAME))
                .expect("read")
                .contains("lang:de_de")
        );
    }

    #[test]
    fn a_value_outside_the_releases_proven_domain_is_skipped_with_a_reason() {
        let base = tempfile::tempdir().expect("temp dir");
        let game = base.path().to_path_buf();
        let preferences = GlobalMinecraftPreferences {
            render_distance: Managed::apply(32),
            simulation_distance: Managed::apply(32),
            ..GlobalMinecraftPreferences::default()
        };

        // 1.21.9 narrowed the simulation distance value set; render distance
        // still spans 2..32 there.
        let report = apply(&game, "26.2", &preferences);
        assert_eq!(report.applied, vec![PreferenceKey::RenderDistance]);
        assert_eq!(
            report.skipped,
            vec![SkippedPreference {
                key: PreferenceKey::SimulationDistance,
                reason: SkipReason::OutOfRange
            }]
        );
    }

    #[test]
    fn a_window_size_is_written_whole_or_not_at_all() {
        let base = tempfile::tempdir().expect("temp dir");
        let game = base.path().to_path_buf();
        let preferences = GlobalMinecraftPreferences {
            window_size: Managed::apply(WindowSize {
                width: 1920,
                height: 1080,
            }),
            ..GlobalMinecraftPreferences::default()
        };

        let report = apply(&game, "1.12.2", &preferences);
        assert_eq!(report.applied, vec![PreferenceKey::WindowSize]);
        let text = std::fs::read_to_string(game.join(OPTIONS_FILE_NAME)).expect("read");
        assert!(text.contains("overrideWidth:1920"));
        assert!(text.contains("overrideHeight:1080"));
    }

    #[test]
    fn applying_the_same_preferences_twice_changes_nothing_the_second_time() {
        // Import layering re-applies preferences after overrides, so this has
        // to be idempotent.
        let base = tempfile::tempdir().expect("temp dir");
        let game = base.path().to_path_buf();
        let mut preferences = gui_scale_only();
        preferences.fov = Managed::apply(90.0);

        apply(&game, "1.20.1", &preferences);
        let once = std::fs::read_to_string(game.join(OPTIONS_FILE_NAME)).expect("read");
        apply(&game, "1.20.1", &preferences);
        let twice = std::fs::read_to_string(game.join(OPTIONS_FILE_NAME)).expect("read");
        assert_eq!(once, twice);
    }

    #[test]
    fn the_game_directory_is_created_when_it_does_not_exist_yet() {
        let base = tempfile::tempdir().expect("temp dir");
        let game = base.path().join("minecraft");

        apply(&game, "1.20.1", &gui_scale_only());
        assert!(game.join(OPTIONS_FILE_NAME).is_file());
        assert!(!game.join("options.txt.tmp").exists());
    }

    #[test]
    fn an_unreadable_file_fails_rather_than_replacing_the_users_settings() {
        // Reading "permission denied" as "no settings yet" would overwrite a
        // whole configuration with a handful of managed keys.
        let base = tempfile::tempdir().expect("temp dir");
        let game = base.path().to_path_buf();
        // A directory where the file should be: readable metadata, unreadable
        // as text, and portable to test.
        std::fs::create_dir(game.join(OPTIONS_FILE_NAME)).expect("mkdir");

        let error = apply_to_game_directory(
            &game,
            "1.20.1",
            &gui_scale_only(),
            &OptionsCompatibility::bundled(),
            LanguageAvailability::Verified,
        )
        .expect_err("must not be treated as absent");
        assert!(matches!(error, OptionsError::Unreadable { .. }));
        assert_eq!(error.message_id(), "error-options-unreadable");
        assert_eq!(error.failing_path(), Some(game.join(OPTIONS_FILE_NAME)));
    }
}
