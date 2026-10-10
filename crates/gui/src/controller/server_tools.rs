//! Server actions and transient status shared by every view of a server.

use std::collections::{HashMap, HashSet};

use ephervex_i18n::lookup;
use ephervex_model::ServerId;

use crate::backend::{ConsoleOutcome, ServerToolEvent};
use crate::ui::ServerRow;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ServerAction {
    Start,
    Stop,
    Kill,
    Console,
}

/// What a linked start claimed on its server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LinkedClaim {
    /// The server is stopped and this action starts a new run of it.
    Starting,
    /// The server is already running and this action joins that run.
    Running,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Starting,
    Stopping,
    Failed,
}

#[derive(Debug, Default)]
struct StopRequests {
    pending: u8,
    killing: bool,
    confirmed: bool,
    exit: Option<Result<(), String>>,
}

#[derive(Debug, Default)]
pub(super) struct ServerTools {
    // Raw snapshot values let completion restore controls without waiting for
    // another poll, including when a control request fails before starting.
    rows: HashMap<ServerId, ServerRow>,
    phases: HashMap<ServerId, Phase>,
    commands: HashSet<ServerId>,
    console_completions: HashMap<ServerId, (i32, bool)>,
    unavailable: HashSet<ServerId>,
    // Control completion and the run's exit can arrive in either order.
    intentional_stops: HashSet<ServerId>,
    stops: HashMap<ServerId, StopRequests>,
    // A control reply can precede the run's final event. Do not let a new
    // start receive that old run's completion as its own.
    active_runs: HashSet<ServerId>,
    observed_runs: HashSet<ServerId>,
    reported_failures: HashMap<ServerId, u64>,
    last_failure: u64,
}

impl ServerTools {
    pub(super) fn observe(&mut self, rows: &[ServerRow]) {
        self.rows = rows
            .iter()
            .filter_map(|row| crate::backend::parse_server_id(&row.id).map(|id| (id, row.clone())))
            .collect();
        self.phases.retain(|id, _| self.rows.contains_key(id));
    }

    pub(super) fn busy(&self, id: ServerId) -> bool {
        matches!(
            self.phases.get(&id),
            Some(Phase::Starting | Phase::Stopping)
        ) || self.commands.contains(&id)
            || self.stops.contains_key(&id)
    }

    pub(super) fn claim_linked(&mut self, id: ServerId, restarting: bool) -> Option<LinkedClaim> {
        if self.busy(id) || restarting {
            return None;
        }
        if self.rows.get(&id).is_some_and(|row| row.can_start) {
            self.request(id, ServerAction::Start, restarting)
                .then_some(LinkedClaim::Starting)
        } else {
            self.rows
                .get(&id)
                .is_some_and(|row| row.can_stop)
                .then_some(LinkedClaim::Running)
        }
    }

    pub(super) fn request(&mut self, id: ServerId, action: ServerAction, restarting: bool) -> bool {
        let Some(row) = self.rows.get(&id) else {
            return false;
        };
        let busy = matches!(
            self.phases.get(&id),
            Some(Phase::Starting | Phase::Stopping)
        );
        match action {
            ServerAction::Start
                if row.can_start
                    && !busy
                    && !restarting
                    && !self.commands.contains(&id)
                    && !self.active_runs.contains(&id)
                    && !self.stops.contains_key(&id) =>
            {
                self.intentional_stops.remove(&id);
                self.reported_failures.remove(&id);
                self.active_runs.insert(id);
                self.phases.insert(id, Phase::Starting);
            }
            ServerAction::Stop if row.can_stop && !busy && !self.stops.contains_key(&id) => {
                self.stops.entry(id).or_default().pending += 1;
                self.intentional_stops.insert(id);
                self.phases.insert(id, Phase::Stopping);
            }
            ServerAction::Kill
                if row.can_kill && !self.stops.get(&id).is_some_and(|stop| stop.killing) =>
            {
                let stop = self.stops.entry(id).or_default();
                stop.pending += 1;
                stop.killing = true;
                self.intentional_stops.insert(id);
                self.phases.insert(id, Phase::Stopping);
            }
            ServerAction::Console
                if row.can_send_command
                    && !busy
                    && !restarting
                    && !self.stops.contains_key(&id)
                    && !self.unavailable.contains(&id) =>
            {
                return self.commands.insert(id);
            }
            _ => return false,
        }
        true
    }

