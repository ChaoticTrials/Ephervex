//! Themes a user adds to the themes folder, as TOML files or theme folders.
//!
//! A theme names a built-in parent and overrides some of its colors; every
//! color it leaves out is the parent's. A theme folder may also replace the
//! logo and window icon with images in its own `branding/` folder. The format
//! is documented in `docs/themes.md` (ADR 0042, ADR 0047). Files are the user's
//! own, but they are still read defensively: bounded in number and size, parsed
//! strictly, and a file that cannot be used is reported and skipped without
//! affecting the others.

use std::collections::HashSet;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use ephervex_core::theme_package::{
    self, BRANDING_FOLDER, BrandingImage, BrandingReference, BrandingReferenceError, BrandingRole,
    MAX_BRANDING_DIMENSION, MAX_BRANDING_FILE_BYTES, MAX_REFERENCE_BYTES, MAX_REFERENCE_DEPTH,
    THEME_MANIFEST, ThemePackageError,
};
use ephervex_core::{ApplicationColorScheme, ApplicationTheme};
use ephervex_i18n::{Arg, lookup, lookup_args};
use slint::Color;

use crate::palette::PALETTE_COLORS;

/// The folder under the configuration directory that holds theme files.
pub const THEMES_FOLDER: &str = "themes";

/// The theme format this build reads.
const FORMAT_VERSION: i64 = 1;
/// Theme files and folders read at most, in name order.
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
    /// Always empty for a standalone file, which has no `branding/` folder.
    pub branding: ThemeBranding,
}

/// The images a theme folder names under `[branding]`. An absent one is the
/// default.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ThemeBranding {
    pub logo: Option<BrandingReference>,
    pub icon: Option<BrandingReference>,
}

/// The chosen theme's decoded branding. An absent image is the default.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BrandingImages {
    pub logo: Option<BrandingImage>,
    pub icon: Option<BrandingImage>,
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
    Syntax {
        line: usize,
        message: String,
    },
    NewerFormat(i64),
    InvalidField(&'static str),
    UnknownParent(String),
    InvalidColor(String),
    // Ignored rather than refused, so a theme written for a build with more
    // tokens still loads here.
    UnknownColor(String),
    UnknownKey(String),
    PackageLink,
    /// A file whose id a theme folder also has; the folder is used.
    Shadowed,
    BrandingWithoutFolder,
    UnknownBrandingKey(String),
    BrandingReference {
        reference: String,
        reason: BrandingReferenceError,
    },
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
            Self::PackageLink => lookup("gui-theme-problem-package-link"),
            Self::Shadowed => lookup("gui-theme-problem-shadowed"),
            Self::BrandingWithoutFolder => lookup("gui-theme-problem-branding-without-folder"),
            Self::UnknownBrandingKey(key) => text("gui-theme-problem-unknown-branding", key),
            Self::BrandingReference { reference, reason } => {
                let limit = |id: &str, limit: usize| {
                    lookup_args(
                        id,
                        [
                            ("value", Arg::from(reference.clone())),
                            (
                                "count",
                                Arg::Count(i64::try_from(limit).unwrap_or(i64::MAX)),
                            ),
                        ],
                    )
                };
                match reason {
                    BrandingReferenceError::Empty => lookup("gui-theme-problem-branding-empty"),
                    BrandingReferenceError::TooLong => {
                        limit("gui-theme-problem-branding-too-long", MAX_REFERENCE_BYTES)
                    }
                    BrandingReferenceError::TooDeep => limit(
                        "gui-theme-problem-branding-too-deep",
                        MAX_REFERENCE_DEPTH - 1,
                    ),
                    BrandingReferenceError::EmptyComponent => {
                        text("gui-theme-problem-branding-empty-component", reference)
                    }
                    BrandingReferenceError::ControlCharacter => {
                        text("gui-theme-problem-branding-control", reference)
                    }
                    BrandingReferenceError::UnsupportedFormat => {
                        text("gui-theme-problem-branding-format", reference)
                    }
                    BrandingReferenceError::Backslash
                    | BrandingReferenceError::Scheme
                    | BrandingReferenceError::Absolute
                    | BrandingReferenceError::Traversal => {
                        text("gui-theme-problem-branding-outside", reference)
                    }
                }
            }
        }
    }
}

