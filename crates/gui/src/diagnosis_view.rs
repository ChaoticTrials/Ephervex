//! What the Diagnostics surface says about a diagnosis, a fix preview, and a
//! fix's outcome. Text only: nothing here reads a host or changes one.

use ephervex_core::{
    CoreError, DiagnosticFix, Finding, FindingClass, FindingKind, FixOutcome, FixPreview,
    HostDiagnosis, InstalledHost, Integrity, IntegrityCheck, JavaCheck, JavaLayer, JavaMismatch,
    JavaSelectionMode, JavaUnavailable, LoaderReportedRequirement, ProposedChange, RepairRefusal,
    UnmetDependency, UnmetRequirement,
};
use ephervex_i18n::{Arg, lookup, lookup_args};
use ephervex_model::ModProvider;

/// Where a finding's secondary action takes the pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingRoute {
    Mods,
    /// The host's own runtime settings.
    HostSettings,
    /// The launcher's Java settings.
    JavaSettings,
    /// The host's live or latest log.
    Log,
    CrashReport,
    /// The Minecraft EULA review.
    Eula,
}

impl FindingRoute {
    /// The identifier the surface passes back when the action is pressed.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mods => "mods",
            Self::HostSettings => "settings",
            Self::JavaSettings => "java-settings",
            Self::Log => "log",
            Self::CrashReport => "crash-report",
            Self::Eula => "eula",
        }
    }

    /// The message id of the button that follows the route.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Mods => "gui-diagnosis-open-mods",
            Self::HostSettings => "gui-diagnosis-open-settings",
            Self::JavaSettings => "gui-diagnosis-open-java-settings",
            Self::Log => "gui-diagnosis-open-log",
            Self::CrashReport => "gui-diagnosis-open-crash-report",
            Self::Eula => "gui-diagnosis-open-eula",
        }
    }
}

/// One finding as a card shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindingView {
    pub class: FindingClass,
    pub title: String,
    pub detail: String,
    /// Why a dependency is needed, outermost first. The last entry is the
    /// dependency itself.
    pub chain: Vec<String>,
    pub files: Vec<String>,
    pub fix: Option<DiagnosticFix>,
    pub fix_label: String,
    pub route: Option<FindingRoute>,
    /// A catalog project that addresses this, installed through the install
    /// confirmation rather than a fix, because the release is chosen there.
    pub install: Option<(ModProvider, String)>,
}

/// One line of what a diagnosis checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckView {
    pub label: String,
    pub value: String,
    /// "ok", "problem", "customized", or "skipped".
    pub state: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosisView {
    pub healthy: bool,
    /// Whether the diagnosis could not answer for Java at all.
    pub incomplete: bool,
    pub problems: Vec<FindingView>,
    pub customized: Vec<FindingView>,
    pub checks: Vec<CheckView>,
    pub integrity_checked: bool,
    pub report: String,
    /// The crash report the diagnosis found after the last start.
    pub crash_report: Option<String>,
}

