//! Instance management and global Minecraft preferences.
//!
//! # What this layer adds over the crates below it
//!
//! `ephervex-storage` knows how to create, copy, and delete an instance
//! directory. `ephervex-options` knows how to turn a normalized preference into
//! a line of one release's `options.txt`. Neither knows *when* the second thing
//! happens to the first. The required sequencing is:
//!
//! > Global preferences are applied **once** when a client instance is
//! > created/imported. They are not live-synchronized afterward. After
//! > creation, the instance owns its `options.txt`.
//!
//! So [`App::create_client`] applies them and nothing else does. There is
//! deliberately no service anywhere in this crate that pushes the global
//! preferences into an instance that already exists — not because it would be
//! hard, but because a launcher that reverts a setting the user changed inside
//! the game is a launcher fighting its user.
//!
//! Duplication is the other side of the same rule: a copy keeps the options
//! file its original had, so [`App::duplicate_client`] does not re-apply
//! preferences either.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ephervex_download::OperationReporter;
use ephervex_minecraft::LanguageCatalog;
use ephervex_model::{InstanceId, ServerId};
use ephervex_options::{
    GlobalMinecraftPreferences, LanguageAvailability, LoadedPreferences, OptionsCompatibility,
    PreferenceApplication, PreferenceKey, PreferenceValue,
};
use ephervex_storage::{
    ClientFolder, ClientRecord, Config, DuplicateClient, DuplicatedClient, ServerFolder,
};
use tokio_util::sync::CancellationToken;
use tracing::{instrument, warn};

use crate::app::App;
use crate::error::CoreResult;

/// Reads the global Minecraft preferences out of the configuration file's
/// shape.
///
/// The bridge `ephervex-options` deliberately does not have, for the same
/// reason [`crate::global_java_settings`] exists: the options crate must not
/// depend on the storage crate, and the configuration file's schema is not its
/// business.
#[must_use]
pub fn global_minecraft_preferences(config: &Config) -> LoadedPreferences {
    GlobalMinecraftPreferences::from_settings(
        config
            .minecraft_preferences
            .iter()
            .map(|(name, setting)| (name.as_str(), setting.enabled, setting.value.as_str())),
    )
}

/// Everything preference application needs that does not come from the
/// instance itself.
///
/// Resolved once, after target validation and before anything is published.
/// Only existing caches are consulted, so application remains an offline step.
#[derive(Debug, Clone)]
pub struct PreferenceContext {
    /// How the target release stores each setting.
    pub compatibility: Arc<OptionsCompatibility>,
    /// Whether the target release is known to offer the configured language.
    pub language: LanguageAvailability,
}

impl PreferenceContext {
    /// The bundled baseline with nothing verified about languages.
    ///
    /// What a caller gets with no network and no cache, and the honest starting
    /// point: the compatibility this build shipped with, and no claim at all
    /// about which languages a release offers.
    #[must_use]
    pub fn offline() -> Self {
        Self {
            compatibility: Arc::new(OptionsCompatibility::bundled()),
            language: LanguageAvailability::Unverified,
        }
    }
}

/// A newly created client instance, and what was applied to it.
#[derive(Debug)]
pub struct CreatedClient {
    pub record: ClientRecord,
    /// What the one-time global preference application did, or
    /// `None` if it failed — which is reported but never fatal: an instance
    /// that exists with unapplied preferences is a far better outcome than a
    /// creation that failed over a GUI scale.
    pub preferences: Option<PreferenceApplication>,
}

impl App {
    // -- global Minecraft preferences --------------------------------------

    /// The configured global preferences, with any unusable value reported.
    #[must_use]
    pub fn minecraft_preferences(&self) -> LoadedPreferences {
        global_minecraft_preferences(self.config())
    }

