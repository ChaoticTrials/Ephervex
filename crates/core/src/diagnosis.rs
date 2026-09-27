//! Whether a host is configured correctly for what it runs.
//!
//! A diagnosis reads what Ephervex already knows about a host: which Java
//! would run it and why, the JVM settings layered over it, what its mods'
//! recorded relations require, what the loader logged at its last start, its
//! newest crash, and, when asked, whether its recorded files are intact.
//!
//! Every finding is either a problem, which is objectively incompatible or
//! missing, or a valid customization, which differs from Ephervex's defaults
//! and still works. A customization is never reported as a problem.
//!
//! Nothing here changes a host. A finding may carry a fix, and a fix is only
//! applied through [`App::apply_fix`] after [`App::preview_fix`] has shown
//! exactly what it would change.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use ephervex_download::OperationReporter;
use ephervex_java::{JavaError, JavaPathChoice, JavaSourceKind, PinSource, RejectionReason};
use ephervex_model::LoaderSelection;
use ephervex_storage::DiagnosticKind;
use ephervex_storage::diagnostics::{MAX_VIEW_BYTES, TextAnchor};
use tokio_util::sync::CancellationToken;
use tracing::{info, instrument};

use crate::app::{App, JavaPlan, JavaPlanRequest, JavaTarget};
use crate::crash_analysis::{LoaderReportedDependency, loader_dependency_failures};
use crate::dependencies::{DeclaredConflict, UnmetDependency, UnmetRequirement};
use crate::diagnostics::{DiagnosticRequest, DiagnosticScope};
use crate::error::{CoreError, CoreResult};
use crate::mods::{InstalledHost, ModPresence};
use crate::verify::{
    Integrity, RepairRefusal, RepairReport, SharedArtifactReport, VerificationReport,
};

/// Whether a finding is something to fix or something to know.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingClass {
    /// Objectively incompatible, missing, or damaged.
    Problem,
    /// Differs from what Ephervex would choose, and still valid.
    Customized,
}

/// Which setting pinned a Java runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JavaLayer {
    /// This host's own Java override.
    Host,
    /// The launcher-wide preferred Java path.
    Launcher,
}

impl JavaLayer {
    const fn of(source: PinSource) -> Self {
        match source {
            PinSource::InstanceOverride | PinSource::ExplicitRequest => Self::Host,
            PinSource::GlobalPreference => Self::Launcher,
        }
    }

    /// A stable identifier for frontends and the technical report.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Launcher => "launcher",
        }
    }
}

/// Why a pinned runtime cannot run this host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JavaMismatch {
    Major { required: u32, found: u32 },
    TooOld { minimum: String, found: String },
    TooNew { maximum: String, found: String },
    Architecture { expected: String, found: String },
}

impl JavaMismatch {
    fn of(reason: &RejectionReason) -> Self {
        match reason {
            RejectionReason::MajorMismatch { required, found } => Self::Major {
                required: *required,
                found: *found,
            },
            RejectionReason::TooOld { minimum, found } => Self::TooOld {
                minimum: minimum.to_string(),
                found: found.to_string(),
            },
            RejectionReason::TooNew { maximum, found } => Self::TooNew {
                maximum: maximum.to_string(),
                found: found.to_string(),
            },
            RejectionReason::ArchitectureMismatch { expected, found } => Self::Architecture {
                expected: expected.clone(),
                found: found.clone(),
            },
        }
    }
}

impl std::fmt::Display for JavaMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Major { required, found } => write!(f, "needs Java {required}, found {found}"),
            Self::TooOld { minimum, found } => write!(f, "needs at least {minimum}, found {found}"),
            Self::TooNew { maximum, found } => write!(f, "needs at most {maximum}, found {found}"),
            Self::Architecture { expected, found } => write!(f, "needs {expected}, found {found}"),
        }
    }
}

/// Why no Java runtime would be chosen for this host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JavaUnavailable {
    /// Automatic detection is off and nothing is pinned.
    DetectionDisabled,
    /// Nothing installed qualifies and automatic download is off.
    NothingCompatible,
}

/// A runtime a diagnosis found or would choose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JavaRuntimeSummary {
    pub version: String,
    pub path: PathBuf,
    /// Ephervex downloaded and manages it.
    pub managed: bool,
}

impl JavaRuntimeSummary {
    fn of(plan: &JavaPlan) -> Self {
        Self {
            version: plan.runtime.version_string.clone(),
            path: plan.runtime.executable.clone(),
            managed: plan
                .runtime
                .origins
                .iter()
                .any(|origin| origin.kind == JavaSourceKind::Managed),
        }
    }
}

/// What automatic selection would run this host with, once nothing on the
/// host pins a runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecommendedJava {
    /// An installed runtime selection would choose.
    Installed(JavaRuntimeSummary),
    /// Nothing installed qualifies; Ephervex downloads this major at launch.
    ManagedDownload { major: u32 },
}

