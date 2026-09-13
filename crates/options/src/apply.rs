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
//! own overrides have been laid down - still once, just later in the same
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

use crate::catalog::MinecraftPreferenceCatalog;
use crate::codec::{OptionCodec, SettingValue, Unrepresentable};
use crate::compatibility::{
    CompatibilitySource, OptionsCompatibility, PreferenceComponent, PreferenceSupport,
    ResolvedField, needs_language_catalog,
};
use crate::error::OptionsError;
use crate::file::{ParsedOptions, VERSION_KEY};
use crate::preference::{ConfiguredPreferences, MinecraftPreferenceValue, PreferenceId};
use crate::preferences::GlobalMinecraftPreferences;

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
    /// The configured input has no representation in the key storage form
    /// this release uses.
    InputUnavailable,
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
            Self::InputUnavailable => "input-unavailable",
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
            Unrepresentable::InputUnavailable => Self::InputUnavailable,
        }
    }
}

/// One managed preference that was not written, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedPreference {
    pub id: PreferenceId,
    pub reason: SkipReason,
}

/// What one configured preference resolves to for one exact release.
///
/// The four "not written" answers are deliberately separate. A settings
/// screen has to be able to say *why* a row is inert, and "this release has no
/// such setting", "nobody has established what this release does", and "this
/// release cannot express the value you chose" are three different sentences
/// with three different remedies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreferenceDisposition {
    /// Managed, authoritative for this release, and the configured value
    /// encodes through the release's own proven codec.
    Writable,
    /// Configured but not applied to new instances.
    Unmanaged,
    /// This build has no product descriptor for the id.
    Unrecognized,
    /// This release provably has no such setting.
    Unsupported,
    /// Nothing is established about how this release stores it.
    Unknown,
    /// This release has the setting, but cannot express the configured value.
    NotRepresentable(SkipReason),
}

impl PreferenceDisposition {
    /// A short stable name, for developer logs and reports.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Writable => "writable",
            Self::Unmanaged => "unmanaged",
            Self::Unrecognized => "unrecognized",
            Self::Unsupported => "unsupported",
            Self::Unknown => "unknown",
            Self::NotRepresentable(_) => "not-representable",
        }
    }

    /// Whether this preference would change the instance's file.
    #[must_use]
    pub const fn is_writable(&self) -> bool {
        matches!(self, Self::Writable)
    }
}

/// One configured preference resolved against one exact release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedPreference {
    pub id: PreferenceId,
    pub disposition: PreferenceDisposition,
    /// The exact `options.txt` assignments this preference produces, empty
    /// unless the disposition is [`PreferenceDisposition::Writable`]. All of
    /// them or none: a compound preference never contributes half its keys.
    writes: Vec<(String, String)>,
}

impl PlannedPreference {
    /// The `options.txt` assignments this preference would make.
    #[must_use]
    pub fn writes(&self) -> &[(String, String)] {
        &self.writes
    }
}

/// What applying the configured preferences to one release would do, resolved
/// without touching any instance.
///
/// The dry form of [`apply_managed_preferences`], so a settings screen can
/// show an exact per-release preview and a creation can decide what it is
/// about to write before it writes anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreferencePlan {
    /// The release this plan was resolved for.
    pub minecraft_version: String,
    /// The schema family this release resolved to, or `None` when the release
    /// is not in the compatibility dataset at all.
    pub schema: Option<String>,
    /// Whether the knowledge used was the bundled baseline or the remote feed.
    pub source: CompatibilitySource,
    /// The data version this release stamps into its own options file, when
    /// the compatibility data knows it.
    pub data_version: Option<i64>,
    /// Every configured preference, in id order.
    pub entries: Vec<PlannedPreference>,
}

impl PreferencePlan {
    /// One preference's resolution, or `None` when it is not configured.
    #[must_use]
    pub fn entry(&self, id: &PreferenceId) -> Option<&PlannedPreference> {
        self.entries.iter().find(|entry| entry.id == *id)
    }

    /// The ids that would be written, in id order.
    #[must_use]
    pub fn writable(&self) -> Vec<PreferenceId> {
        self.ids_with(PreferenceDisposition::is_writable)
    }

