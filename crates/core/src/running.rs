//! What this launcher currently has running.
//!
//! # Why this is one query
//!
//! Running state is recorded in two unrelated places, because the two kinds of
//! run are supervised differently. A client instance holds a launch lock naming
//! the launcher run that owns the launch and, once it exists, the game process
//! under it. A server writes runtime evidence naming its supervisor and that
//! supervisor's java child. Both are read one entity at a time, while building
//! a list of instances or a list of servers, and neither answers "what is
//! running" on its own.
//!
//! This answers it across both, so a screen can show everything at once without
//! knowing which entity anything belongs to.
//!
//! # What counts as running
//!
//! Only what this launcher is supervising: a live launch, and a server whose
//! evidence still names a process that has not been proven gone. A server whose
//! supervisor is proven dead is an orphan and is deliberately not here; it is
//! the server workspace's recovery flow that deals with one, and listing it
//! among healthy runs would make "running" mean two different things.
//!
//! # Live resource use
//!
//! CPU is a rate, not a reading: it can only be computed between two samples of
//! the same process table. So [`ProcessMonitor`] is a value the caller keeps and
//! polls, rather than a function that answers once. The first sample of any
//! process reports no CPU, which is a property of the measurement and not a
//! process that is idle.

use chrono::{DateTime, Utc};
use ephervex_model::{InstanceId, ServerId};
use ephervex_server::RuntimeClassification;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

use crate::app::App;
use crate::diagnostics::DiagnosticScope;
use crate::error::CoreResult;
use crate::mods::InstalledHost;
use ephervex_storage::ForcedStop;

/// How much of a log's tail is read to find the line a row shows.
///
/// A line is the last thing in the file, so a small window from the end is
/// enough; reading more would cost the same on every poll and show the same
/// line.
const LAST_LINE_WINDOW: u64 = 8 * 1024;

/// What one running process is using.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProcessUsage {
    /// Percent of one core, so a process using four cores fully reports 400.
    pub cpu_percent: f32,
    pub memory_bytes: u64,
}

/// One thing this launcher is running.
#[derive(Debug, Clone, PartialEq)]
pub struct RunningProcess {
    /// Which entity this belongs to, and therefore where its actions go.
    pub host: InstalledHost,
    /// Which of that entity's launches this is, when it is not the first.
    ///
    /// An instance can hold more than one at a time -- a second account is a
    /// second launch -- so the host alone does not name a row. `None` is the
    /// first launch, and every server.
    pub launch: Option<String>,
    pub name: String,
    /// The game or server process, once it exists.
    ///
    /// A launch that is still downloading has taken its lock and has no game
    /// process yet, which is a real state a list has to be able to show.
    pub pid: Option<u32>,
    /// For a client, the launcher run supervising the launch.
    ///
    /// Distinct from [`Self::pid`] because stopping the game and stopping the
    /// run that owns it are different acts: the first lets the supervisor
    /// observe the exit and release the lock, the second is for when that
    /// supervisor is itself wedged.
    pub owner_pid: Option<u32>,
    /// When the run started, where that is recorded.
    pub since: Option<DateTime<Utc>>,
    /// Whether commands can be sent to it. Servers only, and only while the
    /// control channel answers.
    pub controllable: bool,
    /// The most recent line of its output, when one could be read.
    pub last_line: Option<String>,
    /// Filled in by [`ProcessMonitor::observe`], never by the listing itself:
    /// reading the process table is a cost a caller opts into.
    pub usage: Option<ProcessUsage>,
}

impl RunningProcess {
    /// How long this has been running, in seconds, as of now.
    ///
    /// Seconds rather than a duration type, so a frontend can show it without
    /// taking on a date library to do it. Negative is clamped away: a clock
    /// that moved backwards is not a run that started in the future.
    #[must_use]
    pub fn uptime_seconds(&self) -> Option<i64> {
        self.since
            .map(|since| (Utc::now() - since).num_seconds().max(0))
    }
}

