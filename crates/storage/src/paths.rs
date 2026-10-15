//! Where everything lives on disk (ADR 0003).
//!
//! Four XDG roots plus one temporary root, all keyed on the single
//! [`APP_ID`] constant so renaming the application is one line plus a migration
//! decision rather than a search-and-replace.
//!
//! # Why this is a struct rather than a set of free functions
//!
//! Free functions that read the environment cannot be pointed somewhere else,
//! which means every test that touches storage would have to run against the
//! developer's real `~/.config`, `~/.local/share`, and `~/.cache`. A test suite
//! that writes into a developer's actual instance directory is a test suite
//! nobody runs twice. [`Paths::rooted_at`] takes a [`tempfile::TempDir`] and the
//! rest of the crate cannot tell the difference.

use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use ephervex_model::{APP_ID, InstanceId, ServerId, StorageClass};

/// Failures resolving or creating application directories.
///
/// Variants carry structured data, never a pre-formatted English sentence: the
/// frontend turns [`PathError::message_id`] into text through `ephervex-i18n`
#[derive(Debug, thiserror::Error)]
pub enum PathError {
    /// Neither `HOME` nor the XDG variables told us where the user lives.
    #[error("could not determine the user's home directory")]
    NoHomeDirectory,
    /// A directory the launcher needs could not be created.
    #[error("could not create directory {path}")]
    DirectoryCreation {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The temporary root exists but cannot safely hold private instances.
    #[error("the temporary directory {path} is unsafe: {reason}")]
    UnsafeTemporaryDirectory { path: PathBuf, reason: &'static str },
    /// A path to persist is not valid UTF-8.
    ///
    /// Instance records and the configuration file are JSON and TOML, which can
    /// only hold text. Storing such a path would either fail late or turn it
    /// into a different path, so it is refused before anything is written.
    /// Commands that only read are unaffected.
    #[error("the path {} is not valid UTF-8", .path.display())]
    NotUtf8 { path: PathBuf },
}

impl PathError {
    /// The `ephervex-i18n` message id describing this failure to a user.
    #[must_use]
    pub const fn message_id(&self) -> &'static str {
        match self {
            Self::NoHomeDirectory => "error-paths-no-home-directory",
            Self::DirectoryCreation { .. } | Self::UnsafeTemporaryDirectory { .. } => {
                "error-paths-directory-creation"
            }
            Self::NotUtf8 { .. } => "error-paths-not-utf8",
        }
    }
}

/// The text of `path`, or [`PathError::NotUtf8`] when it has no lossless text form.
///
/// The one check for every path the launcher persists as text, so that a
/// setting never reads back as a different path than the one that was chosen.
pub fn utf8_path(path: &Path) -> Result<&str, PathError> {
    path.to_str().ok_or_else(|| PathError::NotUtf8 {
        path: path.to_path_buf(),
    })
}

/// The file name of the launcher configuration inside the config root.
pub const CONFIG_FILE_NAME: &str = "config.toml";
/// The file name of the SQLite index inside the state root.
pub const DATABASE_FILE_NAME: &str = "launcher.sqlite";
/// The launcher-owned metadata directory inside every instance and server root
/// (ADR 0002).
pub const LAUNCHER_METADATA_DIR: &str = ".launcher";

/// Resolved application directories.
///
/// Cheap to clone and carried by [`crate::Registry`] rather than re-resolved,
/// so a process cannot end up reading configuration from one place and writing
/// instances to another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    config: PathBuf,
    data: PathBuf,
    cache: PathBuf,
    state: PathBuf,
    temp: PathBuf,
    persistent_instance_root: Option<PathBuf>,
    /// Where desktop integration tools run when these are not the user's own
    /// roots. `None` means the user's session.
    desktop_home: Option<PathBuf>,
}

