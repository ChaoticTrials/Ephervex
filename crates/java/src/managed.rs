//! Installing a runtime Ephervex owns.
//!
//! # No root, ever
//!
//! Everything lands under
//! `$XDG_DATA_HOME/ephervex/java/<major>/<vendor-build>/` - the user's own data
//! directory. There is no package manager, no `sudo`, and no write outside that
//! tree. That is not a convenience: a bug-reproduction tool that needs
//! administrator rights to try a different Java is a tool nobody will use
//! twice.
//!
//! # The four steps, and why each is where it is
//!
//! 1. **Resolve** the release through [`AdoptiumClient`], which reads the API
//!    through the shared metadata cache.
//! 2. **Download** the archive through `ephervex-download`, giving it the
//!    SHA-256 Adoptium published. Atomic finalization and hash verification are
//!    properties of the download manager, so a partial or wrong archive can
//!    never be sitting at the cache path - which is what makes step 3 safe to
//!    skip when the file is already there.
//! 3. **Extract** through [`crate::archive`], which enforces the security rules. The
//!    archive is remote input; the fact that it came from Adoptium over HTTPS
//!    is a reason to *expect* it to be well-formed, not a reason to skip
//!    checking.
//! 4. **Probe** the unpacked `bin/java` and confirm it is the major that was
//!    asked for, before it is renamed into place. An install that produces
//!    something unusable must fail here, not at launch.
//!
//! # Extraction is not done in place
//!
//! Files land in a hidden `.incomplete` sibling that belongs to one attempt, so
//! concurrent installs never share one, and are renamed into position only once
//! the probe succeeds. Discovery skips hidden entries, so an interrupted install
//! leaves no half-unpacked JDK at a path that discovery would find and offer.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ephervex_download::{
    CacheLayout, ContentStore, DownloadManager, DownloadRequest, FileExtension, OperationReporter,
};
use tempfile::TempDir;
use tokio::fs;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, instrument, warn};

use crate::adoptium::{AdoptiumArchitecture, AdoptiumClient, ManagedRelease};
use crate::archive::{ArchiveError, ExtractionLimits, extract_tar_gz_until_canceled};
use crate::discovery::{JavaOrigin, JavaRuntime, JavaSourceKind};
use crate::error::{JavaError, ProbeError};
use crate::install_lock::InstallLock;
use crate::probe::{ProbeReport, probe};

/// Ends the name of every staging directory.
const STAGING_SUFFIX: &str = ".incomplete";

/// The length of the random part that makes a staging directory one attempt's own.
const STAGING_RANDOM_LEN: usize = 6;

/// A runtime Ephervex downloaded and installed.
#[derive(Debug, Clone)]
pub struct ManagedInstallation {
    pub runtime: JavaRuntime,
    pub release: ManagedRelease,
    pub directory: PathBuf,
    /// False when the runtime was already installed and nothing was fetched.
    pub newly_installed: bool,
}

/// Downloads and installs Temurin runtimes into the launcher's data directory.
#[derive(Debug, Clone)]
pub struct ManagedJavaProvider {
    client: AdoptiumClient,
    manager: DownloadManager,
    layout: CacheLayout,
    java_dir: PathBuf,
    limits: ExtractionLimits,
}

impl ManagedJavaProvider {
    /// Builds a provider installing into `java_dir` - `Paths::java_dir()`.
    pub fn new(
        manager: DownloadManager,
        layout: CacheLayout,
        java_dir: impl Into<PathBuf>,
    ) -> Result<Self, JavaError> {
        Ok(Self {
            client: AdoptiumClient::new(manager.clone(), layout.clone())?,
            manager,
            layout,
            java_dir: java_dir.into(),
            limits: ExtractionLimits::default(),
        })
    }

    /// Replaces the API root. For tests against a loopback server.
    pub fn with_api_base(mut self, base: &str) -> Result<Self, JavaError> {
        self.client = AdoptiumClient::with_base(self.manager.clone(), self.layout.clone(), base)?;
        Ok(self)
    }

