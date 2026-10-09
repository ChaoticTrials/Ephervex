//! Asking a JVM what it is.
//!
//! # Why not `java -version`
//!
//! Runtime discovery has to run `java -version`, and that is what the banner
//! *is* - but the banner is prose. It is free-form, it differs between vendors,
//! and it does not report the architecture at all. `-XshowSettings:properties` makes
//! the same JVM print its own system properties as `key = value` lines, which
//! is the difference between parsing a sentence and reading a table:
//!
//! ```text
//!     java.runtime.version = 17.0.19+10-1-24.04.2-Ubuntu
//!     java.vendor = Ubuntu
//!     java.version = 17.0.19
//!     os.arch = amd64
//! ```
//!
//! Both go to **stderr**, not stdout - a JVM treats `-version` as diagnostic
//! output - so both streams are read and concatenated. The banner remains the
//! fallback for a JVM that does not understand the flag.
//!
//! # A probe is a process
//!
//! Every probe forks a JVM. On a host with a dozen entries under
//! `/usr/lib/jvm` that is a noticeable amount of work, which is why discovery
//! deduplicates *before* probing and why the result is cached
//! ([`crate::discovery`]). A JVM that hangs is bounded by [`PROBE_TIMEOUT`]:
//! discovery must not be able to wedge the launcher's startup.
//!
//! The probe is finished when the probed process exits, not when its output
//! pipes close: a descendant that inherited them can hold them open for as long
//! as it lives. Output still pending at the exit is drained for a short, fixed
//! time. The executable is untrusted, so what it prints is capped at
//! [`MAX_OUTPUT_BYTES`]; a process that prints more is killed and is not a JVM.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};
use tokio::time::Instant;
use tracing::{debug, trace};

use crate::error::ProbeError;
use crate::version::JavaVersion;

/// How long a single JVM gets to describe itself.
///
/// Generous: a cold JDK on a spinning disk can take a second or two. The point
/// is only that a broken runtime cannot block discovery forever.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

/// The most a probed process may print across stdout and stderr.
///
/// A JVM's property listing is a few kilobytes. Anything near this is not one.
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;

/// How long output still pending after the probed process exits is read for.
const OUTPUT_DRAIN_GRACE: Duration = Duration::from_millis(200);

/// How many bytes are read from a pipe at a time.
const READ_CHUNK_BYTES: usize = 8 * 1024;

/// How many times a busy executable is retried before the probe gives up.
const BUSY_RETRIES: u32 = 6;

/// The first wait before retrying a busy executable. Each later wait doubles.
const BUSY_BACKOFF: Duration = Duration::from_millis(10);

/// Arguments that make a JVM print its system properties and exit.
const PROPERTY_ARGS: [&str; 2] = ["-XshowSettings:properties", "-version"];

/// What one JVM reported about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeReport {
    /// The executable as invoked, before canonicalization.
    pub executable: PathBuf,
    /// `java.home`, which for a Java 8 JDK is the `jre` subdirectory rather
    /// than the JDK root.
    pub home: Option<PathBuf>,
    pub version: JavaVersion,
    /// `java.version` verbatim, for display. The parsed form normalizes both
    /// numbering schemes; this is what the JVM actually said.
    pub version_string: String,
    /// `java.runtime.version` verbatim, which carries the vendor build.
    pub runtime_version: String,
    /// `java.vendor` verbatim. Never matched against a known set: some Java 8
    /// distributions report `Private Build`, and refusing an unrecognized
    /// vendor would refuse a working runtime.
    pub vendor: String,
    /// `os.arch` in the JVM's own spelling - `amd64`, not `x86_64`, and not
    /// Adoptium's `x64`. Translation happens at the API boundary that needs it.
    pub arch: String,
    /// Whether the properties flag worked, or the banner had to be parsed.
    pub used_property_flag: bool,
}

