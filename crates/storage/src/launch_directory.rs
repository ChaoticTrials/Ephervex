//! The game directory an additional launch runs in.
//!
//! Two clients started from one instance share its saves, options and configs,
//! but Minecraft's logging rolls `logs/latest.log` away when a client starts, so
//! two clients sharing `logs/` destroy each other's log. A mod that reads
//! `<game directory>/logs/latest.log` (a dump or a report) must still find the
//! launch's own log there, so the additional launch cannot simply be pointed at
//! a different log folder.
//!
//! An additional launch therefore runs in a launcher-owned directory outside the
//! instance. Every top-level entry of the instance's game directory appears in
//! it as a link to the real entry, except `logs`, which links to the launch's own
//! folder inside the instance's `logs/` (see [`launch_log_folder_name`]).
//! Everything the game reads or writes below a linked folder lands in the
//! instance, and the launch's logs stay in the instance where its log listing,
//! bundle and storage accounting already look.
//!
//! The directory holds no state of its own and is rebuilt for each launch. A
//! top-level entry the game created in it, because the instance had none of that
//! name when the launch began, stays with the launch until the instance has one.
//! From then on the instance's entry wins, and the launch's copy is moved to
//! `<directory>.set-aside` rather than deleted.

use std::io;
use std::path::{Path, PathBuf};

use crate::diagnostics::launch_log_folder_name;
use crate::lock::is_valid_launch_id;
use crate::paths::PathError;

/// The game directory's own log folder, which an additional launch replaces.
const LOGS: &str = "logs";

/// Top-level folders every launch of a game uses and the game would create the
/// first time it needs them.
///
/// Created in the instance before an additional launch begins, so a client run
/// alongside the first one writes screenshots, saves and crash reports into the
/// instance instead of into its own directory.
const SHARED_FOLDERS: &[&str] = &[
    "saves",
    "screenshots",
    "resourcepacks",
    "crash-reports",
    "config",
];

/// The folder inside the instance's `logs/` that keeps the additional launch
/// `handle`'s game logs.
fn launch_log_folder(game_dir: &Path, handle: &str) -> PathBuf {
    game_dir.join(LOGS).join(launch_log_folder_name(handle))
}

/// Builds or refreshes `directory`, the game directory of the additional launch
/// `handle` of the instance whose game directory is `game_dir`.
///
/// Safe to call again for the same launch: links are repointed when they
/// differ, links to entries the instance no longer has are removed, and an
/// entry the previous run created in `directory` is left untouched.
pub fn prepare_launch_game_directory(
    game_dir: &Path,
    directory: &Path,
    handle: &str,
) -> Result<(), PathError> {
    if !is_valid_launch_id(handle) {
        return Err(refused(
            directory,
            "the launch handle is not a safe file name",
        ));
    }

    let logs = game_dir.join(LOGS);
    // A `logs` that is a link would make the launch's folder, and every log it
    // writes, land wherever the link points.
    require_real_directory(&logs, "the instance's logs folder is a link or a file")?;
    let launch_logs = launch_log_folder(game_dir, handle);
    require_real_directory(&launch_logs, "the launch's log folder is a link or a file")?;
    create_dir(&launch_logs)?;
    create_dir(directory)?;
    for folder in SHARED_FOLDERS {
        let path = game_dir.join(folder);
        if std::fs::symlink_metadata(&path).is_err() {
            create_dir(&path)?;
        }
    }

    let set_aside = set_aside_directory(directory);
    let mut shared = Vec::new();
    for entry in std::fs::read_dir(game_dir).map_err(|error| failure(game_dir, error))? {
        let entry = entry.map_err(|error| failure(game_dir, error))?;
        let name = entry.file_name();
        if name != LOGS {
            link(&entry.path(), &directory.join(&name), &set_aside)?;
            shared.push(name);
        }
    }
    link(&launch_logs, &directory.join(LOGS), &set_aside)?;

    for entry in std::fs::read_dir(directory).map_err(|error| failure(directory, error))? {
        let entry = entry.map_err(|error| failure(directory, error))?;
        let name = entry.file_name();
        let stale = entry.file_type().is_ok_and(|kind| kind.is_symlink())
            && name != LOGS
            && !shared.contains(&name);
        if stale {
            std::fs::remove_file(entry.path()).map_err(|error| failure(&entry.path(), error))?;
        }
    }
    Ok(())
}

