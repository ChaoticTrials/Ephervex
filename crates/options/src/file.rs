//! A parsed `options.txt` that survives being edited.
//!
//! # The rule this module exists to keep
//!
//! *Never replace the entire file merely to change GUI scale.* An
//! `options.txt` belongs to the instance, and by the time Ephervex touches it
//! the user may have hundreds of settings in there, including keys written by
//! a Minecraft version this build has never heard of, keys written by mods, and
//! a `version:` whose meaning is version-sensitive.
//!
//! So this is a *line-preserving* model rather than a map. Every line the file
//! had comes back out in the order it went in, byte for byte, unless a
//! preference explicitly rewrote it. A key this build does not know is not
//! "unknown data to be dropped" — it is simply a line nobody edited.
//!
//! # How Minecraft itself reads the file
//!
//! One `key:value` per line, split at the **first** colon, later assignments
//! winning over earlier ones. Values are free text: `resourcePacks:["vanilla"]`
//! contains both a colon-free JSON array and, in other keys, colons of its own.
//! [`ParsedOptions::get`] therefore reports the last assignment and
//! [`ParsedOptions::set`] rewrites it in place, so this model agrees with the
//! game about what a duplicated key means.

use std::fmt;

/// One line of an `options.txt`.
///
/// Anything that is not a `key:value` assignment — a blank line, a comment
/// somebody added, a truncated line left by a crash — is kept verbatim rather
/// than normalized or dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Line {
    Entry { key: String, value: String },
    Verbatim(String),
}

/// A `options.txt` file, parsed but not normalized.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedOptions {
    lines: Vec<Line>,
    /// Whether the source used `\r\n`. Preserved so that editing one key in a
    /// file written on another platform does not leave mixed line endings.
    crlf: bool,
    /// Whether the source ended with a line terminator. Minecraft writes one;
    /// a file that lacked one keeps lacking it.
    trailing_newline: bool,
}

impl ParsedOptions {
    /// An empty file, as used when an instance has no `options.txt` yet.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            lines: Vec::new(),
            crlf: false,
            // A file Ephervex creates gets the terminator Minecraft would
            // write, so the game's own next save appends cleanly.
            trailing_newline: true,
        }
    }

    /// Parses a file. Infallible: see the module documentation.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        if text.is_empty() {
            return Self::empty();
        }

        let trailing_newline = text.ends_with('\n');
        let body = text.strip_suffix('\n').unwrap_or(text);
        let mut crlf = false;

        let lines = body
            .split('\n')
            .map(|raw| {
                let raw = match raw.strip_suffix('\r') {
                    Some(stripped) => {
                        crlf = true;
                        stripped
                    }
                    None => raw,
                };
                match raw.split_once(':') {
                    Some((key, value)) if !key.is_empty() => Line::Entry {
                        key: key.to_owned(),
                        value: value.to_owned(),
                    },
                    // Everything else is kept exactly as it was: a blank line,
                    // a comment somebody added, a truncated line — and a
                    // leading-colon line, which has no key at all and which the
                    // game would read as an empty one.
                    Some(_) | None => Line::Verbatim(raw.to_owned()),
                }
            })
            .collect();

        Self {
            lines,
            crlf,
            trailing_newline,
        }
    }

    /// The value of a key, as the game would read it: the **last** assignment.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.lines.iter().rev().find_map(|line| match line {
            Line::Entry { key: found, value } if found == key => Some(value.as_str()),
            _ => None,
        })
    }

    /// Whether the file assigns this key at all.
    #[must_use]
    pub fn contains(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    /// Sets a key, rewriting the assignment the game would honor.
    ///
    /// Rewrites the *last* assignment in place rather than appending, so a key
    /// keeps its position in the file and a second copy is never introduced.
    /// Earlier duplicates are left exactly as they were: the game ignores them,
    /// and deleting lines nobody asked about is not this function's business.
    ///
    /// A value containing a line terminator is refused rather than written,
    /// because a newline in a value is how one setting becomes two. Callers
    /// validate their values; this is the backstop.
    pub fn set(&mut self, key: &str, value: impl Into<String>) {
        let value = value.into();
        if value.contains('\n') || value.contains('\r') {
            tracing::warn!(
                key,
                "refusing to write a Minecraft option whose value contains a line break"
            );
            return;
        }

        if let Some(line) = self
            .lines
            .iter_mut()
            .rev()
            .find(|line| matches!(line, Line::Entry { key: found,.. } if found == key))
        {
            *line = Line::Entry {
                key: key.to_owned(),
                value,
            };
            return;
        }

        self.lines.push(Line::Entry {
            key: key.to_owned(),
            value,
        });
    }

    /// Every key the file assigns, in file order, duplicates included.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.lines.iter().filter_map(|line| match line {
            Line::Entry { key, .. } => Some(key.as_str()),
            Line::Verbatim(_) => None,
        })
    }

    /// Whether the file has no lines at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Renders the file back to text.
    ///
    /// A file no preference touched renders byte for byte as it was parsed.
    #[must_use]
    pub fn render(&self) -> String {
        let newline = if self.crlf { "\r\n" } else { "\n" };
        let mut text = String::new();
        for (index, line) in self.lines.iter().enumerate() {
            if index > 0 {
                text.push_str(newline);
            }
            match line {
                Line::Entry { key, value } => {
                    text.push_str(key);
                    text.push(':');
                    text.push_str(value);
                }
                Line::Verbatim(raw) => text.push_str(raw),
            }
        }
        if self.trailing_newline && !self.lines.is_empty() {
            text.push_str(newline);
        }
        text
    }
}

