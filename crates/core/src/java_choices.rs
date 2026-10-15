//! The runtimes a host can be switched to, and the hosts that pin a runtime.
//!
//! [`App::java_choices`] lists every runtime the inventory knows with the
//! verdict the host's rules give it, through the same evaluation selection and
//! the pinned-runtime check apply, so a choice shown as compatible is one a
//! launch accepts. Choosing one is a previewed change, never applied here.

use std::path::{Path, PathBuf};

use ephervex_java::{
    ExpectedJava, JavaCompatibilityRule, JavaRuntime, PinSource, RequiredJava, RuleVerdict,
    VersionBound,
};
use ephervex_model::LoaderKind;
use tracing::instrument;

use crate::app::{App, PinProbe};
use crate::diagnosis::{HostFacts, JavaLayer, JavaMismatch, JavaRuntimeSummary, JavaUnavailable};
use crate::error::CoreResult;
use crate::java_status::{ExpectedRuntime, JavaSelectionMode};
use crate::mods::InstalledHost;

/// What automatic selection resolves a host to right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutomaticJava {
    /// The installed runtime selection would choose.
    Runtime(JavaRuntimeSummary),
    /// Nothing installed qualifies; Ephervex downloads this major at launch.
    DownloadsAtLaunch { major: u32 },
    /// Nothing would be chosen.
    Unavailable(JavaUnavailable),
}

/// One runtime a host could be pinned to, with the host's verdict on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JavaChoice {
    pub runtime: JavaRuntimeSummary,
    /// Why the host's rules refuse it, or `None` when they accept it.
    pub rejection: Option<JavaMismatch>,
    /// Whether this host's own override names this runtime.
    pub current: bool,
}

impl JavaChoice {
    #[must_use]
    pub const fn is_compatible(&self) -> bool {
        self.rejection.is_none()
    }
}

/// The launcher-wide preferred Java path, as a host without its own override
/// uses it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LauncherJavaChoice {
    pub path: PathBuf,
    /// The runtime at that path, or `None` when the path is not a runtime that
    /// can be run.
    pub runtime: Option<JavaRuntimeSummary>,
    /// Why the host's rules refuse the runtime, when there is one to judge.
    pub rejection: Option<JavaMismatch>,
}

impl LauncherJavaChoice {
    /// Whether the host could run with it: the path is a runtime and the
    /// host's rules accept it.
    #[must_use]
    pub const fn is_compatible(&self) -> bool {
        self.runtime.is_some() && self.rejection.is_none()
    }
}

/// Everything a host's runtime can be set to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JavaChoices {
    pub host: InstalledHost,
    pub expected: ExpectedRuntime,
    pub mode: JavaSelectionMode,
    /// What clearing this host's own override resolves to when no launcher
    /// preference is set.
    pub automatic: AutomaticJava,
    /// What clearing this host's own override uses instead, when the launcher
    /// preference is set.
    pub launcher: Option<LauncherJavaChoice>,
    /// Compatible runtimes in the order selection prefers them, then
    /// incompatible ones newest first.
    pub runtimes: Vec<JavaChoice>,
}

/// A client or server whose own Java override names a runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinningHost {
    pub host: InstalledHost,
    pub name: String,
}

/// Every setting that names one runtime.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JavaRuntimePins {
    /// Hosts whose own override names it, clients first, each by name.
    pub hosts: Vec<PinningHost>,
    /// Whether the launcher-wide preferred Java path names it.
    pub launcher: bool,
}

impl JavaRuntimePins {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.hosts.is_empty() && !self.launcher
    }
}

/// The Java settings of every host, keyed by the runtime each names.
///
/// Paths are compared canonically, so a pin through a symlink still names the
/// runtime it resolves to.
#[derive(Debug, Clone, Default)]
pub struct JavaPinIndex {
    hosts: Vec<(PathBuf, PinningHost)>,
    launcher: Option<PathBuf>,
}

