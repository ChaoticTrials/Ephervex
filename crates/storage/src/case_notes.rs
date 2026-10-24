//! What the user writes down while reproducing a bug on one instance: the
//! steps, a note, and how often it reproduced (ADR 0048).
//!
//! The text is the user's own and the file may be edited by hand, so every
//! bound is enforced when a value is stored and again when the file is read.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The most steps one instance keeps.
pub const MAX_CASE_STEPS: usize = 100;
/// The longest step, in characters.
pub const MAX_CASE_STEP_CHARS: usize = 300;
/// The longest note, in characters.
pub const MAX_CASE_NOTE_CHARS: usize = 8000;
/// The highest step id, so every id fits the window's integers.
const MAX_CASE_STEP_ID: u32 = 2_147_483_647;

/// One thing to do to reproduce the bug.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaseStep {
    /// Names the step across edits; never reused while the step exists.
    pub id: u32,
    pub text: String,
    #[serde(default)]
    pub done: bool,
    /// Fields a newer build wrote, kept so this build never erases them.
    #[serde(flatten)]
    unknown: BTreeMap<String, serde_json::Value>,
}

/// Why a step or note was refused. Each is something the user can fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CaseRefusal {
    /// The step has no text.
    StepEmpty,
    /// The step is longer than [`MAX_CASE_STEP_CHARS`].
    StepTooLong,
    /// The instance already has [`MAX_CASE_STEPS`] steps.
    TooManySteps,
    /// The note is longer than [`MAX_CASE_NOTE_CHARS`].
    NoteTooLong,
}

impl std::fmt::Display for CaseRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::StepEmpty => "the step has no text",
            Self::StepTooLong => "the step is too long",
            Self::TooManySteps => "there are too many steps",
            Self::NoteTooLong => "the note is too long",
        })
    }
}

/// The case for one instance.
///
/// Read leniently: a step or field of the wrong type costs that step or field,
/// never the case or the file around it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "StoredCase")]
pub struct CaseNotes {
    pub steps: Vec<CaseStep>,
    pub note: String,
    /// How many times the bug was seen again.
    pub reproduced: u32,
    /// Fields a newer build wrote, kept so this build never erases them.
    #[serde(flatten)]
    unknown: BTreeMap<String, serde_json::Value>,
}

/// A case as the file holds it, before anything is checked.
#[derive(Deserialize)]
struct StoredCase {
    #[serde(default)]
    steps: serde_json::Value,
    #[serde(default)]
    note: serde_json::Value,
    #[serde(default)]
    reproduced: serde_json::Value,
    #[serde(flatten)]
    unknown: BTreeMap<String, serde_json::Value>,
}

impl From<StoredCase> for CaseNotes {
    fn from(stored: StoredCase) -> Self {
        let steps = match stored.steps {
            serde_json::Value::Array(items) => items
                .into_iter()
                .filter_map(|item| serde_json::from_value::<CaseStep>(item).ok())
                .collect(),
            _ => Vec::new(),
        };
        let note = match stored.note {
            serde_json::Value::String(text) => text,
            _ => String::new(),
        };
        let reproduced = stored
            .reproduced
            .as_u64()
            .and_then(|count| u32::try_from(count).ok())
            .unwrap_or(0);
        Self {
            steps,
            note,
            reproduced,
            unknown: stored.unknown,
        }
    }
}

/// The text of a step: control characters dropped, the ends trimmed.
fn clean_step(text: &str) -> String {
    text.chars()
        .filter(|character| !character.is_control())
        .collect::<String>()
        .trim()
        .to_string()
}

/// A note keeps its line breaks and tabs and drops every other control
/// character.
fn clean_note(text: &str) -> String {
    text.chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\t'))
        .collect()
}