/// A process table kept between samples, so CPU can be a rate.
#[derive(Debug)]
pub struct ProcessMonitor {
    system: System,
}

impl Default for ProcessMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessMonitor {
    #[must_use]
    pub fn new() -> Self {
        Self {
            system: System::new(),
        }
    }

    /// Fills in live CPU and memory for the processes named.
    ///
    /// Only the pids asked about are refreshed, so the cost is the size of the
    /// list rather than the size of the machine's process table. A process that
    /// has exited between the listing and this call simply reports nothing.
    pub fn observe(&mut self, processes: &mut [RunningProcess]) {
        let pids: Vec<Pid> = processes
            .iter()
            .filter_map(|process| process.pid)
            .map(Pid::from_u32)
            .collect();
        if pids.is_empty() {
            return;
        }
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&pids),
            true,
            ProcessRefreshKind::nothing().with_cpu().with_memory(),
        );

        for process in processes.iter_mut() {
            process.usage = process
                .pid
                .and_then(|pid| self.system.process(Pid::from_u32(pid)))
                .map(|observed| ProcessUsage {
                    cpu_percent: observed.cpu_usage(),
                    memory_bytes: observed.memory(),
                });
        }
    }
}

impl App {
    /// Everything this launcher is currently running, clients and servers.
    ///
    /// Ordered by how long each has been running, newest first, so the run
    /// somebody just started is the one at the top. Entries whose start time is
    /// not recorded sort last rather than being dropped.
    pub fn running_processes(&self) -> CoreResult<Vec<RunningProcess>> {
        let mut running = Vec::new();

        for record in self.list_clients()? {
            // Every launch, not just the first: a second account running
            // alongside is a second row, or it would be invisible and could
            // not be stopped.
            // At the root the files are in, which a listed row can lag.
            let root = self
                .client(record.id)
                .map_or_else(|_| record.root.clone(), |current| current.root);
            let launches = ephervex_storage::running_launches(&root);
            let shared_log = self.last_log_line(DiagnosticScope::Client(record.id));
            for launch in launches {
                running.push(RunningProcess {
                    host: InstalledHost::Instance(record.id),
                    launch: launch.id,
                    name: record.name.clone(),
                    pid: launch.state.game_pid,
                    owner_pid: Some(launch.state.owner_pid),
                    since: Some(launch.state.since),
                    controllable: false,
                    // The instance's own log, which both clients write to
                    // because they share the game directory.
                    last_line: shared_log.clone(),
                    usage: None,
                });
            }
        }

        for record in self.list_servers()? {
            let status = self.server_status(record.id)?;
            let Some(process) = supervised_process(&status.runtime) else {
                continue;
            };
            running.push(RunningProcess {
                host: InstalledHost::Server(record.id),
                launch: None,
                name: record.name,
                pid: Some(process.pid),
                owner_pid: None,
                since: Some(process.started_at),
                controllable: status.runtime.is_controlled(),
                last_line: self.last_server_line(record.id),
                usage: None,
            });
        }

        running.sort_by(|left, right| {
            right
                .since
                .cmp(&left.since)
                .then_with(|| left.name.cmp(&right.name))
        });
        Ok(running)
    }

    /// Ends a launch this process is not supervising, and releases its lock.
    ///
    /// The ordinary stop is canceling the run that owns the launch, which
    /// kills the game and waits for its real exit. This is the other case: a
    /// launch left by a previous run of the launcher, or one whose supervisor
    /// is wedged, where the instance is stuck claiming to be busy.
    ///
    /// Nothing is signaled without proof that the pid is still the process the
    /// lock describes, so this cannot reach an unrelated program that was given
    /// a recycled id.
    ///
    /// `launch` names which of the instance's launches to end, for an instance
    /// running more than one; `None` is the first.
    pub fn force_stop_launch(
        &self,
        instance: InstanceId,
        launch: Option<&str>,
    ) -> CoreResult<ForcedStop> {
        let root = self.client(instance)?.root;
        Ok(ephervex_storage::force_release(&root, launch)?)
    }

