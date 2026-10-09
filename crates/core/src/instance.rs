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
//! preferences into an instance that already exists - not because it would be
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
use ephervex_model::{AbsentModPolicy, InstanceId, ServerId};
use ephervex_options::{
    GlobalMinecraftPreferences, LanguageAvailability, LoadedPreferences, OptionsCompatibility,
    PreferenceApplication, PreferenceKey, PreferenceValue,
};
use ephervex_pack::{PackSide, SourceStore};
use ephervex_storage::{
    ClientFolder, ClientRecord, Config, DuplicateClient, DuplicatedClient, ServerCommit,
    ServerFolder, ServerRecord,
};
use tokio_util::sync::CancellationToken;
use tracing::{instrument, warn};

use crate::app::App;
use crate::error::{CoreError, CoreResult};

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
    /// `None` if it failed - which is reported but never fatal: an instance
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
        self.reload_cached_minecraft_options().await
    }

    /// Rereads the cached feed document, replacing what this `App` loaded
    /// earlier.
    ///
    /// For an `App` that must see a refresh another one made since, such as a
    /// worker that refreshes the feed while a long-lived `App` keeps answering
    /// settings previews. Never touches the network.
    pub async fn reload_cached_minecraft_options(&self) -> Arc<OptionsCompatibility> {
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
    /// importer needs to call it at a *different* point in the same creation -
    /// after the pack's own overrides have been laid down, preserving the required
    /// ordering - and not because an instance's preferences are ever refreshed.
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
}

/// Drops one copy's records of mods whose jars did not come with it.
///
/// Best effort: a recipe that cannot be rewritten leaves the copy describing a
/// mod it does not have, which is exactly the state the original was in and is
/// not worth failing a duplication over.
fn forget_absent_mods(root: &Path) {
    let store = SourceStore::new(ephervex_storage::metadata::source_dir(root));
    if !store.has_recipe() {
        return;
    }
    let Ok(mut recipe) = store.read_recipe() else {
        return;
    };
    let game_dir = ephervex_storage::metadata::client_game_dir(root);
    let forgotten = recipe.forget_absent_files(&game_dir, PackSide::Client);
    if forgotten.is_empty() {
        return;
    }
    if let Err(error) = store.write_recipe(&recipe) {
        warn!(%error, "a copy kept the records of mods whose jars are gone");
    }
}

/// The longest name an instance or a server may carry.
///
/// Counted in characters rather than bytes, because the limit exists so a name
/// stays readable in a list and in a bug report, not to fit a buffer. Generous
/// enough that no real name meets it.
pub const MAX_ENTITY_NAME_CHARS: usize = 120;

/// Why a name the user typed was refused.
///
/// Typed rather than a message, so the frontend localizes it and the CLI can
/// print its own wording. Each variant is a thing the user can fix by typing
/// something else, which is what separates these from a storage failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum NameRefusal {
    /// Nothing but whitespace. A blank row in the list names nothing.
    Empty,
    /// Longer than [`MAX_ENTITY_NAME_CHARS`].
    TooLong,
    /// Contains control characters, which do not survive a log line, a
    /// directory listing, or a bug report intact.
    ControlCharacters,
}

impl NameRefusal {
    /// The `ephervex-i18n` message id explaining this to a user.
    #[must_use]
    pub const fn message_id(self) -> &'static str {
        match self {
            Self::Empty => "error-name-empty",
            Self::TooLong => "error-name-too-long",
            Self::ControlCharacters => "error-name-control-characters",
        }
    }
}

impl std::fmt::Display for NameRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Empty => "it is empty",
            Self::TooLong => "it is too long",
            Self::ControlCharacters => "it contains control characters",
        })
    }
}

