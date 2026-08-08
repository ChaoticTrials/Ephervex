//! The folders a user can open from an instance.
//!
//! # Why this is not a list of strings in the frontend
//!
//! Two reasons. The layout is this crate's — `<root>/minecraft/` and
//! `<root>/server/` are decided in [`crate::metadata`], and a frontend that
//! joined `"minecraft/mods"` itself would have to be found and changed the day
//! that layout moves. Handling a *missing* folder is a decision, not
//! a formatting detail:
//!
//! > If a folder does not exist, either create safe conventional folders such
//! > as `mods/` and `config/`, or disable the action where creation would be
//! > misleading.
//!
//! So each folder says which of the two it is. `mods/` is created on demand,
//! because an instance is expected to have one and an empty one is exactly
//! what a user who wants to drop a jar in is looking for. `crash-reports/` is
//! not: a freshly created, empty crash-reports folder reads as "there were
//! crashes and they are gone", which is worse than the action being unavailable.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::metadata::{client_game_dir, server_game_dir};
use crate::paths::{PathError, create_dir_all};

/// Whether a folder that does not exist yet should be created.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingFolder {
    /// Create it. Conventional, expected to exist, and empty is meaningful.
    Create,
    /// Leave it, and report the action as unavailable. Creating it would imply
    /// the game had produced something it has not.
    Disable,
}

/// A folder of a client instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ClientFolder {
    /// The instance root, which also holds `.launcher/`.
    Root,
    /// `<root>/minecraft/` — the game directory itself.
    Minecraft,
    Mods,
    Config,
    Logs,
    CrashReports,
    ResourcePacks,
    ShaderPacks,
    Saves,
}

impl ClientFolder {
    /// Every configurable folder in stable display order.
    pub const ALL: &'static [Self] = &[
        Self::Root,
        Self::Minecraft,
        Self::Mods,
        Self::Config,
        Self::Logs,
        Self::CrashReports,
        Self::ResourcePacks,
        Self::ShaderPacks,
        Self::Saves,
    ];

    /// The stable name used on the command line.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Root => "root",
            Self::Minecraft => "minecraft",
            Self::Mods => "mods",
            Self::Config => "config",
            Self::Logs => "logs",
            Self::CrashReports => "crash-reports",
            Self::ResourcePacks => "resourcepacks",
            Self::ShaderPacks => "shaderpacks",
            Self::Saves => "saves",
        }
    }

    /// Parses a command-line name.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|folder| folder.as_str() == name.trim())
    }

    /// Where this folder is, given an instance root.
    #[must_use]
    pub fn path(self, instance_root: &Path) -> PathBuf {
        let game = client_game_dir(instance_root);
        match self {
            Self::Root => instance_root.to_path_buf(),
            Self::Minecraft => game,
            // The names Minecraft itself uses. `crash-reports` is hyphenated
            // and `resourcepacks` is not; both are the game's spelling, not a
            // choice made here.
            Self::Mods => game.join("mods"),
            Self::Config => game.join("config"),
            Self::Logs => game.join("logs"),
            Self::CrashReports => game.join("crash-reports"),
            Self::ResourcePacks => game.join("resourcepacks"),
            Self::ShaderPacks => game.join("shaderpacks"),
            Self::Saves => game.join("saves"),
        }
    }

    /// What to do when the folder is not there.
    #[must_use]
    pub const fn when_missing(self) -> MissingFolder {
        match self {
            // The instance's own two directories are created with the
            // instance; a mods or config folder is conventional and useful
            // empty, which is safe to create automatically.
            Self::Root
            | Self::Minecraft
            | Self::Mods
            | Self::Config
            | Self::ResourcePacks
            | Self::ShaderPacks => MissingFolder::Create,
            // Output of a game that has run. Creating one would misrepresent
            // the instance's history.
            Self::Logs | Self::CrashReports | Self::Saves => MissingFolder::Disable,
        }
    }
}

impl fmt::Display for ClientFolder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A folder of a server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ServerFolder {
    /// The server root, which also holds `.launcher/`.
    Root,
    /// `<root>/server/` — where the server actually runs.
    Server,
    Mods,
    Config,
    Logs,
    CrashReports,
    World,
}

impl ServerFolder {
    /// Every resolved folder in stable display order.
    pub const ALL: &'static [Self] = &[
        Self::Root,
        Self::Server,
        Self::Mods,
        Self::Config,
        Self::Logs,
        Self::CrashReports,
        Self::World,
    ];

    /// The stable name used on the command line.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Root => "root",
            Self::Server => "server",
            Self::Mods => "mods",
            Self::Config => "config",
            Self::Logs => "logs",
            Self::CrashReports => "crash-reports",
            Self::World => "world",
        }
    }

    /// Parses a command-line name.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|folder| folder.as_str() == name.trim())
    }

    /// Where this folder is, given a server root.
    #[must_use]
    pub fn path(self, server_root: &Path) -> PathBuf {
        let game = server_game_dir(server_root);
        match self {
            Self::Root => server_root.to_path_buf(),
            Self::Server => game,
            Self::Mods => game.join("mods"),
            Self::Config => game.join("config"),
            Self::Logs => game.join("logs"),
            Self::CrashReports => game.join("crash-reports"),
            // `world` is `level-name`'s default; a server configured with
            // another level name is server management's business, and until then
            // pointing at a folder that is not there is disabled rather than
            // invented.
            Self::World => game.join("world"),
        }
    }

    /// What to do when the folder is not there.
    #[must_use]
    pub const fn when_missing(self) -> MissingFolder {
        match self {
            Self::Root | Self::Server | Self::Mods | Self::Config => MissingFolder::Create,
            Self::Logs | Self::CrashReports | Self::World => MissingFolder::Disable,
        }
    }
}

