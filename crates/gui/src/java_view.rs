//! How a host's Java is described: a runtime, how it was chosen, what the
//! host expects, and what a selection resolves to. Text only, shared by the
//! runtime section, the picker, the change preview and failure messages.

use ephervex_core::{
    ExpectedRuntime, JavaMismatch, JavaRuntimeSummary, JavaSelectionMode, JavaUnavailable,
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
}