    pub(super) fn apply(&mut self, id: ServerId, event: ServerToolEvent) -> Option<String> {
        match event {
            ServerToolEvent::Started => {
                self.active_runs.insert(id);
                self.observed_runs.insert(id);
                self.unavailable.remove(&id);
                self.reported_failures.remove(&id);
                if matches!(self.phases.get(&id), Some(Phase::Starting | Phase::Failed)) {
                    self.phases.remove(&id);
                }
            }
            ServerToolEvent::StartAbandoned => {
                if !self.observed_runs.contains(&id)
                    && self.phases.get(&id) == Some(&Phase::Starting)
                {
                    self.phases.remove(&id);
                    self.active_runs.remove(&id);
                }
            }
            ServerToolEvent::RestartFailed { failure } => {
                // A failed replacement does not release the old run's guard.
                return self.fail(id, failure);
            }
            ServerToolEvent::Finished { failure } => {
                self.active_runs.remove(&id);
                self.observed_runs.remove(&id);
                if let Some(stop) = self.stops.get_mut(&id) {
                    stop.exit = Some(failure.map_or(Ok(()), Err));
                    return None;
                }
                let intentional = self.intentional_stops.remove(&id);
                if let Some(failure) = failure.filter(|_| !intentional) {
                    return self.fail(id, failure);
                }
                if self.phases.get(&id) != Some(&Phase::Failed) {
                    self.phases.remove(&id);
                }
            }
            ServerToolEvent::StopFinished { succeeded } => {
                if let Some(stop) = self.stops.get_mut(&id) {
                    stop.pending = stop.pending.saturating_sub(1);
                    stop.confirmed |= succeeded;
                    if stop.pending != 0 {
                        return None;
                    }
                    let failure = if stop.confirmed {
                        None
                    } else {
                        stop.exit
                            .as_ref()
                            .and_then(|exit| exit.as_ref().err())
                            .cloned()
                    };
                    if !stop.confirmed || stop.exit.is_some() {
                        self.intentional_stops.remove(&id);
                    }
                    self.stops.remove(&id);
                    if let Some(failure) = failure {
                        return self.fail(id, failure);
                    }
                }
                if self.phases.get(&id) == Some(&Phase::Stopping) {
                    self.phases.remove(&id);
                }
            }
            ServerToolEvent::ConsoleFinished { outcome } => {
                if self.commands.remove(&id) {
                    let completion = self.console_completions.entry(id).or_default();
                    completion.0 = completion.0.wrapping_add(1);
                    completion.1 = outcome == ConsoleOutcome::Delivered;
                }
                if outcome == ConsoleOutcome::Unavailable {
                    self.unavailable.insert(id);
                }
            }
        }
        None
    }

    fn fail(&mut self, id: ServerId, failure: String) -> Option<String> {
        self.phases.insert(id, Phase::Failed);
        if self.reported_failures.contains_key(&id) {
            return None;
        }
        self.last_failure = self.last_failure.wrapping_add(1);
        self.reported_failures.insert(id, self.last_failure);
        Some(failure)
    }

    pub(super) fn failure_revision(&self, id: ServerId) -> Option<u64> {
        self.reported_failures.get(&id).copied()
    }