    #[must_use]
    pub const fn with_limits(mut self, limits: ExtractionLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Where a major's builds live.
    #[must_use]
    pub fn major_directory(&self, major: u32) -> PathBuf {
        self.java_dir.join(major.to_string())
    }

    /// Deletes a runtime this provider installed, and reports its directory.
    ///
    /// The path comes from a frontend and what happens to it is a recursive
    /// delete, so it is checked rather than trusted: only a
    /// `<java_dir>/<major>/<build>` directory is ever removed. A runtime found
    /// on the system belongs to whoever installed it, and the rest of the data
    /// directory is not Java's to delete.
    ///
    /// The tree is moved aside under a staging name before it is deleted, and
    /// under the release lock, so an install never meets a half-deleted runtime
    /// and an interrupted removal leaves only a staging directory.
    #[instrument(skip(self))]
    pub async fn remove(&self, executable: &Path) -> Result<PathBuf, JavaError> {
        let directory = self.installed_directory(executable)?;
        let (Some(major_directory), Some(name)) = (
            directory.parent(),
            directory.file_name().and_then(|name| name.to_str()),
        ) else {
            return Err(JavaError::NotManaged {
                path: executable.to_path_buf(),
            });
        };
        let identity = |metadata: std::fs::Metadata| {
            use std::os::unix::fs::MetadataExt as _;
            (metadata.dev(), metadata.ino())
        };
        let chosen = fs::metadata(&directory).await.ok().map(identity);
        let _lock = lock_release(major_directory, name, &CancellationToken::new()).await?;
        // An install may have replaced it while this waited, and the runtime
        // that was chosen is then already gone.
        if fs::metadata(&directory).await.ok().map(identity) != chosen {
            debug!(path = %directory.display(), "the managed java runtime was replaced before its removal");
            return Ok(directory);
        }
        let retired = new_staging(major_directory, name)?;
        // Onto an empty directory, which a rename replaces.
        fs::rename(&directory, retired.path())
            .await
            .map_err(|source| JavaError::Io {
                path: directory.clone(),
                source,
            })?;
        // The runtime is gone from its path either way. A tree that cannot be
        // deleted now is left under its staging name.
        discard(retired).await;
        info!(path = %directory.display(), "removed a managed java runtime");
        Ok(directory)
    }

    /// The install directory an executable belongs to, once it is established
    /// that this provider is the one that put it there.
    fn installed_directory(&self, executable: &Path) -> Result<PathBuf, JavaError> {
        let not_managed = || JavaError::NotManaged {
            path: executable.to_path_buf(),
        };
        let home = crate::discovery::jdk_home_of(executable).ok_or_else(not_managed)?;
        // Against the resolved paths, so that neither a link nor a `..` can
        // present a directory elsewhere as one of this provider's own.
        let root = std::fs::canonicalize(&self.java_dir).map_err(|_| not_managed())?;
        let home = std::fs::canonicalize(&home).map_err(|_| not_managed())?;
        // Exactly `<root>/<major>/<build>`: one level shallower is every build
        // of a major, and the root itself is every runtime there is.
        let major = home.parent().ok_or_else(not_managed)?;
        if major.parent() != Some(root.as_path()) || !home.is_dir() {
            return Err(not_managed());
        }
        Ok(home)
    }

    /// Installs the newest Temurin build of one major, or returns the one
    /// already installed.
    #[instrument(skip(self, reporter, cancel))]
    pub async fn install(
        &self,
        major: u32,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> Result<ManagedInstallation, JavaError> {
        let architecture =
            AdoptiumArchitecture::current().ok_or_else(|| JavaError::ManagedArchUnsupported {
                arch: std::env::consts::ARCH.to_owned(),
            })?;
        let release = self
            .client
            .latest_release(major, architecture, reporter, cancel)
            .await?;
        self.install_release(release, reporter, cancel).await
    }

    /// Installs the newest GA runtime whose update is at most `maximum_update`.
    /// Used only when a loader compatibility rule declares an upper bound.
    #[instrument(skip(self, reporter, cancel))]
    pub async fn install_at_most_update(
        &self,
        major: u32,
        maximum_update: u32,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> Result<ManagedInstallation, JavaError> {
        let architecture =
            AdoptiumArchitecture::current().ok_or_else(|| JavaError::ManagedArchUnsupported {
                arch: std::env::consts::ARCH.to_owned(),
            })?;
        let exclusive = maximum_update.saturating_add(1);
        let release = self
            .client
            .release_before_update(major, exclusive, architecture, reporter, cancel)
            .await?;
        self.install_release(release, reporter, cancel).await
    }

    async fn install_release(
        &self,
        release: ManagedRelease,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> Result<ManagedInstallation, JavaError> {
        let major = release.major;
        let major_directory = self.major_directory(major);
        let name = release.install_directory_name();
        let directory = major_directory.join(&name);
        let executable = directory.join("bin").join("java");
        let finish = |runtime, newly_installed| ManagedInstallation {
            runtime,
            release: release.clone(),
            directory: directory.clone(),
            newly_installed,
        };

        // A failure here is decided under the lock, where a removal or
        // replacement cannot be renaming the tree away mid-check.
        if let Ok(Installed::Runtime(runtime)) =
            self.installed(&directory, &executable, major).await
        {
            return Ok(finish(runtime, false));
        }

        let Some(lock) = lock_release(&major_directory, &name, cancel).await? else {
            return Err(JavaError::Canceled);
        };
        let lock = Arc::new(lock);
        // Decided again under the lock: another install may have finished
        // while this one waited.
        let replace = match self.installed(&directory, &executable, major).await? {
            Installed::Runtime(runtime) => return Ok(finish(runtime, false)),
            Installed::Nothing => false,
            Installed::Unusable if lock.excludes_other_processes() => true,
            // Without the lock, the directory could be another process's
            // install that is just being published, so only an empty one,
            // which a rename replaces, is installed over.
            Installed::Unusable => {
                if !is_empty(&directory).await? {
                    return Err(JavaError::ManagedInstallBlocked {
                        path: directory.clone(),
                    });
                }
                false
            }
        };
        remove_abandoned_staging(&major_directory, &name, &lock).await;

        // Cancellation is checked between the steps as well as inside them, so
        // a cached archive or index cannot carry a canceled install through to
        // publication.
        ensure_not_canceled(cancel)?;
        let archive = self.download(&release, reporter, cancel).await?;
        ensure_not_canceled(cancel)?;
        let staging = self
            .unpack(&archive, &major_directory, &name, Arc::clone(&lock), cancel)
            .await?;
        // Before publication, so a runtime that fails it is never installed.
        let staged = staging.path().join("bin").join("java");
        let probed = tokio::select! {
            probed = probe_requested(&staged, &executable, major) => probed,
            () = cancel.cancelled() => Err(JavaError::Canceled),
        };
        if let Err(error) = probed {
            discard(staging).await;
            return Err(error);
        }
        if cancel.is_cancelled() {
            discard(staging).await;
            return Err(JavaError::Canceled);
        }
        publish(staging, &major_directory, &name, replace, Arc::clone(&lock)).await?;

        let runtime = self.identify(&executable, major).await?;
        info!(
            major,
            release = %release.release_name,
            path = %executable.display(),
            "installed a managed java runtime"
        );
        Ok(finish(runtime, true))
    }

    /// What is installed at `directory`, whose runtime is `executable`.
    async fn installed(
        &self,
        directory: &Path,
        executable: &Path,
        major: u32,
    ) -> Result<Installed, JavaError> {
        if !exists(directory).await? {
            return Ok(Installed::Nothing);
        }
        // Not something an install leaves, so not something to replace.
        if !fs::metadata(directory)
            .await
            .is_ok_and(|metadata| metadata.is_dir())
        {
            return Err(JavaError::ManagedInstallBlocked {
                path: directory.to_path_buf(),
            });
        }
        if !exists(executable).await? {
            return Ok(Installed::Unusable);
        }
        match self.identify(executable, major).await {
            Ok(runtime) => {
                debug!(path = %executable.display(), "the managed runtime is already installed");
                Ok(Installed::Runtime(runtime))
            }
            Err(error) if proves_unusable(&error) => {
                debug!(%error, "the installed managed java runtime can never run");
                Ok(Installed::Unusable)
            }
            Err(error) => Err(error),
        }
    }

    /// Fetches the archive into the content-addressed download cache.
    ///
    /// The destination comes from the digest, never from the archive's
    /// published file name. The download manager verifies the
    /// SHA-256 and finalizes atomically, so the returned path is either a
    /// complete verified archive or does not exist.
    async fn download(
        &self,
        release: &ManagedRelease,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> Result<PathBuf, JavaError> {
        let extension =
            FileExtension::new("tar.gz").map_err(|source| JavaError::ManagedCacheKey { source })?;
        let destination =
            self.layout
                .content(ContentStore::Downloads, &release.checksum, Some(&extension));

        let request = DownloadRequest::new(release.url.clone(), &destination)
            .verifying(release.checksum.clone())
            .sized(release.size)
            .labeled(release.filename.clone());

        self.manager
            .download(&request, reporter, cancel)
            .await
            .map_err(|source| JavaError::ManagedDownloadFailed {
                major: release.major,
                release: release.release_name.clone(),
                source: Box::new(source),
            })?;
        Ok(destination)
    }

    /// Extracts into a staging directory of this attempt's own.
    ///
    /// The extraction owns the staging directory and the release lock, so an
    /// install future dropped mid-extraction still holds the lock until the
    /// writing stops, and the staging directory is then removed off the runtime.
    async fn unpack(
        &self,
        archive: &Path,
        major_directory: &Path,
        name: &str,
        lock: Arc<InstallLock>,
        cancel: &CancellationToken,
    ) -> Result<TempDir, JavaError> {
        let staging = new_staging(major_directory, name)?;

        let archive = archive.to_path_buf();
        let limits = self.limits;
        let cancel = cancel.clone();
        // `flate2` and `tar` block; a 200 MB JDK would stall the runtime's
        // worker for the duration.
        // Returned in this order so that, for a dropped install, the staging
        // directory is removed before the lock is released.
        let (staging, extraction, _lock) = tokio::task::spawn_blocking(move || {
            // Temurin archives wrap everything in one `jdk-<version>/`
            // directory, which is stripped so `bin/java` lands at the root of
            // the install and the layout is the same for every vendor.
            let extraction =
                extract_tar_gz_until_canceled(&archive, staging.path(), 1, &limits, &cancel);
            (staging, extraction, lock)
        })
        .await
        .map_err(|_| JavaError::ManagedExtractionPanicked)?;

        match extraction {
            Ok(report) => {
                debug!(
                    files = report.files,
                    bytes = report.bytes,
                    "unpacked a managed java runtime"
                );
                Ok(staging)
            }
            Err(ArchiveError::Canceled) => {
                discard(staging).await;
                Err(JavaError::Canceled)
            }
            Err(source) => {
                discard(staging).await;
                Err(JavaError::ManagedArchiveRejected {
                    source: Box::new(source),
                })
            }
        }
    }

    /// Probes the installed executable and checks it is what was asked for.
    async fn identify(&self, executable: &Path, major: u32) -> Result<JavaRuntime, JavaError> {
        let report = probe_requested(executable, executable, major).await?;
        let canonical = fs::canonicalize(executable)
            .await
            .unwrap_or_else(|_| executable.to_path_buf());
        Ok(JavaRuntime::from_probe(
            canonical,
            report,
            JavaOrigin {
                kind: JavaSourceKind::Managed,
                path: executable.to_path_buf(),
            },
        ))
    }
}

/// Probes `executable` and checks it is the major that was asked for. Failures
/// name `reported_as`, the path the runtime is installed at.
async fn probe_requested(
    executable: &Path,
    reported_as: &Path,
    major: u32,
) -> Result<ProbeReport, JavaError> {
    let report = probe(executable)
        .await
        .map_err(|source| JavaError::ManagedRuntimeUnusable {
            path: reported_as.to_path_buf(),
            source: Box::new(source),
        })?;

    if report.version.major != major {
        // Adoptium served a different major than the URL asked for, or the
        // archive was not what its name said. Either way the install is not
        // what was requested and must not be offered as if it were.
        return Err(JavaError::ManagedRuntimeWrongMajor {
            path: reported_as.to_path_buf(),
            requested: major,
            found: report.version.major,
        });
    }
    Ok(report)
}

fn ensure_not_canceled(cancel: &CancellationToken) -> Result<(), JavaError> {
    if cancel.is_cancelled() {
        Err(JavaError::Canceled)
    } else {
        Ok(())
    }
}

async fn is_empty(directory: &Path) -> Result<bool, JavaError> {
    let io = |source| JavaError::Io {
        path: directory.to_path_buf(),
        source,
    };
    let mut entries = fs::read_dir(directory).await.map_err(io)?;
    Ok(entries.next_entry().await.map_err(io)?.is_none())
}

/// Whether `path` exists. Nothing exists beneath a file.
async fn exists(path: &Path) -> Result<bool, JavaError> {
    match fs::try_exists(path).await {
        Err(error) if error.kind() == std::io::ErrorKind::NotADirectory => Ok(false),
        result => result.map_err(|source| JavaError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Takes the lock that serializes installs of release `name`, waiting for any
/// install of it already running.
///
/// `None` when `cancel` fired first.
async fn lock_release(
    major_directory: &Path,
    name: &str,
    cancel: &CancellationToken,
) -> Result<Option<InstallLock>, JavaError> {
    let io = |source| JavaError::Io {
        path: major_directory.to_path_buf(),
        source,
    };
    fs::create_dir_all(major_directory).await.map_err(io)?;
    // Canonical, so two spellings of one directory take the same lock.
    let path = fs::canonicalize(major_directory)
        .await
        .map_err(io)?
        .join(format!(".{name}.lock"));
    InstallLock::acquire(&path, cancel)
        .await
        .map_err(|source| JavaError::Io { path, source })
}

/// A new staging directory for release `name`, unique to this attempt.
///
/// A sibling of the install directory, because a child cannot be renamed onto
/// its own parent.
fn new_staging(major_directory: &Path, name: &str) -> Result<TempDir, JavaError> {
    tempfile::Builder::new()
        .prefix(&format!(".{name}."))
        .rand_bytes(STAGING_RANDOM_LEN)
        .suffix(STAGING_SUFFIX)
        .tempdir_in(major_directory)
        .map_err(|source| JavaError::Io {
            path: major_directory.to_path_buf(),
            source,
        })
}

/// Whether `entry`, a name in a major's directory, is a staging directory of
/// release `name`. The fixed `.<name>.incomplete` an older Ephervex staged
/// into counts too.
fn is_staging_of(entry: &str, name: &str) -> bool {
    let Some(rest) = entry
        .strip_prefix('.')
        .and_then(|rest| rest.strip_prefix(name))
        .and_then(|rest| rest.strip_suffix(STAGING_SUFFIX))
    else {
        return false;
    };
    rest.is_empty()
        || rest.strip_prefix('.').is_some_and(|random| {
            random.len() == STAGING_RANDOM_LEN && random.bytes().all(|b| b.is_ascii_alphanumeric())
        })
}

/// Removes the staging directories interrupted installs of release `name`
/// left behind.
///
/// Run under the release lock, so none of them belongs to a live install. Not
/// run where the lock cannot exclude other processes.
async fn remove_abandoned_staging(major_directory: &Path, name: &str, lock: &InstallLock) {
    if !lock.excludes_other_processes() {
        return;
    }
    let Ok(mut entries) = fs::read_dir(major_directory).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        if !entry
            .file_name()
            .to_str()
            .is_some_and(|entry| is_staging_of(entry, name))
        {
            continue;
        }
        let path = entry.path();
        warn!(path = %path.display(), "removing an interrupted managed java install");
        if let Err(error) = fs::remove_dir_all(&path).await {
            warn!(path = %path.display(), %error, "could not remove an interrupted managed java install");
        }
    }
}

/// Renames a finished staging directory into place as release `name`, in
/// place of the unusable install there when `replace` is set.
///
/// The rename owns the staging directory and the lock, so a dropped install
/// cannot remove the tree while it is being renamed, or let another in first.
async fn publish(
    staging: TempDir,
    major_directory: &Path,
    name: &str,
    replace: bool,
    lock: Arc<InstallLock>,
) -> Result<(), JavaError> {
    let major_directory = major_directory.to_path_buf();
    let name = name.to_owned();
    tokio::task::spawn_blocking(move || {
        let destination = major_directory.join(&name);
        let published = if replace {
            replace_unusable(staging.path(), &major_directory, &name)
        } else {
            std::fs::rename(staging.path(), &destination).map_err(|source| JavaError::Io {
                path: destination,
                source,
            })
        };
        // Whatever is left at the staging path is not the install.
        if staging.path().exists() {
            if let Err(error) = staging.close() {
                warn!(%error, "could not remove a managed java staging directory");
            }
        } else {
            let _ = staging.keep();
        }
        drop(lock);
        published
    })
    .await
    .map_err(|_| JavaError::ManagedExtractionPanicked)?
}

/// Puts `staging` at `destination` in place of the unusable install there.
///
/// Swapped in one step where the filesystem allows it, so the path never
/// holds nothing.
fn replace_unusable(staging: &Path, major_directory: &Path, name: &str) -> Result<(), JavaError> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};
    use rustix::io::Errno;

    let destination = &major_directory.join(name);
    warn!(path = %destination.display(), "replacing a managed java install that can never run");
    match renameat_with(CWD, staging, CWD, destination, RenameFlags::EXCHANGE) {
        Ok(()) => Ok(()),
        // Unsupported by the filesystem, or refused by a sandbox.
        Err(Errno::INVAL | Errno::NOSYS | Errno::OPNOTSUPP | Errno::PERM) => {
            replace_by_moving_aside(staging, major_directory, name)
        }
        Err(errno) => Err(JavaError::Io {
            path: destination.clone(),
            source: errno.into(),
        }),
    }
}

/// [`replace_unusable`] in two renames.
///
/// The old tree is moved aside under a staging name first, so one that cannot
/// be removed now is removed by the next install of the release. If the new
/// tree cannot take its place, the old one is moved back, or kept under that
/// name.
fn replace_by_moving_aside(
    staging: &Path,
    major_directory: &Path,
    name: &str,
) -> Result<(), JavaError> {
    let destination = &major_directory.join(name);
    let at_destination = |source| JavaError::Io {
        path: destination.clone(),
        source,
    };
    let retired = new_staging(major_directory, name)?;
    // Onto an empty directory, which a rename replaces.
    std::fs::rename(destination, retired.path()).map_err(at_destination)?;
    if let Err(source) = std::fs::rename(staging, destination) {
        // Kept either way: back in place, or under its staging name.
        let retired = retired.keep();
        if let Err(error) = std::fs::rename(&retired, destination) {
            warn!(path = %retired.display(), %error, "could not move a replaced managed java install back");
        }
        return Err(at_destination(source));
    }
    if let Err(error) = retired.close() {
        warn!(%error, "could not remove a replaced managed java install");
    }
    Ok(())
}

/// Whether a failed check of an installed runtime proves it can never run.
///
/// Only the install's own files prove that: an answer that is not a JVM's, an
/// executable the kernel refuses to start, or a runtime of another major,
/// which runs but sits where this release belongs. A timeout, a non-zero exit
/// or a spawn failure such as running out of memory can come from the machine,
/// and replacing a working install over one could pull it out from under a
/// game that runs on it. The replacement is published only once it passes the
/// same probe, so a refusal from the filesystem itself replaces nothing.
fn proves_unusable(error: &JavaError) -> bool {
    match error {
        JavaError::ManagedRuntimeWrongMajor { .. } => true,
        JavaError::ManagedRuntimeUnusable { source, .. } => match &**source {
            ProbeError::Unintelligible { .. } => true,
            // Not an executable, or naming a loader that is not there: both
            // said by the file, not the machine.
            ProbeError::NotExecutable { source, .. } => {
                source.raw_os_error() == Some(rustix::io::Errno::NOEXEC.raw_os_error())
                    || source.kind() == std::io::ErrorKind::NotFound
            }
            _ => false,
        },
        _ => false,
    }
}

/// What is at a release's install directory.
enum Installed {
    Nothing,
    Runtime(JavaRuntime),
    /// A directory whose runtime is missing or can never run.
    Unusable,
}

/// Removes a staging directory off the async runtime: a JDK is thousands of files.
async fn discard(staging: TempDir) {
    let path = staging.path().to_path_buf();
    let removed = tokio::task::spawn_blocking(move || staging.close()).await;
    if !matches!(removed, Ok(Ok(()))) {
        warn!(path = %path.display(), "could not remove a managed java staging directory");
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    const NAME: &str = "eclipse-jdk-21.0.12+8";

    #[test]
    fn each_attempt_stages_into_its_own_hidden_sibling() {
        let temp = tempfile::tempdir().unwrap();

        let first = new_staging(temp.path(), NAME).unwrap();
        let second = new_staging(temp.path(), NAME).unwrap();

        assert_ne!(first.path(), second.path());
        for staging in [&first, &second] {
            assert_eq!(staging.path().parent(), Some(temp.path()));
            let entry = staging.path().file_name().unwrap().to_str().unwrap();
            assert!(is_staging_of(entry, NAME), "{entry}");
        }
    }

    #[tokio::test]
    async fn a_canceled_extraction_removes_its_staging_directory() {
        let temp = tempfile::tempdir().unwrap();
        let major = temp.path().join("21");
        let archive = temp.path().join("jdk.tar.gz");
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            std::fs::File::create(&archive).unwrap(),
            flate2::Compression::fast(),
        ));
        let mut header = tar::Header::new_gnu();
        header.set_size(0);
        header.set_mode(0o755);
        builder
            .append_data(&mut header, "jdk/bin/java", std::io::empty())
            .unwrap();
        builder.into_inner().unwrap().finish().unwrap();
        let provider = ManagedJavaProvider::new(
            DownloadManager::new(ephervex_download::DownloaderConfig::default()).unwrap(),
            CacheLayout::new(temp.path().join("cache")),
            temp.path(),
        )
        .unwrap();
        let cancel = CancellationToken::new();
        let lock = lock_release(&major, NAME, &cancel)
            .await
            .unwrap()
            .expect("an uncontended release lock");
        cancel.cancel();

        let error = provider
            .unpack(&archive, &major, NAME, Arc::new(lock), &cancel)
            .await
            .expect_err("a canceled extraction must not produce a staging tree");

        assert!(matches!(error, JavaError::Canceled), "{error:?}");
        let left: Vec<_> = std::fs::read_dir(&major)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|name| is_staging_of(name, NAME))
            .collect();
        assert!(left.is_empty(), "{left:?}");
    }

    #[test]
    fn moving_an_unusable_install_aside_leaves_only_the_new_tree() {
        let temp = tempfile::tempdir().unwrap();
        let major = temp.path();
        let destination = major.join(NAME);
        std::fs::create_dir_all(destination.join("lib")).unwrap();
        std::fs::write(destination.join("lib/leftover"), b"old").unwrap();
        let staging = new_staging(major, NAME).unwrap();
        std::fs::create_dir_all(staging.path().join("bin")).unwrap();
        std::fs::write(staging.path().join("bin/java"), b"new").unwrap();

        replace_by_moving_aside(staging.path(), major, NAME).unwrap();

        assert_eq!(std::fs::read(destination.join("bin/java")).unwrap(), b"new");
        assert!(!destination.join("lib").exists());
        let left: Vec<_> = std::fs::read_dir(major)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(left, [std::ffi::OsString::from(NAME)]);
    }

    #[test]
    fn only_an_answer_from_the_runtime_proves_it_can_never_run() {
        let path = PathBuf::from("/java/21/build/bin/java");
        let unusable = |source| JavaError::ManagedRuntimeUnusable {
            path: path.clone(),
            source: Box::new(source),
        };

        assert!(proves_unusable(&JavaError::ManagedRuntimeWrongMajor {
            path: path.clone(),
            requested: 21,
            found: 17,
        }));
        assert!(proves_unusable(&unusable(ProbeError::Unintelligible {
            path: path.clone(),
        })));
        assert!(!proves_unusable(&unusable(ProbeError::TimedOut {
            path: path.clone(),
            seconds: 15,
        })));
        assert!(!proves_unusable(&unusable(ProbeError::Failed {
            path: path.clone(),
            status: Some(1),
        })));
        let spawn = |errno: rustix::io::Errno| ProbeError::NotExecutable {
            path: path.clone(),
            source: errno.into(),
        };
        assert!(proves_unusable(&unusable(spawn(rustix::io::Errno::NOEXEC))));
        assert!(!proves_unusable(&unusable(spawn(
            rustix::io::Errno::ACCESS
        ))));
        assert!(!proves_unusable(&unusable(spawn(rustix::io::Errno::NOMEM))));
        assert!(!proves_unusable(&unusable(spawn(rustix::io::Errno::AGAIN))));
    }

    #[test]
    fn only_a_releases_own_staging_names_are_taken_for_its_staging() {
        assert!(is_staging_of(".eclipse-jdk-21.0.12+8.incomplete", NAME));
        assert!(is_staging_of(
            ".eclipse-jdk-21.0.12+8.a1B2c3.incomplete",
            NAME
        ));

        for entry in [
            NAME,
            ".eclipse-jdk-21.0.12+8.lock",
            ".eclipse-jdk-21.0.12+8.1.a1B2c3.incomplete",
            ".eclipse-jdk-21.0.12+8.a1B2c.incomplete",
            ".eclipse-jdk-21.0.12+8.a1-2c3.incomplete",
            ".eclipse-jdk-21.0.13+8.a1B2c3.incomplete",
        ] {
            assert!(!is_staging_of(entry, NAME), "{entry}");
        }
    }

    #[tokio::test]
    async fn an_install_removes_only_what_interrupted_installs_of_its_release_left() {
        let temp = tempfile::tempdir().unwrap();
        let major = temp.path();
        let abandoned = [
            ".eclipse-jdk-21.0.12+8.incomplete",
            ".eclipse-jdk-21.0.12+8.a1B2c3.incomplete",
        ];
        let kept = [NAME, ".eclipse-jdk-21.0.13+8.a1B2c3.incomplete"];
        for entry in abandoned.iter().chain(&kept) {
            std::fs::create_dir_all(major.join(entry).join("bin")).unwrap();
        }

        let lock = InstallLock::acquire(&major.join(".lock"), &CancellationToken::new())
            .await
            .unwrap()
            .unwrap();
        remove_abandoned_staging(major, NAME, &lock).await;

        for entry in abandoned {
            assert!(!major.join(entry).exists(), "{entry}");
        }
        for entry in kept {
            assert!(major.join(entry).exists(), "{entry}");
        }
    }
}