    fn ids_with(&self, predicate: impl Fn(&PreferenceDisposition) -> bool) -> Vec<PreferenceId> {
        self.entries
            .iter()
            .filter(|entry| predicate(&entry.disposition))
            .map(|entry| entry.id.clone())
            .collect()
    }

    /// Every assignment this plan makes, in id order.
    fn assignments(&self) -> Vec<(String, String)> {
        self.entries
            .iter()
            .flat_map(|entry| entry.writes.iter().cloned())
            .collect()
    }
}

/// What applying global preferences to one instance did.
///
/// Reported rather than logged and forgotten, because "why is my GUI scale not
/// set" has a small number of answers - the preference is not managed, this
/// release has no such setting, nobody has established what this release does,
/// or the value does not fit what the release accepts - and all of them are
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
    pub applied: Vec<PreferenceId>,
    /// Preferences that were managed but which this Minecraft release provably
    /// does not have.
    pub unsupported: Vec<PreferenceId>,
    /// Preferences whose representation has not been established for this
    /// release. Never written, and deliberately not reported as unsupported.
    pub unknown: Vec<PreferenceId>,
    /// Preferences configured under an id this build has no descriptor for.
    /// Left in the configuration file untouched and never applied.
    pub unrecognized: Vec<PreferenceId>,
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

/// Resolves every configured preference against one exact Minecraft release,
/// without reading or writing any instance.
///
/// Nothing here guesses. A release the dataset does not name resolves to
/// [`PreferenceDisposition::Unknown`] for everything, a value outside a
/// release's proven domain resolves to
/// [`PreferenceDisposition::NotRepresentable`] rather than being moved to the
/// nearest value the release would accept, and a compound preference resolves
/// as one unit.
#[must_use]
pub fn plan_managed_preferences(
    minecraft_version: &str,
    preferences: &ConfiguredPreferences,
    catalog: &MinecraftPreferenceCatalog,
    compatibility: &OptionsCompatibility,
    language: LanguageAvailability,
) -> PreferencePlan {
    let mut entries = Vec::with_capacity(preferences.len());

    for (id, configured) in preferences.iter() {
        let disposition = if configured.managed {
            resolve_one(
                id,
                &configured.value,
                minecraft_version,
                catalog,
                compatibility,
                language,
            )
        } else {
            (PreferenceDisposition::Unmanaged, Vec::new())
        };
        entries.push(PlannedPreference {
            id: id.clone(),
            disposition: disposition.0,
            writes: disposition.1,
        });
    }

    PreferencePlan {
        minecraft_version: minecraft_version.to_owned(),
        schema: compatibility
            .schema_id(minecraft_version)
            .map(str::to_owned),
        source: compatibility.source(),
        data_version: compatibility.data_version(minecraft_version),
        entries,
    }
}

/// Resolves one managed preference, and the assignments it would make.
fn resolve_one(
    id: &PreferenceId,
    value: &MinecraftPreferenceValue,
    minecraft_version: &str,
    catalog: &MinecraftPreferenceCatalog,
    compatibility: &OptionsCompatibility,
    language: LanguageAvailability,
) -> (PreferenceDisposition, Vec<(String, String)>) {
    let Some(descriptor) = catalog.descriptor(id.as_str()) else {
        return (PreferenceDisposition::Unrecognized, Vec::new());
    };
    if descriptor.validate_value(value).is_err() {
        // A value that does not fit the product descriptor cannot be
        // meaningfully asked about a release, and must never be repaired into
        // one that does.
        return (
            PreferenceDisposition::NotRepresentable(SkipReason::TypeMismatch),
            Vec::new(),
        );
    }

    let fields = match compatibility.descriptor_support(descriptor, minecraft_version) {
        PreferenceSupport::Unsupported => return (PreferenceDisposition::Unsupported, Vec::new()),
        PreferenceSupport::Unknown => return (PreferenceDisposition::Unknown, Vec::new()),
        PreferenceSupport::Authoritative(fields) => fields,
    };

    if needs_language_catalog(&fields) && language != LanguageAvailability::Verified {
        let reason = match language {
            LanguageAvailability::Absent => SkipReason::LanguageAbsent,
            _ => SkipReason::LanguageUnverified,
        };
        return (PreferenceDisposition::NotRepresentable(reason), Vec::new());
    }

    match encode_fields(value, &fields) {
        Ok(writes) => (PreferenceDisposition::Writable, writes),
        Err(reason) => (PreferenceDisposition::NotRepresentable(reason), Vec::new()),
    }
}

