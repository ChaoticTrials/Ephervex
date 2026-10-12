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
//! "unknown data to be dropped" - it is simply a line nobody edited.
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

/// The `options.txt` key holding the release's own data version.
///
/// It tells the game which of its historical fixes the rest of the file still
/// needs, so a value from another release is worse than no value at all. Two
/// rules follow, and they are not the same rule: a file Ephervex creates is
/// marked with the data version of the exact release it is for, because a file
/// without one is read as predating every fix the game has ever shipped; a
/// file that already exists keeps whatever marker it came with, which survives
/// an edit for the same reason every other unmanaged line does.
pub const VERSION_KEY: &str = "version";

/// The content of one line of an `options.txt`.
///
/// Anything that is not a `key:value` assignment - a blank line, a comment
/// somebody added, a truncated line left by a crash - is kept verbatim rather
/// than normalized or dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Content {
    Entry { key: String, value: String },
    Verbatim(String),
}

/// How one line ended in the source.
///
/// Every line carries its own, so a file that mixes `\n` and `\r\n` or ends
/// without a terminator comes back out exactly as it went in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ending {
    /// The last line of a file that has no final terminator.
    None,
    Lf,
    CrLf,
    /// A lone `\r` closing the file's last line.
    Cr,
}

impl Ending {
    const fn as_str(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Lf => "\n",
            Self::CrLf => "\r\n",
            Self::Cr => "\r",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Line {
    content: Content,
    ending: Ending,
}

/// Whether a key can be written as `key:value` and read back as the same key.
///
/// Shared with the generated dataset's representation keys so both agree on
/// what an `options.txt` key is.
pub(crate) fn is_valid_key(key: &str) -> bool {
    !key.is_empty() && !key.contains(':') && !key.chars().any(char::is_control)
}

/// A `options.txt` file, parsed but not normalized.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedOptions {
    lines: Vec<Line>,
}

impl ParsedOptions {
    /// An empty file, as used when an instance has no `options.txt` yet.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Parses a file. Infallible: see the module documentation.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut lines = Vec::new();
        let mut rest = text;
        while !rest.is_empty() {
            let (raw, ending) = if let Some((segment, after)) = rest.split_once('\n') {
                rest = after;
                match segment.strip_suffix('\r') {
                    Some(stripped) => (stripped, Ending::CrLf),
                    None => (segment, Ending::Lf),
                }
            } else {
                let last = std::mem::take(&mut rest);
                match last.strip_suffix('\r') {
                    Some(stripped) => (stripped, Ending::Cr),
                    None => (last, Ending::None),
                }
            };
            let content = match raw.split_once(':') {
                Some((key, value)) if !key.is_empty() => Content::Entry {
                    key: key.to_owned(),
                    value: value.to_owned(),
                },
                // Everything else is kept exactly as it was: a blank line,
                // a comment somebody added, a truncated line - and a
                // leading-colon line, which has no key at all and which the
                // game would read as an empty one.
                Some(_) | None => Content::Verbatim(raw.to_owned()),
            };
            lines.push(Line { content, ending });
        }
        Self { lines }
    }

