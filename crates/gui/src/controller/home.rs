//! What the Home page shows: a greeting, a one-line summary, the problems worth
//! a look, the hosts to continue with, and the storage counts.
//!
//! Everything is built from the hosts the newest snapshot described, the run
//! failures this session reported, and the creations in progress. Nothing here
//! asks the worker or diagnoses a host (ADR 0043).

use chrono::{DateTime, Timelike as _, Utc};
use ephervex_core::InstalledHost;
use ephervex_i18n::{Arg, lookup, lookup_args, lookup_attribute_args};

use super::workspace::target_summary;
use super::{reconciled, to_i32};
use crate::backend;
use crate::presentation::{InstancePresentation, ServerPresentation, bounded_text};
use crate::ui::{AppWindow, AttentionRow, ResumeRow};

/// How many hosts "continue where you left off" offers.
const RESUME_LIMIT: usize = 4;
/// The longest a problem's title or detail is allowed to run.
const ATTENTION_TEXT_LIMIT: usize = 240;
/// The longest an account name is allowed to run in the greeting.
const GREETING_NAME_LIMIT: usize = 40;

/// One host as Home reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HomeHost {
    pub host: InstalledHost,
    pub name: String,
    /// Minecraft version and loader as one line.
    pub target: String,
    pub persistent: bool,
    pub run: RunState,
    pub can_launch: bool,
    pub last_run: Option<DateTime<Utc>>,
    /// Each recorded reason the host needs attention, already worded.
    pub attention: Vec<String>,
}

/// Whether a host's process runs, as far as the launcher can tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RunState {
    Idle,
    /// Something this launcher supervises is starting or running, or may be,
    /// which is what Home's Running section lists.
    Active,
    /// The supervisor is proven gone but the process lives: a recovery case,
    /// kept out of "running" as Home's Running section keeps it out.
    Orphaned,
}

impl RunState {
    fn of(state_kind: &str) -> Self {
        match state_kind {
            "running" | "starting" | "unknown" => Self::Active,
            "orphaned" => Self::Orphaned,
            _ => Self::Idle,
        }
    }
}

impl HomeHost {
    pub(super) fn from_instance(item: &InstancePresentation) -> Option<Self> {
        Some(Self {
            host: InstalledHost::Instance(backend::parse_instance_id(&item.id)?),
            name: item.name.clone(),
            target: target_summary(&item.minecraft, &item.loader).to_string(),
            persistent: item.storage_kind == "persistent",
            run: RunState::of(item.state_kind),
            can_launch: item.can_launch,
            last_run: item.last_launched_at,
            attention: item.attention.clone(),
        })
    }

    pub(super) fn from_server(item: &ServerPresentation) -> Option<Self> {
        Some(Self {
            host: InstalledHost::Server(backend::parse_server_id(&item.id)?),
            name: item.name.clone(),
            target: target_summary(&item.minecraft, &item.loader).to_string(),
            persistent: item.storage_kind == "persistent",
            run: RunState::of(item.state_kind),
            can_launch: item.can_start,
            last_run: item.last_started_at,
            attention: item.attention.clone(),
        })
    }
}

/// A run this session reported as failed, kept until its host starts again or
/// the user dismisses it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RunFailure {
    host: InstalledHost,
    title: String,
    failure: String,
}

/// The values the Home properties are written from.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct HomeView {
    pub greeting: String,
    pub summary: String,
    pub attention: Vec<AttentionRow>,
    pub resume: Vec<ResumeRow>,
    pub temporary: usize,
    pub persistent: usize,
    pub servers: usize,
}

#[derive(Default)]
pub(super) struct HomeState {
    clients: Vec<HomeHost>,
    servers: Vec<HomeHost>,
    default_account: String,
    building: usize,
    failures: Vec<RunFailure>,
    /// Something changed since the last publish.
    dirty: bool,
    /// The minute the last publish was built for, so relative times and the
    /// greeting are rebuilt as time passes.
    published_minute: Option<i64>,
}

fn host_surface(host: InstalledHost) -> &'static str {
    match host {
        InstalledHost::Instance(_) => "Diagnostics",
        InstalledHost::Server(_) => "Overview",
    }
}

fn attention_row(host: &HomeHost, title: &str, detail: &str) -> AttentionRow {
    AttentionRow {
        kind: host.host.kind().into(),
        id: host.host.id().into(),
        host_name: host.name.as_str().into(),
        title: bounded_text(title, ATTENTION_TEXT_LIMIT).into(),
        detail: bounded_text(detail, ATTENTION_TEXT_LIMIT).into(),
        surface: host_surface(host.host).into(),
    }
}