    /// Sets one global preference's value and starts managing it.
    ///
    /// Validates before writing, so a mistyped value is refused at the moment
    /// the user typed it rather than silently skipped every time an instance is
    /// created afterwards.
    #[instrument(skip(self))]
    pub fn set_minecraft_preference(
        &mut self,
        key: PreferenceKey,
        value: &str,
    ) -> CoreResult<PreferenceValue> {
        let parsed = PreferenceValue::parse(key, value)?;
        let stored = parsed.to_setting_value();
        self.mutate_config(|config| {
            config.minecraft_preferences.insert(
                key.as_str().to_owned(),
                ephervex_storage::PreferenceSetting {
                    enabled: true,
                    value: stored,
                },
            );
        })?;
        Ok(parsed)
    }

    /// Stops applying one global preference to new instances.
    ///
    /// Keeps the value: turning management back on should restore what the
    /// user last chose rather than a default.
    #[instrument(skip(self))]
    pub fn unmanage_minecraft_preference(&mut self, key: PreferenceKey) -> CoreResult<()> {
        self.mutate_config(|config| {
            if let Some(setting) = config.minecraft_preferences.get_mut(key.as_str()) {
                setting.enabled = false;
            }
        })
    }

    /// Removes one global preference override and restores inherited behavior.
    ///
    /// This differs from [`Self::unmanage_minecraft_preference`], which keeps a
    /// remembered value for command-line workflows that may enable it again.
    /// A desktop reset removes the stored value so the typed default is shown
    /// and future instances leave the corresponding `options.txt` key alone.
    #[instrument(skip(self))]
    pub fn reset_minecraft_preference(&mut self, key: PreferenceKey) -> CoreResult<()> {
        self.mutate_config(|config| {
            config.minecraft_preferences.remove(key.as_str());
        })
    }

    // -- generated options compatibility ----------------------------------

    /// The options compatibility knowledge currently in effect.
    ///
    /// Reads whatever has been loaded, and falls back to the baseline compiled
    /// into this build. Never touches the network, so a settings screen can ask
    /// it freely.
    #[must_use]
    pub fn minecraft_options(&self) -> Arc<OptionsCompatibility> {
        if let Ok(loaded) = self.options_slot().read()
            && let Some(compatibility) = loaded.as_ref()
        {
            return Arc::clone(compatibility);
        }
        let compatibility = Arc::new(OptionsCompatibility::bundled());
        if let Ok(mut slot) = self.options_slot().write() {
            slot.get_or_insert_with(|| Arc::clone(&compatibility));
        }
        compatibility
    }

    /// The compatibility knowledge including anything already cached, without
    /// touching the network.
    ///
    /// Loads the cached feed document once and keeps it for the process, so a
    /// profile that has refreshed before stays on the newer data offline.
    pub async fn cached_minecraft_options(&self) -> Arc<OptionsCompatibility> {
        if let Ok(loaded) = self.options_slot().read()
            && let Some(compatibility) = loaded.as_ref()
        {
            return Arc::clone(compatibility);
        }
        let cached = match self.options_feed() {
            Ok(feed) => feed.cached().await,
            Err(error) => {
                warn!(%error, "could not read the cached Minecraft options data");
                None
            }
        };
        let compatibility = Arc::new(OptionsCompatibility::resolve(cached));
        if let Ok(mut slot) = self.options_slot().write() {
            *slot = Some(Arc::clone(&compatibility));
        }
        compatibility
    }

    /// Refreshes the published options compatibility data, best effort.
    ///
    /// A refresh failure is logged and nothing else: the launcher carries on
    /// with the cached document or the bundled baseline, which is the whole
    /// reason a baseline is compiled in. A document that does not fully
    /// validate never replaces the last known-good cache.
    #[instrument(skip(self))]
    pub async fn refresh_minecraft_options(&self) -> Arc<OptionsCompatibility> {
        let feed = match self.options_feed() {
            Ok(feed) => feed,
            Err(error) => {
                warn!(%error, "could not build the Minecraft options feed client");
                return self.minecraft_options();
            }
        };

        if !feed.is_fresh().await {
            let reporter = OperationReporter::silent("minecraft-options-feed");
            if let Err(error) = feed.refresh(&reporter, &CancellationToken::new()).await {
                warn!(
                    %error,
                    "could not refresh the published Minecraft options data; using what is already available"
                );
            }
        }

        let compatibility = Arc::new(OptionsCompatibility::resolve(feed.cached().await));
        if let Ok(mut slot) = self.options_slot().write() {
            *slot = Some(Arc::clone(&compatibility));
        }
        compatibility
    }