impl Paths {
    /// Resolves the XDG roots from the environment.
    ///
    /// Pure resolution: nothing is created and nothing is read, so calling this
    /// in a test does not touch the developer's real directories. Use
    /// [`Paths::ensure_base_directories`] when the directories are actually
    /// needed.
    pub fn from_environment() -> Result<Self, PathError> {
        // `from_path` rather than `from(qualifier, organization, application)`:
        // the latter lowercases and rewrites the components on some platforms,
        // and the whole point of APP_ID is that the on-disk name is exactly the
        // constant.
        let dirs =
            ProjectDirs::from_path(PathBuf::from(APP_ID)).ok_or(PathError::NoHomeDirectory)?;

        Ok(Self {
            config: dirs.config_dir().to_path_buf(),
            data: dirs.data_dir().to_path_buf(),
            cache: dirs.cache_dir().to_path_buf(),
            // `state_dir` is `None` only on platforms without an XDG state
            // directory. Linux always has one; falling back to the data root
            // keeps the type honest without inventing a second code path.
            state: dirs
                .state_dir()
                .map_or_else(|| dirs.data_dir().join("state"), Path::to_path_buf),
            temp: Self::os_temp_root(),
            persistent_instance_root: None,
            desktop_home: None,
        })
    }

    /// Builds a resolver from explicit roots.
    ///
    /// This is the injection point that lets the whole crate be tested without
    /// a real home directory.
    #[must_use]
    pub fn from_roots(
        config: impl Into<PathBuf>,
        data: impl Into<PathBuf>,
        cache: impl Into<PathBuf>,
        state: impl Into<PathBuf>,
        temp: impl Into<PathBuf>,
    ) -> Self {
        let data = data.into();
        // Beside the data root, where the desktop entries of these roots go.
        let desktop_home = data.parent().unwrap_or(data.as_path()).to_path_buf();
        Self {
            config: config.into(),
            data,
            cache: cache.into(),
            state: state.into(),
            temp: temp.into(),
            persistent_instance_root: None,
            desktop_home: Some(desktop_home),
        }
    }

    /// Lays all five roots out beneath one directory.
    ///
    /// Intended for tests and for a future portable/self-contained mode. The
    /// temporary root is a sibling of the others here, which is deliberate:
    /// a test that wants to simulate the OS clearing `/tmp` needs to be able to
    /// delete it without deleting the real one.
    #[must_use]
    pub fn rooted_at(base: impl AsRef<Path>) -> Self {
        let base = base.as_ref();
        Self::from_roots(
            base.join("config"),
            base.join("data"),
            base.join("cache"),
            base.join("state"),
            base.join("temp").join(APP_ID),
        )
    }

    /// The OS temporary root for this user and application.
    ///
    /// Always derived from [`std::env::temp_dir`], never from a hardcoded
    /// `/tmp` - that is what makes `TMPDIR` work, and it keeps the code honest
    /// about what it depends on (ADR 0003).
    #[must_use]
    pub fn os_temp_root() -> PathBuf {
        #[cfg(unix)]
        let name = format!("{APP_ID}-{}", rustix::process::getuid().as_raw());
        #[cfg(not(unix))]
        let name = APP_ID;
        std::env::temp_dir().join(name)
    }

    /// Overrides where persistent instances are stored.
    ///
    /// The persistent instance root is the *only* root a user may relocate.
    /// Config, cache, and state stay where XDG says, because
    /// moving those breaks the guarantees other tools make about them.
    #[must_use]
    pub fn with_persistent_instance_root(mut self, root: Option<PathBuf>) -> Self {
        self.persistent_instance_root = root;
        self
    }

    /// `$XDG_CONFIG_HOME/ephervex/`. Never contains a secret (ADR 0004).
    #[must_use]
    pub fn config_dir(&self) -> &Path {
        &self.config
    }

    /// `$XDG_CONFIG_HOME/ephervex/config.toml`.
    #[must_use]
    pub fn config_file(&self) -> PathBuf {
        self.config.join(CONFIG_FILE_NAME)
    }

    /// `$XDG_DATA_HOME/ephervex/`.
    #[must_use]
    pub fn data_dir(&self) -> &Path {
        &self.data
    }

    /// `$XDG_CACHE_HOME/ephervex/`. Disposable by definition: deleting it must
    /// never break an installed instance beyond forcing re-downloads.
    #[must_use]
    pub fn cache_dir(&self) -> &Path {
        &self.cache
    }

    /// `$XDG_STATE_HOME/ephervex/`.
    #[must_use]
    pub fn state_dir(&self) -> &Path {
        &self.state
    }

    /// `<temp>/ephervex-<uid>/`, where `<temp>` is [`std::env::temp_dir`].
    #[must_use]
    pub fn temp_dir(&self) -> &Path {
        &self.temp
    }

