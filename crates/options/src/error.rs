//! Failures reading or writing an instance's `options.txt`.

use std::path::PathBuf;

/// Anything that can go wrong applying global preferences to a file.
///
/// Parsing is deliberately absent: [`crate::ParsedOptions::parse`] is
/// infallible, because a line this build cannot interpret must be *preserved*,
/// not rejected. The only failures here are the two the
/// filesystem can produce.
#[derive(Debug, thiserror::Error)]
pub enum OptionsError {
    /// The existing `options.txt` could not be read.
    ///
    /// Never degraded to "start from an empty file": that would silently
    /// discard every setting the user already had.
    #[error("could not read the Minecraft options file {path}")]
    Unreadable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The options file, or the directory holding it, could not be written.
    #[error("could not write the Minecraft options file {path}")]
    Unwritable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl OptionsError {
    /// The `ephervex-i18n` message id describing this failure to a user.
    #[must_use]
    pub const fn message_id(&self) -> &'static str {
        match self {
            Self::Unreadable { .. } => "error-options-unreadable",
            Self::Unwritable { .. } => "error-options-unwritable",
        }
    }

    /// The file this failure is about.
    #[must_use]
    pub fn failing_path(&self) -> Option<PathBuf> {
        match self {
            Self::Unreadable { path, .. } | Self::Unwritable { path, .. } => Some(path.clone()),
        }
    }
}
