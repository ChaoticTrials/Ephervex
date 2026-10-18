//! Linked servers that an isolation session keeps in step with its client.
//!
//! A server follows the client by catalog project (by jar name when a mod
//! records no project): a mod it loads follows a client mod that takes part in
//! the search, and is enabled exactly when the client's round enables that mod,
//! plus what its own mods require. Every other server mod stays as it was and
//! is never part of the search.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use ephervex_model::{InstanceId, ServerId, SnapshotId};
use ephervex_storage::{RegistryError, StorageError, metadata};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::dependencies::{ImpactedMod, ModGraph};
use crate::isolation::{
    IsolationError, IsolationLinkedServer, IsolationMissingDependency, IsolationPlan,
    IsolationSession, read_session_at,
};
use crate::mods::SecondCopies;
use crate::{
    App, CoreError, CoreResult, DependencyKnowledge, InstalledHost, ModPresence, SnapshotOrigin,
};

const LEADER_VERSION: u32 = 1;

/// One mod of a followed server and the client mods that decide whether it loads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IsolationServerMod {
    pub path: PathBuf,
    pub name: String,
    /// The server's own required closure, itself included.
    pub requires: Vec<PathBuf>,
    /// The client mods that take part in the search and share this mod's project.
    /// Empty for a mod that stays as it was.
    pub follows: Vec<PathBuf>,
    pub initially_enabled: bool,
}

/// Everything about a linked server that is derived from durable state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IsolationServerPlan {
    pub server: ServerId,
    pub name: String,
    pub mods: Vec<IsolationServerMod>,
    pub unrecorded: Vec<PathBuf>,
    pub unidentifiable: Vec<PathBuf>,
    pub missing: Vec<IsolationMissingDependency>,
}

impl IsolationServerPlan {
    /// The server's configuration for a client configuration.
    #[must_use]
    pub fn enabled_for(&self, client_enabled: &[PathBuf]) -> Vec<PathBuf> {
        let client: BTreeSet<&Path> = client_enabled.iter().map(PathBuf::as_path).collect();
        let mut enabled = BTreeSet::new();
        for entry in &self.mods {
            let on = if entry.follows.is_empty() {
                entry.initially_enabled
            } else {
                entry
                    .follows
                    .iter()
                    .any(|path| client.contains(path.as_path()))
            };
            if on {
                enabled.insert(entry.path.clone());
                enabled.extend(entry.requires.iter().cloned());
            }
        }
        enabled.into_iter().collect()
    }

    /// A plan that names a server and knows nothing about its mods.
    #[must_use]
    pub(crate) fn unplanned(server: ServerId, name: &str) -> Self {
        Self {
            server,
            name: name.to_owned(),
            mods: Vec::new(),
            unrecorded: Vec::new(),
            unidentifiable: Vec::new(),
            missing: Vec::new(),
        }
    }

    /// The mods `enabled` leaves off.
    #[must_use]
    pub fn disabled_for(&self, enabled: &[PathBuf]) -> Vec<PathBuf> {
        let enabled: BTreeSet<&Path> = enabled.iter().map(PathBuf::as_path).collect();
        self.mods
            .iter()
            .filter(|entry| !enabled.contains(entry.path.as_path()))
            .map(|entry| entry.path.clone())
            .collect()
    }

    /// What the server loaded before the session.
    #[must_use]
    pub fn initially_enabled(&self) -> Vec<PathBuf> {
        self.mods
            .iter()
            .filter(|entry| entry.initially_enabled)
            .map(|entry| entry.path.clone())
            .collect()
    }

    /// The mods that stay as they were, which no round tests: those with no
    /// counterpart in the search, those the server did not load, and what a
    /// mod that stays on requires.
    #[must_use]
    pub fn untested(&self) -> Vec<PathBuf> {
        let mut untested: BTreeSet<&PathBuf> = BTreeSet::new();
        for entry in self.mods.iter().filter(|entry| entry.follows.is_empty()) {
            untested.insert(&entry.path);
            if entry.initially_enabled {
                untested.extend(&entry.requires);
            }
        }
        untested.into_iter().cloned().collect()
    }
}

