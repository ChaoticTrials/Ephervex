//! The SQLite index and its migration runner (ADR 0002).
//!
//! # Why a migration runner and not `CREATE TABLE IF NOT EXISTS`
//!
//! `IF NOT EXISTS` answers "does this table exist", which stops being the
//! question the moment a column is added: the table exists, the statement does
//! nothing, and the new column is silently absent on every existing install.
//! An ordered runner keyed on SQLite's own `PRAGMA user_version` answers "which
//! changes has this file seen", which is the question that keeps being true
//!
//! # Why the version pragma rather than a table
//!
//! `user_version` is a 32-bit field in the database header. It cannot be
//! missing, cannot disagree with itself, and needs no bootstrap migration to
//! create the table that records migrations.

use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::paths::{PathError, create_dir_all};

/// One schema change.
///
/// Public so a test can build an old fixture schema and drive the runner over
/// it, which lets migrations preserve unknown data once version adapters exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Migration {
    /// The `user_version` this migration produces. Must be exactly one more
    /// than the previous entry.
    pub version: u32,
    /// Stable identifier used in logs. Not user-facing, not translated.
    pub name: &'static str,
    /// The statements to run, applied inside one transaction.
    pub sql: &'static str,
}

/// Every migration this build knows, in order.
///
/// Append only. An applied migration is a fact about databases in the wild;
/// editing one changes what a fresh install produces without changing what an
/// existing install has, and the two silently diverge.
pub const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    name: "initial-schema",
    sql: include_str!("migrations/0001-initial-schema.sql"),
}];

/// The `user_version` a fully migrated database has.
#[must_use]
pub fn latest_schema_version() -> u32 {
    MIGRATIONS.last().map_or(0, |migration| migration.version)
}

/// Failures opening, migrating, or querying the index.
#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    /// The database file could not be opened or created.
    #[error("could not open the launcher index at {path}")]
    Unopenable {
        path: PathBuf,
        #[source]
        source: Box<rusqlite::Error>,
    },
    /// A query or statement failed.
    #[error("the launcher index reported an error")]
    Query(#[from] Box<rusqlite::Error>),
    /// A migration failed and was rolled back.
    #[error("migration {version} ({name}) failed")]
    Migration {
        version: u32,
        name: &'static str,
        #[source]
        source: Box<rusqlite::Error>,
    },
    /// The file was written by a newer build.
    #[error("index schema version {found} is newer than the supported {supported}")]
    SchemaFromFuture { found: u32, supported: u32 },
    /// A stored value could not be interpreted, e.g. a column holding an enum
    /// name this build does not know.
    #[error("column `{column}` holds the unusable value `{value}`")]
    MalformedRow { column: &'static str, value: String },
    /// The state directory could not be created.
    #[error(transparent)]
    Path(#[from] PathError),
}

impl IndexError {
    /// The `ephervex-i18n` message id describing this failure to a user.
    #[must_use]
    pub const fn message_id(&self) -> &'static str {
        match self {
            Self::Unopenable { .. } => "error-index-unopenable",
            Self::Query(_) | Self::MalformedRow { .. } => "error-index-query-failed",
            Self::Migration { .. } => "error-index-migration-failed",
            Self::SchemaFromFuture { .. } => "error-index-schema-from-future",
            Self::Path(error) => error.message_id(),
        }
    }
}

impl From<rusqlite::Error> for IndexError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Query(Box::new(error))
    }
}

/// What a migration run did. Logged at startup so an upgrade is visible in a
/// bug report without asking the user to reproduce it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationOutcome {
    pub from: u32,
    pub to: u32,
    pub applied: Vec<&'static str>,
}

impl MigrationOutcome {
    /// Whether the database was already current.
    #[must_use]
    pub fn is_noop(&self) -> bool {
        self.applied.is_empty()
    }
}

/// The launcher index database.
#[derive(Debug)]
pub struct Index {
    connection: Connection,
}

impl Index {
    /// Opens (creating if needed) the index at `path` and migrates it.
    pub fn open(path: &Path) -> Result<Self, IndexError> {
        if let Some(parent) = path.parent() {
            create_dir_all(parent)?;
        }
        let connection = Connection::open(path).map_err(|source| IndexError::Unopenable {
            path: path.to_path_buf(),
            source: Box::new(source),
        })?;
        Self::from_connection(connection)
    }

    /// Opens an in-memory index. Used by tests and by any code path that wants
    /// a registry without a file.
    pub fn open_in_memory() -> Result<Self, IndexError> {
        let connection = Connection::open_in_memory().map_err(|source| IndexError::Unopenable {
            path: PathBuf::from(":memory:"),
            source: Box::new(source),
        })?;
        Self::from_connection(connection)
    }

