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
//! the probe succeeds. An interrupted install therefore leaves no half-unpacked
//! JDK at a path that discovery would later find, probe, and offer.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ephervex_download::{
    CacheLayout, ContentStore, DownloadError, DownloadManager, DownloadRequest, FileExtension,
    OperationReporter, redacted,
};
use tempfile::TempDir;
use tokio::fs;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, instrument, warn};

use crate::adoptium::{AdoptiumArchitecture, AdoptiumClient, ManagedRelease};
use crate::archive::{ExtractionLimits, extract_tar_gz};
use crate::discovery::{JavaOrigin, JavaRuntime, JavaSourceKind};
use crate::error::JavaError;
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
    #[instrument(skip(self))]
    pub async fn remove(&self, executable: &Path) -> Result<PathBuf, JavaError> {
        let directory = self.installed_directory(executable)?;
        fs::remove_dir_all(&directory)
            .await
            .map_err(|source| JavaError::Io {
                path: directory.clone(),
                source,
            })?;
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

        if let Some(runtime) = self.installed_runtime(&executable, major).await? {
            return Ok(finish(runtime, false));
        }

        let lock = Arc::new(lock_release(&major_directory, &name, &release, cancel).await?);
        // Another install may have finished while this one waited.
        if let Some(runtime) = self.installed_runtime(&executable, major).await? {
            return Ok(finish(runtime, false));
        }
        remove_abandoned_staging(&major_directory, &name, &lock).await;

        let archive = self.download(&release, reporter, cancel).await?;
        let staging = self
            .unpack(&archive, &major_directory, &name, Arc::clone(&lock))
            .await?;
        // Before publication, so a runtime that fails it is never installed.
        let staged = staging.path().join("bin").join("java");
        if let Err(error) = probe_requested(&staged, &executable, major).await {
            discard(staging).await;
            return Err(error);
        }
        publish(staging, &directory, Arc::clone(&lock)).await?;

        let runtime = self.identify(&executable, major).await?;
        info!(
            major,
            release = %release.release_name,
            path = %executable.display(),
            "installed a managed java runtime"
        );
        Ok(finish(runtime, true))
    }

    /// The runtime already installed at `executable`, if there is one.
    async fn installed_runtime(
        &self,
        executable: &Path,
        major: u32,
    ) -> Result<Option<JavaRuntime>, JavaError> {
        if !fs::try_exists(executable).await.unwrap_or(false) {
            return Ok(None);
        }
        debug!(path = %executable.display(), "the managed runtime is already installed");
        self.identify(executable, major).await.map(Some)
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
    ) -> Result<TempDir, JavaError> {
        let staging = new_staging(major_directory, name)?;

        let archive = archive.to_path_buf();
        let limits = self.limits;
        // `flate2` and `tar` block; a 200 MB JDK would stall the runtime's
        // worker for the duration.
        // Returned in this order so that, for a dropped install, the staging
        // directory is removed before the lock is released.
        let (staging, extraction, _lock) = tokio::task::spawn_blocking(move || {
            // Temurin archives wrap everything in one `jdk-<version>/`
            // directory, which is stripped so `bin/java` lands at the root of
            // the install and the layout is the same for every vendor.
            let extraction = extract_tar_gz(&archive, staging.path(), 1, &limits);
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

/// Takes the lock that serializes installs of release `name`, waiting for any
/// install of it already running.
async fn lock_release(
    major_directory: &Path,
    name: &str,
    release: &ManagedRelease,
    cancel: &CancellationToken,
) -> Result<InstallLock, JavaError> {
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
    match InstallLock::acquire(&path, cancel).await {
        Ok(Some(lock)) => Ok(lock),
        Ok(None) => Err(JavaError::ManagedDownloadFailed {
            major: release.major,
            release: release.release_name.clone(),
            source: Box::new(DownloadError::Canceled {
                url: redacted(&release.url),
            }),
        }),
        Err(source) => Err(JavaError::Io { path, source }),
    }
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

/// Renames a finished staging directory into place.
///
/// The rename owns the staging directory and the lock, so a dropped install
/// cannot remove the tree while it is being renamed, or let another in first.
async fn publish(
    staging: TempDir,
    directory: &Path,
    lock: Arc<InstallLock>,
) -> Result<(), JavaError> {
    let destination = directory.to_path_buf();
    let renamed = tokio::task::spawn_blocking(move || {
        let renamed = std::fs::rename(staging.path(), &destination);
        if renamed.is_ok() {
            let _ = staging.keep();
        } else if let Err(error) = staging.close() {
            warn!(%error, "could not remove a managed java staging directory");
        }
        drop(lock);
        renamed
    })
    .await
    .map_err(|_| JavaError::ManagedExtractionPanicked)?;
    renamed.map_err(|source| JavaError::Io {
        path: directory.to_path_buf(),
        source,
    })
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
