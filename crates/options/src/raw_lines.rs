//! Raw `options.txt` lines a user adds to the global defaults.
//!
//! # Why this exists next to the catalog
//!
//! The catalog offers a small, verified set of settings. Anything outside it,
//! such as a setting a new release added, a mod's option, or a rarely used key
//! binding, can still be put into a new instance's file as a literal line.
//!
//! # What is and is not checked
//!
//! Only what the file format itself requires: a line is `key:value` with a
//! non-empty key and no control characters. A line is never checked against a
//! release's settings, codecs, or data version, and it is never repaired. The
//! user is responsible for what the text means to the game.
//!
//! The data-version marker is the one key refused by name. It describes the
//! release being written, so it is never taken from text that is not tied to
//! one ([`crate::VERSION_KEY`]).
//!
//! A raw line is applied after every managed preference and after the line
//! Ephervex adds for a release's startup preset, so it wins over both. A file
//! created for a release the compatibility data does not know carries no data
//! version, because there is none to state.

use crate::file::VERSION_KEY;

/// Why one line of raw text cannot be written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawLineProblem {
    /// There is no `:` to separate a key from a value.
    MissingSeparator,
    /// Nothing precedes the first `:`.
    EmptyKey,
    /// The key or the value holds a control character, which the file format
    /// cannot carry on one line.
    ControlCharacter,
    /// The line sets the data-version marker, which belongs to the release
    /// being written.
    DataVersionMarker,
}

impl RawLineProblem {
    /// A short stable name, for machine-readable reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MissingSeparator => "missing-separator",
            Self::EmptyKey => "empty-key",
            Self::ControlCharacter => "control-character",
            Self::DataVersionMarker => "data-version",
        }
    }

    /// The `ephervex-i18n` message id describing this problem to a user.
    #[must_use]
    pub const fn message_id(self) -> &'static str {
        match self {
            Self::MissingSeparator => "preferences-raw-line-missing-separator",
            Self::EmptyKey => "preferences-raw-line-empty-key",
            Self::ControlCharacter => "preferences-raw-line-control-character",
            Self::DataVersionMarker => "preferences-raw-line-data-version",
        }
    }
}

/// One line that was not written, and where it is in the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RejectedRawLine {
    /// The 1-based line number in the text as the user typed it.
    pub line_number: usize,
    pub problem: RawLineProblem,
}

/// Raw text split into the assignments that can be written and the lines that
/// cannot.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RawOptionLines {
    assignments: Vec<(String, String)>,
    rejected: Vec<RejectedRawLine>,
}

impl RawOptionLines {
    /// Reads raw text, one `key:value` assignment per line.
    ///
    /// Infallible: a line that cannot be written is reported in
    /// [`Self::rejected`] and the rest are kept. Blank lines are ignored. Lines
    /// are otherwise taken as given, including any whitespace around the key or
    /// the value.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut parsed = Self::default();
        for (index, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            match classify(line) {
                Ok((key, value)) => parsed.assignments.push((key.to_owned(), value.to_owned())),
                Err(problem) => parsed.rejected.push(RejectedRawLine {
                    line_number: index + 1,
                    problem,
                }),
            }
        }
        parsed
    }

    /// The assignments to write, in the order the text gave them.
    #[must_use]
    pub fn assignments(&self) -> &[(String, String)] {
        &self.assignments
    }

    /// The lines that were not kept, in text order.
    #[must_use]
    pub fn rejected(&self) -> &[RejectedRawLine] {
        &self.rejected
    }

    /// Whether the text holds nothing to write.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.assignments.is_empty()
    }
}

fn classify(line: &str) -> Result<(&str, &str), RawLineProblem> {
    let Some((key, value)) = line.split_once(':') else {
        return Err(if line.chars().any(char::is_control) {
            RawLineProblem::ControlCharacter
        } else {
            RawLineProblem::MissingSeparator
        });
    };
    if key.chars().chain(value.chars()).any(char::is_control) {
        return Err(RawLineProblem::ControlCharacter);
    }
    if key.is_empty() {
        return Err(RawLineProblem::EmptyKey);
    }
    if key == VERSION_KEY {
        return Err(RawLineProblem::DataVersionMarker);
    }
    Ok((key, value))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn pairs(lines: &RawOptionLines) -> Vec<(&str, &str)> {
        lines
            .assignments()
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect()
    }

    #[test]
    fn a_line_splits_at_its_first_separator_and_is_otherwise_taken_as_given() {
        let lines = RawOptionLines::parse("key:a:b\nspaced: 2 \nresourcePacks:[\"vanilla\"]\n");
        assert_eq!(
            pairs(&lines),
            vec![
                ("key", "a:b"),
                ("spaced", " 2 "),
                ("resourcePacks", "[\"vanilla\"]")
            ]
        );
        assert!(lines.rejected().is_empty());
    }

    #[test]
    fn blank_lines_are_ignored_without_shifting_the_numbers_reported_for_others() {
        let lines = RawOptionLines::parse("\n  \nfov:0.5\n\nnot a line\r\nlang:en_us");
        assert_eq!(pairs(&lines), vec![("fov", "0.5"), ("lang", "en_us")]);
        assert_eq!(
            lines.rejected(),
            &[RejectedRawLine {
                line_number: 5,
                problem: RawLineProblem::MissingSeparator,
            }]
        );
    }

    #[test]
    fn a_line_the_file_format_cannot_hold_is_reported_rather_than_repaired() {
        let lines =
            RawOptionLines::parse(":value\nkey\x07:value\nkey:va\x00lue\nplain\tline\nok:1");
        assert_eq!(pairs(&lines), vec![("ok", "1")]);
        assert_eq!(
            lines
                .rejected()
                .iter()
                .map(|rejected| (rejected.line_number, rejected.problem))
                .collect::<Vec<_>>(),
            vec![
                (1, RawLineProblem::EmptyKey),
                (2, RawLineProblem::ControlCharacter),
                (3, RawLineProblem::ControlCharacter),
                (4, RawLineProblem::ControlCharacter),
            ]
        );
    }

    #[test]
    fn a_lone_carriage_return_cannot_join_two_settings_into_one_line() {
        let lines = RawOptionLines::parse("a:1\rb:2");
        assert!(lines.is_empty());
        assert_eq!(lines.rejected().len(), 1);
        assert_eq!(
            lines.rejected()[0].problem,
            RawLineProblem::ControlCharacter
        );
    }

    #[test]
    fn the_data_version_marker_is_never_taken_from_the_text() {
        let lines = RawOptionLines::parse("version:3465\nVersion:1\nfov:0.5");
        assert_eq!(pairs(&lines), vec![("Version", "1"), ("fov", "0.5")]);
        assert_eq!(
            lines.rejected(),
            &[RejectedRawLine {
                line_number: 1,
                problem: RawLineProblem::DataVersionMarker,
            }]
        );
    }

    #[test]
    fn text_with_nothing_to_write_is_empty() {
        for text in ["", "\n\n", "  \r\n", "no separator"] {
            assert!(RawOptionLines::parse(text).is_empty(), "{text:?}");
        }
    }

    #[test]
    fn every_problem_has_its_own_stable_name() {
        let names = [
            RawLineProblem::MissingSeparator,
            RawLineProblem::EmptyKey,
            RawLineProblem::ControlCharacter,
            RawLineProblem::DataVersionMarker,
        ]
        .map(RawLineProblem::as_str);
        assert_eq!(
            names,
            [
                "missing-separator",
                "empty-key",
                "control-character",
                "data-version"
            ]
        );
    }
}