    /// Wraps an existing connection, applying pragmas and migrations.
    pub fn from_connection(mut connection: Connection) -> Result<Self, IndexError> {
        // Foreign keys are off by default in SQLite and must be enabled per
        // connection. Without this the servers/instances link is decorative.
        connection.execute_batch("PRAGMA foreign_keys = ON; PRAGMA synchronous = NORMAL;")?;

        // Write-ahead logging keeps a long-running launch from blocking the
        // instance list. It reports the resulting mode as a row — which is
        // "memory" for an in-memory database — so it cannot go through
        // `execute_batch`.
        let _mode: String =
            connection.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;

        let outcome = apply_migrations(&mut connection, MIGRATIONS)?;
        if outcome.is_noop() {
            tracing::debug!(version = outcome.to, "launcher index schema is current");
        } else {
            tracing::info!(
                from = outcome.from,
                to = outcome.to,
                applied = ?outcome.applied,
                "migrated the launcher index"
            );
        }

        Ok(Self { connection })
    }

    /// The current `user_version`.
    pub fn schema_version(&self) -> Result<u32, IndexError> {
        read_user_version(&self.connection)
    }

    /// Borrows the underlying connection for the registry's queries.
    #[must_use]
    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    /// Mutably borrows the connection, for transactions.
    pub fn connection_mut(&mut self) -> &mut Connection {
        &mut self.connection
    }
}

/// Applies every migration newer than the database's `user_version`.
///
/// Each migration runs inside its own transaction together with the
/// `user_version` bump, so an interrupted upgrade leaves the database at a
/// version whose schema is actually present. A database newer than the supplied
/// list is refused rather than downgraded — guessing at a schema we do not know
/// is how data gets lost.
pub fn apply_migrations(
    connection: &mut Connection,
    migrations: &[Migration],
) -> Result<MigrationOutcome, IndexError> {
    debug_assert!(
        migrations.iter().enumerate().all(|(index, migration)| {
            u32::try_from(index + 1).is_ok_and(|expected| migration.version == expected)
        }),
        "migrations must be numbered 1..n with no gaps"
    );

    let from = read_user_version(connection)?;
    let latest = migrations.last().map_or(0, |migration| migration.version);

    if from > latest {
        return Err(IndexError::SchemaFromFuture {
            found: from,
            supported: latest,
        });
    }

    let mut applied = Vec::new();
    for migration in migrations.iter().filter(|m| m.version > from) {
        let transaction = connection.transaction()?;
        transaction
            .execute_batch(migration.sql)
            .map_err(|source| IndexError::Migration {
                version: migration.version,
                name: migration.name,
                source: Box::new(source),
            })?;
        // `user_version` does not accept a bound parameter, and the value is a
        // `u32` from a `const` list rather than anything user-supplied.
        transaction
            .pragma_update(None, "user_version", migration.version)
            .map_err(|source| IndexError::Migration {
                version: migration.version,
                name: migration.name,
                source: Box::new(source),
            })?;
        transaction.commit()?;
        applied.push(migration.name);
    }

    Ok(MigrationOutcome {
        from,
        to: read_user_version(connection)?,
        applied,
    })
}

