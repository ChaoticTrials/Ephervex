//! The application services both frontends call.
//!
//! This is the application-service seam: the GUI and CLI depend on
//! `ephervex-core` and nothing below it, so provider crates can be reshaped
//! without either frontend noticing. Anything a frontend needs to *do* - as
//! opposed to merely display - belongs here.
//!
//! Keeping it honest matters more than it looks. A frontend that reaches past
//! this layer into a provider crate compiles perfectly well; the boundary is
//! only real for as long as nothing crosses it. The rule is therefore
//! structural: `ephervex-cli` and `ephervex-gui` do not list any provider crate
//! in their manifests, so a shortcut is a manifest change someone has to
//! justify rather than an import someone can add absent-mindedly.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

use crate::instance::{CreatedClient, PreferenceContext, validated_entity_name};
use ephervex_download::{
    CacheLayout, DownloadManager, DownloaderConfig, MetadataCacheError, OperationReporter,
};
use ephervex_java::{
    EffectiveJvmSettings, GlobalJavaSettings, JavaCompatibilityRule, JavaDiscovery, JavaError,
    JavaOrigin, JavaPathChoice, JavaPreference, JavaRuntime, JavaSourceKind, ManagedInstallation,
    ManagedJavaProvider, PinSource, RequiredJava, RuntimeInventory, Selection,
};
use ephervex_loaders::{
    InstalledServer, LoaderError, LoaderRegistry, LoaderVersionListing, ServerInstallRequest,
};
use ephervex_minecraft::{
    LanguageCatalogs, MinecraftError, MinecraftVersionId, ReleaseListing,
    VERSION_MANIFEST_CACHE_KEY, VersionCatalog, VersionMetadata,
};
use ephervex_model::{
    InstanceId, LoaderKind, LoaderVersionMode, OperatorPermissionLevel, ServerId,
};
use ephervex_options::{OptionsCompatibility, OptionsFeed};
use ephervex_storage::{
    ApplicationTextScale, ApplicationTheme, ClientCommit, ClientRecord, Config, FileDisposition,
    JavaConfig, NewClient, Paths, ReconciliationReport, Registry, ServerRecord, Storage,
    TechnicalTextSize,
};
use tokio_util::sync::CancellationToken;
use tracing::{instrument, warn};

use crate::error::{CoreError, CoreResult};

/// What a caller wants a Java plan for, beyond the Minecraft version.
///
/// The default - no instance, no explicit path, no installing - is "what would
/// happen if I launched this right now", which is the question a `--dry-run`
/// and a settings screen both ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum JavaTarget {
    #[default]
    VersionOnly,
    Client(InstanceId),
    Server(ServerId),
}

#[derive(Debug, Clone, Default)]
pub struct JavaPlanRequest {
    /// The typed entity whose loader and overrides apply.
    pub target: JavaTarget,
    /// A path supplied for this operation only. Outranks every stored setting.
    pub explicit_path: Option<PathBuf>,
    /// Whether to download a managed runtime when nothing installed qualifies.
    /// Still subject to the global "Auto-download compatible Java" setting.
    pub install_if_missing: bool,
    /// Ends a managed runtime install this plan triggers. Never fires by default.
    pub cancel: CancellationToken,
    /// Receives the progress of a managed runtime install this plan triggers.
    /// The install is silent when absent.
    pub reporter: Option<OperationReporter>,
}

/// Which runtime would run one Minecraft version, and how that was decided.
///
/// The provenance fields exist because "why did it pick that one" is the
/// question this whole subsystem gets asked: `required.basis` says whether
/// Mojang or the fallback table set the major, `choice` says whether a setting
/// overruled the selection, and `rejected` says what else was on the machine
/// and why it did not win.
#[derive(Debug, Clone)]
pub struct JavaPlan {
    pub minecraft_version: String,
    pub required: RequiredJava,
    pub choice: JavaPathChoice,
    pub runtime: JavaRuntime,
    /// Runtimes that were considered and refused, with the rule that refused
    /// them. Empty when a pinned path short-circuited selection.
    pub rejected: Vec<ephervex_java::RejectedRuntime>,
    /// The memory and argument settings this instance would launch with.
    pub jvm: EffectiveJvmSettings,
    /// Whether the runtime was downloaded during this call.
    pub installed_now: bool,
}

/// How a plan identifies a pinned runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PinProbe {
    /// Run the executable, as a launch must.
    Fresh,
    /// Answer from the cached inventory when it knows the executable.
    ReuseInventory,
}

/// A plan that found no runtime, with the pinned runtime it refused when that
/// one could be identified.
#[derive(Debug)]
pub(crate) struct PlanRefusal {
    pub(crate) error: CoreError,
    pub(crate) attempted: Option<JavaRuntime>,
}

impl From<CoreError> for Box<PlanRefusal> {
    fn from(error: CoreError) -> Self {
        Self::new(PlanRefusal {
            error,
            attempted: None,
        })
    }
}

/// Ephervex interface display preferences exposed to frontends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationDisplaySettings {
    pub text_scale: ApplicationTextScale,
    pub technical_text_size: TechnicalTextSize,
    pub theme: ApplicationTheme,
    pub show_linked_servers: bool,
}

/// Defaults applied to new Ephervex-managed development servers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerApplicationSettings {
    pub make_default_account_operator: bool,
    pub operator_permission_level: OperatorPermissionLevel,
}

/// Configured and resolved locations for persistent instances.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistentStorageSettings {
    pub configured_root: Option<PathBuf>,
    pub effective_root: PathBuf,
    pub default_root: PathBuf,
}

/// Reads the global Java settings out of the configuration file's shape.
///
/// The bridge `ephervex-java` deliberately does not have: the Java crate must
/// not depend on the storage crate, and the configuration file's schema is not
/// its business.
#[must_use]
pub fn global_java_settings(config: &JavaConfig) -> GlobalJavaSettings {
    GlobalJavaSettings {
        auto_detect: config.auto_detect,
        auto_download: config.auto_download,
        min_memory_mib: config.min_memory_mib,
        max_memory_mib: config.max_memory_mib,
        jvm_args: config.jvm_args.clone(),
        preferred_path: config.preferred_path(),
    }
}

/// Probes one executable and reports it as a runtime from a known source.
async fn probe_runtime(
    executable: &Path,
    kind: JavaSourceKind,
) -> Result<JavaRuntime, ephervex_java::JavaError> {
    let report =
        ephervex_java::probe(executable)
            .await
            .map_err(|source| JavaError::RuntimeUnusable {
                path: executable.to_path_buf(),
                source: Box::new(source),
            })?;
    let canonical = tokio::fs::canonicalize(executable)
        .await
        .unwrap_or_else(|_| executable.to_path_buf());
    Ok(JavaRuntime::from_probe(
        canonical,
        report,
        JavaOrigin {
            kind,
            path: executable.to_path_buf(),
        },
    ))
}

/// Which discovery source a pinned path should be recorded as.
const fn pin_source_kind(source: PinSource) -> JavaSourceKind {
    match source {
        PinSource::ExplicitRequest | PinSource::InstanceOverride | PinSource::GlobalPreference => {
            JavaSourceKind::UserSupplied
        }
    }
}

/// Whether a metadata failure is "the provider is unreachable" rather than
/// "that is not a version".
///
/// Only the first kind may fall back to the compatibility table: an unknown or
/// unsupported id has no Java requirement to guess at.
const fn is_offline_failure(error: &MinecraftError) -> bool {
    matches!(
        error,
        MinecraftError::ManifestUnavailable { .. }
            | MinecraftError::VersionMetadataUnavailable { .. }
    )
}

/// The application, opened and ready to serve a frontend.
///
/// Construction resolves paths, loads configuration, applies the configured
/// persistent-instance-root override, creates the base directories, and opens
/// the index. It deliberately does **not** reconcile: reconciliation mutates
/// the index, and a caller that only wants to read should not have to know
/// that. Call [`App::reconcile`] explicitly at startup.
#[derive(Debug)]
pub struct App {
    storage: Storage,
    /// Built on first use rather than at startup: constructing it builds an
    /// HTTP client and a TLS stack, which `ephervex instance list` has no
    /// business paying for.
    minecraft: OnceLock<VersionCatalog>,
    /// Per-release language catalogs, built on first use for the same reason as
    /// the version catalog: it holds an HTTP client.
    languages: OnceLock<LanguageCatalogs>,
    /// The remote options compatibility feed, built on first use.
    options_feed: OnceLock<OptionsFeed>,
    /// The active options compatibility knowledge.
    ///
    /// `RwLock` rather than `OnceLock` because a successful feed refresh has to
    /// be able to replace it mid-process, and `None` until something asks: a
    /// command that only lists instances has no reason to parse a dataset.
    options: RwLock<Option<Arc<OptionsCompatibility>>>,
    loaders: OnceLock<LoaderRegistry>,
    /// The mod catalog, built on first use for the same reason as the two
    /// above: constructing it builds an HTTP client and a TLS stack, and
    /// `ephervex instance list` has no business paying for one.
    pub(crate) mods: OnceLock<ephervex_mod_platform::ModrinthProvider>,
    /// The CurseForge third-party API client, built on first use for the same
    /// reason. It holds no credential: the key is read from the secret store at
    /// the moment a request needs it, so a key set or removed mid-session takes
    /// effect without rebuilding anything.
    pub(crate) curseforge: OnceLock<ephervex_mod_platform::CurseForgeApi>,
    /// The CurseForge catalog as the browser sees it, built on first use over
    /// the client above. Separate from that client because one is a transport
    /// several operations share and the other is a mod provider.
    pub(crate) curseforge_mods: OnceLock<ephervex_mod_platform::CurseForgeProvider>,
    /// The last Java scan, held for the life of the process.
    ///
    /// Discovery forks a JVM per distinct runtime, so recomputing it whenever a
    /// screen repaints is not an option. `RwLock` rather than
    /// `OnceLock` because Refresh has to be able to replace it, and
    /// deliberately not persisted: a Java installed or removed between runs
    /// would make a disk cache lie, and the launcher has no way to notice.
    java: RwLock<Option<Arc<RuntimeInventory>>>,
    /// The signed-in session, for the life of the process and no longer.
    ///
    /// The Minecraft access token lives here and is written nowhere: it expires
    /// in twenty-four hours and the refresh token in the keyring can mint
    /// another, so persisting it would put a second credential at risk for no
    /// benefit. A plain field rather than a lock because the
    /// operations that set it already take `&mut self` - see
    /// [`crate::account`].
    pub(crate) session: Option<ephervex_auth::Session>,
    /// The credential of a session-only sign-in, for the life of the process.
    ///
    /// It never leaves memory and is dropped with the application, so a refresh
    /// reaches the same store the sign-in used (ADR 0009).
    pub(crate) session_only: crate::account::SessionOnlyLogin,
    /// Whether this application may talk plaintext HTTP.
    ///
    /// The field exists only in a test build, so a release binary has no way to
    /// reach the relaxed transport at all: the strict production default is not
    /// a setting anything can change, it is the only behavior compiled in. Set
    /// by [`Self::use_loopback_catalog`] so an integration test can drive the
    /// whole stack - catalog, downloads, verification, publication - against a
    /// local server instead of a production one.
    #[cfg(test)]
    pub(crate) loopback_http: bool,
    /// The sign-in services a test build talks to instead of Microsoft's.
    #[cfg(test)]
    pub(crate) auth_endpoints: Option<ephervex_auth::Endpoints>,
    /// The keyring a test build uses instead of Secret Service. Compiled only
    /// into a test build.
    #[cfg(test)]
    pub(crate) test_keyring: crate::account::TestKeyring,
    /// The Adoptium API root a test build installs managed runtimes from
    /// instead of Adoptium's own. Compiled only into a test build.
    #[cfg(test)]
    pub(crate) adoptium_base: Option<String>,
}

/// Makes a Java override absolute and refuses one with no text form, before
/// anything is claimed or written. A relative path would resolve against
/// whichever directory the next command runs in, so the instance or server
/// would launch a different Java.
pub(crate) fn normalized_java_override(
    java_override: Option<ephervex_model::JavaOverride>,
) -> CoreResult<Option<ephervex_model::JavaOverride>> {
    let Some(java) = java_override else {
        return Ok(None);
    };
    let absolute = std::path::absolute(&java.path).map_err(|source| {
        ephervex_storage::PathError::DirectoryCreation {
            path: java.path.clone(),
            source,
        }
    })?;
    ephervex_storage::utf8_path(&absolute)?;
    Ok(Some(ephervex_model::JavaOverride::new(absolute)))
}

impl App {
    /// Every third-party dependency this build ships, and the terms each is
    /// under.
    ///
    /// Compiled in and generated from the real dependency graph, so it needs
    /// no storage, no network, and no open application; it is an associated
    /// function for that reason, and an "about" screen can answer before
    /// anything else is ready.
    ///
    /// The error is returned rather than swallowed: an inventory this build
    /// cannot read is a broken build, which should be reportable rather than
    /// silently presented as "no dependencies".
    pub fn third_party_licenses()
    -> Result<&'static crate::LicenseInventory, &'static crate::LicenseInventoryError> {
        crate::LicenseInventory::bundled_shared()
    }

    /// Opens the application using the XDG roots resolved from the environment.
    ///
    /// This is the normal startup path for both frontends.
    pub fn open() -> CoreResult<Self> {
        Ok(Self {
            storage: ephervex_storage::open_from_environment()?,
            minecraft: OnceLock::new(),
            languages: OnceLock::new(),
            options_feed: OnceLock::new(),
            options: RwLock::new(None),
            loaders: OnceLock::new(),
            mods: OnceLock::new(),
            curseforge: OnceLock::new(),
            curseforge_mods: OnceLock::new(),
            java: RwLock::new(None),
            session: None,
            session_only: crate::account::SessionOnlyLogin::default(),
            #[cfg(test)]
            loopback_http: false,
            #[cfg(test)]
            auth_endpoints: None,
            #[cfg(test)]
            test_keyring: crate::account::TestKeyring::default(),
            #[cfg(test)]
            adoptium_base: None,
        })
    }