fn greeting(hour: u32, name: &str) -> String {
    let id = match hour {
        5..=11 => "gui-home-greeting-morning",
        12..=17 => "gui-home-greeting-afternoon",
        _ => "gui-home-greeting-evening",
    };
    if name.is_empty() {
        return lookup(id);
    }
    lookup_attribute_args(
        id,
        "named",
        [("name", Arg::from(bounded_text(name, GREETING_NAME_LIMIT)))],
    )
    .unwrap_or_else(|| lookup(id))
}

/// "2 running | 2 problems | 1 building", leaving out what is zero.
fn summary(running: usize, problems: usize, building: usize) -> String {
    let mut parts = Vec::new();
    if running > 0 {
        parts.push(lookup_args(
            "gui-home-summary-running",
            [("count", Arg::from(running))],
        ));
    }
    if problems > 0 {
        parts.push(lookup_args(
            "gui-home-summary-problems",
            [("count", Arg::from(problems))],
        ));
    }
    if building > 0 {
        parts.push(lookup_args(
            "gui-home-summary-building",
            [("count", Arg::from(building))],
        ));
    }
    if parts.is_empty() {
        return lookup("gui-nothing-running");
    }
    parts.join(&lookup("gui-home-summary-separator"))
}

impl HomeState {
    pub(super) fn set_clients(&mut self, clients: Vec<HomeHost>) {
        self.clients = clients;
        self.dirty = true;
    }

    pub(super) fn set_servers(&mut self, servers: Vec<HomeHost>) {
        self.servers = servers;
        self.dirty = true;
    }

    pub(super) fn set_default_account(&mut self, name: Option<String>) {
        let name = name.unwrap_or_default();
        if self.default_account != name {
            self.default_account = name;
            self.dirty = true;
        }
    }

    /// How many creations are in progress.
    pub(super) fn set_building(&mut self, building: usize) {
        if self.building != building {
            self.building = building;
            self.dirty = true;
        }
    }

    /// Keeps a failed run of `host`, replacing the one it had.
    pub(super) fn note_run_failure(&mut self, host: InstalledHost, title: String, failure: String) {
        let notice = RunFailure {
            host,
            title,
            failure,
        };
        match self.failures.iter_mut().find(|known| known.host == host) {
            Some(known) => *known = notice,
            None => self.failures.push(notice),
        }
        self.dirty = true;
    }

    /// Forgets `host`'s failed run, once it starts again or the user dismisses it.
    pub(super) fn clear_run_failure(&mut self, host: InstalledHost) {
        let before = self.failures.len();
        self.failures.retain(|known| known.host != host);
        self.dirty |= self.failures.len() != before;
    }

    #[cfg(test)]
    fn has_run_failure(&self, host: InstalledHost) -> bool {
        self.failures.iter().any(|known| known.host == host)
    }

    /// Every problem: each recorded reason of each host, then each failed run
    /// of a host that is still listed.
    fn attention_rows(&self) -> Vec<AttentionRow> {
        let hosts = || self.clients.iter().chain(&self.servers);
        let orphan = lookup("gui-state-orphaned-detail");
        let recorded = hosts().flat_map(|host| {
            let orphan = (host.run == RunState::Orphaned).then(|| orphan.clone());
            host.attention
                .iter()
                .cloned()
                .chain(orphan)
                .map(move |reason| attention_row(host, &reason, &host.target))
                .collect::<Vec<_>>()
        });
        let failed = self.failures.iter().filter_map(|notice| {
            let host = hosts().find(|host| host.host == notice.host)?;
            Some(attention_row(host, &notice.title, &notice.failure))
        });
        recorded.chain(failed).collect()
    }

    /// The hosts to continue with: the newest last runs, leaving out whatever
    /// is running.
    fn resume_rows(&self, now: DateTime<Utc>) -> Vec<ResumeRow> {
        let mut ran: Vec<(&HomeHost, DateTime<Utc>)> = self
            .clients
            .iter()
            .chain(&self.servers)
            .filter(|host| host.run == RunState::Idle)
            .filter_map(|host| Some((host, host.last_run?)))
            .collect();
        // Stable, so hosts that last ran together keep the list's order.
        ran.sort_by_key(|(_, last_run)| std::cmp::Reverse(*last_run));
        ran.into_iter()
            .take(RESUME_LIMIT)
            .map(|(host, last_run)| ResumeRow {
                kind: host.host.kind().into(),
                id: host.host.id().into(),
                host_name: host.name.as_str().into(),
                last_run: backend::format_last_run(Some(last_run), now).into(),
                can_launch: host.can_launch,
            })
            .collect()
    }

