//! The one progress abstraction used by every long-running task.
//!
//! Importing a pack, resolving dependencies, downloading files, installing a
//! loader or a JVM, launching the game, and creating a server all report
//! through this type. The GUI renders it without knowing which provider is
//! behind it, and the CLI maps the same events to progress text.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::ids::OperationId;

/// Progress towards completion.
///
/// Total is optional because some stages genuinely cannot know their size in
/// advance - a chunked response without `Content-Length`, or a dependency graph
/// still being walked. Those render as indeterminate rather than as a fake
/// percentage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Progress {
    pub current: u64,
    pub total: Option<u64>,
}

impl Progress {
    #[must_use]
    pub const fn indeterminate() -> Self {
        Self {
            current: 0,
            total: None,
        }
    }

    #[must_use]
    pub const fn new(current: u64, total: u64) -> Self {
        Self {
            current,
            total: Some(total),
        }
    }

    /// Completion in the range `0.0..=1.0`, or `None` when indeterminate.
    #[must_use]
    pub fn fraction(&self) -> Option<f64> {
        match self.total {
            // A zero total means "nothing to do", which is complete, not a
            // division by zero.
            Some(0) => Some(1.0),
            #[allow(clippy::cast_precision_loss)]
            Some(total) => Some((self.current.min(total) as f64) / (total as f64)),
            None => None,
        }
    }
}

/// Lifecycle state of an operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "detail")]
pub enum OperationStatus {
    Pending,
    Running,
    /// Canceled at the user's request, not by an error.
    Canceled,
    Completed,
    /// Carries a message already suitable for display; the full technical
    /// detail stays in the logs under this operation's id.
    Failed(String),
}

impl OperationStatus {
    /// Whether the operation has reached a state it cannot leave.
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        matches!(self, Self::Canceled | Self::Completed | Self::Failed(_))
    }
}

/// A long-running task the user can watch and, where supported, cancel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Operation {
    pub id: OperationId,
    /// Message id resolved through the i18n layer by the frontend, never a
    /// pre-translated string baked into business logic.
    pub title: String,
    pub status: OperationStatus,
    pub progress: Progress,
    /// Which phase of a multi-stage operation is running, as a message id the
    /// frontend resolves. `None` for an operation that is one phase.
    ///
    /// Separate from [`Self::title`] because the title names the whole job and
    /// does not change, while this changes as the job moves through its work.
    /// Separate from [`Self::current_step`] because that is a value, usually a
    /// file name, and never something to translate.
    pub stage: Option<String>,
    /// How many of the current stage's units are done, where it counts in
    /// units at all.
    ///
    /// Beside [`Self::progress`] rather than instead of it: a download knows
    /// both how many files it has finished and how many bytes it has moved,
    /// and "34 of 210 files" answers a different question than a percentage.
    pub items: Progress,
    /// Human-readable current stage, e.g. a filename being downloaded.
    pub current_step: Option<String>,
    pub cancellable: bool,
}

impl Operation {
    #[must_use]
    pub fn new(title: impl Into<String>, cancellable: bool) -> Self {
        Self {
            id: OperationId::new(),
            title: title.into(),
            status: OperationStatus::Pending,
            progress: Progress::indeterminate(),
            stage: None,
            items: Progress::indeterminate(),
            current_step: None,
            cancellable,
        }
    }
}

impl fmt::Display for Operation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {} ({:?})", self.id, self.title, self.status)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn indeterminate_progress_has_no_fraction() {
        assert!(Progress::indeterminate().fraction().is_none());
    }

    #[test]
    fn fraction_is_computed_from_current_over_total() {
        let fraction = Progress::new(1, 4).fraction().expect("determinate");
        assert!((fraction - 0.25).abs() < f64::EPSILON);
    }

    #[test]
    fn a_zero_total_counts_as_complete_rather_than_dividing_by_zero() {
        assert_eq!(Progress::new(0, 0).fraction(), Some(1.0));
    }

    #[test]
    fn overshooting_current_is_clamped_to_complete() {
        assert_eq!(Progress::new(99, 10).fraction(), Some(1.0));
    }

    #[test]
    fn only_finished_states_are_terminal() {
        assert!(!OperationStatus::Pending.is_terminal());
        assert!(!OperationStatus::Running.is_terminal());
        assert!(OperationStatus::Canceled.is_terminal());
        assert!(OperationStatus::Failed("boom".to_owned()).is_terminal());
    }

    #[test]
    fn new_operations_start_pending_with_a_fresh_id() {
        let a = Operation::new("import-pack", true);
        let b = Operation::new("import-pack", true);
        assert_eq!(a.status, OperationStatus::Pending);
        assert_ne!(a.id, b.id);
    }
}