/// Applies the configured global preferences to `<game_dir>/options.txt`.
///
/// Reads the existing file when there is one (an imported pack may ship its
/// own), merges only the writable preferences into it, and writes it back
/// atomically. A pack's file with nothing to apply is not rewritten, so an
/// import that changes nothing leaves the bytes it was given.
pub fn apply_managed_preferences(
    game_dir: &Path,
    minecraft_version: &str,
    preferences: &ConfiguredPreferences,
    catalog: &MinecraftPreferenceCatalog,
    compatibility: &OptionsCompatibility,
    language: LanguageAvailability,
) -> Result<PreferenceApplication, OptionsError> {
    let plan = plan_managed_preferences(
        minecraft_version,
        preferences,
        catalog,
        compatibility,
        language,
    );
    apply_plan(game_dir, &plan)
}

/// Writes one already-resolved plan into an instance's options file.
///
/// Split from [`plan_managed_preferences`] so that everything which can decide
/// *not* to write has already decided before the file is touched.
pub fn apply_plan(
    game_dir: &Path,
    plan: &PreferencePlan,
) -> Result<PreferenceApplication, OptionsError> {
    let path = game_dir.join(OPTIONS_FILE_NAME);
    let mut report = PreferenceApplication {
        path: path.clone(),
        minecraft_version: plan.minecraft_version.clone(),
        schema: plan.schema.clone(),
        source: plan.source,
        created_file: false,
        applied: Vec::new(),
        unsupported: Vec::new(),
        unknown: Vec::new(),
        unrecognized: Vec::new(),
        skipped: Vec::new(),
    };

    for entry in &plan.entries {
        match &entry.disposition {
            PreferenceDisposition::Writable => report.applied.push(entry.id.clone()),
            PreferenceDisposition::Unmanaged => {}
            PreferenceDisposition::Unrecognized => {
                report.unrecognized.push(entry.id.clone());
            }
            PreferenceDisposition::Unsupported => {
                tracing::debug!(
                    preference = %entry.id,
                    version = plan.minecraft_version,
                    "skipping a managed preference; this Minecraft release has no setting for it"
                );
                report.unsupported.push(entry.id.clone());
            }
            PreferenceDisposition::Unknown => {
                tracing::info!(
                    preference = %entry.id,
                    version = plan.minecraft_version,
                    "skipping a managed preference; how this Minecraft release stores it has not been established"
                );
                report.unknown.push(entry.id.clone());
            }
            PreferenceDisposition::NotRepresentable(reason) => {
                tracing::info!(
                    preference = %entry.id,
                    version = plan.minecraft_version,
                    reason = reason.as_str(),
                    "skipping a managed preference; this Minecraft release cannot express the configured value"
                );
                report.skipped.push(SkippedPreference {
                    id: entry.id.clone(),
                    reason: *reason,
                });
            }
        }
    }

    let existing = read_options(&path)?;
    report.created_file = existing.is_none();

    let writes = plan.assignments();
    if writes.is_empty() {
        // Writing the file back would rewrite an imported pack's own options
        // for no reason.
        tracing::debug!(
            version = plan.minecraft_version,
            "no global Minecraft preferences to apply; leaving options.txt alone"
        );
        report.applied.clear();
        return Ok(report);
    }

    let mut updated = existing.unwrap_or_else(ParsedOptions::empty);
    // A file this build creates has to say which release wrote it. The game
    // reads that back to decide which of its historical fixes the rest of the
    // file still needs, and a file without one is treated as predating every
    // one of them, which is how a perfectly good setting arrives in the game
    // discarded. An existing file keeps its own marker: it belongs to whoever
    // wrote it, and another release's value is worse than none.
    if report.created_file
        && let Some(data_version) = plan.data_version
    {
        updated.set(VERSION_KEY, data_version.to_string());
    }
    for (key, value) in writes {
        updated.set(&key, value);
    }
    write_options(&path, &updated.render())?;

    tracing::info!(
        path = %path.display(),
        version = plan.minecraft_version,
        schema = report.schema.as_deref().unwrap_or("unmapped"),
        source = report.source.as_str(),
        created = report.created_file,
        applied = report.applied.len(),
        "applied global Minecraft preferences"
    );
    Ok(report)
}