    pub(super) fn stamp(&self, id: ServerId, row: &mut ServerRow, restarting: bool) {
        (row.console_completion, row.console_delivered) = self
            .console_completions
            .get(&id)
            .copied()
            .unwrap_or_default();
        if let Some(base) = self.rows.get(&id) {
            row.state = base.state.clone();
            row.state_kind = base.state_kind.clone();
            row.state_detail = base.state_detail.clone();
            row.can_start = base.can_start;
            row.can_stop = base.can_stop;
            row.can_restart = base.can_restart;
            row.can_kill = base.can_kill;
            row.can_send_command = base.can_send_command;
        }
        let phase = self.phases.get(&id);
        let state = match phase {
            Some(Phase::Stopping) => Some((
                "stopping",
                "gui-state-stopping",
                "gui-server-stopping-detail",
            )),
            _ if restarting => Some((
                "restarting",
                "gui-state-restarting",
                "gui-server-restarting-detail",
            )),
            Some(Phase::Starting) => Some((
                "starting",
                "gui-state-starting",
                "gui-state-starting-detail",
            )),
            Some(Phase::Failed) if row.state_kind == "stopped" => {
                Some(("failed", "gui-state-failed", "gui-server-failed-detail"))
            }
            _ => None,
        };
        if let Some((kind, label, detail)) = state {
            row.state_kind = kind.into();
            row.state = lookup(label).into();
            row.state_detail = lookup(detail).into();
        }
        let busy = matches!(phase, Some(Phase::Starting | Phase::Stopping));
        if busy || restarting {
            row.can_start = false;
            row.can_restart = false;
            row.can_send_command = false;
        }
        if busy {
            row.can_stop = false;
        }
        if self.active_runs.contains(&id) {
            row.can_start = false;
        }
        if self.stops.contains_key(&id) {
            row.can_start = false;
            row.can_stop = false;
            row.can_restart = false;
            row.can_send_command = false;
            if self.stops.get(&id).is_some_and(|stop| stop.killing) {
                row.can_kill = false;
            }
        }
        if self.commands.contains(&id) {
            row.can_start = false;
            row.can_restart = false;
        }
        if self.commands.contains(&id) || self.unavailable.contains(&id) {
            row.can_send_command = false;
        }
        row.console_status = if self.unavailable.contains(&id) {
            lookup("gui-server-console-channel-lost")
        } else if busy || restarting || self.commands.contains(&id) {
            lookup("gui-server-console-busy")
        } else if !row.can_send_command {
            lookup("gui-server-console-unavailable")
        } else {
            String::new()
        }
        .into();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: ServerId, running: bool) -> ServerRow {
        ServerRow {
            id: id.to_string().into(),
            state: if running { "Running" } else { "Stopped" }.into(),
            state_kind: if running { "running" } else { "stopped" }.into(),
            can_start: !running,
            can_stop: running,
            can_restart: running,
            can_kill: running,
            can_send_command: running,
            ..ServerRow::default()
        }
    }

    #[test]
    fn a_new_run_invalidates_pending_failure_diagnostics() {
        let id = ServerId::new();
        let mut tools = ServerTools::default();
        tools.observe(&[row(id, false)]);
        assert!(
            tools
                .apply(
                    id,
                    ServerToolEvent::Finished {
                        failure: Some("crashed".to_owned())
                    }
                )
                .is_some()
        );
        let old = tools.failure_revision(id);
        assert!(old.is_some());
        assert!(tools.request(id, ServerAction::Start, false));
        assert_eq!(tools.failure_revision(id), None);
        tools.apply(id, ServerToolEvent::Started);
        assert!(
            tools
                .apply(
                    id,
                    ServerToolEvent::Finished {
                        failure: Some("another crash".to_owned())
                    }
                )
                .is_some()
        );
        assert_ne!(tools.failure_revision(id), old);
    }