impl JavaPinIndex {
    /// The settings that name `runtime`, through its executable or any path it
    /// was found through.
    #[must_use]
    pub fn pins_for(&self, runtime: &JavaRuntime) -> JavaRuntimePins {
        let names = names_of(runtime);
        JavaRuntimePins {
            hosts: self
                .hosts
                .iter()
                .filter(|(path, _)| names.contains(path))
                .map(|(_, host)| host.clone())
                .collect(),
            launcher: self
                .launcher
                .as_ref()
                .is_some_and(|path| names.contains(path)),
        }
    }
}

/// The path a pin resolves to, or the path itself when it does not resolve.
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Every canonical path that names one runtime.
///
/// A split JDK 8 home is one runtime reached through two real executables,
/// `bin/java` and `jre/bin/java`: discovery keeps the first as the executable
/// and the second as an origin, and a pin may name either.
fn names_of(runtime: &JavaRuntime) -> Vec<PathBuf> {
    let mut names = vec![canonical(&runtime.executable)];
    for origin in &runtime.origins {
        let path = canonical(&origin.path);
        if !names.contains(&path) {
            names.push(path);
        }
    }
    names
}

/// A host's requirement and its loader's bounds, evaluated together.
pub(crate) struct HostJavaRules {
    pub(crate) required: RequiredJava,
    pub(crate) bounds: Vec<VersionBound>,
}

impl HostJavaRules {
    /// Why the host refuses `runtime`, through the evaluation selection uses.
    pub(crate) fn rejection(&self, runtime: &JavaRuntime) -> Option<JavaMismatch> {
        let rules: Vec<&dyn JavaCompatibilityRule> = self
            .bounds
            .iter()
            .map(|rule| rule as &dyn JavaCompatibilityRule)
            .collect();
        match ephervex_java::evaluate_runtime(runtime, &self.required, &rules) {
            RuleVerdict::Accept => None,
            RuleVerdict::Reject(rejection) => Some(JavaMismatch::of(&rejection.reason)),
        }
    }

    pub(crate) fn expected(&self, loader: LoaderKind) -> ExpectedRuntime {
        ExpectedRuntime {
            java: ExpectedJava::new(&self.required, &self.bounds),
            loader,
        }
    }
}

impl App {
    pub(crate) async fn host_java_rules(&self, facts: &HostFacts) -> CoreResult<HostJavaRules> {
        let required = self.required_java(&facts.minecraft_version).await?;
        let bounds =
            self.loader_java_rules(&facts.minecraft_version, &facts.loader, required.major)?;
        Ok(HostJavaRules { required, bounds })
    }