/// The name to store, or why this one cannot be stored.
///
/// Surrounding whitespace is dropped rather than refused: it is invisible, so
/// refusing it would explain nothing. Everything else is left exactly as typed,
/// including the interior spacing and the case, because a name is the user's
/// words and nothing here knows better.
pub fn validated_entity_name(name: &str) -> CoreResult<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(CoreError::EntityNameInvalid {
            reason: NameRefusal::Empty,
        });
    }
    if trimmed.chars().any(char::is_control) {
        return Err(CoreError::EntityNameInvalid {
            reason: NameRefusal::ControlCharacters,
        });
    }
    if trimmed.chars().count() > MAX_ENTITY_NAME_CHARS {
        return Err(CoreError::EntityNameInvalid {
            reason: NameRefusal::TooLong,
        });
    }
    Ok(trimmed.to_owned())
}

/// An entity name derived from text the user did not type, such as a pack's
/// own name, or `None` when nothing usable is left.
///
/// Unlike [`validated_entity_name`] this repairs rather than refuses: a pack
/// with an awkward name should still import. Control characters become spaces,
/// surrounding whitespace is dropped, and a name over [`MAX_ENTITY_NAME_CHARS`]
/// is cut. The result always passes [`validated_entity_name`].
pub(crate) fn sanitized_entity_name(raw: &str) -> Option<String> {
    let replaced: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let truncated: String = replaced
        .trim()
        .chars()
        .take(MAX_ENTITY_NAME_CHARS)
        .collect();
    let name = truncated.trim_end();
    (!name.is_empty()).then(|| name.to_owned())
}

impl App {
    // -- renaming ----------------------------------------------------------

    /// Renames a client instance.
    ///
    /// Only the record changes. An instance root is named after its
    /// [`InstanceId`], never after the instance, so nothing on disk moves and
    /// no path anybody has already opened stops working.
    ///
    /// Names are not unique and are not made unique here. Two instances of the
    /// same pack at the same version genuinely are the same thing to a user,
    /// and inventing a suffix to tell them apart would name them something
    /// neither was asked to be called.
    #[instrument(skip(self))]
    pub fn rename_client(&self, instance: InstanceId, name: &str) -> CoreResult<ClientRecord> {
        let name = validated_entity_name(name)?;
        let mut record = self.client(instance)?;
        record.name = name;
        self.registry_for_services().update_client(&mut record)?;
        Ok(record)
    }

    /// Renames a dedicated server, on the same terms.
    ///
    /// The rename starts from the server's own metadata file, never from the
    /// index copy, so metadata that is missing, unreadable, or about another
    /// server blocks it instead of being replaced.
    #[instrument(skip(self))]
    pub fn rename_server(
        &self,
        server: ServerId,
        name: &str,
    ) -> CoreResult<ServerCommit<ServerRecord>> {
        let name = validated_entity_name(name)?;
        let mut record = self.canonical_server_for_mutation(server)?;
        let previous = record.clone();
        record.name = name;
        let committed = self
            .registry_for_services()
            .update_server_committed(&mut record)?;
        self.sync_linked_server_list(&committed.record, Some(&previous));
        Ok(committed)
    }
}

impl App {
    // -- duplication -------------------------------------------------------