/// Makes `link_path` a link to `target`, replacing a link that points
/// elsewhere.
///
/// A real entry the launch itself created under that name is moved aside so the
/// instance's entry is the one the launch sees, except that nothing is ever
/// moved over `logs`, which must already be a link (see [`LOGS`]).
fn link(target: &Path, link_path: &Path, set_aside: &Path) -> Result<(), PathError> {
    match std::fs::symlink_metadata(link_path) {
        Ok(metadata) if metadata.is_symlink() => {
            if std::fs::read_link(link_path).is_ok_and(|current| current == target) {
                return Ok(());
            }
            std::fs::remove_file(link_path).map_err(|error| failure(link_path, error))?;
        }
        Ok(_) if link_path.file_name().is_some_and(|name| name == LOGS) => {
            return Err(refused(link_path, "the launch's logs entry is not a link"));
        }
        Ok(_) => move_aside(link_path, set_aside)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(failure(link_path, error)),
    }
    symlink(target, link_path).map_err(|error| failure(link_path, error))
}

/// Moves `entry` into the `set_aside` folder, replacing an earlier entry of the
/// same name there.
fn move_aside(entry: &Path, set_aside: &Path) -> Result<(), PathError> {
    let Some(name) = entry.file_name() else {
        return Err(refused(entry, "the entry has no name"));
    };
    create_dir(set_aside)?;
    let destination = set_aside.join(name);
    match std::fs::symlink_metadata(&destination) {
        Ok(metadata) if metadata.is_dir() => {
            std::fs::remove_dir_all(&destination).map_err(|error| failure(&destination, error))?;
        }
        Ok(_) => {
            std::fs::remove_file(&destination).map_err(|error| failure(&destination, error))?;
        }
        Err(_) => {}
    }
    std::fs::rename(entry, &destination).map_err(|error| failure(entry, error))
}

/// Refuses a path that exists but is not a real directory.
fn require_real_directory(path: &Path, reason: &'static str) -> Result<(), PathError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_dir() => Err(refused(path, reason)),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(failure(path, error)),
    }
}

fn refused(path: &Path, reason: &'static str) -> PathError {
    PathError::LaunchDirectoryRefused {
        path: path.to_path_buf(),
        reason,
    }
}

fn set_aside_directory(directory: &Path) -> PathBuf {
    let mut name = directory.as_os_str().to_owned();
    name.push(".set-aside");
    PathBuf::from(name)
}

fn create_dir(path: &Path) -> Result<(), PathError> {
    std::fs::create_dir_all(path).map_err(|error| failure(path, error))
}

fn failure(path: &Path, source: io::Error) -> PathError {
    PathError::DirectoryCreation {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(unix)]
fn symlink(target: &Path, link_path: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link_path)
}

#[cfg(not(unix))]
fn symlink(_target: &Path, _link_path: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "linking requires a Unix platform",
    ))
}

