//! The core-facing view of Ephervex's managed Minecraft preferences.
//!
//! # What this layer adds
//!
//! `ephervex-options` owns the product catalog, the compatibility dataset, the
//! typed value model, and the writer. `ephervex-storage` owns the shape of
//! `config.toml`. Neither knows about the other, and neither should: this
//! module is the join, and the only place that turns configuration rows into
//! typed preferences and back.
//!
//! # Global means "defaults for future instances"
//!
//! A managed preference is applied exactly once, when a client instance is
//! created or imported, and after that the instance owns its `options.txt`.
//! Changing a preference here therefore changes what the *next* instance comes
//! up with and never rewrites one that already exists. Duplication keeps the
//! copied instance's own file for the same reason.
//!
//! # Save is a batch, not a row
//!
//! [`App::save_global_minecraft_preferences`] validates every edit before it
//! writes anything, so an invalid batch leaves the configuration exactly as it
//! was rather than half applied. Entries the batch does not name - including
//! ids written by a newer Ephervex that this build has no descriptor for -
//! are carried through untouched.

use std::collections::BTreeMap;

use ephervex_options::{
    CompatibilitySource, ConfiguredPreference, ConfiguredPreferences, LanguageAvailability,
    LoadedConfiguredPreferences, MinecraftPreferenceCatalog, MinecraftPreferenceDescriptor,
    MinecraftPreferenceValue, PreferenceDisposition, PreferenceId, PreferencePlan,
    PreferenceValueShape, ReleaseSupport, SkipReason,
};
use ephervex_storage::{Config, PreferenceSetting};
use tracing::instrument;

use crate::app::App;
use crate::error::{CoreError, CoreResult};

/// Reads the configured Minecraft preferences out of the configuration file's
/// shape.
///
/// The bridge `ephervex-options` deliberately does not have: the options crate
/// must not depend on the storage crate, and the configuration file's schema
/// is not its business.
#[must_use]
pub fn configured_minecraft_preferences(
    config: &Config,
    catalog: &MinecraftPreferenceCatalog,
) -> LoadedConfiguredPreferences {
    ConfiguredPreferences::from_settings(
        catalog,
        config
            .minecraft_preferences
            .iter()
            .map(|(name, setting)| (name.as_str(), setting.enabled, setting.value.as_str())),
    )
}

/// One product preference as a settings screen needs to see it.
///
/// Carries the whole descriptor rather than a flattened copy of parts of it,
/// so a frontend never has to reconstruct what the catalog already states and
/// core stays the single authority on the product surface.
#[derive(Debug, Clone, PartialEq)]
pub struct MinecraftPreferenceEntry {
    pub descriptor: MinecraftPreferenceDescriptor,
    /// Whether Ephervex applies this preference to new instances.
    pub managed: bool,
    /// The configured value, or `None` when Ephervex has no opinion at all
    /// about this preference. Absent is not the same as the catalog default.
    pub value: Option<MinecraftPreferenceValue>,
}

impl MinecraftPreferenceEntry {
    /// The stable product id.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.descriptor.id
    }

    /// The shape a value for this preference must have.
    #[must_use]
    pub fn shape(&self) -> PreferenceValueShape {
        self.descriptor.value_shape()
    }
}

/// Every product preference this build offers, with what is configured for it.
#[derive(Debug, Clone, PartialEq)]
pub struct MinecraftPreferenceSnapshot {
    /// The Minecraft release the bundled product catalog was generated from.
    pub catalog_minecraft_version: String,
    /// One entry per shipped product preference, in catalog order.
    pub entries: Vec<MinecraftPreferenceEntry>,
    /// Configured values this build could not read, and therefore does not
    /// apply. Reported rather than repaired.
    pub unusable: Vec<ephervex_options::PreferenceValueError>,
    /// How many configured ids this build has no descriptor for. They stay in
    /// the configuration file untouched.
    pub unrecognized: usize,
}

/// What one exact Minecraft release could do with preference values a settings
/// screen is still holding.
///
/// The saved-configuration dry run is
/// [`App::validate_global_minecraft_preferences_for`]; this is the same
/// question asked about values nobody has committed yet, so a screen can show
/// an honest per-release status for the row the user is editing right now
/// without writing anything first.
///
/// Support and representability are kept apart because they are different
/// sentences: a release either has the setting or does not, and separately
/// either can or cannot express the exact value in hand.
#[derive(Debug, Clone, PartialEq)]
pub struct MinecraftPreferencePreview {
    /// The release this preview was resolved for.
    pub minecraft_version: String,
    /// Whether the knowledge used was the bundled baseline or the remote feed.
    pub source: CompatibilitySource,
    /// Every shipped product preference's support for this release, keyed by
    /// stable product id. Complete: a preference nobody has configured still
    /// has an answer.
    pub support: BTreeMap<String, ReleaseSupport>,
    /// Why the release could not express a drafted value, for the drafted ids
    /// where that is the case. An id absent from this map either has no
    /// drafted value or has one the release encodes.
    pub unrepresentable: BTreeMap<String, SkipReason>,
}