    #[test]
    fn only_unconfirmed_or_unrequested_failures_reach_diagnostics() {
        for exit_first in [false, true] {
            for successful in [false, true] {
                let id = ServerId::new();
                let mut tools = ServerTools::default();
                tools.observe(&[row(id, true)]);
                assert!(tools.request(id, ServerAction::Stop, false));
                assert!(tools.request(id, ServerAction::Kill, false));
                let exit = ServerToolEvent::Finished {
                    failure: Some("crashed".to_owned()),
                };
                let stop = ServerToolEvent::StopFinished { succeeded: false };
                let kill = ServerToolEvent::StopFinished {
                    succeeded: successful,
                };
                let events = if exit_first {
                    [exit, stop, kill]
                } else {
                    [stop, kill, exit]
                };
                let failures: Vec<_> = events
                    .into_iter()
                    .filter_map(|event| tools.apply(id, event))
                    .collect();
                assert_eq!(
                    failures,
                    if successful {
                        Vec::<String>::new()
                    } else {
                        vec!["crashed".to_owned()]
                    }
                );
            }
        }
        let id = ServerId::new();
        let mut tools = ServerTools::default();
        assert_eq!(
            tools.apply(
                id,
                ServerToolEvent::Finished {
                    failure: Some("unexpected".to_owned())
                }
            ),
            Some("unexpected".to_owned())
        );
    }

    #[test]
    fn console_receipts_distinguish_delivery_from_failure_and_remain_host_wide() {
        let id = ServerId::new();
        let other = ServerId::new();
        let mut tools = ServerTools::default();
        tools.observe(&[row(id, true), row(other, true)]);
        for (index, outcome) in [
            ConsoleOutcome::Delivered,
            ConsoleOutcome::Rejected,
            ConsoleOutcome::Unavailable,
        ]
        .into_iter()
        .enumerate()
        {
            assert!(tools.request(id, ServerAction::Console, false));
            assert!(!tools.request(id, ServerAction::Console, false));
            tools.apply(id, ServerToolEvent::ConsoleFinished { outcome });
            let mut a = row(id, true);
            let mut b = row(id, true);
            let mut independent = row(other, true);
            tools.stamp(id, &mut a, false);
            tools.stamp(id, &mut b, false);
            tools.stamp(other, &mut independent, false);
            assert_eq!(a, b);
            assert_eq!(usize::try_from(a.console_completion).ok(), Some(index + 1));
            assert_eq!(a.console_delivered, outcome == ConsoleOutcome::Delivered);
            assert_eq!(a.can_send_command, outcome != ConsoleOutcome::Unavailable);
            assert_eq!(independent.console_completion, 0);
            assert!(!independent.console_delivered);
            assert!(independent.can_send_command);
        }
    }

    #[test]
    fn server_actions_share_busy_state_and_restore_controls_on_completion() {
        let id = ServerId::new();
        let other = ServerId::new();
        let mut tools = ServerTools::default();
        tools.observe(&[row(id, false), row(other, true)]);
        assert!(!tools.request(id, ServerAction::Console, false));
        assert!(!tools.request(id, ServerAction::Stop, false));
        assert!(tools.request(id, ServerAction::Start, false));
        assert!(!tools.request(id, ServerAction::Start, false));
        assert!(!tools.request(id, ServerAction::Stop, false));
        let mut a = row(id, false);
        let mut b = row(id, false);
        tools.stamp(id, &mut a, false);
        tools.stamp(id, &mut b, false);
        assert_eq!(a, b);
        assert_eq!(a.state_kind, "starting");
        assert!(!a.can_start && !a.can_send_command);
        assert!(tools.request(other, ServerAction::Console, false));
        tools.apply(
            id,
            ServerToolEvent::Finished {
                failure: Some("failed".to_owned()),
            },
        );
        tools.stamp(id, &mut a, false);
        assert_eq!(a.state_kind, "failed");
        assert!(a.can_start);
        assert!(tools.request(id, ServerAction::Start, false));
        tools.observe(&[row(id, true), row(other, true)]);
        tools.apply(id, ServerToolEvent::Started);
        tools.stamp(id, &mut a, false);
        assert_eq!(a.state_kind, "running");
        assert!(a.can_send_command);
        assert!(tools.request(id, ServerAction::Console, false));
        assert!(!tools.request(id, ServerAction::Console, false));
        tools.stamp(id, &mut a, false);
        assert!(!a.can_send_command);
        // A refused command and a delivered command both settle their request.
        tools.apply(
            id,
            ServerToolEvent::ConsoleFinished {
                outcome: ConsoleOutcome::Delivered,
            },
        );
        tools.stamp(id, &mut a, false);
        assert!(a.can_send_command);
        assert!(tools.request(id, ServerAction::Stop, false));
        tools.stamp(id, &mut a, false);
        assert_eq!(a.state_kind, "stopping");
        assert!(!a.can_stop && !a.can_restart && !a.can_send_command);
        tools.apply(id, ServerToolEvent::StopFinished { succeeded: false });
        tools.stamp(id, &mut a, false);
        assert!(
            a.can_stop && a.can_send_command,
            "a failed stop releases busy state"
        );
    }

