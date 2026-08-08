//! Applying global preferences to an instance's own `options.txt`
//!
//! # Once, at creation
//!
//! Preferences are applied *once*, when a client instance is
//! created or imported, and that afterwards the instance owns its file. There
//! is deliberately no "sync preferences to all instances" operation anywhere in
//! this crate: a user who turns their own render distance down inside the game
//! is making a decision about that instance, and a launcher that reverts it on
//! the next launch is a launcher fighting its user.
//!
//! The one exception is an import, which applies preferences after the pack's
//! own overrides have been laid down — still once, just
//! later in the same creation.

use std::path::{Path, PathBuf};

use crate::adapter::AdapterSet;
use crate::error::OptionsError;
use crate::file::ParsedOptions;
use crate::preferences::{GlobalMinecraftPreferences, PreferenceKey};

/// The name of Minecraft's own settings file inside a game directory.
pub const OPTIONS_FILE_NAME: &str = "options.txt";

/// What applying global preferences to one instance did.
///
/// Reported rather than logged and forgotten, because "why is my GUI scale not
/// set" has exactly two answers — the preference is not managed, or this
/// Minecraft release cannot express it — and both of them are here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreferenceApplication {
    /// The file that was written, or would have been.
    pub path: PathBuf,
    /// Which adapter was chosen ([`crate::OptionsAdapter::name`]).
    pub adapter: &'static str,
    /// Whether the instance had no `options.txt` before this.
    pub created_file: bool,
    /// Preferences that were managed and written.
    pub applied: Vec<PreferenceKey>,
    /// Preferences that were managed but which this Minecraft release cannot
    /// express — window size on a modern release, simulation distance on an
    /// old one.
    pub unsupported: Vec<PreferenceKey>,
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
/// A pack's file with no managed preferences at all is not rewritten, so an
/// import that changes nothing leaves the bytes it was given.
pub fn apply_to_game_directory(
    game_dir: &Path,
    minecraft_version: &str,
    preferences: &GlobalMinecraftPreferences,
) -> Result<PreferenceApplication, OptionsError> {
    let path = game_dir.join(OPTIONS_FILE_NAME);
    let adapter = AdapterSet::default().adapter_for(minecraft_version);

    let expressible = adapter.expressible_keys();
    let managed = preferences.managed_keys();
    let (applied, unsupported): (Vec<_>, Vec<_>) = managed
        .into_iter()
        .partition(|key| expressible.contains(key));

    for key in &unsupported {
        tracing::debug!(
            preference = %key,
            version = minecraft_version,
            adapter = adapter.name(),
            "skipping a managed preference; this Minecraft release has no setting for it"
        );
    }

    let existing = read_options(&path)?;
    let created_file = existing.is_none();
    let report = PreferenceApplication {
        path: path.clone(),
        adapter: adapter.name(),
        created_file,
        applied,
        unsupported,
    };

    if !report.changed_anything() {
        // Nothing managed is expressible here. Writing the file back would
        // rewrite an imported pack's own options for no reason.
        tracing::debug!(
            version = minecraft_version,
            "no global Minecraft preferences to apply; leaving options.txt alone"
        );
        return Ok(report);
    }

    let updated = adapter.apply(existing.unwrap_or_else(ParsedOptions::empty), preferences)?;
    write_options(&path, &updated.render())?;

    tracing::info!(
        path = %path.display(),
        adapter = report.adapter,
        created = report.created_file,
        applied = report.applied.len(),
        "applied global Minecraft preferences"
    );
    Ok(report)
}