/// What one edit does to one preference.
///
/// Management state and the preference's own value are two axes, and an edit
/// says something about both or forgets the preference entirely. There is
/// deliberately no edit that turns management off *and* has to invent a value:
/// a preference with nothing stored has nothing to remember, and writing a
/// placeholder would create a configuration row whose next read is "unusable".
#[derive(Debug, Clone, PartialEq)]
pub enum PreferenceChange {
    /// Record this preference's value and whether Ephervex applies it.
    ///
    /// `managed: false` with a value is a real, storable state: the user chose
    /// a value and chose not to apply it yet.
    Set {
        managed: bool,
        value: MinecraftPreferenceValue,
    },
    /// Stop applying a preference that is already configured, keeping the
    /// exact value it has.
    ///
    /// A preference with nothing stored stays unconfigured: there is no value
    /// to remember, and Ephervex having no opinion is already the state this
    /// would be asking for.
    Unmanage,
    /// Forget the preference entirely, so Ephervex has no opinion about it.
    Reset,
}

/// One preference to change in a batch.
#[derive(Debug, Clone, PartialEq)]
pub struct PreferenceEdit {
    pub id: PreferenceId,
    pub change: PreferenceChange,
}

impl PreferenceEdit {
    /// An edit that applies one preference at one value.
    #[must_use]
    pub const fn manage(id: PreferenceId, value: MinecraftPreferenceValue) -> Self {
        Self {
            id,
            change: PreferenceChange::Set {
                managed: true,
                value,
            },
        }
    }

    /// An edit that records one preference's value without applying it.
    #[must_use]
    pub const fn remember(id: PreferenceId, value: MinecraftPreferenceValue) -> Self {
        Self {
            id,
            change: PreferenceChange::Set {
                managed: false,
                value,
            },
        }
    }

    /// An edit that stops applying a preference that already has a value,
    /// keeping that value.
    #[must_use]
    pub const fn unmanage(id: PreferenceId) -> Self {
        Self {
            id,
            change: PreferenceChange::Unmanage,
        }
    }

    /// An edit that forgets one preference entirely.
    #[must_use]
    pub const fn reset(id: PreferenceId) -> Self {
        Self {
            id,
            change: PreferenceChange::Reset,
        }
    }
}