    /// Opens the application against explicit roots.
    ///
    /// Exists so tests - and, later, a `--data-dir` style override - can point
    /// the whole application at a temporary directory instead of the
    /// developer's real home.
    pub fn open_at(paths: Paths) -> CoreResult<Self> {
        Ok(Self {
            storage: ephervex_storage::open(paths)?,
            minecraft: OnceLock::new(),
            languages: OnceLock::new(),
            options_feed: OnceLock::new(),
            options: RwLock::new(None),
            loaders: OnceLock::new(),
            mods: OnceLock::new(),
            curseforge: OnceLock::new(),
            curseforge_mods: OnceLock::new(),
            java: RwLock::new(None),
            session: None,
            session_only: crate::account::SessionOnlyLogin::default(),
            #[cfg(test)]
            loopback_http: false,
            #[cfg(test)]
            auth_endpoints: None,
            #[cfg(test)]
            test_keyring: crate::account::TestKeyring::default(),
            #[cfg(test)]
            adoptium_base: None,
        })
    }

    /// Points the mod catalog and its downloads at a loopback server.
    ///
    /// Compiled only into a test build. See [`Self::loopback_http`].
    #[cfg(test)]
    pub(crate) fn use_loopback_catalog(&mut self, base: &str) -> CoreResult<()> {
        use ephervex_download::{HttpConfig, RetryPolicy};

        self.loopback_http = true;
        let manager = DownloadManager::new(DownloaderConfig {
            http: HttpConfig::for_loopback_tests(),
            retry: RetryPolicy::none(),
            ..DownloaderConfig::default()
        })
        .map_err(
            |source| ephervex_mod_platform::ModPlatformError::ResponseInvalid {
                provider: ephervex_model::ModProvider::Modrinth,
                detail: source.to_string(),
            },
        )?;
        let provider = ephervex_mod_platform::ModrinthProvider::with_base_url(
            manager,
            CacheLayout::new(self.paths().cache_dir()),
            base,
        )?;
        let _ = self.mods.set(provider);
        Ok(())
    }

    /// The effective configuration.
    #[must_use]
    pub fn config(&self) -> &Config {
        &self.storage.config
    }

    /// The resolved application directories.
    #[must_use]
    pub fn paths(&self) -> &Paths {
        self.storage.registry.paths()
    }

    /// Returns Ephervex interface preferences without involving Minecraft settings.
    #[must_use]
    pub fn application_display_settings(&self) -> ApplicationDisplaySettings {
        ApplicationDisplaySettings {
            text_scale: self.storage.config.display.text_scale,
            technical_text_size: self.storage.config.display.technical_text_size,
            theme: self.storage.config.display.theme(),
            show_linked_servers: self.storage.config.display.show_linked_servers,
        }
    }

    /// Persists whether linked servers are included in the global Servers list.
    pub fn set_show_linked_servers(&mut self, show: bool) -> CoreResult<()> {
        self.mutate_config(|config| config.display.show_linked_servers = show)
    }

    /// Returns defaults used for operator setup on newly created servers.
    #[must_use]
    pub const fn server_application_settings(&self) -> ServerApplicationSettings {
        ServerApplicationSettings {
            make_default_account_operator: self
                .storage
                .config
                .server_defaults
                .make_default_account_operator,
            operator_permission_level: self
                .storage
                .config
                .server_defaults
                .operator_permission_level,
        }
    }

    /// Persists defaults for future server operator setup.
    pub fn set_server_operator_defaults(
        &mut self,
        make_default_account_operator: bool,
        operator_permission_level: OperatorPermissionLevel,
    ) -> CoreResult<()> {
        self.mutate_config(|config| {
            config.server_defaults.make_default_account_operator = make_default_account_operator;
            config.server_defaults.operator_permission_level = operator_permission_level;
        })
    }

    /// Persists the linked-server presentation and creation defaults together.
    pub fn set_linked_server_settings(
        &mut self,
        show_globally: bool,
        make_default_account_operator: bool,
        operator_permission_level: OperatorPermissionLevel,
    ) -> CoreResult<()> {
        self.mutate_config(|config| {
            config.display.show_linked_servers = show_globally;
            config.server_defaults.make_default_account_operator = make_default_account_operator;
            config.server_defaults.operator_permission_level = operator_permission_level;
        })
    }

    /// Persists the bounded text choices used by the desktop frontend.
    pub fn set_application_display_settings(
        &mut self,
        text_scale: ApplicationTextScale,
        technical_text_size: TechnicalTextSize,
        theme: &ApplicationTheme,
    ) -> CoreResult<()> {
        self.mutate_config(|config| {
            config.display.text_scale = text_scale;
            config.display.technical_text_size = technical_text_size;
            config.display.set_theme(theme);
        })
    }

    /// Returns the configured, effective, and default persistent roots.
    #[must_use]
    pub fn persistent_storage_settings(&self) -> PersistentStorageSettings {
        PersistentStorageSettings {
            configured_root: self.config().persistent_instance_root(),
            effective_root: self.paths().persistent_instances_dir(),
            default_root: self
                .paths()
                .clone()
                .with_persistent_instance_root(None)
                .persistent_instances_dir(),
        }
    }

    /// Brings the index back in line with what is actually on disk.
    ///
    /// A vanished temporary root is removed silently - that is what an OS temp
    /// cleanup looks like, not an error. A vanished persistent root keeps its
    /// record and raises a warning instead (ADR 0006).
    #[instrument(skip(self))]
    pub fn reconcile(&self) -> CoreResult<ReconciliationReport> {
        Ok(ephervex_storage::reconcile(&self.storage.registry)?)
    }

    // -- client instances --------------------------------------------------

    /// Creates a client instance and materializes its directory.
    ///
    /// The global Minecraft preferences are applied here, once, when the
    /// instance is created. See [`Self::create_client_reporting`] for what was
    /// applied.
    #[instrument(skip(self, request))]
    pub async fn create_client(&self, request: NewClient) -> CoreResult<ClientRecord> {
        Ok(self.create_client_reporting(request).await?.record)
    }

    /// Creates a client instance and reports the one-time preference
    /// application.
    ///
    /// A failure to write `options.txt` is reported and **not** propagated: the
    /// instance exists, is launchable, and the game will write its own defaults
    /// on first run. Undoing a successful creation over a GUI scale would be a
    /// far worse trade - and leaving it half-created would be worse still.
    #[instrument(skip(self, request))]
    pub async fn create_client_reporting(
        &self,
        mut request: NewClient,
    ) -> CoreResult<CreatedClient> {
        // Before the catalog lookups, since a refused name costs nothing to
        // find out and nothing is published until it passes.
        request.name = validated_entity_name(&request.name)?;
        let validation = self.validate_client_target(&request).await;
        let preferences = after_successful_validation(
            validation,
            self.preference_context(&request.minecraft_version),
        )
        .await?;
        self.materialize_client(request, &preferences)
    }

    /// Validates the exact game and loader selection without publishing any
    /// filesystem or registry state.
    async fn validate_client_target(&self, request: &NewClient) -> CoreResult<()> {
        self.minecraft_version(&request.minecraft_version).await?;
        if request.loader.kind == LoaderKind::Vanilla {
            if request.loader.version.is_none() {
                return Ok(());
            }
            return Err(LoaderError::VersionUnavailable {
                loader: LoaderKind::Vanilla,
                minecraft: request.minecraft_version.clone(),
                mode: "exact",
            }
            .into());
        }

        // Refused before a catalog is asked, because the answer is a fact about
        // the loader rather than about today's metadata: the request would
        // otherwise fail as "no such build", which reads as a typo instead of a
        // loader that never existed for this release.
        Self::require_published_loader(request.loader.kind, &request.minecraft_version)?;

        let selected = request.loader.version.as_deref().unwrap_or_default();
        // A build left out of the listing for its installer layout is named for
        // that, not reported as a build that does not exist.
        ephervex_loaders::require_supported_installer_format(
            request.loader.kind,
            &request.minecraft_version,
            selected,
        )?;
        let listing = self
            .loader_versions(request.loader.kind, &request.minecraft_version)
            .await?;
        if !loader_build_is_available(&listing, selected) {
            return Err(LoaderError::VersionUnavailable {
                loader: request.loader.kind,
                minecraft: request.minecraft_version.clone(),
                mode: "exact",
            }
            .into());
        }
        Ok(())
    }

    #[cfg(test)]
    fn publish_client_after_validation(
        &self,
        request: NewClient,
        validation: CoreResult<()>,
        context: &PreferenceContext,
    ) -> CoreResult<CreatedClient> {
        validation?;
        self.materialize_client(request, context)
    }

    /// Creates the instance, applies the one-time preferences, and publishes.
    ///
    /// Publication is the last step, so a failure before it leaves an instance
    /// no other entry point can see (see `ephervex_storage::registry`).
    fn materialize_client(
        &self,
        request: NewClient,
        context: &PreferenceContext,
    ) -> CoreResult<CreatedClient> {
        let registry = self.registry();
        let record = registry.create_client(request)?;
        let preferences = match self.apply_global_preferences(&record, context) {
            Ok(applied) => Some(applied),
            Err(error) => {
                warn!(
                    instance_id = %record.id,
                    %error,
                    "created the instance without applying the global Minecraft preferences"
                );
                None
            }
        };
        if let Err(error) = registry.publish_client(&record) {
            discard_unpublished(registry, &record);
            return Err(error.into());
        }
        Ok(CreatedClient {
            record,
            preferences,
        })
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn create_client_for_test(&self, request: NewClient) -> CoreResult<ClientRecord> {
        Ok(self
            .materialize_client(request, &PreferenceContext::offline())?
            .record)
    }

    /// Commits a change to a client's canonical metadata alone, leaving its
    /// index row as it was: the state after an index write failed behind a
    /// committed update.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn commit_client_without_index_for_test(
        &self,
        id: InstanceId,
        change: impl FnOnce(&mut ClientRecord),
    ) -> CoreResult<ClientRecord> {
        let mut record = self.current_client(id)?;
        change(&mut record);
        ephervex_storage::metadata::write_client_metadata(&record.root, &record)
            .map_err(ephervex_storage::RegistryError::from)?;
        Ok(record)
    }

    /// Fetches one client instance.
    pub fn client(&self, id: InstanceId) -> CoreResult<ClientRecord> {
        Ok(self.registry().client(id)?)
    }

    /// Fetches one client instance as its canonical metadata holds it.
    ///
    /// Anything that shows or plans from a client's Minecraft version, loader,
    /// overrides, or flags reads this, so a lagging index row can neither be
    /// displayed as the current selection nor seed a new value that writes a
    /// replaced one back. A reader that only needs the name or root path may
    /// use [`Self::client`].
    pub fn current_client(&self, id: InstanceId) -> CoreResult<ClientRecord> {
        Ok(self.registry().current_client(id)?)
    }

    /// Fetches one server.
    pub fn server(&self, id: ServerId) -> CoreResult<ServerRecord> {
        Ok(self.registry().server(id)?)
    }

    /// Lists every indexed client instance.
    pub fn list_clients(&self) -> CoreResult<Vec<ClientRecord>> {
        Ok(self.registry().list_clients()?)
    }

    /// Deletes a client instance and every server linked to it.
    ///
    /// `files` is explicit rather than inferred: removing a user's game
    /// directory is not something to decide on their behalf from context. The
    /// linked servers follow the same choice, because a server that exists to
    /// reproduce one client has nothing left to be once that client is gone.
    ///
    /// The storage layer's own delete only unlinks them, which is the right
    /// primitive: whether a linked server outlives its client is a product
    /// decision, not a storage invariant, so it is made here.
    ///
    /// A launching or running instance, or a running linked server, refuses
    /// the whole deletion rather than being stopped as a side effect. Nothing
    /// is removed in that case, so the user can stop it and ask again, or ask
    /// [`App::delete_client_stopping_linked_servers`] to stop the servers.
    ///
    /// Returns the linked servers that were deleted with the client, so a
    /// caller reports what was removed rather than what it expected to remove.
    #[instrument(skip(self))]
    pub fn delete_client(
        &self,
        id: InstanceId,
        files: FileDisposition,
    ) -> CoreResult<Vec<ServerRecord>> {
        let claim = self.claim_stopped_client(id)?;
        self.delete_claimed_client(id, files, &claim)
    }

    /// The deletion itself, for a caller that holds the instance's claim and
    /// has found it stopped.
    fn delete_claimed_client(
        &self,
        id: InstanceId,
        files: FileDisposition,
        _claim: &ephervex_storage::HostClaim,
    ) -> CoreResult<Vec<ServerRecord>> {
        self.registry()
            .refuse_removing_foreign_client_files(id, files)?;
        let mut servers: Vec<(ServerRecord, ephervex_storage::HostClaim)> = Vec::new();
        // Every listed server is checked before any is deleted, so a refusal removes nothing.
        // Each stays claimed until the client is gone, so none starts in between.
        // The listing is repeated until it holds only claimed servers, so one
        // linked meanwhile is deleted with the client instead of being unlinked.
        loop {
            let mut claimed_more = false;
            for server in self.linked_server_ids(id)? {
                if servers.iter().any(|(record, _)| record.id == server) {
                    continue;
                }
                let claim = self
                    .registry()
                    .claim_host(ephervex_storage::RecordRef::Server(server))?;
                let record = self.server_for_deletion(server, files)?;
                let runtime = crate::server::settled_runtime(&record.root)?;
                if crate::server::stoppable_for_deletion(&runtime) {
                    return Err(crate::CoreError::LinkedServerRunning { name: record.name });
                }
                if runtime.may_be_running() {
                    return Err(crate::CoreError::LinkedServerUnstoppable { name: record.name });
                }
                servers.push((record, claim));
                claimed_more = true;
            }
            if !claimed_more {
                break;
            }
        }
        for (server, _claim) in &servers {
            self.delete_claimed_server(server, files)?;
        }
        let claims: Vec<&ephervex_storage::HostClaim> =
            servers.iter().map(|(_, claim)| claim).collect();
        self.registry().delete_client(id, files, &claims)?;
        Ok(servers.into_iter().map(|(record, _)| record).collect())
    }