/// Applies the seven preferences that predate the generated product catalog.
///
/// The compatibility facade over [`apply_managed_preferences`], kept so the
/// existing configuration and command-line vocabulary keeps working
/// unchanged. It resolves through exactly the same engine.
pub fn apply_to_game_directory(
    game_dir: &Path,
    minecraft_version: &str,
    preferences: &GlobalMinecraftPreferences,
    compatibility: &OptionsCompatibility,
    language: LanguageAvailability,
) -> Result<PreferenceApplication, OptionsError> {
    let catalog = match MinecraftPreferenceCatalog::bundled_shared() {
        Ok(catalog) => catalog,
        Err(error) => {
            // A broken bundled catalog is a broken build, not something the
            // user did. Report it and create the instance anyway rather than
            // failing a creation over a settings file.
            tracing::error!(%error, "the bundled Minecraft preference catalog is unusable");
            return Ok(PreferenceApplication {
                path: game_dir.join(OPTIONS_FILE_NAME),
                minecraft_version: minecraft_version.to_owned(),
                schema: compatibility
                    .schema_id(minecraft_version)
                    .map(str::to_owned),
                source: compatibility.source(),
                created_file: false,
                applied: Vec::new(),
                unsupported: Vec::new(),
                unknown: Vec::new(),
                unrecognized: Vec::new(),
                skipped: Vec::new(),
            });
        }
    };
    let loaded = preferences.to_configured(catalog);
    apply_managed_preferences(
        game_dir,
        minecraft_version,
        &loaded.preferences,
        catalog,
        compatibility,
        language,
    )
}