/// One thing a diagnosis found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FindingKind {
    /// The newest crash report was written after the last start.
    RecentCrash {
        report: String,
        /// The thrown type and its message, exact.
        failure: Option<String>,
        /// Mods the report named or placed in the failing stack.
        suspects: Vec<String>,
        client_code_on_server: bool,
    },
    /// Forge or NeoForge refused to load without this dependency.
    LoaderReportedDependency(LoaderReportedDependency),
    /// A pinned runtime cannot run this host.
    JavaIncompatible {
        layer: JavaLayer,
        path: PathBuf,
        required_major: u32,
        mismatch: JavaMismatch,
    },
    /// A pinned path is not a Java runtime that can be run.
    JavaUnusable {
        layer: JavaLayer,
        path: PathBuf,
    },
    JavaUnavailable {
        required_major: u32,
        reason: JavaUnavailable,
    },
    /// A required mod is turned off.
    DependencyDisabled(UnmetRequirement),
    /// A required mod is not installed.
    DependencyMissing(UnmetRequirement),
    DeclaredConflict(DeclaredConflict),
    /// Recorded files are absent or damaged.
    ManagedFilesDamaged {
        /// Can be fetched again from a recorded source.
        repairable: Vec<(PathBuf, Integrity)>,
        /// Nothing recorded says where to fetch them.
        unrepairable: Vec<(PathBuf, Integrity)>,
    },
    /// Shared launch files are present with the wrong bytes.
    SharedFilesDamaged {
        files: Vec<PathBuf>,
    },
    /// A pinned runtime that satisfies every requirement.
    JavaCustomized {
        layer: JavaLayer,
        runtime: JavaRuntimeSummary,
        required_major: u32,
    },
    JvmArgumentsCustomized {
        layer: JavaLayer,
        count: usize,
    },
    MemoryCustomized {
        min_mib: Option<u32>,
        max_mib: Option<u32>,
    },
    /// Jars in `mods/` that nothing recorded.
    HandPlacedMods {
        names: Vec<String>,
    },
}

impl FindingKind {
    #[must_use]
    pub const fn class(&self) -> FindingClass {
        match self {
            Self::RecentCrash { .. }
            | Self::LoaderReportedDependency(_)
            | Self::JavaIncompatible { .. }
            | Self::JavaUnusable { .. }
            | Self::JavaUnavailable { .. }
            | Self::DependencyDisabled(_)
            | Self::DependencyMissing(_)
            | Self::DeclaredConflict(_)
            | Self::ManagedFilesDamaged { .. }
            | Self::SharedFilesDamaged { .. } => FindingClass::Problem,
            Self::JavaCustomized { .. }
            | Self::JvmArgumentsCustomized { .. }
            | Self::MemoryCustomized { .. }
            | Self::HandPlacedMods { .. } => FindingClass::Customized,
        }
    }
}

/// A change Diagnostics can preview and then apply on request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiagnosticFix {
    /// Turn these mods on, dependencies first.
    EnableMods(Vec<PathBuf>),
    /// Remove this host's Java override so automatic selection applies.
    UseAutomaticJava,
    /// Fetch these recorded files again from their recorded sources.
    RestoreFiles(Vec<PathBuf>),
}

impl DiagnosticFix {
    /// Whether this fix changes which mods load, which an isolation session
    /// owns while it runs.
    const fn touches_mods(&self) -> bool {
        matches!(self, Self::EnableMods(_) | Self::RestoreFiles(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub kind: FindingKind,
    /// A safe change that addresses it, when one can be determined.
    pub fix: Option<DiagnosticFix>,
}

impl Finding {
    #[must_use]
    pub const fn class(&self) -> FindingClass {
        self.kind.class()
    }
}

/// How the Java check went.
#[derive(Debug)]
pub enum JavaCheck {
    /// This runtime would run the host.
    Selected {
        required_major: u32,
        runtime: JavaRuntimeSummary,
        /// The setting that pinned it, or `None` for automatic selection.
        layer: Option<JavaLayer>,
    },
    /// Nothing is installed yet; Ephervex downloads this major at launch.
    DownloadsAtLaunch { required_major: u32 },
    /// No runtime would be chosen. The findings say why.
    Refused { required_major: Option<u32> },
    /// The check itself could not run.
    Failed(Box<CoreError>),
}

impl JavaCheck {
    #[must_use]
    pub const fn required_major(&self) -> Option<u32> {
        match self {
            Self::Selected { required_major, .. } | Self::DownloadsAtLaunch { required_major } => {
                Some(*required_major)
            }
            Self::Refused { required_major } => *required_major,
            Self::Failed(_) => None,
        }
    }
}

/// What checking recorded files found, when a diagnosis was asked to.
#[derive(Debug)]
pub struct IntegrityCheck {
    pub files: VerificationReport,
    /// `None` for a server, whose shared launch files are not checked.
    pub shared: Option<Result<SharedArtifactReport, Box<CoreError>>>,
}

/// How many of a host's mods load, and how much is known about them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ModCounts {
    pub enabled: usize,
    pub disabled: usize,
    /// Enabled mods whose relations are recorded.
    pub answered: usize,
    /// Enabled mods nothing recorded relations for.
    pub unanswered: usize,
}

/// What one diagnosis of a host found.
#[derive(Debug)]
pub struct HostDiagnosis {
    pub host: InstalledHost,
    pub minecraft_version: String,
    pub loader: LoaderSelection,
    pub java: JavaCheck,
    pub mods: ModCounts,
    /// `None` when the diagnosis was not asked to check files.
    pub integrity: Option<IntegrityCheck>,
    /// Whether the last start's log could be read for loader refusals.
    pub loader_log_read: bool,
    /// The newest crash report, when it was written after the last start.
    pub recent_crash: Option<String>,
    /// Problems first, most likely to explain a failure first; then
    /// customizations.
    pub findings: Vec<Finding>,
}

impl HostDiagnosis {
    pub fn problems(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|finding| finding.class() == FindingClass::Problem)
    }

    pub fn customizations(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|finding| finding.class() == FindingClass::Customized)
    }