    /// Deletes a client instance like [`App::delete_client`], stopping its
    /// running linked servers first.
    ///
    /// For a deletion the user confirmed knowing those servers are running.
    /// A running instance still refuses, and a server that cannot be stopped
    /// refuses before anything is removed.
    #[instrument(skip(self))]
    pub async fn delete_client_stopping_linked_servers(
        &self,
        id: InstanceId,
        files: FileDisposition,
    ) -> CoreResult<Vec<ServerRecord>> {
        let claim = self.claim_stopped_client(id)?;
        self.registry()
            .refuse_removing_foreign_client_files(id, files)?;
        self.stop_linked_servers_for_deletion(id, files).await?;
        self.delete_claimed_client(id, files, &claim)
    }

    /// Claims a client for a change that needs its game stopped, and refuses
    /// while a launch holds it.
    ///
    /// The claim is taken before the check and the caller keeps it for the whole
    /// change. A launch records itself under the same claim, so none can begin
    /// between the check and the end of the change.
    pub(crate) fn claim_stopped_client(
        &self,
        id: InstanceId,
    ) -> CoreResult<ephervex_storage::HostClaim> {
        let claim = self
            .registry()
            .claim_host(ephervex_storage::RecordRef::Client(id))?;
        self.refuse_while_running(id)?;
        Ok(claim)
    }

    /// Claims a server for a change that needs it stopped, and refuses while it
    /// may be running. Returns the record as read under the claim.
    ///
    /// The claim is kept for the whole change. A start takes it before it looks
    /// at the runtime and keeps it until the server owns its runtime lock, so
    /// none can begin in between.
    pub(crate) fn claim_stopped_server(
        &self,
        id: ServerId,
    ) -> CoreResult<(ephervex_storage::HostClaim, ServerRecord)> {
        let claim = self
            .registry()
            .claim_host(ephervex_storage::RecordRef::Server(id))?;
        let record = self.canonical_server_for_mutation(id)?;
        crate::server::refuse_if_running(&record)?;
        Ok((claim, record))
    }

    /// Refuses a change that cannot happen under a launching or running game.
    pub(crate) fn refuse_while_running(&self, id: InstanceId) -> CoreResult<()> {
        match self.launch_state(id)? {
            Some(state) => Err(crate::CoreError::InstanceRunning {
                pid: state.holder_pid(),
                since: state.since,
            }),
            None => Ok(()),
        }
    }

    /// Replaces an instance's own Java pin and leaves its JVM settings as they
    /// are, read and written under the same claim, so a JVM change made
    /// meanwhile from elsewhere is never written back over. `None` follows the
    /// global setting again. A relative path is stored as an absolute one.
    #[instrument(skip(self))]
    pub fn set_client_java_override(
        &self,
        id: InstanceId,
        java_override: Option<ephervex_model::JavaOverride>,
    ) -> CoreResult<ClientCommit<ClientRecord>> {
        let java_override = normalized_java_override(java_override)?;
        let claim = self.claim_stopped_client(id)?;
        self.client(id)?;
        Ok(self
            .registry()
            .modify_client_committed_claimed(id, &claim, |record| {
                record.java_override = java_override;
            })?)
    }

    /// Replaces one instance's Java and JVM overrides as one metadata mutation.
    ///
    /// A relative Java path is stored as an absolute one.
    ///
    /// `None` on either restores the corresponding global setting, which is
    /// what "use the global value" means and is a different state from pinning
    /// the value the global one happens to hold today.
    ///
    /// Refuses while the instance is launching or running, for the same reason
    /// the server equivalent does: what a running process was started with
    /// cannot be changed underneath it, and a record that disagreed with the
    /// live command would misdescribe the reproduction.
    #[instrument(skip(self))]
    pub fn set_client_runtime_settings(
        &self,
        id: InstanceId,
        java_override: Option<ephervex_model::JavaOverride>,
        jvm_override: Option<ephervex_model::JvmSettings>,
    ) -> CoreResult<ClientCommit<ClientRecord>> {
        let java_override = normalized_java_override(java_override)?;
        let claim = self.claim_stopped_client(id)?;
        self.client(id)?;
        self.validate_jvm_override(jvm_override.as_ref())?;
        Ok(self
            .registry()
            .modify_client_committed_claimed(id, &claim, |record| {
                record.java_override = java_override;
                record.jvm_override = jvm_override;
            })?)
    }

    /// Refuses an override that, layered over the current global settings,
    /// leaves no valid heap range. Planning checks again because the global
    /// settings can change after the override is saved.
    pub(crate) fn validate_jvm_override(
        &self,
        jvm_override: Option<&ephervex_model::JvmSettings>,
    ) -> CoreResult<()> {
        EffectiveJvmSettings::resolve(&self.java_settings(), jvm_override)
            .validate_memory_range()?;
        Ok(())
    }

    /// Turns pack-owned mod mutation on or off for this instance.
    ///
    /// Available while the instance is running. The setting decides what the
    /// mod list offers, and the operations it unlocks write into `mods/`, which
    /// the game reads when it starts; a running copy is a reason to restart it,
    /// not a reason to refuse the decision.
    #[instrument(skip(self))]
    pub fn set_allow_pack_mod_version_changes(
        &self,
        id: InstanceId,
        allow: bool,
    ) -> CoreResult<ClientCommit<ClientRecord>> {
        Ok(self.registry().modify_client_committed(id, |record| {
            record.allow_pack_mod_version_changes = allow;
        })?)
    }

    /// The servers linked to a client.
    ///
    /// Derived from the server rows rather than stored on the client, so it
    /// cannot go stale when a server is deleted.
    pub fn linked_server_ids(&self, client: InstanceId) -> CoreResult<Vec<ServerId>> {
        Ok(self.registry().linked_server_ids(client)?)
    }

    // -- servers -----------------------------------------------------------

    /// Lists every indexed server.
    pub fn list_servers(&self) -> CoreResult<Vec<ServerRecord>> {
        Ok(self.registry().list_servers()?)
    }

    // -- minecraft versions ------------------------------------------------

    /// The Minecraft releases Ephervex offers: `type == "release"` at or above
    /// 1.12.2, in Mojang's own newest-first order.
    ///
    /// The returned [`ReleaseListing`] says whether the list came off the
    /// network, out of a fresh cache, or out of a stale one because Mojang
    /// could not be reached. A frontend is expected to show that distinction
    /// rather than a spinner.
    /// The published options compatibility data is refreshed alongside the
    /// list, because this is the one operation whose whole job is finding out
    /// what Mojang has published lately. Both refreshes are TTL-gated and
    /// neither can fail the listing.
    #[instrument(skip(self))]
    pub async fn minecraft_releases(&self) -> CoreResult<ReleaseListing> {
        let catalog = self.minecraft()?;
        let listing = catalog
            .supported_releases(&Self::reporter(), &CancellationToken::new())
            .await?;
        self.refresh_minecraft_options().await;
        Ok(listing)
    }

    /// Refuses a loader that never had a build for a Minecraft release.
    pub(crate) fn require_published_loader(loader: LoaderKind, minecraft: &str) -> CoreResult<()> {
        if ephervex_loaders::supports_minecraft(loader, minecraft) {
            return Ok(());
        }
        Err(LoaderError::LoaderNeverPublished {
            loader,
            minecraft: minecraft.to_owned(),
            // Present whenever the check above refused, because a loader with no
            // floor is never refused.
            since: ephervex_loaders::minimum_minecraft(loader).unwrap_or_default(),
        }
        .into())
    }

    /// The Minecraft releases the user has marked as favorites, in the order
    /// they should be offered.
    #[must_use]
    pub fn favorite_minecraft_versions(&self) -> &[String] {
        &self.config().favorite_minecraft_versions
    }

    /// Records which Minecraft releases are favorites, in the order given.
    ///
    /// The order is the setting, so it is stored as given rather than sorted:
    /// what the user put first is what the New Instance dialog offers first.
    /// Duplicates collapse to their first appearance, and a blank entry is
    /// dropped, because neither can name a release.
    ///
    /// The list is never checked against a catalog. A favorite is a statement
    /// about the user's own work, and a release Mojang's manifest is
    /// momentarily unreachable for is not one they have stopped caring about.
    /// An entry no current release matches is simply not shown.
    pub fn set_favorite_minecraft_versions(
        &mut self,
        versions: impl IntoIterator<Item = String>,
    ) -> CoreResult<()> {
        let mut kept: Vec<String> = Vec::new();
        for version in versions {
            let version = version.trim().to_owned();
            if !version.is_empty() && !kept.contains(&version) {
                kept.push(version);
            }
        }
        self.mutate_config(|config| config.favorite_minecraft_versions = kept)
    }

    /// The loaders that have ever had a build for one Minecraft release, in the
    /// order a frontend should offer them.
    ///
    /// A fact about each loader's own history rather than about what a catalog
    /// serves today, so it is answerable offline and instantly. NeoForge did not
    /// exist before Minecraft 1.20.2, and offering it for an older release only
    /// leads to a build list that never fills in.
    #[must_use]
    pub fn available_loaders(minecraft: &str) -> Vec<LoaderKind> {
        [LoaderKind::Vanilla, LoaderKind::Forge, LoaderKind::NeoForge]
            .into_iter()
            .filter(|kind| ephervex_loaders::supports_minecraft(*kind, minecraft))
            .collect()
    }

    /// One supported release's complete metadata.
    ///
    /// Refuses a snapshot, a pre-release, or anything below the floor rather
    /// than resolving it anyway.
    #[instrument(skip(self))]
    pub async fn minecraft_version(&self, id: &str) -> CoreResult<VersionMetadata> {
        let catalog = self.minecraft()?;
        Ok(catalog
            .resolve(id, &Self::reporter(), &CancellationToken::new())
            .await?)
    }

    /// The exact loader builds offered for one Minecraft release.
    #[instrument(skip(self))]
    pub async fn loader_versions(
        &self,
        kind: LoaderKind,
        minecraft: &str,
    ) -> CoreResult<LoaderVersionListing> {
        // A loader that never existed for this release has no catalog to ask,
        // and an empty build list is a far less useful answer than saying so.
        Self::require_published_loader(kind, minecraft)?;
        Ok(self
            .loaders()?
            .provider(kind)
            .list_versions(
                minecraft,
                &OperationReporter::silent("loader-metadata"),
                &CancellationToken::new(),
            )
            .await?)
    }

    /// Resolves Stable, Latest, or an exact imported loader build.
    #[instrument(skip(self))]
    pub async fn resolve_loader_version(
        &self,
        kind: LoaderKind,
        minecraft: &str,
        mode: LoaderVersionMode,
        exact: Option<&str>,
    ) -> CoreResult<String> {
        Ok(self
            .loaders()?
            .provider(kind)
            .resolve_version(
                minecraft,
                mode,
                exact,
                &OperationReporter::silent("loader-metadata"),
                &CancellationToken::new(),
            )
            .await?)
    }

    /// Installs a dedicated-server layout without starting it or touching its
    /// EULA. Server lifecycle belongs to `ephervex-server`.
    #[instrument(skip(self, java, target, cancel))]
    pub async fn install_server_version(
        &self,
        minecraft: &str,
        loader: &ephervex_model::LoaderSelection,
        java: &Path,
        target: &Path,
        cancel: &CancellationToken,
    ) -> CoreResult<InstalledServer> {
        self.install_server_version_reporting(minecraft, loader, java, target, None, cancel)
            .await
    }

    /// The same install, reporting into a caller's operation where it has one.
    pub(crate) async fn install_server_version_reporting(
        &self,
        minecraft: &str,
        loader: &ephervex_model::LoaderSelection,
        java: &Path,
        target: &Path,
        reporter: Option<&OperationReporter>,
        cancel: &CancellationToken,
    ) -> CoreResult<InstalledServer> {
        self.with_artifact_cache(Box::pin(self.install_server_version_holding_the_cache(
            minecraft, loader, java, target, reporter, cancel,
        )))
        .await
    }

    async fn install_server_version_holding_the_cache(
        &self,
        minecraft: &str,
        loader: &ephervex_model::LoaderSelection,
        java: &Path,
        target: &Path,
        caller: Option<&OperationReporter>,
        cancel: &CancellationToken,
    ) -> CoreResult<InstalledServer> {
        Self::require_published_loader(loader.kind, minecraft)?;
        let parent = self.minecraft_version(minecraft).await?;
        let silent = OperationReporter::silent("installing-loader-server");
        let reporter = caller.unwrap_or(&silent);
        Ok(self
            .loaders()?
            .provider(loader.kind)
            .install_server(ServerInstallRequest {
                minecraft,
                exact_loader: loader.version.as_deref(),
                parent: &parent,
                java,
                target,
                reporter,
                cancel,
            })
            .await?)
    }

    // -- java runtimes -----------------------------------------------------

    /// Every Java runtime installed on this machine, from the process cache.
    ///
    /// The first call scans; later calls return what it found. Scanning forks a
    /// JVM per distinct runtime, so a frontend may call this as often as it
    /// likes - which is the point of caching it here rather than asking every
    /// caller to remember. A managed runtime installed or removed since the
    /// scan, by another `App` or process, is caught up by probing the managed
    /// store alone.
    #[instrument(skip(self))]
    pub async fn java_runtimes(&self) -> CoreResult<Arc<RuntimeInventory>> {
        let Some(inventory) = self.cached_java() else {
            return self.refresh_java_runtimes().await;
        };
        let store = self.paths().java_dir();
        if inventory.matches_managed_store(&store) {
            return Ok(inventory);
        }
        let managed = JavaDiscovery::isolated()
            .with_managed_root(&store)
            .discover()
            .await;
        let updated = Arc::new(inventory.with_managed_from(&store, managed));
        if let Ok(mut cache) = self.java.write() {
            *cache = Some(Arc::clone(&updated));
        }
        Ok(updated)
    }

