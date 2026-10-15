//! How a host's Java is described: a runtime, how it was chosen, what the
//! host expects, and what a selection resolves to. Text only, shared by the
//! runtime section, the picker, the change preview and failure messages.

use ephervex_core::{
    AutomaticJava, ExpectedRuntime, FindingClass, FindingKind, HostJavaStatus, JavaCheck,
    JavaChoice, JavaChoices, JavaMismatch, JavaRuntimeSummary, JavaSelectionMode, JavaUnavailable,
    ResolvedJava,
};
use ephervex_i18n::{Arg, lookup, lookup_args};
use ephervex_model::LoaderKind;

/// A loader's name as its project spells it.
#[must_use]
pub const fn loader_display_name(kind: LoaderKind) -> &'static str {
    match kind {
        LoaderKind::Vanilla => "Vanilla",
        LoaderKind::Forge => "Forge",
        LoaderKind::NeoForge => "NeoForge",
    }
}

/// "Java 21.0.8, Eclipse Adoptium, amd64": version, vendor and architecture
/// exactly as the runtime reported them. A JVM too old to report its vendor or
/// architecture is said not to have, rather than shown as a blank.
#[must_use]
pub fn runtime_line(runtime: &JavaRuntimeSummary) -> String {
    let reported = |value: &str, missing: &str| {
        if value.trim().is_empty() {
            lookup(missing)
        } else {
            value.to_owned()
        }
    };
    lookup_args(
        "gui-java-runtime-line",
        [
            ("version", Arg::from(runtime.version.as_str())),
            (
                "vendor",
                Arg::from(reported(&runtime.vendor, "gui-java-vendor-unknown")),
            ),
            (
                "arch",
                Arg::from(reported(&runtime.arch, "gui-java-arch-unknown")),
            ),
        ],
    )
}

/// How a host's runtime was chosen, and whether Ephervex manages it.
#[must_use]
pub fn origin_text(mode: JavaSelectionMode, managed: bool) -> String {
    lookup(match (mode, managed) {
        (JavaSelectionMode::Automatic, false) => "gui-java-origin-automatic",
        (JavaSelectionMode::Automatic, true) => "gui-java-origin-automatic-managed",
        (JavaSelectionMode::PinnedByHost, false) => "gui-java-origin-host",
        (JavaSelectionMode::PinnedByHost, true) => "gui-java-origin-host-managed",
        (JavaSelectionMode::PinnedByLauncher, false) => "gui-java-origin-launcher",
        (JavaSelectionMode::PinnedByLauncher, true) => "gui-java-origin-launcher-managed",
    })
}

/// "Java 17", or "Java 8, update 320 or older (Forge)" when the loader narrows
/// the requirement.
#[must_use]
pub fn expected_text(expected: &ExpectedRuntime) -> String {
    let major = Arg::from(expected.major().to_string());
    let Some(loader) = expected.bounded_by() else {
        return lookup_args("gui-java-expected", [("major", major)]);
    };
    let loader = Arg::from(loader_display_name(loader));
    match (expected.java.minimum, expected.java.maximum) {
        (Some(minimum), Some(maximum)) => lookup_args(
            "gui-java-expected-between",
            [
                ("major", major),
                ("minimum", Arg::from(minimum.update.to_string())),
                ("maximum", Arg::from(maximum.update.to_string())),
                ("loader", loader),
            ],
        ),
        (None, Some(maximum)) => lookup_args(
            "gui-java-expected-at-most",
            [
                ("major", major),
                ("update", Arg::from(maximum.update.to_string())),
                ("loader", loader),
            ],
        ),
        (Some(minimum), None) => lookup_args(
            "gui-java-expected-at-least",
            [
                ("major", major),
                ("update", Arg::from(minimum.update.to_string())),
                ("loader", loader),
            ],
        ),
        (None, None) => lookup_args("gui-java-expected", [("major", major)]),
    }
}