    /// The last line of a client's `latest.log`, best effort.
    fn last_log_line(&self, scope: DiagnosticScope) -> Option<String> {
        let request = crate::diagnostics::DiagnosticRequest::new(
            scope,
            ephervex_storage::DiagnosticKind::Log,
            "latest.log",
        )
        .with_anchor(ephervex_storage::TextAnchor::Tail)
        .with_budget(LAST_LINE_WINDOW);
        let view = self.read_diagnostic(&request).ok()?;
        last_nonempty_line(&view.text)
    }

    /// The last line a server's console captured, best effort.
    fn last_server_line(&self, server: ServerId) -> Option<String> {
        let capture = self.server_output(server, 1).ok()?;
        capture
            .lines
            .last()
            .map(|line| line.text.trim_end().to_owned())
            .filter(|line| !line.is_empty())
    }
}

/// The process a server's evidence names, when the launcher still owns it.
///
/// The java child where there is one, and the supervisor otherwise: before the
/// child is spawned the supervisor is what is running, and a row that showed
/// nothing until the game started would be missing exactly the part of a start
/// worth watching.
///
/// An orphan is deliberately not one of these. Its supervisor is proven gone,
/// which makes it a recovery case rather than a run this launcher is managing.
const fn supervised_process(
    runtime: &RuntimeClassification,
) -> Option<&ephervex_server::ProcessEvidence> {
    match runtime {
        RuntimeClassification::Controlled { supervisor, child }
        | RuntimeClassification::UnknownRunning { supervisor, child } => match child {
            Some(child) => Some(child),
            None => Some(supervisor),
        },
        RuntimeClassification::Stopped
        | RuntimeClassification::Orphaned { .. }
        | RuntimeClassification::ProvenStale { .. } => None,
    }
}