    /// Rescans for Java runtimes, discarding what was cached.
    ///
    /// the runtime inventory refresh operation. Needed because the cache cannot notice a JDK installed
    /// while the launcher was open, and guessing at when to invalidate it would
    /// be worse than letting the user say so.
    #[instrument(skip(self))]
    pub async fn refresh_java_runtimes(&self) -> CoreResult<Arc<RuntimeInventory>> {
        let inventory = Arc::new(
            JavaDiscovery::system(self.paths().java_dir())
                .discover()
                .await,
        );
        if let Ok(mut cache) = self.java.write() {
            *cache = Some(Arc::clone(&inventory));
        }
        Ok(inventory)
    }

    /// Identifies one specific executable, bypassing discovery entirely.
    ///
    /// the custom-runtime validation operation. A path reaching here is untrusted input — a settings
    /// field, an imported override — and running it is precisely how its claim
    /// to be a JVM gets checked rather than assumed.
    #[instrument(skip(self))]
    pub async fn test_java_runtime(&self, executable: &Path) -> CoreResult<JavaRuntime> {
        Ok(probe_runtime(executable, JavaSourceKind::UserSupplied).await?)
    }

    /// The global Java settings, read out of the configuration file.
    #[must_use]
    pub fn java_settings(&self) -> GlobalJavaSettings {
        global_java_settings(&self.config().java)
    }

    /// Replaces global Java discovery and JVM defaults after validating the
    /// memory range as one coherent setting.
    pub fn set_java_settings(&mut self, settings: &GlobalJavaSettings) -> CoreResult<()> {
        EffectiveJvmSettings::resolve(settings, None).validate_memory_range()?;
        let preferred_path = settings
            .preferred_path
            .as_deref()
            .map(ephervex_storage::utf8_path)
            .transpose()?
            .unwrap_or_default();
        self.mutate_config(|config| {
            config.java.auto_detect = settings.auto_detect;
            config.java.auto_download = settings.auto_download;
            config.java.min_memory_mib = settings.min_memory_mib;
            config.java.max_memory_mib = settings.max_memory_mib;
            config.java.jvm_args.clone_from(&settings.jvm_args);
            preferred_path.clone_into(&mut config.java.preferred_path);
        })
    }

    /// Works out which runtime would run a given Minecraft version, and why.
    ///
    /// This call resolves the requirement, applies the configured override layers,
    /// selects or checks a runtime, and — when asked and
    /// permitted — install a managed one because nothing suitable exists.
    ///
    /// It fails rather than substituting. A plan that names a runtime which
    /// will crash on launch is worse than no plan, because the crash arrives
    /// much later wearing someone else's stack trace.
    #[instrument(skip(self, request))]
    pub async fn plan_java(
        &self,
        minecraft_version: &str,
        request: &JavaPlanRequest,
    ) -> CoreResult<JavaPlan> {
        let required = self.required_java(minecraft_version).await?;
        let (loader, java_override, jvm_override) = self.java_target_selection(request.target)?;
        self.plan_java_for_selection(
            minecraft_version,
            required,
            &loader,
            java_override,
            jvm_override,
            request,
        )
        .await
    }

    /// The loader and overrides a Java plan layers over the global settings.
    fn java_target_selection(
        &self,
        target: JavaTarget,
    ) -> CoreResult<(
        ephervex_model::LoaderSelection,
        Option<ephervex_model::JavaOverride>,
        Option<ephervex_model::JvmSettings>,
    )> {
        Ok(match target {
            JavaTarget::VersionOnly => (ephervex_model::LoaderSelection::vanilla(), None, None),
            JavaTarget::Client(id) => {
                let record = self.current_client(id)?;
                (record.loader, record.java_override, record.jvm_override)
            }
            JavaTarget::Server(id) => {
                let record = self.server(id)?;
                (record.loader, record.java_override, record.jvm_override)
            }
        })
    }

    pub(crate) async fn plan_java_for_unindexed_server(
        &self,
        minecraft_version: &str,
        loader: &ephervex_model::LoaderSelection,
        request: &JavaPlanRequest,
    ) -> CoreResult<JavaPlan> {
        let required = self.required_java(minecraft_version).await?;
        self.plan_java_for_selection(minecraft_version, required, loader, None, None, request)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn plan_java_for_selection(
        &self,
        minecraft_version: &str,
        required: RequiredJava,
        loader: &ephervex_model::LoaderSelection,
        java_override: Option<ephervex_model::JavaOverride>,
        jvm_override: Option<ephervex_model::JvmSettings>,
        request: &JavaPlanRequest,
    ) -> CoreResult<JavaPlan> {
        self.resolve_java(
            minecraft_version,
            required,
            loader,
            java_override,
            jvm_override,
            request,
            PinProbe::Fresh,
        )
        .await
        .map_err(|refusal| refusal.error)
    }

    /// The loader's own bounds on the runtime, the rules every plan, status
    /// and runtime listing evaluates.
    pub(crate) fn loader_java_rules(
        &self,
        minecraft_version: &str,
        loader: &ephervex_model::LoaderSelection,
        required_major: u32,
    ) -> CoreResult<Vec<ephervex_java::VersionBound>> {
        Ok(self.loaders()?.provider(loader.kind).java_rules(
            minecraft_version,
            loader.version.as_deref(),
            required_major,
        ))
    }

    /// Plans a runtime like [`Self::plan_java_for_selection`], and on a refusal
    /// keeps the pinned runtime that was refused when it could be identified.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn resolve_java(
        &self,
        minecraft_version: &str,
        required: RequiredJava,
        loader: &ephervex_model::LoaderSelection,
        java_override: Option<ephervex_model::JavaOverride>,
        jvm_override: Option<ephervex_model::JvmSettings>,
        request: &JavaPlanRequest,
        probe: PinProbe,
    ) -> Result<JavaPlan, Box<PlanRefusal>> {
        let loader_rules = self
            .loader_java_rules(minecraft_version, loader, required.major)
            .map_err(Box::<PlanRefusal>::from)?;
        let rules: Vec<&dyn JavaCompatibilityRule> = loader_rules
            .iter()
            .map(|rule| rule as &dyn JavaCompatibilityRule)
            .collect();
        let maximum_update =
            ephervex_java::ExpectedJava::new(&required, &loader_rules).maximum_update();

        let settings = self.java_settings();
        let preference = JavaPreference {
            explicit: request.explicit_path.clone(),
            instance: java_override,
            global: settings.preferred_path.clone(),
        };
        let jvm = EffectiveJvmSettings::resolve(&settings, jvm_override.as_ref());
        jvm.validate_memory_range().map_err(CoreError::from)?;

        let choice = preference.resolve();
        if let JavaPathChoice::Pinned { path, source } = &choice {
            let runtime = self
                .identify_pinned_java(path, *source, probe)
                .await
                .map_err(CoreError::from)?;
            if let Err(error) = ephervex_java::check_pinned(&runtime, &required, &rules, *source) {
                return Err(Box::new(PlanRefusal {
                    error: error.into(),
                    attempted: Some(runtime),
                }));
            }
            return Ok(JavaPlan {
                minecraft_version: minecraft_version.to_owned(),
                required,
                choice: choice.clone(),
                runtime,
                rejected: Vec::new(),
                jvm,
                installed_now: false,
            });
        }

        if !settings.auto_detect {
            // Auto-detection off with nothing pinned leaves nothing to choose
            // from. Scanning anyway would ignore a setting the user set.
            return Err(CoreError::from(JavaError::AutoDetectDisabled {
                required_major: required.major,
            })
            .into());
        }

        let inventory = self.java_runtimes().await?;
        match ephervex_java::select(&inventory, &required, &rules) {
            Ok(Selection { runtime, rejected }) => Ok(JavaPlan {
                minecraft_version: minecraft_version.to_owned(),
                required,
                choice,
                runtime,
                rejected,
                jvm,
                installed_now: false,
            }),
            Err(error) => {
                if !request.install_if_missing {
                    return Err(CoreError::from(error).into());
                }
                if !settings.auto_download {
                    return Err(CoreError::from(JavaError::ManagedDownloadDisabled {
                        required_major: required.major,
                    })
                    .into());
                }
                let installed = self
                    .install_java_with_maximum_update(
                        required.major,
                        maximum_update,
                        request.reporter.as_ref(),
                        &request.cancel,
                    )
                    .await?;
                let inventory = RuntimeInventory {
                    runtimes: vec![installed.runtime.clone()],
                    failures: Vec::new(),
                    probed: 1,
                    candidates: 1,
                    scanned_at: chrono::Utc::now(),
                };
                let checked = ephervex_java::select(&inventory, &required, &rules)
                    .map_err(CoreError::from)?;
                Ok(JavaPlan {
                    minecraft_version: minecraft_version.to_owned(),
                    required,
                    choice,
                    runtime: checked.runtime,
                    rejected: Vec::new(),
                    jvm,
                    installed_now: installed.newly_installed,
                })
            }
        }
    }