/// Why a host refuses a runtime, in the user's words.
#[must_use]
pub fn mismatch_text(mismatch: &JavaMismatch) -> String {
    match mismatch {
        JavaMismatch::Major { required, found } => lookup_args(
            "gui-diagnosis-java-major",
            [
                ("required", Arg::from(required.to_string())),
                ("found", Arg::from(found.to_string())),
            ],
        ),
        JavaMismatch::TooOld { minimum, found } => lookup_args(
            "gui-diagnosis-java-too-old",
            [
                ("minimum", Arg::from(minimum.as_str())),
                ("found", Arg::from(found.as_str())),
            ],
        ),
        JavaMismatch::TooNew { maximum, found } => lookup_args(
            "gui-diagnosis-java-too-new",
            [
                ("maximum", Arg::from(maximum.as_str())),
                ("found", Arg::from(found.as_str())),
            ],
        ),
        JavaMismatch::Architecture { expected, found } => lookup_args(
            "gui-diagnosis-java-architecture",
            [
                ("expected", Arg::from(expected.as_str())),
                ("found", Arg::from(found.as_str())),
            ],
        ),
    }
}

/// Why a picker entry is disabled. The row already shows the runtime's
/// version, so a wrong major names what the host requires instead.
fn choice_reason(mismatch: &JavaMismatch) -> String {
    match mismatch {
        JavaMismatch::Major { required, .. } => lookup_args(
            "gui-java-choice-requires",
            [("required", Arg::from(required.to_string()))],
        ),
        other => mismatch_text(other),
    }
}

/// Why no runtime would be chosen.
#[must_use]
pub fn unavailable_text(reason: JavaUnavailable) -> String {
    lookup(match reason {
        JavaUnavailable::DetectionDisabled => "gui-java-resolved-detection-off",
        JavaUnavailable::NothingCompatible => "gui-java-resolved-nothing",
    })
}

/// What a selection resolves to, as a name and a detail line: the runtime and
/// its path, or why there is none.
///
/// The name carries how the selection was made only when something was
/// chosen; a selection that resolves to nothing chose nothing.
#[must_use]
pub fn resolved_text(mode: JavaSelectionMode, resolved: &ResolvedJava) -> (String, String) {
    let chosen = |name: String, managed: bool| format!("{name} ({})", origin_text(mode, managed));
    match resolved {
        ResolvedJava::Runtime { runtime, rejection } => (
            chosen(runtime_line(runtime), runtime.managed),
            rejection.as_ref().map_or_else(
                || runtime.path.display().to_string(),
                |rejection| {
                    lookup_args(
                        "gui-java-refused-detail",
                        [
                            ("path", Arg::from(runtime.path.display().to_string())),
                            ("reason", Arg::from(mismatch_text(rejection))),
                        ],
                    )
                },
            ),
        ),
        ResolvedJava::Unusable { path } => (
            chosen(lookup("gui-java-resolved-unusable"), false),
            path.display().to_string(),
        ),
        ResolvedJava::DownloadsAtLaunch { major } => (
            chosen(
                lookup_args(
                    "gui-fix-java-download",
                    [("major", Arg::from(major.to_string()))],
                ),
                true,
            ),
            String::new(),
        ),
        ResolvedJava::Unavailable(reason) => (unavailable_text(*reason), String::new()),
    }
}

/// The key the picker gives the entry that clears the host's own Java pin.
pub const DEFAULT_CHOICE: &str = "default";

/// A host's Java as its runtime section and workspace header show it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JavaStatusView {
    /// `healthy`, `customized`, `problem`, or `unchecked`.
    pub tone: String,
    /// The tone in words, so color is never its only sign.
    pub tone_label: String,
    /// The runtime, or what stands in for one.
    pub runtime: String,
    pub path: String,
    /// How the runtime was chosen.
    pub origin: String,
    pub expected: String,
    /// What is wrong, when something is.
    pub problem: String,
    /// The install action's label, or empty when none is offered.
    pub install: String,
    /// The header's one-line summary.
    pub summary: String,
}

/// One entry of a host's Java picker.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JavaChoiceView {
    /// [`DEFAULT_CHOICE`] or the runtime's executable path.
    pub key: String,
    pub title: String,
    pub detail: String,
    pub compatible: bool,
    pub current: bool,
    /// Why the host refuses it, empty when it accepts it.
    pub reason: String,
}