impl fmt::Display for ParsedOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.render())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    const SAMPLE: &str = "version:3465\nguiScale:0\nlang:en_us\nresourcePacks:[\"vanilla\"]\n";

    #[test]
    fn a_file_nobody_edited_renders_byte_for_byte() {
        // The whole point of the line model: reading and writing an instance's
        // options must be a no-op unless a preference actually changed
        for text in [
            SAMPLE,
            "",
            "guiScale:2",
            "\n\n",
            "# a comment somebody added\nfullscreen:true\n",
            "key:value:with:colons\n",
            "guiScale: 2 \n",
            "trailing junk with no colon\n",
        ] {
            assert_eq!(ParsedOptions::parse(text).render(), text, "{text:?}");
        }
    }

    #[test]
    fn windows_line_endings_survive_an_edit() {
        let mut options = ParsedOptions::parse("guiScale:0\r\nlang:en_us\r\n");
        options.set("guiScale", "2");
        assert_eq!(options.render(), "guiScale:2\r\nlang:en_us\r\n");
    }

    #[test]
    fn values_are_read_as_free_text_split_at_the_first_colon() {
        let options = ParsedOptions::parse(SAMPLE);
        assert_eq!(options.get("version"), Some("3465"));
        assert_eq!(options.get("resourcePacks"), Some("[\"vanilla\"]"));
        assert_eq!(
            ParsedOptions::parse("key:value:with:colons").get("key"),
            Some("value:with:colons")
        );
        assert!(options.get("simulationDistance").is_none());
    }

    #[test]
    fn an_unknown_key_is_preserved_rather_than_dropped() {
        // Keys written by a newer Minecraft, or by a mod, are not this build's
        // to understand — only to leave alone.
        let mut options =
            ParsedOptions::parse("aKeyFromTheFuture:17\nguiScale:0\nsomeModSetting:{\"a\":1}\n");
        options.set("guiScale", "2");

        let rendered = options.render();
        assert!(rendered.contains("aKeyFromTheFuture:17"));
        assert!(rendered.contains("someModSetting:{\"a\":1}"));
        assert_eq!(
            rendered,
            "aKeyFromTheFuture:17\nguiScale:2\nsomeModSetting:{\"a\":1}\n"
        );
    }

    #[test]
    fn setting_a_key_rewrites_it_in_place_rather_than_appending_a_second_copy() {
        let mut options = ParsedOptions::parse(SAMPLE);
        options.set("guiScale", "3");
        assert_eq!(options.get("guiScale"), Some("3"));
        assert_eq!(
            options.keys().filter(|key| *key == "guiScale").count(),
            1,
            "a second assignment would shadow the first and confuse a reader"
        );
        assert!(options.render().starts_with("version:3465\nguiScale:3\n"));
    }

    #[test]
    fn a_new_key_is_appended_and_an_empty_file_gains_a_terminator() {
        let mut options = ParsedOptions::empty();
        options.set("guiScale", "2");
        assert_eq!(options.render(), "guiScale:2\n");
    }

    #[test]
    fn a_duplicated_key_follows_the_games_last_one_wins_reading() {
        let mut options = ParsedOptions::parse("guiScale:1\nlang:en_us\nguiScale:2\n");
        assert_eq!(options.get("guiScale"), Some("2"));

        options.set("guiScale", "3");
        // The line the game honors is the one that changed; the dead earlier
        // assignment is left where it was rather than silently deleted.
        assert_eq!(options.render(), "guiScale:1\nlang:en_us\nguiScale:3\n");
        assert_eq!(options.get("guiScale"), Some("3"));
    }

    #[test]
    fn a_value_containing_a_line_break_is_refused_rather_than_written() {
        // A newline in a value turns one setting into two, which is the
        // options-file shape of an injection.
        let mut options = ParsedOptions::parse("guiScale:0\n");
        options.set("lang", "en_us\nfullscreen:true");
        assert!(options.get("lang").is_none());
        assert_eq!(options.render(), "guiScale:0\n");
    }

    #[test]
    fn a_line_with_no_key_is_kept_verbatim_rather_than_read_as_an_assignment() {
        let options = ParsedOptions::parse(":orphan\n");
        assert_eq!(options.keys().count(), 0);
        assert_eq!(options.render(), ":orphan\n");
    }

    #[test]
    fn a_file_without_a_trailing_newline_does_not_gain_one() {
        let mut options = ParsedOptions::parse("guiScale:0");
        options.set("lang", "en_us");
        assert_eq!(options.render(), "guiScale:0\nlang:en_us");
    }
}