    /// The XDG applications directory, where desktop entries live.
    ///
    /// A *sibling* of Ephervex's own data directory rather than a child of it:
    /// [`Self::data_dir`] is `$XDG_DATA_HOME/ephervex`, and the place
    /// `update-desktop-database` and `xdg-mime` read is
    /// `$XDG_DATA_HOME/applications`. An entry written under the application's
    /// own directory is one the desktop never finds.
    #[must_use]
    pub fn desktop_entries_dir(&self) -> PathBuf {
        self.data
            .parent()
            .unwrap_or(self.data.as_path())
            .join("applications")
    }

    /// The environment desktop integration tools such as `xdg-mime` run with.
    ///
    /// Empty for the roots resolved from the environment, so a tool acts on the
    /// user's session. Explicit roots keep the tools beside them instead: the
    /// tools rewrite the session's `mimeapps.list` and desktop caches, which
    /// nothing run against throwaway roots may touch.
    #[must_use]
    pub fn desktop_tool_environment(&self) -> Vec<(&'static str, PathBuf)> {
        self.desktop_home.as_ref().map_or_else(Vec::new, |home| {
            vec![
                ("HOME", home.clone()),
                ("XDG_DATA_HOME", home.clone()),
                ("XDG_CONFIG_HOME", home.join(".config")),
                ("XDG_CACHE_HOME", home.join(".cache")),
                ("XDG_STATE_HOME", home.join(".local").join("state")),
            ]
        })
    }

    /// Where persistent client instances live.
    ///
    /// The configured override if there is one, otherwise
    /// `$XDG_DATA_HOME/ephervex/instances/`.
    #[must_use]
    pub fn persistent_instances_dir(&self) -> PathBuf {
        self.persistent_instance_root
            .clone()
            .unwrap_or_else(|| self.data.join("instances"))
    }

    /// Whether the persistent instance root has been moved away from the
    /// default. Reported in `ephervex info` so a confused user can see it.
    #[must_use]
    pub fn persistent_instance_root_is_overridden(&self) -> bool {
        self.persistent_instance_root.is_some()
    }

    /// Where persistent servers live.
    #[must_use]
    pub fn persistent_servers_dir(&self) -> PathBuf {
        // Servers follow the instance root when it moves: a user who relocated
        // instances onto a bigger disk meant their servers too.
        self.persistent_instance_root.as_ref().map_or_else(
            || self.data.join("servers"),
            |root| {
                root.parent()
                    .map_or_else(|| root.join("servers"), |parent| parent.join("servers"))
            },
        )
    }

    /// Managed Java runtimes (`$XDG_DATA_HOME/ephervex/java/`).
    #[must_use]
    pub fn java_dir(&self) -> PathBuf {
        self.data.join("java")
    }

    /// Files shared between instances (`$XDG_DATA_HOME/ephervex/shared/`).
    #[must_use]
    pub fn shared_dir(&self) -> PathBuf {
        self.data.join("shared")
    }

    /// Imported host icons (`$XDG_DATA_HOME/ephervex/icons/`), one
    /// content-addressed PNG each.
    #[must_use]
    pub fn icons_dir(&self) -> PathBuf {
        self.data.join("icons")
    }

    /// The stored PNG for one imported icon.
    #[must_use]
    pub fn icon_file(&self, icon: &crate::organization::IconId) -> PathBuf {
        self.icons_dir().join(icon.file_name())
    }

    /// `$XDG_DATA_HOME/ephervex/organization.json`: favorites, groups and
    /// icons, kept apart from host metadata (ADR 0048).
    #[must_use]
    pub fn organization_file(&self) -> PathBuf {
        self.data.join(crate::organization::ORGANIZATION_FILE_NAME)
    }

    /// In-progress and completed downloads.
    #[must_use]
    pub fn downloads_cache_dir(&self) -> PathBuf {
        self.cache.join("downloads")
    }

    /// Cached provider metadata documents.
    #[must_use]
    pub fn metadata_cache_dir(&self) -> PathBuf {
        self.cache.join("metadata")
    }

    /// Shared Minecraft assets.
    #[must_use]
    pub fn assets_cache_dir(&self) -> PathBuf {
        self.cache.join("assets")
    }