impl fmt::Display for ServerFolder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Resolves a folder to an openable path, creating it where that is safe.
///
/// `None` means the action is unavailable: the folder does not exist and
/// creating it would tell the user something untrue.
pub fn resolve(path: PathBuf, when_missing: MissingFolder) -> Result<Option<PathBuf>, PathError> {
    if path.is_dir() {
        return Ok(Some(path));
    }
    // A *file* where a folder should be is not something to create over.
    if path.exists() {
        return Ok(None);
    }
    match when_missing {
        MissingFolder::Create => {
            create_dir_all(&path)?;
            Ok(Some(path))
        }
        MissingFolder::Disable => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn client_folders_live_under_the_game_directory_rather_than_the_instance_root() {
        // Only `.launcher/` is a sibling of the game directory; everything a
        // user opens is inside it.
        let root = Path::new("/instances/abc");
        assert_eq!(ClientFolder::Root.path(root), root);
        assert_eq!(
            ClientFolder::Minecraft.path(root),
            Path::new("/instances/abc/minecraft")
        );
        for folder in [
            ClientFolder::Mods,
            ClientFolder::Config,
            ClientFolder::Logs,
            ClientFolder::CrashReports,
            ClientFolder::ResourcePacks,
            ClientFolder::ShaderPacks,
            ClientFolder::Saves,
        ] {
            assert!(
                folder.path(root).starts_with("/instances/abc/minecraft"),
                "{folder}"
            );
        }
        assert_eq!(
            ClientFolder::CrashReports.path(root),
            Path::new("/instances/abc/minecraft/crash-reports"),
            "the game's own spelling, hyphen included"
        );
    }

    #[test]
    fn server_folders_live_under_the_server_directory() {
        let root = Path::new("/servers/abc");
        assert_eq!(ServerFolder::Root.path(root), root);
        assert_eq!(
            ServerFolder::Server.path(root),
            Path::new("/servers/abc/server")
        );
        assert_eq!(
            ServerFolder::World.path(root),
            Path::new("/servers/abc/server/world")
        );
    }

    #[test]
    fn a_conventional_folder_is_created_and_a_history_folder_is_not() {
        // The missing-folder decision is made per folder rather than globally: an empty `mods/`
        // is what a user wants; an empty `crash-reports/` is a lie.
        let base = tempfile::tempdir().expect("temp dir");
        let root = base.path();

        let mods = resolve(
            ClientFolder::Mods.path(root),
            ClientFolder::Mods.when_missing(),
        )
        .expect("resolve");
        assert_eq!(mods, Some(ClientFolder::Mods.path(root)));
        assert!(ClientFolder::Mods.path(root).is_dir());

        let crashes = resolve(
            ClientFolder::CrashReports.path(root),
            ClientFolder::CrashReports.when_missing(),
        )
        .expect("resolve");
        assert_eq!(crashes, None);
        assert!(!ClientFolder::CrashReports.path(root).exists());
    }

    #[test]
    fn a_folder_that_already_exists_is_opened_whatever_its_policy_says() {
        let base = tempfile::tempdir().expect("temp dir");
        let logs = ClientFolder::Logs.path(base.path());
        std::fs::create_dir_all(&logs).expect("mkdir");

        assert_eq!(
            resolve(logs.clone(), ClientFolder::Logs.when_missing()).expect("resolve"),
            Some(logs)
        );
    }

    #[test]
    fn a_file_where_a_folder_should_be_is_never_created_over() {
        let base = tempfile::tempdir().expect("temp dir");
        let mods = ClientFolder::Mods.path(base.path());
        std::fs::create_dir_all(mods.parent().expect("parent")).expect("mkdir");
        std::fs::write(&mods, "not a directory").expect("write");

        assert_eq!(
            resolve(mods.clone(), ClientFolder::Mods.when_missing()).expect("resolve"),
            None
        );
        assert!(mods.is_file(), "the file must survive");
    }

    #[test]
    fn folder_names_are_stable_and_parse_back() {
        for folder in ClientFolder::ALL {
            assert_eq!(ClientFolder::parse(folder.as_str()), Some(*folder));
        }
        for folder in ServerFolder::ALL {
            assert_eq!(ServerFolder::parse(folder.as_str()), Some(*folder));
        }
        assert_eq!(ClientFolder::parse("crash_reports"), None);
    }
}