    /// Whether nothing is wrong. Customizations do not count against it.
    #[must_use]
    pub fn is_healthy(&self) -> bool {
        self.problems().next().is_none() && !matches!(self.java, JavaCheck::Failed(_))
    }
}

/// What a diagnosis should look at.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiagnosisRequest {
    /// Hash every recorded file. Real work on a large pack, so a pass run on
    /// its own after a crash leaves it out.
    pub integrity: bool,
}

/// One change a fix would make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProposedChange {
    Enable {
        name: String,
        logical_path: PathBuf,
    },
    UseJava {
        /// The pinned runtime, as far as it could be read.
        current_path: PathBuf,
        current_version: Option<String>,
        recommended: RecommendedJava,
        required_major: u32,
    },
    Restore {
        logical_path: PathBuf,
    },
}

/// What applying a fix would do, shown before anything changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixPreview {
    pub fix: DiagnosticFix,
    pub changes: Vec<ProposedChange>,
    /// What stops the fix from being applied right now.
    pub blocked: Option<RepairRefusal>,
}

impl FixPreview {
    /// Whether applying it would change anything.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }
}

/// What applying a fix did.
#[derive(Debug, Default)]
pub struct FixOutcome {
    /// What changed, by the name the preview showed.
    pub applied: Vec<String>,
    /// What was asked for and did not change, with why.
    pub failed: Vec<(String, CoreError)>,
    /// For a restore: the repair's own report.
    pub repair: Option<RepairReport>,
}

impl FixOutcome {
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.failed.is_empty() && self.repair.as_ref().is_none_or(RepairReport::is_complete)
    }
}

/// The pieces of a host record a diagnosis reads.
struct HostFacts {
    minecraft_version: String,
    loader: LoaderSelection,
    java_override: Option<PathBuf>,
    jvm_override: Option<ephervex_model::JvmSettings>,
    last_started: Option<DateTime<Utc>>,
}

