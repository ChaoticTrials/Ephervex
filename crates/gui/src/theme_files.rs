//! Themes a user adds as TOML files in the themes folder.
//!
//! A theme names a built-in parent and overrides some of its colors; every
//! color it leaves out is the parent's. The format is documented in
//! `docs/themes.md` (ADR 0042). Files are the user's own, but they are still
//! read defensively: bounded in number and size, parsed strictly, and a file
//! that cannot be used is reported and skipped without affecting the others.

use std::io::Read as _;
use std::path::Path;

use ephervex_core::{ApplicationColorScheme, ApplicationTheme};
use ephervex_i18n::{Arg, lookup, lookup_args};
use slint::Color;

use crate::palette::PALETTE_COLORS;

/// The folder under the configuration directory that holds theme files.
pub const THEMES_FOLDER: &str = "themes";

/// The theme format this build reads.
const FORMAT_VERSION: i64 = 1;
/// Files read at most, in file-name order.
const MAX_THEME_FILES: usize = 100;
/// A theme is a few dozen short lines; anything much larger is not one.
const MAX_THEME_FILE_BYTES: u64 = 64 * 1024;
/// Longest display name kept, in characters.
const MAX_NAME_CHARS: usize = 64;

/// One usable theme file.
#[derive(Debug, Clone, PartialEq)]
pub struct UserTheme {
    /// The file stem, which is what the preference records.
    pub id: String,
    /// What the theme list shows.
    pub name: String,
    pub parent: ApplicationColorScheme,
    /// The colors the file sets, by palette token.
    pub colors: Vec<(&'static str, Color)>,
}

impl UserTheme {
    /// The preference that selects this theme.
    #[must_use]
    pub fn preference(&self) -> ApplicationTheme {
        ApplicationTheme::User(self.id.clone())
    }
}

/// Everything the themes folder holds, and what was wrong with it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UserThemes {
    /// Sorted by name, then by file.
    pub themes: Vec<UserTheme>,
    /// One localized line per file that was skipped or partly ignored.
    pub problems: Vec<String>,
}

/// Why a file was skipped, or what in it was ignored.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Problem {
    FileName,
    Unreadable,
    TooLarge,
    Syntax { line: usize, message: String },
    NewerFormat(i64),
    InvalidField(&'static str),
    UnknownParent(String),
    InvalidColor(String),
    // Ignored rather than refused, so a theme written for a build with more
    // tokens still loads here.
    UnknownColor(String),
    UnknownKey(String),
}

impl Problem {
    fn message(&self) -> String {
        let text =
            |id: &str, value: &str| lookup_args(id, [("value", Arg::from(value.to_owned()))]);
        match self {
            Self::FileName => lookup("gui-theme-problem-file-name"),
            Self::Unreadable => lookup("gui-theme-problem-unreadable"),
            Self::TooLarge => lookup("gui-theme-problem-too-large"),
            Self::Syntax { line, message } => lookup_args(
                "gui-theme-problem-syntax",
                [
                    ("line", Arg::Count(i64::try_from(*line).unwrap_or(i64::MAX))),
                    ("value", Arg::from(message.clone())),
                ],
            ),
            Self::NewerFormat(found) => lookup_args(
                "gui-theme-problem-newer-format",
                [("count", Arg::Count(*found))],
            ),
            Self::InvalidField(key) => text("gui-theme-problem-invalid-field", key),
            Self::UnknownParent(value) => text("gui-theme-problem-unknown-parent", value),
            Self::InvalidColor(key) => text("gui-theme-problem-invalid-color", key),
            Self::UnknownColor(key) => text("gui-theme-problem-unknown-color", key),
            Self::UnknownKey(key) => text("gui-theme-problem-unknown-key", key),
        }
    }
}