    /// The runtimes a host can be set to, each with the host's verdict.
    ///
    /// Lists the inventory, scanning it first when nothing is cached, plus the
    /// host's own pinned runtime when the inventory does not hold it. Probes,
    /// so call it off a frontend's UI thread.
    #[instrument(skip(self), fields(host = %host.id()))]
    pub async fn java_choices(&self, host: InstalledHost) -> CoreResult<JavaChoices> {
        let facts = self.host_facts(host)?;
        let rules = self.host_java_rules(&facts).await?;
        let settings = self.java_settings();
        let inventory = self.java_runtimes().await?;
        let pinned = facts.java_override.as_deref().map(canonical);

        let names_pin = |runtime: &JavaRuntime| {
            pinned
                .as_ref()
                .is_some_and(|pinned| names_of(runtime).contains(pinned))
        };
        let mut runtimes: Vec<JavaRuntime> = inventory.runtimes.clone();
        if let Some(path) = facts.java_override.as_deref()
            && !runtimes.iter().any(names_pin)
            && let Ok(runtime) = self
                .identify_pinned_java(path, PinSource::InstanceOverride, PinProbe::ReuseInventory)
                .await
        {
            runtimes.push(runtime);
        }
        let mut evaluated: Vec<(JavaRuntime, Option<JavaMismatch>)> = runtimes
            .into_iter()
            .map(|runtime| {
                let rejection = rules.rejection(&runtime);
                (runtime, rejection)
            })
            .collect();
        evaluated.sort_by(|(left, left_rejection), (right, right_rejection)| {
            left_rejection
                .is_some()
                .cmp(&right_rejection.is_some())
                .then_with(|| ephervex_java::compare_preference(left, right))
        });

        let automatic = if settings.auto_detect {
            let bounds: Vec<&dyn JavaCompatibilityRule> = rules
                .bounds
                .iter()
                .map(|rule| rule as &dyn JavaCompatibilityRule)
                .collect();
            match ephervex_java::select(&inventory, &rules.required, &bounds) {
                Ok(selection) => {
                    AutomaticJava::Runtime(JavaRuntimeSummary::of_runtime(&selection.runtime))
                }
                Err(_) if settings.auto_download => AutomaticJava::DownloadsAtLaunch {
                    major: rules.required.major,
                },
                Err(_) => AutomaticJava::Unavailable(JavaUnavailable::NothingCompatible),
            }
        } else {
            AutomaticJava::Unavailable(JavaUnavailable::DetectionDisabled)
        };
        let launcher = match settings.preferred_path {
            Some(path) => {
                let runtime = self
                    .identify_pinned_java(
                        &path,
                        PinSource::GlobalPreference,
                        PinProbe::ReuseInventory,
                    )
                    .await
                    .ok();
                Some(LauncherJavaChoice {
                    rejection: runtime
                        .as_ref()
                        .and_then(|runtime| rules.rejection(runtime)),
                    runtime: runtime.as_ref().map(JavaRuntimeSummary::of_runtime),
                    path,
                })
            }
            None => None,
        };
        let layer = if facts.java_override.is_some() {
            Some(JavaLayer::Host)
        } else if launcher.is_some() {
            Some(JavaLayer::Launcher)
        } else {
            None
        };
        Ok(JavaChoices {
            host,
            expected: rules.expected(facts.loader.kind),
            mode: JavaSelectionMode::of_layer(layer),
            automatic,
            launcher,
            runtimes: evaluated
                .into_iter()
                .map(|(runtime, rejection)| JavaChoice {
                    current: names_pin(&runtime),
                    runtime: JavaRuntimeSummary::of_runtime(&runtime),
                    rejection,
                })
                .collect(),
        })
    }

    /// Probes an executable the user named and gives the host's verdict on it,
    /// before any change to the host is previewed.
    ///
    /// Fails when the path is not a runtime that can be run.
    #[instrument(skip(self), fields(host = %host.id()))]
    pub async fn evaluate_java_path(
        &self,
        host: InstalledHost,
        executable: &Path,
    ) -> CoreResult<JavaChoice> {
        let facts = self.host_facts(host)?;
        let rules = self.host_java_rules(&facts).await?;
        let runtime = self
            .identify_pinned_java(executable, PinSource::ExplicitRequest, PinProbe::Fresh)
            .await?;
        let current = facts
            .java_override
            .as_deref()
            .is_some_and(|pinned| names_of(&runtime).contains(&canonical(pinned)));
        Ok(JavaChoice {
            rejection: rules.rejection(&runtime),
            runtime: JavaRuntimeSummary::of_runtime(&runtime),
            current,
        })
    }

    /// Every client and server override and the launcher preference, keyed by
    /// the runtime each names.
    ///
    /// Reads every host record, so a frontend builds it once per listing
    /// rather than once per runtime. Each override is read from the host's
    /// committed metadata, the record a launch or start reads.
    pub fn java_pin_index(&self) -> CoreResult<JavaPinIndex> {
        let mut hosts = Vec::new();
        for listed in self.list_clients()? {
            let client = self.current_client(listed.id)?;
            if let Some(java) = client.java_override {
                hosts.push((
                    canonical(&java.path),
                    PinningHost {
                        host: InstalledHost::Instance(client.id),
                        name: client.name,
                    },
                ));
            }
        }
        for listed in self.list_servers()? {
            let server = self.registry().current_server(listed.id)?;
            if let Some(java) = server.java_override {
                hosts.push((
                    canonical(&java.path),
                    PinningHost {
                        host: InstalledHost::Server(server.id),
                        name: server.name,
                    },
                ));
            }
        }
        Ok(JavaPinIndex {
            hosts,
            launcher: self
                .java_settings()
                .preferred_path
                .as_deref()
                .map(canonical),
        })
    }
}