/// A linked server a session changes, with the snapshot that undoes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IsolationFollowedServer {
    #[serde(flatten)]
    pub plan: IsolationServerPlan,
    pub safety_snapshot: SnapshotId,
    /// The session no longer changes this server: it was restored by a cancel
    /// in progress, or the registry no longer has it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    released: bool,
}

impl IsolationFollowedServer {
    #[must_use]
    pub const fn new(plan: IsolationServerPlan, safety_snapshot: SnapshotId) -> Self {
        Self {
            plan,
            safety_snapshot,
            released: false,
        }
    }

    pub(crate) const fn is_released(&self) -> bool {
        self.released
    }

    pub(crate) const fn mark_released(&mut self) {
        self.released = true;
    }
}

/// What a server is set to for the session's current configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IsolationServerConfiguration {
    pub server: ServerId,
    pub name: String,
    pub enabled: Vec<PathBuf>,
    pub disabled: Vec<PathBuf>,
    /// Mods that stay as they were because no round tests them.
    pub untested: Vec<PathBuf>,
}

/// What a finished session left a followed server with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IsolationServerResult {
    pub server: ServerId,
    pub name: String,
    pub safety_snapshot: SnapshotId,
    pub enabled: Vec<PathBuf>,
    pub disabled: Vec<PathBuf>,
    /// Mods that stayed as they were because no round tested them.
    pub untested: Vec<PathBuf>,
}

/// A followed server whose files could not be reached when its session ended,
/// so it was neither restored nor set to the result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IsolationUnreachedServer {
    pub server: ServerId,
    pub name: String,
    /// The snapshot that keeps its state from before the session.
    pub safety_snapshot: SnapshotId,
}

/// The client whose isolation session currently owns a server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsolationLeader {
    pub client: InstanceId,
    pub name: String,
    pub safety_snapshot: SnapshotId,
}

#[derive(Serialize, Deserialize)]
struct LeaderDocument {
    isolation_leader_version: u32,
    client: InstanceId,
    safety_snapshot: SnapshotId,
}

impl IsolationSession {
    /// The servers this session still changes.
    pub(crate) fn active_servers(&self) -> impl Iterator<Item = &IsolationFollowedServer> {
        self.followed_servers
            .iter()
            .filter(|followed| !followed.released)
    }

    /// The linked servers the session does not change: those never named, and
    /// those it has released.
    #[must_use]
    pub fn unchanged_linked_servers(&self) -> Vec<&IsolationLinkedServer> {
        self.plan
            .linked_servers
            .iter()
            .filter(|server| {
                !self
                    .active_servers()
                    .any(|followed| followed.plan.server.to_string() == server.id)
            })
            .collect()
    }

    /// Each server the session still changes, as set for its current configuration.
    #[must_use]
    pub fn server_configurations(&self) -> Vec<IsolationServerConfiguration> {
        self.active_servers()
            .map(|followed| {
                let enabled = followed.plan.enabled_for(self.intended_enabled());
                IsolationServerConfiguration {
                    server: followed.plan.server,
                    name: followed.plan.name.clone(),
                    disabled: followed.plan.disabled_for(&enabled),
                    enabled,
                    untested: followed.plan.untested(),
                }
            })
            .collect()
    }
}

impl App {
    /// The client session that owns `server`, if one does.
    ///
    /// The client's session is the authority: it must list the server. The
    /// marker on the server and the server's link name the client to ask, so a
    /// server whose marker was lost is still owned, and a marker left behind
    /// by an ended session is inert.
    pub fn isolation_leader(&self, server: ServerId) -> CoreResult<Option<IsolationLeader>> {
        let linked = self.server(server)?.linked_client_id;
        let clients: BTreeSet<InstanceId> = self
            .read_leader_marker(server)?
            .into_iter()
            .chain(linked)
            .collect();
        for client in clients {
            let record = match self.client(client) {
                Ok(record) => record,
                Err(CoreError::Storage(StorageError::Registry(
                    RegistryError::InstanceUnknown { .. },
                ))) => continue,
                Err(error) => return Err(error),
            };
            let Some(session) = read_session_at(&record.root, InstalledHost::Instance(client))?
            else {
                continue;
            };
            if let Some(followed) = session
                .followed_servers
                .iter()
                .find(|followed| followed.plan.server == server)
            {
                return Ok(Some(IsolationLeader {
                    client,
                    name: record.name,
                    safety_snapshot: followed.safety_snapshot,
                }));
            }
        }
        Ok(None)
    }

