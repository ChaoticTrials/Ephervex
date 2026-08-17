//! The application services both frontends call.
//!
//! This is the application-service seam: the GUI and CLI depend on
//! `ephervex-core` and nothing below it, so provider crates can be reshaped
//! without either frontend noticing. Anything a frontend needs to *do* — as
//! opposed to merely display — belongs here.
//!
//! Keeping it honest matters more than it looks. A frontend that reaches past
//! this layer into a provider crate compiles perfectly well; the boundary is
//! only real for as long as nothing crosses it. The rule is therefore
//! structural: `ephervex-cli` and `ephervex-gui` do not list any provider crate
//! in their manifests, so a shortcut is a manifest change someone has to
//! justify rather than an import someone can add absent-mindedly.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

use crate::instance::{CreatedClient, PreferenceContext};
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
    ApplicationTextScale, ClientRecord, Config, FileDisposition, JavaConfig, NewClient, Paths,
    ReconciliationReport, Registry, ServerRecord, Storage, TechnicalTextSize,
};
use tokio_util::sync::CancellationToken;
use tracing::{instrument, warn};

use crate::error::CoreResult;

/// What a caller wants a Java plan for, beyond the Minecraft version.
///
/// The default — no instance, no explicit path, no installing — is "what would
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

/// Ephervex interface display preferences exposed to frontends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplicationDisplaySettings {
    pub text_scale: ApplicationTextScale,
    pub technical_text_size: TechnicalTextSize,
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
    /// operations that set it already take `&mut self` — see
    /// [`crate::account`].
    pub(crate) session: Option<ephervex_auth::Session>,
    /// Whether this application may talk plaintext HTTP.
    ///
    /// The field exists only in a test build, so a release binary has no way to
    /// reach the relaxed transport at all: the strict production default is not
    /// a setting anything can change, it is the only behavior compiled in. Set
    /// by [`Self::use_loopback_catalog`] so an integration test can drive the
    /// whole stack — catalog, downloads, verification, publication — against a
    /// local server instead of a production one.
    #[cfg(test)]
    pub(crate) loopback_http: bool,
}