    /// One release's language catalog, from Mojang's own metadata.
    ///
    /// Resolves and caches it when the network allows, and falls back to
    /// whatever was cached before. `None` means nothing is known about that
    /// release's languages, which is a usable answer: it simply leaves the
    /// managed language preference unapplied.
    #[instrument(skip(self))]
    pub async fn minecraft_language_catalog(
        &self,
        minecraft_version: &str,
    ) -> Option<LanguageCatalog> {
        let catalogs = self
            .languages()
            .inspect_err(|error| warn!(%error, "could not build the language catalog reader"))
            .ok()?;
        let reporter = OperationReporter::silent("minecraft-language-catalog");
        match catalogs
            .resolve(minecraft_version, &reporter, &CancellationToken::new())
            .await
        {
            Ok(catalog) => Some(catalog),
            Err(error) => {
                warn!(
                    version = minecraft_version,
                    %error,
                    "could not resolve the language catalog; falling back to whatever is cached"
                );
                catalogs.cached(minecraft_version).await
            }
        }
    }

    /// Resolves everything preference application needs for one release.
    ///
    /// Uses the compatibility data that is already loaded or cached rather than
    /// refreshing the feed: creating an instance should not wait on a metadata
    /// host, and [`Self::minecraft_releases`] already refreshes it at the point
    /// a user is choosing a version. Every step is best effort, and the worst
    /// outcome is the bundled baseline plus an unverified language, which
    /// applies fewer preferences and writes nothing it cannot vouch for.
    pub async fn preference_context(&self, minecraft_version: &str) -> PreferenceContext {
        let compatibility = self.cached_minecraft_options().await;
        let language = match self.managed_language_code() {
            // Nothing to verify: the language is not managed, so no catalog is
            // read for it either. A catalog failure therefore cannot reach any
            // other preference.
            None => LanguageAvailability::Unverified,
            Some(code) => match self
                .cached_minecraft_language_catalog(minecraft_version)
                .await
            {
                Some(catalog) if catalog.contains(&code) => LanguageAvailability::Verified,
                Some(_) => LanguageAvailability::Absent,
                None => LanguageAvailability::Unverified,
            },
        };
        PreferenceContext {
            compatibility,
            language,
        }
    }

    /// The managed language code, whichever way it was configured.
    ///
    /// The one preference whose validity is not a property of `options.txt`:
    /// which codes a release accepts comes from that release's own catalog, so
    /// it has to be resolved before anything is written and it must not block
    /// anything else when it cannot be.
    fn managed_language_code(&self) -> Option<String> {
        let catalog = self.minecraft_preference_catalog().ok()?;
        let id = PreferenceKey::Language.id();
        let loaded =
            crate::minecraft_preferences::configured_minecraft_preferences(self.config(), catalog);
        match loaded.preferences.get(&id)?.applied_value()? {
            ephervex_options::MinecraftPreferenceValue::LanguageCode(code) => Some(code.clone()),
            _ => None,
        }
    }

    /// Cached language knowledge for creation/import preference application.
    /// This deliberately never calls `resolve`, because that method may fetch
    /// Mojang metadata and instance publication must not wait for it.
    async fn cached_minecraft_language_catalog(
        &self,
        minecraft_version: &str,
    ) -> Option<LanguageCatalog> {
        let catalogs = self
            .languages()
            .inspect_err(|error| warn!(%error, "could not build the language catalog reader"))
            .ok()?;
        catalogs.cached(minecraft_version).await
    }