/// What the runtime section says about a host's Java.
///
/// `unchecked` carries the reason the check could not run, already in the
/// user's words.
#[must_use]
pub fn status_view(status: &HostJavaStatus, unchecked: &dyn Fn() -> String) -> JavaStatusView {
    let tone = status.tone();
    let mut view = JavaStatusView {
        tone: tone.as_str().to_owned(),
        tone_label: lookup(match tone {
            ephervex_core::JavaStatusTone::Healthy => "gui-java-tone-healthy",
            ephervex_core::JavaStatusTone::Customized => "gui-java-tone-customized",
            ephervex_core::JavaStatusTone::Problem => "gui-java-tone-problem",
            ephervex_core::JavaStatusTone::Unchecked => "gui-java-tone-unchecked",
        }),
        expected: expected_text(&status.expected),
        install: status.install.map_or_else(String::new, |offer| {
            lookup_args(
                "gui-java-install-for-host",
                [("major", Arg::from(offer.major.to_string()))],
            )
        }),
        ..JavaStatusView::default()
    };
    if let Some(runtime) = status.runtime() {
        view.runtime = runtime_line(runtime);
        view.path = runtime.path.display().to_string();
        view.origin = origin_text(status.mode, runtime.managed);
        view.summary = if runtime.vendor.trim().is_empty() {
            lookup_args(
                "gui-java-summary-plain",
                [("version", Arg::from(runtime.version.as_str()))],
            )
        } else {
            lookup_args(
                "gui-java-summary",
                [
                    ("version", Arg::from(runtime.version.as_str())),
                    ("vendor", Arg::from(runtime.vendor.as_str())),
                ],
            )
        };
    } else {
        match &status.check {
            JavaCheck::DownloadsAtLaunch { required_major } => {
                view.runtime = lookup_args(
                    "gui-fix-java-download",
                    [("major", Arg::from(required_major.to_string()))],
                );
                view.origin = origin_text(JavaSelectionMode::Automatic, true);
                view.summary = lookup_args(
                    "gui-java-summary-download",
                    [("major", Arg::from(required_major.to_string()))],
                );
            }
            JavaCheck::Failed(_) => {
                view.runtime = lookup("gui-java-status-unchecked");
                view.problem = unchecked();
                view.summary = view.runtime.clone();
            }
            JavaCheck::Selected { .. } | JavaCheck::Refused { .. } => {
                view.runtime = lookup("gui-diagnosis-check-java-refused");
                view.path = status
                    .pinned_path
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default();
                if status.mode != JavaSelectionMode::Automatic {
                    view.origin = origin_text(status.mode, false);
                }
                view.summary = view.runtime.clone();
            }
        }
    }
    if let Some(problem) = status
        .findings
        .iter()
        .find(|finding| finding.class() == FindingClass::Problem)
    {
        view.problem = problem_text(&problem.kind);
    }
    view
}

/// A Java problem in one line.
fn problem_text(kind: &FindingKind) -> String {
    match kind {
        FindingKind::JavaIncompatible { mismatch, .. } => mismatch_text(mismatch),
        FindingKind::JavaUnusable { .. } => lookup("gui-diagnosis-java-unusable"),
        FindingKind::JavaUnavailable { reason, .. } => unavailable_text(*reason),
        _ => String::new(),
    }
}

/// The entries of a host's Java picker: the default first, then the runtimes
/// in the order core lists them, without the one the default resolves to
/// unless this host pins it.
#[must_use]
pub fn choice_views(choices: &JavaChoices) -> Vec<JavaChoiceView> {
    let follows_default = choices.mode != JavaSelectionMode::PinnedByHost;
    let default = if let Some(launcher) = &choices.launcher {
        JavaChoiceView {
            key: DEFAULT_CHOICE.to_owned(),
            title: lookup("gui-java-choice-launcher"),
            detail: launcher.runtime.as_ref().map_or_else(
                || launcher.path.display().to_string(),
                |runtime| format!("{} ({})", runtime_line(runtime), runtime.path.display()),
            ),
            compatible: launcher.is_compatible(),
            current: follows_default,
            reason: match (&launcher.runtime, &launcher.rejection) {
                (None, _) => lookup("gui-java-resolved-unusable"),
                (Some(_), Some(rejection)) => choice_reason(rejection),
                (Some(_), None) => String::new(),
            },
        }
    } else {
        let (detail, reason) = match &choices.automatic {
            AutomaticJava::Runtime(runtime) => (
                format!("{} ({})", runtime_line(runtime), runtime.path.display()),
                String::new(),
            ),
            AutomaticJava::DownloadsAtLaunch { major } => (
                lookup_args(
                    "gui-fix-java-download",
                    [("major", Arg::from(major.to_string()))],
                ),
                String::new(),
            ),
            AutomaticJava::Unavailable(reason) => (String::new(), unavailable_text(*reason)),
        };
        JavaChoiceView {
            key: DEFAULT_CHOICE.to_owned(),
            title: lookup("gui-java-choice-automatic"),
            detail,
            compatible: reason.is_empty(),
            current: follows_default,
            reason,
        }
    };
    // The default entry already offers the runtime it resolves to, so that
    // runtime gets its own row only while this host pins it.
    let resolved = match (&choices.launcher, &choices.automatic) {
        (Some(launcher), _) => launcher.runtime.as_ref().map(|runtime| &runtime.path),
        (None, AutomaticJava::Runtime(runtime)) => Some(&runtime.path),
        (None, _) => None,
    };
    std::iter::once(default)
        .chain(
            choices
                .runtimes
                .iter()
                .filter(|choice| choice.current || Some(&choice.runtime.path) != resolved)
                .map(choice_view),
        )
        .collect()
}

