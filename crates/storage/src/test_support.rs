//! A disposable application environment for this crate's own tests.
//!
//! Every root — config, data, cache, state, and the *temporary* one — lives
//! inside a [`tempfile::TempDir`]. That matters twice over: a test can delete
//! the temporary instance root to simulate the OS clearing `/tmp` without
//! touching the real one, and no test can ever write into the developer's
//! actual `~/.local/share/ephervex`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use tempfile::TempDir;

use crate::paths::Paths;
use crate::registry::Registry;

/// A throwaway set of application directories.
#[derive(Debug)]
pub(crate) struct TestEnvironment {
    directory: TempDir,
    paths: Paths,
}

impl TestEnvironment {
    pub(crate) fn new() -> Self {
        let directory = tempfile::tempdir().expect("a temporary directory for the test");
        let paths = Paths::rooted_at(directory.path());
        paths
            .ensure_base_directories()
            .expect("create the test directories");
        Self { directory, paths }
    }

    /// The directory containing all five roots.
    pub(crate) fn base(&self) -> &Path {
        self.directory.path()
    }

    /// Opens a registry against this environment.
    ///
    /// Calling it twice models a restart: the second registry reopens the same
    /// database file with no shared in-memory state.
    pub(crate) fn registry(&self) -> Registry {
        Registry::open(self.paths.clone()).expect("open the registry")
    }

    /// Deletes the index, simulating the corrupt-shutdown case ADR 0002 is
    /// designed to survive.
    pub(crate) fn destroy_database(&self) {
        let database = self.paths.database_file();
        for path in [
            database.clone(),
            PathBuf::from(format!("{}-wal", database.display())),
            PathBuf::from(format!("{}-shm", database.display())),
        ] {
            let _ = std::fs::remove_file(path);
        }
    }
}
