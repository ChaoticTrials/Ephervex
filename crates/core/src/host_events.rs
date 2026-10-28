//! The history of what happened to one instance or server (see
//! [`ephervex_storage::host_events`]).
//!
//! Recording is a convenience for whoever reads the history later, never a
//! reason for anything else to fail: a history that cannot be written is
//! logged and skipped.

use ephervex_storage::{EventHost, HostEvent, HostEventKind, append_host_event, read_host_events};

use crate::app::App;
use crate::mods::InstalledHost;

const fn event_host(host: InstalledHost) -> EventHost {
    match host {
        InstalledHost::Instance(id) => EventHost::Client(id),
        InstalledHost::Server(id) => EventHost::Server(id),
    }
}

/// What a creation event says about where the host lives.
pub(crate) const fn storage_event_subject(storage: ephervex_model::StorageClass) -> &'static str {
    match storage {
        ephervex_model::StorageClass::Temporary => "temporary",
        ephervex_model::StorageClass::Persistent => "persistent",
    }
}

/// How a client ended, as its event records it: an exit code, or `signal:`
/// and the signal that stopped it.
pub(crate) fn exit_event_subject(exit: &ephervex_minecraft::GameExit) -> String {
    exit.code.map_or_else(
        || {
            exit.signal
                .map_or_else(|| "unknown".to_owned(), |signal| format!("signal:{signal}"))
        },
        |code| code.to_string(),
    )
}

impl App {
    /// The host's history, oldest first.
    #[must_use]
    pub fn host_events(&self, host: InstalledHost) -> Vec<HostEvent> {
        read_host_events(self.paths(), event_host(host))
    }

    /// Adds how a client's game ended to its history, with the crash report
    /// that came with it when it did not end cleanly.
    pub(crate) fn record_client_exit(
        &self,
        instance: ephervex_model::InstanceId,
        exit: &ephervex_minecraft::GameExit,
    ) {
        let host = InstalledHost::Instance(instance);
        self.record_host_event(host, HostEventKind::Exited, &exit_event_subject(exit), "");
        if !exit.is_success() {
            self.record_crash_event(host);
        }
    }

    /// Adds one event to the host's history, now.
    pub fn record_host_event(
        &self,
        host: InstalledHost,
        kind: HostEventKind,
        subject: &str,
        detail: &str,
    ) {
        if let Err(error) = append_host_event(
            self.paths(),
            event_host(host),
            &HostEvent::now(kind, subject, detail),
        ) {
            tracing::warn!(%error, ?kind, "could not record a host event");
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use ephervex_model::InstanceId;

    use super::*;

    #[test]
    fn a_recorded_event_is_in_the_hosts_history() {
        let directory = tempfile::tempdir().expect("temp dir");
        let app = App::open_at(ephervex_storage::Paths::rooted_at(directory.path()))
            .expect("app should open against a temporary root");
        let host = InstalledHost::Instance(InstanceId::new());

        app.record_host_event(host, HostEventKind::Launched, "QuietBlock", "");

        let events = app.host_events(host);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, HostEventKind::Launched);
        assert_eq!(events[0].subject, "QuietBlock");
    }
}
