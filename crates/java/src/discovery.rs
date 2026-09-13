//! Finding the JVMs installed on this machine.
//!
//! # Twelve directories, four runtimes
//!
//! A Debian-family `/usr/lib/jvm` is mostly symlinks. On the machine this was
//! written against it holds twelve entries that resolve to exactly four
//! executables: `default-java`, `java-1.21.0-openjdk-amd64`, and
//! `java-21-openjdk-amd64` are one JDK wearing three names, and a Java 8 JDK
//! answers to both `<home>/bin/java` and `<home>/jre/bin/java`. A scan that
//! reports what it finds shows the user four copies of the same runtime and
//! forks twelve JVMs to do it.
//!
//! So candidates are **canonicalized before they are probed**, and the paths
//! that collapsed onto one executable are kept as that runtime's origins. Three
//! of those twelve entries (`openjdk-17`, `openjdk-21`, `openjdk-25`) contain
//! no `bin/java` at all - they are the JDK's non-executable bits - and a
//! candidate that yields no executable is skipped at `trace` level rather than
//! reported as a failure. Nothing is wrong; the directory was simply never a
//! runtime.
//!
//! # Where it looks
//!
//! `PATH`, `JAVA_HOME`, the common system JVM directories, Ephervex's own
//! managed runtimes, and SDKMAN. Each hit records *where* it came from, because
//! "why did it pick that one" is a question a developer will ask, and the only
//! satisfying answer names a directory.
//!
//! # Caching
//!
//! Probing forks a JVM per runtime, so an inventory is a thing to compute once
//! and hand out, not something to recompute on every repaint. This module
//! returns an inventory with the time it was taken; holding onto it and
//! offering a Refresh is the caller's job - `ephervex-core` does it for both
//! frontends.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use tracing::{debug, instrument, trace, warn};

use crate::error::ProbeError;
use crate::probe::{ProbeReport, probe};
use crate::version::JavaVersion;

/// System directories conventionally holding JVM installations.
///
/// `/usr/lib/jvm` is the Debian/Fedora convention; the other two catch
/// multilib layouts and hand-unpacked JDKs. A directory that does not exist
/// costs one failed `read_dir`.
pub const SYSTEM_JVM_DIRECTORIES: &[&str] = &["/usr/lib/jvm", "/usr/lib64/jvm", "/opt/java"];

/// Where one runtime was found.
///
/// A runtime usually has several: the same JDK is typically on `PATH`, under
/// `JAVA_HOME`, and in `/usr/lib/jvm` at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum JavaSourceKind {
    /// Ephervex downloaded and installed it.
    Managed,
    /// Named by `JAVA_HOME`.
    JavaHome,
    /// Found on `PATH`.
    Path,
    /// Found under a system JVM directory.
    System,
    /// Found under SDKMAN's candidates directory.
    Sdkman,
    /// Handed to the launcher directly - a settings field, an override, or
    /// `ephervex java test`. Never trusted until probed.
    UserSupplied,
}

impl JavaSourceKind {
    /// A stable, untranslated identifier for logs and `--json` output
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Managed => "managed",
            Self::JavaHome => "java-home",
            Self::Path => "path",
            Self::System => "system",
            Self::Sdkman => "sdkman",
            Self::UserSupplied => "user-supplied",
        }
    }
}

impl std::fmt::Display for JavaSourceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One place a runtime was found, and the path that led there.
///
/// The path is the *uncanonicalized* candidate - `/usr/lib/jvm/default-java/bin/java`
/// rather than what it resolves to - because that is the string the user
/// recognizes when they ask why a particular runtime was chosen.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct JavaOrigin {
    pub kind: JavaSourceKind,
    pub path: PathBuf,
}

