//! How a host's history is told: one line per event, in the reader's words,
//! with the time it happened and a tone that says whether it was good news.
//!
//! The events carry kinds and short facts, never sentences, so what a line says
//! is decided here and localized once.

use chrono::{DateTime, Local, Utc};
use ephervex_core::{HostEvent, HostEventKind};
use ephervex_i18n::{Arg, lookup, lookup_args};

/// The newest events a history shows.
pub(crate) const SHOWN_EVENTS: usize = 60;

/// One line of a host's history as the window shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryRow {
    /// The time of day, with the date when it is not today.
    pub time: String,
    pub text: String,
    /// More text under the line, such as the lines of a log error.
    pub detail: String,
    /// "neutral", "good", "bad" or "notice".
    pub tone: &'static str,
}

fn with(id: &str, value: &str) -> String {
    lookup_args(id, [("value", Arg::from(value))])
}

/// How an exit was recorded: a code, or `signal:` and a signal.
enum Ending<'a> {
    Code(i32),
    Signal(&'a str),
    Unknown,
}

fn ending(subject: &str) -> Ending<'_> {
    if let Some(signal) = subject.strip_prefix("signal:") {
        Ending::Signal(signal)
    } else if let Ok(code) = subject.parse() {
        Ending::Code(code)
    } else {
        Ending::Unknown
    }
}

/// What one event says, and in what tone.
fn describe(event: &HostEvent) -> (String, &'static str) {
    let subject = event.subject.as_str();
    match event.kind {
        HostEventKind::Created => {
            let storage = if subject == "persistent" {
                lookup("gui-event-storage-persistent")
            } else {
                lookup("gui-event-storage-temporary")
            };
            (with("gui-event-created", &storage), "neutral")
        }
        HostEventKind::Launched => (with("gui-event-launched", subject), "good"),
        HostEventKind::Exited | HostEventKind::ServerStopped => {
            let client = event.kind == HostEventKind::Exited;
            match ending(subject) {
                Ending::Code(0) => (
                    lookup(if client {
                        "gui-event-exited-clean"
                    } else {
                        "gui-event-stopped-clean"
                    }),
                    "neutral",
                ),
                Ending::Code(code) => (
                    with(
                        if client {
                            "gui-event-exited-code"
                        } else {
                            "gui-event-stopped-code"
                        },
                        &code.to_string(),
                    ),
                    "bad",
                ),
                Ending::Signal(signal) => (
                    with(
                        if client {
                            "gui-event-exited-signal"
                        } else {
                            "gui-event-stopped-signal"
                        },
                        signal,
                    ),
                    "bad",
                ),
                Ending::Unknown => (
                    lookup(if client {
                        "gui-event-exited-unknown"
                    } else {
                        "gui-event-stopped-unknown"
                    }),
                    "notice",
                ),
            }
        }
        HostEventKind::ServerStarted => (with("gui-event-server-started", subject), "good"),
        HostEventKind::LogError => (with("gui-event-log-error", subject), "bad"),
        HostEventKind::Reproduced => (
            lookup_args(
                "gui-event-reproduced",
                [("count", Arg::Count(subject.parse().unwrap_or(1)))],
            ),
            "notice",
        ),
        _ => (lookup("gui-event-other"), "neutral"),
    }
}

fn time_of(at: DateTime<Utc>, now: DateTime<Local>) -> String {
    let local = at.with_timezone(&Local);
    if local.date_naive() == now.date_naive() {
        local.format("%H:%M").to_string()
    } else {
        local.format("%Y-%m-%d %H:%M").to_string()
    }
}

/// The newest [`SHOWN_EVENTS`] events, oldest first.
#[must_use]
pub(crate) fn rows(events: &[HostEvent], now: DateTime<Local>) -> Vec<HistoryRow> {
    let skip = events.len().saturating_sub(SHOWN_EVENTS);
    events[skip..]
        .iter()
        .map(|event| {
            let (text, tone) = describe(event);
            HistoryRow {
                time: time_of(event.at, now),
                text,
                detail: event.detail.clone(),
                tone,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use chrono::TimeZone as _;

    use super::*;

    fn event(kind: HostEventKind, subject: &str, detail: &str) -> HostEvent {
        HostEvent::at(Utc::now(), kind, subject, detail)
    }

    fn told(kind: HostEventKind, subject: &str) -> (String, &'static str) {
        describe(&event(kind, subject, ""))
    }

    #[test]
    fn a_clean_exit_is_neutral_and_a_failed_one_names_its_code_or_signal() {
        assert_eq!(
            told(HostEventKind::Exited, "0"),
            (lookup("gui-event-exited-clean"), "neutral")
        );
        assert_eq!(
            told(HostEventKind::Exited, "1"),
            (with("gui-event-exited-code", "1"), "bad")
        );
        assert_eq!(
            told(HostEventKind::ServerStopped, "signal:9"),
            (with("gui-event-stopped-signal", "9"), "bad")
        );
        assert_eq!(
            told(HostEventKind::ServerStopped, "garbage").1,
            "notice",
            "an ending nobody can read is not called a failure"
        );
    }

    #[test]
    fn a_creation_and_a_start_say_where_and_as_whom() {
        assert_eq!(
            told(HostEventKind::Created, "persistent"),
            (
                with("gui-event-created", &lookup("gui-event-storage-persistent")),
                "neutral"
            )
        );
        assert_eq!(
            told(HostEventKind::Launched, "QuietBlock"),
            (with("gui-event-launched", "QuietBlock"), "good")
        );
    }

    #[test]
    fn a_kind_this_build_does_not_know_is_still_a_line() {
        assert_eq!(
            told(HostEventKind::Other, ""),
            (lookup("gui-event-other"), "neutral")
        );
    }

    #[test]
    fn only_the_newest_events_are_shown_and_a_log_error_keeps_its_excerpt() {
        let events: Vec<HostEvent> = (0..SHOWN_EVENTS + 5)
            .map(|number| {
                event(
                    HostEventKind::LogError,
                    &number.to_string(),
                    "first line\nsecond line",
                )
            })
            .collect();

        let shown = rows(&events, Local::now());

        assert_eq!(shown.len(), SHOWN_EVENTS);
        assert_eq!(shown[0].text, with("gui-event-log-error", "5"));
        assert_eq!(shown[0].detail, "first line\nsecond line");
    }

    #[test]
    fn today_shows_a_time_of_day_and_another_day_shows_its_date() {
        let now = Local
            .with_ymd_and_hms(2026, 10, 25, 15, 0, 0)
            .single()
            .expect("a local time");
        let today = now.with_timezone(&Utc);
        let earlier = Local
            .with_ymd_and_hms(2026, 10, 22, 15, 0, 0)
            .single()
            .expect("a local time")
            .with_timezone(&Utc);

        assert_eq!(time_of(today, now), "15:00");
        assert_eq!(time_of(earlier, now), "2026-10-22 15:00");
    }
}