impl CaseNotes {
    /// Whether nothing is written down, so nothing needs to be stored.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty() && self.note.is_empty() && self.reproduced == 0
    }

    /// Adds a step at the end and returns its id.
    pub fn add_step(&mut self, text: &str) -> Result<u32, CaseRefusal> {
        let text = clean_step(text);
        if text.is_empty() {
            return Err(CaseRefusal::StepEmpty);
        }
        if text.chars().count() > MAX_CASE_STEP_CHARS {
            return Err(CaseRefusal::StepTooLong);
        }
        if self.steps.len() >= MAX_CASE_STEPS {
            return Err(CaseRefusal::TooManySteps);
        }
        let next = self
            .steps
            .iter()
            .map(|step| step.id)
            .max()
            .map_or(1, |highest| highest.saturating_add(1));
        let id = if next > MAX_CASE_STEP_ID {
            self.renumber();
            u32::try_from(self.steps.len() + 1).unwrap_or(1)
        } else {
            next
        };
        self.steps.push(CaseStep {
            id,
            text,
            done: false,
            unknown: BTreeMap::new(),
        });
        Ok(id)
    }

    /// Marks a step done or not done. False when no step has this id.
    pub fn set_step_done(&mut self, id: u32, done: bool) -> bool {
        self.steps
            .iter_mut()
            .find(|step| step.id == id)
            .map(|step| step.done = done)
            .is_some()
    }

    /// Removes a step. False when no step has this id.
    pub fn remove_step(&mut self, id: u32) -> bool {
        let before = self.steps.len();
        self.steps.retain(|step| step.id != id);
        self.steps.len() != before
    }

    /// Replaces the note.
    pub fn set_note(&mut self, note: &str) -> Result<(), CaseRefusal> {
        let note = clean_note(note);
        if note.chars().count() > MAX_CASE_NOTE_CHARS {
            return Err(CaseRefusal::NoteTooLong);
        }
        self.note = note;
        Ok(())
    }

    /// Counts one more time the bug was seen.
    pub fn count_reproduced(&mut self) {
        self.reproduced = self.reproduced.saturating_add(1);
    }

    /// Starts the count again.
    pub fn reset_reproduced(&mut self) {
        self.reproduced = 0;
    }

    /// Numbers the steps 1, 2, 3 in their order again.
    fn renumber(&mut self) {
        for (index, step) in self.steps.iter_mut().enumerate() {
            step.id = u32::try_from(index + 1).unwrap_or(MAX_CASE_STEP_ID);
        }
    }

    /// Brings what a hand-edited file holds back within the bounds: surplus
    /// steps and repeated ids are dropped, long text is cut, and ids too high
    /// for the window are numbered again.
    pub(crate) fn repaired(mut self) -> Self {
        let mut seen = std::collections::BTreeSet::new();
        self.steps.retain_mut(|step| {
            step.text = clean_step(&step.text);
            step.text = step.text.chars().take(MAX_CASE_STEP_CHARS).collect();
            !step.text.is_empty() && seen.insert(step.id)
        });
        self.steps.truncate(MAX_CASE_STEPS);
        if self.steps.iter().any(|step| step.id > MAX_CASE_STEP_ID) {
            self.renumber();
        }
        self.note = clean_note(&self.note)
            .chars()
            .take(MAX_CASE_NOTE_CHARS)
            .collect();
        self
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing)]

    use super::*;

    #[test]
    fn steps_get_increasing_ids_that_are_not_reused_after_a_removal() {
        let mut case = CaseNotes::default();
        let first = case.add_step("Place a belt").expect("first");
        let second = case.add_step("Feed the splitter").expect("second");
        assert!(case.remove_step(second));
        let third = case.add_step("Fill the belt").expect("third");
        assert_eq!((first, second, third), (1, 2, 2));
        assert!(case.remove_step(first));
        assert_eq!(case.add_step("Again").expect("fourth"), 3);
    }

    #[test]
    fn a_step_is_trimmed_and_refused_when_empty_or_too_long() {
        let mut case = CaseNotes::default();
        assert_eq!(case.add_step("  \t \n"), Err(CaseRefusal::StepEmpty));
        assert_eq!(
            case.add_step(&"x".repeat(MAX_CASE_STEP_CHARS + 1)),
            Err(CaseRefusal::StepTooLong)
        );
        case.add_step("  Open the\nworld  ").expect("kept");
        assert_eq!(case.steps[0].text, "Open theworld");
    }

    #[test]
    fn the_step_count_is_bounded() {
        let mut case = CaseNotes::default();
        for number in 0..MAX_CASE_STEPS {
            case.add_step(&format!("step {number}")).expect("room");
        }
        assert_eq!(case.add_step("one more"), Err(CaseRefusal::TooManySteps));
    }

    #[test]
    fn a_step_can_be_checked_and_unchecked_by_id() {
        let mut case = CaseNotes::default();
        let id = case.add_step("Do it").expect("step");
        assert!(case.set_step_done(id, true));
        assert!(case.steps[0].done);
        assert!(case.set_step_done(id, false));
        assert!(!case.set_step_done(id + 1, true));
        assert!(!case.steps[0].done);
    }

    #[test]
    fn a_note_keeps_its_line_breaks_and_is_bounded() {
        let mut case = CaseNotes::default();
        case.set_note("one\ntwo\u{7}\tthree").expect("note");
        assert_eq!(case.note, "one\ntwo\tthree");
        assert_eq!(
            case.set_note(&"x".repeat(MAX_CASE_NOTE_CHARS + 1)),
            Err(CaseRefusal::NoteTooLong)
        );
        assert_eq!(case.note, "one\ntwo\tthree");
    }

    #[test]
    fn the_reproduced_count_saturates_and_resets() {
        let mut case = CaseNotes {
            reproduced: u32::MAX,
            ..CaseNotes::default()
        };
        case.count_reproduced();
        assert_eq!(case.reproduced, u32::MAX);
        case.reset_reproduced();
        assert_eq!(case.reproduced, 0);
        assert!(case.is_empty());
    }

    #[test]
    fn a_hand_edited_case_is_brought_back_within_bounds() {
        let steps = (0..u32::try_from(MAX_CASE_STEPS).expect("small") + 20)
            .map(|id| CaseStep {
                id: id % 50,
                text: format!("step {id}\u{0}"),
                done: false,
                unknown: BTreeMap::new(),
            })
            .chain([CaseStep {
                id: 900,
                text: "   ".into(),
                done: true,
                unknown: BTreeMap::new(),
            }])
            .collect();
        let case = CaseNotes {
            steps,
            note: "n".repeat(MAX_CASE_NOTE_CHARS + 5),
            reproduced: 2,
            ..CaseNotes::default()
        }
        .repaired();
        assert!(case.steps.len() <= 50, "repeated ids collapse to one step");
        assert!(case.steps.iter().all(|step| !step.text.is_empty()));
        assert!(case.steps.iter().all(|step| !step.text.contains('\u{0}')));
        assert_eq!(case.note.chars().count(), MAX_CASE_NOTE_CHARS);
        assert_eq!(case.reproduced, 2);
    }

    #[test]
    fn a_wrongly_typed_field_costs_that_field_and_not_the_case() {
        let case: CaseNotes = serde_json::from_str(
            r#"{"steps":[{"id":1,"text":"ok"},{"id":2,"text":null},{"id":4294967296,"text":"big"},"x"],"note":5,"reproduced":-1}"#,
        )
        .expect("lenient");
        assert_eq!(case.steps.len(), 1);
        assert_eq!(case.steps[0].text, "ok");
        assert_eq!(case.note, "");
        assert_eq!(case.reproduced, 0);
        let case: CaseNotes = serde_json::from_str(r#"{"steps":"x"}"#).expect("lenient");
        assert!(case.is_empty());
    }

    #[test]
    fn fields_a_newer_build_wrote_survive_a_read_and_write() {
        let text = r#"{"steps":[{"id":1,"text":"ok","done":true,"color":"red"}],"note":"n","reproduced":1,"mood":"calm"}"#;
        let case: CaseNotes = serde_json::from_str(text).expect("read");
        let written = serde_json::to_string(&case).expect("write");
        let again: serde_json::Value = serde_json::from_str(&written).expect("json");
        assert_eq!(again["mood"], "calm");
        assert_eq!(again["steps"][0]["color"], "red");
    }

    #[test]
    fn ids_too_high_for_the_window_are_numbered_again_on_read_and_on_add() {
        let case: CaseNotes =
            serde_json::from_str(r#"{"steps":[{"id":3000000000,"text":"a"},{"id":7,"text":"b"}]}"#)
                .expect("read");
        let mut case = case.repaired();
        assert_eq!(
            case.steps.iter().map(|step| step.id).collect::<Vec<_>>(),
            [1, 2]
        );
        case.steps[1].id = MAX_CASE_STEP_ID;
        let id = case.add_step("c").expect("room after renumbering");
        assert_eq!(id, 3);
        assert!(case.steps.iter().all(|step| step.id <= MAX_CASE_STEP_ID));
    }
}