impl App {
    /// The product preferences this build exposes.
    ///
    /// Bundled and application-owned: no remote document can add, remove, or
    /// re-point one. A compatibility update can only change whether an
    /// already-shipped preference is writable for a given release.
    pub fn minecraft_preference_catalog(&self) -> CoreResult<&'static MinecraftPreferenceCatalog> {
        MinecraftPreferenceCatalog::bundled_shared().map_err(|error| {
            CoreError::PreferenceCatalogUnusable {
                detail: error.to_string(),
            }
        })
    }

    /// The configured preferences in the generic engine's typed form.
    pub fn configured_minecraft_preferences(&self) -> CoreResult<LoadedConfiguredPreferences> {
        Ok(configured_minecraft_preferences(
            self.config(),
            self.minecraft_preference_catalog()?,
        ))
    }

    /// Every product preference, with its configured state.
    ///
    /// Everything a settings screen needs about *what exists and what is
    /// chosen*. What one exact release can do with each is a separate
    /// question, answered by [`Self::minecraft_preference_compatibility`],
    /// because it depends on a release the settings screen may not have picked
    /// yet.
    pub fn global_minecraft_preference_snapshot(&self) -> CoreResult<MinecraftPreferenceSnapshot> {
        let catalog = self.minecraft_preference_catalog()?;
        let loaded = configured_minecraft_preferences(self.config(), catalog);
        let entries = catalog
            .descriptors
            .iter()
            .map(|descriptor| {
                let configured = PreferenceId::parse(&descriptor.id)
                    .ok()
                    .and_then(|id| loaded.preferences.get(&id).cloned());
                MinecraftPreferenceEntry {
                    descriptor: descriptor.clone(),
                    managed: configured.as_ref().is_some_and(|entry| entry.managed),
                    value: configured.map(|entry| entry.value),
                }
            })
            .collect();
        Ok(MinecraftPreferenceSnapshot {
            catalog_minecraft_version: catalog.canonical_minecraft_version.clone(),
            entries,
            unusable: loaded.rejected,
            unrecognized: loaded.unrecognized,
        })
    }

    /// What one exact Minecraft release can do with each product preference.
    ///
    /// Three answers, kept apart: proven, provably absent, and not
    /// established. Never inferred from a neighboring release.
    pub fn minecraft_preference_compatibility(
        &self,
        minecraft_version: &str,
    ) -> CoreResult<std::collections::BTreeMap<String, ReleaseSupport>> {
        let catalog = self.minecraft_preference_catalog()?;
        Ok(self
            .minecraft_options()
            .catalog_release_support(catalog, minecraft_version))
    }

    /// What applying the configured preferences to one exact release would do,
    /// without touching any instance.
    ///
    /// The dry run behind creation and behind a settings screen's preview.
    /// A preference that is authoritative for the release but whose configured
    /// value the release cannot express stays distinguishable from one the
    /// release does not have at all.
    pub fn validate_global_minecraft_preferences_for(
        &self,
        minecraft_version: &str,
        language: ephervex_options::LanguageAvailability,
    ) -> CoreResult<PreferencePlan> {
        let catalog = self.minecraft_preference_catalog()?;
        let loaded = configured_minecraft_preferences(self.config(), catalog);
        Ok(ephervex_options::plan_managed_preferences(
            minecraft_version,
            &loaded.preferences,
            catalog,
            &self.minecraft_options(),
            language,
        ))
    }

    /// What one exact release could do with a caller-supplied draft, without
    /// touching the configuration file or any instance.
    ///
    /// Takes the typed values rather than reading the stored ones, which is
    /// the whole point: a settings screen previews what the user has chosen so
    /// far, and that is by definition not what is saved. Nothing here mutates,
    /// and nothing about the draft is remembered afterwards.
    ///
    /// Representability is resolved as though every drafted value were
    /// applied, including the ones the draft currently leaves unmanaged. A
    /// remembered value is a value the user may switch on next, so "this
    /// release cannot express it" is worth saying before they do.
    pub fn preview_minecraft_preference_draft(
        &self,
        minecraft_version: &str,
        draft: &ConfiguredPreferences,
        language: LanguageAvailability,
    ) -> CoreResult<MinecraftPreferencePreview> {
        let catalog = self.minecraft_preference_catalog()?;
        let compatibility = self.minecraft_options();
        let support = compatibility.catalog_release_support(catalog, minecraft_version);

        let mut as_applied = ConfiguredPreferences::new();
        for (id, configured) in draft.iter() {
            as_applied.insert(
                id.clone(),
                ConfiguredPreference::managed(configured.value.clone()),
            );
        }
        let plan = ephervex_options::plan_managed_preferences(
            minecraft_version,
            &as_applied,
            catalog,
            &compatibility,
            language,
        );
        let unrepresentable = plan
            .entries
            .iter()
            .filter_map(|entry| match &entry.disposition {
                PreferenceDisposition::NotRepresentable(reason) => {
                    Some((entry.id.as_str().to_owned(), *reason))
                }
                _ => None,
            })
            .collect();

        Ok(MinecraftPreferencePreview {
            minecraft_version: minecraft_version.to_owned(),
            source: compatibility.source(),
            support,
            unrepresentable,
        })
    }

    /// Applies a batch of preference edits, or none of them.
    ///
    /// Every edit is validated against the product catalog first, so a batch
    /// carrying one unusable value leaves the configuration file untouched
    /// rather than half written. Preferences the batch does not name keep
    /// exactly what they had, and ids this build has no descriptor for are
    /// carried through unread and unrewritten.
    #[instrument(skip(self, edits))]
    pub fn save_global_minecraft_preferences(
        &mut self,
        edits: &[PreferenceEdit],
    ) -> CoreResult<()> {
        let catalog = self.minecraft_preference_catalog()?;
        // Validated before anything is written, then applied in order to the
        // file as it is on disk, so an edit that keeps a stored value keeps the
        // current one and a later edit in the batch sees an earlier one.
        let mut changes: Vec<(String, PreferenceChange)> = Vec::with_capacity(edits.len());
        for edit in edits {
            let Some(descriptor) = catalog.descriptor(edit.id.as_str()) else {
                return Err(CoreError::PreferenceUnrecognized {
                    id: edit.id.to_string(),
                });
            };
            if let PreferenceChange::Set { value, .. } = &edit.change {
                // Validated whether or not it is applied: a value stored
                // now is a value some later instance creation reads.
                descriptor.validate_value(value)?;
            }
            changes.push((edit.id.as_str().to_owned(), edit.change.clone()));
        }

        self.mutate_config(|config| {
            for (id, change) in changes {
                match change {
                    PreferenceChange::Set { managed, value } => {
                        config.minecraft_preferences.insert(
                            id,
                            PreferenceSetting {
                                enabled: managed,
                                value: value.to_stored_text(),
                            },
                        );
                    }
                    // Keeping the stored value is the whole difference between
                    // "stop applying this" and "forget this". With nothing
                    // stored there is nothing to keep.
                    PreferenceChange::Unmanage => {
                        if let Some(setting) = config.minecraft_preferences.get_mut(&id) {
                            setting.enabled = false;
                        }
                    }
                    PreferenceChange::Reset => {
                        config.minecraft_preferences.remove(&id);
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use std::path::Path;

    use ephervex_model::LoaderSelection;
    use ephervex_options::{
        InputBinding, LanguageAvailability, MouseButton, OPTIONS_FILE_NAME, PhysicalKey,
        PreferenceDisposition,
    };
    use ephervex_storage::{ClientRecord, NewClient, Paths};

    use super::*;

    fn app_rooted_at(base: &Path) -> App {
        App::open_at(Paths::rooted_at(base)).expect("app opens")
    }

    fn id(text: &str) -> PreferenceId {
        PreferenceId::parse(text).expect("a well-formed id")
    }

    fn options_of(record: &ClientRecord) -> Option<String> {
        std::fs::read_to_string(
            ephervex_storage::metadata::client_game_dir(&record.root).join(OPTIONS_FILE_NAME),
        )
        .ok()
    }

    fn create(app: &App, name: &str, version: &str) -> ClientRecord {
        app.create_client_for_test(NewClient::new(name, version, LoaderSelection::vanilla()))
            .expect("create")
    }

    #[test]
    fn the_snapshot_offers_every_shipped_preference_and_says_which_are_chosen() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        app.save_global_minecraft_preferences(&[
            PreferenceEdit::manage(id("fullscreen"), MinecraftPreferenceValue::Boolean(false)),
            PreferenceEdit::manage(
                id("cloud_status"),
                MinecraftPreferenceValue::Enum("fast".to_owned()),
            ),
        ])
        .expect("save");

        let snapshot = app
            .global_minecraft_preference_snapshot()
            .expect("snapshot");
        assert_eq!(
            snapshot.entries.len(),
            app.minecraft_preference_catalog()
                .expect("catalog")
                .descriptors
                .len()
        );
        assert!(snapshot.unusable.is_empty());
        assert_eq!(snapshot.unrecognized, 0);

        let fullscreen = snapshot
            .entries
            .iter()
            .find(|entry| entry.id() == "fullscreen")
            .expect("a shipped preference");
        assert!(fullscreen.managed);
        assert_eq!(
            fullscreen.value,
            Some(MinecraftPreferenceValue::Boolean(false)),
            "managing a toggle to false is a choice, not an absence"
        );
        assert_eq!(fullscreen.shape(), PreferenceValueShape::Boolean);

        let untouched = snapshot
            .entries
            .iter()
            .find(|entry| entry.id() == "render_distance")
            .expect("a shipped preference");
        assert!(!untouched.managed);
        assert_eq!(
            untouched.value, None,
            "a preference nobody configured has no value, not a default"
        );
    }

    #[test]
    fn only_configured_preferences_reach_the_configuration_file() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        assert!(app.config().minecraft_preferences.is_empty());

        app.save_global_minecraft_preferences(&[PreferenceEdit::manage(
            id("fov"),
            MinecraftPreferenceValue::decimal(90.0).expect("finite"),
        )])
        .expect("save");

        assert_eq!(
            app.config().minecraft_preferences.len(),
            1,
            "the other shipped preferences have no stored default"
        );
        assert_eq!(app.config().minecraft_preferences["fov"].value, "90");
        assert!(app.config().minecraft_preferences["fov"].enabled);
    }

    #[test]
    fn an_id_written_by_a_newer_build_survives_an_unrelated_save() {
        // An older Ephervex reading a newer one's configuration must leave
        // what it does not understand exactly where it found it.
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        app.mutate_config(|config| {
            config.minecraft_preferences.insert(
                "a_setting_from_the_future".to_owned(),
                PreferenceSetting::managed("17"),
            );
        })
        .expect("seed");

        app.save_global_minecraft_preferences(&[PreferenceEdit::manage(
            id("fullscreen"),
            MinecraftPreferenceValue::Boolean(true),
        )])
        .expect("save");

        let stored = &app.config().minecraft_preferences;
        assert_eq!(stored["a_setting_from_the_future"].value, "17");
        assert!(stored["a_setting_from_the_future"].enabled);

        let snapshot = app
            .global_minecraft_preference_snapshot()
            .expect("snapshot");
        assert_eq!(snapshot.unrecognized, 1);
        assert!(
            !snapshot
                .entries
                .iter()
                .any(|entry| entry.id() == "a_setting_from_the_future"),
            "an id with no descriptor is not a product preference"
        );

        // ...and it reaches no instance either.
        let record = create(&app, "Future", "1.20.1");
        let options = options_of(&record).expect("options");
        assert!(options.contains("fullscreen:true"));
        assert!(!options.contains("a_setting_from_the_future"));
    }

    #[test]
    fn an_invalid_batch_leaves_the_configuration_exactly_as_it_was() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        app.save_global_minecraft_preferences(&[PreferenceEdit::manage(
            id("fullscreen"),
            MinecraftPreferenceValue::Boolean(true),
        )])
        .expect("save");
        let before = app.config().minecraft_preferences.clone();

        let error = app
            .save_global_minecraft_preferences(&[
                PreferenceEdit::manage(id("gui_scale"), MinecraftPreferenceValue::Integer(2)),
                PreferenceEdit::manage(
                    id("fov"),
                    MinecraftPreferenceValue::decimal(500.0).expect("finite"),
                ),
            ])
            .expect_err("the batch carries a value the product refuses");
        assert_eq!(
            error.message_id(),
            "error-options-preference-value-unusable"
        );
        assert_eq!(
            error.message_args(),
            vec![("preference", ephervex_i18n::Arg::from("fov".to_owned()))],
            "the message must name the setting that was refused"
        );
        assert_eq!(
            app.config().minecraft_preferences,
            before,
            "one bad edit must not half-apply the batch"
        );
    }

    #[test]
    fn an_id_this_build_does_not_offer_cannot_be_written_deliberately() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        let error = app
            .save_global_minecraft_preferences(&[PreferenceEdit::manage(
                id("a_setting_from_the_future"),
                MinecraftPreferenceValue::Integer(17),
            )])
            .expect_err("nothing in this build could ever apply it");
        assert_eq!(error.message_id(), "error-options-preference-unrecognized");
        assert!(app.config().minecraft_preferences.is_empty());
    }

    #[test]
    fn unmanaging_a_preference_that_was_never_configured_writes_no_row_at_all() {
        // A row with no value is not a preference: the next read rejects it as
        // unusable, so the launcher would have manufactured a broken setting
        // out of an operation that meant "do not apply this".
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());

        app.save_global_minecraft_preferences(&[PreferenceEdit::unmanage(id("render_distance"))])
            .expect("unmanage");

        assert!(
            app.config().minecraft_preferences.is_empty(),
            "nothing was configured, so nothing is stored"
        );
        let loaded = app.configured_minecraft_preferences().expect("configured");
        assert!(loaded.rejected.is_empty(), "no unusable row was invented");
        assert!(loaded.preferences.is_empty());
    }

    #[test]
    fn no_edit_can_leave_a_stored_preference_without_a_readable_value() {
        // The whole batch surface, run over one never-configured preference
        // and one configured one, must never produce a row the reader refuses.
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        app.save_global_minecraft_preferences(&[PreferenceEdit::manage(
            id("fov"),
            MinecraftPreferenceValue::decimal(90.0).expect("finite"),
        )])
        .expect("seed");

        for edit in [
            PreferenceEdit::unmanage(id("render_distance")),
            PreferenceEdit::reset(id("render_distance")),
            PreferenceEdit::unmanage(id("fov")),
            PreferenceEdit::remember(id("gui_scale"), MinecraftPreferenceValue::Integer(2)),
        ] {
            app.save_global_minecraft_preferences(std::slice::from_ref(&edit))
                .unwrap_or_else(|error| panic!("{edit:?}: {error}"));
            assert!(
                app.config()
                    .minecraft_preferences
                    .values()
                    .all(|setting| !setting.value.is_empty()),
                "{edit:?} stored a row with no value"
            );
            assert!(
                app.configured_minecraft_preferences()
                    .expect("configured")
                    .rejected
                    .is_empty(),
                "{edit:?} stored a value the reader refuses"
            );
        }
    }

    #[test]
    fn a_value_chosen_without_being_applied_is_stored_exactly_and_survives_a_restart() {
        // The two axes a settings screen edits separately: what the value is,
        // and whether Ephervex applies it.
        let base = tempfile::tempdir().expect("temp dir");
        {
            let mut app = app_rooted_at(base.path());
            app.save_global_minecraft_preferences(&[
                PreferenceEdit::remember(
                    id("window_size"),
                    MinecraftPreferenceValue::WindowSize(ephervex_options::WindowSize {
                        width: 2560,
                        height: 1440,
                    }),
                ),
                PreferenceEdit::remember(
                    id("keybind_inventory"),
                    MinecraftPreferenceValue::KeyBinding(InputBinding::Mouse(MouseButton::Button5)),
                ),
            ])
            .expect("save");
        }

        let reopened = app_rooted_at(base.path());
        let loaded = reopened
            .configured_minecraft_preferences()
            .expect("configured");
        assert!(loaded.rejected.is_empty());

        let window = loaded
            .preferences
            .get(&id("window_size"))
            .expect("still configured");
        assert!(!window.managed);
        assert_eq!(
            window.value,
            MinecraftPreferenceValue::WindowSize(ephervex_options::WindowSize {
                width: 2560,
                height: 1440,
            })
        );
        assert_eq!(window.applied_value(), None, "and it is not applied");

        let binding = loaded
            .preferences
            .get(&id("keybind_inventory"))
            .expect("still configured");
        assert_eq!(
            binding.value,
            MinecraftPreferenceValue::KeyBinding(InputBinding::Mouse(MouseButton::Button5)),
            "a mouse button beyond the named three survives storage exactly"
        );
    }

    #[test]
    fn a_disabled_toggle_and_a_toggle_set_to_false_stay_two_separate_facts() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        app.save_global_minecraft_preferences(&[
            PreferenceEdit::manage(id("fullscreen"), MinecraftPreferenceValue::Boolean(false)),
            PreferenceEdit::remember(id("auto_jump"), MinecraftPreferenceValue::Boolean(false)),
        ])
        .expect("save");

        let loaded = app.configured_minecraft_preferences().expect("configured");
        let applied = loaded.preferences.get(&id("fullscreen")).expect("stored");
        let remembered = loaded.preferences.get(&id("auto_jump")).expect("stored");

        assert_eq!(applied.value, remembered.value, "the same value");
        assert_eq!(
            applied.applied_value(),
            Some(&MinecraftPreferenceValue::Boolean(false)),
            "managing a toggle to false writes false"
        );
        assert_eq!(
            remembered.applied_value(),
            None,
            "and not managing it writes nothing at all"
        );

        let record = create(&app, "Toggles", "1.20.1");
        let options = options_of(&record).expect("options");
        assert!(options.contains("fullscreen:false"));
        assert!(!options.contains("autoJump:"), "{options}");
    }

    #[test]
    fn one_batch_can_change_a_value_while_leaving_it_unapplied() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        app.save_global_minecraft_preferences(&[PreferenceEdit::manage(
            id("render_distance"),
            MinecraftPreferenceValue::Integer(16),
        )])
        .expect("save");

        app.save_global_minecraft_preferences(&[PreferenceEdit::remember(
            id("render_distance"),
            MinecraftPreferenceValue::Integer(24),
        )])
        .expect("save");

        let stored = &app.config().minecraft_preferences["render_distance"];
        assert_eq!(stored.value, "24");
        assert!(!stored.enabled);

        let record = create(&app, "Unapplied", "1.20.1");
        assert!(
            options_of(&record).is_none(),
            "an unapplied preference writes no file"
        );
    }

    #[test]
    fn unmanaging_after_setting_in_one_batch_keeps_the_value_the_batch_chose() {
        // Edits fold in order, so the pair a settings draft ends on is what is
        // written, and it is written once.
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        app.save_global_minecraft_preferences(&[
            PreferenceEdit::manage(id("gui_scale"), MinecraftPreferenceValue::Integer(3)),
            PreferenceEdit::unmanage(id("gui_scale")),
        ])
        .expect("save");

        let stored = &app.config().minecraft_preferences["gui_scale"];
        assert_eq!(stored.value, "3");
        assert!(!stored.enabled);
    }

    #[test]
    fn an_id_written_by_a_newer_build_survives_every_kind_of_edit() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        app.mutate_config(|config| {
            config.minecraft_preferences.insert(
                "a_setting_from_the_future".to_owned(),
                PreferenceSetting::managed("17"),
            );
        })
        .expect("seed");

        for edit in [
            PreferenceEdit::manage(
                id("fov"),
                MinecraftPreferenceValue::decimal(90.0).expect("finite"),
            ),
            PreferenceEdit::remember(
                id("fov"),
                MinecraftPreferenceValue::decimal(80.0).expect("finite"),
            ),
            PreferenceEdit::unmanage(id("fov")),
            PreferenceEdit::unmanage(id("render_distance")),
            PreferenceEdit::reset(id("fov")),
        ] {
            app.save_global_minecraft_preferences(std::slice::from_ref(&edit))
                .unwrap_or_else(|error| panic!("{edit:?}: {error}"));
            assert_eq!(
                app.config().minecraft_preferences["a_setting_from_the_future"],
                PreferenceSetting::managed("17"),
                "{edit:?} disturbed an id it has nothing to do with"
            );
        }
    }

    #[test]
    fn unmanaging_keeps_the_value_and_resetting_forgets_the_preference() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        app.save_global_minecraft_preferences(&[PreferenceEdit::manage(
            id("render_distance"),
            MinecraftPreferenceValue::Integer(16),
        )])
        .expect("save");

        app.save_global_minecraft_preferences(&[PreferenceEdit::unmanage(id("render_distance"))])
            .expect("unmanage");
        assert_eq!(
            app.config().minecraft_preferences["render_distance"].value,
            "16"
        );
        assert!(!app.config().minecraft_preferences["render_distance"].enabled);

        app.save_global_minecraft_preferences(&[PreferenceEdit::reset(id("render_distance"))])
            .expect("reset");
        assert!(
            !app.config()
                .minecraft_preferences
                .contains_key("render_distance"),
            "a reset leaves Ephervex with no opinion at all"
        );
    }

    #[test]
    fn a_configured_preference_reads_back_identically_after_a_restart() {
        let base = tempfile::tempdir().expect("temp dir");
        let edits = [
            PreferenceEdit::manage(
                id("keybind_inventory"),
                MinecraftPreferenceValue::KeyBinding(InputBinding::Keyboard(PhysicalKey::F3)),
            ),
            PreferenceEdit::manage(
                id("window_size"),
                MinecraftPreferenceValue::WindowSize(ephervex_options::WindowSize {
                    width: 1920,
                    height: 1080,
                }),
            ),
            PreferenceEdit::manage(id("fullscreen"), MinecraftPreferenceValue::Boolean(false)),
        ];
        let expected = {
            let mut app = app_rooted_at(base.path());
            app.save_global_minecraft_preferences(&edits).expect("save");
            app.configured_minecraft_preferences()
                .expect("configured")
                .preferences
        };

        // A second process, reading only what was written to disk.
        let reopened = app_rooted_at(base.path());
        let loaded = reopened
            .configured_minecraft_preferences()
            .expect("configured");
        assert!(loaded.rejected.is_empty());
        assert_eq!(loaded.preferences, expected);
    }

    #[test]
    fn a_dry_run_says_what_one_exact_release_would_do_without_touching_anything() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        app.save_global_minecraft_preferences(&[
            PreferenceEdit::manage(
                id("simulation_distance"),
                MinecraftPreferenceValue::Integer(10),
            ),
            PreferenceEdit::manage(id("gui_scale"), MinecraftPreferenceValue::Integer(3)),
        ])
        .expect("save");

        let legacy = app
            .validate_global_minecraft_preferences_for("1.12.2", LanguageAvailability::Verified)
            .expect("plan");
        assert_eq!(
            legacy
                .entry(&id("simulation_distance"))
                .expect("configured")
                .disposition,
            PreferenceDisposition::Unsupported
        );
        assert_eq!(legacy.writable(), vec![id("gui_scale")]);

        let modern = app
            .validate_global_minecraft_preferences_for("1.20.1", LanguageAvailability::Verified)
            .expect("plan");
        assert_eq!(
            modern.writable(),
            vec![id("gui_scale"), id("simulation_distance")]
        );

        let future = app
            .validate_global_minecraft_preferences_for("26.99", LanguageAvailability::Verified)
            .expect("plan");
        assert!(future.writable().is_empty());
        assert_eq!(future.schema, None);
    }

    #[test]
    fn changing_a_global_preference_later_never_rewrites_an_existing_instance() {
        // Global preferences are defaults for future instances. An instance
        // that exists owns its own file, and the user edits it in Minecraft.
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        app.save_global_minecraft_preferences(&[PreferenceEdit::manage(
            id("gui_scale"),
            MinecraftPreferenceValue::Integer(2),
        )])
        .expect("save");

        let existing = create(&app, "Existing", "1.20.1");
        let before = options_of(&existing).expect("options");
        assert!(before.contains("guiScale:2"));

        app.save_global_minecraft_preferences(&[PreferenceEdit::manage(
            id("gui_scale"),
            MinecraftPreferenceValue::Integer(4),
        )])
        .expect("save");

        assert_eq!(
            options_of(&existing).expect("options"),
            before,
            "an existing instance is never revisited"
        );
        let later = create(&app, "Later", "1.20.1");
        assert!(options_of(&later).expect("options").contains("guiScale:4"));
    }

    #[test]
    fn duplicating_an_instance_keeps_its_file_rather_than_reapplying_the_defaults() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        app.save_global_minecraft_preferences(&[PreferenceEdit::manage(
            id("gui_scale"),
            MinecraftPreferenceValue::Integer(2),
        )])
        .expect("save");
        let source = create(&app, "Source", "1.20.1");

        // Stand in for the user tuning the instance inside the game.
        let path =
            ephervex_storage::metadata::client_game_dir(&source.root).join(OPTIONS_FILE_NAME);
        std::fs::write(&path, "guiScale:4\nrenderDistance:32\n").expect("write");

        app.save_global_minecraft_preferences(&[PreferenceEdit::manage(
            id("gui_scale"),
            MinecraftPreferenceValue::Integer(1),
        )])
        .expect("save");

        let copy = app
            .duplicate_client(
                source.id,
                &ephervex_storage::DuplicateClient::named("Copy"),
                ephervex_model::AbsentModPolicy::Keep,
            )
            .expect("duplicate");
        assert_eq!(
            options_of(&copy.record).expect("options"),
            "guiScale:4\nrenderDistance:32\n",
            "a duplicate keeps what it copied"
        );
    }

    /// The preview's own fixture: a draft built without touching configuration.
    fn draft(entries: Vec<(&str, bool, MinecraftPreferenceValue)>) -> ConfiguredPreferences {
        let mut draft = ConfiguredPreferences::new();
        for (name, managed, value) in entries {
            draft.insert(
                id(name),
                if managed {
                    ConfiguredPreference::managed(value)
                } else {
                    ConfiguredPreference::remembered(value)
                },
            );
        }
        draft
    }

    #[test]
    fn previewing_a_draft_answers_for_every_shipped_preference_without_storing_anything() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let catalog = app.minecraft_preference_catalog().expect("catalog");

        let preview = app
            .preview_minecraft_preference_draft(
                "1.21.1",
                &draft(vec![(
                    "fullscreen",
                    true,
                    MinecraftPreferenceValue::Boolean(true),
                )]),
                LanguageAvailability::Unverified,
            )
            .expect("preview");

        assert_eq!(preview.minecraft_version, "1.21.1");
        assert_eq!(preview.support.len(), catalog.descriptors.len());
        assert_eq!(
            preview.support.get("fullscreen"),
            Some(&ReleaseSupport::Authoritative)
        );
        assert!(preview.unrepresentable.is_empty());
        assert!(
            app.config().minecraft_preferences.is_empty(),
            "a preview must not configure anything"
        );
    }

    #[test]
    fn a_release_nobody_analyzed_previews_as_unknown_rather_than_unsupported() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());

        let preview = app
            .preview_minecraft_preference_draft(
                "99.99.99",
                &ConfiguredPreferences::new(),
                LanguageAvailability::Unverified,
            )
            .expect("preview");

        assert!(
            preview
                .support
                .values()
                .all(|support| *support == ReleaseSupport::Unknown),
            "an unanalyzed release establishes nothing about any preference"
        );
    }

    #[test]
    fn a_preference_a_release_provably_lacks_previews_as_unsupported() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());

        let preview = app
            .preview_minecraft_preference_draft(
                "1.12.2",
                &ConfiguredPreferences::new(),
                LanguageAvailability::Unverified,
            )
            .expect("preview");

        assert_eq!(
            preview.support.get("simulation_distance"),
            Some(&ReleaseSupport::Unsupported),
            "the setting did not exist that far back"
        );
    }

    #[test]
    fn an_authoritative_preference_whose_draft_value_cannot_encode_is_reported_separately() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());

        let preview = app
            .preview_minecraft_preference_draft(
                "1.12.2",
                &draft(vec![(
                    "keybind_attack",
                    true,
                    MinecraftPreferenceValue::KeyBinding(InputBinding::Mouse(MouseButton::Left)),
                )]),
                LanguageAvailability::Unverified,
            )
            .expect("preview");

        assert_eq!(
            preview.support.get("keybind_attack"),
            Some(&ReleaseSupport::Authoritative),
            "the release has the binding"
        );
        assert_eq!(
            preview.unrepresentable.get("keybind_attack"),
            Some(&SkipReason::InputUnavailable),
            "but nothing proves how that era stores a mouse binding"
        );
    }

    #[test]
    fn a_remembered_draft_value_is_still_checked_for_representability() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());

        let preview = app
            .preview_minecraft_preference_draft(
                "1.12.2",
                &draft(vec![(
                    "keybind_attack",
                    false,
                    MinecraftPreferenceValue::KeyBinding(InputBinding::Unbound),
                )]),
                LanguageAvailability::Unverified,
            )
            .expect("preview");

        assert_eq!(
            preview.unrepresentable.get("keybind_attack"),
            Some(&SkipReason::InputUnavailable),
            "a value the user may switch on next is worth previewing now"
        );
    }

    #[test]
    fn previewing_a_draft_never_reads_or_writes_the_saved_preferences() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        app.save_global_minecraft_preferences(&[PreferenceEdit::manage(
            id("gui_scale"),
            MinecraftPreferenceValue::Integer(3),
        )])
        .expect("save");
        let stored = app.config().minecraft_preferences.clone();

        let preview = app
            .preview_minecraft_preference_draft(
                "1.21.1",
                &draft(vec![(
                    "gui_scale",
                    true,
                    MinecraftPreferenceValue::Integer(2),
                )]),
                LanguageAvailability::Unverified,
            )
            .expect("preview");

        assert!(preview.unrepresentable.is_empty());
        assert_eq!(
            app.config().minecraft_preferences,
            stored,
            "the saved value is untouched by a preview of a different one"
        );
    }

    #[test]
    fn window_size_previews_as_one_product_status_rather_than_two_fields() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());

        let preview = app
            .preview_minecraft_preference_draft(
                "1.21.1",
                &ConfiguredPreferences::new(),
                LanguageAvailability::Unverified,
            )
            .expect("preview");

        assert!(preview.support.contains_key("window_size"));
        assert!(!preview.support.contains_key("overrideWidth"));
        assert!(!preview.support.contains_key("overrideHeight"));
    }
}