impl App {
    /// Reads what a host is configured to run with and what is wrong with it.
    ///
    /// Changes nothing. Offline except for the Java requirement, which uses
    /// cached release metadata and falls back to the compatibility table.
    #[instrument(skip(self, reporter, cancel), fields(host = %host.id()))]
    pub async fn diagnose_host(
        &self,
        host: InstalledHost,
        request: DiagnosisRequest,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> CoreResult<HostDiagnosis> {
        let facts = self.host_facts(host)?;
        reporter.set_stage(Some("operation-stage-checking-java".to_owned()));
        let (java, java_findings) = self.check_java(host, &facts).await;
        let (java_problems, mut java_customizations): (Vec<Finding>, Vec<Finding>) = java_findings
            .into_iter()
            .partition(|finding| finding.class() == FindingClass::Problem);

        reporter.set_stage(Some("operation-stage-checking-mods".to_owned()));
        let snapshot = self.host_mod_snapshot(host)?;
        let requirements = self.requirement_problems(host)?;
        let mods = ModCounts {
            enabled: snapshot
                .mods
                .iter()
                .filter(|entry| entry.presence == ModPresence::Installed)
                .count(),
            disabled: snapshot
                .mods
                .iter()
                .filter(|entry| entry.presence == ModPresence::Disabled)
                .count(),
            answered: requirements.answered,
            unanswered: requirements.unanswered,
        };

        let mut findings = Vec::new();
        let recent_crash = self.recent_crash(host, facts.last_started, &mut findings);
        let log = self.start_log(host);
        if let Some(log) = log.as_deref() {
            findings.extend(
                loader_dependency_failures(log)
                    .into_iter()
                    .map(|reported| Finding {
                        kind: FindingKind::LoaderReportedDependency(reported),
                        fix: None,
                    }),
            );
        }
        findings.extend(java_problems);
        for unmet in requirements.unmet {
            findings.push(match &unmet.dependency {
                UnmetDependency::Disabled(disabled) => Finding {
                    fix: Some(DiagnosticFix::EnableMods(
                        self.enable_order(host, &disabled.logical_path),
                    )),
                    kind: FindingKind::DependencyDisabled(unmet),
                },
                UnmetDependency::Missing { .. } => Finding {
                    kind: FindingKind::DependencyMissing(unmet),
                    fix: None,
                },
            });
        }
        findings.extend(requirements.conflicts.into_iter().map(|conflict| Finding {
            kind: FindingKind::DeclaredConflict(conflict),
            fix: None,
        }));

        let integrity = if request.integrity {
            let check = self.check_integrity(host, reporter, cancel).await?;
            findings.extend(integrity_findings(&check));
            Some(check)
        } else {
            None
        };

        findings.append(&mut java_customizations);
        findings.extend(jvm_findings(self, facts.jvm_override.as_ref()));
        let hand_placed: Vec<String> = snapshot
            .mods
            .iter()
            .filter(|entry| {
                entry.origin == ephervex_model::ModOrigin::HandPlaced
                    && entry.presence == ModPresence::Installed
            })
            .map(|entry| entry.file_name.clone())
            .collect();
        if !hand_placed.is_empty() {
            findings.push(Finding {
                kind: FindingKind::HandPlacedMods { names: hand_placed },
                fix: None,
            });
        }

        let diagnosis = HostDiagnosis {
            host,
            minecraft_version: facts.minecraft_version,
            loader: facts.loader,
            java,
            mods,
            integrity,
            loader_log_read: log.is_some(),
            recent_crash,
            findings,
        };
        info!(
            problems = diagnosis.problems().count(),
            customizations = diagnosis.customizations().count(),
            "diagnosed a host's configuration"
        );
        Ok(diagnosis)
    }

    /// Shows what a fix would change. Changes nothing.
    pub async fn preview_fix(
        &self,
        host: InstalledHost,
        fix: &DiagnosticFix,
    ) -> CoreResult<FixPreview> {
        let changes = match fix {
            DiagnosticFix::EnableMods(paths) => {
                let snapshot = self.host_mod_snapshot(host)?;
                paths
                    .iter()
                    .filter_map(|path| {
                        snapshot
                            .mods
                            .iter()
                            .find(|entry| {
                                &entry.logical_path == path
                                    && entry.presence == ModPresence::Disabled
                            })
                            .map(|entry| ProposedChange::Enable {
                                name: entry.display_name().to_owned(),
                                logical_path: entry.logical_path.clone(),
                            })
                    })
                    .collect()
            }
            DiagnosticFix::UseAutomaticJava => {
                let facts = self.host_facts(host)?;
                let Some(current_path) = facts.java_override.clone() else {
                    return Ok(FixPreview {
                        fix: fix.clone(),
                        changes: Vec::new(),
                        blocked: None,
                    });
                };
                let (required_major, recommended) = self.recommended_java(host, &facts).await?;
                let current_version = self
                    .test_java_runtime(&current_path)
                    .await
                    .ok()
                    .map(|runtime| runtime.version_string);
                vec![ProposedChange::UseJava {
                    current_path,
                    current_version,
                    recommended,
                    required_major,
                }]
            }
            DiagnosticFix::RestoreFiles(paths) => paths
                .iter()
                .map(|path| ProposedChange::Restore {
                    logical_path: path.clone(),
                })
                .collect(),
        };
        Ok(FixPreview {
            fix: fix.clone(),
            changes,
            blocked: self.change_blocker(host, fix.touches_mods())?,
        })
    }

    /// Applies a fix a preview showed. Reports each change on its own, so a
    /// fix that partly failed says exactly what did and did not change.
    #[instrument(skip(self, reporter, cancel), fields(host = %host.id()))]
    pub async fn apply_fix(
        &self,
        host: InstalledHost,
        fix: &DiagnosticFix,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> CoreResult<FixOutcome> {
        if let Some(reason) = self.change_blocker(host, fix.touches_mods())? {
            return Err(CoreError::ChangeRefused { reason });
        }
        let mut outcome = FixOutcome::default();
        match fix {
            DiagnosticFix::EnableMods(paths) => {
                let snapshot = self.host_mod_snapshot(host)?;
                for path in paths {
                    if cancel.is_cancelled() {
                        break;
                    }
                    let name = snapshot
                        .mods
                        .iter()
                        .find(|entry| &entry.logical_path == path)
                        .map_or_else(
                            || path.display().to_string(),
                            |entry| entry.display_name().to_owned(),
                        );
                    match self.set_mod_enabled(host, path, true) {
                        Ok(_) => outcome.applied.push(name),
                        Err(error) => outcome.failed.push((name, error)),
                    }
                }
            }
            DiagnosticFix::UseAutomaticJava => {
                let facts = self.host_facts(host)?;
                let result = match host {
                    InstalledHost::Instance(id) => self
                        .set_client_runtime_settings(id, None, facts.jvm_override)
                        .map(drop),
                    InstalledHost::Server(id) => self
                        .set_server_runtime_settings(id, None, facts.jvm_override)
                        .map(drop),
                };
                let name = facts
                    .java_override
                    .map_or_else(String::new, |path| path.display().to_string());
                match result {
                    Ok(()) => outcome.applied.push(name),
                    Err(error) => outcome.failed.push((name, error)),
                }
            }
            DiagnosticFix::RestoreFiles(_) => {
                let report = self.repair_host(host, reporter, cancel).await?;
                outcome.applied = report
                    .repaired
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect();
                outcome.repair = Some(report);
            }
        }
        info!(
            applied = outcome.applied.len(),
            failed = outcome.failed.len(),
            "applied a diagnostic fix"
        );
        Ok(outcome)
    }

    fn host_facts(&self, host: InstalledHost) -> CoreResult<HostFacts> {
        Ok(match host {
            InstalledHost::Instance(id) => {
                let record = self.client(id)?;
                HostFacts {
                    minecraft_version: record.minecraft_version,
                    loader: record.loader,
                    java_override: record.java_override.map(|java| java.path),
                    jvm_override: record.jvm_override,
                    last_started: record.last_launched_at,
                }
            }
            InstalledHost::Server(id) => {
                let record = self.server(id)?;
                HostFacts {
                    minecraft_version: record.minecraft_version,
                    loader: record.loader,
                    java_override: record.java_override.map(|java| java.path),
                    jvm_override: record.jvm_override,
                    last_started: record.last_started_at,
                }
            }
        })
    }

    /// What stops a fix from changing this host now.
    fn change_blocker(
        &self,
        host: InstalledHost,
        touches_mods: bool,
    ) -> CoreResult<Option<RepairRefusal>> {
        Ok(match self.mutation_blocker(host)? {
            Some(RepairRefusal::IsolationActive) if !touches_mods => None,
            other => other,
        })
    }

    async fn check_java(
        &self,
        host: InstalledHost,
        facts: &HostFacts,
    ) -> (JavaCheck, Vec<Finding>) {
        let pinned_layer = if facts.java_override.is_some() {
            Some(JavaLayer::Host)
        } else if self.java_settings().preferred_path.is_some() {
            Some(JavaLayer::Launcher)
        } else {
            None
        };
        let plan = self
            .plan_java(
                &facts.minecraft_version,
                &JavaPlanRequest {
                    target: java_target(host),
                    explicit_path: None,
                    install_if_missing: false,
                },
            )
            .await;
        // Removing this host's own pin is only offered when doing so leads
        // somewhere that works.
        let use_automatic = (pinned_layer == Some(JavaLayer::Host)
            && self.recommended_java(host, facts).await.is_ok())
        .then_some(DiagnosticFix::UseAutomaticJava);
        let plan = match plan {
            Ok(plan) => plan,
            Err(error) => {
                let required_major = self.required_major(&facts.minecraft_version).await;
                return java_refusal(
                    error,
                    pinned_layer,
                    required_major,
                    use_automatic,
                    self.java_settings().auto_download,
                );
            }
        };
        let runtime = JavaRuntimeSummary::of(&plan);
        let required_major = plan.required.major;
        let layer = match plan.choice {
            JavaPathChoice::Pinned { source, .. } => Some(JavaLayer::of(source)),
            JavaPathChoice::Automatic => None,
        };
        let findings = layer
            .map(|layer| Finding {
                kind: FindingKind::JavaCustomized {
                    layer,
                    runtime: runtime.clone(),
                    required_major,
                },
                fix: use_automatic,
            })
            .into_iter()
            .collect();
        (
            JavaCheck::Selected {
                required_major,
                runtime,
                layer,
            },
            findings,
        )
    }

    /// The Java major a release needs, falling back to the table offline.
    async fn required_major(&self, minecraft_version: &str) -> u32 {
        match self.required_java(minecraft_version).await {
            Ok(required) => required.major,
            Err(_) => {
                ephervex_java::RequiredJava::from_table(
                    &ephervex_minecraft::MinecraftVersionId::parse(minecraft_version),
                )
                .major
            }
        }
    }

    /// What automatic selection would choose once this host pins nothing.
    async fn recommended_java(
        &self,
        host: InstalledHost,
        facts: &HostFacts,
    ) -> CoreResult<(u32, RecommendedJava)> {
        let required = self.required_java(&facts.minecraft_version).await?;
        let major = required.major;
        let planned = self
            .plan_java_for_selection(
                &facts.minecraft_version,
                required,
                &facts.loader,
                None,
                facts.jvm_override.clone(),
                &JavaPlanRequest {
                    target: java_target(host),
                    explicit_path: None,
                    install_if_missing: false,
                },
            )
            .await;
        match planned {
            // A launcher-wide pin that works is what removing this host's own
            // override leads to, and it is a real recommendation.
            Ok(plan) => Ok((
                major,
                RecommendedJava::Installed(JavaRuntimeSummary::of(&plan)),
            )),
            Err(CoreError::Java(JavaError::NoCompatibleRuntime { .. }))
                if self.java_settings().auto_download =>
            {
                Ok((major, RecommendedJava::ManagedDownload { major }))
            }
            Err(error) => Err(error),
        }
    }

    /// The newest crash report, when it was written after the last start.
    fn recent_crash(
        &self,
        host: InstalledHost,
        last_started: Option<DateTime<Utc>>,
        findings: &mut Vec<Finding>,
    ) -> Option<String> {
        let review = self.crash_review(host).ok().flatten()?;
        let after_start = match (review.reported_at, last_started) {
            (Some(reported), Some(started)) => reported >= started,
            (_, None) => true,
            (None, Some(_)) => false,
        };
        if !after_start {
            return None;
        }
        let mut suspects: Vec<String> = Vec::new();
        for suspect in review.analysis.stated() {
            let label = suspect.label().to_owned();
            if !suspects.contains(&label) {
                suspects.push(label);
            }
        }
        findings.push(Finding {
            kind: FindingKind::RecentCrash {
                report: review.report_name.clone(),
                failure: review.analysis.failure.clone(),
                suspects,
                client_code_on_server: review.analysis.client_code_on_server,
            },
            fix: None,
        });
        Some(review.report_name)
    }

    /// The last start's log, from its beginning, where loader refusals are.
    fn start_log(&self, host: InstalledHost) -> Option<String> {
        self.read_diagnostic(
            &DiagnosticRequest::new(diagnostic_scope(host), DiagnosticKind::Log, "latest.log")
                .with_anchor(TextAnchor::Head)
                .with_budget(MAX_VIEW_BYTES),
        )
        .ok()
        .map(|view| view.text)
    }

    /// The mod at `path` and every turned-off mod it requires, requirements
    /// first so nothing is turned on before what it needs.
    fn enable_order(&self, host: InstalledHost, path: &Path) -> Vec<PathBuf> {
        let mut order = vec![path.to_path_buf()];
        if let Ok(required) = self.dependencies_of(host, path) {
            let mut disabled = required.disabled;
            disabled.sort_by_key(|entry| entry.distance);
            order.extend(disabled.into_iter().map(|entry| entry.logical_path));
        }
        order.dedup();
        order.reverse();
        order
    }

    async fn check_integrity(
        &self,
        host: InstalledHost,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> CoreResult<IntegrityCheck> {
        reporter.set_stage(Some("operation-verifying-host".to_owned()));
        let files = self.verify_host(host, reporter, cancel).await?;
        let shared = match host {
            InstalledHost::Instance(instance) => {
                reporter.set_stage(Some("operation-stage-checking-shared-files".to_owned()));
                Some(
                    self.verify_shared_artifacts(instance)
                        .await
                        .map_err(Box::new),
                )
            }
            InstalledHost::Server(_) => None,
        };
        Ok(IntegrityCheck { files, shared })
    }
}

/// What a Java plan that failed says about the host.
fn java_refusal(
    error: CoreError,
    pinned_layer: Option<JavaLayer>,
    required_major: u32,
    use_automatic: Option<DiagnosticFix>,
    auto_download: bool,
) -> (JavaCheck, Vec<Finding>) {
    let refused = |kind: FindingKind, fix: Option<DiagnosticFix>| {
        (
            JavaCheck::Refused {
                required_major: Some(required_major),
            },
            vec![Finding { kind, fix }],
        )
    };
    match error {
        CoreError::Java(JavaError::PinnedRuntimeIncompatible {
            path,
            pinned_by,
            rejection,
        }) => {
            let layer = JavaLayer::of(pinned_by);
            refused(
                FindingKind::JavaIncompatible {
                    layer,
                    path,
                    required_major,
                    mismatch: JavaMismatch::of(&rejection.reason),
                },
                use_automatic.filter(|_| layer == JavaLayer::Host),
            )
        }
        CoreError::Java(JavaError::RuntimeUnusable { path, .. }) => {
            let layer = pinned_layer.unwrap_or(JavaLayer::Host);
            refused(
                FindingKind::JavaUnusable { layer, path },
                use_automatic.filter(|_| layer == JavaLayer::Host),
            )
        }
        CoreError::Java(JavaError::AutoDetectDisabled { required_major }) => refused(
            FindingKind::JavaUnavailable {
                required_major,
                reason: JavaUnavailable::DetectionDisabled,
            },
            None,
        ),
        CoreError::Java(JavaError::NoCompatibleRuntime { required_major, .. }) if auto_download => {
            (JavaCheck::DownloadsAtLaunch { required_major }, Vec::new())
        }
        CoreError::Java(JavaError::NoCompatibleRuntime { required_major, .. }) => refused(
            FindingKind::JavaUnavailable {
                required_major,
                reason: JavaUnavailable::NothingCompatible,
            },
            None,
        ),
        error => (JavaCheck::Failed(Box::new(error)), Vec::new()),
    }
}

fn integrity_findings(check: &IntegrityCheck) -> Vec<Finding> {
    let mut findings = Vec::new();
    let listed = |files: Vec<&crate::verify::VerifiedFile>| -> Vec<(PathBuf, Integrity)> {
        files
            .into_iter()
            .map(|file| (file.logical_path.clone(), file.integrity.clone()))
            .collect()
    };
    let repairable = listed(check.files.repairable().collect());
    let unrepairable = listed(check.files.unrepairable().collect());
    if !repairable.is_empty() || !unrepairable.is_empty() {
        findings.push(Finding {
            fix: (!repairable.is_empty()).then(|| {
                DiagnosticFix::RestoreFiles(
                    repairable.iter().map(|(path, _)| path.clone()).collect(),
                )
            }),
            kind: FindingKind::ManagedFilesDamaged {
                repairable,
                unrepairable,
            },
        });
    }
    if let Some(Ok(shared)) = &check.shared {
        let files: Vec<PathBuf> = shared
            .faults()
            .map(|file| file.logical_path.clone())
            .collect();
        if !files.is_empty() {
            findings.push(Finding {
                kind: FindingKind::SharedFilesDamaged { files },
                fix: None,
            });
        }
    }
    findings
}

fn jvm_findings(app: &App, host_override: Option<&ephervex_model::JvmSettings>) -> Vec<Finding> {
    let mut findings = Vec::new();
    let host_args = host_override.map_or(0, |settings| settings.jvm_args.len());
    let launcher_args = app.java_settings().jvm_args.len();
    if host_args > 0 {
        findings.push(Finding {
            kind: FindingKind::JvmArgumentsCustomized {
                layer: JavaLayer::Host,
                count: host_args,
            },
            fix: None,
        });
    } else if launcher_args > 0 {
        findings.push(Finding {
            kind: FindingKind::JvmArgumentsCustomized {
                layer: JavaLayer::Launcher,
                count: launcher_args,
            },
            fix: None,
        });
    }
    if let Some(settings) = host_override
        && (settings.min_memory_mib.is_some() || settings.max_memory_mib.is_some())
    {
        findings.push(Finding {
            kind: FindingKind::MemoryCustomized {
                min_mib: settings.min_memory_mib,
                max_mib: settings.max_memory_mib,
            },
            fix: None,
        });
    }
    findings
}

const fn java_target(host: InstalledHost) -> JavaTarget {
    match host {
        InstalledHost::Instance(id) => JavaTarget::Client(id),
        InstalledHost::Server(id) => JavaTarget::Server(id),
    }
}

const fn diagnostic_scope(host: InstalledHost) -> DiagnosticScope {
    match host {
        InstalledHost::Instance(id) => DiagnosticScope::Client(id),
        InstalledHost::Server(id) => DiagnosticScope::Server(id),
    }
}

impl HostDiagnosis {
    /// A plain-text summary to paste into a bug report or support thread.
    ///
    /// Paths under the user's home directory are written from `~`, and JVM
    /// argument values are left out, since either can carry something private.
    #[must_use]
    pub fn technical_report(&self) -> String {
        self.technical_report_with_home(std::env::home_dir().as_deref())
    }

    pub(crate) fn technical_report_with_home(&self, home: Option<&Path>) -> String {
        use std::fmt::Write as _;

        let path = |path: &Path| redact_home(path, home);
        let mut out = String::from("Ephervex diagnostic report\n\n");
        let _ = writeln!(out, "Ephervex: {}", ephervex_model::APP_VERSION);
        let _ = writeln!(out, "Host: {}", self.host.kind());
        let _ = writeln!(out, "Minecraft: {}", self.minecraft_version);
        let _ = writeln!(out, "Loader: {}", self.loader);
        write_java(&mut out, &self.java, &path, home);
        let problems = self.problems().count();
        let customizations = self.customizations().count();
        let _ = writeln!(
            out,
            "Result: {}",
            if problems == 0 {
                "no problems found".to_owned()
            } else {
                format!("{problems} problem(s), {customizations} customization(s)")
            }
        );

        for (title, class) in [
            ("Problems", FindingClass::Problem),
            ("Valid, customized", FindingClass::Customized),
        ] {
            let lines: Vec<String> = self
                .findings
                .iter()
                .filter(|finding| finding.class() == class)
                .map(|finding| report_line(&finding.kind, &path))
                .collect();
            if lines.is_empty() {
                continue;
            }
            let _ = write!(out, "\n{title}:\n");
            for line in lines {
                let _ = writeln!(out, "- {line}");
            }
        }

        let _ = write!(
            out,
            "\nMods: {} enabled, {} turned off; requirements recorded for {}, unknown for {}\n",
            self.mods.enabled, self.mods.disabled, self.mods.answered, self.mods.unanswered
        );
        out.push_str("\nIntegrity:\n");
        write_integrity(&mut out, self.integrity.as_ref(), home);
        out.push_str("\nRecent run:\n");
        match &self.recent_crash {
            Some(report) => {
                let _ = writeln!(
                    out,
                    "- Crashed after its last start; crash report: {report}"
                );
            }
            None => out.push_str("- No crash report since its last start\n"),
        }
        if !self.loader_log_read {
            out.push_str("- latest.log could not be read\n");
        }
        out
    }
}

fn write_java(
    out: &mut String,
    java: &JavaCheck,
    path: &dyn Fn(&Path) -> String,
    home: Option<&Path>,
) {
    use std::fmt::Write as _;

    match java {
        JavaCheck::Selected {
            required_major,
            runtime,
            layer,
        } => {
            let _ = writeln!(*out, "Java required: {required_major}");
            let source = match (layer, runtime.managed) {
                (Some(JavaLayer::Host), _) => "host override",
                (Some(JavaLayer::Launcher), _) => "launcher preference",
                (None, true) => "automatic, managed",
                (None, false) => "automatic",
            };
            let _ = writeln!(
                *out,
                "Java selected: {} [{source}] {}",
                runtime.version,
                path(&runtime.path)
            );
        }
        JavaCheck::DownloadsAtLaunch { required_major } => {
            let _ = writeln!(*out, "Java required: {required_major}");
            let _ = writeln!(
                *out,
                "Java selected: managed Java {required_major}, downloaded at launch"
            );
        }
        JavaCheck::Refused { required_major } => {
            if let Some(major) = required_major {
                let _ = writeln!(*out, "Java required: {major}");
            }
            out.push_str("Java selected: none usable\n");
        }
        JavaCheck::Failed(error) => {
            let _ = writeln!(
                *out,
                "Java: not checked ({})",
                redact_text(&error.to_string(), home)
            );
        }
    }
}

fn write_integrity(out: &mut String, integrity: Option<&IntegrityCheck>, home: Option<&Path>) {
    use std::fmt::Write as _;

    match integrity {
        None => out.push_str("- Recorded files not checked\n"),
        Some(check) => {
            let files = &check.files;
            let _ = writeln!(
                *out,
                "- {} recorded files: {} intact, {} missing, {} damaged, {} unreadable, {} without a checksum",
                files.files.len(),
                files.count(&Integrity::Intact),
                files.count(&Integrity::Absent),
                files.count(&Integrity::Corrupt),
                files.count(&Integrity::Unreadable),
                files.count(&Integrity::Unverifiable),
            );
            match &check.shared {
                None => {}
                Some(Ok(shared)) if !shared.metadata_cached => {
                    out.push_str("- Shared launch files: not checked, release not launched yet\n");
                }
                Some(Ok(shared)) => {
                    let _ = writeln!(
                        *out,
                        "- Shared launch files: {} checked, {} damaged, {} not downloaded yet",
                        shared.files.len(),
                        shared.faults().count(),
                        shared.not_yet_fetched().count()
                    );
                }
                Some(Err(error)) => {
                    let _ = writeln!(
                        *out,
                        "- Shared launch files: not checked ({})",
                        redact_text(&error.to_string(), home)
                    );
                }
            }
        }
    }
}

fn report_line(kind: &FindingKind, path: &dyn Fn(&Path) -> String) -> String {
    use std::fmt::Write as _;

    let chain = |requirement: &UnmetRequirement| {
        requirement
            .chain
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>()
            .join(" -> ")
    };
    match kind {
        FindingKind::RecentCrash {
            report,
            failure,
            suspects,
            client_code_on_server,
        } => {
            let mut line = format!(
                "Crashed: {} ({report})",
                failure.as_deref().unwrap_or("no failure stated")
            );
            if !suspects.is_empty() {
                let _ = write!(line, "; named: {}", suspects.join(", "));
            }
            if *client_code_on_server {
                line.push_str("; client-only code ran on a server");
            }
            line
        }
        FindingKind::LoaderReportedDependency(reported) => format!(
            "Loader refused to start: {} requires {} {}, found {}",
            reported.requested_by,
            reported.mod_id,
            reported.expected_range,
            reported.actual.as_deref().unwrap_or("nothing")
        ),
        FindingKind::JavaIncompatible { .. }
        | FindingKind::JavaUnusable { .. }
        | FindingKind::JavaUnavailable { .. } => java_problem_line(kind, path),
        FindingKind::DependencyDisabled(requirement) => {
            let UnmetDependency::Disabled(disabled) = &requirement.dependency else {
                return String::new();
            };
            format!(
                "{} is turned off; required by {}",
                disabled.name,
                chain(requirement)
            )
        }
        FindingKind::DependencyMissing(requirement) => {
            let UnmetDependency::Missing { name, .. } = &requirement.dependency else {
                return String::new();
            };
            format!(
                "{name} is not installed; required by {}",
                chain(requirement)
            )
        }
        FindingKind::DeclaredConflict(conflict) => format!(
            "{} declares it does not work with {}",
            conflict.declared_by.name, conflict.conflicts_with.name
        ),
        FindingKind::ManagedFilesDamaged {
            repairable,
            unrepairable,
        } => {
            let names: Vec<String> = repairable
                .iter()
                .chain(unrepairable)
                .map(|(file, integrity)| format!("{} ({})", file.display(), integrity.as_str()))
                .collect();
            format!(
                "Recorded files missing or damaged ({} can be restored): {}",
                repairable.len(),
                names.join(", ")
            )
        }
        FindingKind::SharedFilesDamaged { files } => format!(
            "Shared launch files damaged: {}",
            files
                .iter()
                .map(|file| file.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        FindingKind::JavaCustomized { .. }
        | FindingKind::JvmArgumentsCustomized { .. }
        | FindingKind::MemoryCustomized { .. }
        | FindingKind::HandPlacedMods { .. } => customization_line(kind, path),
    }
}

fn customization_line(kind: &FindingKind, path: &dyn Fn(&Path) -> String) -> String {
    match kind {
        FindingKind::JavaCustomized {
            layer: pinned,
            runtime,
            ..
        } => format!(
            "Java {} ({}): {}",
            runtime.version,
            layer_label(*pinned),
            path(&runtime.path)
        ),
        FindingKind::JvmArgumentsCustomized {
            layer: pinned,
            count,
        } => format!(
            "{count} JVM argument(s) ({}); values omitted",
            layer_label(*pinned)
        ),
        FindingKind::MemoryCustomized { min_mib, max_mib } => format!(
            "Memory override: min {}, max {}",
            min_mib.map_or_else(|| "default".to_owned(), |mib| format!("{mib} MiB")),
            max_mib.map_or_else(|| "default".to_owned(), |mib| format!("{mib} MiB"))
        ),
        FindingKind::HandPlacedMods { names } => {
            format!("Hand-placed mods: {}", names.join(", "))
        }
        _ => String::new(),
    }
}

fn java_problem_line(kind: &FindingKind, path: &dyn Fn(&Path) -> String) -> String {
    match kind {
        FindingKind::JavaIncompatible {
            layer: pinned,
            path: runtime,
            mismatch,
            ..
        } => format!(
            "Incompatible Java ({}): {}, {mismatch}",
            layer_label(*pinned),
            path(runtime)
        ),
        FindingKind::JavaUnusable {
            layer: pinned,
            path: runtime,
        } => format!(
            "Java path ({}) is not a usable runtime: {}",
            layer_label(*pinned),
            path(runtime)
        ),
        FindingKind::JavaUnavailable {
            required_major,
            reason,
        } => match reason {
            JavaUnavailable::DetectionDisabled => {
                format!("No Java {required_major}: detection is off and no path is set")
            }
            JavaUnavailable::NothingCompatible => {
                format!("No Java {required_major} installed, and automatic download is off")
            }
        },
        _ => String::new(),
    }
}

const fn layer_label(layer: JavaLayer) -> &'static str {
    match layer {
        JavaLayer::Host => "host override",
        JavaLayer::Launcher => "launcher preference",
    }
}

/// Free text with the home directory written as `~`.
fn redact_text(text: &str, home: Option<&Path>) -> String {
    match home.map(|home| home.display().to_string()) {
        Some(home) if !home.is_empty() && home != "/" => text.replace(&home, "~"),
        _ => text.to_owned(),
    }
}

/// A path as the report prints it, from `~` when it is under `home`.
fn redact_home(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_paths_under_home_start_from_a_tilde() {
        let home = Path::new("/home/someone");
        assert_eq!(
            redact_home(Path::new("/home/someone/.jdks/21/bin/java"), Some(home)),
            "~/.jdks/21/bin/java"
        );
        assert_eq!(
            redact_home(Path::new("/usr/lib/jvm/java-21/bin/java"), Some(home)),
            "/usr/lib/jvm/java-21/bin/java"
        );
        assert_eq!(
            redact_home(Path::new("/home/someone-else/java"), Some(home)),
            "/home/someone-else/java",
            "a sibling that shares the prefix is not under home"
        );
    }
}
