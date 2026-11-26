//! The window size a mod loader's early loading window is created at.
//!
//! Forge from 1.20.1 and every NeoForge release open their own window before
//! Minecraft starts, at the size `config/fml.toml` names (`earlyWindowWidth`,
//! `earlyWindowHeight`), and Minecraft then takes that window over. The
//! `overrideWidth` and `overrideHeight` keys in `options.txt` are never
//! consulted for it, so a window size preference has to reach this file too.
//!
//! Whether a loader has such a window is the loader crate's knowledge; this
//! module only edits the file, and edits it the way `options.txt` is edited:
//! every other key, comment, and table in an existing file survives, and a file
//! this build cannot read as TOML is left exactly as it was.

use std::path::{Path, PathBuf};

use ephervex_storage::atomic::{AtomicWriteOptions, write_bytes_atomic};
use toml_edit::{DocumentMut, Item, Value};

use crate::error::OptionsError;
use crate::preferences::WindowSize;

/// The loader configuration file, relative to a game directory.
pub const EARLY_WINDOW_CONFIG_FILE: &str = "config/fml.toml";

const WIDTH_KEY: &str = "earlyWindowWidth";
const HEIGHT_KEY: &str = "earlyWindowHeight";

/// Why an existing loader configuration file was not changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EarlyWindowRefusal {
    /// The file is not valid TOML, so no edit could be known to keep it intact.
    NotToml,
    /// A size key exists but is not a plain value, such as a table.
    UnexpectedShape,
}

impl EarlyWindowRefusal {
    /// A short stable name, for developer logs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotToml => "not-toml",
            Self::UnexpectedShape => "unexpected-shape",
        }
    }
}

/// What applying a size to the loader configuration did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EarlyWindowOutcome {
    /// The file now names the size.
    Written { path: PathBuf, created_file: bool },
    /// The file already named the size and was not rewritten.
    Unchanged { path: PathBuf },
    /// The file was left exactly as it was.
    Refused {
        path: PathBuf,
        reason: EarlyWindowRefusal,
    },
}

