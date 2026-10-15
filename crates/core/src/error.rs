//! The error type frontends see.
//!
//! Frontends must not have to match on `ephervex-storage`'s error enums, or
//! `ephervex-download`'s, or any other provider's - that would make the layer
//! below `core` part of the frontend's compile-time surface, which is exactly
//! what the dependency direction exists to prevent.
//!
//! So every provider error is wrapped here. What crosses the boundary is a
//! stable message id plus the technical chain, and the frontend resolves the
//! id through `ephervex-i18n`. Nothing in this crate knows what
//! language the user reads.

use std::path::PathBuf;

use ephervex_auth::AuthError;
use ephervex_i18n::Arg;
use ephervex_java::JavaError;
use ephervex_loaders::LoaderError;
use ephervex_minecraft::{LaunchError, MinecraftError};
use ephervex_mod_platform::ModPlatformError;
use ephervex_model::{InstanceId, ServerId};
use ephervex_pack::{CredentialUnavailable, PackError};
use ephervex_server::ServerError;
use ephervex_storage::StorageError;

use crate::ConnectError;
use crate::instance::NameRefusal;
use crate::isolation::IsolationError;
use crate::snapshot::SnapshotError;
use crate::verify::RepairRefusal;

/// An error together with everything that caused it, for one log line.
///
/// `Display` on a `thiserror` enum prints only the outermost message, so a
/// wrapper reaches the log without the artifact, status, or io error that
/// explains it. A download error never names the query or credentials of a url,
/// nor the text of one that failed to parse; they are dropped where it is built.
#[derive(Debug)]
pub struct ErrorChain<'a>(pub &'a dyn std::error::Error);

impl std::fmt::Display for ErrorChain<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut printed = self.0.to_string();
        write!(formatter, "{printed}")?;
        let mut cause = self.0.source();
        while let Some(error) = cause {
            let message = error.to_string();
            // Some errors render their own source, so appending it would say
            // the same thing twice. Deeper causes are still worth walking to.
            if !printed.ends_with(&message) {
                write!(formatter, ": {message}")?;
            }
            printed = message;
            cause = error.source();
        }
        Ok(())
    }
}