    /// Shared Minecraft and loader libraries.
    #[must_use]
    pub fn libraries_cache_dir(&self) -> PathBuf {
        self.cache.join("libraries")
    }

    /// Downloaded loader installers.
    #[must_use]
    pub fn installer_cache_dir(&self) -> PathBuf {
        self.cache.join("installer-cache")
    }

    /// Natives unpacked per Minecraft version and platform.
    ///
    /// Under the cache rather than under `shared/` because the contents are
    /// re-derivable from the libraries store: deleting it costs an unpack, not
    /// an instance, which is exactly the promise the cache root makes.
    #[must_use]
    pub fn natives_cache_dir(&self) -> PathBuf {
        self.cache.join("natives")
    }

    /// `$XDG_STATE_HOME/ephervex/launcher.sqlite`.
    #[must_use]
    pub fn database_file(&self) -> PathBuf {
        self.state.join(DATABASE_FILE_NAME)
    }

    /// `$XDG_STATE_HOME/ephervex/moves/`, holding one guard file per host a
    /// snapshot restore or storage move has claimed, and one per host whose
    /// recipe a change has claimed.
    #[must_use]
    pub(crate) fn move_guards_dir(&self) -> PathBuf {
        self.state.join("moves")
    }

    /// `$XDG_STATE_HOME/ephervex/logs/`.
    #[must_use]
    pub fn logs_dir(&self) -> PathBuf {
        self.state.join("logs")
    }

    /// `$XDG_STATE_HOME/ephervex/logs/launcher/`, where the launcher's own
    /// diagnostics are written.
    ///
    /// A subdirectory rather than the log root because the root also holds the
    /// per-server output capture, and browsing the launcher's log should not
    /// mean paging past every server's directory.
    #[must_use]
    pub fn launcher_logs_dir(&self) -> PathBuf {
        self.logs_dir().join("launcher")
    }

    /// Where one server's captured process output is kept.
    ///
    /// Under the state root rather than inside the server directory: the
    /// capture must survive a server whose files are being reinstalled, and a
    /// temporary server's output is worth keeping after the OS clears its root.
    #[must_use]
    pub fn server_capture_dir(&self, id: ServerId) -> PathBuf {
        self.logs_dir().join("servers").join(id.to_string())
    }

    /// `<temp>/ephervex-<uid>/instances/`.
    #[must_use]
    pub fn temporary_instances_dir(&self) -> PathBuf {
        self.temp.join("instances")
    }

    /// `<temp>/ephervex-<uid>/servers/`.
    ///
    /// Temporary servers live under the OS temp root for the same reason
    /// temporary instances do.
    #[must_use]
    pub fn temporary_servers_dir(&self) -> PathBuf {
        self.temp.join("servers")
    }

    /// The directory holding client instances of the given storage class.
    #[must_use]
    pub fn instances_dir(&self, storage: StorageClass) -> PathBuf {
        match storage {
            StorageClass::Temporary => self.temporary_instances_dir(),
            StorageClass::Persistent => self.persistent_instances_dir(),
        }
    }

    /// The directory holding servers of the given storage class.
    #[must_use]
    pub fn servers_dir(&self, storage: StorageClass) -> PathBuf {
        match storage {
            StorageClass::Temporary => self.temporary_servers_dir(),
            StorageClass::Persistent => self.persistent_servers_dir(),
        }
    }

    /// The root of one client instance.
    ///
    /// Named by id rather than by the user's chosen name: names are not unique,
    /// may be renamed, and arrive from untrusted pack manifests.
    #[must_use]
    pub fn instance_root(&self, storage: StorageClass, id: InstanceId) -> PathBuf {
        self.instances_dir(storage).join(id.to_string())
    }

    /// The root of one server.
    #[must_use]
    pub fn server_root(&self, storage: StorageClass, id: ServerId) -> PathBuf {
        self.servers_dir(storage).join(id.to_string())
    }