/// Encodes every component of one preference, or none of them.
///
/// All or nothing on purpose: a window size whose width was writable and whose
/// height was not would configure half a window.
fn encode_fields(
    value: &MinecraftPreferenceValue,
    fields: &[ResolvedField<'_>],
) -> Result<Vec<(String, String)>, SkipReason> {
    let mut encoded = Vec::with_capacity(fields.len());
    for field in fields {
        let codec = &field.representation.codec;
        let setting = setting_value_for(value, field.component, codec)?;
        let text = codec.encode(&setting)?;
        encoded.push((field.representation.key.clone(), text));
    }
    Ok(encoded)
}

/// Turns one typed preference value into the release-neutral value one
/// options key holds.
///
/// The codec is consulted rather than guessed at for the two shapes whose
/// storage form is genuinely a per-release fact: an enumeration is persisted
/// as an id in some releases and a token in others, and a key binding is a
/// numeric scan code before 1.13 and a name afterwards. Everything else has
/// exactly one release-neutral form.
fn setting_value_for(
    value: &MinecraftPreferenceValue,
    component: PreferenceComponent,
    codec: &OptionCodec,
) -> Result<SettingValue, SkipReason> {
    match value {
        MinecraftPreferenceValue::Boolean(on) => Ok(SettingValue::Boolean(*on)),
        MinecraftPreferenceValue::Integer(number) => Ok(SettingValue::Integer(*number)),
        MinecraftPreferenceValue::Decimal(number) => Ok(SettingValue::Decimal(number.get())),
        MinecraftPreferenceValue::Text(text) | MinecraftPreferenceValue::LanguageCode(text) => {
            Ok(SettingValue::Text(text.clone()))
        }
        MinecraftPreferenceValue::Enum(technical) => match codec {
            OptionCodec::EnumId { .. } => technical
                .parse()
                .map(SettingValue::Integer)
                .map_err(|_| SkipReason::NotInDomain),
            OptionCodec::EnumToken { .. } => Ok(SettingValue::Text(technical.clone())),
            _ => Err(SkipReason::TypeMismatch),
        },
        MinecraftPreferenceValue::KeyBinding(binding) => Ok(SettingValue::KeyBinding(*binding)),
        MinecraftPreferenceValue::WindowSize(size) => match component {
            PreferenceComponent::Width => Ok(SettingValue::Integer(i64::from(size.width))),
            PreferenceComponent::Height => Ok(SettingValue::Integer(i64::from(size.height))),
            PreferenceComponent::Value => Err(SkipReason::TypeMismatch),
        },
    }
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
    use crate::preferences::{GuiScalePreference, Managed, PreferenceKey, WindowSize};

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
            vec![PreferenceKey::Fov.id(), PreferenceKey::GuiScale.id()],
            "a report is ordered by stable id, never by a settings screen's order"
        );

        let legacy_text = std::fs::read_to_string(old.join(OPTIONS_FILE_NAME)).expect("read");
        let modern_text = std::fs::read_to_string(new.join(OPTIONS_FILE_NAME)).expect("read");
        assert!(legacy_text.contains("guiScale:3"));
        assert!(modern_text.contains("guiScale:3"));
        for (version, text) in [("1.12.2", &legacy_text), ("1.20.1", &modern_text)] {
            assert!(
                text.contains("fov:0.5"),
                "{version} stores (degrees - 70) / 40, got {text:?}"
            );
        }
        // A file this build creates says which release wrote it. Without it
        // the game reads the file as predating every fix it has ever shipped
        // and throws the settings away.
        assert!(
            modern_text.contains("version:3465"),
            "1.20.1 marks its own file, got {modern_text:?}"
        );
        // 1.12.2's client jar carries no data version to read, so its file is
        // left unmarked rather than marked with a number from a release that
        // is not this one. That is the same file this build wrote before, and
        // no worse than it.
        assert!(
            !legacy_text.contains("version:"),
            "an unknown data version is not invented, got {legacy_text:?}"
        );
    }

    #[test]
    fn a_data_version_is_read_for_the_release_being_written_and_no_other() {
        // The number is the release's own. Reading one release's marker for
        // another is how a file gets the wrong historical fixes applied to it.
        let compatibility = OptionsCompatibility::bundled();
        assert_eq!(compatibility.data_version("1.20.1"), Some(3465));
        assert_eq!(compatibility.data_version("1.12.2"), None);
        assert_eq!(compatibility.data_version("not-a-release"), None);

        let plan = plan_managed_preferences(
            "1.20.1",
            &ConfiguredPreferences::default(),
            MinecraftPreferenceCatalog::bundled_shared().expect("the bundled catalog"),
            &compatibility,
            LanguageAvailability::Verified,
        );
        assert_eq!(plan.data_version, Some(3465));
    }

    #[test]
    fn an_existing_file_keeps_everything_the_preferences_do_not_name() {
        let base = tempfile::tempdir().expect("temp dir");
        let game = base.path().to_path_buf();
        let original = "version:3465\nguiScale:0\nrenderDistance:32\nmodSetting:{\"a\":1}\n";
        std::fs::write(game.join(OPTIONS_FILE_NAME), original).expect("write");

        let report = apply(&game, "1.20.1", &gui_scale_only());
        assert!(!report.created_file);
        assert_eq!(report.applied, vec![PreferenceKey::GuiScale.id()]);

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
        assert_eq!(
            report.unsupported,
            vec![PreferenceKey::SimulationDistance.id()]
        );
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
        assert_eq!(report.unknown, vec![PreferenceKey::GuiScale.id()]);
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
                    id: PreferenceKey::Language.id(),
                    reason: expected
                }]
            );
            assert!(!game.join(OPTIONS_FILE_NAME).exists());
        }

        let game = base.path().join("verified");
        std::fs::create_dir_all(&game).expect("mkdir");
        let report = apply(&game, "1.12.2", &preferences);
        assert_eq!(report.applied, vec![PreferenceKey::Language.id()]);
        assert!(
            std::fs::read_to_string(game.join(OPTIONS_FILE_NAME))
                .expect("read")
                .contains("lang:de_de")
        );
    }

    #[test]
    fn the_default_simulation_distance_is_representable_in_26_2() {
        let base = tempfile::tempdir().expect("temp dir");
        let game = base.path().to_path_buf();
        let preferences = GlobalMinecraftPreferences {
            simulation_distance: Managed::apply(12),
            ..GlobalMinecraftPreferences::default()
        };

        let report = apply(&game, "26.2", &preferences);
        assert_eq!(report.applied, vec![PreferenceKey::SimulationDistance.id()]);
        assert!(report.skipped.is_empty());
        assert!(
            std::fs::read_to_string(game.join(OPTIONS_FILE_NAME))
                .expect("options")
                .contains("simulationDistance:12")
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
        assert_eq!(report.applied, vec![PreferenceKey::WindowSize.id()]);
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
