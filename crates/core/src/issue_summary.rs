//! The text an instance contributes to a bug report: what it runs and what it
//! takes to reproduce the bug.
//!
//! It is plain text meant to be pasted into an issue, so it carries no paths
//! or other detail from the user's machine beyond what the user wrote in the
//! case.

use std::fmt::Write as _;

use crate::CaseNotes;

/// What the summary says about an instance.
#[derive(Debug, Clone, Copy)]
pub struct IssueSummaryFacts<'a> {
    pub name: &'a str,
    /// The issue the instance reproduces, empty when none is set.
    pub issue_link: &'a str,
    pub minecraft: &'a str,
    pub loader: &'a str,
    pub mods: usize,
    /// How the instance is stored, as the user reads it.
    pub storage: &'a str,
    pub linked_servers: usize,
    /// The linked server that Launch and Connect uses, empty when none is.
    pub paired_server: &'a str,
    pub case: &'a CaseNotes,
}

/// The summary of an instance, ready to paste into an issue.
#[must_use]
pub fn issue_summary(facts: &IssueSummaryFacts<'_>) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{}", facts.name);
    if !facts.issue_link.is_empty() {
        let _ = writeln!(out, "Issue: {}", facts.issue_link);
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "Minecraft: {}", facts.minecraft);
    let _ = writeln!(out, "Loader: {}", facts.loader);
    let _ = writeln!(out, "Mods: {}", facts.mods);
    let _ = writeln!(out, "Storage: {}", facts.storage);
    if !facts.paired_server.is_empty() {
        let _ = writeln!(out, "Linked server: {}", facts.paired_server);
    } else if facts.linked_servers > 0 {
        let _ = writeln!(out, "Linked servers: {}", facts.linked_servers);
    }
    if !facts.case.steps.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "Steps:");
        for (index, step) in facts.case.steps.iter().enumerate() {
            let mark = if step.done { "x" } else { " " };
            let _ = writeln!(out, "{}. [{mark}] {}", index + 1, step.text);
        }
    }
    if facts.case.reproduced > 0 {
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "Seen again {} time{}.",
            facts.case.reproduced,
            if facts.case.reproduced == 1 { "" } else { "s" }
        );
    }
    if !facts.case.note.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "Note:");
        let _ = writeln!(out, "{}", facts.case.note);
    }
    out.truncate(out.trim_end().len());
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::*;

    fn facts(case: &CaseNotes) -> IssueSummaryFacts<'_> {
        IssueSummaryFacts {
            name: "Conveyor dupe",
            issue_link: "",
            minecraft: "1.20.1",
            loader: "Forge 47.3.0",
            mods: 38,
            storage: "Temporary",
            linked_servers: 0,
            paired_server: "",
            case,
        }
    }

    #[test]
    fn an_instance_with_no_case_states_only_what_it_runs() {
        let case = CaseNotes::default();
        assert_eq!(
            issue_summary(&facts(&case)),
            "Conveyor dupe\n\nMinecraft: 1.20.1\nLoader: Forge 47.3.0\nMods: 38\nStorage: Temporary"
        );
    }

    #[test]
    fn the_case_adds_its_link_steps_count_and_note() {
        let mut case = CaseNotes::default();
        case.add_step("Place a belt").expect("step");
        case.add_step("Fill it").expect("step");
        case.set_step_done(1, true);
        case.count_reproduced();
        case.count_reproduced();
        case.set_note("Only when full").expect("note");
        let mut summary = facts(&case);
        summary.issue_link = "https://example.com/issues/7";
        summary.paired_server = "conveyor-srv";
        summary.linked_servers = 2;

        assert_eq!(
            issue_summary(&summary),
            "Conveyor dupe\nIssue: https://example.com/issues/7\n\n\
             Minecraft: 1.20.1\nLoader: Forge 47.3.0\nMods: 38\nStorage: Temporary\n\
             Linked server: conveyor-srv\n\n\
             Steps:\n1. [x] Place a belt\n2. [ ] Fill it\n\n\
             Seen again 2 times.\n\nNote:\nOnly when full"
        );
    }

    #[test]
    fn linked_servers_are_counted_when_none_is_paired() {
        let case = CaseNotes::default();
        let mut summary = facts(&case);
        summary.linked_servers = 2;
        assert!(issue_summary(&summary).ends_with("Linked servers: 2"));
    }

    #[test]
    fn a_single_sighting_is_not_pluralized() {
        let mut case = CaseNotes::default();
        case.count_reproduced();
        assert!(issue_summary(&facts(&case)).ends_with("Seen again 1 time."));
    }
}