/// Why one branding image of the chosen theme is not shown.
fn branding_message(error: &ThemePackageError) -> String {
    let count = |value: u64| Arg::Count(i64::try_from(value).unwrap_or(i64::MAX));
    match error {
        ThemePackageError::Missing => lookup("gui-theme-branding-missing"),
        ThemePackageError::SymbolicLink => lookup("gui-theme-branding-link"),
        ThemePackageError::NotAFile => lookup("gui-theme-branding-not-file"),
        ThemePackageError::TooLarge { .. } => lookup_args(
            "gui-theme-branding-too-large",
            [("count", count(MAX_BRANDING_FILE_BYTES / (1024 * 1024)))],
        ),
        ThemePackageError::TooManyPixels { .. } => lookup_args(
            "gui-theme-branding-too-many-pixels",
            [("count", count(u64::from(MAX_BRANDING_DIMENSION)))],
        ),
        ThemePackageError::FormatMismatch => lookup("gui-theme-branding-format-mismatch"),
        ThemePackageError::DecodeLimit => lookup("gui-theme-branding-decode-limit"),
        ThemePackageError::Malformed(_) => lookup("gui-theme-branding-malformed"),
        ThemePackageError::InvalidPackageName | ThemePackageError::Unreadable(_) => {
            lookup("gui-theme-branding-unreadable")
        }
    }
}

/// One theme's result: the theme, if usable, and what was wrong with it.
type ThemeRead = (Option<UserTheme>, Vec<Problem>);

/// Something in the themes folder that holds a theme.
#[derive(Debug)]
enum Candidate {
    File(PathBuf),
    Package(String),
    /// A link where a theme folder would be, which is never followed.
    LinkedPackage(String),
}

/// What `entry` holds, if it looks like a theme. Only decides what to try:
/// a theme folder is read through `theme_package`, which enforces the rules.
fn candidate(entry: &std::fs::DirEntry) -> Option<(String, Candidate)> {
    let name = entry.file_name().to_string_lossy().into_owned();
    let path = entry.path();
    if path
        .extension()
        .is_some_and(|extension| extension == "toml")
        && path.is_file()
    {
        return Some((name, Candidate::File(path)));
    }
    let kind = entry.file_type().ok()?;
    let manifest = path.join(THEME_MANIFEST);
    if kind.is_dir() && manifest.symlink_metadata().is_ok() {
        Some((name.clone(), Candidate::Package(name)))
    } else if kind.is_symlink() && manifest.exists() {
        Some((name.clone(), Candidate::LinkedPackage(name)))
    } else {
        None
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
        .filter_map(|entry| candidate(&entry))
        .collect();
    files.sort_by(|a, b| a.0.cmp(&b.0));
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

    // Folders are read first, because only a usable one shadows the file it
    // shares a name with.
    let mut read: Vec<(String, ThemeRead)> = files
        .iter()
        .map(|(name, candidate)| match candidate {
            Candidate::File(_) => (name.clone(), (None, Vec::new())),
            Candidate::Package(id) => (
                format!("{id}/{THEME_MANIFEST}"),
                read_theme_package(folder, id),
            ),
            Candidate::LinkedPackage(id) => (
                format!("{id}/{THEME_MANIFEST}"),
                (None, vec![Problem::PackageLink]),
            ),
        })
        .collect();
    let usable: HashSet<String> = read
        .iter()
        .filter_map(|(_, (theme, _))| theme.as_ref().map(|theme| theme.id.clone()))
        .collect();
    for ((_, candidate), (_, result)) in files.iter().zip(&mut read) {
        if let Candidate::File(path) = candidate {
            let shadowed = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(|stem| usable.contains(stem));
            *result = if shadowed {
                (None, vec![Problem::Shadowed])
            } else {
                read_theme_file(path)
            };
        }
    }
    for (file, (theme, problems)) in read {
        for problem in problems {
            tracing::warn!(%file, ?problem, "theme file problem");
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
        Ok(_) => {
            let (mut theme, mut problems) = parse_theme(id, &text);
            let is_branding = |problem: &Problem| {
                matches!(
                    problem,
                    Problem::BrandingReference { .. } | Problem::UnknownBrandingKey(_)
                )
            };
            if let Some(theme) = &mut theme
                && (theme.branding != ThemeBranding::default() || problems.iter().any(is_branding))
            {
                theme.branding = ThemeBranding::default();
                problems.retain(|problem| !is_branding(problem));
                problems.push(Problem::BrandingWithoutFolder);
            }
            (theme, problems)
        }
        Err(_) => (None, vec![Problem::Unreadable]),
    }
}

/// Reads `<folder>/<id>/theme.toml`, never through a link.
fn read_theme_package(folder: &Path, id: &str) -> (Option<UserTheme>, Vec<Problem>) {
    if ApplicationTheme::user(id).is_none() {
        return (None, vec![Problem::FileName]);
    }
    match theme_package::read_theme_manifest(folder, id, MAX_THEME_FILE_BYTES) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => parse_theme(id, &text),
            Err(_) => (None, vec![Problem::Unreadable]),
        },
        Err(ThemePackageError::TooLarge { .. }) => (None, vec![Problem::TooLarge]),
        Err(ThemePackageError::SymbolicLink) => (None, vec![Problem::PackageLink]),
        Err(_) => (None, vec![Problem::Unreadable]),
    }
}

