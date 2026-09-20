//! Dependency-aware mod isolation for one installed host.
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::dependencies::ModGraph;
use crate::{App, CoreError, CoreResult, DependencyKnowledge, InstalledHost, ModPresence};

/// A suspect the user can test, together with everything it requires.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IsolationCandidate {
    pub root: PathBuf,
    pub name: String,
    pub closure: Vec<PathBuf>,
}

/// A required dependency whose jar is unavailable.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct IsolationMissingDependency {
    pub required_by: PathBuf,
    pub dependency: String,
}

/// A linked installation that this single-host workflow does not change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IsolationLinkedServer {
    pub id: String,
    pub name: String,
}

/// An offline plan derived entirely from the shared dependency graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IsolationPlan {
    pub pinned: Vec<PathBuf>,
    pub pinned_closure: Vec<PathBuf>,
    pub candidates: Vec<IsolationCandidate>,
    pub available: Vec<PathBuf>,
    pub initially_enabled: Vec<PathBuf>,
    pub unrecorded: Vec<PathBuf>,
    pub unidentifiable: Vec<PathBuf>,
    pub missing: Vec<IsolationMissingDependency>,
    pub linked_servers: Vec<IsolationLinkedServer>,
}

impl IsolationPlan {
    /// Recomputes a configuration as a union of complete required closures.
    /// A shared library stays enabled whenever any selected root requires it.
    pub fn enabled_for(&self, roots: &[PathBuf]) -> Vec<PathBuf> {
        let mut enabled: BTreeSet<_> = self.pinned_closure.iter().cloned().collect();
        for candidate in &self.candidates {
            if roots.contains(&candidate.root) {
                enabled.extend(candidate.closure.iter().cloned());
            }
        }
        enabled.into_iter().collect()
    }
}

impl App {
    /// Plans an isolation session without modifying files or reaching a catalog.
    /// Unrecorded and missing dependencies are returned for an explicit decision.
    pub fn plan_isolation(
        &self,
        host: InstalledHost,
        keep: &[PathBuf],
    ) -> CoreResult<IsolationPlan> {
        let graph = ModGraph::build(self, host)?;
        let mut plan = IsolationPlan {
            pinned: keep
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
            pinned_closure: Vec::new(),
            candidates: Vec::new(),
            available: Vec::new(),
            initially_enabled: Vec::new(),
            unrecorded: Vec::new(),
            unidentifiable: Vec::new(),
            missing: Vec::new(),
            linked_servers: Vec::new(),
        };
        for path in &plan.pinned {
            let Some(index) = graph.index_of(path) else {
                return Err(CoreError::ModFileMissing { path: path.clone() });
            };
            if graph.entries[index].mod_entry.presence == ModPresence::Missing {
                return Err(CoreError::ModFileMissing { path: path.clone() });
            }
        }
        let mut closures = BTreeMap::new();
        let mut relevant = BTreeSet::new();
        let mut missing = BTreeSet::new();
        for (index, entry) in graph.entries.iter().enumerate() {
            let entry = &entry.mod_entry;
            if entry.presence == ModPresence::Missing {
                continue;
            }
            plan.available.push(entry.logical_path.clone());
            if entry.presence == ModPresence::Installed {
                plan.initially_enabled.push(entry.logical_path.clone());
            }
            if entry.presence != ModPresence::Installed
                && !plan.pinned.contains(&entry.logical_path)
            {
                continue;
            }
            let closure = graph.required_closure(index);
            let paths: BTreeSet<_> = closure
                .present
                .iter()
                .map(|entry| entry.logical_path.clone())
                .collect();
            relevant.extend(paths.iter().cloned());
            missing.extend(
                closure
                    .absent
                    .into_iter()
                    .map(|absent| IsolationMissingDependency {
                        required_by: entry.logical_path.clone(),
                        dependency: absent.name,
                    }),
            );
            if plan.pinned.contains(&entry.logical_path) {
                plan.pinned_closure.extend(paths.iter().cloned());
            }
            closures.insert(entry.logical_path.clone(), (entry.name.clone(), paths));
        }
        plan.pinned_closure.sort();
        plan.pinned_closure.dedup();
        populate_candidates(&mut plan, &closures);
        for entry in &graph.entries {
            let entry = &entry.mod_entry;
            if !relevant.contains(&entry.logical_path) {
                continue;
            }
            match entry.knowledge {
                DependencyKnowledge::Recorded => {}
                DependencyKnowledge::Unrecorded => plan.unrecorded.push(entry.logical_path.clone()),
                DependencyKnowledge::Unidentified | DependencyKnowledge::Unidentifiable => {
                    plan.unidentifiable.push(entry.logical_path.clone());
                }
            }
        }
        plan.available.sort();
        plan.initially_enabled.sort();
        plan.unrecorded.sort();
        plan.unidentifiable.sort();
        plan.candidates.sort_by(|a, b| a.root.cmp(&b.root));
        plan.missing = missing.into_iter().collect();
        if let InstalledHost::Instance(id) = host {
            plan.linked_servers = self
                .list_servers()?
                .into_iter()
                .filter(|server| server.linked_client_id == Some(id))
                .map(|server| IsolationLinkedServer {
                    id: server.id.to_string(),
                    name: server.name,
                })
                .collect();
            plan.linked_servers.sort_by(|a, b| a.id.cmp(&b.id));
        }
        Ok(plan)
    }
}

fn populate_candidates(
    plan: &mut IsolationPlan,
    closures: &BTreeMap<PathBuf, (String, BTreeSet<PathBuf>)>,
) {
    let pool: Vec<_> = plan
        .initially_enabled
        .iter()
        .filter(|path| !plan.pinned_closure.contains(path))
        .cloned()
        .collect();
    for path in &pool {
        let (name, closure) = &closures[path];
        // Mutually required mods form an inseparable component. Its
        // lexicographically first member represents it deterministically.
        let required_by_other = pool.iter().any(|other| {
            other != path
                && closures[other].1.contains(path)
                && (!closure.contains(other) || other < path)
        });
        if !required_by_other {
            plan.candidates.push(IsolationCandidate {
                root: path.clone(),
                name: name.clone(),
                closure: closure.iter().cloned().collect(),
            });
        }
    }
}