/// Anything an application service can fail with.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CoreError {
    /// Paths, configuration, the index, or the registry.
    #[error(transparent)]
    Storage(StorageError),

    /// A write reached the filesystem and the backing device had no space.
    #[error("the storage device is full")]
    StorageFull { path: Option<PathBuf> },

    /// Mojang version metadata: the manifest, the release filter, or one
    /// version's document.
    #[error(transparent)]
    Minecraft(#[from] MinecraftError),

    /// Java runtime discovery, selection, or managed installation.
    #[error(transparent)]
    Java(#[from] JavaError),

    /// A launch or server start refused its Java runtime, with the runtime it
    /// tried and what the host expects. Every predicate answers as it does for
    /// the [`JavaError`] it carries.
    #[error(transparent)]
    JavaRefused(Box<crate::java_failure::JavaRefusal>),

    /// The effective JVM heap range cannot produce a valid Java command.
    /// Each bound names the layer that supplied it.
    #[error(
        "java memory settings are invalid: minimum {minimum_mib} MiB ({minimum_layer}), maximum {maximum_mib} MiB ({maximum_layer})",
        minimum_layer = .minimum_layer.as_str(),
        maximum_layer = .maximum_layer.as_str(),
    )]
    JavaSettingsInvalid {
        minimum_mib: u32,
        maximum_mib: u32,
        minimum_layer: ephervex_java::JvmSettingLayer,
        maximum_layer: ephervex_java::JvmSettingLayer,
    },

    /// Forge/NeoForge discovery and installation.
    #[error(transparent)]
    Loader(#[from] LoaderError),

    /// The Microsoft/Xbox/Minecraft token chain, or credential storage.
    #[error(transparent)]
    Auth(#[from] AuthError),

    /// Reading or writing an instance's `options.txt`.
    #[error(transparent)]
    Options(#[from] ephervex_options::OptionsError),

    /// A global Minecraft preference the user set to something unusable.
    ///
    /// Separate from [`Self::Options`] because it is not a filesystem failure
    /// and has a different next step: the value is refused at the moment it is
    /// typed, and the message names the syntax that would have worked.
    #[error(transparent)]
    Preference(#[from] ephervex_options::InvalidPreferenceValue),

    /// A configured value that does not fit the product preference it names.
    ///
    /// The generic engine's counterpart to [`Self::Preference`]: it names the
    /// stable preference id rather than one of the seven facade keys, and it
    /// is refused when the batch is saved rather than skipped at every later
    /// instance creation.
    #[error(transparent)]
    PreferenceValue(#[from] ephervex_options::PreferenceValueError),

    /// A preference id this build has no product descriptor for.
    ///
    /// Reading configuration written by a newer Ephervex leaves such an id
    /// alone; deliberately *writing* one is a caller mistake, because nothing
    /// in this build could ever apply it.
    #[error("`{id}` is not a Minecraft preference this build offers")]
    PreferenceUnrecognized { id: String },

    /// The bundled product preference catalog could not be read.
    ///
    /// A broken build rather than anything a user did, so it stays reportable
    /// instead of aborting the process.
    #[error("the bundled Minecraft preference catalog is unusable: {detail}")]
    PreferenceCatalogUnusable { detail: String },

    /// Reading, validating, or materializing an imported pack.
    ///
    /// Constructed through the conversion below rather than by `#[from]`, so a
    /// pack failure that is really a provider-credential failure arrives as
    /// [`Self::ModPlatform`] and keeps the vocabulary a frontend routes on.
    #[error(transparent)]
    Pack(PackError),

    /// Local snapshot capture, listing, restoration, or removal.
    #[error(transparent)]
    Snapshot(SnapshotError),

    /// A durable dependency-aware mod isolation session.
    #[error(transparent)]
    Isolation(IsolationError),

    /// Searching a mod catalog, or resolving a mod's dependencies.
    #[error(transparent)]
    ModPlatform(#[from] ModPlatformError),

    /// Installing or launching a version.
    ///
    /// The typed `LaunchError::AuthenticationRequired` and `MissingJava` states are
    /// deliberately *not* in here: they arrive as [`Self::Auth`] and
    /// [`Self::Java`], and the predicates below -
    /// [`Self::requires_interactive_login`], [`Self::required_java_major`] -
    /// are what a frontend asks instead. One definition of "the user must sign
    /// in" rather than two that can drift.
    #[error(transparent)]
    Launch(#[from] LaunchError),

    /// Dedicated-server properties, consent, process, or capture lifecycle.
    #[error(transparent)]
    Server(#[from] ServerError),

    /// Validating and preparing a linked client/server launch.
    #[error(transparent)]
    Connect(#[from] ConnectError),

    /// Measuring, collecting, or clearing the reusable artifact cache.
    ///
    /// Reaches a frontend only for the operations a user asked for directly -
    /// reading the cache's size, clearing it, changing its budget. Automatic
    /// maintenance never surfaces here: see `App::maintain_artifact_cache` for
    /// why a housekeeping failure must not become an installation failure.
    #[error(transparent)]
    ArtifactCache(#[from] ephervex_download::ArtifactCacheError),

    /// A cache budget the user typed that cannot be represented.
    #[error(transparent)]
    ArtifactCacheLimitInvalid(#[from] ephervex_model::ArtifactCacheLimitError),

    /// A mutation named a file the instance does not record as one of its mods.
    ///
    /// Always a stale view rather than a bad request: a frontend acts on the
    /// list it was given, and the mod may have been removed since.
    #[error("`{path}` is not one of this instance's managed mods")]
    ModNotManaged { path: std::path::PathBuf },

    /// The release is right, and the provider will not let this launcher
    /// fetch it.
    ///
    /// Not a download failure and not a reason to refuse the release: the
    /// project's owner turned third-party distribution off, and the bytes are
    /// nowhere on this machine yet. Carries every blocked file so a frontend
    /// can offer the same user-assisted acquisition a pack import does -
    /// the provider's own page, the exact file name, and the digest a
    /// hand-placed copy is recognized by - and then ask for the same change
    /// again.
    #[error("`{path}` needs a release its provider does not permit this launcher to download")]
    ModAcquisitionRestricted {
        path: std::path::PathBuf,
        blocked: Vec<ephervex_pack::BlockedProviderFile>,
    },

    /// The mod declares that it does not run on a server, so it is never
    /// installed on one.
    #[error("`{project}` is declared client-only, so it cannot be installed on a server")]
    ModClientOnly { project: String },

    /// The install would put mods on a server whose catalog never stated server
    /// support, and the request did not confirm them.
    #[error("server support for {} was not stated and was not confirmed", .projects.join(", "))]
    ModServerSupportUnconfirmed { projects: Vec<String> },

    /// The mod exists and its ownership or identity does not allow this.
    ///
    /// Carries the reason so a frontend explains the refusal rather than
    /// reporting a generic failure for four different situations.
    #[error("`{path}` cannot be changed: {}", restriction.as_str())]
    ModMutationRestricted {
        path: std::path::PathBuf,
        restriction: crate::mutation::MutationRestriction,
    },

    /// A provider's project artwork cannot be shown.
    ///
    /// Never fatal and never propagated to a workspace: artwork is decoration,
    /// so this is logged and the mod keeps its place in the list under the
    /// generic icon. It is a typed error rather than a bare log line because
    /// "the provider sent something this build cannot draw" is a real answer
    /// with a real cause, and a caller that one day wants to show it should not
    /// have to parse a message to find out which project it was about.
    #[error("artwork for `{project}` cannot be displayed: {detail}")]
    ArtworkUnusable { project: String, detail: String },

    /// The instance has no loader family to stay inside, so there is no version
    /// of one to change.
    ///
    /// Turning a vanilla instance into a modded one - or one loader into
    /// another - changes what the instance is and invalidates every mod in it.
    /// That is a different operation from moving between builds of one loader,
    /// and this refuses rather than quietly becoming it.
    #[error("`{loader}` instances have no loader build to change")]
    LoaderFamilyFixed { loader: ephervex_model::LoaderKind },

    /// Managed mods are known not to fit the candidate loader build.
    ///
    /// A refusal the caller can override, because the user may be changing the
    /// loader precisely to see what breaks. Never resolved by updating the
    /// mods: that would move two things at once in an instance whose purpose is
    /// to isolate one.
    #[error("{incompatible} installed mods do not fit the selected loader build")]
    LoaderChangeBlockedByMods { incompatible: usize },

    /// The recorded default mod catalog is a name this build cannot resolve.
    ///
    /// A newer Ephervex may know a catalog this one does not, and the recorded
    /// name is kept rather than rewritten. Reported instead of quietly browsing
    /// the built-in default, which would search one catalog while the user's
    /// setting names another.
    #[error("`{configured}` is not a mod catalog this version of Ephervex knows")]
    ModCatalogPreferenceUnrecognized { configured: String },

    /// The one-launcher-per-user socket could not be claimed or reached.
    #[error("another launcher could not be reached: {detail}")]
    SingleInstanceUnavailable { detail: String },

    /// The desktop link handler could not be registered or removed.
    #[error("the link handler could not be changed: {detail}")]
    UrlHandlerUnavailable { detail: String },

    /// A pack address that is not one Ephervex will follow.
    ///
    /// The address itself is wrong: not absolute, not HTTPS, no host, or
    /// carrying credentials in its authority. Distinct from
    /// [`Self::PackHostNotTrusted`], which is about consent rather than shape.
    #[error("{url} is not an address a pack can be fetched from: {detail}")]
    PackUrlInvalid { url: String, detail: String },

    /// A well-formed pack address nobody has agreed to fetch from.
    ///
    /// Not a failure of the address. Downloading from a host a web page chose
    /// is something the user says yes to first, and this is what refusing to
    /// guess on their behalf looks like (ADR 0027).
    #[error("nobody has agreed to download packs from {host}")]
    PackHostNotTrusted { url: String, host: String },

    /// The pack archive could not be downloaded.
    #[error("the pack at {url} could not be downloaded: {detail}")]
    PackDownloadFailed { url: String, detail: String },

    /// Asked to make an instance persistent that already is.
    ///
    /// A refusal rather than a silent success, so a frontend that offered the
    /// action was wrong about the instance and hears so.
    #[error("instance {id} is already persistent")]
    InstanceAlreadyPersistent { id: InstanceId },

    /// Asked to delete an instance while one of its linked servers runs.
    #[error("the linked server {name} is running")]
    LinkedServerRunning { name: String },

    /// A linked server may be running where Ephervex cannot stop it.
    #[error("the linked server {name} may still be running and cannot be stopped")]
    LinkedServerUnstoppable { name: String },

    /// A linked server did not stop for its instance's deletion.
    #[error("the linked server {name} could not be stopped: {detail}")]
    LinkedServerNotStopped { name: String, detail: String },

    /// Asked to change an instance whose game is launching or running.
    #[error("the instance is running as process {pid} since {since}")]
    InstanceRunning {
        pid: u32,
        since: chrono::DateTime<chrono::Utc>,
    },

    /// Asked to move a server that is not linked to the instance being moved.
    ///
    /// Promotion takes an explicit list of servers, and a list naming somebody
    /// else's server is a caller mistake rather than a thing to quietly skip.
    #[error("server {server} is not linked to instance {instance}")]
    ServerNotLinkedToInstance {
        server: ServerId,
        instance: InstanceId,
    },

    /// Asked to enable or disable a mod whose jar is in neither state.
    ///
    /// Reported rather than treated as a success: enabling a mod that is not
    /// on disk would leave the caller believing the loader will pick something
    /// up that is not there. The recipe still records it, so the answer is a
    /// repair install, not a rename.
    #[error("no file for `{}` is installed", path.display())]
    ModFileMissing { path: PathBuf },

    /// The rename between `<name>.jar` and `<name>.jar.disabled` failed.
    #[error("`{}` could not be enabled or disabled", path.display())]
    ModFileNotToggled {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The name a toggle would move a mod's jar to is already taken.
    ///
    /// Both `<name>.jar` and `<name>.jar.disabled` exist, so either rename
    /// would destroy one of two files the user may want. Neither is touched.
    #[error("both `{}` and its disabled twin exist", path.display())]
    ModSpellingOccupied { path: PathBuf },

    /// Asked to delete a jar the launcher installed and records.
    ///
    /// Deleting a file is only ever offered for a jar somebody put in `mods/`
    /// themselves. Taking a launcher-owned mod out of an instance is a removal,
    /// which resolves what the mod's dependencies leave orphaned and retires
    /// the files so the change can be undone; deleting the jar underneath that
    /// record would leave the instance describing a mod it no longer has.
    #[error("`{}` was installed by Ephervex, so it is not deleted this way", path.display())]
    ModNotHandPlaced { path: PathBuf },

    /// Deleting a hand-placed jar failed.
    #[error("`{}` could not be deleted", path.display())]
    ModFileNotDeleted {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Writing a diagnostic bundle failed.
    #[error("a diagnostic bundle could not be written to `{}`", path.display())]
    DiagnosticBundleNotWritten { path: PathBuf, detail: String },

    /// The destination of a diagnostic bundle is one of the files it collects.
    #[error("`{}` is a diagnostic file, so a bundle cannot be written over it", path.display())]
    DiagnosticBundleOverwritesSource { path: PathBuf },

    /// Copying a jar into `mods/` failed part-way.
    ///
    /// The staged copy is discarded, so `mods/` never holds the half of a file
    /// that was written before this.
    #[error("`{}` could not be added to the mods folder", path.display())]
    ModFileNotAdded {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// A name the user typed cannot be stored as it stands.
    #[error("that name cannot be used: {reason}")]
    EntityNameInvalid { reason: NameRefusal },

    /// A repair that would rewrite mods something else is using right now.
    #[error("this host cannot be repaired while {reason}")]
    RepairRefused { reason: RepairRefusal },

    /// A diagnostic fix that would change a host something else holds.
    #[error("this host cannot be changed while {reason}")]
    ChangeRefused { reason: RepairRefusal },

    /// Working out which dependencies are still needed requires every root's
    /// project, and one of them has none.
    ///
    /// Refused rather than answered partially: a partial answer to "what is
    /// still required" reads as proof, and acting on it deletes a mod something
    /// still needs.
    #[error("`{path}` has no provider identity, so the instance's dependencies cannot be proven")]
    ModIdentityIncomplete { path: std::path::PathBuf },

    /// A version change would have to move another mod that it must not.
    ///
    /// Refused because only a dependency's own dependents decide its release,
    /// and only while nothing else installed needs the release it has. A mod a
    /// pack declared or a person chose is never moved as a side effect of
    /// changing something else.
    #[error("changing the release of `{target}` would also move `{path}`")]
    ModDependencyNotMovable {
        target: std::path::PathBuf,
        path: std::path::PathBuf,
    },
}

/// Lifts a pack failure into the error frontends see.
///
/// One case is not a pack failure at all. Materializing retained desired state
/// can discover that an artifact's verified copy is no longer in the shared
/// cache and that its provider will only serve it to an authenticated request;
/// `ephervex-pack` cannot name that provider's error type, so it carries the
/// original through and this is where it is taken back out. What reaches a
/// frontend is therefore the same typed credential failure a provider request
/// would have produced - absent key, unreadable key, refused key - rather than a
/// download failure it could only report as "something went wrong".
impl From<PackError> for CoreError {
    fn from(error: PackError) -> Self {
        if error_chain_is_storage_full(&error)
            || error
                .recovery_issues()
                .iter()
                .any(|issue| issue.error_kind == std::io::ErrorKind::StorageFull)
        {
            return Self::StorageFull {
                path: error.failing_path(),
            };
        }
        let PackError::ArtifactCredentialUnavailable { path, source } = error else {
            return Self::Pack(error);
        };
        match source.into_inner().downcast::<ModPlatformError>() {
            Ok(platform) => Self::ModPlatform(*platform),
            // Something else entirely, which keeps the pack crate's own wording
            // rather than being described as a catalog failure it is not.
            Err(other) => Self::Pack(PackError::ArtifactCredentialUnavailable {
                path,
                source: CredentialUnavailable::from_boxed(other),
            }),
        }
    }
}

impl From<SnapshotError> for CoreError {
    fn from(error: SnapshotError) -> Self {
        if error_chain_is_storage_full(&error) {
            Self::StorageFull {
                path: error.failing_path(),
            }
        } else {
            Self::Snapshot(error)
        }
    }
}

impl From<IsolationError> for CoreError {
    fn from(error: IsolationError) -> Self {
        if error_chain_is_storage_full(&error) {
            Self::StorageFull {
                path: error.failing_path(),
            }
        } else {
            Self::Isolation(error)
        }
    }
}

impl From<ephervex_java::MemoryRangeViolation> for CoreError {
    fn from(violation: ephervex_java::MemoryRangeViolation) -> Self {
        Self::JavaSettingsInvalid {
            minimum_mib: violation.minimum_mib,
            maximum_mib: violation.maximum_mib,
            minimum_layer: violation.minimum_layer,
            maximum_layer: violation.maximum_layer,
        }
    }
}

impl CoreError {
    /// Whether the metadata at an indexed instance or server root names a
    /// different record, so the host can be forgotten but not changed.
    #[must_use]
    pub(crate) fn is_foreign_host_metadata(&self) -> bool {
        matches!(
            self,
            Self::Storage(StorageError::Registry(
                ephervex_storage::RegistryError::InstanceMetadataForeign { .. }
                    | ephervex_storage::RegistryError::ServerMetadataForeign { .. }
            ))
        )
    }

    /// The `ephervex-i18n` message id describing this failure to a user.
    ///
    /// Delegating rather than re-deriving keeps one message id per failure
    /// across the whole stack, so the CLI and the GUI cannot drift into
    /// describing the same problem differently.
    #[must_use]
    pub fn message_id(&self) -> &'static str {
        match self {
            Self::Storage(error) => error.message_id(),
            Self::StorageFull { .. } => "error-storage-full",
            Self::Minecraft(error) => error.message_id(),
            Self::Java(error) => error.message_id(),
            Self::JavaRefused(refusal) => refusal.error().message_id(),
            Self::JavaSettingsInvalid { .. } => "error-java-settings-invalid",
            Self::Loader(error) => error.message_id(),
            // Not `const`: this one chooses between message ids on the HTTP
            // status, so that a temporary outage and a permanent refusal do not
            // share a sentence.
            Self::Auth(error) => error.message_id(),
            Self::Launch(error) => error.message_id(),
            Self::Options(error) => error.message_id(),
            Self::Preference(error) => error.message_id(),
            Self::PreferenceValue(error) => error.message_id(),
            Self::PreferenceUnrecognized { .. } => "error-options-preference-unrecognized",
            Self::PreferenceCatalogUnusable { .. } => "error-options-preference-catalog-invalid",
            Self::Pack(error) => error.message_id(),
            Self::Snapshot(error) => error.message_id(),
            Self::Isolation(error) => error.message_id(),
            Self::ModPlatform(error) => error.message_id(),
            Self::Server(error) => error.message_id(),
            Self::Connect(error) => error.message_id(),
            Self::ArtifactCache(error) => error.message_id(),
            Self::ArtifactCacheLimitInvalid(error) => error.message_id(),
            Self::ArtworkUnusable { .. } => "error-artwork-unusable",
            Self::LoaderFamilyFixed { .. } => "error-loader-family-fixed",
            Self::LoaderChangeBlockedByMods { .. } => "error-loader-change-blocked-by-mods",
            Self::ModNotManaged { .. } => "error-mod-not-managed",
            Self::ModAcquisitionRestricted { .. } => "error-mod-acquisition-restricted",
            Self::ModClientOnly { .. } => "error-mod-client-only",
            Self::ModServerSupportUnconfirmed { .. } => "error-mod-server-support-unconfirmed",
            Self::ModMutationRestricted { restriction, .. } => restriction.message_id(),
            Self::ModIdentityIncomplete { .. } => "error-mod-identity-incomplete",
            Self::ModDependencyNotMovable { .. } => "error-mod-dependency-not-movable",
            Self::ModCatalogPreferenceUnrecognized { .. } => {
                "error-mod-catalog-preference-unrecognized"
            }
            Self::UrlHandlerUnavailable { .. } => "error-url-handler-unavailable",
            Self::SingleInstanceUnavailable { .. } => "error-single-instance-unavailable",
            Self::PackUrlInvalid { .. } => "error-pack-url-invalid",
            Self::PackHostNotTrusted { .. } => "error-pack-host-not-trusted",
            Self::PackDownloadFailed { .. } => "error-pack-archive-download-failed",
            Self::InstanceAlreadyPersistent { .. } => "error-instance-already-persistent",
            Self::InstanceRunning { .. } => "error-instance-running",
            Self::LinkedServerRunning { .. } => "error-linked-server-running",
            Self::LinkedServerUnstoppable { .. } => "error-linked-server-unstoppable",
            Self::LinkedServerNotStopped { .. } => "error-linked-server-not-stopped",
            Self::ServerNotLinkedToInstance { .. } => "error-server-not-linked",
            Self::ModFileMissing { .. } => "error-mod-file-missing",
            Self::ModFileNotToggled { .. } => "error-mod-file-not-toggled",
            Self::ModSpellingOccupied { .. } => "error-mod-spelling-occupied",
            Self::ModNotHandPlaced { .. } => "error-mod-not-hand-placed",
            Self::ModFileNotDeleted { .. } => "error-mod-file-not-deleted",
            Self::DiagnosticBundleNotWritten { .. } => "error-diagnostic-bundle-not-written",
            Self::DiagnosticBundleOverwritesSource { .. } => {
                "error-diagnostic-bundle-overwrites-source"
            }
            Self::ModFileNotAdded { .. } => "error-mod-file-not-added",
            Self::EntityNameInvalid { reason } => reason.message_id(),
            Self::RepairRefused { reason } => reason.message_id(),
            Self::ChangeRefused { reason } => match reason {
                RepairRefusal::HostRunning => "error-change-host-running",
                RepairRefusal::IsolationActive => "error-change-isolation-active",
            },
        }
    }

    /// Extra values the localized message and its next step take as arguments.
    ///
    /// The same idea as [`Self::failing_path`] and [`Self::help_subject`], for
    /// the failures whose messages name several technical values. Delegated so
    /// that each provider states its own arguments beside its own message ids,
    /// and a frontend supplies them without matching on any error hierarchy.
    /// A quantity is an [`Arg::Count`] so that plural selection sees a number.
    #[must_use]
    pub fn message_args(&self) -> Vec<(&'static str, Arg<'static>)> {
        fn text(args: Vec<(&'static str, String)>) -> Vec<(&'static str, Arg<'static>)> {
            args.into_iter()
                .map(|(name, value)| (name, Arg::from(value)))
                .collect()
        }

        match self {
            Self::Loader(error) => text(error.message_args()),
            Self::Pack(error) => text(error.message_args()),
            Self::Snapshot(error) => text(error.message_args()),
            Self::Isolation(error) => text(error.message_args()),
            Self::ModPlatform(error) => text(error.message_args()),
            Self::Server(error) => text(error.message_args()),
            Self::Connect(error) => text(error.message_args()),
            Self::ArtifactCacheLimitInvalid(error) => text(error.message_args()),
            Self::LoaderFamilyFixed { loader } => vec![("loader", Arg::from(loader.to_string()))],
            Self::InstanceRunning { pid, .. } => vec![("pid", Arg::from(pid.to_string()))],
            Self::LinkedServerRunning { name }
            | Self::LinkedServerUnstoppable { name }
            | Self::LinkedServerNotStopped { name, .. } => {
                vec![("name", Arg::from(name.clone()))]
            }
            Self::LoaderChangeBlockedByMods { incompatible } => {
                vec![("count", Arg::from(*incompatible))]
            }
            Self::ModCatalogPreferenceUnrecognized { configured } => {
                vec![("catalog", Arg::from(configured.clone()))]
            }
            Self::ModClientOnly { project } => vec![("project", Arg::from(project.clone()))],
            Self::ModServerSupportUnconfirmed { projects } => {
                vec![("mods", Arg::from(projects.join(", ")))]
            }
            // The first file named, because the sentence is a next step rather
            // than an inventory: a frontend that can show the whole list has
            // the list itself.
            Self::ModAcquisitionRestricted { blocked, .. } => vec![(
                "file",
                Arg::from(
                    blocked
                        .first()
                        .and_then(|file| file.file_name.clone())
                        .unwrap_or_default(),
                ),
            )],
            Self::Preference(error) => vec![("syntax", Arg::from(error.syntax.to_owned()))],
            Self::PreferenceValue(error) => vec![("preference", Arg::from(error.id.to_string()))],
            Self::EntityNameInvalid {
                reason: NameRefusal::TooLong,
            } => vec![(
                "limit",
                Arg::from(crate::instance::MAX_ENTITY_NAME_CHARS.to_string()),
            )],
            Self::JavaSettingsInvalid {
                minimum_mib,
                maximum_mib,
                minimum_layer,
                maximum_layer,
            } => vec![
                ("minimum", Arg::from(minimum_mib.to_string())),
                ("maximum", Arg::from(maximum_mib.to_string())),
                (
                    "minimum_layer",
                    Arg::from(minimum_layer.as_str().to_owned()),
                ),
                (
                    "maximum_layer",
                    Arg::from(maximum_layer.as_str().to_owned()),
                ),
            ],
            _ => Vec::new(),
        }
    }

    /// Whether the user has to sign in again before this can succeed.
    ///
    /// Exists for the same reason as [`Self::failing_path`]: a frontend that
    /// wants to show a "Sign in" button must not have to match on `AuthError`
    /// to know it should, because doing so would put the whole authentication
    /// error hierarchy on its compile-time surface. Vanilla launch support's
    /// `LaunchError::AuthenticationRequired` is this predicate wearing a
    /// different name.
    #[must_use]
    pub const fn requires_interactive_login(&self) -> bool {
        matches!(self, Self::Auth(error) if error.requires_interactive_login())
    }

    /// Whether offering a session-only login is a sensible next step.
    ///
    /// True only when secure credential storage is missing. Callers use this to
    /// offer to be explicit rather than a silent fallback, so the decision is
    /// made once here instead of by each frontend.
    #[must_use]
    pub const fn session_only_login_would_help(&self) -> bool {
        matches!(self, Self::Auth(error) if error.session_only_would_help())
    }

    /// Whether a copy in the local cache or store is damaged, as opposed to a
    /// provider failing or a check being unable to run. A frontend grades
    /// this as a problem to fix rather than a check it had to skip.
    #[must_use]
    pub const fn is_damaged_local_data(&self) -> bool {
        matches!(self, Self::Minecraft(error) if error.is_damaged_local_data())
    }

    /// Whether the user canceled rather than something failing.
    #[must_use]
    pub fn is_cancellation(&self) -> bool {
        match self {
            Self::Auth(error) => error.is_cancellation(),
            Self::Loader(error) => error.is_cancellation(),
            Self::Launch(error) => error.is_cancellation(),
            Self::Minecraft(error) => error.is_cancellation(),
            Self::Java(error) => error.is_cancellation(),
            Self::JavaRefused(refusal) => refusal.error().is_cancellation(),
            Self::Pack(error) => error.is_cancellation(),
            Self::Snapshot(error) => error.is_cancellation(),
            Self::Isolation(error) => error.is_cancellation(),
            Self::ModPlatform(error) => error.is_cancellation(),
            Self::Server(ServerError::ReadinessCanceled | ServerError::StartCanceled) => true,
            _ => false,
        }
    }

    /// Whether a server run ended because the replacement for a restart could
    /// not be started. The restart's own caller is told the same failure.
    #[must_use]
    pub const fn is_failed_restart_replacement(&self) -> bool {
        matches!(
            self,
            Self::Server(ServerError::RestartReplacementFailed { .. })
        )
    }

    /// Whether this installation cannot authenticate until its Azure
    /// application registration is approved for the Minecraft Services API.
    ///
    /// Worth a predicate of its own because it is the one authentication
    /// failure no user action inside the launcher can resolve - not signing in
    /// again, not waiting, not switching accounts. A frontend showing a
    /// "Sign in" button for it would be offering a button that cannot work.
    #[must_use]
    pub const fn needs_app_registration_approval(&self) -> bool {
        matches!(self, Self::Auth(error) if error.needs_app_registration_approval())
    }

    /// A page the user has to visit to resolve this failure, where one exists.
    ///
    /// The same idea as [`Self::failing_path`], for the failures that send a
    /// user to a web page rather than to a file. The URL is untranslated
    /// technical text that the localized message takes as an argument, so a
    /// frontend needs it without matching on `AuthError`.
    #[must_use]
    pub const fn help_url(&self) -> Option<&'static str> {
        match self {
            Self::Auth(error) => error.help_url(),
            _ => None,
        }
    }

    /// The value the user has to supply at [`Self::help_url`], where the
    /// failure names one - for the approval form, the Client ID.
    #[must_use]
    pub fn help_subject(&self) -> Option<&str> {
        match self {
            Self::Auth(error) => error.help_subject(),
            _ => None,
        }
    }

    /// The input syntax a refused setting expected.
    ///
    /// The same idea as [`Self::help_subject`], for the one failure whose next
    /// step is "here is what you should have typed". A literal format such as
    /// `WIDTHxHEIGHT` is untranslated technical text that the localized message
    /// takes as an argument, so a frontend needs it without
    /// matching on the preference error.
    #[must_use]
    pub const fn expected_syntax(&self) -> Option<&'static str> {
        match self {
            Self::Preference(error) => Some(error.syntax),
            _ => None,
        }
    }

    /// The mod loader an imported pack asked for that this launcher does not
    /// support.
    ///
    /// Exists for the same reason as [`Self::required_java_major`]: a frontend
    /// that wants to say *which* loader was refused - and to record it as an
    /// attention reason - must not have to match on `PackError` to find the
    /// name.
    #[must_use]
    pub fn unsupported_loader(&self) -> Option<&str> {
        match self {
            Self::Pack(error) => error.unsupported_loader(),
            _ => None,
        }
    }

    /// The Java major version this failure is about, where it names one.
    ///
    /// Exists for the same reason as [`Self::failing_path`]: a frontend that
    /// wants to offer "download Java 21" must not have to match on `JavaError`
    /// to find the number, because doing so would put the whole Java error
    /// hierarchy on its compile-time surface.
    #[must_use]
    pub const fn required_java_major(&self) -> Option<u32> {
        match self {
            Self::Java(error) => error.required_major(),
            Self::JavaRefused(refusal) => refusal.error().required_major(),
            Self::Storage(_)
            | Self::StorageFull { .. }
            | Self::ArtifactCache(_)
            | Self::ArtifactCacheLimitInvalid(_)
            | Self::DiagnosticBundleNotWritten { .. }
            | Self::DiagnosticBundleOverwritesSource { .. }
            | Self::JavaSettingsInvalid { .. }
            | Self::Minecraft(_)
            | Self::Loader(_)
            | Self::Auth(_)
            | Self::Launch(_)
            | Self::Options(_)
            | Self::Preference(_)
            | Self::PreferenceValue(_)
            | Self::PreferenceUnrecognized { .. }
            | Self::PreferenceCatalogUnusable { .. }
            | Self::ModPlatform(_)
            | Self::Pack(_)
            | Self::Snapshot(_)
            | Self::Isolation(_)
            | Self::Server(_)
            | Self::Connect(_)
            | Self::ModNotManaged { .. }
            | Self::ModAcquisitionRestricted { .. }
            | Self::ModClientOnly { .. }
            | Self::ModServerSupportUnconfirmed { .. }
            | Self::ModMutationRestricted { .. }
            | Self::ModIdentityIncomplete { .. }
            | Self::ModDependencyNotMovable { .. }
            | Self::ModFileMissing { .. }
            | Self::ModFileNotToggled { .. }
            | Self::ModSpellingOccupied { .. }
            | Self::InstanceAlreadyPersistent { .. }
            | Self::InstanceRunning { .. }
            | Self::LinkedServerRunning { .. }
            | Self::LinkedServerUnstoppable { .. }
            | Self::LinkedServerNotStopped { .. }
            | Self::ServerNotLinkedToInstance { .. }
            | Self::PackUrlInvalid { .. }
            | Self::PackHostNotTrusted { .. }
            | Self::PackDownloadFailed { .. }
            | Self::UrlHandlerUnavailable { .. }
            | Self::SingleInstanceUnavailable { .. }
            | Self::ModNotHandPlaced { .. }
            | Self::ModFileNotDeleted { .. }
            | Self::ModFileNotAdded { .. }
            | Self::EntityNameInvalid { .. }
            | Self::LoaderFamilyFixed { .. }
            | Self::LoaderChangeBlockedByMods { .. }
            | Self::ModCatalogPreferenceUnrecognized { .. }
            | Self::RepairRefused { .. }
            | Self::ChangeRefused { .. }
            | Self::ArtworkUnusable { .. } => None,
        }
    }

    /// The runtime a launch or start refused and what the host expected, when
    /// the failure is such a refusal.
    #[must_use]
    pub fn java_refusal(&self) -> Option<&crate::java_failure::JavaRefusal> {
        match self {
            Self::JavaRefused(refusal) => Some(refusal),
            _ => None,
        }
    }

    /// Whether downloading a managed runtime is a sensible next step.
    #[must_use]
    pub const fn managed_java_would_help(&self) -> bool {
        match self {
            Self::Java(error) => error.managed_download_would_help(),
            Self::JavaRefused(refusal) => refusal.error().managed_download_would_help(),
            _ => false,
        }
    }

    /// The path this failure is about, where it names one.
    ///
    /// Frontends want to tell the user *which file* to look at, and the answer
    /// is buried at varying depths inside the provider error enums. Digging it
    /// out here rather than in the frontend is what keeps `ConfigError`,
    /// `RegistryError`, `IndexError`, and friends off a frontend's compile-time
    /// surface — otherwise "show the path" quietly drags the whole storage
    /// error hierarchy across the boundary.
    #[must_use]
    pub fn failing_path(&self) -> Option<PathBuf> {
        match self {
            Self::Storage(error) => storage_failing_path(error),
            Self::StorageFull { path } => path.clone(),
            Self::Minecraft(error) => error.failing_path(),
            Self::Java(error) => error.failing_path(),
            Self::JavaRefused(refusal) => refusal.error().failing_path(),
            Self::Loader(error) => error.failing_path(),
            Self::Launch(error) => error.failing_path(),
            Self::Options(error) => error.failing_path(),
            Self::Pack(error) => error.failing_path(),
            Self::Snapshot(error) => error.failing_path(),
            Self::Isolation(error) => error.failing_path(),
            Self::Server(error) => error.failing_path(),
            // Authentication failures are never about a file. The one thing
            // that would be — the keyring entry — is not a path the user can
            // usefully be pointed at. A rejected preference value is about a
            // setting, not a file either, and a catalog failure is about a
            // project rather than anything on disk.
            Self::ModNotManaged { path }
            | Self::ModAcquisitionRestricted { path, .. }
            | Self::ModMutationRestricted { path, .. }
            | Self::ModFileMissing { path }
            | Self::ModFileNotToggled { path, .. }
            | Self::ModSpellingOccupied { path }
            | Self::ModNotHandPlaced { path }
            | Self::ModFileNotDeleted { path, .. }
            | Self::ModFileNotAdded { path, .. }
            | Self::ModIdentityIncomplete { path }
            | Self::ModDependencyNotMovable { path, .. }
            | Self::DiagnosticBundleNotWritten { path, .. }
            | Self::DiagnosticBundleOverwritesSource { path }
            | Self::Auth(
                ephervex_auth::AuthError::CredentialChangeBusy { path }
                | ephervex_auth::AuthError::CredentialChangeUnavailable { path },
            ) => Some(path.clone()),
            Self::ArtifactCache(error) => error.failing_path(),
            Self::Auth(_)
            | Self::ArtifactCacheLimitInvalid(_)
            | Self::JavaSettingsInvalid { .. }
            | Self::Preference(_)
            | Self::PreferenceValue(_)
            | Self::PreferenceUnrecognized { .. }
            | Self::PreferenceCatalogUnusable { .. }
            | Self::ModPlatform(_)
            | Self::Connect(_)
            | Self::LoaderFamilyFixed { .. }
            | Self::LoaderChangeBlockedByMods { .. }
            | Self::ModClientOnly { .. }
            | Self::ModServerSupportUnconfirmed { .. }
            | Self::ModCatalogPreferenceUnrecognized { .. }
            | Self::InstanceAlreadyPersistent { .. }
            | Self::InstanceRunning { .. }
            | Self::LinkedServerRunning { .. }
            | Self::LinkedServerUnstoppable { .. }
            | Self::LinkedServerNotStopped { .. }
            | Self::ServerNotLinkedToInstance { .. }
            | Self::PackUrlInvalid { .. }
            | Self::PackHostNotTrusted { .. }
            | Self::PackDownloadFailed { .. }
            | Self::UrlHandlerUnavailable { .. }
            | Self::SingleInstanceUnavailable { .. }
            | Self::EntityNameInvalid { .. }
            | Self::RepairRefused { .. }
            | Self::ChangeRefused { .. }
            | Self::ArtworkUnusable { .. } => None,
        }
    }
}

impl From<StorageError> for CoreError {
    fn from(error: StorageError) -> Self {
        if error_chain_is_storage_full(&error) {
            Self::StorageFull {
                path: storage_failing_path(&error),
            }
        } else {
            Self::Storage(error)
        }
    }
}

fn error_chain_is_storage_full(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut current = Some(error);
    while let Some(cause) = current {
        if cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::StorageFull)
        {
            return true;
        }
        current = cause.source();
    }
    false
}

/// One function per error type rather than one deep flattened match: the arms
/// of a flattened version repeat identically across variants.
fn storage_failing_path(error: &StorageError) -> Option<PathBuf> {
    match error {
        StorageError::Config(inner) => config_failing_path(inner),
        StorageError::Registry(inner) => registry_failing_path(inner),
        StorageError::Path(inner) => path_failing_path(inner),
        StorageError::Lock(inner) => inner.failing_path(),
        // Always about a file or a directory: naming it is most of what makes
        // a diagnostics failure actionable.
        StorageError::Diagnostics(inner) => Some(inner.failing_path()),
    }
}

impl From<ephervex_storage::DiagnosticError> for CoreError {
    fn from(error: ephervex_storage::DiagnosticError) -> Self {
        Self::from(StorageError::Diagnostics(error))
    }
}

impl From<ephervex_storage::LockError> for CoreError {
    fn from(error: ephervex_storage::LockError) -> Self {
        Self::from(StorageError::Lock(error))
    }
}

fn config_failing_path(error: &ephervex_storage::ConfigError) -> Option<PathBuf> {
    use ephervex_storage::ConfigError;
    match error {
        ConfigError::Unreadable { path, .. }
        | ConfigError::Invalid { path, .. }
        | ConfigError::Unwritable { path, .. }
        | ConfigError::Busy { path } => Some(path.clone()),
        ConfigError::Path(inner) => path_failing_path(inner),
        ConfigError::Serialization(_)
        | ConfigError::UnsupportedSchema { .. }
        | ConfigError::SecretRejected { .. } => None,
    }
}

fn registry_failing_path(error: &ephervex_storage::RegistryError) -> Option<PathBuf> {
    use ephervex_storage::{IndexError, RegistryError};
    match error {
        RegistryError::RootOccupied { path }
        | RegistryError::HostBusy { path }
        | RegistryError::HostNotClaimed { path }
        | RegistryError::RecipeBusy { path }
        | RegistryError::RestoreUnfinished { path }
        | RegistryError::HostGuardUnusable { path, .. }
        | RegistryError::RootMissing { path }
        | RegistryError::InstanceMetadataForeign { path }
        | RegistryError::ServerMetadataForeign { path }
        | RegistryError::Io { path, .. }
        | RegistryError::Index(IndexError::Unopenable { path, .. }) => Some(path.clone()),
        RegistryError::Metadata(inner) => metadata_failing_path(inner),
        RegistryError::Path(inner) => path_failing_path(inner),
        RegistryError::Lock(inner) => inner.failing_path(),
        RegistryError::Index(inner) => index_failing_path(inner),
        RegistryError::InstanceUnknown { .. } | RegistryError::ServerUnknown { .. } => None,
    }
}

fn metadata_failing_path(error: &ephervex_storage::MetadataError) -> Option<PathBuf> {
    use ephervex_storage::MetadataError;
    match error {
        MetadataError::Unwritable { path, .. }
        | MetadataError::Unreadable { path, .. }
        | MetadataError::Invalid { path, .. }
        | MetadataError::UnsupportedVersion { path, .. } => Some(path.clone()),
        MetadataError::Path(inner) => path_failing_path(inner),
    }
}

fn index_failing_path(error: &ephervex_storage::IndexError) -> Option<PathBuf> {
    use ephervex_storage::IndexError;
    match error {
        IndexError::Unopenable { path, .. } => Some(path.clone()),
        IndexError::Path(inner) => path_failing_path(inner),
        IndexError::Query(_)
        | IndexError::Migration { .. }
        | IndexError::SchemaFromFuture { .. }
        | IndexError::MalformedRow { .. } => None,
    }
}

fn path_failing_path(error: &ephervex_storage::PathError) -> Option<PathBuf> {
    use ephervex_storage::PathError;
    match error {
        PathError::DirectoryCreation { path, .. }
        | PathError::UnsafeTemporaryDirectory { path, .. }
        | PathError::NotUtf8 { path } => Some(path.clone()),
        PathError::NoHomeDirectory => None,
    }
}

impl From<ephervex_storage::RegistryError> for CoreError {
    fn from(error: ephervex_storage::RegistryError) -> Self {
        Self::from(StorageError::Registry(error))
    }
}

impl From<ephervex_storage::ConfigError> for CoreError {
    fn from(error: ephervex_storage::ConfigError) -> Self {
        Self::from(StorageError::Config(error))
    }
}

impl From<ephervex_storage::PathError> for CoreError {
    fn from(error: ephervex_storage::PathError) -> Self {
        Self::from(StorageError::Path(error))
    }
}

/// Shorthand for a service result.
pub type CoreResult<T> = Result<T, CoreError>;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn a_canceled_metadata_lookup_is_classified_as_cancellation() {
        let canceled = || ephervex_download::DownloadError::Canceled {
            url: ephervex_download::redacted(&"https://example.invalid/index".parse().unwrap()),
        };
        let minecraft = CoreError::from(MinecraftError::ManifestUnavailable {
            source: ephervex_download::MetadataCacheError::Unavailable {
                key: "minecraft-version-manifest".to_owned(),
                source: Box::new(canceled()),
            },
        });
        let java = CoreError::from(JavaError::ManagedIndexUnavailable {
            major: 21,
            source: Box::new(ephervex_download::MetadataCacheError::Unavailable {
                key: "adoptium-21".to_owned(),
                source: Box::new(canceled()),
            }),
        });
        let canceled_install = CoreError::from(JavaError::Canceled);
        let failed = CoreError::from(JavaError::ManagedArchUnsupported {
            arch: "sparcv9".to_owned(),
        });

        assert!(minecraft.is_cancellation());
        assert!(java.is_cancellation());
        assert!(canceled_install.is_cancellation());
        assert!(!failed.is_cancellation());
        assert!(!canceled_install.managed_java_would_help());
    }

    #[test]
    fn a_wrapped_storage_error_keeps_the_message_id_storage_gave_it() {
        let inner = ephervex_storage::PathError::NoHomeDirectory;
        let expected = inner.message_id();
        let wrapped = CoreError::from(inner);
        assert_eq!(wrapped.message_id(), expected);
    }

    #[test]
    fn an_actual_storage_full_write_is_classified_with_its_path() {
        let path = PathBuf::from("/instance/.launcher/instance.json");
        let error = ephervex_storage::ConfigError::Unwritable {
            path: path.clone(),
            source: std::io::Error::from(std::io::ErrorKind::StorageFull),
        };

        let classified = CoreError::from(error);

        assert!(matches!(classified, CoreError::StorageFull { .. }));
        assert_eq!(classified.message_id(), "error-storage-full");
        assert_eq!(classified.failing_path(), Some(path));
    }

    #[test]
    fn a_pack_write_that_fills_storage_uses_the_same_typed_failure() {
        let path = PathBuf::from("/instance/minecraft/mods/example.jar");
        let error = PackError::Unwritable {
            path: path.clone(),
            source: std::io::Error::from(std::io::ErrorKind::StorageFull),
        };

        let classified = CoreError::from(error);

        assert!(matches!(classified, CoreError::StorageFull { .. }));
        assert_eq!(classified.failing_path(), Some(path));
    }

    #[test]
    fn a_wrapped_minecraft_error_keeps_its_own_message_id_and_path() {
        let inner = MinecraftError::unsupported("1.7.10");
        let expected = inner.message_id();
        assert_eq!(CoreError::from(inner).message_id(), expected);

        let unreadable = MinecraftError::VersionMetadataUnreadable {
            path: PathBuf::from("/cache/ephervex/metadata/sha1/ab/abc.json"),
            source: std::io::Error::other("denied"),
        };
        assert_eq!(
            CoreError::from(unreadable).failing_path(),
            Some(PathBuf::from("/cache/ephervex/metadata/sha1/ab/abc.json")),
            "a frontend must be able to name the file without matching on MinecraftError"
        );
    }

    #[test]
    fn only_a_damaged_local_copy_is_reported_as_damaged_local_data() {
        let damaged = CoreError::from(MinecraftError::CachedVersionMetadataDamaged {
            id: "1.21.1".to_owned(),
        });
        let served = CoreError::from(MinecraftError::VersionMetadataCorrupt {
            id: "1.21.1".to_owned(),
        });

        assert!(damaged.is_damaged_local_data());
        assert!(
            !served.is_damaged_local_data(),
            "a document a provider served wrong is not a damaged local copy"
        );
        assert!(!CoreError::from(MinecraftError::unsupported("1.7.10")).is_damaged_local_data());
    }

    #[test]
    fn every_message_id_core_can_hand_out_exists_in_the_bundled_resources() {
        // The frontend resolves these through `ephervex-i18n`; an id with no
        // message renders as a placeholder in the user's face.
        for id in [
            MinecraftError::unsupported("x").message_id(),
            MinecraftError::UnknownVersion { id: String::new() }.message_id(),
            MinecraftError::VersionMetadataCorrupt { id: String::new() }.message_id(),
            MinecraftError::CachedVersionMetadataDamaged { id: String::new() }.message_id(),
            ephervex_storage::PathError::NoHomeDirectory.message_id(),
            ephervex_storage::PathError::NotUtf8 {
                path: PathBuf::new(),
            }
            .message_id(),
            ServerError::ReadinessTimeout {
                target: "127.0.0.1:25565".to_owned(),
                timeout_seconds: 120,
            }
            .message_id(),
            ServerError::ForeignListener {
                target: "127.0.0.1:25565".to_owned(),
                timeout_seconds: 120,
            }
            .message_id(),
            ServerError::ReadinessCanceled.message_id(),
            ServerError::StartCanceled.message_id(),
            ConnectError::ServerNotLinked {
                server: ephervex_model::ServerId::new(),
            }
            .message_id(),
            ConnectError::RelationshipMismatch {
                server: ephervex_model::ServerId::new(),
                requested_client: ephervex_model::InstanceId::new(),
                linked_client: ephervex_model::InstanceId::new(),
            }
            .message_id(),
            ConnectError::TargetMismatch {
                client_minecraft: "1.20.1".to_owned(),
                server_minecraft: "1.20.2".to_owned(),
                client_loader: "Forge 47.4.0".to_owned(),
                server_loader: "Forge 48.0.0".to_owned(),
            }
            .message_id(),
            ConnectError::RuntimeUncertain {
                server: ephervex_model::ServerId::new(),
            }
            .message_id(),
            ConnectError::ServerStoppedBeforeReady {
                server: ephervex_model::ServerId::new(),
            }
            .message_id(),
            ConnectError::SupervisorLost {
                server: ephervex_model::ServerId::new(),
            }
            .message_id(),
            ephervex_download::ArtifactCacheError::Busy {
                path: PathBuf::new(),
            }
            .message_id(),
            ephervex_download::ArtifactCacheError::LockUnusable {
                path: PathBuf::new(),
                source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            }
            .message_id(),
            ephervex_download::ArtifactCacheError::ScanFailed {
                path: PathBuf::new(),
                source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            }
            .message_id(),
            ephervex_model::ArtifactCacheLimitError::TooSmall.message_id(),
            CoreError::InstanceRunning {
                pid: 1,
                since: chrono::Utc::now(),
            }
            .message_id(),
            CoreError::LinkedServerRunning {
                name: String::new(),
            }
            .message_id(),
            CoreError::LinkedServerUnstoppable {
                name: String::new(),
            }
            .message_id(),
            CoreError::LinkedServerNotStopped {
                name: String::new(),
                detail: String::new(),
            }
            .message_id(),
            ephervex_model::ArtifactCacheLimitError::TooLarge {
                value: u64::MAX,
                unit: ephervex_model::ByteUnit::Tebibytes,
            }
            .message_id(),
            ephervex_model::ArtifactCacheLimitError::Malformed {
                value: String::new(),
            }
            .message_id(),
        ] {
            assert!(ephervex_i18n::has(id), "missing message id: {id}");
        }
    }

    #[test]
    fn a_refused_cache_limit_names_the_bound_the_message_reads_out() {
        let error = CoreError::from(
            ephervex_model::ArtifactCacheLimit::from_unit(
                u64::MAX,
                ephervex_model::ByteUnit::Tebibytes,
            )
            .expect_err("an overflowing budget is refused"),
        );
        assert_eq!(error.message_id(), "error-artifact-cache-limit-too-large");
        assert_eq!(
            error.message_args(),
            vec![("maximum", Arg::from("4 EiB".to_owned()))],
            "the sentence has to name the same ceiling the type enforces"
        );
        assert!(ephervex_i18n::has_attribute(error.message_id(), "action"));
    }

    #[test]
    fn a_blocked_credential_change_names_the_lock_a_user_can_be_pointed_at() {
        let path = PathBuf::from("/config/ephervex/credentials.lock");
        for error in [
            ephervex_auth::AuthError::CredentialChangeBusy { path: path.clone() },
            ephervex_auth::AuthError::CredentialChangeUnavailable { path: path.clone() },
        ] {
            let error = CoreError::from(error);
            assert_eq!(error.failing_path(), Some(path.clone()));
            assert!(ephervex_i18n::has_attribute(error.message_id(), "action"));
        }
    }

    #[test]
    fn a_busy_cache_names_the_lock_a_user_can_be_pointed_at() {
        let error = CoreError::from(ephervex_download::ArtifactCacheError::Busy {
            path: PathBuf::from("/cache/ephervex/artifact-cache.lock"),
        });
        assert_eq!(
            error.failing_path(),
            Some(PathBuf::from("/cache/ephervex/artifact-cache.lock"))
        );
    }

    /// What a `Display`-only log line threw away.
    #[test]
    fn logging_an_error_keeps_the_cause_the_outer_message_hides() {
        #[derive(Debug, thiserror::Error)]
        #[error("http 404 from https://piston-meta.mojang.com/v1/packages/x/26.1.2.json")]
        struct Fetch;

        #[derive(Debug, thiserror::Error)]
        #[error("vanilla installation failed")]
        struct Install(#[source] Fetch);

        let install = Install(Fetch);
        // The outer message alone is what the report reader was given, and it
        // names neither the artifact nor the status.
        assert_eq!(install.to_string(), "vanilla installation failed");
        assert_eq!(
            ErrorChain(&install).to_string(),
            "vanilla installation failed: http 404 from \
             https://piston-meta.mojang.com/v1/packages/x/26.1.2.json"
        );
    }

    /// reqwest renders the full request URL, query string included, in the
    /// message of its own error.
    #[tokio::test]
    async fn a_failed_transfer_logs_no_part_of_the_signed_query_string() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let url = format!("http://{address}/artifact.jar?token=request-secret");
        let source = reqwest::Client::new().get(&url).send().await.unwrap_err();
        assert!(
            source.to_string().contains("request-secret"),
            "the fixture must carry the URL the log would otherwise print"
        );

        let error = ephervex_download::DownloadError::transport(
            ephervex_download::redacted(&url.parse().unwrap()),
            source,
        );

        let logged = ErrorChain(&error).to_string();
        assert!(!logged.contains("request-secret"), "{logged}");
        assert!(logged.contains("/artifact.jar"), "{logged}");
    }

    /// An error that renders its own source must not have it repeated.
    #[test]
    fn a_cause_the_outer_message_already_prints_is_not_printed_twice() {
        #[derive(Debug, thiserror::Error)]
        #[error("Too many open files (os error 24)")]
        struct Limit;

        // The shape `DownloadError::Io` has: the message ends with its source.
        #[derive(Debug, thiserror::Error)]
        #[error("could not read the cached artifact at /objects/86/8670: {0}")]
        struct Read(#[source] Limit);

        #[derive(Debug, thiserror::Error)]
        #[error("vanilla installation failed")]
        struct Install(#[source] Read);

        assert_eq!(
            ErrorChain(&Install(Read(Limit))).to_string(),
            "vanilla installation failed: could not read the cached artifact at \
             /objects/86/8670: Too many open files (os error 24)"
        );
    }

    fn rendered(error: &CoreError) -> String {
        ephervex_i18n::lookup_args(error.message_id(), error.message_args())
    }

    #[test]
    fn a_single_incompatible_mod_selects_the_singular_message() {
        let one = rendered(&CoreError::LoaderChangeBlockedByMods { incompatible: 1 });
        let many = rendered(&CoreError::LoaderChangeBlockedByMods { incompatible: 3 });

        assert!(one.contains("1 installed mod does"), "{one}");
        assert!(many.contains("3 installed mods "), "{many}");
    }

    #[test]
    fn a_refused_preference_value_names_its_setting_in_the_message() {
        let error = CoreError::from(ephervex_options::PreferenceValueError {
            id: ephervex_options::PreferenceId::parse("fov").unwrap(),
            expected: ephervex_options::PreferenceValueShape::Decimal,
            problem: ephervex_options::PreferenceValueProblem::OutOfDomain,
        });

        let message = rendered(&error);
        assert!(message.contains("fov"), "{message}");
        assert!(!message.contains("$preference"), "{message}");
    }
}