/// Decodes the branding of the chosen theme: a theme folder's own images. A
/// built-in theme has none, so it shows the default logo and icon. Each image
/// that cannot be used is reported in `themes.problems` and left at the
/// default.
#[must_use]
pub fn read_selected_branding(
    folder: &Path,
    themes: &mut UserThemes,
    chosen: &ApplicationTheme,
) -> BrandingImages {
    let ApplicationTheme::User(id) = chosen else {
        return BrandingImages::default();
    };
    let Some(theme) = themes.themes.iter().find(|theme| theme.id == *id) else {
        return BrandingImages::default();
    };
    let mut problems = Vec::new();
    let mut read = |reference: Option<&BrandingReference>, role| {
        let reference = reference?;
        match theme_package::read_branding_image(folder, &theme.id, reference, role) {
            Ok(image) => Some(image),
            Err(error) => {
                let file = format!("{}/{BRANDING_FOLDER}/{}", theme.id, reference.as_str());
                tracing::warn!(%file, %error, "theme branding unusable");
                problems.push(lookup_args(
                    "gui-theme-problem",
                    [
                        ("file", Arg::from(file)),
                        ("value", Arg::from(branding_message(&error))),
                    ],
                ));
                None
            }
        }
    };
    let images = BrandingImages {
        logo: read(theme.branding.logo.as_ref(), BrandingRole::Logo),
        icon: read(theme.branding.icon.as_ref(), BrandingRole::Icon),
    };
    themes.problems.extend(problems);
    images
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
            "format_version" | "name" | "parent" | "colors" | "branding"
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

    let branding = match parse_branding(document.get("branding"), &mut notes) {
        Ok(branding) => branding,
        Err(problem) => return (None, vec![problem]),
    };

    let theme = UserTheme {
        id: id.to_owned(),
        name,
        parent,
        colors,
        branding,
    };
    (Some(theme), notes)
}