/// One runtime as a picker entry.
#[must_use]
pub fn choice_view(choice: &JavaChoice) -> JavaChoiceView {
    JavaChoiceView {
        key: choice.runtime.path.display().to_string(),
        title: runtime_line(&choice.runtime),
        detail: if choice.runtime.managed {
            lookup_args(
                "gui-java-choice-managed",
                [("path", Arg::from(choice.runtime.path.display().to_string()))],
            )
        } else {
            choice.runtime.path.display().to_string()
        },
        compatible: choice.is_compatible(),
        current: choice.current,
        reason: choice
            .rejection
            .as_ref()
            .map(choice_reason)
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ephervex_core::{
        ExpectedJava, JavaSourceKind, JavaVersion, RequiredJava, RequirementBasis,
    };

    use super::*;

    fn expected(loader: LoaderKind, maximum: Option<JavaVersion>) -> ExpectedRuntime {
        let required = RequiredJava {
            major: 8,
            basis: RequirementBasis::FallbackTable,
        };
        let bounds: Vec<ephervex_core::VersionBound> = maximum
            .map(|maximum| {
                ephervex_core::VersionBound::new("forge-legacy-java8")
                    .major(8)
                    .at_most(maximum)
            })
            .into_iter()
            .collect();
        ExpectedRuntime {
            java: ExpectedJava::new(&required, &bounds),
            loader,
        }
    }

    #[test]
    fn a_requirement_without_loader_bounds_is_just_the_major() {
        assert_eq!(
            expected_text(&expected(LoaderKind::Vanilla, None)),
            "Java 8"
        );
    }

    #[test]
    fn a_loader_ceiling_names_the_update_and_the_loader() {
        assert_eq!(
            expected_text(&expected(
                LoaderKind::Forge,
                Some(JavaVersion::new(8, 0, 320))
            )),
            "Java 8, update 320 or older (Forge)"
        );
    }

    #[test]
    fn a_runtime_line_shows_version_vendor_and_architecture_exactly() {
        let runtime = JavaRuntimeSummary {
            version: "21.0.8".to_owned(),
            major: 21,
            path: PathBuf::from("/opt/jdk/bin/java"),
            managed: false,
            vendor: "Eclipse Adoptium".to_owned(),
            arch: "amd64".to_owned(),
            source: JavaSourceKind::System,
        };
        assert_eq!(
            runtime_line(&runtime),
            "Java 21.0.8, Eclipse Adoptium, amd64"
        );
    }

    #[test]
    fn a_runtime_that_reported_no_vendor_or_architecture_says_so() {
        let runtime = JavaRuntimeSummary {
            version: "1.6.0_45".to_owned(),
            major: 6,
            path: PathBuf::from("/opt/jdk6/bin/java"),
            managed: false,
            vendor: String::new(),
            arch: String::new(),
            source: JavaSourceKind::UserSupplied,
        };
        assert_eq!(
            runtime_line(&runtime),
            "Java 1.6.0_45, vendor not reported, architecture not reported"
        );
    }

    #[test]
    fn a_launcher_pin_is_named_as_the_launcher_s_not_the_host_s() {
        assert_ne!(
            origin_text(JavaSelectionMode::PinnedByLauncher, false),
            origin_text(JavaSelectionMode::PinnedByHost, false)
        );
    }

    #[test]
    fn a_selection_that_resolves_to_nothing_is_not_described_as_chosen() {
        let (name, _) = resolved_text(
            JavaSelectionMode::Automatic,
            &ResolvedJava::Unavailable(JavaUnavailable::DetectionDisabled),
        );
        assert_eq!(name, lookup("gui-java-resolved-detection-off"));
    }

    fn summary(version: &str, path: &str) -> JavaRuntimeSummary {
        JavaRuntimeSummary {
            version: version.to_owned(),
            major: version
                .split('.')
                .next()
                .and_then(|major| major.parse().ok())
                .unwrap_or_default(),
            path: PathBuf::from(path),
            managed: false,
            vendor: "Eclipse Adoptium".to_owned(),
            arch: "amd64".to_owned(),
            source: JavaSourceKind::System,
        }
    }

    fn java_21() -> ExpectedRuntime {
        let required = RequiredJava {
            major: 21,
            basis: RequirementBasis::FallbackTable,
        };
        ExpectedRuntime {
            java: ExpectedJava::new(&required, &[]),
            loader: LoaderKind::Vanilla,
        }
    }

    fn host() -> ephervex_core::InstalledHost {
        ephervex_core::InstalledHost::Instance(ephervex_model::InstanceId::new())
    }

    #[test]
    fn the_picker_starts_with_automatic_then_lists_runtimes_with_their_reasons() {
        let choices = JavaChoices {
            host: host(),
            expected: java_21(),
            mode: JavaSelectionMode::Automatic,
            automatic: AutomaticJava::Runtime(summary("21.0.8", "/opt/jdk-21/bin/java")),
            launcher: None,
            runtimes: vec![
                JavaChoice {
                    runtime: summary("21.0.8", "/opt/jdk-21/bin/java"),
                    rejection: None,
                    current: false,
                },
                JavaChoice {
                    runtime: summary("17.0.12", "/opt/jdk-17/bin/java"),
                    rejection: Some(JavaMismatch::Major {
                        required: 21,
                        found: 17,
                    }),
                    current: false,
                },
            ],
        };

        let views = choice_views(&choices);

        assert_eq!(
            views.len(),
            2,
            "the runtime Automatic resolves to is not listed twice"
        );
        assert_eq!(views[0].key, DEFAULT_CHOICE);
        assert_eq!(views[0].title, lookup("gui-java-choice-automatic"));
        assert!(views[0].current && views[0].compatible);
        assert!(
            views[0]
                .detail
                .contains("Java 21.0.8, Eclipse Adoptium, amd64")
        );
        assert_eq!(views[1].key, "/opt/jdk-17/bin/java");
        assert!(!views[1].compatible);
        assert_eq!(views[1].reason, "This host requires Java 21.");
    }

    /// A host that pins the runtime the default entry would also choose keeps
    /// its pin in the list, so the current choice is always shown.
    #[test]
    fn a_pinned_runtime_stays_listed_even_when_the_default_resolves_to_it() {
        let runtime = summary("21.0.8", "/opt/jdk-21/bin/java");
        let choices = JavaChoices {
            host: host(),
            expected: java_21(),
            mode: JavaSelectionMode::PinnedByHost,
            automatic: AutomaticJava::Runtime(runtime.clone()),
            launcher: None,
            runtimes: vec![JavaChoice {
                runtime,
                rejection: None,
                current: true,
            }],
        };

        let views = choice_views(&choices);

        assert_eq!(views.len(), 2);
        assert!(!views[0].current);
        assert_eq!(views[1].key, "/opt/jdk-21/bin/java");
        assert!(views[1].current);
    }

    #[test]
    fn the_runtime_a_launcher_preference_names_is_not_listed_twice() {
        let runtime = summary("21.0.8", "/opt/jdk-21/bin/java");
        let choices = JavaChoices {
            host: host(),
            expected: java_21(),
            mode: JavaSelectionMode::PinnedByLauncher,
            automatic: AutomaticJava::Runtime(summary("21.0.9", "/opt/jdk-21.0.9/bin/java")),
            launcher: Some(ephervex_core::LauncherJavaChoice {
                path: runtime.path.clone(),
                runtime: Some(runtime.clone()),
                rejection: None,
            }),
            runtimes: vec![
                JavaChoice {
                    runtime,
                    rejection: None,
                    current: false,
                },
                JavaChoice {
                    runtime: summary("21.0.9", "/opt/jdk-21.0.9/bin/java"),
                    rejection: None,
                    current: false,
                },
            ],
        };

        let keys: Vec<_> = choice_views(&choices)
            .into_iter()
            .map(|view| view.key)
            .collect();

        assert_eq!(keys, [DEFAULT_CHOICE, "/opt/jdk-21.0.9/bin/java"]);
    }

    #[test]
    fn a_launcher_preference_takes_the_default_entry_and_keeps_its_own_verdict() {
        let choices = JavaChoices {
            host: host(),
            expected: java_21(),
            mode: JavaSelectionMode::PinnedByHost,
            automatic: AutomaticJava::Unavailable(JavaUnavailable::DetectionDisabled),
            launcher: Some(ephervex_core::LauncherJavaChoice {
                path: PathBuf::from("/opt/jdk-17/bin/java"),
                runtime: Some(summary("17.0.12", "/opt/jdk-17/bin/java")),
                rejection: Some(JavaMismatch::Major {
                    required: 21,
                    found: 17,
                }),
            }),
            runtimes: Vec::new(),
        };

        let views = choice_views(&choices);
        assert_eq!(views.len(), 1);
        let default = &views[0];

        assert_eq!(default.title, lookup("gui-java-choice-launcher"));
        assert!(!default.current, "the host pins its own runtime");
        assert!(!default.compatible);
        assert_eq!(default.reason, "This host requires Java 21.");
    }

    #[test]
    fn a_valid_host_pin_reads_as_customized_with_how_it_was_chosen() {
        let runtime = summary("21.0.8", "/opt/jdk-21/bin/java");
        let status = HostJavaStatus {
            host: host(),
            expected: java_21(),
            mode: JavaSelectionMode::PinnedByHost,
            pinned_path: Some(runtime.path.clone()),
            check: JavaCheck::Selected {
                required_major: 21,
                runtime: runtime.clone(),
                layer: Some(ephervex_core::JavaLayer::Host),
            },
            findings: vec![ephervex_core::Finding {
                kind: FindingKind::JavaCustomized {
                    layer: ephervex_core::JavaLayer::Host,
                    runtime,
                    required_major: 21,
                },
                fix: None,
            }],
            install: None,
        };

        let view = status_view(&status, &String::new);

        assert_eq!(view.tone, "customized");
        assert_eq!(view.tone_label, lookup("gui-java-tone-customized"));
        assert_eq!(view.runtime, "Java 21.0.8, Eclipse Adoptium, amd64");
        assert_eq!(view.origin, "pinned by this host");
        assert_eq!(view.expected, "Java 21");
        assert_eq!(view.summary, "Java 21.0.8 (Eclipse Adoptium)");
        assert!(view.problem.is_empty() && view.install.is_empty());
    }

    #[test]
    fn a_runtime_that_reported_no_vendor_is_summarized_by_its_version_alone() {
        let mut runtime = summary("1.6.0_45", "/opt/jdk6/bin/java");
        runtime.vendor = String::new();
        let status = HostJavaStatus {
            host: host(),
            expected: java_21(),
            mode: JavaSelectionMode::PinnedByHost,
            pinned_path: Some(runtime.path.clone()),
            check: JavaCheck::Refused {
                required_major: Some(21),
                attempted: Some(runtime),
            },
            findings: Vec::new(),
            install: None,
        };

        assert_eq!(status_view(&status, &String::new).summary, "Java 1.6.0_45");
    }

    #[test]
    fn a_refused_pin_names_the_problem_and_offers_the_install() {
        let runtime = summary("17.0.12", "/opt/jdk-17/bin/java");
        let status = HostJavaStatus {
            host: host(),
            expected: java_21(),
            mode: JavaSelectionMode::PinnedByHost,
            pinned_path: Some(runtime.path.clone()),
            check: JavaCheck::Refused {
                required_major: Some(21),
                attempted: Some(runtime.clone()),
            },
            findings: vec![ephervex_core::Finding {
                kind: FindingKind::JavaIncompatible {
                    layer: ephervex_core::JavaLayer::Host,
                    path: runtime.path.clone(),
                    required_major: 21,
                    mismatch: JavaMismatch::Major {
                        required: 21,
                        found: 17,
                    },
                },
                fix: None,
            }],
            install: Some(ephervex_core::JavaInstallOffer {
                major: 21,
                maximum_update: None,
            }),
        };

        let view = status_view(&status, &String::new);

        assert_eq!(view.tone, "problem");
        assert_eq!(view.runtime, "Java 17.0.12, Eclipse Adoptium, amd64");
        assert_eq!(view.problem, "This runtime is Java 17.");
        assert_eq!(view.install, "Install Java 21");
    }
}