/// Runs a JVM and reads back what it says about itself.
///
/// Nothing about the executable is trusted before this point: a path from an
/// imported archive, a `PATH` entry, or a user's settings field is just a path
/// until a JVM at the other end answers.
pub async fn probe(executable: &Path) -> Result<ProbeReport, ProbeError> {
    match run(executable, &PROPERTY_ARGS).await {
        Ok(properties) => {
            if let Some(report) = from_properties(executable, &properties) {
                trace!(path = %executable.display(), version = %report.version, "probed a java runtime");
                return Ok(report);
            }
            debug!(
                path = %executable.display(),
                "the properties flag produced nothing usable; falling back to the version banner"
            );
        }
        // A JVM that predates the flag rejects it and exits non-zero. Any other
        // outcome says something about the executable itself, which a second
        // run would not change.
        Err(error @ ProbeError::Failed { .. }) => {
            debug!(
                path = %executable.display(),
                %error,
                "the properties flag was rejected; falling back to the version banner"
            );
        }
        Err(error) => return Err(error),
    }

    // Either the flag is unsupported or the output was not properties. The
    // banner is far less informative, but a Java 6-era JVM that only manages
    // that is still better identified than skipped.
    let banner = run(executable, &["-version"]).await?;
    from_banner(executable, &banner).ok_or_else(|| ProbeError::Unintelligible {
        path: executable.to_path_buf(),
    })
}

/// Executes the JVM and returns stderr and stdout concatenated.
async fn run(executable: &Path, args: &[&str]) -> Result<String, ProbeError> {
    let mut command = Command::new(executable);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // A JVM inherits _JAVA_OPTIONS and JAVA_TOOL_OPTIONS from the
        // environment and prints a "Picked up..." line when it does. Clearing
        // them keeps a developer's shell customization out of the parse, and
        // out of the probe's behavior.
        .env_remove("_JAVA_OPTIONS")
        .env_remove("JAVA_TOOL_OPTIONS")
        .env_remove("JDK_JAVA_OPTIONS")
        .kill_on_drop(true);

    let mut child =
        spawn_once_not_busy(&mut command)
            .await
            .map_err(|source| ProbeError::NotExecutable {
                path: executable.to_path_buf(),
                source,
            })?;
    let captured = capture(&mut child, executable).await?;

    if !captured.status.success() {
        return Err(ProbeError::Failed {
            path: executable.to_path_buf(),
            status: captured.status.code(),
        });
    }

    let mut text = String::from_utf8_lossy(&captured.stderr).into_owned();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&captured.stdout));
    Ok(text)
}

/// Spawns the command, retrying while the executable is busy.
///
/// `ETXTBSY` is transient: a just-written executable, such as a fresh managed
/// install, stays busy until every process forked during the write has exec'd
/// and dropped its inherited copy of the write descriptor.
async fn spawn_once_not_busy(command: &mut Command) -> std::io::Result<Child> {
    let mut backoff = BUSY_BACKOFF;
    for _ in 0..BUSY_RETRIES {
        match command.spawn() {
            Err(error) if error.kind() == ErrorKind::ExecutableFileBusy => {
                trace!(?backoff, "java executable is busy; retrying");
                tokio::time::sleep(backoff).await;
                backoff *= 2;
            }
            result => return result,
        }
    }
    command.spawn()
}