/// The last line of a window of text that has anything in it.
fn last_nonempty_line(text: &str) -> Option<String> {
    text.lines()
        .rev()
        .map(str::trim_end)
        .find(|line| !line.is_empty())
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use ephervex_model::{LoaderSelection, StorageClass};
    use ephervex_storage::{ClientRecord, LaunchLock, NewClient, NewServer, Paths};

    use super::*;

    fn app_rooted_at(base: &std::path::Path) -> App {
        App::open_at(Paths::rooted_at(base)).expect("app opens")
    }

    fn instance(app: &App, name: &str) -> ClientRecord {
        app.create_client_for_test(
            NewClient::new(name, "1.21.1", LoaderSelection::vanilla())
                .with_storage(StorageClass::Temporary),
        )
        .expect("create")
    }

    /// An idle launcher is running nothing, and says so rather than failing.
    #[test]
    fn a_launcher_with_nothing_running_reports_an_empty_list() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        instance(&app, "Repro");

        assert!(app.running_processes().expect("query").is_empty());
    }

    /// A launch is running from the moment it takes its lock, which is before
    /// there is a game process to name.
    #[test]
    fn a_launch_is_running_before_its_game_process_exists() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let record = instance(&app, "Repro");
        let _lock = LaunchLock::acquire(&record.root).expect("the launch takes its lock");

        let running = app.running_processes().expect("query");
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].name, "Repro");
        assert_eq!(running[0].host, InstalledHost::Instance(record.id));
        assert_eq!(
            running[0].owner_pid,
            Some(std::process::id()),
            "the run that owns the launch is this process"
        );
        assert_eq!(
            running[0].pid, None,
            "a launch that is still preparing has no game to name yet"
        );
        assert!(running[0].since.is_some());
        assert!(
            !running[0].controllable,
            "a client has no console to send commands to"
        );
    }

    /// A released lock means the launch is over, whatever else is on disk.
    #[test]
    fn a_finished_launch_stops_being_reported() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let record = instance(&app, "Repro");

        let lock = LaunchLock::acquire(&record.root).expect("lock");
        assert_eq!(app.running_processes().expect("query").len(), 1);
        drop(lock);
        assert!(app.running_processes().expect("query").is_empty());
    }

    /// A launch that is still alive keeps its lock, because releasing it would
    /// let a second launch start beside the first.
    #[test]
    fn ending_a_launch_whose_launcher_run_is_alive_keeps_the_lock() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let record = instance(&app, "Repro");

        // Nothing held: not a failure, and nothing was signaled.
        let outcome = app
            .force_stop_launch(record.id, None)
            .expect("no lock is held");
        assert_eq!(outcome, ephervex_storage::ForcedStop::default());

        let lock = LaunchLock::acquire(&record.root).expect("lock");
        assert_eq!(app.running_processes().expect("query").len(), 1);

        let outcome = app.force_stop_launch(record.id, None).expect("ended");
        assert_eq!(
            outcome.lock,
            ephervex_storage::LockDisposition::Retained,
            "the instance stays busy while its launcher run is alive"
        );
        assert!(
            !outcome.signaled_owner,
            "this test process owns the lock, and ending a launch must not kill the launcher"
        );
        assert_eq!(app.running_processes().expect("query").len(), 1);
        drop(lock);
        assert!(
            app.running_processes().expect("query").is_empty(),
            "the launch releases its own lock when it ends"
        );
    }

    /// A pid alone is not safe to signal, so the lock records what proves it.
    #[test]
    fn a_recorded_game_is_only_believed_while_it_is_the_same_process() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let record = instance(&app, "Repro");
        let mut lock = LaunchLock::acquire(&record.root).expect("lock");

        // This process stands in for the game: it is alive, and its start time
        // is recorded alongside the id.
        lock.record_game_pid(std::process::id()).expect("record");
        let state = ephervex_storage::running_state(&record.root).expect("running");
        assert!(state.game_start_ticks.is_some());
        assert!(state.game_is_alive());

        // The same id, describing a process that started at a different time,
        // is a different process and must not be believed.
        let recycled = ephervex_storage::LaunchState {
            game_start_ticks: state.game_start_ticks.map(|ticks| ticks.wrapping_add(1)),
            ..state
        };
        assert!(!recycled.game_is_alive());

        // And a launch that never spawned anything has nothing to signal.
        let unspawned = ephervex_storage::LaunchState {
            game_pid: None,
            game_start_ticks: None,
            ..recycled
        };
        assert!(!unspawned.game_is_alive());
    }

    /// Launching as a second account is a second launch of one instance, and
    /// a row that did not appear could not be watched or stopped.
    #[test]
    fn a_second_launch_of_one_instance_is_its_own_row() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let record = instance(&app, "Repro");

        let _first = LaunchLock::acquire(&record.root).expect("the first launch");
        let _second =
            LaunchLock::acquire_additional(&record.root, "bob").expect("a second account");

        let running = app.running_processes().expect("query");
        assert_eq!(running.len(), 2, "both clients are running");
        assert!(
            running
                .iter()
                .all(|process| process.host == InstalledHost::Instance(record.id)),
            "both belong to the one instance"
        );
        let handles: Vec<Option<&str>> = running
            .iter()
            .map(|process| process.launch.as_deref())
            .collect();
        assert!(
            handles.contains(&None) && handles.contains(&Some("bob")),
            "each row names which launch it is: {handles:?}"
        );
    }

    /// Ending one launch leaves the other alone, which is the whole reason
    /// each holds its own lock.
    #[test]
    fn ending_one_launch_leaves_the_other_running() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let record = instance(&app, "Repro");

        let _first = LaunchLock::acquire(&record.root).expect("the first launch");
        let second = LaunchLock::acquire_additional(&record.root, "bob").expect("a second account");

        let outcome = app
            .force_stop_launch(record.id, Some("bob"))
            .expect("the second launch is asked to end");
        assert_eq!(
            outcome.lock,
            ephervex_storage::LockDisposition::Retained,
            "its launcher run is still alive"
        );
        drop(second);

        let running = app.running_processes().expect("query");
        assert_eq!(running.len(), 1, "the first client is untouched");
        assert_eq!(running[0].launch, None);
    }

    /// An orphan is a recovery case, not a run this launcher is managing, and
    /// listing it among healthy runs would make "running" mean two things.
    #[test]
    fn a_server_is_reported_only_while_the_launcher_still_owns_it() {
        use ephervex_server::{ProcessEvidence, ProcessRole, RuntimeEvidence};

        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let record = app
            .registry_for_services()
            .prepare_server(NewServer::new(
                "Repro Server",
                "1.21.1",
                LoaderSelection::vanilla(),
            ))
            .expect("prepare");
        let staging = record.root.with_extension("staging");
        std::fs::create_dir_all(ephervex_storage::metadata::server_game_dir(&staging))
            .expect("staging");
        app.registry_for_services()
            .publish_staged_server(&record, &staging)
            .expect("publish");

        // Nothing recorded: stopped.
        assert!(app.running_processes().expect("query").is_empty());

        // This process, supervising itself, is evidence the launcher owns.
        let evidence = RuntimeEvidence::for_current_supervisor().expect("evidence");
        let supervisor = evidence.supervisor.clone();
        let path = ephervex_server::runtime_path(&record.root);
        std::fs::create_dir_all(path.parent().expect("metadata dir")).expect("dir");
        std::fs::write(&path, serde_json::to_vec(&evidence).expect("evidence")).expect("write");

        let running = app.running_processes().expect("query");
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].host, InstalledHost::Server(record.id));
        assert_eq!(
            running[0].pid,
            Some(supervisor.pid),
            "before a child exists the supervisor is what is running"
        );
        assert_eq!(
            running[0].owner_pid, None,
            "a server's supervision is its own, not a launch lock"
        );

        // A supervisor that is proven gone is an orphan, and belongs to the
        // recovery flow rather than to this list.
        let mut orphaned = evidence.clone();
        orphaned.supervisor.start_ticks = supervisor.start_ticks.wrapping_add(1);
        orphaned.child = Some(ProcessEvidence {
            role: ProcessRole::JavaChild,
            ..supervisor
        });
        std::fs::write(&path, serde_json::to_vec(&orphaned).expect("evidence")).expect("write");
        assert!(
            app.running_processes().expect("query").is_empty(),
            "an orphan is not a run this launcher is managing"
        );
    }

    #[test]
    fn the_line_shown_beside_a_run_is_the_last_one_its_log_carries() {
        assert_eq!(
            last_nonempty_line("first\nsecond\n\n   \n"),
            Some("second".to_owned()),
            "trailing blank lines are not the latest news"
        );
        assert_eq!(
            last_nonempty_line("only line"),
            Some("only line".to_owned())
        );
        assert_eq!(last_nonempty_line("\n\n  \n"), None);
        assert_eq!(last_nonempty_line(""), None);
    }

    /// CPU is a rate between two samples, so the first one has nothing to
    /// report yet. Asking about nothing must not read the process table at all.
    #[test]
    fn observing_no_processes_reads_nothing() {
        let mut monitor = ProcessMonitor::new();
        let mut empty: Vec<RunningProcess> = Vec::new();
        monitor.observe(&mut empty);

        // This process exists, so memory is readable on the very first sample.
        let mut mine = vec![RunningProcess {
            host: InstalledHost::Instance(ephervex_model::InstanceId::new()),
            launch: None,
            name: "self".to_owned(),
            pid: Some(std::process::id()),
            owner_pid: None,
            since: None,
            controllable: false,
            last_line: None,
            usage: None,
        }];
        monitor.observe(&mut mine);
        let usage = mine[0].usage.expect("this process is observable");
        assert!(usage.memory_bytes > 0);
    }
}