    /// Duplicates a client instance.
    ///
    /// Global preferences are deliberately **not** re-applied: the copy keeps
    /// the options file its original had, so duplicating an instance whose
    /// settings were tuned by hand does not quietly reset them.
    ///
    /// `absent_mods` decides what the copy does with mods the original records
    /// but no longer has on disk. The caller has to say, because only the user
    /// knows which kind of copy this is: one made to carry on bisecting wants
    /// them gone, one made to get back to a working state wants them.
    ///
    /// Refuses while the original is launching or running: the game keeps
    /// writing its world and configuration, so a copy would mix files from
    /// different moments.
    #[instrument(skip(self, request))]
    pub fn duplicate_client(
        &self,
        source: InstanceId,
        request: &DuplicateClient,
        absent_mods: AbsentModPolicy,
    ) -> CoreResult<DuplicatedClient> {
        self.refuse_while_running(source)?;
        let copy = self
            .registry_for_services()
            .duplicate_client(source, request)?;
        if absent_mods.drops_them() {
            // After the copy and only ever on the copy. The original still
            // wants those mods, and a duplication is not a decision about it.
            forget_absent_mods(&copy.record.root);
        }
        Ok(copy)
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

    /// Resolves retained stdout/stderr capture without creating an empty log folder.
    pub fn server_capture_folder(&self, server: ServerId) -> CoreResult<Option<PathBuf>> {
        self.server(server)?;
        Ok(ephervex_storage::folders::resolve(
            self.paths().server_capture_dir(server),
            ephervex_storage::MissingFolder::Disable,
        )?)
    }

    /// Opens Ephervex's retained stdout/stderr capture for this server.
    /// Minecraft's own log files remain in `ServerFolder::Logs`.
    pub fn open_server_capture_folder(&self, server: ServerId) -> CoreResult<Option<PathBuf>> {
        let path = self.server_capture_folder(server)?;
        if let Some(path) = &path {
            open_folder(path);
        }
        Ok(path)
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
    fn renaming_an_instance_changes_the_record_and_moves_no_directory() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let original = create(&app, "Copy of something", "1.20.1");

        let renamed = app
            .rename_client(original.id, "Chest bug, NeoForge 47")
            .expect("rename");

        assert_eq!(renamed.name, "Chest bug, NeoForge 47");
        assert_eq!(renamed.id, original.id);
        assert_eq!(
            renamed.root, original.root,
            "an instance root is named after its id, so nothing moves"
        );
        assert!(renamed.root.is_dir());
        assert_eq!(
            app.client(original.id).expect("reread").name,
            "Chest bug, NeoForge 47",
            "and the change survives a reread rather than living in one record"
        );
    }

    #[test]
    fn a_renamed_instance_keeps_every_other_thing_about_it() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let original = create(&app, "Repro", "1.20.1");

        let renamed = app
            .rename_client(original.id, "Repro, narrowed")
            .expect("rename");

        assert_eq!(renamed.minecraft_version, original.minecraft_version);
        assert_eq!(renamed.loader, original.loader);
        assert_eq!(renamed.storage, original.storage);
        assert_eq!(renamed.created_at, original.created_at);
    }

    #[test]
    fn surrounding_whitespace_is_dropped_and_the_rest_of_a_name_is_left_alone() {
        // Invisible, so refusing it would explain nothing. Interior spacing and
        // case are the user's words and nothing here knows better.
        assert_eq!(
            validated_entity_name("  Chest   bug  ").expect("valid"),
            "Chest   bug"
        );
    }

    #[test]
    fn a_name_that_is_empty_or_only_whitespace_is_refused() {
        for blank in ["", "   ", "\t\n"] {
            let error = validated_entity_name(blank).expect_err("blank names name nothing");
            assert!(
                matches!(
                    error,
                    CoreError::EntityNameInvalid {
                        reason: NameRefusal::Empty | NameRefusal::ControlCharacters
                    }
                ),
                "{error:?}"
            );
        }
    }

    #[test]
    fn a_name_carrying_a_control_character_is_refused_rather_than_stripped() {
        // Stripping would store something other than what was typed. A line
        // break also does not survive a log line or a directory listing.
        let error = validated_entity_name("Repro\nsecond line").expect_err("refused");
        assert!(
            matches!(
                error,
                CoreError::EntityNameInvalid {
                    reason: NameRefusal::ControlCharacters
                }
            ),
            "{error:?}"
        );
    }

    #[test]
    fn a_sanitized_name_always_passes_the_rules_a_rename_enforces() {
        let long = "w".repeat(MAX_ENTITY_NAME_CHARS * 2);
        for raw in [
            "Evil\u{1b}[2J\nsecond",
            "  padded \t ",
            "\u{85}tail\u{9f}",
            long.as_str(),
            &format!("{} tail", "w".repeat(MAX_ENTITY_NAME_CHARS - 1)),
        ] {
            let name = sanitized_entity_name(raw).expect("something usable remains");
            assert_eq!(
                validated_entity_name(&name).expect("valid"),
                name,
                "{raw:?}"
            );
        }
        assert_eq!(sanitized_entity_name("a\nb").as_deref(), Some("a b"));
        assert_eq!(sanitized_entity_name(" \t\n\u{1b} "), None);
        assert_eq!(sanitized_entity_name(""), None);
    }