    #[test]
    fn a_restart_blocks_commands_but_allows_an_explicit_stop() {
        let id = ServerId::new();
        let mut tools = ServerTools::default();
        tools.observe(&[row(id, true)]);
        let mut shown = row(id, true);
        tools.stamp(id, &mut shown, true);
        assert_eq!(shown.state_kind, "restarting");
        assert!(!shown.can_restart && !shown.can_send_command);
        assert!(shown.can_stop);
        assert!(!tools.request(id, ServerAction::Console, true));
        assert!(tools.request(id, ServerAction::Stop, true));
        tools.apply(id, ServerToolEvent::Started);
        tools.stamp(id, &mut shown, true);
        assert_eq!(
            shown.state_kind, "stopping",
            "late start events cannot clear a stop"
        );
        tools.observe(&[row(id, false)]);
        tools.apply(
            id,
            ServerToolEvent::Finished {
                failure: Some("failed".to_owned()),
            },
        );
        tools.apply(id, ServerToolEvent::StopFinished { succeeded: true });
        tools.stamp(id, &mut shown, false);
        assert_eq!(
            shown.state_kind, "stopped",
            "an intentional stop is not a crash"
        );
        assert!(shown.can_start);
    }
    #[test]
    fn failed_input_stays_unavailable_across_snapshots_until_a_new_child_starts() {
        let id = ServerId::new();
        let mut tools = ServerTools::default();
        tools.observe(&[row(id, true)]);
        assert!(tools.request(id, ServerAction::Console, false));
        tools.apply(
            id,
            ServerToolEvent::ConsoleFinished {
                outcome: ConsoleOutcome::Unavailable,
            },
        );
        tools.observe(&[row(id, true)]);
        assert!(!tools.request(id, ServerAction::Console, false));
        let mut shown = row(id, true);
        tools.stamp(id, &mut shown, false);
        assert!(!shown.can_send_command);
        assert_eq!(
            shown.console_status,
            lookup("gui-server-console-channel-lost")
        );
        assert!(shown.can_stop && shown.can_restart && shown.can_kill);
        tools.apply(id, ServerToolEvent::Started);
        tools.stamp(id, &mut shown, false);
        assert!(shown.can_send_command);
        assert!(shown.console_status.is_empty());
    }
    #[test]
    fn a_delayed_exit_after_successful_kill_is_not_reported_as_a_crash() {
        let id = ServerId::new();
        let mut tools = ServerTools::default();
        tools.observe(&[row(id, true)]);
        assert!(tools.request(id, ServerAction::Kill, false));
        tools.observe(&[row(id, false)]);
        tools.apply(id, ServerToolEvent::StopFinished { succeeded: true });
        tools.apply(
            id,
            ServerToolEvent::Finished {
                failure: Some("failed".to_owned()),
            },
        );
        let mut shown = row(id, false);
        tools.stamp(id, &mut shown, false);
        assert_eq!(shown.state_kind, "stopped");
        assert!(shown.can_start);
    }
    #[test]
    fn a_new_start_waits_for_the_previous_runs_exit_event() {
        let id = ServerId::new();
        let mut tools = ServerTools::default();
        tools.observe(&[row(id, false)]);
        assert!(tools.request(id, ServerAction::Start, false));
        tools.observe(&[row(id, true)]);
        tools.apply(id, ServerToolEvent::Started);
        assert!(tools.request(id, ServerAction::Kill, false));
        assert!(!tools.request(id, ServerAction::Kill, false));
        tools.observe(&[row(id, false)]);
        tools.apply(id, ServerToolEvent::StopFinished { succeeded: true });
        assert!(!tools.request(id, ServerAction::Start, false));
        tools.apply(
            id,
            ServerToolEvent::Finished {
                failure: Some("failed".to_owned()),
            },
        );
        assert!(tools.request(id, ServerAction::Start, false));
        tools.apply(
            id,
            ServerToolEvent::Finished {
                failure: Some("failed".to_owned()),
            },
        );
        let mut shown = row(id, false);
        tools.stamp(id, &mut shown, false);
        assert_eq!(
            shown.state_kind, "failed",
            "the next run's genuine failure is still reported"
        );
    }