/// What a probed process printed and how it ended.
struct Captured {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// Reads a probed process's output until it exits.
///
/// The process ending is the signal, not the pipes closing, so a descendant
/// that holds them open cannot stall the probe. The time limit covers the
/// process running; after it exits only [`OUTPUT_DRAIN_GRACE`] is spent on
/// output still in flight. A process that is killed here, for running too long
/// or printing too much, is reaped by `kill_on_drop`.
async fn capture(child: &mut Child, executable: &Path) -> Result<Captured, ProbeError> {
    let (Some(mut stdout), Some(mut stderr)) = (child.stdout.take(), child.stderr.take()) else {
        return Err(ProbeError::NotExecutable {
            path: executable.to_path_buf(),
            source: std::io::Error::other("the output pipes of the probe were not created"),
        });
    };
    let not_executable = |source| ProbeError::NotExecutable {
        path: executable.to_path_buf(),
        source,
    };

    let mut out = Vec::new();
    let mut err = Vec::new();
    // On the heap: two inline chunks would make every caller's future large.
    let mut out_chunk = vec![0_u8; READ_CHUNK_BYTES];
    let mut err_chunk = vec![0_u8; READ_CHUNK_BYTES];
    let (mut out_open, mut err_open) = (true, true);
    let mut exited = None;
    // The probe's deadline until the process exits, then the drain's.
    let mut timer = pin!(tokio::time::sleep(PROBE_TIMEOUT));

    let status = loop {
        if let Some(status) = exited
            && !out_open
            && !err_open
        {
            break status;
        }
        tokio::select! {
            read = stdout.read(&mut out_chunk), if out_open => match read {
                Ok(count) if count > 0 => out.extend_from_slice(&out_chunk[..count]),
                _ => out_open = false,
            },
            read = stderr.read(&mut err_chunk), if err_open => match read {
                Ok(count) if count > 0 => err.extend_from_slice(&err_chunk[..count]),
                _ => err_open = false,
            },
            waited = child.wait(), if exited.is_none() => {
                exited = Some(waited.map_err(not_executable)?);
                timer.as_mut().reset(Instant::now() + OUTPUT_DRAIN_GRACE);
            },
            () = &mut timer => match exited {
                Some(status) => break status,
                None => {
                    return Err(ProbeError::TimedOut {
                        path: executable.to_path_buf(),
                        seconds: PROBE_TIMEOUT.as_secs(),
                    });
                }
            },
        }
        if out.len() + err.len() > MAX_OUTPUT_BYTES {
            debug!(path = %executable.display(), "a probed process printed more than a JVM does");
            return Err(ProbeError::Unintelligible {
                path: executable.to_path_buf(),
            });
        }
    };

    Ok(Captured {
        status,
        stdout: out,
        stderr: err,
    })
}

/// Builds a report from `-XshowSettings:properties` output.
fn from_properties(executable: &Path, text: &str) -> Option<ProbeReport> {
    let property = |name: &str| property(text, name);

    let version_string = property("java.version")?;
    let version = JavaVersion::parse(&version_string)?;
    let runtime_version =
        property("java.runtime.version").unwrap_or_else(|| version_string.clone());

    Some(ProbeReport {
        executable: executable.to_path_buf(),
        home: property("java.home").map(PathBuf::from),
        // The runtime version carries the vendor build; the parsed
        // `java.version` does not. Prefer the richer one when they agree on the
        // major, and keep the plainer one when a vendor writes something
        // unparseable there.
        version: JavaVersion::parse(&runtime_version)
            .filter(|parsed| parsed.major == version.major)
            .unwrap_or(version),
        version_string,
        runtime_version,
        vendor: property("java.vendor").unwrap_or_default(),
        arch: property("os.arch").unwrap_or_default(),
        used_property_flag: true,
    })
}

/// Builds a report from a `java -version` banner.
fn from_banner(executable: &Path, banner: &str) -> Option<ProbeReport> {
    let version = JavaVersion::parse_banner(banner)?;
    let version_string = version.to_string();
    Some(ProbeReport {
        executable: executable.to_path_buf(),
        home: None,
        version,
        runtime_version: version_string.clone(),
        version_string,
        // Deliberately empty rather than guessed. A caller that needs the
        // architecture must be able to tell "not reported" from a wrong answer.
        vendor: String::new(),
        arch: String::new(),
        used_property_flag: false,
    })
}

/// Reads one `key = value` property line.
///
/// The properties block indents each entry and separates with ` = `. Multi-
/// valued properties such as `java.class.path` continue on further-indented
/// lines with no separator, which this skips by requiring the key to be a bare
/// token.
fn property(text: &str, name: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        (key.trim() == name).then(|| value.trim().to_owned())
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    /// Representative abridged output of
    /// `java -XshowSettings:properties -version` from an Ubuntu Java 8 runtime.
    const JAVA_8_PROPERTIES: &str = r#"Property settings:
    file.encoding = UTF-8
    java.class.path =
    java.home = /usr/lib/jvm/java-8-openjdk-amd64/jre
    java.library.path = /usr/java/packages/lib/amd64
        /usr/lib/x86_64-linux-gnu/jni
        /lib
    java.runtime.version = 1.8.0_492-8u492-ga~us2-0ubuntu1~24.04.1-b09
    java.vendor = Private Build
    java.vendor.url = http://java.oracle.com/
    java.version = 1.8.0_492
    os.arch = amd64
    os.name = Linux

openjdk version "1.8.0_492"
"#;

    const JAVA_25_PROPERTIES: &str = r"Property settings:
    java.home = /usr/lib/jvm/java-25-openjdk-amd64
    java.runtime.version = 25.0.3+9-2-24.04.2-Ubuntu
    java.vendor = Ubuntu
    java.version = 25.0.3
    java.version.date = 2026-04-21
    os.arch = amd64
";

    #[test]
    fn the_properties_block_yields_a_full_report() {
        let report = from_properties(Path::new("/x/bin/java"), JAVA_25_PROPERTIES).expect("report");
        assert_eq!(report.version, JavaVersion::new(25, 0, 3).with_build(9));
        assert_eq!(report.vendor, "Ubuntu");
        assert_eq!(report.arch, "amd64");
        assert_eq!(
            report.home,
            Some(PathBuf::from("/usr/lib/jvm/java-25-openjdk-amd64"))
        );
    }

    #[test]
    fn an_unrecognized_vendor_is_carried_through_rather_than_rejected() {
        // The fixture reports `Private Build`. A launcher that only accepts a
        // known vendor enum would discard a working JDK.
        let report = from_properties(Path::new("/x/bin/java"), JAVA_8_PROPERTIES).expect("report");
        assert_eq!(report.vendor, "Private Build");
        assert_eq!(report.version.major, 8);
        assert_eq!(report.version.update, 492);
    }

    #[test]
    fn java_8s_home_is_the_jre_subdirectory() {
        // Worth pinning: `<home>/bin/java` and `<home>/jre/bin/java` are two
        // paths to one executable on a Java 8 JDK, and discovery has to
        // deduplicate them.
        let report = from_properties(Path::new("/x/bin/java"), JAVA_8_PROPERTIES).expect("report");
        assert!(
            report
                .home
                .expect("home")
                .ends_with("java-8-openjdk-amd64/jre")
        );
    }

    #[test]
    fn a_multi_valued_property_does_not_leak_into_the_next_key() {
        // `java.library.path` continues over three lines. A parser that took
        // any indented line as a value would read `/lib` as a property.
        assert_eq!(
            property(JAVA_8_PROPERTIES, "java.vendor").as_deref(),
            Some("Private Build")
        );
        assert_eq!(property(JAVA_8_PROPERTIES, "/lib"), None);
    }

    #[test]
    fn output_without_a_java_version_property_is_not_a_report() {
        assert!(
            from_properties(Path::new("/x"), "Property settings:\n    os.arch = amd64").is_none()
        );
    }

    #[test]
    fn the_banner_fallback_reports_a_version_and_admits_it_knows_nothing_else() {
        let report = from_banner(
            Path::new("/x/bin/java"),
            "openjdk version \"1.8.0_492\"\nOpenJDK Runtime Environment (build 1.8.0_492-b09)",
        )
        .expect("report");
        assert_eq!(report.version.major, 8);
        assert!(!report.used_property_flag);
        assert!(
            report.arch.is_empty(),
            "an unknown architecture must not be invented"
        );
    }

    /// Linux refuses to exec a file while any descriptor holds it open for
    /// writing, so the open writer here makes the first attempt fail with
    /// `ETXTBSY` exactly as a just-written executable can.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn an_executable_still_open_for_writing_is_probed_once_it_is_released() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("temp");
        let executable = temp.path().join("java");
        let mut writer = std::fs::File::create(&executable).expect("create java");
        writer
            .write_all(
                b"#!/bin/sh\ncat >&2 <<'EOF'\nProperty settings:\n    java.version = 21.0.12\nEOF\n",
            )
            .expect("write java");
        writer
            .set_permissions(std::fs::Permissions::from_mode(0o755))
            .expect("chmod");

        let release = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(writer);
        });
        let report = probe(&executable).await.expect("probe after release");
        release.await.expect("release writer");

        assert_eq!(report.version.major, 21);
    }

    /// Writes an executable `java` that runs `script` and returns its path.
    #[cfg(unix)]
    fn fake_java(directory: &Path, script: &str) -> PathBuf {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        let executable = directory.join("java");
        let mut file = std::fs::File::create(&executable).expect("create java");
        write!(file, "#!/bin/sh\n{script}\n").expect("write java");
        file.set_permissions(std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
        executable
    }

    #[cfg(unix)]
    const FAKE_PROPERTIES: &str =
        "printf 'Property settings:\\n    java.version = 21.0.12\\n    os.arch = amd64\\n' >&2";

    #[cfg(unix)]
    #[tokio::test]
    async fn output_beyond_the_cap_ends_the_probe_as_unintelligible() {
        let temp = tempfile::tempdir().expect("temp");
        // Never exits on its own, so only the cap can end it.
        let executable = fake_java(temp.path(), "exec yes >&2");

        let started = Instant::now();
        let error = probe(&executable).await.expect_err("endless output");

        assert!(
            matches!(error, ProbeError::Unintelligible { .. }),
            "{error:?}"
        );
        assert!(started.elapsed() < PROBE_TIMEOUT / 2);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_process_that_prints_more_than_the_cap_and_exits_is_unintelligible() {
        let temp = tempfile::tempdir().expect("temp");
        let executable = fake_java(
            temp.path(),
            &format!("{FAKE_PROPERTIES}\nhead -c 3000000 /dev/zero"),
        );

        let error = probe(&executable).await.expect_err("oversized output");

        assert!(
            matches!(error, ProbeError::Unintelligible { .. }),
            "{error:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_descendant_holding_the_output_pipes_does_not_delay_the_report() {
        let temp = tempfile::tempdir().expect("temp");
        let executable = fake_java(temp.path(), &format!("{FAKE_PROPERTIES}\n(sleep 10 &)"));

        let started = Instant::now();
        let report = probe(&executable).await.expect("probe");

        assert_eq!(report.version.major, 21);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_rejected_properties_flag_falls_back_to_the_version_banner() {
        let temp = tempfile::tempdir().expect("temp");
        let executable = fake_java(
            temp.path(),
            r#"case "$1" in
-XshowSettings:properties) echo "Unrecognized option: $1" >&2; exit 1 ;;
esac
echo 'java version "1.6.0_45"' >&2"#,
        );

        let report = probe(&executable).await.expect("banner fallback");

        assert!(!report.used_property_flag);
        assert_eq!(report.version.major, 6);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_executable_that_fails_both_attempts_reports_the_failure() {
        let temp = tempfile::tempdir().expect("temp");
        let executable = fake_java(temp.path(), "exit 3");

        let error = probe(&executable).await.expect_err("both fail");

        assert!(
            matches!(
                error,
                ProbeError::Failed {
                    status: Some(3),
                    ..
                }
            ),
            "{error:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_unintelligible_banner_after_a_rejected_flag_is_unintelligible() {
        let temp = tempfile::tempdir().expect("temp");
        let executable = fake_java(
            temp.path(),
            r#"case "$1" in
-XshowSettings:properties) exit 1 ;;
esac
echo 'not a jvm' >&2"#,
        );

        let error = probe(&executable).await.expect_err("not a jvm");

        assert!(
            matches!(error, ProbeError::Unintelligible { .. }),
            "{error:?}"
        );
    }
}