    /// What Home shows at `now`, with the local clock at `hour`.
    pub(super) fn view(&self, now: DateTime<Utc>, hour: u32) -> HomeView {
        let attention = self.attention_rows();
        let running = self
            .clients
            .iter()
            .chain(&self.servers)
            .filter(|host| host.run == RunState::Active)
            .count();
        HomeView {
            greeting: greeting(hour, &self.default_account),
            summary: summary(running, attention.len(), self.building),
            attention,
            resume: self.resume_rows(now),
            temporary: self.clients.iter().filter(|host| !host.persistent).count(),
            persistent: self.clients.iter().filter(|host| host.persistent).count(),
            servers: self.servers.len(),
        }
    }

    /// Whether Home needs writing again: something changed, or a minute passed.
    pub(super) fn due(&self, now: DateTime<Utc>) -> bool {
        self.dirty || self.published_minute != Some(now.timestamp() / 60)
    }

    /// Writes Home into the window.
    pub(super) fn publish(&mut self, ui: &AppWindow, now: DateTime<Utc>, hour: u32) {
        let view = self.view(now, hour);
        ui.set_home_greeting(view.greeting.into());
        ui.set_home_summary(view.summary.into());
        ui.set_home_attention(reconciled(&ui.get_home_attention(), view.attention));
        ui.set_home_resume(reconciled(&ui.get_home_resume(), view.resume));
        ui.set_home_temporary_count(to_i32(view.temporary));
        ui.set_home_persistent_count(to_i32(view.persistent));
        ui.set_home_server_count(to_i32(view.servers));
        self.dirty = false;
        self.published_minute = Some(now.timestamp() / 60);
    }

    /// Writes Home when it is due, from the local clock.
    pub(super) fn publish_when_due(&mut self, ui: &AppWindow) {
        let now = Utc::now();
        if self.due(now) {
            self.publish(ui, now, chrono::Local::now().hour());
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use chrono::TimeDelta;
    use ephervex_model::{InstanceId, ServerId};
    use slint::Model as _;

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).expect("a timestamp")
    }

    fn client(name: &str, ago: Option<TimeDelta>) -> HomeHost {
        HomeHost {
            host: InstalledHost::Instance(InstanceId::new()),
            name: name.to_owned(),
            target: "Minecraft 1.20.1 | Forge".to_owned(),
            persistent: false,
            run: RunState::Idle,
            can_launch: true,
            last_run: ago.map(|ago| now() - ago),
            attention: Vec::new(),
        }
    }

    fn server(name: &str, ago: Option<TimeDelta>) -> HomeHost {
        HomeHost {
            host: InstalledHost::Server(ServerId::new()),
            ..client(name, ago)
        }
    }

    fn state(clients: Vec<HomeHost>, servers: Vec<HomeHost>) -> HomeState {
        let mut state = HomeState::default();
        state.set_clients(clients);
        state.set_servers(servers);
        state
    }

    #[test]
    fn the_greeting_follows_the_hour_and_names_the_default_account() {
        assert_eq!(greeting(5, "Quiet"), "Good morning, Quiet");
        assert_eq!(greeting(11, "Quiet"), "Good morning, Quiet");
        assert_eq!(greeting(12, "Quiet"), "Good afternoon, Quiet");
        assert_eq!(greeting(17, "Quiet"), "Good afternoon, Quiet");
        assert_eq!(greeting(18, "Quiet"), "Good evening, Quiet");
        assert_eq!(greeting(2, "Quiet"), "Good evening, Quiet");
        assert_eq!(greeting(9, ""), "Good morning");
    }

    #[test]
    fn a_long_account_name_is_cut_in_the_greeting() {
        let text = greeting(9, &"n".repeat(300));
        assert!(text.chars().count() < 80);
        assert!(text.ends_with("..."));
    }

    #[test]
    fn the_summary_counts_running_problems_and_building_and_skips_zeros() {
        assert_eq!(summary(2, 2, 1), "2 running | 2 problems | 1 building");
        assert_eq!(summary(0, 1, 0), "1 problem");
        assert_eq!(summary(3, 0, 0), "3 running");
        assert_eq!(summary(0, 0, 0), "Nothing is running.");
    }

    #[test]
    fn running_counts_come_from_active_hosts_of_both_kinds() {
        let mut running_client = client("a", None);
        running_client.run = RunState::Active;
        let mut running_server = server("b", None);
        running_server.run = RunState::Active;
        let mut home = state(
            vec![running_client, client("c", None)],
            vec![running_server],
        );
        home.set_building(1);
        assert_eq!(home.view(now(), 9).summary, "2 running | 1 building");
    }