/// A Java runtime installed on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JavaRuntime {
    /// The canonical executable. Two candidates with the same canonical path
    /// are the same runtime.
    pub executable: PathBuf,
    /// `java.home` as the JVM reports it, which for a Java 8 JDK is the `jre`
    /// subdirectory.
    pub home: Option<PathBuf>,
    pub version: JavaVersion,
    pub version_string: String,
    pub runtime_version: String,
    pub vendor: String,
    /// The JVM's own spelling of the architecture: `amd64`, not `x64`.
    pub arch: String,
    /// Every place this runtime was found, deduplicated and ordered by kind so
    /// the most authoritative source reads first.
    pub origins: Vec<JavaOrigin>,
}

impl JavaRuntime {
    /// Builds a runtime from a probe report and the candidate that produced it.
    #[must_use]
    pub fn from_probe(canonical: PathBuf, report: ProbeReport, origin: JavaOrigin) -> Self {
        Self {
            executable: canonical,
            home: report.home,
            version: report.version,
            version_string: report.version_string,
            runtime_version: report.runtime_version,
            vendor: report.vendor,
            arch: report.arch,
            origins: vec![origin],
        }
    }

    /// Whether Ephervex installed this runtime itself.
    #[must_use]
    pub fn is_managed(&self) -> bool {
        self.origins
            .iter()
            .any(|origin| origin.kind == JavaSourceKind::Managed)
    }

    /// The most authoritative source this runtime was found through.
    #[must_use]
    pub fn primary_source(&self) -> JavaSourceKind {
        self.origins
            .first()
            .map_or(JavaSourceKind::UserSupplied, |origin| origin.kind)
    }

    fn absorb(&mut self, origin: JavaOrigin) {
        if !self.origins.contains(&origin) {
            self.origins.push(origin);
            self.origins.sort();
        }
    }
}

/// A candidate probe that did not produce a runtime.
///
/// Carried rather than discarded: a `JAVA_HOME` pointing at a deleted JDK is
/// something the user wants told about, even though it must not stop discovery.
#[derive(Debug, Clone)]
pub struct ProbeFailure {
    pub origin: JavaOrigin,
    /// Shared rather than owned so an inventory can be cloned out of the
    /// cache. `ProbeError` wraps `io::Error`, which is not `Clone`.
    pub error: Arc<ProbeError>,
}

/// Every runtime found by one scan.
#[derive(Debug, Clone)]
pub struct RuntimeInventory {
    /// Deduplicated runtimes, newest first.
    pub runtimes: Vec<JavaRuntime>,
    /// Candidates that had an executable but could not be identified.
    pub failures: Vec<ProbeFailure>,
    /// How many distinct executables were probed. Compare against
    /// `runtimes.len()` to see the deduplication at work.
    pub probed: usize,
    /// How many candidate paths were considered before deduplication.
    pub candidates: usize,
    pub scanned_at: DateTime<Utc>,
}

impl RuntimeInventory {
    /// Runtimes whose major version matches, newest first.
    pub fn with_major(&self, major: u32) -> impl Iterator<Item = &JavaRuntime> {
        self.runtimes
            .iter()
            .filter(move |runtime| runtime.version.major == major)
    }

    /// The runtime at a given canonical path, if it was found.
    #[must_use]
    pub fn find(&self, executable: &Path) -> Option<&JavaRuntime> {
        self.runtimes
            .iter()
            .find(|runtime| runtime.executable == executable)
    }
}

/// Scans the machine for Java runtimes.
///
/// The environment sources are opt-in so tests can scan a fixture tree without
/// picking up whatever JDKs the build machine happens to have. A test suite
/// that forks the host's JVMs is a test suite whose results depend on the host.
#[derive(Debug, Clone)]
pub struct JavaDiscovery {
    managed_root: Option<PathBuf>,
    system_directories: Vec<PathBuf>,
    scan_path: bool,
    scan_java_home: bool,
    scan_sdkman: bool,
}

impl JavaDiscovery {
    /// Scans the supported system, managed, and configured runtime locations.
    #[must_use]
    pub fn system(managed_root: impl Into<PathBuf>) -> Self {
        Self {
            managed_root: Some(managed_root.into()),
            system_directories: SYSTEM_JVM_DIRECTORIES.iter().map(PathBuf::from).collect(),
            scan_path: true,
            scan_java_home: true,
            scan_sdkman: true,
        }
    }