    fn read_leader_marker(&self, server: ServerId) -> CoreResult<Option<InstanceId>> {
        let path = self.leader_path(server)?;
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(IsolationError::Unreadable { path, source }.into()),
        };
        let document: LeaderDocument =
            serde_json::from_slice(&bytes).map_err(|source| IsolationError::Invalid {
                path: path.clone(),
                source,
            })?;
        if document.isolation_leader_version > LEADER_VERSION {
            return Err(IsolationError::UnsupportedVersion {
                path,
                found: document.isolation_leader_version,
                supported: LEADER_VERSION,
            }
            .into());
        }
        Ok(Some(document.client))
    }

    /// Whether an isolation session owns the host's mods, as its own session or
    /// as a server that a client session keeps in step.
    pub(crate) fn isolation_holds(&self, host: InstalledHost) -> CoreResult<bool> {
        if self.isolation_state(host)?.is_some() {
            return Ok(true);
        }
        match host {
            InstalledHost::Instance(_) => Ok(false),
            InstalledHost::Server(id) => Ok(self.isolation_leader(id)?.is_some()),
        }
    }

    /// The servers linked to a client, each with the dependency gaps it would
    /// bring into a session, in id order.
    pub(crate) fn linked_server_summaries(
        &self,
        client: &ModGraph,
        participating: &[PathBuf],
        id: InstanceId,
    ) -> CoreResult<Vec<IsolationLinkedServer>> {
        let mut servers: Vec<_> = self
            .list_servers()?
            .into_iter()
            .filter(|server| server.linked_client_id == Some(id))
            .map(|server| {
                let gaps = if server.installation.is_installed() {
                    self.server_knowledge_gaps(client, participating, server.id, &server.name)
                } else {
                    IsolationServerPlan::unplanned(server.id, &server.name)
                };
                IsolationLinkedServer {
                    id: server.id.to_string(),
                    name: server.name,
                    installed: server.installation.is_installed(),
                    unrecorded: gaps.unrecorded,
                    unidentifiable: gaps.unidentifiable,
                    missing: gaps.missing,
                }
            })
            .collect();
        servers.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(servers)
    }

    /// Takes each named server's safety snapshot, discarding the ones already
    /// taken, and the host's, when one fails.
    pub(crate) fn snapshot_followed_servers(
        &self,
        host: InstalledHost,
        host_snapshot: SnapshotId,
        plans: Vec<IsolationServerPlan>,
        cancel: &CancellationToken,
    ) -> CoreResult<Vec<IsolationFollowedServer>> {
        let mut followed = Vec::with_capacity(plans.len());
        for plan in plans {
            let snapshot = self
                .create_snapshot(
                    InstalledHost::Server(plan.server),
                    "Before mod isolation",
                    SnapshotOrigin::BeforeIsolation,
                    cancel,
                )
                .inspect_err(|_| self.discard_unused_snapshots(host, host_snapshot, &followed))?;
            followed.push(IsolationFollowedServer::new(plan, snapshot.id));
        }
        Ok(followed)
    }

    /// Checks that `servers` can join a session on `client` and returns them in id order.
    pub(crate) fn resolve_followed_servers(
        &self,
        host: InstalledHost,
        servers: &[ServerId],
    ) -> CoreResult<Vec<(ServerId, String)>> {
        let ids: BTreeSet<ServerId> = servers.iter().copied().collect();
        let mut resolved = Vec::with_capacity(ids.len());
        for id in ids {
            let record = self
                .server(id)
                .map_err(|_| IsolationError::ServerNotLinked {
                    server: id.to_string(),
                })?;
            let linked = matches!(host, InstalledHost::Instance(client) if record.linked_client_id == Some(client));
            if !linked || !record.installation.is_installed() {
                return Err(IsolationError::ServerNotLinked {
                    server: record.name,
                }
                .into());
            }
            resolved.push((id, record.name));
        }
        Ok(resolved)
    }

    /// Plans how `server` follows the client the graph describes.
    pub(crate) fn plan_isolation_server(
        &self,
        client: &ModGraph,
        participating: &[PathBuf],
        server: ServerId,
        name: String,
    ) -> CoreResult<IsolationServerPlan> {
        let graph = ModGraph::build(self, InstalledHost::Server(server))?;
        let participating: BTreeSet<&Path> = participating.iter().map(PathBuf::as_path).collect();
        let mut counterparts: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
        for entry in &client.entries {
            let entry = &entry.mod_entry;
            if entry.presence != ModPresence::Missing
                && participating.contains(entry.logical_path.as_path())
            {
                counterparts
                    .entry(counterpart_key(entry))
                    .or_default()
                    .push(entry.logical_path.clone());
            }
        }
        let mut plan = IsolationServerPlan {
            server,
            name,
            mods: Vec::new(),
            unrecorded: Vec::new(),
            unidentifiable: Vec::new(),
            missing: Vec::new(),
        };
        let mut relevant = BTreeSet::new();
        let mut missing = BTreeSet::new();
        for (index, entry) in graph.entries.iter().enumerate() {
            let entry = &entry.mod_entry;
            if entry.presence == ModPresence::Missing {
                continue;
            }
            let closure = graph.required_closure(index);
            let requires: BTreeSet<PathBuf> = closure
                .present
                .iter()
                .map(|required| required.logical_path.clone())
                .collect();
            let initially_enabled = entry.presence == ModPresence::Installed;
            if initially_enabled {
                relevant.extend(requires.iter().cloned());
                missing.extend(closure.absent.into_iter().map(|absent| {
                    IsolationMissingDependency {
                        required_by: entry.logical_path.clone(),
                        dependency: absent.name,
                    }
                }));
            }
            plan.mods.push(IsolationServerMod {
                path: entry.logical_path.clone(),
                name: entry.name.clone(),
                requires: requires.into_iter().collect(),
                // A mod the server did not load is never turned on to match.
                follows: counterparts
                    .get(&counterpart_key(entry))
                    .filter(|_| initially_enabled)
                    .cloned()
                    .unwrap_or_default(),
                initially_enabled,
            });
        }
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
        plan.mods.sort_by(|a, b| a.path.cmp(&b.path));
        plan.unrecorded.sort();
        plan.unidentifiable.sort();
        plan.missing = missing.into_iter().collect();
        Ok(plan)
    }

    /// The knowledge gaps of a linked server, left empty when it cannot be read.
    pub(crate) fn server_knowledge_gaps(
        &self,
        client: &ModGraph,
        participating: &[PathBuf],
        server: ServerId,
        name: &str,
    ) -> IsolationServerPlan {
        self.plan_isolation_server(client, participating, server, name.to_owned())
            .unwrap_or_else(|error| {
                warn!(%server, %error, "a linked server's mods could not be read for isolation");
                IsolationServerPlan::unplanned(server, name)
            })
    }

    /// Writes the marker that makes each followed server session-owned.
    ///
    /// Idempotent, and called again on resume so a session interrupted before
    /// its markers existed still owns its servers.
    pub(crate) fn write_leader_markers(
        &self,
        host: InstalledHost,
        session: &IsolationSession,
    ) -> CoreResult<()> {
        let InstalledHost::Instance(client) = host else {
            return Ok(());
        };
        for followed in self.reachable_servers(session)? {
            let path = self.leader_path(followed.plan.server)?;
            ephervex_storage::write_atomic(
                &path,
                ephervex_storage::AtomicWriteOptions::private(),
                |file| {
                    serde_json::to_writer(
                        file,
                        &LeaderDocument {
                            isolation_leader_version: LEADER_VERSION,
                            client,
                            safety_snapshot: followed.safety_snapshot,
                        },
                    )
                    .map_err(std::io::Error::other)
                },
            )
            .map_err(|source| IsolationError::Unwritable { path, source })?;
        }
        Ok(())
    }

    /// Removes the markers once the session document is gone. A marker that
    /// outlives its session is inert, so a failure here is only logged.
    pub(crate) fn remove_leader_markers(&self, session: &IsolationSession) {
        for followed in &session.followed_servers {
            let removed =
                self.leader_path(followed.plan.server)
                    .and_then(|path| match std::fs::remove_file(&path) {
                        Ok(()) => Ok(()),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                        Err(source) => Err(IsolationError::Unwritable { path, source }.into()),
                    });
            if let Err(error) = removed {
                warn!(server = %followed.plan.server, %error, "an isolation marker could not be removed");
            }
        }
    }

    /// Refuses a start when the host, or a server that would join, already
    /// belongs to a session.
    pub(crate) fn refuse_owned_hosts(
        &self,
        host: InstalledHost,
        servers: &[(ServerId, String)],
    ) -> CoreResult<()> {
        if let InstalledHost::Server(id) = host
            && self.isolation_leader(id)?.is_some()
        {
            return Err(IsolationError::OwnedElsewhere {
                host: self.server(id)?.name,
            }
            .into());
        }
        for (id, name) in servers {
            if self.isolation_holds(InstalledHost::Server(*id))? {
                return Err(IsolationError::OwnedElsewhere { host: name.clone() }.into());
            }
        }
        Ok(())
    }

    /// Refuses turning on a server jar that would be a second copy of one
    /// loading, for the largest configuration any round can ask of the server.
    pub(crate) fn refuse_server_second_copies(
        &self,
        plan: &IsolationPlan,
        servers: &[IsolationServerPlan],
    ) -> CoreResult<()> {
        let client_enabled = plan.enabled_for(&plan.roots());
        for server in servers {
            let initially: BTreeSet<PathBuf> = server.initially_enabled().into_iter().collect();
            let enabled = server.enabled_for(&client_enabled);
            let turned_on: BTreeSet<&PathBuf> = enabled
                .iter()
                .filter(|path| !initially.contains(*path))
                .collect();
            self.require_distinct_jars(InstalledHost::Server(server.server), turned_on)?;
        }
        Ok(())
    }

    /// Deletes snapshots a session that never started took, best effort.
    pub(crate) fn discard_unused_snapshots(
        &self,
        host: InstalledHost,
        snapshot: SnapshotId,
        followed: &[IsolationFollowedServer],
    ) {
        let taken = std::iter::once((host, snapshot)).chain(followed.iter().map(|server| {
            (
                InstalledHost::Server(server.plan.server),
                server.safety_snapshot,
            )
        }));
        for (host, id) in taken {
            if let Err(error) = self.delete_snapshot(host, id) {
                warn!(host = %host.id(), %error, "an unused isolation snapshot could not be removed");
            }
        }
    }

    /// Sets every followed server to its configuration for `client_enabled`.
    pub(crate) fn apply_server_configurations(
        &self,
        session: &IsolationSession,
        client_enabled: &[PathBuf],
    ) -> CoreResult<()> {
        for followed in self.reachable_servers(session)? {
            self.apply_server_enabled(&followed.plan, &followed.plan.enabled_for(client_enabled))?;
        }
        Ok(())
    }

    /// Sets one server's mods to exactly `enabled`.
    pub(crate) fn apply_server_enabled(
        &self,
        plan: &IsolationServerPlan,
        enabled: &[PathBuf],
    ) -> CoreResult<()> {
        let enabled: BTreeSet<&PathBuf> = enabled.iter().collect();
        let changes: Vec<_> = plan
            .mods
            .iter()
            .map(|entry| (entry.path.clone(), enabled.contains(&entry.path)))
            .collect();
        self.apply_mod_toggles(
            InstalledHost::Server(plan.server),
            &changes,
            SecondCopies::Tolerated,
        )?;
        Ok(())
    }

    /// Whether every followed server has exactly the mods its configuration intends.
    pub(crate) fn server_configurations_applied(
        &self,
        session: &IsolationSession,
        client_enabled: &[PathBuf],
    ) -> CoreResult<bool> {
        for followed in self.reachable_servers(session)? {
            let graph = ModGraph::build(self, InstalledHost::Server(followed.plan.server))?;
            let intended: BTreeSet<_> = followed
                .plan
                .enabled_for(client_enabled)
                .into_iter()
                .collect();
            let applied = followed.plan.mods.iter().all(|entry| {
                graph.index_of(&entry.path).is_some_and(|index| {
                    match graph.entries[index].mod_entry.presence {
                        ModPresence::Installed => intended.contains(&entry.path),
                        ModPresence::Disabled => !intended.contains(&entry.path),
                        ModPresence::Missing => false,
                    }
                })
            });
            if !applied {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Refuses while a server the session changes may be running.
    pub(crate) fn refuse_followed_servers_running(
        &self,
        session: &IsolationSession,
    ) -> CoreResult<()> {
        for followed in self.reachable_servers(session)? {
            if self
                .server_status(followed.plan.server)?
                .runtime
                .may_be_running()
            {
                return Err(IsolationError::HostRunning {
                    host: followed.plan.name.clone(),
                }
                .into());
            }
        }
        Ok(())
    }

    /// Whether the registry still has the server.
    pub(crate) fn server_exists(&self, server: ServerId) -> CoreResult<bool> {
        match self.server(server) {
            Ok(_) => Ok(true),
            Err(CoreError::Storage(StorageError::Registry(RegistryError::ServerUnknown {
                ..
            }))) => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// The servers the session changes whose files can be reached right now.
    ///
    /// A server whose folder is missing, such as on an unmounted drive, is
    /// skipped for the operation without being released, so it is brought back
    /// in step once the folder returns.
    pub(crate) fn reachable_servers<'a>(
        &self,
        session: &'a IsolationSession,
    ) -> CoreResult<Vec<&'a IsolationFollowedServer>> {
        let mut reachable = Vec::new();
        for followed in session.active_servers() {
            if self.server_reachable(followed.plan.server)? {
                reachable.push(followed);
            }
        }
        Ok(reachable)
    }

    /// The servers the session still changes whose files cannot be reached now.
    pub(crate) fn unreached_servers(
        &self,
        session: &IsolationSession,
    ) -> CoreResult<Vec<IsolationUnreachedServer>> {
        let mut unreached = Vec::new();
        for followed in session.active_servers() {
            if !self.server_reachable(followed.plan.server)? {
                unreached.push(IsolationUnreachedServer {
                    server: followed.plan.server,
                    name: followed.plan.name.clone(),
                    safety_snapshot: followed.safety_snapshot,
                });
            }
        }
        Ok(unreached)
    }

    pub(crate) fn server_reachable(&self, server: ServerId) -> CoreResult<bool> {
        match self.server(server) {
            Ok(record) => Ok(record.root.is_dir()),
            Err(CoreError::Storage(StorageError::Registry(RegistryError::ServerUnknown {
                ..
            }))) => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn leader_path(&self, server: ServerId) -> CoreResult<PathBuf> {
        Ok(
            metadata::metadata_dir(&self.server(server)?.root)
                .join(metadata::ISOLATION_LEADER_FILE),
        )
    }
}

/// A server mod follows the client mod of the same catalog project, or of the
/// same jar name when neither records a project.
fn counterpart_key(entry: &ImpactedMod) -> String {
    match &entry.project_id {
        Some(project) => format!("project:{project}"),
        None => format!(
            "file:{}",
            entry
                .logical_path
                .file_name()
                .map(|name| name.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default()
        ),
    }
}