    #[test]
    fn a_server_that_may_be_running_counts_as_running_and_is_not_offered_to_continue() {
        let mut unknown = server("Unknown", Some(TimeDelta::hours(1)));
        unknown.run = RunState::Active;
        let home = state(Vec::new(), vec![unknown]);
        let view = home.view(now(), 9);
        assert_eq!(view.summary, "1 running");
        assert!(view.resume.is_empty());
    }

    #[test]
    fn an_orphaned_host_is_a_problem_and_not_a_running_host() {
        let mut orphan = server("Orphan", Some(TimeDelta::hours(1)));
        orphan.run = RunState::Orphaned;
        let home = state(vec![client("Fine", None)], vec![orphan]);
        let view = home.view(now(), 9);
        assert_eq!(view.summary, "1 problem");
        assert!(view.resume.is_empty());
        assert_eq!(view.attention.len(), 1);
        assert_eq!(view.attention[0].title, lookup("gui-state-orphaned-detail"));
        assert_eq!(view.attention[0].surface, "Overview");
    }

    #[test]
    fn from_presented_hosts_unknown_is_active_and_orphaned_is_not() {
        use crate::presentation::{ServerPresentation, samples::server_status};
        use ephervex_core::RuntimeClassification;
        let evidence = |runtime| {
            let mut status = server_status(RuntimeClassification::Stopped);
            status.runtime = runtime;
            HomeHost::from_server(&ServerPresentation::from_status(&status)).expect("a host")
        };
        let process = ephervex_server::ProcessEvidence {
            role: ephervex_server::ProcessRole::Supervisor,
            pid: std::process::id(),
            boot_id: String::new(),
            start_ticks: 1,
            executable: std::path::PathBuf::new(),
            executable_device: None,
            executable_inode: None,
            started_at: Utc::now(),
        };
        let unknown = evidence(RuntimeClassification::UnknownRunning {
            supervisor: process.clone(),
            child: None,
        });
        assert_eq!(unknown.run, RunState::Active);
        let orphan = evidence(RuntimeClassification::Orphaned { child: process });
        assert_eq!(orphan.run, RunState::Orphaned);
    }

    #[test]
    fn every_recorded_reason_is_a_row_that_opens_the_surface_that_explains_it() {
        let mut broken = client("Frostbound", None);
        broken.attention = vec!["Folder missing".to_owned(), "Review files".to_owned()];
        let mut conversion = server("Pack server", None);
        conversion.attention = vec!["Best effort".to_owned()];
        let home = state(vec![broken.clone(), client("Fine", None)], vec![conversion]);

        let rows = home.view(now(), 9).attention;
        let shown: Vec<(&str, &str, &str, &str)> = rows
            .iter()
            .map(|row| {
                (
                    row.kind.as_str(),
                    row.host_name.as_str(),
                    row.title.as_str(),
                    row.surface.as_str(),
                )
            })
            .collect();
        assert_eq!(
            shown,
            [
                ("client", "Frostbound", "Folder missing", "Diagnostics"),
                ("client", "Frostbound", "Review files", "Diagnostics"),
                ("server", "Pack server", "Best effort", "Overview"),
            ]
        );
        assert_eq!(rows[0].id.as_str(), broken.host.id());
        assert_eq!(rows[0].detail.as_str(), "Minecraft 1.20.1 | Forge");
    }

    #[test]
    fn a_failed_run_is_kept_for_a_listed_host_until_it_starts_again() {
        let crashed = client("Crashy", None);
        let host = crashed.host;
        let mut home = state(vec![crashed], Vec::new());
        home.note_run_failure(host, "Minecraft crashed".to_owned(), "exit 1".to_owned());

        let rows = home.view(now(), 9).attention;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title.as_str(), "Minecraft crashed");
        assert_eq!(rows[0].detail.as_str(), "exit 1");