    /// Creates the directories the launcher expects to exist.
    ///
    /// Called once at startup. Temporary roots are included because the OS may
    /// have removed them since the last run - that is the whole premise of
    /// ADR 0006.
    pub fn ensure_base_directories(&self) -> Result<(), PathError> {
        for directory in [
            self.config.clone(),
            self.data.clone(),
            self.cache.clone(),
            self.state.clone(),
            self.persistent_instances_dir(),
            self.persistent_servers_dir(),
            self.java_dir(),
            self.shared_dir(),
            self.icons_dir(),
            self.downloads_cache_dir(),
            self.metadata_cache_dir(),
            self.assets_cache_dir(),
            self.libraries_cache_dir(),
            self.installer_cache_dir(),
            self.natives_cache_dir(),
            self.logs_dir(),
            self.launcher_logs_dir(),
        ] {
            create_dir_all(&directory)?;
        }
        ensure_private_temporary_directory(&self.temp)?;
        ensure_private_temporary_directory(&self.temporary_instances_dir())?;
        ensure_private_temporary_directory(&self.temporary_servers_dir())?;
        Ok(())
    }
}

#[cfg(unix)]
fn ensure_private_temporary_directory(path: &Path) -> Result<(), PathError> {
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};

    if let Some(parent) = path.parent() {
        create_dir_all(parent)?;
    }

    match std::fs::symlink_metadata(path) {
        Ok(_) => {}
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = std::fs::DirBuilder::new();
            builder.mode(0o700);
            builder
                .create(path)
                .map_err(|source| PathError::DirectoryCreation {
                    path: path.to_path_buf(),
                    source,
                })?;
        }
        Err(source) => {
            return Err(PathError::DirectoryCreation {
                path: path.to_path_buf(),
                source,
            });
        }
    }

    let metadata =
        std::fs::symlink_metadata(path).map_err(|source| PathError::DirectoryCreation {
            path: path.to_path_buf(),
            source,
        })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(PathError::UnsafeTemporaryDirectory {
            path: path.to_path_buf(),
            reason: "it is not a real directory",
        });
    }
    if metadata.uid() != rustix::process::getuid().as_raw() {
        return Err(PathError::UnsafeTemporaryDirectory {
            path: path.to_path_buf(),
            reason: "it is owned by another user",
        });
    }

    if metadata.mode() & 0o077 != 0 {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).map_err(
            |source| PathError::DirectoryCreation {
                path: path.to_path_buf(),
                source,
            },
        )?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_private_temporary_directory(path: &Path) -> Result<(), PathError> {
    create_dir_all(path)
}

