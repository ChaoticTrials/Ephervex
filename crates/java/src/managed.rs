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
//! 4. **Probe** the installed `bin/java` and confirm it is the major that was
//!    asked for. An install that produces something unusable must fail here,
//!    not at launch.
//!
//! # Extraction is not done in place
//!
//! Files land in a sibling `.incomplete` directory and are renamed into
//! position only once the probe succeeds. An interrupted install therefore
//! leaves no half-unpacked JDK at a path that discovery would later find,
//! probe, and offer.

use std::path::{Path, PathBuf};

use ephervex_download::{
    CacheLayout, ContentStore, DownloadManager, DownloadRequest, FileExtension, OperationReporter,
};
use tokio::fs;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, instrument, warn};

use crate::adoptium::{AdoptiumArchitecture, AdoptiumClient, ManagedRelease};
use crate::archive::{ExtractionLimits, extract_tar_gz};
use crate::discovery::{JavaOrigin, JavaRuntime, JavaSourceKind};
use crate::error::JavaError;
use crate::probe::probe;

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

        let directory = self
            .major_directory(major)
            .join(release.install_directory_name());
        let executable = directory.join("bin").join("java");

        if fs::try_exists(&executable).await.unwrap_or(false) {
            debug!(path = %executable.display(), "the managed runtime is already installed");
            let runtime = self.identify(&executable, major).await?;
            return Ok(ManagedInstallation {
                runtime,
                release,
                directory,
                newly_installed: false,
            });
        }

        let archive = self.download(&release, reporter, cancel).await?;
        self.unpack(&archive, &directory).await?;

        let runtime = self.identify(&executable, major).await?;
        info!(
            major,
            release = %release.release_name,
            path = %executable.display(),
            "installed a managed java runtime"
        );
        Ok(ManagedInstallation {
            runtime,
            release,
            directory,
            newly_installed: true,
        })
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

    /// Extracts into a staging directory and renames it into place.
    async fn unpack(&self, archive: &Path, directory: &Path) -> Result<(), JavaError> {
        let staging = staging_path(directory);
        // A previous interrupted attempt would otherwise be extracted over,
        // mixing two builds into one directory.
        if fs::try_exists(&staging).await.unwrap_or(false) {
            warn!(path = %staging.display(), "removing an interrupted managed java install");
            let _ = fs::remove_dir_all(&staging).await;
        }
        if let Some(parent) = directory.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|source| JavaError::Io {
                    path: parent.to_path_buf(),
                    source,
                })?;
        }

        let archive = archive.to_path_buf();
        let target = staging.clone();
        let limits = self.limits;
        // `flate2` and `tar` block; a 200 MB JDK would stall the runtime's
        // worker for the duration.
        let extraction = tokio::task::spawn_blocking(move || {
            // Temurin archives wrap everything in one `jdk-<version>/`
            // directory, which is stripped so `bin/java` lands at the root of
            // the install and the layout is the same for every vendor.
            extract_tar_gz(&archive, &target, 1, &limits)
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
            }
            Err(source) => {
                let _ = fs::remove_dir_all(&staging).await;
                return Err(JavaError::ManagedArchiveRejected {
                    source: Box::new(source),
                });
            }
        }

        fs::rename(&staging, directory)
            .await
            .map_err(|source| JavaError::Io {
                path: directory.to_path_buf(),
                source,
            })
    }

    /// Probes the installed executable and checks it is what was asked for.
    async fn identify(&self, executable: &Path, major: u32) -> Result<JavaRuntime, JavaError> {
        let report =
            probe(executable)
                .await
                .map_err(|source| JavaError::ManagedRuntimeUnusable {
                    path: executable.to_path_buf(),
                    source: Box::new(source),
                })?;

        if report.version.major != major {
            // Adoptium served a different major than the URL asked for, or the
            // archive was not what its name said. Either way the install is not
            // what was requested and must not be offered as if it were.
            return Err(JavaError::ManagedRuntimeWrongMajor {
                path: executable.to_path_buf(),
                requested: major,
                found: report.version.major,
            });
        }

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

/// The staging directory an install is extracted into before it is renamed.
///
/// A sibling rather than a child: renaming a child into its own parent is not a
/// rename, and a dot-prefixed sibling is also skipped by discovery's scan
/// because it holds no `bin/java` until the moment it does.
fn staging_path(directory: &Path) -> PathBuf {
    let name = directory.file_name().map_or_else(
        || ".incomplete".to_owned(),
        |name| format!(".{}.incomplete", name.to_string_lossy()),
    );
    directory
        .parent()
        .map_or_else(|| PathBuf::from(&name), |parent| parent.join(&name))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn staging_is_a_hidden_sibling_of_the_final_directory() {
        let staging = staging_path(Path::new("/data/ephervex/java/21/eclipse-jdk-21.0.12+8"));
        assert_eq!(
            staging,
            PathBuf::from("/data/ephervex/java/21/.eclipse-jdk-21.0.12+8.incomplete")
        );
    }
}