/// Reads every theme in `folder`. A missing folder is simply no themes.
#[must_use]
pub fn read_user_themes(folder: &Path) -> UserThemes {
    let mut found = UserThemes::default();
    let entries = match std::fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return found,
        Err(error) => {
            tracing::warn!(path = %folder.display(), %error, "themes folder unreadable");
            found.problems.push(lookup("gui-theme-problem-folder"));
            return found;
        }
    };
    let mut files: Vec<_> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "toml")
        })
        .filter(|path| path.is_file())
        .collect();
    files.sort();
    if files.len() > MAX_THEME_FILES {
        found.problems.push(lookup_args(
            "gui-theme-problem-too-many",
            [
                (
                    "count",
                    Arg::Count(i64::try_from(MAX_THEME_FILES).unwrap_or(i64::MAX)),
                ),
                (
                    "value",
                    Arg::Count(i64::try_from(files.len() - MAX_THEME_FILES).unwrap_or(i64::MAX)),
                ),
            ],
        ));
        files.truncate(MAX_THEME_FILES);
    }

    for path in files {
        let file = path
            .file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
        let (theme, problems) = read_theme_file(&path);
        for problem in problems {
            tracing::warn!(path = %path.display(), ?problem, "theme file problem");
            found.problems.push(lookup_args(
                "gui-theme-problem",
                [
                    ("file", Arg::from(file.clone())),
                    ("value", Arg::from(problem.message())),
                ],
            ));
        }
        found.themes.extend(theme);
    }
    found.themes.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.id.cmp(&b.id))
    });
    found
}

fn read_theme_file(path: &Path) -> (Option<UserTheme>, Vec<Problem>) {
    let Some(id) = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| ApplicationTheme::user(stem).is_some())
    else {
        return (None, vec![Problem::FileName]);
    };
    let mut text = String::new();
    let read = std::fs::File::open(path).and_then(|file| {
        file.take(MAX_THEME_FILE_BYTES + 1)
            .read_to_string(&mut text)
    });
    match read {
        Ok(length) if length as u64 > MAX_THEME_FILE_BYTES => (None, vec![Problem::TooLarge]),
        Ok(_) => parse_theme(id, &text),
        Err(_) => (None, vec![Problem::Unreadable]),
    }
}

/// Parses one theme file's text. Returns the theme, if it is usable, and
/// everything that was wrong with the file.
fn parse_theme(id: &str, text: &str) -> (Option<UserTheme>, Vec<Problem>) {
    let document: toml::Table = match toml::from_str(text) {
        Ok(document) => document,
        Err(error) => {
            let line = error.span().map_or(1, |span| {
                text[..span.start.min(text.len())].lines().count().max(1)
            });
            let message = error.message().to_owned();
            return (None, vec![Problem::Syntax { line, message }]);
        }
    };

    let mut notes = Vec::new();
    for key in document.keys() {
        if !matches!(
            key.as_str(),
            "format_version" | "name" | "parent" | "colors"
        ) {
            notes.push(Problem::UnknownKey(key.clone()));
        }
    }

    match document.get("format_version") {
        None => {}
        Some(toml::Value::Integer(version)) if *version > FORMAT_VERSION => {
            return (None, vec![Problem::NewerFormat(*version)]);
        }
        Some(toml::Value::Integer(version)) if *version >= 1 => {}
        Some(_) => return (None, vec![Problem::InvalidField("format_version")]),
    }

    let name = match document.get("name") {
        None => id.to_owned(),
        Some(toml::Value::String(name)) => {
            let name: String = name
                .chars()
                .filter(|c| !c.is_control())
                .take(MAX_NAME_CHARS)
                .collect();
            let name = name.trim();
            if name.is_empty() {
                id.to_owned()
            } else {
                name.to_owned()
            }
        }
        Some(_) => return (None, vec![Problem::InvalidField("name")]),
    };

    let parent = match document.get("parent") {
        None => ApplicationColorScheme::default(),
        Some(toml::Value::String(parent)) => match ApplicationColorScheme::from_name(parent) {
            Some(parent) => parent,
            None => return (None, vec![Problem::UnknownParent(parent.clone())]),
        },
        Some(_) => return (None, vec![Problem::InvalidField("parent")]),
    };

    let mut colors = Vec::new();
    match document.get("colors") {
        None => {}
        Some(toml::Value::Table(table)) => {
            for (key, value) in table {
                let Some(token) = PALETTE_COLORS.iter().find(|token| **token == key.as_str())
                else {
                    notes.push(Problem::UnknownColor(key.clone()));
                    continue;
                };
                match value.as_str().and_then(parse_color) {
                    Some(color) => colors.push((*token, color)),
                    None => return (None, vec![Problem::InvalidColor(key.clone())]),
                }
            }
        }
        Some(_) => return (None, vec![Problem::InvalidField("colors")]),
    }

    let theme = UserTheme {
        id: id.to_owned(),
        name,
        parent,
        colors,
    };
    (Some(theme), notes)
}

