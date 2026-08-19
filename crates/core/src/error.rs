//! The error type frontends see.
//!
//! Frontends must not have to match on `ephervex-storage`'s error enums, or
//! `ephervex-download`'s, or any other provider's — that would make the layer
//! below `core` part of the frontend's compile-time surface, which is exactly
//! what the dependency direction exists to prevent.
//!
//! So every provider error is wrapped here. What crosses the boundary is a
//! stable message id plus the technical chain, and the frontend resolves the
//! id through `ephervex-i18n`. Nothing in this crate knows what
//! language the user reads.

use std::path::PathBuf;

use ephervex_auth::AuthError;
use ephervex_java::JavaError;
use ephervex_loaders::LoaderError;
use ephervex_minecraft::{LaunchError, MinecraftError};
use ephervex_mod_platform::ModPlatformError;
use ephervex_pack::{CredentialUnavailable, PackError};
use ephervex_server::ServerError;
use ephervex_storage::StorageError;

use crate::ConnectError;

/// Anything an application service can fail with.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CoreError {
    /// Paths, configuration, the index, or the registry.
    #[error(transparent)]
    Storage(#[from] StorageError),

    /// Mojang version metadata: the manifest, the release filter, or one
    /// version's document.
    #[error(transparent)]
    Minecraft(#[from] MinecraftError),

    /// Java runtime discovery, selection, or managed installation.
    #[error(transparent)]
    Java(#[from] JavaError),

    /// Global JVM memory settings cannot produce a valid Java command.
    #[error(
        "java memory settings are invalid: minimum {minimum_mib} MiB, maximum {maximum_mib} MiB"
    )]
    JavaSettingsInvalid { minimum_mib: u32, maximum_mib: u32 },

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

    /// Reading, validating, or materializing an imported pack.
    ///
    /// Constructed through the conversion below rather than by `#[from]`, so a
    /// pack failure that is really a provider-credential failure arrives as
    /// [`Self::ModPlatform`] and keeps the vocabulary a frontend routes on.
    #[error(transparent)]
    Pack(PackError),

    /// Searching a mod catalog, or resolving a mod's dependencies.
    #[error(transparent)]
    ModPlatform(#[from] ModPlatformError),

    /// Installing or launching a version.
    ///
    /// The typed `LaunchError::AuthenticationRequired` and `MissingJava` states are
    /// deliberately *not* in here: they arrive as [`Self::Auth`] and
    /// [`Self::Java`], and the predicates below —
    /// [`Self::requires_interactive_login`], [`Self::required_java_major`] —
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
    /// Reaches a frontend only for the operations a user asked for directly —
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
    /// Turning a vanilla instance into a modded one — or one loader into
    /// another — changes what the instance is and invalidates every mod in it.
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

    /// Working out which dependencies are still needed requires every root's
    /// project, and one of them has none.
    ///
    /// Refused rather than answered partially: a partial answer to "what is
    /// still required" reads as proof, and acting on it deletes a mod something
    /// still needs.
    #[error("`{path}` has no provider identity, so the instance's dependencies cannot be proven")]
    ModIdentityIncomplete { path: std::path::PathBuf },
}

/// Lifts a pack failure into the error frontends see.
///
/// One case is not a pack failure at all. Materializing retained desired state
/// can discover that an artifact's verified copy is no longer in the shared
/// cache and that its provider will only serve it to an authenticated request;
/// `ephervex-pack` cannot name that provider's error type, so it carries the
/// original through and this is where it is taken back out. What reaches a
/// frontend is therefore the same typed credential failure a provider request
/// would have produced — absent key, unreadable key, refused key — rather than a
/// download failure it could only report as "something went wrong".
impl From<PackError> for CoreError {
    fn from(error: PackError) -> Self {
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

impl CoreError {
    /// The `ephervex-i18n` message id describing this failure to a user.
    ///
    /// Delegating rather than re-deriving keeps one message id per failure
    /// across the whole stack, so the CLI and the GUI cannot drift into
    /// describing the same problem differently.
    #[must_use]
    pub fn message_id(&self) -> &'static str {
        match self {
            Self::Storage(error) => error.message_id(),
            Self::Minecraft(error) => error.message_id(),
            Self::Java(error) => error.message_id(),
            Self::JavaSettingsInvalid { .. } => "error-java-settings-invalid",
            Self::Loader(error) => error.message_id(),
            // Not `const`: this one chooses between message ids on the HTTP
            // status, so that a temporary outage and a permanent refusal do not
            // share a sentence.
            Self::Auth(error) => error.message_id(),
            Self::Launch(error) => error.message_id(),
            Self::Options(error) => error.message_id(),
            Self::Preference(error) => error.message_id(),
            Self::Pack(error) => error.message_id(),
            Self::ModPlatform(error) => error.message_id(),
            Self::Server(error) => error.message_id(),
            Self::Connect(error) => error.message_id(),
            Self::ArtifactCache(error) => error.message_id(),
            Self::ArtifactCacheLimitInvalid(error) => error.message_id(),
            Self::ArtworkUnusable { .. } => "error-artwork-unusable",
            Self::LoaderFamilyFixed { .. } => "error-loader-family-fixed",
            Self::LoaderChangeBlockedByMods { .. } => "error-loader-change-blocked-by-mods",
            Self::ModNotManaged { .. } => "error-mod-not-managed",
            Self::ModMutationRestricted { restriction, .. } => restriction.message_id(),
            Self::ModIdentityIncomplete { .. } => "error-mod-identity-incomplete",
            Self::ModCatalogPreferenceUnrecognized { .. } => {
                "error-mod-catalog-preference-unrecognized"
            }
        }
    }

    /// Extra values the localized message and its next step take as arguments.
    ///
    /// The same idea as [`Self::failing_path`] and [`Self::help_subject`], for
    /// the failures whose messages name several technical values. Delegated so
    /// that each provider states its own arguments beside its own message ids,
    /// and a frontend supplies them without matching on any error hierarchy.
    #[must_use]
    pub fn message_args(&self) -> Vec<(&'static str, String)> {
        match self {
            Self::Pack(error) => error.message_args(),
            Self::ModPlatform(error) => error.message_args(),
            Self::Server(error) => error.message_args(),
            Self::Connect(error) => error.message_args(),
            Self::ArtifactCacheLimitInvalid(error) => error.message_args(),
            Self::LoaderFamilyFixed { loader } => vec![("loader", loader.to_string())],
            Self::LoaderChangeBlockedByMods { incompatible } => {
                vec![("count", incompatible.to_string())]
            }
            Self::ModCatalogPreferenceUnrecognized { configured } => {
                vec![("catalog", configured.clone())]
            }
            Self::Preference(error) => vec![("syntax", error.syntax.to_owned())],
            Self::JavaSettingsInvalid {
                minimum_mib,
                maximum_mib,
            } => vec![
                ("minimum", minimum_mib.to_string()),
                ("maximum", maximum_mib.to_string()),
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

    /// Whether the user canceled rather than something failing.
    #[must_use]
    pub const fn is_cancellation(&self) -> bool {
        match self {
            Self::Auth(error) => error.is_cancellation(),
            Self::Loader(error) => error.is_cancellation(),
            Self::Launch(error) => error.is_cancellation(),
            Self::Pack(error) => error.is_cancellation(),
            Self::ModPlatform(error) => error.is_cancellation(),
            Self::Server(ServerError::ReadinessCanceled) => true,
            _ => false,
        }
    }

    /// Whether this installation cannot authenticate until its Azure
    /// application registration is approved for the Minecraft Services API.
    ///
    /// Worth a predicate of its own because it is the one authentication
    /// failure no user action inside the launcher can resolve — not signing in
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
    /// failure names one — for the approval form, the Client ID.
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
    /// that wants to say *which* loader was refused — and to record it as an
    /// attention reason — must not have to match on `PackError` to find the
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
            Self::Storage(_)
            | Self::ArtifactCache(_)
            | Self::ArtifactCacheLimitInvalid(_)
            | Self::JavaSettingsInvalid { .. }
            | Self::Minecraft(_)
            | Self::Loader(_)
            | Self::Auth(_)
            | Self::Launch(_)
            | Self::Options(_)
            | Self::Preference(_)
            | Self::ModPlatform(_)
            | Self::Pack(_)
            | Self::Server(_)
            | Self::Connect(_)
            | Self::ModNotManaged { .. }
            | Self::ModMutationRestricted { .. }
            | Self::ModIdentityIncomplete { .. }
            | Self::LoaderFamilyFixed { .. }
            | Self::LoaderChangeBlockedByMods { .. }
            | Self::ModCatalogPreferenceUnrecognized { .. }
            | Self::ArtworkUnusable { .. } => None,
        }
    }

    /// Whether downloading a managed runtime is a sensible next step.
    #[must_use]
    pub const fn managed_java_would_help(&self) -> bool {
        matches!(self, Self::Java(error) if error.managed_download_would_help())
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
            Self::Minecraft(error) => error.failing_path(),
            Self::Java(error) => error.failing_path(),
            Self::Loader(error) => error.failing_path(),
            Self::Launch(error) => error.failing_path(),
            Self::Options(error) => error.failing_path(),
            Self::Pack(error) => error.failing_path(),
            Self::Server(error) => error.failing_path(),
            // Authentication failures are never about a file. The one thing
            // that would be — the keyring entry — is not a path the user can
            // usefully be pointed at. A rejected preference value is about a
            // setting, not a file either, and a catalog failure is about a
            // project rather than anything on disk.
            Self::ModNotManaged { path }
            | Self::ModMutationRestricted { path, .. }
            | Self::ModIdentityIncomplete { path } => Some(path.clone()),
            Self::ArtifactCache(error) => error.failing_path(),
            Self::Auth(_)
            | Self::ArtifactCacheLimitInvalid(_)
            | Self::JavaSettingsInvalid { .. }
            | Self::Preference(_)
            | Self::ModPlatform(_)
            | Self::Connect(_)
            | Self::LoaderFamilyFixed { .. }
            | Self::LoaderChangeBlockedByMods { .. }
            | Self::ModCatalogPreferenceUnrecognized { .. }
            | Self::ArtworkUnusable { .. } => None,
        }
    }
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
        Self::Storage(StorageError::Diagnostics(error))
    }
}

impl From<ephervex_storage::LockError> for CoreError {
    fn from(error: ephervex_storage::LockError) -> Self {
        Self::Storage(StorageError::Lock(error))
    }
}

fn config_failing_path(error: &ephervex_storage::ConfigError) -> Option<PathBuf> {
    use ephervex_storage::ConfigError;
    match error {
        ConfigError::Unreadable { path, .. }
        | ConfigError::Invalid { path, .. }
        | ConfigError::Unwritable { path, .. } => Some(path.clone()),
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
        | RegistryError::RootMissing { path }
        | RegistryError::Io { path, .. }
        | RegistryError::Index(IndexError::Unopenable { path, .. }) => Some(path.clone()),
        RegistryError::Metadata(inner) => metadata_failing_path(inner),
        RegistryError::Path(inner) => path_failing_path(inner),
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
        | PathError::UnsafeTemporaryDirectory { path, .. } => Some(path.clone()),
        PathError::NoHomeDirectory => None,
    }
}

impl From<ephervex_storage::RegistryError> for CoreError {
    fn from(error: ephervex_storage::RegistryError) -> Self {
        Self::Storage(StorageError::Registry(error))
    }
}

impl From<ephervex_storage::ConfigError> for CoreError {
    fn from(error: ephervex_storage::ConfigError) -> Self {
        Self::Storage(StorageError::Config(error))
    }
}

impl From<ephervex_storage::PathError> for CoreError {
    fn from(error: ephervex_storage::PathError) -> Self {
        Self::Storage(StorageError::Path(error))
    }
}

/// Shorthand for a service result.
pub type CoreResult<T> = Result<T, CoreError>;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn a_wrapped_storage_error_keeps_the_message_id_storage_gave_it() {
        let inner = ephervex_storage::PathError::NoHomeDirectory;
        let expected = inner.message_id();
        let wrapped = CoreError::from(inner);
        assert_eq!(wrapped.message_id(), expected);
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
    fn every_message_id_core_can_hand_out_exists_in_the_bundled_resources() {
        // The frontend resolves these through `ephervex-i18n`; an id with no
        // message renders as a placeholder in the user's face.
        for id in [
            MinecraftError::unsupported("x").message_id(),
            MinecraftError::UnknownVersion { id: String::new() }.message_id(),
            MinecraftError::VersionMetadataCorrupt { id: String::new() }.message_id(),
            ephervex_storage::PathError::NoHomeDirectory.message_id(),
            ServerError::ReadinessTimeout {
                target: "127.0.0.1:25565".to_owned(),
                timeout_seconds: 120,
            }
            .message_id(),
            ServerError::ReadinessCanceled.message_id(),
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
            vec![("maximum", "4 EiB".to_owned())],
            "the sentence has to name the same ceiling the type enforces"
        );
        assert!(ephervex_i18n::has_attribute(error.message_id(), "action"));
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
}
