//! Explaining a launch or start that failed because of its Java runtime.
//!
//! Two shapes are covered. A plan that refuses a runtime before anything runs
//! carries a [`JavaRefusal`]: the runtime it tried when there was one, what
//! the host expects, and the concrete mismatch. A process that exits because
//! its runtime is too old prints `java.lang.UnsupportedClassVersionError`, and
//! [`class_version_mismatch`] reads that exact message so the failure can name
//! the Java the class needs. Nothing here builds prose; frontends localize the
//! typed values.

use std::fmt;

use ephervex_java::JavaError;
use ephervex_model::JavaOverride;

use crate::app::{App, JavaPlan, JavaPlanRequest, PinProbe};
use crate::diagnosis::{JavaLayer, JavaMismatch, JavaRuntimeSummary};
use crate::error::{CoreError, CoreResult};
use crate::java_status::{ExpectedRuntime, JavaSelectionMode};
use crate::mods::InstalledHost;

/// How the runtime a refused launch or start tried came to be the one it
/// tried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusedJavaChoice {
    /// The host's settings chose it, as its status reports.
    Selected(JavaSelectionMode),
    /// Given for this launch or start alone, outranking every setting.
    GivenForRun,
}

impl RefusedJavaChoice {
    /// A stable identifier for frontends and `--json`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Selected(mode) => mode.as_str(),
            Self::GivenForRun => "given-for-run",
        }
    }
}

/// A Java refusal before a launch or server start, with what it concerned.
#[derive(Debug)]
pub struct JavaRefusal {
    pub(crate) error: JavaError,
    /// How the refused runtime was chosen.
    pub chosen: RefusedJavaChoice,
    /// The runtime the plan tried, when it could be identified.
    pub attempted: Option<JavaRuntimeSummary>,
    /// What the host's Minecraft and loader combination expects.
    pub expected: ExpectedRuntime,
    /// Why the host's rules refused the attempted runtime, when they did.
    pub mismatch: Option<JavaMismatch>,
}

impl JavaRefusal {
    /// The refusal itself.
    #[must_use]
    pub const fn error(&self) -> &JavaError {
        &self.error
    }

    /// The refusal's message, worded for a runtime given for one run when it
    /// was not a setting.
    #[must_use]
    pub fn message_id(&self) -> &'static str {
        match &self.error {
            JavaError::PinnedRuntimeIncompatible { .. }
                if self.chosen == RefusedJavaChoice::GivenForRun =>
            {
                "error-java-explicit-incompatible"
            }
            error => error.message_id(),
        }
    }
}

impl fmt::Display for JavaRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(f)
    }
}

impl std::error::Error for JavaRefusal {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.error.source()
    }
}

/// The JVM refused to load a class compiled for a newer Java.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassVersionMismatch {
    /// The class the JVM named, exactly as printed.
    pub class: String,
    /// The Java major the class was compiled for.
    pub class_major: u32,
    /// The newest Java major the running JVM accepts, when its message says.
    pub runtime_major: Option<u32>,
}

impl ClassVersionMismatch {
    /// Whether a runtime of `major` could have printed this: it is the major
    /// the JVM said it was, or, where it did not say, one too old for the class.
    #[must_use]
    pub fn could_come_from(&self, major: u32) -> bool {
        self.runtime_major
            .map_or(major < self.class_major, |runtime| runtime == major)
    }
}

/// A game or server that exited because its runtime could not load its code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JavaRuntimeFailure {
    pub mismatch: ClassVersionMismatch,
    /// The runtime the process ran on, when known.
    pub attempted: Option<JavaRuntimeSummary>,
    /// What the host's Minecraft and loader combination expects.
    pub expected: ExpectedRuntime,
}

/// Where a class file's major version starts counting Java majors: class
/// version 52 is Java 8, 61 is Java 17, 65 is Java 21.
const CLASS_VERSION_OFFSET: u32 = 44;

const MARKER: &str = "java.lang.UnsupportedClassVersionError: ";
const COMPILED: &str =
    " has been compiled by a more recent version of the Java Runtime (class file version ";
const RECOGNIZES: &str =
    "), this version of the Java Runtime only recognizes class file versions up to ";
const LEGACY: &str = " : Unsupported major.minor version ";

