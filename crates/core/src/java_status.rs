//! Which Java a host would run with, and whether it fits.
//!
//! [`App::host_java_status`] answers both questions for one client or server
//! before it starts. It runs the same Java classification
//! [`App::diagnose_host`] reports, through the same rule evaluation launch
//! applies, and reads nothing else: no logs, mods, crash reports or files. A
//! status and a diagnosis of the same host therefore never disagree about its
//! Java.
//!
//! The status explains and changes nothing. A change to which runtime a host
//! uses is a [`DiagnosticFix`] the user previews and applies (ADR 0043).

use std::path::PathBuf;

use ephervex_java::ExpectedJava;
use ephervex_model::LoaderKind;
use tracing::instrument;

use crate::app::App;
use crate::diagnosis::{
    DiagnosticFix, Finding, FindingClass, JavaCheck, JavaLayer, JavaMismatch, JavaRuntimeSummary,
    JavaUnavailable,
};
use crate::error::CoreResult;
use crate::mods::InstalledHost;

/// How a host's runtime is chosen.
///
/// Separate from whether the runtime itself is Ephervex-managed: automatic
/// selection can pick a system runtime, and a pin can name a managed one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JavaSelectionMode {
    /// Ephervex picks the best compatible runtime.
    Automatic,
    /// This host's own Java override names the runtime.
    PinnedByHost,
    /// The launcher-wide preferred Java path names the runtime, because this
    /// host has no override of its own.
    PinnedByLauncher,
}

impl JavaSelectionMode {
    pub(crate) const fn of_layer(layer: Option<JavaLayer>) -> Self {
        match layer {
            None => Self::Automatic,
            Some(JavaLayer::Host) => Self::PinnedByHost,
            Some(JavaLayer::Launcher) => Self::PinnedByLauncher,
        }
    }

    /// A stable identifier for frontends and `--json`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::PinnedByHost => "host",
            Self::PinnedByLauncher => "launcher",
        }
    }
}

/// The Java a host's Minecraft and loader combination expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpectedRuntime {
    /// The required major and any update bounds the loader adds.
    pub java: ExpectedJava,
    /// The loader whose rules supplied the bounds.
    pub loader: LoaderKind,
}

impl ExpectedRuntime {
    #[must_use]
    pub const fn major(&self) -> u32 {
        self.java.major
    }

    /// The loader that narrowed the requirement beyond its major, if any.
    #[must_use]
    pub const fn bounded_by(&self) -> Option<LoaderKind> {
        if self.java.minimum.is_some() || self.java.maximum.is_some() {
            Some(self.loader)
        } else {
            None
        }
    }
}

/// What one Java selection for a host resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedJava {
    /// This runtime, and why the host refuses it when it does.
    Runtime {
        runtime: JavaRuntimeSummary,
        rejection: Option<JavaMismatch>,
    },
    /// A pinned path that is not a runtime that can be run.
    Unusable { path: PathBuf },
    /// Nothing installed qualifies; Ephervex downloads this major at launch.
    DownloadsAtLaunch { major: u32 },
    /// No runtime would be chosen.
    Unavailable(JavaUnavailable),
}

impl ResolvedJava {
    /// Whether a host with this selection would launch on a fitting runtime.
    #[must_use]
    pub const fn is_usable(&self) -> bool {
        matches!(
            self,
            Self::Runtime {
                rejection: None,
                ..
            } | Self::DownloadsAtLaunch { .. }
        )
    }
}

/// A host's Java selection and what it resolves to, one side of a previewed
/// change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JavaSelectionState {
    pub mode: JavaSelectionMode,
    pub resolved: ResolvedJava,
}

/// The state a host's Java is in, in Diagnostics' terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JavaStatusTone {
    /// Automatic selection resolves, or downloads a runtime at launch.
    Healthy,
    /// A pin that satisfies every rule, even if automatic selection would
    /// choose another runtime.
    Customized,
    /// No usable runtime would be chosen.
    Problem,
    /// The check itself could not run.
    Unchecked,
}

impl JavaStatusTone {
    /// A stable identifier for frontends and `--json`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Customized => "customized",
            Self::Problem => "problem",
            Self::Unchecked => "unchecked",
        }
    }
}

/// A managed runtime Ephervex can install for a host on request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JavaInstallOffer {
    pub major: u32,
    /// The newest update the loader accepts, when it caps one.
    pub maximum_update: Option<u32>,
}

/// Which Java a host would run with, whether it fits, and what can be done.
#[derive(Debug)]
pub struct HostJavaStatus {
    pub host: InstalledHost,
    pub expected: ExpectedRuntime,
    pub mode: JavaSelectionMode,
    /// The path a pin names, exactly as stored.
    pub pinned_path: Option<PathBuf>,
    pub check: JavaCheck,
    /// The Java findings a diagnosis of this host reports, problems first.
    pub findings: Vec<Finding>,
    /// Offered when no installed runtime satisfies the host.
    pub install: Option<JavaInstallOffer>,
}

impl HostJavaStatus {
    #[must_use]
    pub fn tone(&self) -> JavaStatusTone {
        if matches!(self.check, JavaCheck::Failed(_)) {
            return JavaStatusTone::Unchecked;
        }
        if self
            .findings
            .iter()
            .any(|finding| finding.class() == FindingClass::Problem)
        {
            return JavaStatusTone::Problem;
        }
        if self
            .findings
            .iter()
            .any(|finding| finding.class() == FindingClass::Customized)
        {
            return JavaStatusTone::Customized;
        }
        JavaStatusTone::Healthy
    }

    /// The runtime the host would run with, or the pinned one it refuses.
    #[must_use]
    pub const fn runtime(&self) -> Option<&JavaRuntimeSummary> {
        match &self.check {
            JavaCheck::Selected { runtime, .. } => Some(runtime),
            JavaCheck::Refused { attempted, .. } => attempted.as_ref(),
            JavaCheck::DownloadsAtLaunch { .. } | JavaCheck::Failed(_) => None,
        }
    }

    /// The previewable change a finding offers, when one does.
    #[must_use]
    pub fn fix(&self) -> Option<&DiagnosticFix> {
        self.findings
            .iter()
            .find_map(|finding| finding.fix.as_ref())
    }
}

impl App {
    /// Which Java a host would run with, what it expects, and whether the two
    /// fit, without reading anything a full diagnosis reads beyond that.
    ///
    /// Probes a pinned runtime that the cached inventory does not already
    /// know, so call it off a frontend's UI thread.
    #[instrument(skip(self), fields(host = %host.id()))]
    pub async fn host_java_status(&self, host: InstalledHost) -> CoreResult<HostJavaStatus> {
        let facts = self.host_facts(host)?;
        self.assess_java(host, &facts).await
    }
}