    /// Applies the global preferences to one instance's `options.txt`.
    ///
    /// Called once, from [`Self::create_client`]. Public because Modrinth pack import's
    /// importer needs to call it at a *different* point in the same creation —
    /// after the pack's own overrides have been laid down, preserving the required
    /// ordering — and not because an instance's preferences are ever refreshed.
    #[instrument(skip(self, record, context), fields(instance = %record.id))]
    pub fn apply_global_preferences(
        &self,
        record: &ClientRecord,
        context: &PreferenceContext,
    ) -> CoreResult<PreferenceApplication> {
        let catalog = self.minecraft_preference_catalog()?;
        let loaded =
            crate::minecraft_preferences::configured_minecraft_preferences(self.config(), catalog);
        for rejected in &loaded.rejected {
            warn!(%rejected, "a configured Minecraft preference was not applied");
        }
        if loaded.unrecognized > 0 {
            // Written by a newer Ephervex, or retired by this one. Left in the
            // configuration file exactly as it was; there is simply nothing
            // this build could apply it to.
            warn!(
                count = loaded.unrecognized,
                "ignoring configured Minecraft preferences this build has no descriptor for"
            );
        }
        Ok(ephervex_options::apply_managed_preferences(
            &ephervex_storage::metadata::client_game_dir(&record.root),
            &record.minecraft_version,
            &loaded.preferences,
            catalog,
            &context.compatibility,
            context.language,
        )?)
    }

    // -- duplication -------------------------------------------------------

    /// Duplicates a client instance.
    ///
    /// Global preferences are deliberately **not** re-applied: the copy keeps
    /// the options file its original had, so duplicating an instance whose
    /// settings were tuned by hand does not quietly reset them.
    #[instrument(skip(self, request))]
    pub fn duplicate_client(
        &self,
        source: InstanceId,
        request: &DuplicateClient,
    ) -> CoreResult<DuplicatedClient> {
        Ok(self
            .registry_for_services()
            .duplicate_client(source, request)?)
    }

    // -- open folder actions  ---------------------------------

    /// Where one of an instance's folders is, or `None` when the action is
    /// unavailable because the folder does not exist and creating it would
    /// misrepresent the instance.
    pub fn client_folder(
        &self,
        instance: InstanceId,
        folder: ClientFolder,
    ) -> CoreResult<Option<PathBuf>> {
        let record = self.client(instance)?;
        Ok(ephervex_storage::folders::resolve(
            folder.path(&record.root),
            folder.when_missing(),
        )?)
    }

    /// The folders of one instance that can currently be opened.
    ///
    /// Resolving them all is what lets a frontend gray out the unavailable
    /// entries instead of offering an action that then fails.
    pub fn client_folders(
        &self,
        instance: InstanceId,
    ) -> CoreResult<Vec<(ClientFolder, Option<PathBuf>)>> {
        let record = self.client(instance)?;
        ClientFolder::ALL
            .iter()
            .map(|folder| {
                let resolved = ephervex_storage::folders::resolve(
                    folder.path(&record.root),
                    folder.when_missing(),
                )?;
                Ok((*folder, resolved))
            })
            .collect()
    }

    /// Hands one of an instance's folders to the desktop's file manager.
    ///
    /// Returns the path that was opened, or `None` when the action is
    /// unavailable.
    #[instrument(skip(self))]
    pub fn open_client_folder(
        &self,
        instance: InstanceId,
        folder: ClientFolder,
    ) -> CoreResult<Option<PathBuf>> {
        let Some(path) = self.client_folder(instance, folder)? else {
            return Ok(None);
        };
        open_folder(&path);
        Ok(Some(path))
    }

    /// Where one of a server's folders is (the project's server half).
    pub fn server_folder(
        &self,
        server: ServerId,
        folder: ServerFolder,
    ) -> CoreResult<Option<PathBuf>> {
        let record = self.server(server)?;
        Ok(ephervex_storage::folders::resolve(
            folder.path(&record.root),
            folder.when_missing(),
        )?)
    }

    /// Hands one of a server's folders to the desktop's file manager.
    #[instrument(skip(self))]
    pub fn open_server_folder(
        &self,
        server: ServerId,
        folder: ServerFolder,
    ) -> CoreResult<Option<PathBuf>> {
        let Some(path) = self.server_folder(server, folder)? else {
            return Ok(None);
        };
        open_folder(&path);
        Ok(Some(path))
    }
}