/// Reads one output line for the JVM's `UnsupportedClassVersionError`
/// message, in either of the shapes HotSpot prints:
///
/// ```text
/// java.lang.UnsupportedClassVersionError: a/B has been compiled by a more recent version of the Java Runtime (class file version 65.0), this version of the Java Runtime only recognizes class file versions up to 52.0
/// java.lang.UnsupportedClassVersionError: a/B : Unsupported major.minor version 52.0
/// ```
///
/// The error may follow `Exception in thread "<name>" `, `Caused by: ` or
/// indentation, as the launcher and stack traces print it. Anything that does
/// not match one of these shapes exactly is not reported.
#[must_use]
pub fn class_version_mismatch(line: &str) -> Option<ClassVersionMismatch> {
    let line = line.trim_end();
    let start = line.find(MARKER)?;
    if !is_error_prefix(&line[..start]) {
        return None;
    }
    let rest = &line[start + MARKER.len()..];
    if let Some((class, versions)) = rest.split_once(COMPILED) {
        let (class_version, runtime) = versions.split_once(RECOGNIZES)?;
        let runtime = runtime.strip_suffix(".0")?;
        return mismatch(class, class_version, Some(runtime));
    }
    let (class, class_version) = rest.split_once(LEGACY)?;
    mismatch(class, class_version, None)
}

/// Whether text before the error is only the way the JVM introduces it.
fn is_error_prefix(prefix: &str) -> bool {
    let prefix = prefix.trim();
    prefix.is_empty()
        || prefix == "Caused by:"
        || prefix
            .strip_prefix("Exception in thread \"")
            .and_then(|thread| thread.strip_suffix('"'))
            .is_some_and(|thread| !thread.contains('"'))
}

fn mismatch(
    class: &str,
    class_version: &str,
    runtime_version: Option<&str>,
) -> Option<ClassVersionMismatch> {
    if class.is_empty() || class.contains(char::is_whitespace) {
        return None;
    }
    let (major, minor) = class_version.split_once('.')?;
    if minor.is_empty() || !minor.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let class_major = java_major(major)?;
    let runtime_major = match runtime_version {
        Some(version) => Some(java_major(version)?),
        None => None,
    };
    Some(ClassVersionMismatch {
        class: class.to_owned(),
        class_major,
        runtime_major,
    })
}

/// The Java major a class file major version stands for.
fn java_major(class_version: &str) -> Option<u32> {
    if class_version.is_empty() || !class_version.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    class_version
        .parse::<u32>()
        .ok()?
        .checked_sub(CLASS_VERSION_OFFSET)
        .filter(|major| *major > 0)
}

/// The first class-version failure in a process's output, if any.
pub(crate) fn first_class_version_mismatch<'a>(
    lines: impl IntoIterator<Item = &'a str>,
) -> Option<ClassVersionMismatch> {
    lines.into_iter().find_map(class_version_mismatch)
}

impl App {
    /// Plans the runtime a launch or server start uses. A Java refusal comes
    /// back as [`CoreError::JavaRefused`], naming the runtime it tried and what
    /// the host expects.
    pub(crate) async fn plan_java_for_host(
        &self,
        host: InstalledHost,
        request: &JavaPlanRequest,
    ) -> CoreResult<JavaPlan> {
        let facts = self.host_facts(host)?;
        let rules = self.host_java_rules(&facts).await?;
        let planned = self
            .resolve_java(
                &facts.minecraft_version,
                rules.required.clone(),
                &facts.loader,
                facts.java_override.clone().map(JavaOverride::new),
                facts.jvm_override.clone(),
                request,
                PinProbe::Fresh,
            )
            .await;
        let refusal = match planned {
            Ok(plan) => return Ok(plan),
            Err(refusal) => *refusal,
        };
        let CoreError::Java(error) = refusal.error else {
            return Err(refusal.error);
        };
        if error.is_cancellation() {
            return Err(error.into());
        }
        let layer = if facts.java_override.is_some() {
            Some(JavaLayer::Host)
        } else if self.java_settings().preferred_path.is_some() {
            Some(JavaLayer::Launcher)
        } else {
            None
        };
        let mismatch = match &error {
            JavaError::PinnedRuntimeIncompatible { rejection, .. } => {
                Some(JavaMismatch::of(&rejection.reason))
            }
            _ => None,
        };
        Err(CoreError::JavaRefused(Box::new(JavaRefusal {
            error,
            chosen: if request.explicit_path.is_some() {
                RefusedJavaChoice::GivenForRun
            } else {
                RefusedJavaChoice::Selected(JavaSelectionMode::of_layer(layer))
            },
            attempted: refusal
                .attempted
                .as_ref()
                .map(JavaRuntimeSummary::of_runtime),
            expected: rules.expected(facts.loader.kind),
            mismatch,
        })))
    }