fn read_user_version(connection: &Connection) -> Result<u32, IndexError> {
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    u32::try_from(version).map_err(|_| IndexError::MalformedRow {
        column: "user_version",
        value: version.to_string(),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn table_exists(connection: &Connection, name: &str) -> bool {
        connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [name],
                |row| row.get::<_, i64>(0),
            )
            .expect("query sqlite_master")
            > 0
    }

    #[test]
    fn the_migration_list_is_ordered_and_gapless() {
        // The runner relies on this, and a duplicate or skipped number would
        // otherwise only show up on someone else's machine.
        for (index, migration) in MIGRATIONS.iter().enumerate() {
            assert_eq!(
                usize::try_from(migration.version).unwrap(),
                index + 1,
                "migration `{}` is out of order",
                migration.name
            );
        }
        assert_eq!(
            latest_schema_version(),
            u32::try_from(MIGRATIONS.len()).unwrap()
        );
    }

    #[test]
    fn a_fresh_database_is_migrated_to_the_latest_schema() {
        let index = Index::open_in_memory().expect("open");
        assert_eq!(
            index.schema_version().expect("version"),
            latest_schema_version()
        );
        assert!(table_exists(index.connection(), "instances"));
        assert!(table_exists(index.connection(), "servers"));
    }

    #[test]
    fn migrations_are_not_reapplied_to_an_already_current_database() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("launcher.sqlite");

        let first = Index::open(&path).expect("first open");
        drop(first);

        // Re-running `CREATE TABLE` would fail here; the version check is what
        // makes reopening cheap and safe.
        let second = Index::open(&path).expect("second open");
        assert_eq!(
            second.schema_version().expect("version"),
            latest_schema_version()
        );
    }

    #[test]
    fn an_old_fixture_schema_is_migrated_forward_without_losing_its_rows() {
        // migrations must be exercised against a database built
        // the way an older release built it, not only against a fresh file.
        const LEGACY_FIXTURE: &str = "
            CREATE TABLE instances (
                id   TEXT PRIMARY KEY NOT NULL,
                name TEXT NOT NULL
            );
        ";
        const ADD_STORAGE_CLASS: &[Migration] = &[
            Migration {
                version: 1,
                name: "fixture-initial",
                sql: LEGACY_FIXTURE,
            },
            Migration {
                version: 2,
                name: "fixture-add-storage-class",
                sql: "ALTER TABLE instances
                      ADD COLUMN storage_class TEXT NOT NULL DEFAULT 'temporary';",
            },
        ];

        let mut connection = Connection::open_in_memory().expect("open");

        // Build the database exactly as the old release left it: schema at
        // version 1, with a row in it.
        let first = apply_migrations(&mut connection, &ADD_STORAGE_CLASS[..1]).expect("v1");
        assert_eq!(first.applied, vec!["fixture-initial"]);
        connection
            .execute(
                "INSERT INTO instances (id, name) VALUES ('abc', 'old repro')",
                [],
            )
            .expect("insert legacy row");

        // Now upgrade.
        let second = apply_migrations(&mut connection, ADD_STORAGE_CLASS).expect("v2");
        assert_eq!(second.from, 1);
        assert_eq!(second.to, 2);
        assert_eq!(
            second.applied,
            vec!["fixture-add-storage-class"],
            "only the unapplied migration should run"
        );

        let (name, storage): (String, String) = connection
            .query_row(
                "SELECT name, storage_class FROM instances WHERE id = 'abc'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("legacy row survived");
        assert_eq!(name, "old repro");
        assert_eq!(storage, "temporary");

        // Running again is a no-op.
        assert!(
            apply_migrations(&mut connection, ADD_STORAGE_CLASS)
                .expect("third run")
                .is_noop()
        );
    }

    #[test]
    fn a_failing_migration_is_rolled_back_and_the_version_is_not_advanced() {
        const BROKEN: &[Migration] = &[
            Migration {
                version: 1,
                name: "good",
                sql: "CREATE TABLE kept (id TEXT);",
            },
            Migration {
                version: 2,
                name: "broken",
                sql: "CREATE TABLE half (id TEXT); THIS IS NOT SQL;",
            },
        ];

        let mut connection = Connection::open_in_memory().expect("open");
        let error = apply_migrations(&mut connection, BROKEN).expect_err("must fail");
        assert!(matches!(error, IndexError::Migration { version: 2, .. }));
        assert_eq!(error.message_id(), "error-index-migration-failed");

        assert_eq!(read_user_version(&connection).expect("version"), 1);
        assert!(table_exists(&connection, "kept"));
        assert!(
            !table_exists(&connection, "half"),
            "the failed migration's partial work must be rolled back"
        );
    }

    #[test]
    fn a_database_from_a_newer_build_is_refused_rather_than_downgraded() {
        let mut connection = Connection::open_in_memory().expect("open");
        connection
            .pragma_update(None, "user_version", 999)
            .expect("set version");

        let error = apply_migrations(&mut connection, MIGRATIONS).expect_err("must refuse");
        assert!(matches!(
            error,
            IndexError::SchemaFromFuture {
                found: 999,
                supported: 1
            }
        ));
        assert_eq!(error.message_id(), "error-index-schema-from-future");
    }

    #[test]
    fn deleting_a_client_leaves_its_linked_servers_rather_than_cascading() {
        // A user who deletes a client instance has not asked for the servers
        // they built from it to disappear.
        let index = Index::open_in_memory().expect("open");
        let connection = index.connection();
        connection
            .execute(
                "INSERT INTO instances
                 (id, name, root, storage_class, minecraft_version, loader_kind,
                  source_kind, created_at, updated_at)
                 VALUES ('client', 'c', '/x', 'temporary', '1.20.1', 'vanilla',
                         'manual', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
                [],
            )
            .expect("insert client");
        connection
            .execute(
                "INSERT INTO servers
                 (id, name, root, storage_class, linked_client_id, minecraft_version,
                  loader_kind, network, source_kind, created_at, updated_at)
                 VALUES ('server', 's', '/y', 'temporary', 'client', '1.20.1',
                         'vanilla', '{}', 'manual', '2026-01-01T00:00:00Z',
                         '2026-01-01T00:00:00Z')",
                [],
            )
            .expect("insert server");

        connection
            .execute("DELETE FROM instances WHERE id = 'client'", [])
            .expect("delete client");

        let linked: Option<String> = connection
            .query_row(
                "SELECT linked_client_id FROM servers WHERE id = 'server'",
                [],
                |row| row.get(0),
            )
            .expect("server still exists");
        assert_eq!(linked, None, "the link is cleared, the server is kept");
    }
}