/// One row of a fix preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeView {
    /// What happens: "Enable", "Restore", "Current", "After".
    pub action: String,
    pub name: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewView {
    pub changes: Vec<ChangeView>,
    pub notes: Vec<String>,
    /// Why Apply is held back, or empty.
    pub blocked: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutcomeView {
    pub message: String,
    pub failed: bool,
    /// Whether anything about the host changed, which makes an earlier
    /// diagnosis out of date.
    pub changed: bool,
    pub lines: Vec<String>,
}

#[must_use]
pub fn diagnosis_view(
    diagnosis: &HostDiagnosis,
    localized: &dyn Fn(&CoreError) -> String,
) -> DiagnosisView {
    let mc = diagnosis.minecraft_version.as_str();
    let (problems, customized) = diagnosis
        .findings
        .iter()
        .map(|finding| {
            finding_view(
                finding,
                mc,
                matches!(diagnosis.host, InstalledHost::Server(_)),
            )
        })
        .partition(|view| view.class == FindingClass::Problem);
    DiagnosisView {
        healthy: diagnosis.is_healthy(),
        incomplete: matches!(diagnosis.java, JavaCheck::Failed(_)),
        problems,
        customized,
        checks: checks(diagnosis, localized),
        integrity_checked: diagnosis.integrity.is_some(),
        report: diagnosis.technical_report(),
        crash_report: diagnosis.recent_crash.clone(),
    }
}

fn count(message: &str, count: usize) -> String {
    lookup_args(message, [("count", Arg::from(count))])
}

/// How a runtime the given layer pinned, or none did, was chosen.
const fn selection_mode(layer: Option<JavaLayer>) -> JavaSelectionMode {
    match layer {
        None => JavaSelectionMode::Automatic,
        Some(JavaLayer::Host) => JavaSelectionMode::PinnedByHost,
        Some(JavaLayer::Launcher) => JavaSelectionMode::PinnedByLauncher,
    }
}

fn layer_route(layer: JavaLayer) -> FindingRoute {
    match layer {
        JavaLayer::Host => FindingRoute::HostSettings,
        JavaLayer::Launcher => FindingRoute::JavaSettings,
    }
}

fn pinned_by(layer: JavaLayer) -> String {
    lookup(match layer {
        JavaLayer::Host => "gui-diagnosis-java-pinned-host",
        JavaLayer::Launcher => "gui-diagnosis-java-pinned-launcher",
    })
}

fn mismatch_text(mismatch: &JavaMismatch) -> String {
    crate::java_view::mismatch_text(mismatch)
}

/// The versions the loader said the asking mods accept: one range when one
/// satisfies them all, otherwise each distinct range with the mods asking it.
fn accepted_versions(reported: &LoaderReportedRequirement) -> Vec<String> {
    let ranges = reported.ranges_asked_for();
    if let (Some(range), [(_, mods)]) = (&reported.accepted_range, ranges.as_slice()) {
        return vec![
            lookup_args(
                "gui-diagnosis-loader-range",
                [("range", Arg::from(range.as_str()))],
            ),
            lookup_args(
                "gui-diagnosis-loader-asked-by",
                [("mods", Arg::from(mods.join(", ")))],
            ),
        ];
    }
    let mut lines = vec![lookup("gui-diagnosis-loader-ranges")];
    lines.extend(ranges.into_iter().map(|(range, mods)| {
        lookup_args(
            "gui-diagnosis-loader-range-by",
            [
                ("range", Arg::from(range)),
                ("mods", Arg::from(mods.join(", "))),
            ],
        )
    }));
    lines
}

fn required_by(requirement: &UnmetRequirement) -> String {
    let direct = requirement
        .chain
        .last()
        .map_or_else(String::new, |entry| entry.name.clone());
    if requirement.required_by > 1 {
        lookup_args(
            "gui-diagnosis-required-by-others",
            [
                ("name", Arg::from(direct)),
                ("count", Arg::from(requirement.required_by - 1)),
            ],
        )
    } else {
        lookup_args("gui-diagnosis-required-by", [("name", Arg::from(direct))])
    }
}

fn chain_with(requirement: &UnmetRequirement, last: &str) -> Vec<String> {
    requirement
        .chain
        .iter()
        .map(|entry| entry.name.clone())
        .chain(std::iter::once(last.to_owned()))
        .collect()
}

fn integrity_word(integrity: &Integrity) -> String {
    lookup(match integrity {
        Integrity::Absent => "gui-integrity-absent",
        Integrity::PutAside => "gui-integrity-put-aside",
        Integrity::Corrupt => "gui-integrity-corrupt",
        Integrity::Unreadable => "gui-integrity-unreadable",
        Integrity::Intact | Integrity::Unverifiable => "gui-integrity-unverifiable",
    })
}

struct Card {
    title: String,
    detail: Vec<String>,
    chain: Vec<String>,
    files: Vec<String>,
    route: Option<FindingRoute>,
}

impl Card {
    fn new(title: String, detail: Vec<String>, route: Option<FindingRoute>) -> Self {
        Self {
            title,
            detail,
            chain: Vec::new(),
            files: Vec::new(),
            route,
        }
    }
}

#[must_use]
pub fn finding_view(finding: &Finding, mc: &str, on_server: bool) -> FindingView {
    let mut card = match &finding.kind {
        FindingKind::JavaIncompatible { .. }
        | FindingKind::JavaUnusable { .. }
        | FindingKind::JavaUnavailable { .. }
        | FindingKind::JavaCustomized { .. } => java_card(&finding.kind, mc),
        FindingKind::DependencyDisabled(_)
        | FindingKind::DependencyMissing(_)
        | FindingKind::DependentProbablyClientOnly(_)
        | FindingKind::DeclaredConflict(_)
        | FindingKind::LoaderReportedDependency(_)
        | FindingKind::HandPlacedMods { .. }
        | FindingKind::ModsPutAside { .. } => mod_card(&finding.kind),
        _ => other_card(&finding.kind, on_server),
    };
    let missing = missing_dependency(&finding.kind);
    // A mod its host records is restored at the recorded release instead,
    // never replaced by a newer one. Only a mod nothing records is installed
    // from a catalog, on a client and on a server alike.
    let install = match (missing, &finding.fix) {
        (
            Some(UnmetDependency::Missing {
                project_id: Some(project),
                provider: Some(provider),
                proven: true,
                recorded: None,
                ..
            }),
            None,
        ) => Some((*provider, project.clone())),
        _ => None,
    };
    if missing.is_some() {
        card.detail.push(lookup(if finding.fix.is_some() {
            "gui-diagnosis-dependency-restore-detail"
        } else if install.is_some() {
            "gui-diagnosis-dependency-install-detail"
        } else {
            "gui-diagnosis-dependency-missing-detail"
        }));
    }
    let fix_label = match (&finding.fix, missing) {
        (
            Some(DiagnosticFix::RestoreFiles(_) | DiagnosticFix::RestorePutAside(_)),
            Some(UnmetDependency::Missing { name, .. }),
        ) => lookup_args(
            "gui-diagnosis-restore-mod",
            [("name", Arg::from(name.as_str()))],
        ),
        _ => String::new(),
    };
    let fix_label = match &finding.fix {
        _ if !fix_label.is_empty() => fix_label,
        None => String::new(),
        Some(DiagnosticFix::UseAutomaticJava) if finding.class() == FindingClass::Customized => {
            lookup("gui-diagnosis-use-recommended-java")
        }
        Some(_) => lookup("gui-diagnosis-review-fix"),
    };
    FindingView {
        class: finding.class(),
        title: card.title,
        detail: card.detail.join("\n"),
        chain: card.chain,
        files: card.files,
        fix: finding.fix.clone(),
        fix_label,
        route: card.route,
        install,
    }
}

fn missing_dependency(kind: &FindingKind) -> Option<&UnmetDependency> {
    match kind {
        FindingKind::DependencyMissing(requirement) => Some(&requirement.dependency),
        _ => None,
    }
}

fn java_card(kind: &FindingKind, mc: &str) -> Card {
    let requires = |major: u32| {
        lookup_args(
            "gui-diagnosis-java-requires",
            [
                ("minecraft", Arg::from(mc)),
                ("major", Arg::from(major.to_string())),
            ],
        )
    };
    match kind {
        FindingKind::JavaIncompatible {
            layer,
            path,
            required_major,
            mismatch,
        } => Card::new(
            lookup("gui-diagnosis-java-incompatible"),
            vec![
                requires(*required_major),
                mismatch_text(mismatch),
                pinned_by(*layer),
                path.display().to_string(),
            ],
            Some(layer_route(*layer)),
        ),
        FindingKind::JavaUnusable { layer, path } => Card::new(
            lookup("gui-diagnosis-java-unusable"),
            vec![pinned_by(*layer), path.display().to_string()],
            Some(layer_route(*layer)),
        ),
        FindingKind::JavaUnavailable {
            required_major,
            reason,
        } => Card::new(
            lookup(match reason {
                JavaUnavailable::DetectionDisabled => "gui-diagnosis-java-detection-off",
                JavaUnavailable::NothingCompatible => "gui-diagnosis-java-none-installed",
            }),
            vec![
                requires(*required_major),
                lookup(match reason {
                    JavaUnavailable::DetectionDisabled => "gui-diagnosis-java-detection-off-detail",
                    JavaUnavailable::NothingCompatible => "gui-diagnosis-java-download-off-detail",
                }),
            ],
            Some(FindingRoute::JavaSettings),
        ),
        FindingKind::JavaCustomized {
            layer,
            runtime,
            required_major,
        } => Card::new(
            lookup_args(
                "gui-diagnosis-java-custom",
                [("version", Arg::from(runtime.version.as_str()))],
            ),
            vec![
                crate::java_view::runtime_line(runtime),
                runtime.path.display().to_string(),
                lookup_args(
                    "gui-diagnosis-java-satisfies",
                    [
                        ("minecraft", Arg::from(mc)),
                        ("major", Arg::from(required_major.to_string())),
                    ],
                ),
                pinned_by(*layer),
            ],
            Some(layer_route(*layer)),
        ),
        _ => Card::new(String::new(), Vec::new(), None),
    }
}

fn client_only_dependent_card(requirement: &UnmetRequirement) -> Card {
    let UnmetDependency::Missing { name, .. } = &requirement.dependency else {
        return Card::new(String::new(), Vec::new(), None);
    };
    let dependents = requirement
        .requirers
        .iter()
        .map(|requirer| requirer.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let mut card = Card::new(
        lookup_args(
            "gui-diagnosis-dependent-client-only",
            [
                ("dependent", Arg::from(dependents.as_str())),
                ("count", Arg::from(requirement.requirers.len())),
            ],
        ),
        vec![lookup_args(
            "gui-diagnosis-dependent-client-only-detail",
            [
                ("name", Arg::from(name.as_str())),
                ("dependent", Arg::from(dependents.as_str())),
                ("count", Arg::from(requirement.requirers.len())),
            ],
        )],
        Some(FindingRoute::Mods),
    );
    card.chain = chain_with(requirement, name);
    card
}

fn mod_card(kind: &FindingKind) -> Card {
    match kind {
        FindingKind::DependencyDisabled(requirement) => {
            let UnmetDependency::Disabled(disabled) = &requirement.dependency else {
                return Card::new(String::new(), Vec::new(), None);
            };
            let mut card = Card::new(
                lookup_args(
                    "gui-diagnosis-dependency-disabled",
                    [("name", Arg::from(disabled.name.as_str()))],
                ),
                vec![required_by(requirement)],
                Some(FindingRoute::Mods),
            );
            card.chain = chain_with(requirement, &disabled.name);
            card
        }
        FindingKind::DependencyMissing(requirement) => {
            let UnmetDependency::Missing { name, .. } = &requirement.dependency else {
                return Card::new(String::new(), Vec::new(), None);
            };
            let mut card = Card::new(
                lookup_args(
                    "gui-diagnosis-dependency-missing",
                    [("name", Arg::from(name.as_str()))],
                ),
                vec![required_by(requirement)],
                Some(FindingRoute::Mods),
            );
            card.chain = chain_with(requirement, name);
            card
        }
        FindingKind::DependentProbablyClientOnly(requirement) => {
            client_only_dependent_card(requirement)
        }
        FindingKind::DeclaredConflict(conflict) => Card::new(
            lookup_args(
                "gui-diagnosis-conflict",
                [
                    ("name", Arg::from(conflict.declared_by.name.as_str())),
                    ("other", Arg::from(conflict.conflicts_with.name.as_str())),
                ],
            ),
            vec![lookup("gui-diagnosis-conflict-detail")],
            Some(FindingRoute::Mods),
        ),
        FindingKind::LoaderReportedDependency(reported) => {
            let dependency = Arg::from(reported.mod_id.as_str());
            let title = match &reported.actual {
                None => lookup_args("gui-diagnosis-loader-missing", [("dependency", dependency)]),
                Some(actual) => lookup_args(
                    "gui-diagnosis-loader-version",
                    [
                        ("dependency", dependency),
                        ("actual", Arg::from(actual.as_str())),
                    ],
                ),
            };
            let mut detail = accepted_versions(reported);
            detail.push(lookup("gui-diagnosis-loader-detail"));
            Card::new(title, detail, Some(FindingRoute::Log))
        }
        FindingKind::HandPlacedMods { names } => {
            let mut card = Card::new(
                count("gui-diagnosis-hand-placed", names.len()),
                vec![lookup("gui-diagnosis-hand-placed-detail")],
                Some(FindingRoute::Mods),
            );
            card.files.clone_from(names);
            card
        }
        FindingKind::ModsPutAside { files } => {
            let mut card = Card::new(
                count("gui-diagnosis-put-aside", files.len()),
                vec![lookup("gui-diagnosis-put-aside-detail")],
                Some(FindingRoute::Mods),
            );
            card.files = files
                .iter()
                .map(|path| path.display().to_string())
                .collect();
            card
        }
        _ => Card::new(String::new(), Vec::new(), None),
    }
}

fn other_card(kind: &FindingKind, on_server: bool) -> Card {
    match kind {
        FindingKind::EulaNotAccepted { malformed } => Card::new(
            lookup(if *malformed {
                "gui-diagnosis-eula-malformed"
            } else {
                "gui-diagnosis-eula-not-accepted"
            }),
            vec![lookup("gui-diagnosis-eula-detail")],
            Some(FindingRoute::Eula),
        ),
        FindingKind::RecentCrash {
            report,
            failure,
            suspects,
            client_code_on_server,
        } => {
            let mut detail = vec![failure.clone().unwrap_or_else(|| report.clone())];
            if !suspects.is_empty() {
                detail.push(lookup_args(
                    "gui-diagnosis-crash-names",
                    [("names", Arg::from(suspects.join(", ")))],
                ));
            }
            if *client_code_on_server {
                detail.push(lookup("crash-client-code-on-server"));
            }
            Card::new(
                lookup("gui-diagnosis-crash"),
                detail,
                Some(FindingRoute::CrashReport),
            )
        }
        FindingKind::ManagedFilesDamaged { .. } | FindingKind::SharedFilesDamaged { .. } => {
            files_card(kind, on_server)
        }
        FindingKind::JvmArgumentsCustomized { layer, count: args } => Card::new(
            lookup(match layer {
                JavaLayer::Host => "gui-diagnosis-jvm-args-host",
                JavaLayer::Launcher => "gui-diagnosis-jvm-args-launcher",
            }),
            vec![count(
                match layer {
                    JavaLayer::Host => "gui-diagnosis-jvm-args-host-detail",
                    JavaLayer::Launcher => "gui-diagnosis-jvm-args-launcher-detail",
                },
                *args,
            )],
            Some(layer_route(*layer)),
        ),
        FindingKind::MemoryCustomized { min_mib, max_mib } => {
            let amount = |mib: &Option<u32>| {
                mib.map_or_else(
                    || lookup("gui-diagnosis-memory-default"),
                    |mib| format!("{mib} MiB"),
                )
            };
            Card::new(
                lookup("gui-diagnosis-memory"),
                vec![lookup_args(
                    "gui-diagnosis-memory-detail",
                    [
                        ("min", Arg::from(amount(min_mib))),
                        ("max", Arg::from(amount(max_mib))),
                    ],
                )],
                Some(FindingRoute::HostSettings),
            )
        }
        _ => Card::new(String::new(), Vec::new(), None),
    }
}

fn files_card(kind: &FindingKind, on_server: bool) -> Card {
    match kind {
        FindingKind::ManagedFilesDamaged {
            repairable,
            unrepairable,
        } => {
            let mut detail = Vec::new();
            if !repairable.is_empty() {
                detail.push(count("gui-diagnosis-files-restorable", repairable.len()));
            }
            if !unrepairable.is_empty() {
                // A server has no pack of its own to import again.
                detail.push(count(
                    if on_server {
                        "gui-diagnosis-files-unrestorable-server"
                    } else {
                        "gui-diagnosis-files-unrestorable"
                    },
                    unrepairable.len(),
                ));
            }
            let mut card = Card::new(
                count(
                    "gui-diagnosis-files-damaged",
                    repairable.len() + unrepairable.len(),
                ),
                detail,
                None,
            );
            card.files = repairable
                .iter()
                .chain(unrepairable)
                .map(|(path, integrity)| {
                    format!("{} ({})", path.display(), integrity_word(integrity))
                })
                .collect();
            card
        }
        FindingKind::SharedFilesDamaged { files } => {
            let mut card = Card::new(
                count("gui-diagnosis-shared-damaged", files.len()),
                vec![lookup("gui-integrity-shared-detail")],
                None,
            );
            card.files = files
                .iter()
                .map(|path| path.display().to_string())
                .collect();
            card
        }
        _ => Card::new(String::new(), Vec::new(), None),
    }
}

fn checks(diagnosis: &HostDiagnosis, localized: &dyn Fn(&CoreError) -> String) -> Vec<CheckView> {
    let has = |test: fn(&FindingKind) -> bool| {
        diagnosis.findings.iter().any(|finding| test(&finding.kind))
    };
    let mut checks = Vec::new();
    // A server start is refused before anything else here matters.
    if matches!(diagnosis.host, InstalledHost::Server(_)) {
        let refused = has(|kind| matches!(kind, FindingKind::EulaNotAccepted { .. }));
        checks.push(CheckView {
            label: lookup("gui-diagnosis-check-eula"),
            value: lookup(if refused {
                "gui-diagnosis-check-eula-refused"
            } else {
                "gui-diagnosis-check-eula-accepted"
            }),
            state: if refused { "problem" } else { "ok" },
        });
    }
    checks.push(java_check(&diagnosis.java, localized));
    let mods_problem = has(|kind| {
        matches!(
            kind,
            FindingKind::DependencyDisabled(_)
                | FindingKind::DependencyMissing(_)
                | FindingKind::DependentProbablyClientOnly(_)
                | FindingKind::DeclaredConflict(_)
        )
    });
    let mut mods_value = lookup_args(
        "gui-diagnosis-check-mods-value",
        [
            ("enabled", Arg::from(diagnosis.mods.enabled)),
            ("disabled", Arg::from(diagnosis.mods.disabled)),
            ("answered", Arg::from(diagnosis.mods.answered)),
            ("unanswered", Arg::from(diagnosis.mods.unanswered)),
        ],
    );
    if diagnosis.mods.unconfirmed > 0 {
        mods_value.push(' ');
        mods_value.push_str(&count(
            "gui-diagnosis-check-mods-unconfirmed",
            diagnosis.mods.unconfirmed,
        ));
    }
    checks.push(CheckView {
        label: lookup("gui-diagnosis-check-mods"),
        value: mods_value,
        state: if mods_problem { "problem" } else { "ok" },
    });
    let loader_problem = has(|kind| matches!(kind, FindingKind::LoaderReportedDependency(_)));
    checks.push(CheckView {
        label: lookup("gui-diagnosis-check-log"),
        value: lookup(if !diagnosis.loader_log_read {
            "gui-diagnosis-check-log-none"
        } else if loader_problem {
            "gui-diagnosis-check-log-refused"
        } else {
            "gui-diagnosis-check-log-clean"
        }),
        state: if loader_problem {
            "problem"
        } else if diagnosis.loader_log_read {
            "ok"
        } else {
            "skipped"
        },
    });
    checks.push(CheckView {
        label: lookup("gui-diagnosis-check-crash"),
        value: diagnosis.recent_crash.as_ref().map_or_else(
            || lookup("gui-diagnosis-check-crash-none"),
            |report| {
                lookup_args(
                    "gui-diagnosis-check-crash-found",
                    [("report", Arg::from(report.as_str()))],
                )
            },
        ),
        state: if diagnosis.recent_crash.is_some() {
            "problem"
        } else {
            "ok"
        },
    });
    checks.extend(integrity_checks(diagnosis.integrity.as_ref(), localized));
    checks
}

fn java_check(java: &JavaCheck, localized: &dyn Fn(&CoreError) -> String) -> CheckView {
    let (value, state) = match java {
        JavaCheck::Selected {
            required_major,
            runtime,
            layer,
        } => (
            lookup_args(
                "gui-diagnosis-check-java-runtime",
                [
                    (
                        "runtime",
                        Arg::from(crate::java_view::runtime_line(runtime)),
                    ),
                    (
                        "origin",
                        Arg::from(crate::java_view::origin_text(
                            selection_mode(*layer),
                            runtime.managed,
                        )),
                    ),
                    ("major", Arg::from(required_major.to_string())),
                ],
            ),
            if layer.is_some() { "customized" } else { "ok" },
        ),
        JavaCheck::DownloadsAtLaunch { required_major } => (
            lookup_args(
                "gui-diagnosis-check-java-download",
                [("major", Arg::from(required_major.to_string()))],
            ),
            "ok",
        ),
        JavaCheck::Refused {
            attempted: Some(runtime),
            ..
        } => (
            lookup_args(
                "gui-diagnosis-check-java-refused-runtime",
                [(
                    "runtime",
                    Arg::from(crate::java_view::runtime_line(runtime)),
                )],
            ),
            "problem",
        ),
        JavaCheck::Refused { .. } => (lookup("gui-diagnosis-check-java-refused"), "problem"),
        JavaCheck::Failed(error) => (
            lookup_args(
                "gui-diagnosis-check-failed",
                [("error", Arg::from(localized(error)))],
            ),
            "skipped",
        ),
    };
    CheckView {
        label: lookup("gui-diagnosis-check-java"),
        value,
        state,
    }
}

/// A damaged local copy is a problem to fix; any other failure means the check
/// could not run.
fn shared_failure(damaged_local_data: bool, error: &str) -> (String, &'static str) {
    let (message, state) = if damaged_local_data {
        ("gui-integrity-shared-damaged", "problem")
    } else {
        ("gui-diagnosis-check-failed", "skipped")
    };
    (lookup_args(message, [("error", Arg::from(error))]), state)
}

fn integrity_checks(
    integrity: Option<&IntegrityCheck>,
    localized: &dyn Fn(&CoreError) -> String,
) -> Vec<CheckView> {
    let Some(check) = integrity else {
        return vec![CheckView {
            label: lookup("gui-diagnosis-check-files"),
            value: lookup("gui-diagnosis-check-files-skipped"),
            state: "skipped",
        }];
    };
    let files = &check.files;
    let unsound = files
        .files
        .iter()
        .filter(|file| !file.integrity.is_sound())
        .count();
    let mut checks = vec![CheckView {
        label: lookup("gui-diagnosis-check-files"),
        value: if files.files.is_empty() {
            lookup("gui-integrity-nothing-recorded")
        } else {
            lookup_args(
                "gui-diagnosis-check-files-value",
                [
                    ("count", Arg::from(files.files.len())),
                    ("intact", Arg::from(files.count(&Integrity::Intact))),
                    ("aside", Arg::from(files.count(&Integrity::PutAside))),
                    ("problems", Arg::from(unsound)),
                    (
                        "unverifiable",
                        Arg::from(files.count(&Integrity::Unverifiable)),
                    ),
                ],
            )
        },
        state: if unsound > 0 { "problem" } else { "ok" },
    }];
    if let Some(shared) = &check.shared {
        let (value, state) = match shared {
            Err(error) => shared_failure(error.is_damaged_local_data(), &localized(error)),
            Ok(shared) if !shared.metadata_cached => {
                (lookup("gui-integrity-shared-not-cached"), "skipped")
            }
            Ok(shared) => {
                let faults = shared.faults().count();
                let waiting = shared.not_yet_fetched().count();
                let checked = shared.files.len() - waiting;
                let mut parts = Vec::new();
                if faults > 0 {
                    parts.push(count("gui-integrity-shared-faults", faults));
                } else if checked > 0 {
                    parts.push(count("gui-integrity-shared-sound", checked));
                }
                if waiting > 0 {
                    parts.push(count("gui-integrity-shared-waiting", waiting));
                }
                if shared.loader_libraries_uncovered {
                    parts.push(lookup("gui-integrity-shared-loader"));
                }
                let state = if faults > 0 {
                    "problem"
                } else if checked == 0 {
                    "skipped"
                } else {
                    "ok"
                };
                (parts.join(" "), state)
            }
        };
        checks.push(CheckView {
            label: lookup("gui-integrity-shared-title"),
            value,
            state,
        });
    }
    checks
}

#[must_use]
pub fn preview_view(preview: &FixPreview) -> PreviewView {
    let mut changes = Vec::new();
    let mut notes = Vec::new();
    for change in &preview.changes {
        match change {
            ProposedChange::Enable { name, logical_path } => changes.push(ChangeView {
                action: lookup("gui-fix-enable"),
                name: name.clone(),
                detail: logical_path.display().to_string(),
            }),
            ProposedChange::Disable { name, logical_path } => changes.push(ChangeView {
                action: lookup("gui-fix-disable"),
                name: name.clone(),
                detail: logical_path.display().to_string(),
            }),
            ProposedChange::Restore { logical_path } => changes.push(ChangeView {
                action: lookup("gui-fix-restore"),
                name: logical_path.display().to_string(),
                detail: String::new(),
            }),
            ProposedChange::UseJava(change) => {
                for (action, side) in [
                    ("gui-fix-current", &change.current),
                    ("gui-fix-proposed", &change.proposed),
                ] {
                    let (name, detail) = crate::java_view::resolved_text(side.mode, &side.resolved);
                    changes.push(ChangeView {
                        action: lookup(action),
                        name,
                        detail,
                    });
                }
                notes.push(lookup_args(
                    "gui-fix-java-expected",
                    [(
                        "expected",
                        Arg::from(crate::java_view::expected_text(&change.expected)),
                    )],
                ));
            }
        }
    }
    notes.push(lookup(match preview.fix {
        DiagnosticFix::EnableMods(_) => "gui-fix-enable-note",
        DiagnosticFix::DisableMods(_) => "gui-fix-disable-note",
        DiagnosticFix::UseAutomaticJava | DiagnosticFix::PinJava(_) => "gui-fix-java-note",
        DiagnosticFix::RestoreFiles(_) => "gui-fix-restore-note",
        DiagnosticFix::RestorePutAside(_) => "gui-fix-restore-put-aside-note",
    }));
    if preview.is_empty() {
        notes = vec![lookup("gui-fix-nothing-left")];
    }
    PreviewView {
        changes,
        notes,
        blocked: preview.blocked.map_or_else(String::new, |reason| {
            lookup(match reason {
                RepairRefusal::HostRunning => "gui-fix-blocked-running",
                _ => "gui-fix-blocked-isolation",
            })
        }),
    }
}

/// What turning mods on did, change by change.
fn enable_message(outcome: &FixOutcome, lines: &mut Vec<String>) -> String {
    if outcome.was_canceled() {
        lines.extend(outcome.skipped.iter().cloned());
        lookup_args(
            "gui-fix-enable-stopped",
            [
                ("count", Arg::from(outcome.applied.len())),
                (
                    "total",
                    Arg::from(outcome.applied.len() + outcome.failed.len() + outcome.skipped.len()),
                ),
            ],
        )
    } else if outcome.failed.is_empty() {
        count("gui-fix-enabled", outcome.applied.len())
    } else {
        lookup_args(
            "gui-fix-enabled-partly",
            [
                ("count", Arg::from(outcome.applied.len())),
                (
                    "total",
                    Arg::from(outcome.applied.len() + outcome.failed.len()),
                ),
            ],
        )
    }
}

/// What turning mods off did. The batch is all or nothing, so a failure means
/// none of them changed.
fn disable_message(outcome: &FixOutcome, lines: &mut Vec<String>) -> String {
    if outcome.was_canceled() {
        lines.extend(outcome.skipped.iter().cloned());
        lookup_args(
            "gui-fix-disable-stopped",
            [("total", Arg::from(outcome.skipped.len()))],
        )
    } else if outcome.failed.is_empty() {
        count("gui-fix-disabled", outcome.applied.len())
    } else {
        lookup("gui-fix-disable-failed")
    }
}

#[must_use]
pub fn outcome_view(
    fix: &DiagnosticFix,
    outcome: &FixOutcome,
    localized: &dyn Fn(&CoreError) -> String,
) -> OutcomeView {
    let mut lines: Vec<String> = outcome
        .failed
        .iter()
        .map(|(name, error)| format!("{name}: {}", localized(error)))
        .collect();
    let stopped = outcome.was_canceled();
    let message = match fix {
        DiagnosticFix::EnableMods(_) => enable_message(outcome, &mut lines),
        DiagnosticFix::DisableMods(_) => disable_message(outcome, &mut lines),
        DiagnosticFix::UseAutomaticJava if outcome.failed.is_empty() => {
            lookup("gui-fix-java-applied")
        }
        DiagnosticFix::PinJava(_) if outcome.failed.is_empty() => lookup("gui-fix-java-pinned"),
        DiagnosticFix::UseAutomaticJava | DiagnosticFix::PinJava(_) => {
            lookup("gui-fix-java-not-applied")
        }
        DiagnosticFix::RestoreFiles(_) | DiagnosticFix::RestorePutAside(_) => {
            let repair = outcome.repair.clone().unwrap_or_default();
            let mut message = count(
                match fix {
                    _ if stopped => "gui-integrity-repair-stopped",
                    DiagnosticFix::RestorePutAside(_) => "gui-fix-restored-put-aside",
                    _ => "gui-integrity-repaired",
                },
                repair.repaired.len(),
            );
            if !repair.failed.is_empty() {
                message.push(' ');
                message.push_str(&count(
                    if matches!(fix, DiagnosticFix::RestorePutAside(_)) {
                        "gui-fix-restore-put-aside-failed"
                    } else {
                        "gui-integrity-repair-failed"
                    },
                    repair.failed.len(),
                ));
            }
            lines.extend(repair.failed.iter().map(|path| path.display().to_string()));
            if !repair.second_copies.is_empty() {
                message.push(' ');
                message.push_str(&count(
                    "gui-integrity-repair-second-copies",
                    repair.second_copies.len(),
                ));
                lines.extend(repair.second_copies.iter().map(|copy| {
                    lookup_args(
                        "gui-integrity-repair-second-copy",
                        [
                            ("path", Arg::from(copy.logical_path.display().to_string())),
                            ("mod_id", Arg::from(copy.mod_id.clone())),
                            ("existing", Arg::from(copy.existing.display().to_string())),
                        ],
                    )
                }));
            }
            lines.extend(repair.skipped.iter().map(|path| path.display().to_string()));
            if !repair.recovery_issues.is_empty() {
                lines.push(lookup("gui-integrity-recovery"));
                lines.extend(
                    repair
                        .recovery_issues
                        .iter()
                        .map(|issue| issue.path.display().to_string()),
                );
            }
            message
        }
    };
    OutcomeView {
        message,
        failed: !outcome.is_complete(),
        changed: !outcome.applied.is_empty(),
        lines,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::path::PathBuf;

    use ephervex_core::{
        ImpactedMod, JavaRuntimeSummary, ModPresence, RepairReport, ResolvedJava, SecondCopy,
        SideSupport, UnmetRequirement,
    };
    use ephervex_model::ModOrigin;

    use super::*;

    fn impacted(name: &str) -> ImpactedMod {
        ImpactedMod {
            logical_path: PathBuf::from("mods").join(format!("{name}.jar")),
            name: name.to_owned(),
            presence: ModPresence::Installed,
            origin: ModOrigin::UserSelected,
            project_id: None,
            knowledge: ephervex_core::DependencyKnowledge::Recorded,
            distance: 0,
        }
    }

    #[test]
    fn a_missing_dependency_shows_the_whole_chain_ending_in_what_is_missing() {
        let finding = Finding {
            kind: FindingKind::DependencyMissing(UnmetRequirement {
                dependency: UnmetDependency::Missing {
                    name: "Registrate".to_owned(),
                    project_id: None,
                    provider: None,
                    proven: true,
                    recorded: None,
                    server_side: SideSupport::Unknown,
                },
                chain: vec![impacted("Create"), impacted("Flywheel")],
                required_by: 1,
                requirers: vec![impacted("Flywheel")],
            }),
            fix: None,
        };

        let view = finding_view(&finding, "1.21.1", false);

        assert_eq!(view.class, FindingClass::Problem);
        assert_eq!(view.chain, ["Create", "Flywheel", "Registrate"]);
        assert_eq!(view.route, Some(FindingRoute::Mods));
        assert!(
            view.fix_label.is_empty(),
            "installing is never offered here"
        );
    }

    #[test]
    fn a_server_without_eula_consent_leads_with_a_refused_check_and_offers_the_eula_review() {
        let diagnosis = |findings: Vec<Finding>| HostDiagnosis {
            host: InstalledHost::Server(ephervex_model::ServerId::new()),
            minecraft_version: "1.21.1".to_owned(),
            loader: ephervex_model::LoaderSelection::vanilla(),
            java: JavaCheck::DownloadsAtLaunch { required_major: 21 },
            mods: ephervex_core::ModCounts::default(),
            integrity: None,
            loader_log_read: false,
            recent_crash: None,
            findings,
        };
        let refused = diagnosis_view(
            &diagnosis(vec![Finding {
                kind: FindingKind::EulaNotAccepted { malformed: false },
                fix: None,
            }]),
            &|_| String::new(),
        );

        assert!(!refused.healthy);
        assert_eq!(refused.problems.len(), 1, "{:?}", refused.problems);
        let problem = &refused.problems[0];
        assert_eq!(problem.title, lookup("gui-diagnosis-eula-not-accepted"));
        assert_eq!(problem.route, Some(FindingRoute::Eula));
        assert_eq!(FindingRoute::Eula.label(), "gui-diagnosis-open-eula");
        assert_eq!(refused.checks[0].label, lookup("gui-diagnosis-check-eula"));
        assert_eq!(refused.checks[0].state, "problem");

        let accepted = diagnosis_view(&diagnosis(Vec::new()), &|_| String::new());
        assert_eq!(accepted.checks[0].label, lookup("gui-diagnosis-check-eula"));
        assert_eq!(accepted.checks[0].state, "ok");
    }

    /// The loader's own words, kept as its own finding: its exact mod id, the
    /// one range every requester accepts, and who asked. No catalog action.
    #[test]
    fn a_loader_refusal_keeps_its_mod_id_range_and_requesters_and_offers_no_install() {
        let finding = Finding {
            kind: FindingKind::LoaderReportedDependency(LoaderReportedRequirement {
                mod_id: "libx".to_owned(),
                actual: None,
                requests: vec![
                    ephervex_core::LoaderRequest {
                        requested_by: "datatrader".to_owned(),
                        expected_range: "[1.19.3-4.3.0,)".to_owned(),
                    },
                    ephervex_core::LoaderRequest {
                        requested_by: "packessentials".to_owned(),
                        expected_range: "[1.20.1-5.0.14,)".to_owned(),
                    },
                ],
                accepted_range: Some("[1.20.1-5.0.14,)".to_owned()),
            }),
            fix: None,
        };

        let view = finding_view(&finding, "1.20.1", false);

        assert_eq!(
            view.title,
            lookup_args(
                "gui-diagnosis-loader-missing",
                [("dependency", Arg::from("libx"))]
            )
        );
        assert!(view.detail.contains(&lookup_args(
            "gui-diagnosis-loader-range",
            [("range", Arg::from("[1.20.1-5.0.14,)"))]
        )));
        assert!(view.detail.contains(&lookup_args(
            "gui-diagnosis-loader-asked-by",
            [("mods", Arg::from("datatrader, packessentials"))]
        )));
        assert_eq!(view.install, None);
        assert!(view.fix_label.is_empty());
    }

    fn missing_libx(recorded: Option<&str>, fix: Option<DiagnosticFix>) -> Finding {
        Finding {
            kind: FindingKind::DependencyMissing(UnmetRequirement {
                dependency: UnmetDependency::Missing {
                    name: "LibX".to_owned(),
                    project_id: Some("qEH6GYul".to_owned()),
                    provider: Some(ModProvider::Modrinth),
                    proven: true,
                    recorded: recorded.map(PathBuf::from),
                    server_side: SideSupport::Unknown,
                },
                chain: vec![impacted("Data Trader")],
                required_by: 1,
                requirers: vec![impacted("Data Trader")],
            }),
            fix,
        }
    }

    /// A server is not offered a mod the catalog says never runs on one; it is
    /// told that what asks for it likely belongs only on a client, and the fix
    /// turns that off.
    #[test]
    fn a_dependent_of_a_client_only_dependency_is_reported_as_probably_client_only() {
        let mut finding = missing_libx(
            None,
            Some(DiagnosticFix::DisableMods(vec![PathBuf::from(
                "mods/Data Trader.jar",
            )])),
        );
        let FindingKind::DependencyMissing(requirement) = finding.kind.clone() else {
            unreachable!("the fixture is a missing dependency");
        };
        finding.kind = FindingKind::DependentProbablyClientOnly(requirement);

        let view = finding_view(&finding, "1.20.1", true);

        assert_eq!(
            view.title,
            lookup_args(
                "gui-diagnosis-dependent-client-only",
                [
                    ("dependent", Arg::from("Data Trader")),
                    ("count", Arg::from(1))
                ]
            )
        );
        assert!(view.detail.contains(&lookup_args(
            "gui-diagnosis-dependent-client-only-detail",
            [
                ("name", Arg::from("LibX")),
                ("dependent", Arg::from("Data Trader")),
                ("count", Arg::from(1))
            ],
        )));
        assert_eq!(view.chain, ["Data Trader", "LibX"]);
        assert_eq!(view.install, None, "a server is never offered the install");
        assert_eq!(view.fix_label, lookup("gui-diagnosis-review-fix"));
    }

    #[test]
    fn several_dependents_of_a_client_only_dependency_are_named_in_the_plural() {
        let mut finding = missing_libx(None, None);
        let FindingKind::DependencyMissing(mut requirement) = finding.kind.clone() else {
            unreachable!("the fixture is a missing dependency");
        };
        requirement.requirers = vec![impacted("Data Trader"), impacted("Trade Tweaks")];
        finding.kind = FindingKind::DependentProbablyClientOnly(requirement);

        let view = finding_view(&finding, "1.20.1", true);

        assert!(
            view.title.contains("are probably client-only"),
            "{}",
            view.title
        );
        assert!(
            view.title.contains("Data Trader, Trade Tweaks"),
            "{}",
            view.title
        );
        assert!(view.detail.contains("need it"), "{}", view.detail);
    }

    #[test]
    fn turning_mods_off_is_previewed_and_reported_per_change() {
        let fix = DiagnosticFix::DisableMods(vec![PathBuf::from("mods/Data Trader.jar")]);
        let preview = preview_view(&FixPreview {
            fix: fix.clone(),
            changes: vec![ProposedChange::Disable {
                name: "Data Trader".to_owned(),
                logical_path: PathBuf::from("mods/Data Trader.jar"),
            }],
            blocked: None,
        });
        assert_eq!(preview.changes[0].action, lookup("gui-fix-disable"));
        assert_eq!(preview.changes[0].name, "Data Trader");
        assert_eq!(preview.notes, [lookup("gui-fix-disable-note")]);

        let applied = outcome_view(
            &fix,
            &FixOutcome {
                applied: vec!["Data Trader".to_owned()],
                ..FixOutcome::default()
            },
            &|_| String::new(),
        );
        assert_eq!(applied.message, count("gui-fix-disabled", 1));

        let canceled = outcome_view(
            &fix,
            &FixOutcome {
                skipped: vec!["Data Trader".to_owned()],
                ..FixOutcome::default()
            },
            &|_| String::new(),
        );
        assert_eq!(canceled.lines, ["Data Trader"]);
    }

    #[test]
    fn a_recorded_missing_mod_is_restored_rather_than_installed() {
        let path = "mods/LibX-1.20.1-5.0.14.jar";
        let finding = missing_libx(
            Some(path),
            Some(DiagnosticFix::RestoreFiles(vec![PathBuf::from(path)])),
        );
        let view = finding_view(&finding, "1.20.1", false);
        assert_eq!(
            view.fix_label,
            lookup_args("gui-diagnosis-restore-mod", [("name", Arg::from("LibX"))])
        );
        assert_eq!(view.install, None);
        assert!(
            view.detail
                .contains(&lookup("gui-diagnosis-dependency-restore-detail"))
        );
    }

    #[test]
    fn a_server_file_with_no_source_is_not_told_to_import_a_pack_again() {
        let finding = Finding {
            kind: FindingKind::ManagedFilesDamaged {
                repairable: Vec::new(),
                unrepairable: vec![(PathBuf::from("mods/miner.jar"), Integrity::Absent)],
            },
            fix: None,
        };

        let server = finding_view(&finding, "1.21.1", true);
        assert!(
            server
                .detail
                .contains(&count("gui-diagnosis-files-unrestorable-server", 1))
        );
        assert!(
            !server
                .detail
                .contains(&count("gui-diagnosis-files-unrestorable", 1))
        );

        let client = finding_view(&finding, "1.21.1", false);
        assert!(
            client
                .detail
                .contains(&count("gui-diagnosis-files-unrestorable", 1))
        );
    }

    #[test]
    fn a_put_aside_mod_is_a_deliberate_change_that_offers_a_restore() {
        let path = PathBuf::from("mods/miner.jar");
        let finding = Finding {
            kind: FindingKind::ModsPutAside {
                files: vec![path.clone()],
            },
            fix: Some(DiagnosticFix::RestorePutAside(vec![path])),
        };

        let view = finding_view(&finding, "1.21.1", true);
        assert_eq!(view.class, FindingClass::Customized);
        assert_eq!(view.title, count("gui-diagnosis-put-aside", 1));
        assert_eq!(view.files, ["mods/miner.jar"]);
        assert!(view.fix.is_some());
        assert_eq!(view.route, Some(FindingRoute::Mods));
    }

    #[test]
    fn an_unrecorded_missing_mod_is_installed_from_a_catalog_on_either_host() {
        let finding = missing_libx(None, None);

        let view = finding_view(&finding, "1.20.1", false);
        assert_eq!(
            view.install,
            Some((ModProvider::Modrinth, "qEH6GYul".to_owned())),
            "the host does not decide it: a server has a catalog install too"
        );
        assert!(
            view.detail
                .contains(&lookup("gui-diagnosis-dependency-install-detail"))
        );

        assert!(
            !view
                .detail
                .contains(&lookup("gui-diagnosis-dependency-missing-detail"))
        );
    }

    #[test]
    fn a_valid_custom_java_offers_the_recommended_one_rather_than_a_repair() {
        let finding = Finding {
            kind: FindingKind::JavaCustomized {
                layer: JavaLayer::Host,
                runtime: JavaRuntimeSummary {
                    version: "21.0.8".to_owned(),
                    major: 21,
                    path: PathBuf::from("/usr/lib/jvm/temurin-21/bin/java"),
                    managed: false,
                    vendor: "Eclipse Adoptium".to_owned(),
                    arch: "amd64".to_owned(),
                    source: ephervex_core::JavaSourceKind::System,
                },
                required_major: 21,
            },
            fix: Some(DiagnosticFix::UseAutomaticJava),
        };

        let view = finding_view(&finding, "1.21.1", false);

        assert_eq!(view.class, FindingClass::Customized);
        assert_eq!(view.fix_label, lookup("gui-diagnosis-use-recommended-java"));
        assert!(view.detail.contains("/usr/lib/jvm/temurin-21/bin/java"));
        assert!(view.detail.contains("Java 21.0.8, Eclipse Adoptium, amd64"));
    }

    fn summary(version: &str) -> JavaRuntimeSummary {
        JavaRuntimeSummary {
            version: version.to_owned(),
            major: version
                .split('.')
                .next()
                .and_then(|major| major.parse().ok())
                .unwrap_or_default(),
            path: PathBuf::from("/opt/jdk/bin/java"),
            managed: false,
            vendor: "Eclipse Adoptium".to_owned(),
            arch: "amd64".to_owned(),
            source: ephervex_core::JavaSourceKind::UserSupplied,
        }
    }

    #[test]
    fn the_java_check_names_the_runtime_its_vendor_architecture_and_how_it_was_chosen() {
        let check = java_check(
            &JavaCheck::Selected {
                required_major: 21,
                runtime: summary("21.0.8"),
                layer: Some(JavaLayer::Launcher),
            },
            &|_| String::new(),
        );
        assert_eq!(
            check.value,
            "Java 21.0.8, Eclipse Adoptium, amd64 (pinned by the launcher preference). \
             Requires Java 21."
        );
        assert_eq!(check.state, "customized");

        let refused = java_check(
            &JavaCheck::Refused {
                required_major: Some(21),
                attempted: Some(summary("17.0.12")),
            },
            &|_| String::new(),
        );
        assert_eq!(
            refused.value,
            "No usable runtime. Refused Java 17.0.12, Eclipse Adoptium, amd64."
        );
        assert_eq!(refused.state, "problem");
    }

    #[test]
    fn a_preview_that_found_nothing_left_to_change_says_so() {
        let view = preview_view(&FixPreview {
            fix: DiagnosticFix::EnableMods(vec![PathBuf::from("mods/a.jar")]),
            changes: Vec::new(),
            blocked: None,
        });
        assert_eq!(view.notes, [lookup("gui-fix-nothing-left")]);
        assert!(view.blocked.is_empty());
    }

    #[test]
    fn a_partly_applied_enable_names_what_did_not_change() {
        let outcome = FixOutcome {
            applied: vec!["A".to_owned()],
            failed: vec![(
                "B".to_owned(),
                CoreError::ModFileMissing {
                    path: PathBuf::from("mods/b.jar"),
                },
            )],
            skipped: Vec::new(),
            client_warnings: Vec::new(),
            server_warnings: Vec::new(),
            repair: None,
        };

        let view = outcome_view(&DiagnosticFix::EnableMods(Vec::new()), &outcome, &|_| {
            "gone".to_owned()
        });

        assert!(view.failed);
        assert_eq!(view.lines, ["B: gone"]);
    }

    #[test]
    fn a_canceled_enable_names_the_mods_it_never_reached() {
        let outcome = FixOutcome {
            applied: vec!["A".to_owned()],
            failed: Vec::new(),
            skipped: vec!["B".to_owned(), "C".to_owned()],
            client_warnings: Vec::new(),
            server_warnings: Vec::new(),
            repair: None,
        };

        let view = outcome_view(&DiagnosticFix::EnableMods(Vec::new()), &outcome, &|_| {
            String::new()
        });

        assert!(view.failed, "a canceled run is not a successful one");
        assert!(view.changed, "the mod enabled before the cancel did change");
        assert_eq!(view.lines, ["B", "C"]);
        assert_eq!(
            view.message,
            lookup_args(
                "gui-fix-enable-stopped",
                [("count", Arg::from(1_usize)), ("total", Arg::from(3_usize))],
            )
        );
    }

    #[test]
    fn a_canceled_restore_names_the_files_it_never_reached() {
        let repair = RepairReport {
            repaired: vec![PathBuf::from("mods/a.jar")],
            skipped: vec![PathBuf::from("mods/b.jar")],
            ..RepairReport::default()
        };
        let outcome = FixOutcome {
            applied: vec!["mods/a.jar".to_owned()],
            failed: Vec::new(),
            skipped: vec!["mods/b.jar".to_owned()],
            client_warnings: Vec::new(),
            server_warnings: Vec::new(),
            repair: Some(repair),
        };

        let view = outcome_view(&DiagnosticFix::RestoreFiles(Vec::new()), &outcome, &|_| {
            String::new()
        });

        assert!(view.failed);
        assert!(view.changed);
        assert_eq!(view.lines, ["mods/b.jar"]);
        assert_eq!(view.message, count("gui-integrity-repair-stopped", 1));
    }

    #[test]
    fn a_restore_left_out_as_a_second_copy_names_the_jar_already_providing_the_mod() {
        let repair = RepairReport {
            second_copies: vec![SecondCopy {
                logical_path: PathBuf::from("mods/libx.jar"),
                mod_id: "libx".to_owned(),
                existing: PathBuf::from("mods/libx-other.jar"),
            }],
            ..RepairReport::default()
        };
        let outcome = FixOutcome {
            applied: Vec::new(),
            failed: Vec::new(),
            skipped: Vec::new(),
            client_warnings: Vec::new(),
            server_warnings: Vec::new(),
            repair: Some(repair),
        };

        let view = outcome_view(&DiagnosticFix::RestoreFiles(Vec::new()), &outcome, &|_| {
            String::new()
        });

        assert!(view.failed);
        assert!(!view.changed);
        assert_eq!(
            view.lines,
            [lookup_args(
                "gui-integrity-repair-second-copy",
                [
                    ("path", Arg::from("mods/libx.jar")),
                    ("mod_id", Arg::from("libx")),
                    ("existing", Arg::from("mods/libx-other.jar")),
                ],
            )]
        );
        assert!(
            view.message
                .ends_with(&count("gui-integrity-repair-second-copies", 1)),
            "{}",
            view.message
        );
    }

    #[test]
    fn restoring_a_put_aside_mod_says_it_comes_back_from_the_kept_copy() {
        let path = PathBuf::from("mods/miner.jar");
        let fix = DiagnosticFix::RestorePutAside(vec![path.clone()]);
        let preview = preview_view(&FixPreview {
            fix: fix.clone(),
            changes: vec![ProposedChange::Restore {
                logical_path: path.clone(),
            }],
            blocked: None,
        });
        assert!(
            preview
                .notes
                .contains(&lookup("gui-fix-restore-put-aside-note"))
        );
        assert!(!preview.notes.contains(&lookup("gui-fix-restore-note")));

        let outcome = FixOutcome {
            applied: vec!["mods/miner.jar".to_owned()],
            failed: Vec::new(),
            skipped: Vec::new(),
            client_warnings: Vec::new(),
            server_warnings: Vec::new(),
            repair: Some(RepairReport {
                repaired: vec![path],
                ..RepairReport::default()
            }),
        };
        let view = outcome_view(&fix, &outcome, &|_| String::new());
        assert_eq!(view.message, count("gui-fix-restored-put-aside", 1));
    }

    fn shared_row(files: Vec<(&str, Integrity)>, loader_libraries_uncovered: bool) -> CheckView {
        let check = IntegrityCheck {
            files: ephervex_core::VerificationReport { files: Vec::new() },
            shared: Some(Ok(ephervex_core::SharedArtifactReport {
                metadata_cached: true,
                files: files
                    .into_iter()
                    .map(|(name, integrity)| ephervex_core::VerifiedFile {
                        logical_path: PathBuf::from(name),
                        integrity,
                        has_source: true,
                    })
                    .collect(),
                loader_libraries_uncovered,
            })),
        };
        integrity_checks(Some(&check), &|error| error.to_string())
            .into_iter()
            .find(|row| row.label == lookup("gui-integrity-shared-title"))
            .expect("the shared launch files row")
    }

    #[test]
    fn shared_files_not_downloaded_yet_are_left_out_of_the_checked_count() {
        let row = shared_row(
            vec![
                ("client.jar", Integrity::Intact),
                ("a.jar", Integrity::Absent),
                ("b.jar", Integrity::Absent),
            ],
            false,
        );

        assert_eq!(row.state, "ok");
        assert!(row.value.contains(&count("gui-integrity-shared-sound", 1)));
        assert!(
            row.value
                .contains(&count("gui-integrity-shared-waiting", 2))
        );
    }

    #[test]
    fn a_shared_row_with_nothing_downloaded_claims_no_check_and_is_not_graded_ok() {
        let row = shared_row(
            vec![("a.jar", Integrity::Absent), ("b.jar", Integrity::Absent)],
            true,
        );

        assert_eq!(row.state, "skipped");
        assert_eq!(
            row.value,
            format!(
                "{} {}",
                count("gui-integrity-shared-waiting", 2),
                lookup("gui-integrity-shared-loader")
            )
        );
        assert!(!row.value.contains(&count("gui-integrity-shared-sound", 0)));
    }

    #[test]
    fn a_damaged_shared_file_is_a_problem_and_still_counts_the_waiting_ones_separately() {
        let row = shared_row(
            vec![
                ("client.jar", Integrity::Corrupt),
                ("a.jar", Integrity::Absent),
            ],
            false,
        );

        assert_eq!(row.state, "problem");
        assert!(
            row.value
                .starts_with(&count("gui-integrity-shared-faults", 1))
        );
        assert!(
            row.value
                .contains(&count("gui-integrity-shared-waiting", 1))
        );
    }

    #[test]
    fn a_damaged_local_copy_is_a_problem_that_does_not_claim_the_check_could_not_run() {
        let (value, state) = shared_failure(true, "the saved copy is damaged");

        assert_eq!(state, "problem");
        assert_eq!(
            value,
            lookup_args(
                "gui-integrity-shared-damaged",
                [("error", Arg::from("the saved copy is damaged"))]
            )
        );
    }

    #[test]
    fn any_other_shared_check_failure_is_a_skipped_check() {
        let (value, state) = shared_failure(false, "offline");

        assert_eq!(state, "skipped");
        assert_eq!(
            value,
            lookup_args(
                "gui-diagnosis-check-failed",
                [("error", Arg::from("offline"))]
            )
        );
    }

    #[test]
    fn a_java_change_shows_both_runtimes_with_how_each_is_chosen_and_the_expectation() {
        let runtime = |version: &str, path: &str| ephervex_core::JavaRuntimeSummary {
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
            source: ephervex_core::JavaSourceKind::UserSupplied,
        };
        let required = ephervex_core::RequiredJava {
            major: 21,
            basis: ephervex_core::RequirementBasis::FallbackTable,
        };
        let change = ephervex_core::JavaChange {
            current: ephervex_core::JavaSelectionState {
                mode: ephervex_core::JavaSelectionMode::PinnedByHost,
                resolved: ResolvedJava::Runtime {
                    runtime: runtime("17.0.12", "/opt/jdk-17/bin/java"),
                    rejection: Some(JavaMismatch::Major {
                        required: 21,
                        found: 17,
                    }),
                },
            },
            proposed: ephervex_core::JavaSelectionState {
                mode: ephervex_core::JavaSelectionMode::PinnedByHost,
                resolved: ResolvedJava::Runtime {
                    runtime: runtime("21.0.8", "/opt/jdk-21/bin/java"),
                    rejection: None,
                },
            },
            expected: ephervex_core::ExpectedRuntime {
                java: ephervex_core::ExpectedJava::new(&required, &[]),
                loader: ephervex_model::LoaderKind::Vanilla,
            },
        };
        let view = preview_view(&FixPreview {
            fix: DiagnosticFix::PinJava(PathBuf::from("/opt/jdk-21/bin/java")),
            changes: vec![ProposedChange::UseJava(Box::new(change))],
            blocked: None,
        });

        assert_eq!(view.changes.len(), 2, "{:?}", view.changes);
        let (current, after) = (&view.changes[0], &view.changes[1]);
        assert_eq!(current.action, lookup("gui-fix-current"));
        assert_eq!(
            current.name,
            "Java 17.0.12, Eclipse Adoptium, amd64 (pinned by this host)"
        );
        assert_eq!(
            current.detail,
            "/opt/jdk-17/bin/java. This runtime is Java 17."
        );
        assert_eq!(after.action, lookup("gui-fix-proposed"));
        assert_eq!(after.detail, "/opt/jdk-21/bin/java");
        assert!(
            view.notes
                .contains(&"This host expects Java 21.".to_owned())
        );
    }
}