/// The `[branding]` table. A wrong type refuses the file; a reference that is
/// unsafe or unsupported is reported in `notes` and left at the default.
fn parse_branding(
    table: Option<&toml::Value>,
    notes: &mut Vec<Problem>,
) -> Result<ThemeBranding, Problem> {
    let mut branding = ThemeBranding::default();
    let table = match table {
        None => return Ok(branding),
        Some(toml::Value::Table(table)) => table,
        Some(_) => return Err(Problem::InvalidField("branding")),
    };
    for (key, value) in table {
        let (slot, field) = match key.as_str() {
            "logo" => (&mut branding.logo, "branding.logo"),
            "icon" => (&mut branding.icon, "branding.icon"),
            _ => {
                notes.push(Problem::UnknownBrandingKey(key.clone()));
                continue;
            }
        };
        let Some(text) = value.as_str() else {
            return Err(Problem::InvalidField(field));
        };
        match BrandingReference::parse(text) {
            Ok(reference) => *slot = Some(reference),
            Err(reason) => notes.push(Problem::BrandingReference {
                reference: text.chars().filter(|c| !c.is_control()).collect(),
                reason,
            }),
        }
    }
    Ok(branding)
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
    #![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

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

    /// Dark and light stay valid parents, so a theme written for either keeps
    /// loading.
    #[test]
    fn every_built_in_name_is_a_valid_parent() {
        for (name, parent) in [
            ("workbench", ApplicationColorScheme::Workbench),
            ("dark", ApplicationColorScheme::Dark),
            ("light", ApplicationColorScheme::Light),
        ] {
            let (theme, problems) = parse_theme("mine", &format!("parent = \"{name}\"\n"));
            assert!(problems.is_empty(), "{name}: {problems:?}");
            assert_eq!(theme.expect("usable").parent, parent, "{name}");
        }
        let (theme, problems) = parse_theme("mine", "parent = \"solarized\"\n");
        assert!(theme.is_none());
        assert_eq!(problems, [Problem::UnknownParent("solarized".to_owned())]);
    }

    #[test]
    fn an_empty_file_is_the_default_parent_under_its_file_name() {
        let (theme, problems) = parse_theme("plain", "");
        assert!(problems.is_empty());
        let theme = theme.expect("usable");
        assert_eq!(theme.name, "plain");
        assert_eq!(theme.parent, ApplicationColorScheme::Workbench);
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
    fn the_documented_example_branding_is_valid_once_uncommented() {
        let uncommented: String = EXAMPLE
            .lines()
            .map(|line| match line {
                "# [branding]" | "# logo = \"logo.png\"" | "# icon = \"icon.png\"" => &line[2..],
                line => line,
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert_ne!(
            uncommented,
            EXAMPLE.trim_end(),
            "the section is in the example"
        );
        let (theme, problems) = parse_theme("modrinth", &uncommented);
        assert!(problems.is_empty(), "{problems:?}");
        let branding = theme.expect("usable").branding;
        assert_eq!(branding.logo.expect("logo").as_str(), "logo.png");
        assert_eq!(branding.icon.expect("icon").as_str(), "icon.png");
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

    fn package(folder: &Path, id: &str, manifest: &str) -> PathBuf {
        let branding = folder.join(id).join(BRANDING_FOLDER);
        std::fs::create_dir_all(&branding).expect("package");
        std::fs::write(folder.join(id).join(THEME_MANIFEST), manifest).expect("manifest");
        branding
    }

    fn png(rgba: [u8; 4]) -> Vec<u8> {
        ephervex_core::test_support::solid_png(4, 2, rgba)
    }

    #[test]
    fn a_theme_folder_names_its_branding_relative_to_its_branding_folder() {
        let (theme, problems) = parse_theme(
            "pumpkin",
            "name = \"Pumpkin\"\n[branding]\nlogo = \"logo.png\"\nicon = \"icons/app.webp\"\n",
        );
        assert!(problems.is_empty(), "{problems:?}");
        let branding = theme.expect("usable").branding;
        assert_eq!(branding.logo.expect("logo").as_str(), "logo.png");
        assert_eq!(branding.icon.expect("icon").as_str(), "icons/app.webp");
    }

    #[test]
    fn an_unusable_branding_reference_is_reported_and_the_theme_still_loads() {
        for (text, expected) in [
            (
                "../other/branding/logo.png",
                Problem::BrandingReference {
                    reference: "../other/branding/logo.png".to_owned(),
                    reason: BrandingReferenceError::Traversal,
                },
            ),
            (
                "/etc/logo.png",
                Problem::BrandingReference {
                    reference: "/etc/logo.png".to_owned(),
                    reason: BrandingReferenceError::Absolute,
                },
            ),
            (
                "https://example.com/logo.png",
                Problem::BrandingReference {
                    reference: "https://example.com/logo.png".to_owned(),
                    reason: BrandingReferenceError::Scheme,
                },
            ),
            (
                "logo.svg",
                Problem::BrandingReference {
                    reference: "logo.svg".to_owned(),
                    reason: BrandingReferenceError::UnsupportedFormat,
                },
            ),
        ] {
            let (theme, problems) = parse_theme(
                "t",
                &format!("[branding]\nlogo = \"{text}\"\nicon = \"ok.png\""),
            );
            let theme = theme.expect("usable");
            assert_eq!(theme.branding.logo, None, "{text}");
            assert!(theme.branding.icon.is_some(), "{text}");
            assert_eq!(problems, [expected], "{text}");
        }
        let (theme, problems) = parse_theme("t", "[branding]\nbanner = \"b.png\"");
        assert_eq!(theme.expect("usable").branding, ThemeBranding::default());
        assert_eq!(problems, [Problem::UnknownBrandingKey("banner".to_owned())]);
    }

    #[test]
    fn branding_of_the_wrong_type_skips_the_theme() {
        for (text, expected) in [
            ("branding = \"logo.png\"", "branding"),
            ("[branding]\nlogo = 1", "branding.logo"),
            ("[branding]\nicon = [\"a.png\"]", "branding.icon"),
        ] {
            let (theme, problems) = parse_theme("t", text);
            assert!(theme.is_none(), "{text}");
            assert_eq!(problems, [Problem::InvalidField(expected)], "{text}");
        }
    }

    #[test]
    fn a_standalone_file_keeps_its_colors_and_ignores_branding() {
        let folder = tempfile::tempdir().expect("temp dir");
        std::fs::write(
            folder.path().join("plain.toml"),
            "[colors]\naccent = \"#112233\"\n[branding]\nlogo = \"logo.png\"\n",
        )
        .expect("write");
        std::fs::write(
            folder.path().join("unsafe.toml"),
            "[branding]\nlogo = \"../logo.png\"\n",
        )
        .expect("write");
        let found = read_user_themes(folder.path());
        let [plain, unsafe_theme] = found.themes.as_slice() else {
            panic!("two themes: {found:?}");
        };
        assert_eq!(plain.colors.len(), 1);
        assert_eq!(plain.branding, ThemeBranding::default());
        assert_eq!(unsafe_theme.branding, ThemeBranding::default());
        let without_folder = lookup("gui-theme-problem-branding-without-folder");
        for file in ["plain.toml", "unsafe.toml"] {
            let expected = lookup_args(
                "gui-theme-problem",
                [
                    ("file", Arg::from(file)),
                    ("value", Arg::from(without_folder.clone())),
                ],
            );
            assert!(
                found.problems.contains(&expected),
                "{file}: {:?}",
                found.problems
            );
        }
        assert_eq!(found.problems.len(), 2, "{:?}", found.problems);
    }

    #[test]
    fn theme_folders_are_read_beside_theme_files() {
        let folder = tempfile::tempdir().expect("temp dir");
        package(
            folder.path(),
            "pumpkin",
            "name = \"Pumpkin\"\nparent = \"light\"\n[branding]\nlogo = \"logo.png\"",
        );
        std::fs::write(folder.path().join("modrinth.toml"), "name = \"Modrinth\"").expect("write");
        // A folder holding no theme is not one, and says nothing.
        std::fs::create_dir(folder.path().join("backup")).expect("dir");

        let found = read_user_themes(folder.path());
        assert!(found.problems.is_empty(), "{:?}", found.problems);
        let ids: Vec<&str> = found.themes.iter().map(|theme| theme.id.as_str()).collect();
        assert_eq!(ids, ["modrinth", "pumpkin"]);
        let pumpkin = &found.themes[1];
        assert_eq!(pumpkin.parent, ApplicationColorScheme::Light);
        assert_eq!(
            pumpkin
                .branding
                .logo
                .as_ref()
                .map(BrandingReference::as_str),
            Some("logo.png")
        );
    }

    #[test]
    fn a_theme_folder_is_never_read_through_a_link() {
        let outside = tempfile::tempdir().expect("outside");
        package(outside.path(), "real", "name = \"Outside\"");
        let folder = tempfile::tempdir().expect("temp dir");
        std::os::unix::fs::symlink(outside.path().join("real"), folder.path().join("linked"))
            .expect("link");
        let branding = package(folder.path(), "manifest-link", "");
        std::fs::remove_file(folder.path().join("manifest-link").join(THEME_MANIFEST))
            .expect("remove");
        std::os::unix::fs::symlink(
            outside.path().join("real").join(THEME_MANIFEST),
            branding.parent().expect("package").join(THEME_MANIFEST),
        )
        .expect("link");

        let found = read_user_themes(folder.path());
        assert!(found.themes.is_empty(), "{:?}", found.themes);
        assert_eq!(found.problems.len(), 2, "{:?}", found.problems);
        let link = lookup("gui-theme-problem-package-link");
        for file in ["linked/theme.toml", "manifest-link/theme.toml"] {
            assert!(
                found.problems.contains(&lookup_args(
                    "gui-theme-problem",
                    [
                        ("file", Arg::from(file)),
                        ("value", Arg::from(link.clone()))
                    ]
                )),
                "{file}: {:?}",
                found.problems
            );
        }
    }

    #[test]
    fn a_theme_folder_wins_over_a_file_of_the_same_name_and_bad_names_are_reported() {
        let folder = tempfile::tempdir().expect("temp dir");
        package(folder.path(), "mine", "name = \"Folder\"");
        std::fs::write(folder.path().join("mine.toml"), "name = \"File\"").expect("write");
        package(folder.path(), "Bad Name", "");
        // A refused link leaves the file of its name in use.
        let outside = tempfile::tempdir().expect("outside");
        package(outside.path(), "real", "");
        std::os::unix::fs::symlink(outside.path().join("real"), folder.path().join("other"))
            .expect("link");
        std::fs::write(folder.path().join("other.toml"), "name = \"Other\"").expect("write");
        // So does a folder that cannot be used.
        package(folder.path(), "broken", "colors = 1");
        std::fs::write(folder.path().join("broken.toml"), "name = \"Broken file\"").expect("write");

        let found = read_user_themes(folder.path());
        let names: Vec<&str> = found
            .themes
            .iter()
            .map(|theme| theme.name.as_str())
            .collect();
        assert_eq!(names, ["Broken file", "Folder", "Other"]);
        assert_eq!(found.problems.len(), 4, "{:?}", found.problems);
        assert!(
            found
                .problems
                .iter()
                .any(|problem| problem.starts_with("broken/theme.toml: "))
        );
        assert!(
            found
                .problems
                .iter()
                .any(|problem| problem.starts_with("other/theme.toml: "))
        );
        assert!(
            found
                .problems
                .iter()
                .any(|problem| problem.starts_with("mine.toml: "))
        );
        assert!(
            found
                .problems
                .iter()
                .any(|problem| problem.starts_with("Bad Name/theme.toml: "))
        );
    }

    #[test]
    fn the_chosen_theme_folder_shows_its_logo_and_icon() {
        let folder = tempfile::tempdir().expect("temp dir");
        let branding = package(
            folder.path(),
            "pumpkin",
            "[branding]\nlogo = \"logo.png\"\nicon = \"icon.png\"",
        );
        std::fs::write(branding.join("logo.png"), png([200, 100, 0, 255])).expect("write");
        std::fs::write(branding.join("icon.png"), png([0, 0, 0, 255])).expect("write");
        let mut found = read_user_themes(folder.path());

        let images = read_selected_branding(
            folder.path(),
            &mut found,
            &ApplicationTheme::User("pumpkin".to_owned()),
        );
        assert!(found.problems.is_empty(), "{:?}", found.problems);
        let logo = images.logo.expect("logo");
        assert_eq!(
            (logo.width(), logo.height()),
            (4, 2),
            "the logo keeps its shape"
        );
        assert_eq!(&logo.rgba()[..4], &[200, 100, 0, 255]);
        let icon = images.icon.expect("icon");
        assert_eq!(
            (icon.width(), icon.height()),
            (4, 4),
            "the icon is made square"
        );
    }

    #[test]
    fn a_branding_image_that_cannot_be_used_is_reported_and_the_default_kept() {
        let outside = tempfile::tempdir().expect("outside");
        std::fs::write(outside.path().join("secret.png"), png([1, 1, 1, 255])).expect("write");
        let folder = tempfile::tempdir().expect("temp dir");
        let branding = package(
            folder.path(),
            "broken",
            "[branding]\nlogo = \"absent.png\"\nicon = \"linked.png\"",
        );
        std::os::unix::fs::symlink(
            outside.path().join("secret.png"),
            branding.join("linked.png"),
        )
        .expect("link");
        let mut found = read_user_themes(folder.path());

        let images = read_selected_branding(
            folder.path(),
            &mut found,
            &ApplicationTheme::User("broken".to_owned()),
        );
        assert_eq!(images, BrandingImages::default());
        let reported = |file: &str, message: &str| {
            lookup_args(
                "gui-theme-problem",
                [
                    ("file", Arg::from(file)),
                    ("value", Arg::from(lookup(message))),
                ],
            )
        };
        assert_eq!(
            found.problems,
            [
                reported("broken/branding/absent.png", "gui-theme-branding-missing"),
                reported("broken/branding/linked.png", "gui-theme-branding-link"),
            ]
        );

        std::fs::remove_file(branding.join("linked.png")).expect("remove");
        std::fs::write(branding.join("absent.png"), "not a png").expect("write");
        let mut found = read_user_themes(folder.path());
        let images = read_selected_branding(
            folder.path(),
            &mut found,
            &ApplicationTheme::User("broken".to_owned()),
        );
        assert_eq!(images, BrandingImages::default());
        assert_eq!(
            found.problems[0],
            reported(
                "broken/branding/absent.png",
                "gui-theme-branding-format-mismatch"
            )
        );
    }

    #[test]
    fn a_built_in_theme_shows_the_default_logo_and_icon() {
        let folder = tempfile::tempdir().expect("temp dir");
        let mut found = read_user_themes(folder.path());
        for scheme in [
            ApplicationColorScheme::Workbench,
            ApplicationColorScheme::Dark,
            ApplicationColorScheme::Light,
        ] {
            assert_eq!(
                read_selected_branding(
                    folder.path(),
                    &mut found,
                    &ApplicationTheme::BuiltIn(scheme)
                ),
                BrandingImages::default()
            );
        }
        assert!(found.problems.is_empty());
    }

    #[test]
    fn each_refused_branding_reference_says_what_to_change() {
        let message = |text: &str| {
            let (_, problems) = parse_theme("t", &format!("[branding]\nlogo = \"{text}\""));
            let [problem] = problems.as_slice() else {
                panic!("one problem for {text:?}: {problems:?}");
            };
            problem.message()
        };
        assert_eq!(message(""), lookup("gui-theme-problem-branding-empty"));
        assert!(message("a/b/c/d/e.png").contains('3'));
        assert!(message(&format!("{}.png", "a".repeat(300))).contains("255"));
        assert_eq!(
            message("icons//logo.png"),
            lookup_args(
                "gui-theme-problem-branding-empty-component",
                [("value", Arg::from("icons//logo.png"))]
            )
        );
        assert_eq!(
            message("logo\\u0007.png"),
            lookup_args(
                "gui-theme-problem-branding-control",
                [("value", Arg::from("logo.png"))]
            )
        );
        let outside = lookup_args(
            "gui-theme-problem-branding-outside",
            [("value", Arg::from("../logo.png"))],
        );
        assert_eq!(message("../logo.png"), outside);
        let distinct: HashSet<String> = [
            "",
            "a/b/c/d/e.png",
            "icons//logo.png",
            "../logo.png",
            "logo.svg",
        ]
        .into_iter()
        .map(message)
        .collect();
        assert_eq!(distinct.len(), 5);
    }

    #[test]
    fn only_the_chosen_theme_folder_is_decoded() {
        let folder = tempfile::tempdir().expect("temp dir");
        let branding = package(folder.path(), "pumpkin", "[branding]\nlogo = \"logo.png\"");
        std::fs::write(branding.join("logo.png"), png([200, 100, 0, 255])).expect("write");
        let mut found = read_user_themes(folder.path());
        for selected in [
            ApplicationTheme::default(),
            ApplicationTheme::User("absent".to_owned()),
        ] {
            assert_eq!(
                read_selected_branding(folder.path(), &mut found, &selected),
                BrandingImages::default()
            );
        }
        assert!(found.problems.is_empty());
    }
}