/// `#rrggbb` or `#rrggbbaa`, and nothing looser.
fn parse_color(value: &str) -> Option<Color> {
    let hex = value.strip_prefix('#')?;
    if !matches!(hex.len(), 6 | 8) || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let channel = |at: usize| u8::from_str_radix(&hex[at..at + 2], 16).ok();
    let alpha = if hex.len() == 8 { channel(6)? } else { u8::MAX };
    Some(Color::from_argb_u8(
        alpha,
        channel(0)?,
        channel(2)?,
        channel(4)?,
    ))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;

    const EXAMPLE: &str = include_str!("../../../docs/themes/modrinth.toml");

    #[test]
    fn colors_are_six_or_eight_hex_digits_after_a_hash() {
        assert_eq!(
            parse_color("#1bd96a"),
            Some(Color::from_argb_u8(255, 0x1b, 0xd9, 0x6a))
        );
        assert_eq!(
            parse_color("#00000080"),
            Some(Color::from_argb_u8(0x80, 0, 0, 0))
        );
        for loose in [
            "1bd96a",
            "#fff",
            "#1bd96",
            "#1bd96a0",
            "#gggggg",
            "rgb(0,0,0)",
            "#+1bd96",
        ] {
            assert_eq!(parse_color(loose), None, "{loose}");
        }
    }

    #[test]
    fn a_theme_sets_only_what_it_names_on_top_of_its_parent() {
        let (theme, problems) = parse_theme(
            "mine",
            "name = \"Mine\"\nparent = \"light\"\n[colors]\naccent = \"#112233\"\n",
        );
        assert!(problems.is_empty(), "{problems:?}");
        let theme = theme.expect("usable");
        assert_eq!(theme.name, "Mine");
        assert_eq!(theme.parent, ApplicationColorScheme::Light);
        assert_eq!(
            theme.colors,
            [("accent", Color::from_argb_u8(255, 0x11, 0x22, 0x33))]
        );
        assert_eq!(
            theme.preference(),
            ApplicationTheme::User("mine".to_owned())
        );
    }

    #[test]
    fn an_empty_file_is_the_default_parent_under_its_file_name() {
        let (theme, problems) = parse_theme("plain", "");
        assert!(problems.is_empty());
        let theme = theme.expect("usable");
        assert_eq!(theme.name, "plain");
        assert_eq!(theme.parent, ApplicationColorScheme::default());
        assert!(theme.colors.is_empty());
    }

    #[test]
    fn a_name_is_shown_without_control_characters_and_at_a_bounded_length() {
        let long = "x".repeat(MAX_NAME_CHARS + 10);
        let (theme, _) = parse_theme("t", "name = \"  A\\u0007B\\nC  \"\n");
        assert_eq!(theme.expect("usable").name, "ABC");
        let (theme, _) = parse_theme("t", &format!("name = \"{long}\""));
        assert_eq!(theme.expect("usable").name.chars().count(), MAX_NAME_CHARS);
        let (theme, _) = parse_theme("t", "name = \"   \"");
        assert_eq!(theme.expect("usable").name, "t");
    }

    #[test]
    fn a_file_that_cannot_be_used_says_why_and_yields_no_theme() {
        for (text, expected) in [
            (
                "parent = \"solarized\"",
                Problem::UnknownParent("solarized".to_owned()),
            ),
            (
                "parent = \"user:other\"",
                Problem::UnknownParent("user:other".to_owned()),
            ),
            ("parent = 1", Problem::InvalidField("parent")),
            ("name = []", Problem::InvalidField("name")),
            ("colors = \"red\"", Problem::InvalidField("colors")),
            (
                "[colors]\naccent = \"green\"",
                Problem::InvalidColor("accent".to_owned()),
            ),
            (
                "[colors]\naccent = 5",
                Problem::InvalidColor("accent".to_owned()),
            ),
            ("format_version = 2", Problem::NewerFormat(2)),
            (
                "format_version = 0",
                Problem::InvalidField("format_version"),
            ),
        ] {
            let (theme, problems) = parse_theme("t", text);
            assert!(theme.is_none(), "{text}");
            assert_eq!(problems, [expected], "{text}");
        }
        let (theme, problems) = parse_theme("t", "name = \"a\"\nparent = \n");
        assert!(theme.is_none());
        assert!(matches!(
            problems.as_slice(),
            [Problem::Syntax { line: 2, .. }]
        ));
    }

    /// A theme written for a build with more tokens or keys still loads, and
    /// what was not understood is said rather than silently dropped.
    #[test]
    fn unknown_colors_and_keys_are_ignored_and_reported() {
        let (theme, problems) = parse_theme(
            "t",
            "author = \"me\"\n[colors]\nacent = \"#112233\"\naccent = \"#445566\"\n",
        );
        assert_eq!(theme.expect("usable").colors.len(), 1);
        assert_eq!(
            problems,
            [
                Problem::UnknownKey("author".to_owned()),
                Problem::UnknownColor("acent".to_owned())
            ]
        );
    }

    #[test]
    fn the_documented_example_is_a_complete_valid_theme() {
        let (theme, problems) = parse_theme("modrinth", EXAMPLE);
        assert!(problems.is_empty(), "{problems:?}");
        let theme = theme.expect("usable");
        assert_eq!(theme.name, "Modrinth");
        // Every token, so the example doubles as the list of what can be set.
        let mut set: Vec<&str> = theme.colors.iter().map(|(token, _)| *token).collect();
        let mut all = PALETTE_COLORS.to_vec();
        set.sort_unstable();
        all.sort_unstable();
        assert_eq!(set, all);
    }

    #[test]
    fn every_color_a_theme_can_set_is_documented() {
        let guide = include_str!("../../../docs/themes.md");
        for token in PALETTE_COLORS {
            assert!(
                guide.contains(&format!("| `{token}` |")),
                "`{token}` is undocumented"
            );
        }
    }

    #[test]
    fn the_folder_yields_every_usable_theme_sorted_by_name_and_reports_the_rest() {
        let folder = tempfile::tempdir().expect("temp dir");
        let write = |name: &str, text: &str| {
            std::fs::write(folder.path().join(name), text).expect("write");
        };
        write("zeta.toml", "name = \"Alpha\"");
        write("alpha.toml", "name = \"Zeta\"");
        write("broken.toml", "parent = \"nope\"");
        write("Bad Name.toml", "");
        write("notes.txt", "not a theme");
        write(
            "huge.toml",
            &"#".repeat(usize::try_from(MAX_THEME_FILE_BYTES).unwrap() + 1),
        );
        std::fs::create_dir(folder.path().join("dir.toml")).expect("dir");

        let found = read_user_themes(folder.path());
        let names: Vec<(&str, &str)> = found
            .themes
            .iter()
            .map(|theme| (theme.id.as_str(), theme.name.as_str()))
            .collect();
        assert_eq!(names, [("zeta", "Alpha"), ("alpha", "Zeta")]);
        assert_eq!(found.problems.len(), 3, "{:?}", found.problems);
        for file in ["Bad Name.toml", "broken.toml", "huge.toml"] {
            assert!(
                found.problems.iter().any(|problem| problem.contains(file)),
                "{file} not reported: {:?}",
                found.problems
            );
        }
    }

    #[test]
    fn no_folder_is_no_themes_and_no_problems() {
        let folder = tempfile::tempdir().expect("temp dir");
        assert_eq!(
            read_user_themes(&folder.path().join(THEMES_FOLDER)),
            UserThemes::default()
        );
    }

    #[test]
    fn only_so_many_files_are_read() {
        let folder = tempfile::tempdir().expect("temp dir");
        for index in 0..=MAX_THEME_FILES {
            std::fs::write(folder.path().join(format!("t{index:03}.toml")), "").expect("write");
        }
        let found = read_user_themes(folder.path());
        assert_eq!(found.themes.len(), MAX_THEME_FILES);
        assert_eq!(found.problems.len(), 1);
    }
}