/// `create_dir_all` with the path attached to the error, because "permission
/// denied" without a path is a bug report nobody can act on.
pub(crate) fn create_dir_all(path: &Path) -> Result<(), PathError> {
    std::fs::create_dir_all(path).map_err(|source| PathError::DirectoryCreation {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn xdg_roots_are_resolved_beneath_the_app_id() {
        let paths = Paths::from_environment().expect("a home directory exists in the test env");

        for root in [
            paths.config_dir(),
            paths.data_dir(),
            paths.cache_dir(),
            paths.state_dir(),
        ] {
            assert_eq!(
                root.file_name().and_then(std::ffi::OsStr::to_str),
                Some(APP_ID),
                "{root:?} should end in the app id"
            );
            assert!(root.is_absolute(), "{root:?} should be absolute");
        }

        assert_eq!(paths.config_file().file_name().unwrap(), CONFIG_FILE_NAME);
        assert_eq!(
            paths.database_file().file_name().unwrap(),
            DATABASE_FILE_NAME
        );
        assert_eq!(paths.database_file().parent().unwrap(), paths.state_dir());
    }

    #[test]
    fn the_four_xdg_roots_are_distinct_from_each_other() {
        let paths = Paths::from_environment().expect("a home directory exists in the test env");

        // Config, cache, and state have different lifetimes and different
        // backup expectations. Collapsing any two of them would mean clearing
        // the cache could delete instances.
        let roots = [
            paths.config_dir(),
            paths.data_dir(),
            paths.cache_dir(),
            paths.state_dir(),
        ];
        for (index, root) in roots.iter().enumerate() {
            for other in &roots[index + 1..] {
                assert_ne!(root, other);
            }
        }
    }

    #[test]
    fn the_temporary_root_comes_from_the_os_temp_dir_and_is_never_hardcoded() {
        // `std::env::temp_dir()` is what honors `TMPDIR`; the assertion that
        // we actually delegate to it is the point of this test. The end-to-end
        // proof that a different `TMPDIR` relocates instances lives in the CLI
        // integration test, which can set the variable in a child process
        // instead of mutating this one.
        #[cfg(unix)]
        let expected =
            std::env::temp_dir().join(format!("{APP_ID}-{}", rustix::process::getuid().as_raw()));
        #[cfg(not(unix))]
        let expected = std::env::temp_dir().join(APP_ID);
        assert_eq!(Paths::os_temp_root(), expected);

        let paths = Paths::from_environment().expect("a home directory exists in the test env");
        assert_eq!(paths.temp_dir(), expected);
        assert!(
            paths
                .temporary_instances_dir()
                .starts_with(std::env::temp_dir()),
            "temporary instances must live under the OS temp dir"
        );
    }

    #[test]
    fn temporary_and_persistent_instance_roots_resolve_to_different_places() {
        let paths = Paths::rooted_at("/example");
        let id = InstanceId::new();

        let temporary = paths.instance_root(StorageClass::Temporary, id);
        let persistent = paths.instance_root(StorageClass::Persistent, id);

        assert_eq!(
            temporary,
            Path::new("/example/temp/ephervex/instances").join(id.to_string())
        );
        assert_eq!(
            persistent,
            Path::new("/example/data/instances").join(id.to_string())
        );
        assert_ne!(temporary, persistent);
    }

    #[test]
    fn instance_roots_are_named_by_id_rather_than_by_user_supplied_name() {
        // Names come from pack manifests and from users; ids do not, so they
        // cannot contain a path separator or `..`.
        let paths = Paths::rooted_at("/example");
        let id = InstanceId::new();
        assert_eq!(
            paths
                .instance_root(StorageClass::Persistent, id)
                .file_name()
                .and_then(std::ffi::OsStr::to_str),
            Some(id.to_string().as_str())
        );
    }

    #[test]
    fn only_the_persistent_instance_root_is_overridable() {
        let base = Paths::rooted_at("/example");
        let overridden = base
            .clone()
            .with_persistent_instance_root(Some(PathBuf::from("/mnt/big/minecraft")));

        assert_eq!(
            overridden.persistent_instances_dir(),
            Path::new("/mnt/big/minecraft")
        );
        assert!(overridden.persistent_instance_root_is_overridden());

        // Everything else is untouched: relocating config or cache would break
        // the guarantees other tools make about those directories.
        assert_eq!(overridden.config_dir(), base.config_dir());
        assert_eq!(overridden.cache_dir(), base.cache_dir());
        assert_eq!(overridden.state_dir(), base.state_dir());
        assert_eq!(overridden.temp_dir(), base.temp_dir());
        assert_eq!(
            overridden.instances_dir(StorageClass::Temporary),
            base.instances_dir(StorageClass::Temporary)
        );
    }

    #[test]
    fn servers_follow_the_instance_root_when_it_is_relocated() {
        let paths = Paths::rooted_at("/example")
            .with_persistent_instance_root(Some(PathBuf::from("/mnt/big/instances")));
        assert_eq!(
            paths.persistent_servers_dir(),
            Path::new("/mnt/big/servers")
        );
    }

    #[test]
    fn cache_subdirectories_match_the_documented_layout() {
        let paths = Paths::rooted_at("/example");
        assert_eq!(
            paths.downloads_cache_dir(),
            Path::new("/example/cache/downloads")
        );
        assert_eq!(
            paths.metadata_cache_dir(),
            Path::new("/example/cache/metadata")
        );
        assert_eq!(paths.assets_cache_dir(), Path::new("/example/cache/assets"));
        assert_eq!(
            paths.libraries_cache_dir(),
            Path::new("/example/cache/libraries")
        );
        assert_eq!(
            paths.installer_cache_dir(),
            Path::new("/example/cache/installer-cache")
        );
        assert_eq!(
            paths.natives_cache_dir(),
            Path::new("/example/cache/natives")
        );
        assert_eq!(paths.java_dir(), Path::new("/example/data/java"));
        assert_eq!(paths.shared_dir(), Path::new("/example/data/shared"));
        assert_eq!(paths.icons_dir(), Path::new("/example/data/icons"));
        assert_eq!(paths.logs_dir(), Path::new("/example/state/logs"));
        assert_eq!(
            paths.launcher_logs_dir(),
            Path::new("/example/state/logs/launcher")
        );
    }

    #[test]
    fn launcher_and_server_diagnostics_do_not_share_a_directory() {
        // Both live under the state log root, and neither may list the other's
        // files when a user browses one of them.
        let paths = Paths::rooted_at("/example");
        let server = paths.server_capture_dir(ServerId::new());
        assert!(server.starts_with(paths.logs_dir()));
        assert!(paths.launcher_logs_dir().starts_with(paths.logs_dir()));
        assert!(!server.starts_with(paths.launcher_logs_dir()));
    }

    #[test]
    fn ensure_base_directories_creates_every_root_it_promises() {
        let base = tempfile::tempdir().expect("temp dir");
        let paths = Paths::rooted_at(base.path());
        paths.ensure_base_directories().expect("create dirs");

        for directory in [
            paths.config_dir().to_path_buf(),
            paths.data_dir().to_path_buf(),
            paths.cache_dir().to_path_buf(),
            paths.state_dir().to_path_buf(),
            paths.persistent_instances_dir(),
            paths.temporary_instances_dir(),
            paths.temporary_servers_dir(),
            paths.logs_dir(),
        ] {
            assert!(directory.is_dir(), "{directory:?} should exist");
        }

        // Idempotent: startup runs this on every launch.
        paths.ensure_base_directories().expect("second run");
    }

    #[test]
    fn desktop_tools_stay_inside_explicit_roots_and_use_the_session_otherwise() {
        let base = tempfile::tempdir().expect("temp dir");
        let paths = Paths::rooted_at(base.path());
        let environment = paths.desktop_tool_environment();
        for name in [
            "HOME",
            "XDG_DATA_HOME",
            "XDG_CONFIG_HOME",
            "XDG_CACHE_HOME",
            "XDG_STATE_HOME",
        ] {
            let (_, value) = environment
                .iter()
                .find(|(variable, _)| *variable == name)
                .unwrap_or_else(|| panic!("{name} is set"));
            assert!(value.starts_with(base.path()), "{name} is {value:?}");
        }
        let data_home = environment
            .iter()
            .find(|(variable, _)| *variable == "XDG_DATA_HOME")
            .map(|(_, value)| value.join("applications"));
        assert_eq!(data_home, Some(paths.desktop_entries_dir()));

        let session = Paths::from_environment().expect("a home directory");
        assert!(session.desktop_tool_environment().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn temporary_roots_are_private_to_the_current_user() {
        use std::os::unix::fs::PermissionsExt as _;

        let base = tempfile::tempdir().expect("temp dir");
        let paths = Paths::rooted_at(base.path());
        paths.ensure_base_directories().expect("create dirs");

        for directory in [
            paths.temp_dir().to_path_buf(),
            paths.temporary_instances_dir(),
            paths.temporary_servers_dir(),
        ] {
            let mode = std::fs::symlink_metadata(&directory)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o700, "{directory:?} must be private");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_cannot_be_used_as_the_temporary_root() {
        let base = tempfile::tempdir().expect("temp dir");
        let elsewhere = tempfile::tempdir().expect("outside dir");
        let paths = Paths::from_roots(
            base.path().join("config"),
            base.path().join("data"),
            base.path().join("cache"),
            base.path().join("state"),
            base.path().join("temp-link"),
        );
        std::os::unix::fs::symlink(elsewhere.path(), paths.temp_dir()).expect("symlink");

        assert!(matches!(
            paths.ensure_base_directories(),
            Err(PathError::UnsafeTemporaryDirectory { .. })
        ));
    }

    #[test]
    fn path_errors_carry_a_message_id_rather_than_english_prose() {
        assert_eq!(
            PathError::NoHomeDirectory.message_id(),
            "error-paths-no-home-directory"
        );
        assert_eq!(
            PathError::NotUtf8 {
                path: PathBuf::new()
            }
            .message_id(),
            "error-paths-not-utf8"
        );
    }

    #[test]
    fn a_utf8_path_is_returned_as_the_text_it_was() {
        assert_eq!(
            utf8_path(Path::new("/srv/ephervex")).unwrap(),
            "/srv/ephervex"
        );
    }
}