    /// The value of a key, as the game would read it: the **last** assignment.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.lines
            .iter()
            .rev()
            .find_map(|line| match &line.content {
                Content::Entry { key: found, value } if found == key => Some(value.as_str()),
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
    /// because a newline in a value is how one setting becomes two. A key the
    /// file format cannot hold (empty, containing a colon or a control
    /// character) is refused for the same reason: it would read back as a
    /// different key or as no assignment. Callers validate their input; this
    /// is the backstop.
    ///
    /// A new line ends like the line before it, and a file that had no final
    /// terminator still has none afterwards. A file closed by a bare `\r` is
    /// terminated, so it stays terminated.
    pub fn set(&mut self, key: &str, value: impl Into<String>) {
        let value = value.into();
        if !is_valid_key(key) {
            tracing::warn!(
                key = key.escape_debug().to_string(),
                "refusing to write a Minecraft option whose key cannot be stored in options.txt"
            );
            return;
        }
        if value.contains('\n') || value.contains('\r') {
            tracing::warn!(
                key,
                "refusing to write a Minecraft option whose value contains a line break"
            );
            return;
        }

        let content = Content::Entry {
            key: key.to_owned(),
            value,
        };
        if let Some(line) =
            self.lines.iter_mut().rev().find(
                |line| matches!(&line.content, Content::Entry { key: found, .. } if found == key),
            )
        {
            line.content = content;
            return;
        }

        // The old last line needs a terminator of its own now, and a bare
        // `\r` would join the next line to it on reading.
        let style = self
            .lines
            .iter()
            .rev()
            .map(|line| line.ending)
            .find(|ending| matches!(ending, Ending::Lf | Ending::CrLf))
            .unwrap_or(Ending::Lf);
        let ending = match self.lines.last_mut() {
            None => Ending::Lf,
            Some(last) if matches!(last.ending, Ending::Lf | Ending::CrLf) => last.ending,
            Some(last) => {
                let was_terminated = last.ending == Ending::Cr;
                let ends_in_cr = match &last.content {
                    Content::Entry { value, .. } => value.ends_with('\r'),
                    Content::Verbatim(raw) => raw.ends_with('\r'),
                };
                // A `\n` after content ending in `\r` would read as `\r\n`.
                last.ending = if ends_in_cr { Ending::CrLf } else { style };
                if was_terminated {
                    last.ending
                } else {
                    Ending::None
                }
            }
        };
        self.lines.push(Line { content, ending });
    }

    /// Every key the file assigns, in file order, duplicates included.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.lines.iter().filter_map(|line| match &line.content {
            Content::Entry { key, .. } => Some(key.as_str()),
            Content::Verbatim(_) => None,
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
        let mut text = String::new();
        for line in &self.lines {
            match &line.content {
                Content::Entry { key, value } => {
                    text.push_str(key);
                    text.push(':');
                    text.push_str(value);
                }
                Content::Verbatim(raw) => text.push_str(raw),
            }
            text.push_str(line.ending.as_str());
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
        // to understand - only to leave alone.
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

    #[test]
    fn each_line_keeps_its_own_terminator() {
        for text in [
            "a:1\r\nb:2\n",
            "a:1\nb:2\r\n",
            "a:1\r\nb:2",
            "a:1\r",
            "a:1\r\n\r",
            "a:1\rb:2",
            "\r\n\n\r\n",
            "a:1\r\r\n",
        ] {
            assert_eq!(ParsedOptions::parse(text).render(), text, "{text:?}");
        }
    }

    #[test]
    fn editing_one_line_leaves_the_terminators_of_the_others() {
        let mut options = ParsedOptions::parse("a:1\r\nb:2\nc:3\r\n");
        options.set("b", "9");
        assert_eq!(options.render(), "a:1\r\nb:9\nc:3\r\n");
    }

    #[test]
    fn a_new_line_ends_like_the_line_before_it() {
        let mut options = ParsedOptions::parse("a:1\nb:2\r\n");
        options.set("c", "3");
        assert_eq!(options.render(), "a:1\nb:2\r\nc:3\r\n");
    }

    #[test]
    fn appending_after_an_unterminated_line_keeps_the_file_unterminated() {
        let mut options = ParsedOptions::parse("a:1\r\nb:2");
        options.set("c", "3");
        assert_eq!(options.render(), "a:1\r\nb:2\r\nc:3");
    }

    #[test]
    fn appending_after_a_bare_carriage_return_does_not_join_the_lines() {
        let mut options = ParsedOptions::parse("a:1\r");
        options.set("b", "2");
        let rendered = options.render();
        assert_eq!(rendered, "a:1\nb:2\n");
        assert_eq!(ParsedOptions::parse(&rendered), options);
    }

    #[test]
    fn a_key_the_line_format_cannot_hold_is_refused() {
        for key in ["", "a:b", "a\nb", "a\rb", "a\tb", "a\u{7f}b"] {
            let mut options = ParsedOptions::parse("guiScale:0\n");
            options.set(key, "x");
            assert_eq!(options.render(), "guiScale:0\n", "{key:?}");
            assert_eq!(options.keys().count(), 1, "{key:?}");
        }
    }

    #[test]
    fn parsing_then_rendering_is_the_identity_for_every_short_terminator_mix() {
        let mut texts = vec![String::new()];
        for _ in 0..7 {
            let longer: Vec<String> = texts
                .iter()
                .flat_map(|text| {
                    ["a", ":", "\r", "\n"]
                        .into_iter()
                        .map(move |piece| format!("{text}{piece}"))
                })
                .collect();
            for text in &longer {
                assert_eq!(ParsedOptions::parse(text).render(), *text, "{text:?}");
            }
            texts = longer;
        }
    }

    #[test]
    fn a_rendered_edit_reads_back_as_the_same_file() {
        let mut texts = vec![String::new()];
        for _ in 0..5 {
            texts = texts
                .iter()
                .flat_map(|text| {
                    ["a", ":", "\r", "\n"]
                        .into_iter()
                        .map(move |piece| format!("{text}{piece}"))
                })
                .collect();
            for text in &texts {
                for key in ["a", "new"] {
                    let mut options = ParsedOptions::parse(text);
                    options.set(key, "v:1");
                    let rendered = options.render();
                    assert_eq!(ParsedOptions::parse(&rendered), options, "{text:?} + {key}");
                    assert_eq!(options.get(key), Some("v:1"), "{text:?} + {key}");
                }
            }
        }
    }
}