/// Reads an instance's options file, or `None` when it has none yet.
///
/// A file that exists but cannot be read is an error rather than an empty file:
/// treating "permission denied" as "no settings" would replace a user's whole
/// configuration with three managed keys.
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

    #[test]
    fn one_normalized_preference_reaches_two_different_releases_correctly() {
        // The instance and preference management required behavior, at the level this crate owns: the
        // same normalized preference, two Minecraft versions, no file copied
        // between them.
        let base = tempfile::tempdir().expect("temp dir");
        let old = base.path().join("old");
        let new = base.path().join("new");
        std::fs::create_dir_all(&old).expect("mkdir");
        std::fs::create_dir_all(&new).expect("mkdir");

        let mut preferences = gui_scale_only();
        preferences.window_size = Managed::apply(WindowSize {
            width: 1280,
            height: 720,
        });

        let legacy = apply_to_game_directory(&old, "1.12.2", &preferences).expect("apply");
        let modern = apply_to_game_directory(&new, "1.20.1", &preferences).expect("apply");

        assert_eq!(legacy.adapter, "minecraft-1.12");
        assert_eq!(modern.adapter, "minecraft-1.18");
        assert!(legacy.created_file && modern.created_file);
        assert_eq!(legacy.unsupported, Vec::new());
        assert_eq!(
            modern.unsupported,
            vec![PreferenceKey::WindowSize],
            "a modern release has no options key for the window size"
        );

        let legacy_text = std::fs::read_to_string(old.join(OPTIONS_FILE_NAME)).expect("read");
        let modern_text = std::fs::read_to_string(new.join(OPTIONS_FILE_NAME)).expect("read");
        assert!(legacy_text.contains("guiScale:3"));
        assert!(modern_text.contains("guiScale:3"));
        assert!(legacy_text.contains("overrideWidth:1280"));
        assert!(!modern_text.contains("overrideWidth"));
        assert!(!legacy_text.contains("version:"));
        assert!(!modern_text.contains("version:"));
    }

    #[test]
    fn an_existing_file_keeps_everything_the_preferences_do_not_name() {
        let base = tempfile::tempdir().expect("temp dir");
        let game = base.path().to_path_buf();
        let original = "version:3465\nguiScale:0\nrenderDistance:32\nmodSetting:{\"a\":1}\n";
        std::fs::write(game.join(OPTIONS_FILE_NAME), original).expect("write");

        let report = apply_to_game_directory(&game, "1.20.1", &gui_scale_only()).expect("apply");
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

        let report =
            apply_to_game_directory(&game, "1.20.1", &GlobalMinecraftPreferences::default())
                .expect("apply");
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

        let report = apply_to_game_directory(&game, "1.12.2", &preferences).expect("apply");
        assert_eq!(report.unsupported, vec![PreferenceKey::SimulationDistance]);
        assert!(report.applied.is_empty());
        assert!(
            !game.join(OPTIONS_FILE_NAME).exists(),
            "an empty options.txt is worse than none"
        );
    }

    #[test]
    fn the_game_directory_is_created_when_it_does_not_exist_yet() {
        let base = tempfile::tempdir().expect("temp dir");
        let game = base.path().join("minecraft");

        apply_to_game_directory(&game, "1.20.1", &gui_scale_only()).expect("apply");
        assert!(game.join(OPTIONS_FILE_NAME).is_file());
        assert!(!game.join("options.txt.tmp").exists());
    }

    #[test]
    fn an_unreadable_file_fails_rather_than_replacing_the_users_settings() {
        // Reading "permission denied" as "no settings yet" would overwrite a
        // whole configuration with three managed keys.
        let base = tempfile::tempdir().expect("temp dir");
        let game = base.path().to_path_buf();
        // A directory where the file should be: readable metadata, unreadable
        // as text, and portable to test.
        std::fs::create_dir(game.join(OPTIONS_FILE_NAME)).expect("mkdir");

        let error = apply_to_game_directory(&game, "1.20.1", &gui_scale_only())
            .expect_err("must not be treated as absent");
        assert!(matches!(error, OptionsError::Unreadable { .. }));
        assert_eq!(error.message_id(), "error-options-unreadable");
        assert_eq!(error.failing_path(), Some(game.join(OPTIONS_FILE_NAME)));
    }
}