    #[test]
    fn a_failed_restart_preserves_the_run_guard_until_supervision_finishes() {
        for exit_first in [false, true] {
            let id = ServerId::new();
            let mut tools = ServerTools::default();
            tools.observe(&[row(id, false)]);
            assert!(tools.request(id, ServerAction::Start, false));
            tools.observe(&[row(id, true)]);
            tools.apply(id, ServerToolEvent::Started);
            tools.observe(&[row(id, false)]);
            let exit = ServerToolEvent::Finished { failure: None };
            if exit_first {
                tools.apply(id, exit.clone());
            }
            tools.apply(
                id,
                ServerToolEvent::RestartFailed {
                    failure: "replacement failed".to_owned(),
                },
            );
            if !exit_first {
                assert!(!tools.request(id, ServerAction::Start, false));
                tools.apply(id, exit.clone());
            }
            let mut shown = row(id, false);
            tools.stamp(id, &mut shown, false);
            assert_eq!(shown.state_kind, "failed");
            assert!(shown.can_start);
            assert!(tools.request(id, ServerAction::Start, false));
        }
    }

    #[test]
    fn a_failed_stop_reply_cannot_erase_a_successful_kill() {
        for kill_first in [false, true] {
            let id = ServerId::new();
            let mut tools = ServerTools::default();
            tools.observe(&[row(id, true)]);
            assert!(tools.request(id, ServerAction::Stop, false));
            assert!(tools.request(id, ServerAction::Kill, false));
            assert!(!tools.request(id, ServerAction::Kill, false));
            for succeeded in [kill_first, !kill_first] {
                tools.apply(id, ServerToolEvent::StopFinished { succeeded });
            }
            tools.observe(&[row(id, false)]);
            tools.apply(
                id,
                ServerToolEvent::Finished {
                    failure: Some("failed".to_owned()),
                },
            );
            let mut shown = row(id, false);
            tools.stamp(id, &mut shown, false);
            assert_eq!(shown.state_kind, "stopped");
            assert!(shown.can_start);
        }
    }
    #[test]
    fn a_refused_stop_does_not_hide_a_concurrent_unexpected_exit() {
        for exit_first in [false, true] {
            let id = ServerId::new();
            let mut tools = ServerTools::default();
            tools.observe(&[row(id, true)]);
            assert!(tools.request(id, ServerAction::Stop, false));
            tools.observe(&[row(id, false)]);
            let exit = ServerToolEvent::Finished {
                failure: Some("failed".to_owned()),
            };
            let refused = ServerToolEvent::StopFinished { succeeded: false };
            for event in if exit_first {
                [exit, refused]
            } else {
                [refused, exit]
            } {
                tools.apply(id, event);
            }
            let mut shown = row(id, false);
            tools.stamp(id, &mut shown, false);
            assert_eq!(shown.state_kind, "failed");
            assert!(shown.can_start);
        }
    }
}