/// Hands a path to the desktop, warning rather than failing if it declines.
///
/// A launcher on a headless machine still knows where the folder is, and the
/// caller has the path — refusing the whole operation because no file manager
/// answered would be worse than saying so in the log.
pub(crate) fn open_folder(path: &Path) {
    if let Err(error) = open::that_detached(path) {
        warn!(
            path = %path.display(),
            %error,
            "could not hand the folder to a file manager"
        );
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use ephervex_model::LoaderSelection;
    use ephervex_options::OPTIONS_FILE_NAME;
    use ephervex_storage::{NewClient, Paths};

    use super::*;

    fn app_rooted_at(base: &Path) -> App {
        App::open_at(Paths::rooted_at(base)).expect("app opens")
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
    fn one_normalized_preference_reaches_two_minecraft_versions_correctly() {
        // The instance and preference management required behavior, end to end through the services
        // layer: two instances, two releases, one preference, and no
        // options.txt copied between them.
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        app.set_minecraft_preference(PreferenceKey::GuiScale, "3")
            .expect("set");

        let old = create(&app, "Legacy", "1.12.2");
        let new = create(&app, "Modern", "1.20.1");

        let old_options = options_of(&old).expect("an options file");
        let new_options = options_of(&new).expect("an options file");
        assert!(old_options.contains("guiScale:3"), "{old_options}");
        assert!(new_options.contains("guiScale:3"), "{new_options}");

        // Neither instance inherited the other's file: a copied one would
        // carry the other release's data-version marker.
        assert!(!old_options.contains("version:"), "{old_options}");
        assert!(!new_options.contains("version:"), "{new_options}");
        assert_eq!(old_options, new_options, "only the managed key was written");
    }

    #[test]
    fn editing_one_instance_afterwards_changes_neither_the_settings_nor_another_instance() {
        // The second half of the required behavior: after creation the
        // instance owns its file.
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        app.set_minecraft_preference(PreferenceKey::GuiScale, "2")
            .expect("set");

        let first = create(&app, "First", "1.20.1");
        let second = create(&app, "Second", "1.20.1");

        // Stand in for the user changing the setting inside the game.
        let path = ephervex_storage::metadata::client_game_dir(&first.root).join(OPTIONS_FILE_NAME);
        std::fs::write(&path, "guiScale:4\nrenderDistance:32\n").expect("write");

        assert!(options_of(&second).expect("options").contains("guiScale:2"));
        assert_eq!(
            app.minecraft_preferences().preferences.gui_scale.value,
            ephervex_options::GuiScalePreference::Fixed(2),
            "an instance edit must not reach back into the global settings"
        );

        //...and a third instance created afterwards still gets the global
        // value, not the one the user set inside the first instance.
        let third = create(&app, "Third", "1.20.1");
        assert!(options_of(&third).expect("options").contains("guiScale:2"));
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "guiScale:4\nrenderDistance:32\n",
            "nothing re-applies preferences to an instance that already exists"
        );
    }

    #[test]
    fn nothing_managed_means_a_new_instance_gets_no_options_file_at_all() {
        // A launcher-written options.txt containing nothing is worse than no
        // file: the game writes a complete one on first run.
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let mut config = app.config().clone();
        config.minecraft_preferences.clear();
        config.save(app.paths()).expect("save");

        let app = app_rooted_at(base.path());
        let record = create(&app, "Untouched", "1.20.1");
        assert!(options_of(&record).is_none());
    }

    #[test]
    fn an_unusable_configured_value_is_reported_and_creation_still_succeeds() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let mut config = app.config().clone();
        config.minecraft_preferences.insert(
            "fov".to_owned(),
            ephervex_storage::PreferenceSetting::managed("very wide"),
        );
        config.minecraft_preferences.insert(
            "render_distance".to_owned(),
            ephervex_storage::PreferenceSetting::managed("16"),
        );
        config.save(app.paths()).expect("save");

        let app = app_rooted_at(base.path());
        assert_eq!(app.minecraft_preferences().rejected.len(), 1);

        let record = create(&app, "Still works", "1.20.1");
        let options = options_of(&record).expect("the valid preference applied");
        assert!(!options.contains("fov"));
        assert!(options.contains("renderDistance:16"));
    }

    #[test]
    fn a_preference_is_validated_before_it_is_stored() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());

        let error = app
            .set_minecraft_preference(PreferenceKey::GuiScale, "enormous")
            .expect_err("must be refused");
        assert_eq!(error.message_id(), "error-options-preference-value-invalid");
        assert!(
            !app.config().minecraft_preferences.contains_key("gui_scale"),
            "a refused value must not reach the configuration file"
        );
    }

    #[tokio::test]
    async fn creation_preferences_use_no_language_network_when_the_catalog_is_missing() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        app.set_minecraft_preference(PreferenceKey::Language, "de_de")
            .expect("set language");

        let context = app.preference_context("26.2").await;
        assert_eq!(context.language, LanguageAvailability::Unverified);
    }

    #[test]
    fn unmanaging_a_preference_keeps_its_value_and_stops_applying_it() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        app.set_minecraft_preference(PreferenceKey::RenderDistance, "16")
            .expect("set");
        app.unmanage_minecraft_preference(PreferenceKey::RenderDistance)
            .expect("unmanage");

        let preferences = app.minecraft_preferences().preferences;
        assert!(!preferences.render_distance.enabled);
        assert_eq!(preferences.render_distance.value, 16);

        let record = create(&app, "Untouched", "1.20.1");
        assert!(
            !options_of(&record)
                .unwrap_or_default()
                .contains("renderDistance"),
            "an unmanaged preference is not written"
        );
    }

    #[test]
    fn resetting_a_preference_removes_the_override_and_restores_the_typed_default() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        app.set_minecraft_preference(PreferenceKey::RenderDistance, "16")
            .expect("set");
        app.reset_minecraft_preference(PreferenceKey::RenderDistance)
            .expect("reset");

        assert!(
            !app.config()
                .minecraft_preferences
                .contains_key("render_distance")
        );
        let preferences = app.minecraft_preferences().preferences;
        assert!(!preferences.render_distance.enabled);
        assert_eq!(preferences.render_distance.value, 12);

        let record = create(&app, "Inherited", "1.20.1");
        assert!(
            !options_of(&record)
                .unwrap_or_default()
                .contains("renderDistance"),
            "reset must not write a default value into options.txt"
        );
    }

    #[test]
    fn a_duplicate_keeps_the_options_file_rather_than_having_preferences_re_applied() {
        // "do not re-apply global preferences after duplication".
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        app.set_minecraft_preference(PreferenceKey::GuiScale, "2")
            .expect("set");
        let original = create(&app, "Original", "1.20.1");

        let path =
            ephervex_storage::metadata::client_game_dir(&original.root).join(OPTIONS_FILE_NAME);
        std::fs::write(&path, "guiScale:4\n").expect("write");

        let copy = app
            .duplicate_client(original.id, &DuplicateClient::default())
            .expect("duplicate")
            .record;

        assert_eq!(
            options_of(&copy).expect("options"),
            "guiScale:4\n",
            "the copy keeps what the original had"
        );
        assert_eq!(copy.name, "Original (copy)");
    }

    #[test]
    fn folder_actions_create_the_conventional_folders_and_disable_the_rest() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let record = create(&app, "Repro", "1.20.1");

        let mods = app
            .client_folder(record.id, ClientFolder::Mods)
            .expect("resolve")
            .expect("mods is created on demand");
        assert!(mods.is_dir());
        assert!(mods.starts_with(&record.root));

        assert_eq!(
            app.client_folder(record.id, ClientFolder::CrashReports)
                .expect("resolve"),
            None,
            "an empty crash-reports folder would misrepresent the instance"
        );

        // Every folder is answerable without a second lookup per entry.
        let folders = app.client_folders(record.id).expect("folders");
        assert_eq!(folders.len(), ClientFolder::ALL.len());
        assert!(
            folders
                .iter()
                .any(|(folder, path)| *folder == ClientFolder::Root && path.is_some())
        );
    }

    #[test]
    fn a_folder_of_an_unknown_instance_reports_the_instance_rather_than_the_path() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let error = app
            .client_folder(InstanceId::new(), ClientFolder::Mods)
            .expect_err("no such instance");
        assert_eq!(error.message_id(), "error-registry-instance-unknown");
    }
}