impl App {
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
            java: RwLock::new(None),
            session: None,
            #[cfg(test)]
            loopback_http: false,
        })
    }

    /// Opens the application against explicit roots.
    ///
    /// Exists so tests — and, later, a `--data-dir` style override — can point
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
            java: RwLock::new(None),
            session: None,
            #[cfg(test)]
            loopback_http: false,
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
    pub const fn application_display_settings(&self) -> ApplicationDisplaySettings {
        ApplicationDisplaySettings {
            text_scale: self.storage.config.display.text_scale,
            technical_text_size: self.storage.config.display.technical_text_size,
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
    ) -> CoreResult<()> {
        self.mutate_config(|config| {
            config.display.text_scale = text_scale;
            config.display.technical_text_size = technical_text_size;
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
    /// A vanished temporary root is removed silently — that is what an OS temp
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
    /// far worse trade — and leaving it half-created would be worse still.
    #[instrument(skip(self, request))]
    pub async fn create_client_reporting(&self, request: NewClient) -> CoreResult<CreatedClient> {
        let validation = self.validate_client_target(&request).await;
        let preferences = after_successful_validation(
            validation,
            self.preference_context(&request.minecraft_version),
        )
        .await?;
        self.publish_client(request, &preferences)
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

        let selected = request.loader.version.as_deref().unwrap_or_default();
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
        self.publish_client(request, context)
    }

    fn publish_client(
        &self,
        request: NewClient,
        context: &PreferenceContext,
    ) -> CoreResult<CreatedClient> {
        let record = self.registry().create_client(request)?;
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
        Ok(CreatedClient {
            record,
            preferences,
        })
    }

    #[cfg(test)]
    pub(crate) fn create_client_for_test(&self, request: NewClient) -> CoreResult<ClientRecord> {
        Ok(self
            .publish_client(request, &PreferenceContext::offline())?
            .record)
    }

    /// Fetches one client instance.
    pub fn client(&self, id: InstanceId) -> CoreResult<ClientRecord> {
        Ok(self.registry().client(id)?)
    }

    /// Fetches one server.
    pub fn server(&self, id: ServerId) -> CoreResult<ServerRecord> {
        Ok(self.registry().server(id)?)
    }

    /// Lists every indexed client instance.
    pub fn list_clients(&self) -> CoreResult<Vec<ClientRecord>> {
        Ok(self.registry().list_clients()?)
    }

    /// Deletes a client instance.
    ///
    /// `files` is explicit rather than inferred: removing a user's game
    /// directory is not something to decide on their behalf from context.
    #[instrument(skip(self))]
    pub fn delete_client(&self, id: InstanceId, files: FileDisposition) -> CoreResult<()> {
        Ok(self.registry().delete_client(id, files)?)
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
        self.with_artifact_cache(Box::pin(self.install_server_version_holding_the_cache(
            minecraft, loader, java, target, cancel,
        )))
        .await
    }

    async fn install_server_version_holding_the_cache(
        &self,
        minecraft: &str,
        loader: &ephervex_model::LoaderSelection,
        java: &Path,
        target: &Path,
        cancel: &CancellationToken,
    ) -> CoreResult<InstalledServer> {
        let parent = self.minecraft_version(minecraft).await?;
        let reporter = OperationReporter::silent("installing-loader-server");
        Ok(self
            .loaders()?
            .provider(loader.kind)
            .install_server(ServerInstallRequest {
                minecraft,
                exact_loader: loader.version.as_deref(),
                parent: &parent,
                java,
                target,
                reporter: &reporter,
                cancel,
            })
            .await?)
    }

    // -- java runtimes -----------------------------------------------------

    /// Every Java runtime installed on this machine, from the process cache.
    ///
    /// The first call scans; later calls return what it found. Scanning forks a
    /// JVM per distinct runtime, so a frontend may call this as often as it
    /// likes — which is the point of caching it here rather than asking every
    /// caller to remember.
    #[instrument(skip(self))]
    pub async fn java_runtimes(&self) -> CoreResult<Arc<RuntimeInventory>> {
        if let Some(inventory) = self.cached_java() {
            return Ok(inventory);
        }
        self.refresh_java_runtimes().await
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
        if settings.min_memory_mib == 0
            || settings.max_memory_mib == 0
            || settings.min_memory_mib > settings.max_memory_mib
        {
            return Err(crate::CoreError::JavaSettingsInvalid {
                minimum_mib: settings.min_memory_mib,
                maximum_mib: settings.max_memory_mib,
            });
        }
        self.mutate_config(|config| {
            config.java.auto_detect = settings.auto_detect;
            config.java.auto_download = settings.auto_download;
            config.java.min_memory_mib = settings.min_memory_mib;
            config.java.max_memory_mib = settings.max_memory_mib;
            config.java.jvm_args.clone_from(&settings.jvm_args);
            config.java.preferred_path = settings
                .preferred_path
                .as_ref()
                .map_or_else(String::new, |path| path.to_string_lossy().into_owned());
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
        let (loader, java_override, jvm_override) = match request.target {
            JavaTarget::VersionOnly => (ephervex_model::LoaderSelection::vanilla(), None, None),
            JavaTarget::Client(id) => {
                let record = self.client(id)?;
                (record.loader, record.java_override, record.jvm_override)
            }
            JavaTarget::Server(id) => {
                let record = self.server(id)?;
                (record.loader, record.java_override, record.jvm_override)
            }
        };
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
    async fn plan_java_for_selection(
        &self,
        minecraft_version: &str,
        required: RequiredJava,
        loader: &ephervex_model::LoaderSelection,
        java_override: Option<ephervex_model::JavaOverride>,
        jvm_override: Option<ephervex_model::JvmSettings>,
        request: &JavaPlanRequest,
    ) -> CoreResult<JavaPlan> {
        let loader_rules = self
            .loaders()?
            .provider(loader.kind)
            .java_rules(minecraft_version, loader.version.as_deref());
        let rules: Vec<&dyn JavaCompatibilityRule> = loader_rules
            .iter()
            .map(|rule| rule as &dyn JavaCompatibilityRule)
            .collect();
        let maximum_update = loader_rules
            .iter()
            .filter_map(ephervex_java::VersionBound::maximum)
            .filter(|maximum| maximum.major == required.major)
            .map(|maximum| maximum.update)
            .min();

        let settings = self.java_settings();
        let preference = JavaPreference {
            explicit: request.explicit_path.clone(),
            instance: java_override,
            global: settings.preferred_path.clone(),
        };
        let jvm = EffectiveJvmSettings::resolve(&settings, jvm_override.as_ref());

        let choice = preference.resolve();
        if let JavaPathChoice::Pinned { path, source } = &choice {
            let runtime = probe_runtime(path, pin_source_kind(*source)).await?;
            ephervex_java::check_pinned(&runtime, &required, &rules, *source)?;
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
            return Err(JavaError::AutoDetectDisabled {
                required_major: required.major,
            }
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
                    return Err(error.into());
                }
                if !settings.auto_download {
                    return Err(JavaError::ManagedDownloadDisabled {
                        required_major: required.major,
                    }
                    .into());
                }
                let installed = self
                    .install_java_with_maximum_update(required.major, maximum_update)
                    .await?;
                let inventory = RuntimeInventory {
                    runtimes: vec![installed.runtime.clone()],
                    failures: Vec::new(),
                    probed: 1,
                    candidates: 1,
                    scanned_at: chrono::Utc::now(),
                };
                let checked = ephervex_java::select(&inventory, &required, &rules)?;
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

    /// Downloads and installs a managed runtime of one major.
    ///
    /// Installs under the user's own data directory; nothing here needs or asks
    /// for elevated privileges. The inventory cache is invalidated afterwards
    /// so the new runtime is visible without an explicit Refresh.
    #[instrument(skip(self))]
    pub async fn install_java(&self, major: u32) -> CoreResult<ManagedInstallation> {
        self.install_java_with_maximum_update(major, None).await
    }

    async fn install_java_with_maximum_update(
        &self,
        major: u32,
        maximum_update: Option<u32>,
    ) -> CoreResult<ManagedInstallation> {
        // The archive lands in the shared cache and is unpacked into the data
        // root, so the lease has to outlive the unpack rather than the download.
        self.with_artifact_cache(Box::pin(
            self.install_java_holding_the_cache(major, maximum_update),
        ))
        .await
    }

    async fn install_java_holding_the_cache(
        &self,
        major: u32,
        maximum_update: Option<u32>,
    ) -> CoreResult<ManagedInstallation> {
        let manager = DownloadManager::new(DownloaderConfig::default()).map_err(|source| {
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

        let reporter = OperationReporter::silent("managed-java");
        let cancel = CancellationToken::new();
        let installed = match maximum_update {
            Some(maximum) => {
                provider
                    .install_at_most_update(major, maximum, &reporter, &cancel)
                    .await?
            }
            None => provider.install(major, &reporter, &cancel).await?,
        };
        if let Ok(mut cache) = self.java.write() {
            *cache = None;
        }
        Ok(installed)
    }

    /// The Java major a Minecraft version needs.
    ///
    /// Mojang's own answer when the metadata can be reached, the fallback table
    /// when it cannot. An *unreachable* provider degrades to the table; an id
    /// that is genuinely not a supported release does not,
    /// because answering "Java 8" for a version that does not exist would be a
    /// plan for nothing.
    async fn required_java(&self, minecraft_version: &str) -> CoreResult<RequiredJava> {
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
    fn cached_java(&self) -> Option<Arc<RuntimeInventory>> {
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
        let manager = DownloadManager::new(DownloaderConfig::default()).map_err(|source| {
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
        let manager = DownloadManager::new(DownloaderConfig::default()).map_err(|source| {
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

    fn minecraft(&self) -> CoreResult<&VersionCatalog> {
        if let Some(catalog) = self.minecraft.get() {
            return Ok(catalog);
        }
        let manager = DownloadManager::new(DownloaderConfig::default()).map_err(|source| {
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
        let manager = DownloadManager::new(DownloaderConfig {
            http: self.http_config(),
            ..DownloaderConfig::default()
        })
        .map_err(|source| ephervex_loaders::LoaderError::MetadataInvalid {
            provider: "loader",
            detail: source.to_string(),
        })?;
        let loaders = LoaderRegistry::new(manager, CacheLayout::new(self.paths().cache_dir()))?;
        Ok(self.loaders.get_or_init(|| loaders))
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
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned());
        self.mutate_config(|config| {
            config.persistent_instance_root = setting.clone().unwrap_or_default();
        })?;

        // Only this one field is overridable, so re-deriving is a replacement
        // rather than a re-resolution of the other roots (see `Paths`).
        let paths = self.paths().clone().with_persistent_instance_root(root);
        paths.ensure_base_directories()?;
        self.storage.registry = Registry::open(paths)?;

        tracing::info!(
            root = %self.paths().persistent_instances_dir().display(),
            "moved the persistent instance root"
        );
        Ok(())
    }

    /// Changes a setting and writes the configuration file.
    ///
    /// The single place configuration is mutated, so no service can change a
    /// value in memory and forget to persist it — and so `Config::save`'s
    /// refusal to write a credential-looking key (ADR 0004) is on the path of
    /// every write rather than of most of them.
    ///
    /// The change is applied to a copy and only committed once the write
    /// succeeds: an in-memory setting that failed to reach disk would come back
    /// on the next run and look like the launcher had ignored the user.
    pub(crate) fn mutate_config(&mut self, change: impl FnOnce(&mut Config)) -> CoreResult<()> {
        let mut updated = self.storage.config.clone();
        change(&mut updated);
        updated.save(self.paths())?;
        self.storage.config = updated;
        Ok(())
    }
}

async fn after_successful_validation<T>(
    validation: CoreResult<()>,
    optional_work: impl std::future::Future<Output = T>,
) -> CoreResult<T> {
    validation?;
    Ok(optional_work.await)
}

fn loader_build_is_available(listing: &LoaderVersionListing, selected: &str) -> bool {
    listing.builds.iter().any(|build| build.version == selected)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use ephervex_model::{LoaderSelection, StorageClass};

    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn app_rooted_at(base: &std::path::Path) -> App {
        App::open_at(Paths::rooted_at(base)).expect("app should open against a temporary root")
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
    fn application_display_settings_persist_without_managing_minecraft_preferences() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        assert_eq!(
            app.application_display_settings(),
            ApplicationDisplaySettings {
                text_scale: ApplicationTextScale::Default,
                technical_text_size: TechnicalTextSize::Default,
                show_linked_servers: false,
            }
        );

        app.set_application_display_settings(
            ApplicationTextScale::Large,
            TechnicalTextSize::Comfortable,
        )
        .expect("save display settings");
        assert!(app.config().minecraft_preferences.is_empty());

        let reopened = app_rooted_at(base.path());
        assert_eq!(
            reopened.application_display_settings(),
            ApplicationDisplaySettings {
                text_scale: ApplicationTextScale::Large,
                technical_text_size: TechnicalTextSize::Comfortable,
                show_linked_servers: false,
            }
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