        home.clear_run_failure(host);
        assert!(!home.has_run_failure(host));
        assert!(home.view(now(), 9).attention.is_empty());
    }

    #[test]
    fn a_newer_failure_replaces_the_one_a_host_had() {
        let crashed = client("Crashy", None);
        let host = crashed.host;
        let mut home = state(vec![crashed], Vec::new());
        home.note_run_failure(host, "First".to_owned(), "one".to_owned());
        home.note_run_failure(host, "Second".to_owned(), "two".to_owned());
        let rows = home.view(now(), 9).attention;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title.as_str(), "Second");
    }

    #[test]
    fn a_failure_of_a_host_that_is_gone_is_kept_but_not_listed() {
        let mut home = state(vec![client("Other", None)], Vec::new());
        let gone = InstalledHost::Instance(InstanceId::new());
        home.note_run_failure(gone, "Crashed".to_owned(), "exit 1".to_owned());
        assert!(home.has_run_failure(gone));
        assert!(home.view(now(), 9).attention.is_empty());
    }

    #[test]
    fn failure_text_is_cut_to_a_bounded_length() {
        let crashed = client("Crashy", None);
        let host = crashed.host;
        let mut home = state(vec![crashed], Vec::new());
        home.note_run_failure(host, "Crashed".to_owned(), "x".repeat(5_000));
        let rows = home.view(now(), 9).attention;
        assert!(rows[0].detail.chars().count() <= ATTENTION_TEXT_LIMIT);
        assert!(rows[0].detail.ends_with("..."));
    }

    #[test]
    fn continue_offers_the_four_newest_runs_of_hosts_that_are_not_running() {
        let hour = TimeDelta::hours(1);
        let mut running = client("running", Some(hour));
        running.run = RunState::Active;
        let clients = vec![
            client("oldest", Some(hour * 50)),
            client("never", None),
            running,
            client("two-hours", Some(hour * 2)),
            client("thirty-hours", Some(hour * 30)),
            client("five-hours", Some(hour * 5)),
        ];
        let servers = vec![server("srv", Some(hour * 3))];
        let rows = state(clients, servers).view(now(), 9).resume;
        let shown: Vec<(&str, &str)> = rows
            .iter()
            .map(|row| (row.host_name.as_str(), row.last_run.as_str()))
            .collect();
        assert_eq!(
            shown,
            [
                ("two-hours", "2 hours ago"),
                ("srv", "3 hours ago"),
                ("five-hours", "5 hours ago"),
                ("thirty-hours", "1 day ago"),
            ]
        );
        assert_eq!(rows[1].kind.as_str(), "server");
    }

    #[test]
    fn continue_carries_whether_the_host_can_launch() {
        let mut blocked = client("blocked", Some(TimeDelta::hours(1)));
        blocked.can_launch = false;
        let rows = state(vec![blocked], Vec::new()).view(now(), 9).resume;
        assert!(!rows[0].can_launch);
    }

    #[test]
    fn storage_counts_come_from_every_host_before_any_filter() {
        let mut persistent = client("p", None);
        persistent.persistent = true;
        let home = state(
            vec![client("t1", None), client("t2", None), persistent],
            vec![server("s1", None), server("s2", None)],
        );
        let view = home.view(now(), 9);
        assert_eq!((view.temporary, view.persistent, view.servers), (2, 1, 2));
    }

    #[test]
    fn home_is_due_after_a_change_and_again_when_a_minute_passes() {
        let mut home = HomeState::default();
        assert!(home.due(now()));
        let ui = test_window();
        home.publish(&ui, now(), 9);
        assert!(!home.due(now()));
        assert!(home.due(now() + TimeDelta::seconds(61)));
        home.set_building(2);
        assert!(home.due(now()));
        home.publish(&ui, now(), 9);
        home.set_building(2);
        assert!(!home.due(now()));
    }

    #[test]
    fn publishing_writes_every_home_property() {
        let ui = test_window();
        let mut broken = client("Frostbound", Some(TimeDelta::hours(2)));
        broken.attention = vec!["Folder missing".to_owned()];
        let mut home = state(vec![broken], vec![server("srv", None)]);
        home.set_default_account(Some("Quiet".to_owned()));
        home.set_building(1);
        home.publish(&ui, now(), 15);

        assert_eq!(ui.get_home_greeting(), "Good afternoon, Quiet");
        assert_eq!(ui.get_home_summary(), "1 problem | 1 building");
        assert_eq!(ui.get_home_attention().row_count(), 1);
        assert_eq!(ui.get_home_resume().row_count(), 1);
        assert_eq!(ui.get_home_temporary_count(), 1);
        assert_eq!(ui.get_home_persistent_count(), 0);
        assert_eq!(ui.get_home_server_count(), 1);
    }

    fn test_window() -> AppWindow {
        let _ =
            slint::platform::set_platform(Box::new(i_slint_backend_testing::TestingBackend::new(
                i_slint_backend_testing::TestingBackendOptions {
                    threading: false,
                    ..Default::default()
                },
            )));
        AppWindow::new().expect("window")
    }
}