    /// Replaces the cached inventory, as if a scan had found exactly these.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn use_java_inventory_for_test(&self, runtimes: Vec<JavaRuntime>) {
        if let Ok(mut cache) = self.java.write() {
            *cache = Some(Arc::new(RuntimeInventory {
                probed: runtimes.len(),
                candidates: runtimes.len(),
                runtimes,
                failures: Vec::new(),
                scanned_at: chrono::Utc::now(),
            }));
        }
    }

    /// Identifies a pinned executable as a runtime, found through the pin and
    /// marked managed when it lives under Ephervex's managed Java directory.
    ///
    /// [`PinProbe::ReuseInventory`] takes the probed facts from the cached
    /// inventory when it already holds the same canonical executable, so a
    /// status refresh does not fork a JVM each time. A launch always probes,
    /// because what it runs must be what was checked. Either way the runtime is
    /// reported as found through the pin, so its origin does not depend on
    /// whether a scan happened to run first.
    pub(crate) async fn identify_pinned_java(
        &self,
        path: &Path,
        source: PinSource,
        probe: PinProbe,
    ) -> Result<JavaRuntime, JavaError> {
        let canonical = tokio::fs::canonicalize(path).await.ok();
        let known = (probe == PinProbe::ReuseInventory)
            .then(|| self.cached_java())
            .flatten()
            .zip(canonical.as_ref())
            .and_then(|(inventory, canonical)| {
                inventory
                    .runtimes
                    .iter()
                    .find(|runtime| &runtime.executable == canonical)
                    .cloned()
            });
        let mut runtime = match known {
            Some(mut known) => {
                known.origins = vec![JavaOrigin {
                    kind: pin_source_kind(source),
                    path: path.to_path_buf(),
                }];
                known
            }
            None => probe_runtime(path, pin_source_kind(source)).await?,
        };
        if self.is_managed_java(&runtime.executable).await {
            runtime.origins.push(JavaOrigin {
                kind: JavaSourceKind::Managed,
                path: runtime.executable.clone(),
            });
            runtime.origins.sort();
        }
        Ok(runtime)
    }

    /// Whether an executable lives under the managed Java directory.
    async fn is_managed_java(&self, executable: &Path) -> bool {
        let Ok(root) = tokio::fs::canonicalize(self.paths().java_dir()).await else {
            return false;
        };
        tokio::fs::canonicalize(executable)
            .await
            .is_ok_and(|executable| executable.starts_with(root))
    }

    /// Downloads and installs a managed runtime of one major.
    ///
    /// Installs under the user's own data directory; nothing here needs or asks
    /// for elevated privileges. The new runtime joins the cached inventory, so
    /// it is visible without an explicit Refresh.
    #[instrument(skip(self))]
    pub async fn install_java(&self, major: u32) -> CoreResult<ManagedInstallation> {
        self.install_java_with_maximum_update(major, None, None, &CancellationToken::new())
            .await
    }

    pub(crate) async fn install_java_with_maximum_update(
        &self,
        major: u32,
        maximum_update: Option<u32>,
        reporter: Option<&OperationReporter>,
        cancel: &CancellationToken,
    ) -> CoreResult<ManagedInstallation> {
        // The archive lands in the shared cache and is unpacked into the data
        // root, so the lease has to outlive the unpack rather than the download.
        self.with_artifact_cache(Box::pin(self.install_java_holding_the_cache(
            major,
            maximum_update,
            reporter,
            cancel,
        )))
        .await
    }

    async fn install_java_holding_the_cache(
        &self,
        major: u32,
        maximum_update: Option<u32>,
        reporter: Option<&OperationReporter>,
        cancel: &CancellationToken,
    ) -> CoreResult<ManagedInstallation> {
        let manager = DownloadManager::new(self.download_config()).map_err(|source| {
            JavaError::ManagedDownloadFailed {
                major,
                release: String::new(),
                source: Box::new(source),
            }
        })?;
        let provider = ManagedJavaProvider::new(
            manager,
            CacheLayout::new(self.paths().cache_dir()),
            self.paths().java_dir(),
        )?;

        #[cfg(test)]
        let provider = match &self.adoptium_base {
            Some(base) => provider.with_api_base(base)?,
            None => provider,
        };

        let reporter = reporter
            .cloned()
            .unwrap_or_else(|| OperationReporter::silent("managed-java"));
        let installed = match maximum_update {
            Some(maximum) => {
                provider
                    .install_at_most_update(major, maximum, &reporter, cancel)
                    .await?
            }
            None => provider.install(major, &reporter, cancel).await?,
        };
        self.remember_installed_java(&installed.runtime);
        Ok(installed)
    }

    /// Adds a runtime Ephervex just installed to the cached inventory, so it
    /// is visible at once without scanning every other runtime again.
    fn remember_installed_java(&self, runtime: &JavaRuntime) {
        let Ok(mut cache) = self.java.write() else {
            return;
        };
        if let Some(inventory) = cache.as_ref() {
            let mut updated = RuntimeInventory::clone(inventory);
            updated.remember(runtime.clone());
            *cache = Some(Arc::new(updated));
        }
    }

    /// Removes a runtime Ephervex installed, and reports the directory that
    /// held it.
    ///
    /// Only a managed runtime can be removed this way: a JDK found on `PATH`
    /// or in `/usr/lib/jvm` belongs to whoever installed it, and the launcher
    /// deleting it would be deleting somebody else's software. The cached
    /// inventory is dropped so the next scan reflects what is left.
    ///
    /// A runtime a game is currently running from is not special-cased: the
    /// process holds its own open files, and what is deleted is the directory
    /// the *next* launch would have selected.
    #[instrument(skip(self))]
    pub async fn remove_managed_java_runtime(&self, executable: &Path) -> CoreResult<PathBuf> {
        let manager = DownloadManager::new(self.download_config()).map_err(|source| {
            JavaError::ManagedDownloadFailed {
                major: 0,
                release: String::new(),
                source: Box::new(source),
            }
        })?;
        let provider = ManagedJavaProvider::new(
            manager,
            CacheLayout::new(self.paths().cache_dir()),
            self.paths().java_dir(),
        )?;
        let removed = provider.remove(executable).await?;
        if let Ok(mut cache) = self.java.write() {
            *cache = None;
        }
        Ok(removed)
    }

    /// The Java major a Minecraft version needs.
    ///
    /// Mojang's own answer when the metadata can be reached, the fallback table
    /// when it cannot. An *unreachable* provider degrades to the table; an id
    /// that is genuinely not a supported release does not,
    /// because answering "Java 8" for a version that does not exist would be a
    /// plan for nothing.
    pub(crate) async fn required_java(&self, minecraft_version: &str) -> CoreResult<RequiredJava> {
        match self.minecraft_version(minecraft_version).await {
            Ok(metadata) => Ok(RequiredJava::resolve(&metadata)),
            Err(crate::CoreError::Minecraft(error)) if is_offline_failure(&error) => {
                warn!(
                    version = minecraft_version,
                    %error,
                    "using the java compatibility table; mojang metadata is unavailable"
                );
                Ok(RequiredJava::from_table(&MinecraftVersionId::parse(
                    minecraft_version,
                )))
            }
            Err(other) => Err(other),
        }
    }

    /// The cached inventory, if a scan has already run.
    ///
    /// A poisoned lock is treated as a cache miss rather than propagated: the
    /// worst outcome is one extra scan, and refusing to list runtimes because
    /// an unrelated thread panicked would be a poor trade.
    pub(crate) fn cached_java(&self) -> Option<Arc<RuntimeInventory>> {
        self.java.read().ok()?.clone()
    }

    /// The version catalog, built on first use.
    ///
    /// `OnceLock` rather than a field set in `open`: both construction steps
    /// can fail, and a launcher that refuses to open because the HTTP client
    /// could not be built would be unable to show the user their existing
    /// instances — which is precisely what it should still be able to do
    /// Per-release language catalogs, on the same download manager and cache
    /// root as everything else.
    pub(crate) fn languages(&self) -> CoreResult<&LanguageCatalogs> {
        if let Some(catalogs) = self.languages.get() {
            return Ok(catalogs);
        }
        let catalog = self.minecraft()?.clone();
        let manager = DownloadManager::new(self.download_config()).map_err(|source| {
            MinecraftError::ManifestUnavailable {
                source: MetadataCacheError::Unavailable {
                    key: VERSION_MANIFEST_CACHE_KEY.to_owned(),
                    source: Box::new(source),
                },
            }
        })?;
        let catalogs =
            LanguageCatalogs::new(catalog, manager, CacheLayout::new(self.paths().cache_dir()));
        Ok(self.languages.get_or_init(|| catalogs))
    }

    /// The remote options compatibility feed.
    pub(crate) fn options_feed(&self) -> CoreResult<&OptionsFeed> {
        if let Some(feed) = self.options_feed.get() {
            return Ok(feed);
        }
        let manager = DownloadManager::new(self.download_config()).map_err(|source| {
            MinecraftError::ManifestUnavailable {
                source: MetadataCacheError::Unavailable {
                    key: VERSION_MANIFEST_CACHE_KEY.to_owned(),
                    source: Box::new(source),
                },
            }
        })?;
        let feed = OptionsFeed::new(manager, CacheLayout::new(self.paths().cache_dir()))
            .map_err(MinecraftError::from)?;
        Ok(self.options_feed.get_or_init(|| feed))
    }

    /// The active options compatibility knowledge, without any I/O beyond
    /// reading what is already loaded.
    pub(crate) fn options_slot(&self) -> &RwLock<Option<Arc<OptionsCompatibility>>> {
        &self.options
    }

    pub(crate) fn minecraft(&self) -> CoreResult<&VersionCatalog> {
        if let Some(catalog) = self.minecraft.get() {
            return Ok(catalog);
        }
        let manager = DownloadManager::new(self.download_config()).map_err(|source| {
            MinecraftError::ManifestUnavailable {
                source: MetadataCacheError::Unavailable {
                    key: VERSION_MANIFEST_CACHE_KEY.to_owned(),
                    source: Box::new(source),
                },
            }
        })?;
        let catalog = VersionCatalog::mojang(manager, CacheLayout::new(self.paths().cache_dir()))?;
        // Losing the race just means another thread built an equivalent
        // catalog; this one is dropped and theirs is returned.
        Ok(self.minecraft.get_or_init(|| catalog))
    }

    /// The loader registry shared by discovery, Java planning, and launch.
    pub(crate) fn loaders(&self) -> CoreResult<&LoaderRegistry> {
        if let Some(loaders) = self.loaders.get() {
            return Ok(loaders);
        }
        let manager = DownloadManager::new(self.download_config()).map_err(|source| {
            ephervex_loaders::LoaderError::MetadataInvalid {
                provider: "loader",
                detail: source.to_string(),
            }
        })?;
        let loaders = LoaderRegistry::new(manager, CacheLayout::new(self.paths().cache_dir()))?;
        Ok(self.loaders.get_or_init(|| loaders))
    }

    /// The downloader settings every request this application makes starts
    /// from, carrying the effective User-Agent.
    pub(crate) fn download_config(&self) -> DownloaderConfig {
        DownloaderConfig {
            http: self.http_config(),
            ..DownloaderConfig::default()
        }
    }

    /// Drops every cached service that holds an HTTP client, so the next use
    /// builds it from the current settings.
    fn forget_http_services(&mut self) {
        self.minecraft.take();
        self.languages.take();
        self.options_feed.take();
        self.loaders.take();
        self.mods.take();
        self.curseforge.take();
        self.curseforge_mods.take();
    }

    /// Progress reporting for a metadata fetch.
    ///
    /// Metadata documents are small enough that this operation does not expose
    /// progress. Large artifact downloads use their own reporting sink.
    fn reporter() -> OperationReporter {
        OperationReporter::silent("minecraft-metadata")
    }

    /// The registry, for services inside this crate.
    ///
    /// Private on purpose: exposing it would let a frontend reach the whole
    /// storage API through `App` and re-open the boundary this layer closes.
    pub(crate) fn registry(&self) -> &Registry {
        &self.storage.registry
    }

    /// The registry, for the services in sibling modules ([`crate::launch`],
    /// [`crate::instance`]). Still not reachable from a frontend.
    pub(crate) fn registry_for_services(&self) -> &Registry {
        self.registry()
    }

    /// Moves the persistent instance root.
    ///
    /// The one relocatable root, and the only setting that has to take effect
    /// *within* the running process: the registry holds the resolved paths, so
    /// writing the configuration alone would leave this process creating
    /// instances in the old place until it was restarted. The index is
    /// therefore reopened against the new resolver.
    ///
    /// Instances that already exist do not move. Their roots are absolute and
    /// recorded, so they stay exactly where they are and keep working; the new
    /// root applies to instances created from here on.
    ///
    /// A relative path is made absolute before it is stored. A setting that
    /// resolved against the working directory would point somewhere different
    /// for the GUI, the CLI, and a shell in another directory — three different
    /// instance roots for one configured value.
    #[instrument(skip(self))]
    pub fn set_persistent_instance_root(&mut self, root: Option<&Path>) -> CoreResult<()> {
        let root = root
            .map(|path| {
                std::path::absolute(path).map_err(|source| {
                    ephervex_storage::PathError::DirectoryCreation {
                        path: path.to_path_buf(),
                        source,
                    }
                })
            })
            .transpose()?;

        let setting = root
            .as_deref()
            .map(|path| ephervex_storage::utf8_path(path).map(str::to_owned))
            .transpose()?;
        // Only this one field is overridable, so re-deriving is a replacement
        // rather than a re-resolution of the other roots (see `Paths`).
        //
        // The new root is created and opened before the setting is written: a
        // root that cannot be used must never reach the file, because the next
        // start would fail on it before any window could change it back.
        let paths = self.paths().clone().with_persistent_instance_root(root);
        paths.ensure_base_directories()?;
        let registry = Registry::open(paths)?;

        let updated = Config::update(self.paths(), |config| {
            config.persistent_instance_root = setting.clone().unwrap_or_default();
        })?;
        self.storage.registry = registry;
        self.adopt_config(updated);

        tracing::info!(
            root = %self.paths().persistent_instances_dir().display(),
            "moved the persistent instance root"
        );
        Ok(())
    }

    /// The folders a download watcher checks for a manually placed copy of a
    /// file its provider refused to serve, resolved to the OS Downloads
    /// folder when nothing is configured.
    #[must_use]
    pub fn watched_download_folders(&self) -> Vec<PathBuf> {
        self.config().watched_download_folders()
    }

    /// Replaces the configured watched-folder list.
    ///
    /// `None` returns to the unconfigured default (the OS Downloads folder);
    /// `Some(&[])` explicitly clears it, watching nothing. Each path is made
    /// absolute before it is stored, on the same reasoning
    /// [`Self::set_persistent_instance_root`] does: a relative path would
    /// resolve against whatever directory happened to launch the process.
    #[instrument(skip(self))]
    pub fn set_watched_download_folders(&mut self, folders: Option<&[PathBuf]>) -> CoreResult<()> {
        let absolute = folders
            .map(|folders| {
                folders
                    .iter()
                    .map(|path| {
                        std::path::absolute(path).map_err(|source| {
                            ephervex_storage::PathError::DirectoryCreation {
                                path: path.clone(),
                                source,
                            }
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?;
        let setting = absolute
            .map(|folders| {
                folders
                    .iter()
                    .map(|path| ephervex_storage::utf8_path(path).map(str::to_owned))
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?;
        self.mutate_config(|config| {
            config.watched_download_folders.clone_from(&setting);
        })
    }

    /// Changes a setting and writes the configuration file.
    ///
    /// The single place configuration is mutated, so no service can change a
    /// value in memory and forget to persist it — and so `Config::save`'s
    /// refusal to write a credential-looking key (ADR 0004) is on the path of
    /// every write rather than of most of them.
    ///
    /// The change is applied to the file as it is on disk, so another `App` or
    /// process's change is never reverted, and kept in memory only once the
    /// write succeeds: an in-memory setting that failed to reach disk would
    /// come back on the next run and look like the launcher had ignored the user.
    pub(crate) fn mutate_config(&mut self, change: impl FnOnce(&mut Config)) -> CoreResult<()> {
        let updated = Config::update(self.paths(), change)?;
        self.adopt_config(updated);
        Ok(())
    }

    /// [`Self::mutate_config`] for an async caller: the wait for the
    /// configuration lock, which another process can hold for seconds, happens on
    /// the blocking pool instead of holding a runtime worker.
    ///
    /// The whole update runs there, so the lock is released before this
    /// returns to the runtime. `change` returns a value the caller reads back,
    /// such as whether a record was found.
    pub(crate) async fn mutate_config_off_worker<R: Default + Send + 'static>(
        &mut self,
        change: impl FnOnce(&mut Config) -> R + Send + 'static,
    ) -> CoreResult<R> {
        let paths = self.paths().clone();
        let (updated, result) = crate::blocking::off_worker(move || {
            let mut result = R::default();
            Config::update(&paths, |config| result = change(config))
                .map(|updated| (updated, result))
        })
        .await
        .ok_or_else(|| ephervex_storage::ConfigError::Unwritable {
            path: self.paths().config_file(),
            source: std::io::Error::other("the runtime shut down"),
        })??;
        self.adopt_config(updated);
        Ok(result)
    }

    /// Rereads the configuration file, picking up what another `App` or
    /// process wrote since this one last read or wrote it.
    ///
    /// For a long-lived `App`. On failure the configuration read earlier stays.
    pub fn reload_config(&mut self) -> CoreResult<()> {
        let loaded = Config::load_or_default(self.paths())?;
        self.adopt_config(loaded);
        Ok(())
    }

    /// Holds `config` as current, moving to its persistent instance root when
    /// another writer changed it, as opening storage would.
    ///
    /// A cached session for the account that stops being the default is
    /// dropped, as choosing another default here would.
    fn adopt_config(&mut self, config: Config) {
        let was_default = self.storage.config.active_account().map(str::to_owned);
        if self.session.as_ref().is_some_and(|session| {
            was_default.as_deref() == Some(session.account.id.to_string().as_str())
                && config.active_account() != was_default.as_deref()
        }) {
            self.session = None;
        }

        // Compared with the root in use, so a switch that failed is tried again.
        let paths = config.apply_to(self.paths().clone());
        if paths.persistent_instances_dir() != self.paths().persistent_instances_dir() {
            let opened = paths
                .ensure_base_directories()
                .map_err(ephervex_storage::StorageError::from)
                .and_then(|()| Registry::open(paths).map_err(ephervex_storage::StorageError::from));
            match opened {
                Ok(registry) => self.storage.registry = registry,
                Err(error) => warn!(%error, "kept the persistent instance root opened earlier"),
            }
        }
        // A cached service keeps the User-Agent its client was built with.
        if self.storage.config.api.user_agent() != config.api.user_agent() {
            self.forget_http_services();
        }
        self.storage.config = config;
    }
}

async fn after_successful_validation<T>(
    validation: CoreResult<()>,
    optional_work: impl std::future::Future<Output = T>,
) -> CoreResult<T> {
    validation?;
    Ok(optional_work.await)
}

/// Removes an instance that never reached publication.
///
/// A partial instance is not a reproduction of anything, so it is removed
/// rather than left behind. Failing to remove it is reported and never
/// propagated: the caller already has a real error to return, and the leftover
/// directory carries an incomplete marker that keeps reconciliation from ever
/// presenting it as an instance.
pub(crate) fn discard_unpublished(registry: &Registry, record: &ClientRecord) {
    if let Err(error) = registry.discard_unpublished_client(record) {
        warn!(
            instance_id = %record.id,
            %error,
            "could not remove the instance left behind by a failed creation"
        );
    }
}

fn loader_build_is_available(listing: &LoaderVersionListing, selected: &str) -> bool {
    listing.builds.iter().any(|build| build.version == selected)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use ephervex_model::{LoaderSelection, StorageClass};
    use ephervex_storage::ApplicationColorScheme;

    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn app_rooted_at(base: &std::path::Path) -> App {
        App::open_at(Paths::rooted_at(base)).expect("app should open against a temporary root")
    }

    /// Another process holds the configuration lock for as long as the guard
    /// returned lives.
    fn hold_config_lock_like_another_process(paths: &Paths) -> std::fs::File {
        std::fs::create_dir_all(paths.config_dir()).expect("config dir");
        let held = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(paths.config_dir().join("config.lock"))
            .expect("lock file");
        rustix::fs::flock(&held, rustix::fs::FlockOperation::LockExclusive).expect("flock");
        held
    }

    #[tokio::test]
    async fn a_contended_configuration_lock_does_not_stall_the_runtime() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        let other_process = hold_config_lock_like_another_process(app.paths());

        {
            let update =
                app.mutate_config_off_worker(|config| config.display.show_linked_servers = false);
            tokio::pin!(update);
            crate::blocking::assert_other_tasks_run_while(update.as_mut()).await;

            drop(other_process);
            update.await.expect("written once the lock is free");
        }
        assert!(!app.config().display.show_linked_servers);
    }

    #[test]
    fn changing_the_user_agent_rebuilds_services_that_were_built_with_the_old_one() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        app.minecraft().expect("catalog");
        app.languages().expect("languages");
        app.options_feed().expect("feed");
        app.loaders().expect("loaders");
        app.modrinth_catalog().expect("mods");
        app.curseforge_api().expect("curseforge");
        let services_are_built = |app: &App| {
            [
                app.minecraft.get().is_some(),
                app.languages.get().is_some(),
                app.options_feed.get().is_some(),
                app.loaders.get().is_some(),
                app.mods.get().is_some(),
                app.curseforge.get().is_some(),
            ]
        };
        assert_eq!(services_are_built(&app), [true; 6]);

        app.set_custom_user_agent(Some("BugReportBot/2.0"))
            .expect("set");
        assert_eq!(
            services_are_built(&app),
            [false; 6],
            "a service built with the previous User-Agent must not survive the change"
        );

        app.minecraft().expect("catalog");
        app.set_custom_user_agent(Some("BugReportBot/2.0"))
            .expect("same value");
        assert!(
            app.minecraft.get().is_some(),
            "writing the User-Agent already in effect keeps what was built"
        );
    }

    #[test]
    fn a_user_agent_written_by_another_process_reaches_services_on_reload() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        app.minecraft().expect("catalog");
        let mut other = app_rooted_at(base.path());
        other
            .set_custom_user_agent(Some("BugReportBot/2.0"))
            .expect("set");

        app.reload_config().expect("reload");

        assert!(app.minecraft.get().is_none());
        assert_eq!(app.download_config().http.user_agent, "BugReportBot/2.0");
    }

    #[tokio::test]
    async fn a_managed_java_install_observes_the_cancellation_of_the_operation_that_planned_it() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        // Nothing installed qualifies, so the plan has to install a runtime.
        *app.java.write().expect("inventory cache") = Some(Arc::new(RuntimeInventory {
            runtimes: Vec::new(),
            failures: Vec::new(),
            probed: 0,
            candidates: 0,
            scanned_at: chrono::Utc::now(),
        }));
        let required =
            RequiredJava::from_table(&ephervex_minecraft::MinecraftVersionId::parse("1.21.1"));
        let cancel = CancellationToken::new();
        cancel.cancel();

        let error = app
            .plan_java_for_selection(
                "1.21.1",
                required,
                &LoaderSelection::vanilla(),
                None,
                None,
                &JavaPlanRequest {
                    install_if_missing: true,
                    cancel,
                    ..JavaPlanRequest::default()
                },
            )
            .await
            .expect_err("a canceled operation must not install a runtime");

        assert!(error.is_cancellation(), "{error:?}");
        assert!(
            !app.paths().java_dir().exists()
                || std::fs::read_dir(app.paths().java_dir())
                    .is_ok_and(|mut entries| entries.next().is_none()),
            "nothing may be installed"
        );
    }

    #[test]
    fn watched_download_folders_persist_across_a_reopen_and_absolutize_relative_paths() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());

        app.set_watched_download_folders(Some(&[PathBuf::from("relative/downloads")]))
            .expect("set a relative folder");
        let configured = app.watched_download_folders();
        assert_eq!(configured.len(), 1);
        assert!(
            configured[0].is_absolute(),
            "a relative path must not resolve against whatever directory launched the process"
        );

        let reopened = app_rooted_at(base.path());
        assert_eq!(
            reopened.watched_download_folders(),
            configured,
            "the setting must survive a restart"
        );

        let mut app = reopened;
        app.set_watched_download_folders(Some(&[]))
            .expect("clear the list");
        assert!(
            app.watched_download_folders().is_empty(),
            "an explicit empty list must not fall back to the OS Downloads folder"
        );
    }

    #[test]
    fn failed_target_validation_publishes_no_instance_or_directory() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let request = NewClient::new(
            "Invalid target",
            "1.21.f",
            LoaderSelection::exact(LoaderKind::Forge, "12121212"),
        );
        let validation = Err(MinecraftError::UnknownVersion {
            id: "1.21.f".to_owned(),
        }
        .into());

        let result =
            app.publish_client_after_validation(request, validation, &PreferenceContext::offline());

        assert!(result.is_err());
        assert!(app.list_clients().expect("list").is_empty());
        assert_eq!(
            std::fs::read_dir(app.paths().temporary_instances_dir())
                .expect("temporary instance root")
                .count(),
            0
        );
    }

    #[tokio::test]
    async fn a_client_name_a_rename_would_refuse_is_refused_before_anything_is_created() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let too_long = "n".repeat(crate::instance::MAX_ENTITY_NAME_CHARS + 1);
        let refused = [
            ("   ", crate::NameRefusal::Empty),
            ("two\nlines", crate::NameRefusal::ControlCharacters),
            ("esc\u{1b}[31m", crate::NameRefusal::ControlCharacters),
            (too_long.as_str(), crate::NameRefusal::TooLong),
        ];

        for (name, reason) in refused {
            // An unknown release would fail later, so reaching this error
            // proves the name was checked before the catalog was asked.
            let error = app
                .create_client_reporting(NewClient::new(name, "1.21.f", LoaderSelection::vanilla()))
                .await
                .expect_err("the name is refused");
            assert!(
                matches!(&error, crate::CoreError::EntityNameInvalid { reason: found } if *found == reason),
                "{name:?}: {error:?}"
            );
        }

        assert!(app.list_clients().expect("list").is_empty());
        assert_eq!(
            std::fs::read_dir(app.paths().temporary_instances_dir())
                .expect("temporary instance root")
                .count(),
            0
        );
    }

    #[tokio::test]
    async fn failed_target_validation_does_not_start_optional_metadata_work() {
        let started = AtomicBool::new(false);
        let validation = Err(MinecraftError::UnknownVersion {
            id: "invalid".to_owned(),
        }
        .into());
        let result = after_successful_validation(validation, async {
            started.store(true, Ordering::SeqCst);
        })
        .await;

        assert!(result.is_err());
        assert!(!started.load(Ordering::SeqCst));
    }

    #[test]
    fn exact_loader_validation_requires_a_catalog_build_match() {
        let listing = LoaderVersionListing {
            loader: LoaderKind::Forge,
            minecraft: "1.20.1".to_owned(),
            builds: vec![ephervex_loaders::LoaderBuild {
                version: "47.3.0".to_owned(),
                prerelease: false,
            }],
            latest: Some("47.3.0".to_owned()),
            stable: Some("47.3.0".to_owned()),
            freshness: ephervex_download::Freshness::Cached,
        };

        assert!(loader_build_is_available(&listing, "47.3.0"));
        assert!(!loader_build_is_available(&listing, "12121212"));
    }

    #[test]
    fn the_services_layer_walks_the_instance_lifecycle() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());

        assert!(app.list_clients().expect("list").is_empty());

        let created = app
            .create_client_for_test(NewClient::new(
                "Bug Repro",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create");
        assert_eq!(created.storage, StorageClass::Temporary);

        let fetched = app.client(created.id).expect("fetch");
        assert_eq!(fetched.name, "Bug Repro");
        assert_eq!(app.list_clients().expect("list").len(), 1);

        app.delete_client(created.id, FileDisposition::Remove)
            .expect("delete");
        assert!(app.list_clients().expect("list").is_empty());
    }

    #[test]
    fn instance_runtime_overrides_are_settable_and_clearable() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let created = app
            .create_client_for_test(NewClient::new(
                "Bug Repro",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create");
        assert!(created.java_override.is_none());
        assert!(created.jvm_override.is_none());

        let pinned = app
            .set_client_runtime_settings(
                created.id,
                Some(ephervex_model::JavaOverride::new("/opt/jdk/bin/java")),
                Some(ephervex_model::JvmSettings {
                    min_memory_mib: Some(2048),
                    max_memory_mib: Some(8192),
                    jvm_args: vec!["-XX:+UseZGC".to_owned()],
                }),
            )
            .expect("pin")
            .record;
        assert_eq!(
            pinned
                .java_override
                .as_ref()
                .map(|java| java.path.as_path()),
            Some(std::path::Path::new("/opt/jdk/bin/java"))
        );

        // Reopened, because the point of the setting is that it survives.
        let reopened = app_rooted_at(base.path());
        let stored = reopened.client(created.id).expect("client");
        assert_eq!(
            stored
                .jvm_override
                .as_ref()
                .and_then(|jvm| jvm.max_memory_mib),
            Some(8192)
        );

        // `None` restores the global setting rather than pinning today's value.
        let cleared = reopened
            .set_client_runtime_settings(created.id, None, None)
            .expect("clear")
            .record;
        assert!(cleared.java_override.is_none());
        assert!(cleared.jvm_override.is_none());
    }

    #[test]
    fn a_client_setting_change_keeps_a_committed_rename_the_index_row_lags_behind() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let created = app
            .create_client_for_test(NewClient::new(
                "indexed",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create");
        let mut renamed = created.clone();
        renamed.name = "renamed".to_owned();
        ephervex_storage::metadata::write_client_metadata(&created.root, &renamed)
            .expect("canonical write");
        assert_eq!(app.client(created.id).expect("row").name, "indexed");

        let changed = app
            .set_allow_pack_mod_version_changes(created.id, true)
            .expect("setting")
            .record;

        assert_eq!(changed.name, "renamed");
        assert!(changed.allow_pack_mod_version_changes);
        assert_eq!(app.client(created.id).expect("row").name, "renamed");
    }

    /// A client whose canonical metadata holds a newer loader build and Java
    /// override than its index row, as after an index write failed behind a
    /// committed update.
    fn client_with_lagging_index_row(app: &App) -> (ClientRecord, ClientRecord) {
        let created = app
            .create_client_for_test(NewClient::new(
                "lagging",
                "1.21.1",
                LoaderSelection::exact(ephervex_model::LoaderKind::NeoForge, "21.1.100"),
            ))
            .expect("create");
        let mut committed = created.clone();
        committed.loader = LoaderSelection::exact(ephervex_model::LoaderKind::NeoForge, "21.1.172");
        committed.java_override = Some(ephervex_model::JavaOverride::new("/opt/jdk/bin/java"));
        ephervex_storage::metadata::write_client_metadata(&created.root, &committed)
            .expect("canonical write");
        let row = app.client(created.id).expect("row");
        assert_eq!(row.loader.version.as_deref(), Some("21.1.100"));
        (committed, row)
    }

    #[test]
    fn the_current_client_is_the_committed_record_while_the_index_row_lags() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let (committed, _row) = client_with_lagging_index_row(&app);

        let current = app.current_client(committed.id).expect("current");

        assert_eq!(current.loader, committed.loader);
        assert_eq!(current.java_override, committed.java_override);
    }

    #[test]
    fn a_java_plan_for_a_client_layers_the_committed_loader_and_overrides() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let (committed, _row) = client_with_lagging_index_row(&app);

        let (loader, java_override, jvm_override) = app
            .java_target_selection(JavaTarget::Client(committed.id))
            .expect("selection");

        assert_eq!(loader, committed.loader);
        assert_eq!(java_override, committed.java_override);
        assert_eq!(jvm_override, committed.jvm_override);
    }

    #[test]
    fn an_instance_override_whose_layered_heap_range_is_invalid_is_refused_and_not_stored() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let created = app
            .create_client_for_test(NewClient::new(
                "Bug Repro",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create");

        // The default global range is 512..4096 MiB.
        let error = app
            .set_client_runtime_settings(
                created.id,
                None,
                Some(ephervex_model::JvmSettings {
                    min_memory_mib: Some(8192),
                    ..ephervex_model::JvmSettings::default()
                }),
            )
            .expect_err("a minimum above the global maximum cannot launch");

        assert!(
            matches!(
                error,
                crate::CoreError::JavaSettingsInvalid {
                    minimum_mib: 8192,
                    maximum_mib: 4096,
                    minimum_layer: ephervex_java::JvmSettingLayer::Override,
                    maximum_layer: ephervex_java::JvmSettingLayer::Global,
                }
            ),
            "{error:?}"
        );
        assert!(
            app.client(created.id)
                .expect("client")
                .jvm_override
                .is_none()
        );
    }

    #[tokio::test]
    async fn planning_refuses_a_pinned_minimum_that_a_lowered_global_maximum_left_invalid() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        let pinned = ephervex_model::JvmSettings {
            min_memory_mib: Some(2048),
            ..ephervex_model::JvmSettings::default()
        };
        app.validate_jvm_override(Some(&pinned))
            .expect("valid against the default global range");

        app.set_java_settings(&GlobalJavaSettings {
            max_memory_mib: 1024,
            ..app.java_settings()
        })
        .expect("a lower global maximum is valid on its own");

        let required =
            RequiredJava::from_table(&ephervex_minecraft::MinecraftVersionId::parse("1.21.1"));
        let error = app
            .plan_java_for_selection(
                "1.21.1",
                required,
                &LoaderSelection::vanilla(),
                None,
                Some(pinned),
                &JavaPlanRequest::default(),
            )
            .await
            .expect_err("the stored override no longer yields a launchable range");

        assert!(
            matches!(
                error,
                crate::CoreError::JavaSettingsInvalid {
                    minimum_mib: 2048,
                    maximum_mib: 1024,
                    minimum_layer: ephervex_java::JvmSettingLayer::Override,
                    maximum_layer: ephervex_java::JvmSettingLayer::Global,
                }
            ),
            "{error:?}"
        );
    }

    #[test]
    fn a_relative_instance_java_path_is_stored_as_an_absolute_one() {
        // A path that resolved against the working directory would launch a
        // different Java, or none, from every other directory.
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let created = app
            .create_client_for_test(NewClient::new(
                "Bug Repro",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create");

        let relative = std::path::Path::new("jdk-17/bin/java");
        let pinned = app
            .set_client_runtime_settings(
                created.id,
                Some(ephervex_model::JavaOverride::new(relative)),
                None,
            )
            .expect("pin")
            .record;

        let stored = pinned.java_override.expect("a pinned java").path;
        assert!(stored.is_absolute(), "stored as `{}`", stored.display());
        assert!(
            stored.ends_with(relative),
            "stored as `{}`",
            stored.display()
        );
        let reopened = app_rooted_at(base.path())
            .client(created.id)
            .expect("client");
        assert_eq!(reopened.java_override.map(|java| java.path), Some(stored));
    }

    /// The setting decides what the mod list offers, and what it unlocks is
    /// written into `mods/` for the next start to read. A running copy is a
    /// reason to restart the game, never a reason to refuse the decision.
    #[test]
    fn allowing_pack_mod_version_changes_persists_and_is_available_while_running() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let created = app
            .create_client_for_test(NewClient::new(
                "Bug Repro",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create");
        // Nothing declared this instance's mods but the person making it, so
        // there is no declaration to preserve and the mods are theirs.
        assert!(created.allow_pack_mod_version_changes);

        let updated = app
            .set_allow_pack_mod_version_changes(created.id, false)
            .expect("opt out")
            .record;
        assert!(!updated.allow_pack_mod_version_changes);

        // Reopened, because the point of the setting is that it survives.
        let reopened = app_rooted_at(base.path());
        let stored = reopened.client(created.id).expect("client");
        assert!(!stored.allow_pack_mod_version_changes);

        let _lock = ephervex_storage::LaunchLock::acquire(&stored.root).expect("acquire lock");
        let running = reopened
            .set_allow_pack_mod_version_changes(created.id, true)
            .expect("a running copy does not take the decision away")
            .record;
        assert!(running.allow_pack_mod_version_changes);
    }

    /// An imported instance's mod list is a record of what the pack shipped
    /// until its owner says otherwise.
    #[test]
    fn an_imported_instance_starts_out_preserving_the_packs_declaration() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let created = app
            .create_client_for_test(
                NewClient::new("Imported", "1.20.1", LoaderSelection::vanilla()).with_source(
                    ephervex_storage::ImportSource::imported(
                        ephervex_model::ImportSourceKind::ModrinthPack,
                        "report.mrpack",
                    ),
                ),
            )
            .expect("create");
        assert!(!created.allow_pack_mod_version_changes);
    }

    /// A linked server exists to reproduce one client. Left behind it is
    /// indistinguishable from a standalone one, which is how a launcher
    /// accumulates servers nobody can account for.
    #[test]
    fn deleting_a_client_takes_the_servers_linked_to_it() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let client = repro_client(&app);
        let server = linked_server(&app, base.path(), client.id, "Repro Server");
        assert_eq!(app.linked_server_ids(client.id).expect("linked").len(), 1);

        let deleted = app
            .delete_client(client.id, FileDisposition::Remove)
            .expect("delete");

        assert_eq!(
            deleted.iter().map(|record| record.id).collect::<Vec<_>>(),
            vec![server.id],
            "the caller is told which servers went with the client"
        );
        assert!(app.list_clients().expect("clients").is_empty());
        assert!(
            app.list_servers().expect("servers").is_empty(),
            "a linked server must not outlive the client it reproduces"
        );
        assert!(!server.root.exists(), "its files follow the same choice");
    }

    /// A running game keeps its directory, its lock, and its linked servers.
    #[test]
    fn a_running_client_refuses_deletion_and_keeps_everything() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let client = repro_client(&app);
        let server = linked_server(&app, base.path(), client.id, "Repro Server");
        let _lock = ephervex_storage::LaunchLock::acquire(&client.root).expect("acquire lock");

        let error = app
            .delete_client(client.id, FileDisposition::Remove)
            .expect_err("a running instance cannot be deleted underneath itself");

        assert!(
            matches!(error, crate::CoreError::InstanceRunning { .. }),
            "expected an already-running refusal, got {error}"
        );
        assert_eq!(app.list_clients().expect("clients").len(), 1);
        assert!(client.root.exists(), "the game directory stays");
        assert_eq!(app.linked_server_ids(client.id).expect("linked").len(), 1);
        assert!(server.root.exists(), "the linked server stays");
        assert!(app.launch_state(client.id).expect("state").is_some());
    }

    #[test]
    fn a_deletion_is_refused_while_another_operation_holds_the_instance_or_a_linked_server() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let client = repro_client(&app);
        let server = linked_server(&app, base.path(), client.id, "Bug Server");
        let delete = || {
            app.delete_client(client.id, FileDisposition::Remove)
                .expect_err("something else holds it")
        };

        let operation = app
            .registry()
            .claim_host(ephervex_storage::RecordRef::Client(client.id))
            .expect("claim");
        assert_eq!(delete().message_id(), "error-registry-host-busy");
        drop(operation);

        let start = app
            .registry()
            .claim_host(ephervex_storage::RecordRef::Server(server.id))
            .expect("claim");
        assert_eq!(delete().message_id(), "error-registry-host-busy");
        drop(start);

        assert!(client.root.exists(), "nothing was removed");
        assert!(server.root.exists(), "the linked server stays");
        assert_eq!(app.list_clients().expect("clients").len(), 1);
    }

    /// A copy taken while the game writes its world would mix files from
    /// different moments, so a launching or running original is not copied,
    /// whether the lock is the first launch or an additional one.
    #[test]
    fn a_running_client_refuses_duplication_and_creates_no_copy() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let client = repro_client(&app);
        let roots_before = std::fs::read_dir(client.root.parent().expect("instances dir"))
            .expect("list instances dir")
            .count();

        let first = ephervex_storage::LaunchLock::acquire(&client.root).expect("acquire lock");
        let additional = ephervex_storage::LaunchLock::acquire_additional(&client.root, "bob")
            .expect("a second account");
        let refuse = || {
            let error = app
                .duplicate_client(
                    client.id,
                    &ephervex_storage::DuplicateClient::default(),
                    ephervex_model::AbsentModPolicy::Keep,
                )
                .expect_err("a running instance cannot be duplicated");
            assert!(
                matches!(error, crate::CoreError::InstanceRunning { .. }),
                "expected an already-running refusal, got {error}"
            );
        };
        refuse();
        drop(first);
        refuse();
        drop(additional);

        assert_eq!(app.list_clients().expect("clients").len(), 1);
        let roots_after = std::fs::read_dir(client.root.parent().expect("instances dir"))
            .expect("list instances dir")
            .count();
        assert_eq!(roots_before, roots_after, "no copy directory was created");
        assert!(client.root.exists(), "the original is untouched");
    }

    /// One running linked server keeps every other one, not only itself. One
    /// without a supervisor to stop it refuses even a deletion that stops servers.
    #[tokio::test]
    async fn a_running_linked_server_refuses_deletion_before_any_server_is_removed() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let client = repro_client(&app);
        let stopped = linked_server(&app, base.path(), client.id, "A Server");
        let running = linked_server(&app, base.path(), client.id, "B Server");
        let evidence =
            ephervex_server::RuntimeEvidence::for_current_supervisor().expect("runtime evidence");
        ephervex_server::write_runtime_evidence(&running.root, &evidence).expect("mark running");

        let refusals = [
            app.delete_client(client.id, FileDisposition::Remove),
            app.delete_client_stopping_linked_servers(client.id, FileDisposition::Remove)
                .await,
        ];

        for refusal in refusals {
            let error = refusal.expect_err("a running linked server refuses the deletion");
            assert!(
                matches!(
                    &error,
                    crate::CoreError::LinkedServerUnstoppable { name } if name == "B Server"
                ),
                "expected the running server to be named, got {error}"
            );
        }
        assert_eq!(app.list_clients().expect("clients").len(), 1);
        assert_eq!(app.list_servers().expect("servers").len(), 2);
        assert!(stopped.root.exists(), "the stopped server stays");
        assert!(running.root.exists(), "the running server stays");
    }

    /// Only a kill ends this server, so its deletion cannot wait on a clean stop.
    #[tokio::test]
    async fn deleting_files_kills_a_running_linked_server_and_removes_everything() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let client = repro_client(&app);
        let server = linked_server(&app, base.path(), client.id, "Repro Server");
        let supervisor = supervise(&server, "while IFS= read -r line; do :; done").await;

        app.delete_client_stopping_linked_servers(client.id, FileDisposition::Remove)
            .await
            .expect("delete");

        let exit = supervisor.await.expect("join").expect("supervised");
        assert!(exit.signal.is_some(), "the server was killed, got {exit:?}");
        assert!(app.list_clients().expect("clients").is_empty());
        assert!(app.list_servers().expect("servers").is_empty());
        assert!(!client.root.exists());
        assert!(!server.root.exists());
    }

    /// Kept files are a world someone may open again, so it is saved on the way out.
    #[tokio::test]
    async fn keeping_files_stops_a_running_linked_server_cleanly() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let client = repro_client(&app);
        let server = linked_server(&app, base.path(), client.id, "Repro Server");
        let supervisor = supervise(
            &server,
            "while IFS= read -r line; do [ \"$line\" = stop ] && exit 0; done",
        )
        .await;

        app.delete_client_stopping_linked_servers(client.id, FileDisposition::Keep)
            .await
            .expect("delete");

        let exit = supervisor.await.expect("join").expect("supervised");
        assert_eq!(
            exit.code,
            Some(0),
            "the server stopped itself, got {exit:?}"
        );
        assert!(app.list_clients().expect("clients").is_empty());
        assert!(app.list_servers().expect("servers").is_empty());
        assert!(client.root.exists());
        assert!(server.root.exists());
    }

    /// A caller whose runtime is ending saves the world rather than killing it.
    #[tokio::test]
    async fn stopping_or_killing_a_supervised_server_stops_it_cleanly_first() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let client = repro_client(&app);
        let server = linked_server(&app, base.path(), client.id, "Repro Server");
        let supervisor = supervise(
            &server,
            "while IFS= read -r line; do [ \"$line\" = stop ] && exit 0; done",
        )
        .await;

        app.stop_server_or_kill(server.id).await.expect("stopped");

        let exit = supervisor.await.expect("join").expect("supervised");
        assert_eq!(
            exit.code,
            Some(0),
            "the server stopped itself, got {exit:?}"
        );
        assert!(exit.stop_requested, "the exit records the stop");
    }

    /// A deletion that is refused anyway has no reason to take the server down.
    #[tokio::test]
    async fn a_running_client_refuses_before_any_linked_server_is_stopped() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let client = repro_client(&app);
        let server = linked_server(&app, base.path(), client.id, "Repro Server");
        let _supervisor = supervise(&server, "while IFS= read -r line; do :; done").await;
        let _lock = ephervex_storage::LaunchLock::acquire(&client.root).expect("acquire lock");

        let error = app
            .delete_client_stopping_linked_servers(client.id, FileDisposition::Remove)
            .await
            .expect_err("a running instance cannot be deleted underneath itself");

        assert!(
            matches!(error, crate::CoreError::InstanceRunning { .. }),
            "expected an already-running refusal, got {error}"
        );
        assert!(matches!(
            ephervex_server::classify_runtime(&server.root),
            Ok(ephervex_server::RuntimeClassification::Controlled { .. })
        ));
        ephervex_server::send_control(&server.root, &ephervex_server::ControlAction::Kill)
            .await
            .expect("clean up the server");
    }

    /// Runs `script` as the server's process under a real supervisor.
    async fn supervise(
        server: &ServerRecord,
        script: &str,
    ) -> tokio::task::JoinHandle<ephervex_server::ServerResult<ephervex_server::ServerExit>> {
        let root = server.root.clone();
        let capture = root.join("capture-for-test");
        std::fs::create_dir_all(root.join("server")).expect("server directory");
        let command = ephervex_server::ServerCommand {
            java: PathBuf::from("/bin/sh"),
            arguments: vec!["-c".to_owned(), script.to_owned()],
            working_directory: root.join("server"),
        };
        let supervisor = tokio::spawn(async move {
            ephervex_server::ServerSupervisor::run(
                &command,
                &root,
                &capture,
                &ephervex_server::ServerObserver::silent(),
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if matches!(
                    ephervex_server::classify_runtime(&server.root),
                    Ok(ephervex_server::RuntimeClassification::Controlled { .. })
                ) {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("controlled runtime");
        supervisor
    }

    fn repro_client(app: &App) -> ClientRecord {
        app.create_client_for_test(NewClient::new(
            "Bug Repro",
            "1.20.1",
            LoaderSelection::vanilla(),
        ))
        .expect("create")
    }

    fn linked_server(
        app: &App,
        base: &std::path::Path,
        client: InstanceId,
        name: &str,
    ) -> ServerRecord {
        let staging = base.join(format!("staged-{name}"));
        std::fs::create_dir_all(&staging).expect("staging");
        let server = app
            .registry_for_services()
            .prepare_server(ephervex_storage::NewServer::linked(name, client))
            .expect("prepare");
        app.registry_for_services()
            .publish_staged_server(&server, &staging)
            .expect("publish");
        server
    }

    #[test]
    fn reconciliation_is_reachable_without_touching_the_registry_directly() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());

        let created = app
            .create_client_for_test(NewClient::new(
                "Doomed",
                "1.21.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create");

        // Stand in for an OS temp cleanup.
        std::fs::remove_dir_all(&created.root).expect("remove the instance root");

        let report = app.reconcile().expect("reconcile");
        assert_eq!(report.removed_stale_temporary.len(), 1);
        assert!(app.list_clients().expect("list").is_empty());
    }

    #[test]
    fn moving_the_persistent_instance_root_takes_effect_without_a_restart() {
        // The registry holds the resolved paths, so writing the setting alone
        // would leave this process creating instances in the old place
        let base = tempfile::tempdir().expect("temp dir");
        let elsewhere = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());

        let before = app
            .create_client_for_test(
                NewClient::new("before", "1.20.1", LoaderSelection::vanilla())
                    .with_storage(StorageClass::Persistent),
            )
            .expect("create");
        assert!(before.root.starts_with(base.path()));

        app.set_persistent_instance_root(Some(elsewhere.path()))
            .expect("move the root");

        let after = app
            .create_client_for_test(
                NewClient::new("after", "1.20.1", LoaderSelection::vanilla())
                    .with_storage(StorageClass::Persistent),
            )
            .expect("create");
        assert!(after.root.starts_with(elsewhere.path()));
        assert!(app.paths().persistent_instance_root_is_overridden());

        // The instance created before the move stays where it is and stays
        // readable: its root is absolute and recorded.
        assert_eq!(app.client(before.id).expect("read").root, before.root);
        assert!(before.root.is_dir());
        assert_eq!(app.list_clients().expect("list").len(), 2);

        // The setting survives the process, which is what a user expects of a
        // settings screen.
        let reopened = app_rooted_at(base.path());
        assert_eq!(
            reopened.paths().persistent_instances_dir(),
            elsewhere.path()
        );

        //...and clearing it puts new instances back under XDG.
        let mut app = reopened;
        app.set_persistent_instance_root(None).expect("clear");
        assert!(!app.paths().persistent_instance_root_is_overridden());
    }

    #[test]
    fn an_instance_root_that_cannot_be_created_is_neither_saved_nor_applied() {
        let base = tempfile::tempdir().expect("temp dir");
        let elsewhere = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        app.set_persistent_instance_root(Some(elsewhere.path()))
            .expect("move the root");
        let configured = app.config().persistent_instance_root.clone();
        let in_use = app.paths().persistent_instances_dir().clone();

        // A directory cannot be created below a regular file, whatever the
        // permissions of the user running the test.
        let file = base.path().join("not-a-directory");
        std::fs::write(&file, b"").expect("write a regular file");

        app.set_persistent_instance_root(Some(&file.join("instances")))
            .expect_err("a root below a regular file cannot be created");

        assert_eq!(app.config().persistent_instance_root, configured);
        assert_eq!(app.paths().persistent_instances_dir(), in_use);
        let reopened = app_rooted_at(base.path());
        assert_eq!(reopened.config().persistent_instance_root, configured);
        assert_eq!(reopened.paths().persistent_instances_dir(), in_use);
    }

    #[cfg(unix)]
    fn non_utf8_path(base: &std::path::Path) -> PathBuf {
        use std::os::unix::ffi::OsStrExt as _;
        base.join(std::ffi::OsStr::from_bytes(b"d\xff"))
    }

    #[cfg(unix)]
    #[test]
    fn a_persistent_instance_root_that_is_not_utf8_is_refused_and_not_saved() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        let configured = app.config().persistent_instance_root.clone();
        let in_use = app.paths().persistent_instances_dir();
        let bad = non_utf8_path(base.path());

        let error = app
            .set_persistent_instance_root(Some(&bad))
            .expect_err("a root with no text form is refused");

        assert_eq!(error.message_id(), "error-paths-not-utf8");
        assert_eq!(error.failing_path(), Some(bad.clone()));
        assert!(!bad.exists(), "the refused root must not be created");
        assert_eq!(app.config().persistent_instance_root, configured);
        assert_eq!(app.paths().persistent_instances_dir(), in_use);
        let reopened = app_rooted_at(base.path());
        assert_eq!(reopened.config().persistent_instance_root, configured);
    }

    #[cfg(unix)]
    #[test]
    fn a_preferred_java_path_that_is_not_utf8_is_refused_and_not_saved() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        let before = app.java_settings();
        let bad = non_utf8_path(base.path());

        let error = app
            .set_java_settings(&GlobalJavaSettings {
                preferred_path: Some(bad.clone()),
                max_memory_mib: before.max_memory_mib + 512,
                ..before.clone()
            })
            .expect_err("a Java path with no text form is refused");

        assert_eq!(error.message_id(), "error-paths-not-utf8");
        assert_eq!(error.failing_path(), Some(bad));
        assert_eq!(app.java_settings(), before, "no part of the change applies");
        assert_eq!(app_rooted_at(base.path()).java_settings(), before);
    }

    #[cfg(unix)]
    #[test]
    fn a_watched_download_folder_that_is_not_utf8_is_refused_and_not_saved() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        app.set_watched_download_folders(Some(&[PathBuf::from("/srv/downloads")]))
            .expect("set a valid folder");
        let configured = app.watched_download_folders();
        let bad = non_utf8_path(base.path());

        let error = app
            .set_watched_download_folders(Some(&[PathBuf::from("/srv/other"), bad.clone()]))
            .expect_err("a folder with no text form is refused");

        assert_eq!(error.message_id(), "error-paths-not-utf8");
        assert_eq!(error.failing_path(), Some(bad));
        assert_eq!(app.watched_download_folders(), configured);
        assert_eq!(
            app_rooted_at(base.path()).watched_download_folders(),
            configured
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_instance_java_override_that_is_not_utf8_is_refused_and_not_saved() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let client = app
            .create_client_for_test(NewClient::new("java", "1.20.1", LoaderSelection::vanilla()))
            .expect("create");
        let bad = non_utf8_path(base.path());

        let error = app
            .set_client_runtime_settings(
                client.id,
                Some(ephervex_model::JavaOverride::new(&bad)),
                None,
            )
            .expect_err("a Java path with no text form is refused");

        assert_eq!(error.message_id(), "error-paths-not-utf8");
        assert_eq!(app.client(client.id).expect("read").java_override, None);
    }

    #[test]
    fn favorite_minecraft_versions_persist_in_the_order_they_were_given() {
        let base = tempfile::tempdir().expect("temp dir");
        {
            let mut app = app_rooted_at(base.path());
            assert!(app.favorite_minecraft_versions().is_empty());
            app.set_favorite_minecraft_versions([
                "1.21.1".to_owned(),
                "1.12.2".to_owned(),
                // A blank entry names no release, and a repeat is already
                // expressed by its first appearance.
                "  ".to_owned(),
                "1.21.1".to_owned(),
            ])
            .expect("record");
            assert_eq!(app.favorite_minecraft_versions(), ["1.21.1", "1.12.2"]);
        }
        let reopened = app_rooted_at(base.path());
        assert_eq!(reopened.favorite_minecraft_versions(), ["1.21.1", "1.12.2"]);
    }

    #[test]
    fn a_favorite_is_kept_verbatim_and_never_checked_against_a_catalog() {
        // Offline, and against a release id nothing would resolve. A favorite
        // records what the user works with, not what a manifest currently says.
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        app.set_favorite_minecraft_versions(["1.99.9".to_owned()])
            .expect("record");
        assert_eq!(app.favorite_minecraft_versions(), ["1.99.9"]);
    }

    #[test]
    fn application_display_settings_persist_without_managing_minecraft_preferences() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        assert_eq!(
            app.application_display_settings(),
            ApplicationDisplaySettings {
                text_scale: ApplicationTextScale::Default,
                technical_text_size: TechnicalTextSize::Default,
                theme: ApplicationTheme::BuiltIn(ApplicationColorScheme::Dark),
                show_linked_servers: false,
            }
        );

        app.set_application_display_settings(
            ApplicationTextScale::Large,
            TechnicalTextSize::Comfortable,
            &ApplicationTheme::BuiltIn(ApplicationColorScheme::Light),
        )
        .expect("save display settings");
        assert!(app.config().minecraft_preferences.is_empty());

        let reopened = app_rooted_at(base.path());
        assert_eq!(
            reopened.application_display_settings(),
            ApplicationDisplaySettings {
                text_scale: ApplicationTextScale::Large,
                technical_text_size: TechnicalTextSize::Comfortable,
                theme: ApplicationTheme::BuiltIn(ApplicationColorScheme::Light),
                show_linked_servers: false,
            }
        );
    }

    /// The settings page saves every display choice together, so changing the
    /// text size must not replace a theme a newer build recorded.
    #[test]
    fn saving_display_settings_keeps_a_theme_this_build_does_not_know() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        app.mutate_config(|config| "high_contrast".clone_into(&mut config.display.color_scheme))
            .expect("record a newer scheme");
        assert_eq!(
            app.application_display_settings().theme,
            ApplicationTheme::default()
        );

        app.set_application_display_settings(
            ApplicationTextScale::Large,
            TechnicalTextSize::Default,
            &ApplicationTheme::default(),
        )
        .expect("save display settings");

        let reopened = app_rooted_at(base.path());
        assert_eq!(reopened.config().display.color_scheme, "high_contrast");
        assert_eq!(
            reopened.application_display_settings().text_scale,
            ApplicationTextScale::Large
        );
    }

    #[test]
    fn linked_server_visibility_and_operator_defaults_persist_additively() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        app.set_show_linked_servers(true).expect("visibility");
        app.set_server_operator_defaults(
            false,
            OperatorPermissionLevel::from_value(2).expect("level"),
        )
        .expect("operator defaults");

        let reopened = app_rooted_at(base.path());
        assert!(reopened.application_display_settings().show_linked_servers);
        assert_eq!(
            reopened.server_application_settings(),
            ServerApplicationSettings {
                make_default_account_operator: false,
                operator_permission_level: OperatorPermissionLevel::from_value(2).expect("level"),
            }
        );
        assert!(reopened.config().minecraft_preferences.is_empty());
    }

    #[test]
    fn persistent_storage_settings_distinguish_default_override_and_effective_root() {
        let base = tempfile::tempdir().expect("temp dir");
        let custom = tempfile::tempdir().expect("custom root");
        let mut app = app_rooted_at(base.path());
        let defaults = app.persistent_storage_settings();
        assert!(defaults.configured_root.is_none());
        assert_eq!(defaults.effective_root, defaults.default_root);

        app.set_persistent_instance_root(Some(custom.path()))
            .expect("set root");
        let overridden = app.persistent_storage_settings();
        assert_eq!(overridden.configured_root.as_deref(), Some(custom.path()));
        assert_eq!(overridden.effective_root, custom.path());
        assert_eq!(overridden.default_root, defaults.default_root);

        app.set_persistent_instance_root(None).expect("reset root");
        assert_eq!(app.persistent_storage_settings(), defaults);
    }

    #[test]
    fn a_relative_instance_root_is_stored_as_an_absolute_one() {
        // A setting that resolved against the working directory would mean a
        // different place for the GUI, the CLI, and a shell somewhere else.
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());

        // Resolution is against the working directory by definition, so the
        // target has to be *in* it. A `TempDir` there takes itself away again,
        // including if this test fails.
        let working = tempfile::TempDir::new_in(".").expect("temp dir in the working directory");
        let relative = std::path::Path::new(
            working
                .path()
                .file_name()
                .expect("the temporary directory has a name"),
        )
        .join("instances");

        app.set_persistent_instance_root(Some(&relative))
            .expect("set");

        let stored = &app.config().persistent_instance_root;
        assert!(
            std::path::Path::new(stored).is_absolute(),
            "stored as `{stored}`"
        );
        assert!(
            stored.ends_with(&relative.to_string_lossy().into_owned()),
            "stored as `{stored}`"
        );
        assert!(app.paths().persistent_instances_dir().is_absolute());
    }

    #[test]
    fn configuration_and_paths_are_readable_through_the_services_layer() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());

        assert_eq!(
            app.config().schema_version,
            ephervex_storage::CONFIG_SCHEMA_VERSION
        );
        assert!(app.paths().config_file().starts_with(base.path()));
    }
}