    /// A scan that touches nothing outside the directories it is given.
    #[must_use]
    pub fn isolated() -> Self {
        Self {
            managed_root: None,
            system_directories: Vec::new(),
            scan_path: false,
            scan_java_home: false,
            scan_sdkman: false,
        }
    }

    #[must_use]
    pub fn with_managed_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.managed_root = Some(root.into());
        self
    }

    /// Adds a directory whose children are JDK homes.
    #[must_use]
    pub fn with_system_directory(mut self, directory: impl Into<PathBuf>) -> Self {
        self.system_directories.push(directory.into());
        self
    }

    /// Enumerates candidates, deduplicates them, and probes what is left.
    #[instrument(skip(self))]
    pub async fn discover(&self) -> RuntimeInventory {
        let candidates = self.candidates();
        let total = candidates.len();

        // Canonicalize first, probe second. Twelve candidates become four
        // executables on a typical machine, and the saving is eight forked
        // JVMs.
        let mut by_executable: BTreeMap<PathBuf, Vec<JavaOrigin>> = BTreeMap::new();
        for origin in candidates {
            let Some(canonical) = canonical_executable(&origin.path) else {
                trace!(path = %origin.path.display(), "candidate holds no java executable");
                continue;
            };
            by_executable.entry(canonical).or_default().push(origin);
        }

        let probed = by_executable.len();
        debug!(
            candidates = total,
            distinct = probed,
            "deduplicated java candidates by canonical executable"
        );

        let mut runtimes = Vec::new();
        let mut failures = Vec::new();
        for (canonical, mut origins) in by_executable {
            origins.sort();
            let primary = origins.first().cloned().unwrap_or(JavaOrigin {
                kind: JavaSourceKind::UserSupplied,
                path: canonical.clone(),
            });
            match probe(&canonical).await {
                Ok(report) => {
                    let mut runtime = JavaRuntime::from_probe(canonical, report, primary);
                    for origin in origins {
                        runtime.absorb(origin);
                    }
                    runtimes.push(runtime);
                }
                Err(error) => {
                    warn!(path = %canonical.display(), %error, "a java candidate could not be identified");
                    failures.push(ProbeFailure {
                        origin: primary,
                        error: Arc::new(error),
                    });
                }
            }
        }

        // Newest first, then by path so the order is stable between scans.
        runtimes.sort_by(|left, right| {
            right
                .version
                .cmp(&left.version)
                .then_with(|| left.executable.cmp(&right.executable))
        });

        RuntimeInventory {
            runtimes,
            failures,
            probed,
            candidates: total,
            scanned_at: Utc::now(),
        }
    }

    /// Every path worth canonicalizing, in source-priority order.
    fn candidates(&self) -> Vec<JavaOrigin> {
        let mut candidates = Vec::new();

        if let Some(root) = &self.managed_root {
            // `<java>/<major>/<vendor-build>/bin/java`: the layout the managed
            // installer writes.
            for major in children(root) {
                for build in children(&major) {
                    push_home(&mut candidates, JavaSourceKind::Managed, &build);
                }
            }
        }

        if self.scan_java_home
            && let Some(home) = std::env::var_os("JAVA_HOME")
            && !home.is_empty()
        {
            push_home(&mut candidates, JavaSourceKind::JavaHome, Path::new(&home));
        }

        if self.scan_path
            && let Some(path) = std::env::var_os("PATH")
        {
            for directory in std::env::split_paths(&path) {
                candidates.push(JavaOrigin {
                    kind: JavaSourceKind::Path,
                    path: directory.join("java"),
                });
            }
        }

        for directory in &self.system_directories {
            for home in children(directory) {
                push_home(&mut candidates, JavaSourceKind::System, &home);
            }
        }

        if self.scan_sdkman
            && let Some(home) = std::env::var_os("HOME")
        {
            let candidates_dir = Path::new(&home)
                .join(".sdkman")
                .join("candidates")
                .join("java");
            for installed in children(&candidates_dir) {
                push_home(&mut candidates, JavaSourceKind::Sdkman, &installed);
            }
        }

        candidates
    }
}