#[cfg(all(test, unix))]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn instance() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let base = tempfile::tempdir().expect("tempdir");
        let game = base.path().join("instance").join("minecraft");
        std::fs::create_dir_all(game.join("saves").join("world")).expect("saves");
        std::fs::create_dir_all(game.join("config")).expect("config");
        std::fs::write(game.join("options.txt"), "fov:70").expect("options");
        std::fs::create_dir_all(game.join("logs")).expect("logs");
        std::fs::write(game.join("logs").join("latest.log"), "first").expect("first log");
        let directory = base.path().join("launch-game");
        (base, game, directory)
    }

    #[test]
    fn an_additional_launch_writes_its_log_into_its_own_folder_of_the_instance() {
        let (_base, game, directory) = instance();
        prepare_launch_game_directory(&game, &directory, "second").expect("prepared");

        std::fs::write(directory.join("logs").join("latest.log"), "second").expect("write");

        assert_eq!(
            std::fs::read_to_string(game.join("logs").join("launch-second").join("latest.log"))
                .expect("launch log"),
            "second"
        );
        assert_eq!(
            std::fs::read_to_string(game.join("logs").join("latest.log")).expect("first log"),
            "first",
            "the first launch's log is untouched"
        );
    }

    #[test]
    fn everything_but_the_logs_is_the_instances_own() {
        let (_base, game, directory) = instance();
        prepare_launch_game_directory(&game, &directory, "second").expect("prepared");

        std::fs::write(directory.join("saves").join("world").join("level.dat"), "w")
            .expect("save through the link");
        assert_eq!(
            std::fs::read_to_string(directory.join("options.txt")).expect("options"),
            "fov:70"
        );
        assert!(game.join("saves").join("world").join("level.dat").is_file());
        assert!(
            std::fs::symlink_metadata(directory.join("config"))
                .expect("config")
                .is_symlink()
        );
    }

    #[test]
    fn folders_every_launch_uses_exist_in_the_instance_before_the_launch_starts() {
        let (_base, game, directory) = instance();
        prepare_launch_game_directory(&game, &directory, "second").expect("prepared");

        for folder in SHARED_FOLDERS {
            assert!(game.join(folder).is_dir(), "{folder}");
        }
        std::fs::write(directory.join("crash-reports").join("crash.txt"), "c").expect("crash");
        assert!(game.join("crash-reports").join("crash.txt").is_file());
    }

    #[test]
    fn preparing_again_follows_the_instance_and_keeps_the_launchs_own_files() {
        let (_base, game, directory) = instance();
        prepare_launch_game_directory(&game, &directory, "second").expect("prepared");
        std::fs::write(directory.join("leftover.txt"), "kept").expect("leftover");
        std::fs::remove_dir_all(game.join("config")).expect("remove config");
        std::fs::create_dir_all(game.join("mods")).expect("mods");

        prepare_launch_game_directory(&game, &directory, "second").expect("prepared again");

        // `config` is created again in the instance before the links are made.
        assert!(directory.join("config").is_dir());
        assert!(directory.join("mods").is_dir());
        assert_eq!(
            std::fs::read_to_string(directory.join("leftover.txt")).expect("leftover"),
            "kept"
        );
    }

    #[test]
    fn a_copy_the_launch_made_no_longer_hides_the_instances_entry_and_is_set_aside() {
        let (_base, game, directory) = instance();
        prepare_launch_game_directory(&game, &directory, "second").expect("prepared");
        // A game replacing a linked file by renaming over it leaves a real file.
        std::fs::remove_file(directory.join("options.txt")).expect("unlink");
        std::fs::write(directory.join("options.txt"), "fov:110").expect("own copy");

        prepare_launch_game_directory(&game, &directory, "second").expect("prepared again");

        assert_eq!(
            std::fs::read_to_string(directory.join("options.txt")).expect("options"),
            "fov:70",
            "the launch sees the instance's file again"
        );
        assert_eq!(
            std::fs::read_to_string(set_aside_directory(&directory).join("options.txt"))
                .expect("set aside"),
            "fov:110"
        );
    }

    #[test]
    fn a_launch_log_folder_that_is_a_link_is_refused() {
        let (base, game, directory) = instance();
        let elsewhere = base.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).expect("elsewhere");
        std::os::unix::fs::symlink(&elsewhere, game.join("logs").join("launch-second"))
            .expect("link");

        let error =
            prepare_launch_game_directory(&game, &directory, "second").expect_err("refused");
        assert!(
            matches!(error, PathError::LaunchDirectoryRefused { .. }),
            "{error}"
        );
        assert!(
            std::fs::read_dir(&elsewhere)
                .expect("read")
                .next()
                .is_none()
        );
    }

    #[test]
    fn a_logs_folder_that_is_a_link_is_refused_rather_than_written_through() {
        let (base, game, directory) = instance();
        let elsewhere = base.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).expect("elsewhere");
        std::fs::remove_dir_all(game.join("logs")).expect("remove logs");
        std::os::unix::fs::symlink(&elsewhere, game.join("logs")).expect("link");

        assert!(prepare_launch_game_directory(&game, &directory, "second").is_err());
        assert!(
            std::fs::read_dir(&elsewhere)
                .expect("read")
                .next()
                .is_none()
        );
    }

    #[test]
    fn a_handle_that_is_not_a_safe_file_name_is_refused() {
        let (_base, game, directory) = instance();
        for handle in ["", "../up", "a/b", "a b"] {
            assert!(
                prepare_launch_game_directory(&game, &directory, handle).is_err(),
                "{handle:?}"
            );
        }
    }
}