    /// Explains a game or server that exited because its runtime could not
    /// load its code, from the lines the process printed. `attempted` is kept
    /// only when it could have printed the error.
    pub(crate) async fn java_runtime_failure<'a>(
        &self,
        host: InstalledHost,
        attempted: Option<JavaRuntimeSummary>,
        output: impl IntoIterator<Item = &'a str>,
    ) -> Option<JavaRuntimeFailure> {
        let mismatch = first_class_version_mismatch(output)?;
        let attempted = attempted.filter(|runtime| mismatch.could_come_from(runtime.major));
        let facts = self.host_facts(host).ok()?;
        let expected = self
            .host_java_rules(&facts)
            .await
            .ok()?
            .expected(facts.loader.kind);
        Some(JavaRuntimeFailure {
            mismatch,
            attempted,
            expected,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::*;

    fn found(line: &str) -> Option<(u32, Option<u32>)> {
        class_version_mismatch(line).map(|found| (found.class_major, found.runtime_major))
    }

    #[test]
    fn the_hotspot_message_names_the_java_the_class_needs_and_the_runtime_accepts() {
        for (class_version, runtime_version, class_major, runtime_major) in [
            (52, 51, 8, 7),
            (61, 52, 17, 8),
            (65, 61, 21, 17),
            (69, 65, 25, 21),
        ] {
            let line = format!(
                "Exception in thread \"main\" java.lang.UnsupportedClassVersionError: \
                 net/minecraft/client/main/Main has been compiled by a more recent version \
                 of the Java Runtime (class file version {class_version}.0), this version of \
                 the Java Runtime only recognizes class file versions up to {runtime_version}.0"
            );
            assert_eq!(
                found(&line),
                Some((class_major, Some(runtime_major))),
                "{line}"
            );
        }
    }

    #[test]
    fn the_launcher_s_linkage_report_is_read_through_its_indentation() {
        let line = "\tjava.lang.UnsupportedClassVersionError: net/minecraft/server/Main has \
                    been compiled by a more recent version of the Java Runtime (class file \
                    version 65.0), this version of the Java Runtime only recognizes class \
                    file versions up to 61.0";
        let found = class_version_mismatch(line).expect("a mismatch");
        assert_eq!(found.class, "net/minecraft/server/Main");
        assert_eq!((found.class_major, found.runtime_major), (21, Some(17)));
    }

    #[test]
    fn the_older_major_minor_message_names_the_java_the_class_needs() {
        assert_eq!(
            found(
                "Exception in thread \"main\" java.lang.UnsupportedClassVersionError: \
                 net/minecraft/client/main/Main : Unsupported major.minor version 52.0"
            ),
            Some((8, None))
        );
        assert_eq!(
            found(
                "Caused by: java.lang.UnsupportedClassVersionError: \
                 cpw/mods/modlauncher/Launcher : Unsupported major.minor version 52.0"
            ),
            Some((8, None))
        );
    }

    #[test]
    fn a_preview_class_file_still_names_its_major() {
        assert_eq!(
            found(
                "java.lang.UnsupportedClassVersionError: a/B has been compiled by a more \
                 recent version of the Java Runtime (class file version 65.65535), this \
                 version of the Java Runtime only recognizes class file versions up to 61.0"
            ),
            Some((21, Some(17)))
        );
    }

    #[test]
    fn near_misses_report_nothing() {
        for line in [
            "",
            "java.lang.UnsupportedClassVersionError",
            "java.lang.UnsupportedClassVersionError: a/B",
            "java.lang.NoClassDefFoundError: a/B : Unsupported major.minor version 52.0",
            "java.lang.UnsupportedClassVersionError: a/B : Unsupported major.minor version 52",
            "java.lang.UnsupportedClassVersionError: a/B : Unsupported major.minor version x.0",
            "java.lang.UnsupportedClassVersionError: a B : Unsupported major.minor version 52.0",
            "java.lang.UnsupportedClassVersionError: a/B has been compiled by a more recent \
             version of the Java Runtime (class file version 65.0), this version of the Java \
             Runtime only recognizes class file versions up to 61.1",
            "java.lang.UnsupportedClassVersionError: a/B has been compiled by a more recent \
             version of the Java Runtime (class file version 65.0), this version of the Java \
             Runtime only recognizes class file versions up to 61.0 and more",
            "java.lang.UnsupportedClassVersionError: a/B : Unsupported major.minor version 40.0",
            "[12:00:01] [main/INFO]: java.lang.UnsupportedClassVersionError: a/B : \
             Unsupported major.minor version 52.0",
            "Error: Could not create the Java Virtual Machine.",
            "Error occurred during initialization of VM\nCould not reserve enough space",
        ] {
            assert_eq!(class_version_mismatch(line), None, "{line}");
        }
    }

    #[test]
    fn only_a_runtime_that_could_have_printed_the_error_is_its_source() {
        let modern = class_version_mismatch(
            "java.lang.UnsupportedClassVersionError: a/B has been compiled by a more recent \
             version of the Java Runtime (class file version 65.0), this version of the Java \
             Runtime only recognizes class file versions up to 61.0",
        )
        .expect("a mismatch");
        assert!(modern.could_come_from(17));
        assert!(!modern.could_come_from(21));
        assert!(!modern.could_come_from(8));

        let legacy = class_version_mismatch(
            "java.lang.UnsupportedClassVersionError: a/B : Unsupported major.minor version 61.0",
        )
        .expect("a mismatch");
        assert!(legacy.could_come_from(8));
        assert!(!legacy.could_come_from(17));
        assert!(!legacy.could_come_from(21));
    }

    #[test]
    fn the_first_mismatch_in_the_output_is_the_one_reported() {
        let lines = [
            "Error: A JNI error has occurred, please check your installation and try again",
            "Exception in thread \"main\" java.lang.UnsupportedClassVersionError: a/B : \
             Unsupported major.minor version 61.0",
            "\tat java.lang.ClassLoader.defineClass1(Native Method)",
        ];
        assert_eq!(
            first_class_version_mismatch(lines).map(|found| found.class_major),
            Some(17)
        );
    }

    mod host {
        #![allow(clippy::unwrap_used, clippy::panic)]

        use std::path::Path;

        use ephervex_model::{InstanceId, JavaOverride, LoaderSelection};
        use ephervex_storage::{NewClient, Paths};

        use super::super::*;
        use crate::app::JavaTarget;
        use crate::test_fixtures::{fake_java, seed_minecraft};

        struct Fixture {
            directory: tempfile::TempDir,
            app: App,
            id: InstanceId,
        }

        /// A vanilla 1.21.1 client, with detection and downloads off.
        async fn fixture() -> Fixture {
            let directory = tempfile::tempdir().unwrap();
            let paths = Paths::rooted_at(directory.path());
            seed_minecraft(paths.cache_dir()).await;
            let mut app = App::open_at(paths).unwrap();
            let mut settings = app.java_settings();
            settings.auto_detect = false;
            settings.auto_download = false;
            app.set_java_settings(&settings).unwrap();
            let client = app
                .registry()
                .create_client(NewClient::new(
                    "Refusal",
                    "1.21.1",
                    LoaderSelection::vanilla(),
                ))
                .unwrap();
            app.registry().publish_client(&client).unwrap();
            Fixture {
                directory,
                app,
                id: client.id,
            }
        }

        impl Fixture {
            fn root(&self) -> &Path {
                self.directory.path()
            }

            fn pin(&self, path: &Path) {
                self.app
                    .set_client_runtime_settings(self.id, Some(JavaOverride::new(path)), None)
                    .unwrap();
            }

            async fn refusal(&self) -> CoreError {
                self.app
                    .plan_java_for_host(
                        InstalledHost::Instance(self.id),
                        &JavaPlanRequest {
                            target: JavaTarget::Client(self.id),
                            ..JavaPlanRequest::default()
                        },
                    )
                    .await
                    .expect_err("the plan refuses")
            }
        }

        #[tokio::test]
        async fn a_refused_pin_names_the_runtime_tried_and_what_the_host_expects() {
            let fixture = fixture().await;
            let old = fake_java(&fixture.root().join("jdk-17"), "17.0.12");
            fixture.pin(&old);

            let error = fixture.refusal().await;

            let refusal = error.java_refusal().expect("a refusal with context");
            assert_eq!(
                refusal.chosen,
                RefusedJavaChoice::Selected(JavaSelectionMode::PinnedByHost)
            );
            let attempted = refusal.attempted.as_ref().expect("the runtime it tried");
            assert_eq!(attempted.path, old);
            assert_eq!(attempted.version, "17.0.12");
            assert_eq!(attempted.vendor, "Eclipse Adoptium");
            assert_eq!(refusal.expected.major(), 21);
            assert_eq!(
                refusal.mismatch,
                Some(JavaMismatch::Major {
                    required: 21,
                    found: 17
                })
            );
            assert_eq!(
                error.required_java_major(),
                refusal.error().required_major(),
                "every predicate answers as the wrapped error does"
            );
            assert_eq!(error.message_id(), refusal.error().message_id());
        }

        /// A runtime given for one launch is not a setting, so the message
        /// does not tell the user to clear an override.
        #[tokio::test]
        async fn a_refused_runtime_given_for_one_launch_is_not_called_an_override() {
            let fixture = fixture().await;
            let old = fake_java(&fixture.root().join("jdk-17"), "17.0.12");

            let error = fixture
                .app
                .plan_java_for_host(
                    InstalledHost::Instance(fixture.id),
                    &JavaPlanRequest {
                        target: JavaTarget::Client(fixture.id),
                        explicit_path: Some(old),
                        ..JavaPlanRequest::default()
                    },
                )
                .await
                .expect_err("the plan refuses");

            assert_eq!(
                error.java_refusal().expect("a refusal").chosen,
                RefusedJavaChoice::GivenForRun
            );
            assert_eq!(error.message_id(), "error-java-explicit-incompatible");
            assert!(ephervex_i18n::has_attribute(error.message_id(), "action"));
        }

        #[tokio::test]
        async fn an_unusable_pin_is_refused_with_its_path_and_no_runtime() {
            let fixture = fixture().await;
            let missing = fixture.root().join("gone/bin/java");
            fixture.pin(&missing);

            let error = fixture.refusal().await;

            let refusal = error.java_refusal().expect("a refusal with context");
            assert!(refusal.attempted.is_none());
            assert!(matches!(refusal.error(), JavaError::RuntimeUnusable { .. }));
            assert_eq!(error.failing_path(), Some(missing));
            assert_eq!(refusal.expected.major(), 21);
        }

        #[tokio::test]
        async fn a_refusal_with_nothing_pinned_says_automatic_selection_found_nothing() {
            let fixture = fixture().await;

            let error = fixture.refusal().await;

            let refusal = error.java_refusal().expect("a refusal with context");
            assert_eq!(
                refusal.chosen,
                RefusedJavaChoice::Selected(JavaSelectionMode::Automatic)
            );
            assert!(matches!(
                refusal.error(),
                JavaError::AutoDetectDisabled { required_major: 21 }
            ));
            assert_eq!(error.required_java_major(), Some(21));
        }

        #[tokio::test]
        async fn a_refused_launcher_preference_is_named_as_the_launcher_s() {
            let mut fixture = fixture().await;
            let old = fake_java(&fixture.root().join("launcher-17"), "17.0.12");
            let mut settings = fixture.app.java_settings();
            settings.preferred_path = Some(old);
            fixture.app.set_java_settings(&settings).unwrap();

            let error = fixture.refusal().await;

            let refusal = error.java_refusal().expect("a refusal with context");
            assert_eq!(
                refusal.chosen,
                RefusedJavaChoice::Selected(JavaSelectionMode::PinnedByLauncher)
            );
            assert!(refusal.mismatch.is_some());
        }

        #[tokio::test]
        async fn an_exit_on_a_class_version_error_names_the_java_the_game_needs() {
            let fixture = fixture().await;
            let output = [
                "Error: A JNI error has occurred, please check your installation and try again",
                "Exception in thread \"main\" java.lang.UnsupportedClassVersionError: \
                 net/minecraft/client/main/Main has been compiled by a more recent version of \
                 the Java Runtime (class file version 65.0), this version of the Java Runtime \
                 only recognizes class file versions up to 61.0",
            ];

            let failure = fixture
                .app
                .java_runtime_failure(InstalledHost::Instance(fixture.id), None, output)
                .await
                .expect("a runtime failure");

            assert_eq!(failure.mismatch.class_major, 21);
            assert_eq!(failure.mismatch.runtime_major, Some(17));
            assert_eq!(failure.expected.major(), 21);
            assert!(
                fixture
                    .app
                    .java_runtime_failure(
                        InstalledHost::Instance(fixture.id),
                        None,
                        ["Error: Could not create the Java Virtual Machine."],
                    )
                    .await
                    .is_none(),
                "other JVM startup errors are not classified"
            );
        }
    }
}