/// Adds both executables a JDK home can expose.
///
/// Java 8 JDKs ship a `jre` subdirectory whose `bin/java` is the real binary,
/// with `<home>/bin/java` as a symlink to it. Both are offered; canonicalization
/// collapses them.
fn push_home(candidates: &mut Vec<JavaOrigin>, kind: JavaSourceKind, home: &Path) {
    candidates.push(JavaOrigin {
        kind,
        path: home.join("bin").join("java"),
    });
    candidates.push(JavaOrigin {
        kind,
        path: home.join("jre").join("bin").join("java"),
    });
}

/// The immediate children of a directory, sorted. An unreadable or absent
/// directory yields nothing: it is not a JVM directory, which is not an error.
fn children(directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    paths.sort();
    paths
}

/// Resolves a candidate to a canonical executable, or `None` if it is not one.
///
/// `None` covers the ordinary cases - a `PATH` entry with no `java`, a
/// `/usr/lib/jvm` entry that holds only resources - and they are all silence,
/// not failure.
fn canonical_executable(path: &Path) -> Option<PathBuf> {
    let canonical = std::fs::canonicalize(path).ok()?;
    is_executable_file(&canonical).then_some(canonical)
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    /// A fake JDK home whose `bin/java` prints the properties block of a real
    /// one. Discovery only ever asks a candidate to describe itself, so a shell
    /// script is a faithful stand-in - and unlike the host's JDKs it makes the
    /// test say the same thing on every machine.
    fn fake_jdk(root: &Path, name: &str, version: &str, runtime: &str, vendor: &str) -> PathBuf {
        let home = root.join(name);
        let bin = home.join("bin");
        std::fs::create_dir_all(&bin).expect("create bin");
        let script = format!(
            "#!/bin/sh\ncat >&2 <<'EOF'\nProperty settings:\n    java.home = {home}\n    \
             java.runtime.version = {runtime}\n    java.vendor = {vendor}\n    \
             java.version = {version}\n    os.arch = amd64\nEOF\n",
            home = home.display(),
        );
        let executable = bin.join("java");
        std::fs::write(&executable, script).expect("write java");
        set_executable(&executable);
        home
    }

    #[cfg(unix)]
    fn set_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    #[cfg(not(unix))]
    fn set_executable(_path: &Path) {}

    #[tokio::test]
    async fn twelve_names_for_four_runtimes_are_reported_as_four() {
        // The shape of a real /usr/lib/jvm, reproduced: four JDKs plus the
        // alias symlinks a distribution adds around them.
        let temp = tempfile::tempdir().expect("temp");
        let jvm = temp.path().join("jvm");
        std::fs::create_dir_all(&jvm).expect("create jvm dir");

        fake_jdk(
            &jvm,
            "java-8-openjdk-amd64",
            "1.8.0_492",
            "1.8.0_492-b09",
            "Private Build",
        );
        fake_jdk(
            &jvm,
            "java-17-openjdk-amd64",
            "17.0.19",
            "17.0.19+10",
            "Ubuntu",
        );
        fake_jdk(
            &jvm,
            "java-21-openjdk-amd64",
            "21.0.11",
            "21.0.11+10",
            "Ubuntu",
        );
        fake_jdk(
            &jvm,
            "java-25-openjdk-amd64",
            "25.0.3",
            "25.0.3+9",
            "Ubuntu",
        );

        #[cfg(unix)]
        {
            for (alias, target) in [
                ("default-java", "java-21-openjdk-amd64"),
                ("java-1.8.0-openjdk-amd64", "java-8-openjdk-amd64"),
                ("java-1.17.0-openjdk-amd64", "java-17-openjdk-amd64"),
                ("java-1.21.0-openjdk-amd64", "java-21-openjdk-amd64"),
                ("java-1.25.0-openjdk-amd64", "java-25-openjdk-amd64"),
            ] {
                std::os::unix::fs::symlink(jvm.join(target), jvm.join(alias)).expect("symlink");
            }
        }
        // Three entries that hold no executable at all, exactly as
        // `openjdk-17`, `openjdk-21`, and `openjdk-25` do on a real machine.
        for empty in ["openjdk-17", "openjdk-21", "openjdk-25"] {
            std::fs::create_dir_all(jvm.join(empty)).expect("create empty");
        }

        let inventory = JavaDiscovery::isolated()
            .with_system_directory(&jvm)
            .discover()
            .await;

        assert_eq!(
            inventory.runtimes.len(),
            4,
            "aliases must collapse onto the runtime they point at, got {:?}",
            inventory
                .runtimes
                .iter()
                .map(|r| r.executable.display().to_string())
                .collect::<Vec<_>>()
        );
        assert!(
            inventory.candidates > inventory.probed,
            "the dedup has to actually remove candidates"
        );
        assert!(
            inventory.failures.is_empty(),
            "a directory with no bin/java is not a failure: {:?}",
            inventory.failures
        );

        let majors: Vec<u32> = inventory.runtimes.iter().map(|r| r.version.major).collect();
        assert_eq!(majors, vec![25, 21, 17, 8], "newest first");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_aliased_runtime_remembers_every_name_it_was_found_under() {
        let temp = tempfile::tempdir().expect("temp");
        let jvm = temp.path().join("jvm");
        std::fs::create_dir_all(&jvm).expect("create jvm dir");
        fake_jdk(
            &jvm,
            "java-21-openjdk-amd64",
            "21.0.11",
            "21.0.11+10",
            "Ubuntu",
        );
        std::os::unix::fs::symlink(jvm.join("java-21-openjdk-amd64"), jvm.join("default-java"))
            .expect("symlink");

        let inventory = JavaDiscovery::isolated()
            .with_system_directory(&jvm)
            .discover()
            .await;

        let runtime = &inventory.runtimes[0];
        assert_eq!(runtime.origins.len(), 2, "{:?}", runtime.origins);
        assert!(
            runtime
                .origins
                .iter()
                .any(|origin| origin.path.to_string_lossy().contains("default-java")),
            "the alias the user recognizes must survive: {:?}",
            runtime.origins
        );
    }

    #[tokio::test]
    async fn a_managed_runtime_is_found_and_marked_as_managed() {
        let temp = tempfile::tempdir().expect("temp");
        let managed = temp.path().join("java");
        let major = managed.join("21");
        std::fs::create_dir_all(&major).expect("create major dir");
        fake_jdk(
            &major,
            "eclipse-jdk-21.0.12+8",
            "21.0.12",
            "21.0.12+8",
            "Eclipse Adoptium",
        );

        let inventory = JavaDiscovery::isolated()
            .with_managed_root(&managed)
            .discover()
            .await;

        assert_eq!(inventory.runtimes.len(), 1);
        assert!(inventory.runtimes[0].is_managed());
        assert_eq!(
            inventory.runtimes[0].primary_source(),
            JavaSourceKind::Managed
        );
    }

    #[tokio::test]
    async fn a_candidate_that_is_not_a_jvm_is_a_failure_rather_than_a_runtime() {
        let temp = tempfile::tempdir().expect("temp");
        let jvm = temp.path().join("jvm");
        let bin = jvm.join("broken").join("bin");
        std::fs::create_dir_all(&bin).expect("create bin");
        std::fs::write(bin.join("java"), "#!/bin/sh\necho not a jvm\n").expect("write");
        set_executable(&bin.join("java"));

        let inventory = JavaDiscovery::isolated()
            .with_system_directory(&jvm)
            .discover()
            .await;

        assert!(inventory.runtimes.is_empty());
        assert_eq!(inventory.failures.len(), 1, "the user needs telling");
    }

    #[test]
    fn an_absent_directory_yields_no_candidates_rather_than_an_error() {
        assert!(children(Path::new("/nonexistent/ephervex/jvm")).is_empty());
    }
}