    #[test]
    fn the_name_ceiling_counts_characters_rather_than_bytes() {
        // A byte ceiling would refuse a shorter name in one language than in
        // another, which is not a rule anybody could predict.
        let wide = "\u{00e4}".repeat(MAX_ENTITY_NAME_CHARS);
        assert!(
            wide.len() > MAX_ENTITY_NAME_CHARS,
            "two bytes per character"
        );
        assert!(validated_entity_name(&wide).is_ok());

        let error = validated_entity_name(&"a".repeat(MAX_ENTITY_NAME_CHARS + 1))
            .expect_err("one over the ceiling");
        assert!(
            matches!(
                error,
                CoreError::EntityNameInvalid {
                    reason: NameRefusal::TooLong
                }
            ),
            "{error:?}"
        );
    }

    #[test]
    fn a_refused_rename_leaves_the_record_exactly_as_it_was() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let original = create(&app, "Repro", "1.20.1");

        app.rename_client(original.id, "   ")
            .expect_err("a blank name is refused");

        assert_eq!(app.client(original.id).expect("reread").name, "Repro");
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

        // Neither instance inherited the other's file. Each is marked with
        // its own release's data version, so a copied one would carry the
        // wrong marker rather than merely the wrong settings; 1.12.2's client
        // jar never carried one to read.
        assert!(new_options.contains("version:3465"), "{new_options}");
        assert!(!old_options.contains("version:"), "{old_options}");
        assert_ne!(old_options, new_options, "each file is its own release's");
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
            .duplicate_client(
                original.id,
                &DuplicateClient::default(),
                AbsentModPolicy::Keep,
            )
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
    #[test]
    fn server_shortcuts_resolve_the_selected_servers_real_files_and_capture() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let create = |name| {
            app.registry_for_services()
                .create_server(ephervex_storage::NewServer::new(
                    name,
                    "1.20.1",
                    LoaderSelection::vanilla(),
                ))
                .expect("server")
        };
        let first = create("First");
        let second = create("Second");
        for server in [&first, &second] {
            assert_eq!(
                app.server_folder(server.id, ServerFolder::Server)
                    .expect("game"),
                Some(server.root.join("server"))
            );
            assert_eq!(
                app.server_folder(server.id, ServerFolder::Config)
                    .expect("config"),
                Some(server.root.join("server/config"))
            );
            assert_eq!(
                app.server_folder(server.id, ServerFolder::Logs)
                    .expect("logs"),
                None
            );
            assert_eq!(
                app.server_folder(server.id, ServerFolder::CrashReports)
                    .expect("crashes"),
                None
            );
            assert_eq!(app.server_capture_folder(server.id).expect("capture"), None);
        }
        let logs = first.root.join("server/logs");
        let crashes = first.root.join("server/crash-reports");
        let capture = app.paths().server_capture_dir(first.id);
        for path in [&logs, &crashes, &capture] {
            std::fs::create_dir_all(path).expect("folder");
        }
        assert_eq!(
            app.server_folder(first.id, ServerFolder::Logs)
                .expect("logs"),
            Some(logs)
        );
        assert_eq!(
            app.server_folder(first.id, ServerFolder::CrashReports)
                .expect("crashes"),
            Some(crashes)
        );
        assert_eq!(
            app.server_capture_folder(first.id).expect("capture"),
            Some(capture)
        );
        assert_eq!(
            app.server_capture_folder(second.id).expect("other capture"),
            None
        );
        assert!(app.server_capture_folder(ServerId::new()).is_err());
    }
}