/// Sets the early loading window's size in `<game_dir>/config/fml.toml`.
///
/// Creates the file when there is none; the loader fills in every key it owns
/// the first time it starts. An existing file keeps all of its other content.
pub fn apply_early_window_size(
    game_dir: &Path,
    size: WindowSize,
) -> Result<EarlyWindowOutcome, OptionsError> {
    let path = game_dir.join(EARLY_WINDOW_CONFIG_FILE);
    let existing = match std::fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => None,
        Err(source) => {
            return Err(OptionsError::Unreadable { path, source });
        }
    };
    let created_file = existing.is_none();
    let Ok(mut document) = existing
        .as_deref()
        .unwrap_or_default()
        .parse::<DocumentMut>()
    else {
        return Ok(EarlyWindowOutcome::Refused {
            path,
            reason: EarlyWindowRefusal::NotToml,
        });
    };

    let mut changed = false;
    for (key, edge) in [(WIDTH_KEY, size.width), (HEIGHT_KEY, size.height)] {
        match set_integer(&mut document, key, i64::from(edge)) {
            Ok(did_change) => changed |= did_change,
            Err(reason) => return Ok(EarlyWindowOutcome::Refused { path, reason }),
        }
    }
    if !changed {
        return Ok(EarlyWindowOutcome::Unchanged { path });
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| OptionsError::Unwritable {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    write_bytes_atomic(
        &path,
        document.to_string().as_bytes(),
        AtomicWriteOptions::default(),
    )
    .map_err(|source| OptionsError::Unwritable {
        path: path.clone(),
        source,
    })?;
    Ok(EarlyWindowOutcome::Written { path, created_file })
}

/// Sets one top-level integer, keeping the key's comments and the value's own
/// surrounding whitespace and trailing comment. Reports whether it changed.
fn set_integer(
    document: &mut DocumentMut,
    key: &str,
    number: i64,
) -> Result<bool, EarlyWindowRefusal> {
    match document.get_mut(key) {
        None => {
            document.insert(key, Item::Value(Value::from(number)));
            Ok(true)
        }
        Some(Item::Value(value)) => {
            if value.as_integer() == Some(number) {
                return Ok(false);
            }
            let decor = value.decor().clone();
            *value = Value::from(number);
            *value.decor_mut() = decor;
            Ok(true)
        }
        Some(_) => Err(EarlyWindowRefusal::UnexpectedShape),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    const SIZE: WindowSize = WindowSize {
        width: 1920,
        height: 1080,
    };

    fn write_config(game: &Path, text: &str) {
        let path = game.join(EARLY_WINDOW_CONFIG_FILE);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn read_config(game: &Path) -> String {
        std::fs::read_to_string(game.join(EARLY_WINDOW_CONFIG_FILE)).unwrap()
    }

    #[test]
    fn a_missing_file_is_created_with_exactly_the_two_size_keys() {
        let game = tempfile::tempdir().unwrap();
        let outcome = apply_early_window_size(game.path(), SIZE).unwrap();
        assert!(matches!(
            outcome,
            EarlyWindowOutcome::Written {
                created_file: true,
                ..
            }
        ));
        let table: toml_edit::DocumentMut = read_config(game.path()).parse().unwrap();
        assert_eq!(table[WIDTH_KEY].as_integer(), Some(1920));
        assert_eq!(table[HEIGHT_KEY].as_integer(), Some(1080));
        assert_eq!(table.len(), 2);
    }

    #[test]
    fn existing_sizes_are_replaced_and_everything_else_is_kept_byte_for_byte() {
        let game = tempfile::tempdir().unwrap();
        write_config(
            game.path(),
            "# Global version checking\nversionCheck = true\n\n#Early window height\nearlyWindowHeight = 480 # inline\nearlyWindowWidth = 854\nearlyWindowSkipGLVersions = [\n    \"3.2\",\n]\n\n[extra]\n# kept\nkey = \"value\"\n",
        );
        let outcome = apply_early_window_size(game.path(), SIZE).unwrap();
        assert!(matches!(
            outcome,
            EarlyWindowOutcome::Written {
                created_file: false,
                ..
            }
        ));
        assert_eq!(
            read_config(game.path()),
            "# Global version checking\nversionCheck = true\n\n#Early window height\nearlyWindowHeight = 1080 # inline\nearlyWindowWidth = 1920\nearlyWindowSkipGLVersions = [\n    \"3.2\",\n]\n\n[extra]\n# kept\nkey = \"value\"\n"
        );
    }

    #[test]
    fn a_missing_key_is_added_beside_the_keys_a_file_already_has() {
        let game = tempfile::tempdir().unwrap();
        write_config(
            game.path(),
            "versionCheck = true\nearlyWindowWidth = 854\n\n[extra]\nkey = 1\n",
        );
        apply_early_window_size(game.path(), SIZE).unwrap();
        let text = read_config(game.path());
        let document: toml_edit::DocumentMut = text.parse().unwrap();
        assert_eq!(document[WIDTH_KEY].as_integer(), Some(1920));
        assert_eq!(document[HEIGHT_KEY].as_integer(), Some(1080));
        assert_eq!(document["versionCheck"].as_bool(), Some(true));
        assert_eq!(document["extra"]["key"].as_integer(), Some(1));
        assert!(
            text.find(HEIGHT_KEY).unwrap() < text.find("[extra]").unwrap(),
            "a top-level key must not land inside a table: {text}"
        );
    }

    #[test]
    fn a_file_that_already_names_the_size_is_not_rewritten() {
        let game = tempfile::tempdir().unwrap();
        let text = "earlyWindowHeight=1080\nearlyWindowWidth   =   1920\n";
        write_config(game.path(), text);
        let outcome = apply_early_window_size(game.path(), SIZE).unwrap();
        assert!(matches!(outcome, EarlyWindowOutcome::Unchanged { .. }));
        assert_eq!(read_config(game.path()), text);
    }

    #[test]
    fn a_file_that_is_not_toml_is_left_untouched() {
        let game = tempfile::tempdir().unwrap();
        let text = "earlyWindowWidth = \nthis is [not toml\n";
        write_config(game.path(), text);
        let outcome = apply_early_window_size(game.path(), SIZE).unwrap();
        assert!(matches!(
            outcome,
            EarlyWindowOutcome::Refused {
                reason: EarlyWindowRefusal::NotToml,
                ..
            }
        ));
        assert_eq!(read_config(game.path()), text);
    }

    #[test]
    fn a_size_key_that_is_a_table_is_left_untouched() {
        let game = tempfile::tempdir().unwrap();
        let text = "[earlyWindowWidth]\nvalue = 1\n";
        write_config(game.path(), text);
        let outcome = apply_early_window_size(game.path(), SIZE).unwrap();
        assert!(matches!(
            outcome,
            EarlyWindowOutcome::Refused {
                reason: EarlyWindowRefusal::UnexpectedShape,
                ..
            }
        ));
        assert_eq!(read_config(game.path()), text);
    }
}
