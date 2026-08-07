//! Create, read, list, update, and delete instances and servers.
//!
//! The registry is the only thing in the workspace that writes to both halves
//! of ADR 0002 — the `.launcher/*.json` metadata and the SQLite index — and it
//! always writes them in that order. A crash between the two leaves a directory
//! that [`crate::reconcile`] will pick up and re-index on the next start; the
//! reverse order would leave an index row pointing at a directory that was
//! never created.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use ephervex_model::{
    AttentionReason, ImportSourceKind, InstanceId, LoaderKind, LoaderSelection, ServerId,
    StorageClass,
};
use rusqlite::{Row, params};

use crate::index::{Index, IndexError};
use crate::metadata::{self, MetadataError};
use crate::paths::{PathError, Paths, create_dir_all};
use crate::records::{
    ClientRecord, FileDisposition, ImportSource, JavaOverride, JvmSettings, NewClient, NewServer,
    ServerNetworkSettings, ServerRecord,
};

const CLIENT_COLUMNS: &str = "id, name, root, storage_class, minecraft_version, loader_kind, \
     loader_version, source_kind, source_reference, java_override, jvm_override, attention, \
     created_at, updated_at, last_launched_at";

const SERVER_COLUMNS: &str = "id, name, root, storage_class, linked_client_id, minecraft_version, \
     loader_kind, loader_version, network, eula_accepted, source_kind, source_reference, \
     java_override, jvm_override, attention, created_at, updated_at, last_started_at";

/// Failures from registry operations.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    /// No instance with that id is indexed.
    #[error("no instance with id {id}")]
    InstanceUnknown { id: InstanceId },
    /// No server with that id is indexed.
    #[error("no server with id {id}")]
    ServerUnknown { id: ServerId },
    /// The directory chosen for a new instance already has contents.
    #[error("the instance directory {path} already contains files")]
    RootOccupied { path: PathBuf },
    /// A filesystem operation failed.
    #[error("filesystem error at {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Index(#[from] IndexError),
    #[error(transparent)]
    Metadata(#[from] MetadataError),
    #[error(transparent)]
    Path(#[from] PathError),
}

impl RegistryError {
    /// The `ephervex-i18n` message id describing this failure to a user.
    #[must_use]
    pub const fn message_id(&self) -> &'static str {
        match self {
            Self::InstanceUnknown { .. } => "error-registry-instance-unknown",
            Self::ServerUnknown { .. } => "error-registry-server-unknown",
            Self::RootOccupied { .. } => "error-registry-root-occupied",
            Self::Io { .. } => "error-paths-directory-creation",
            Self::Index(error) => error.message_id(),
            Self::Metadata(error) => error.message_id(),
            Self::Path(error) => error.message_id(),
        }
    }
}

impl From<rusqlite::Error> for RegistryError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Index(IndexError::from(error))
    }
}

/// The instance and server registry.
///
/// Owns the resolved [`Paths`] rather than re-deriving them, so a process
/// cannot end up creating instances in one place and looking for them in
/// another.
#[derive(Debug)]
pub struct Registry {
    paths: Paths,
    index: Index,
}

impl Registry {
    /// Opens the registry against the database named by `paths`.
    ///
    /// Migrations run here. Startup reconciliation does not — that is a
    /// separate, explicitly invoked step so that a caller which only wants to
    /// read cannot accidentally mutate the index (see [`crate::reconcile`]).
    pub fn open(paths: Paths) -> Result<Self, RegistryError> {
        let index = Index::open(&paths.database_file())?;
        Ok(Self { paths, index })
    }

    /// Opens a registry backed by an in-memory index.
    ///
    /// Instances still land on disk under `paths`; only the index is
    /// throwaway. Used by tests and by "inspect this directory without
    /// touching my real index" tooling.
    pub fn open_in_memory(paths: Paths) -> Result<Self, RegistryError> {
        Ok(Self {
            paths,
            index: Index::open_in_memory()?,
        })
    }

    /// The resolved application paths this registry writes through.
    #[must_use]
    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    /// The underlying index, for schema inspection.
    #[must_use]
    pub fn index(&self) -> &Index {
        &self.index
    }

    // ---------------------------------------------------------------- clients

    /// Creates a client instance: directory, metadata, then index row.
    pub fn create_client(&self, request: NewClient) -> Result<ClientRecord, RegistryError> {
        let id = InstanceId::new();
        let root = self.paths.instance_root(request.storage, id);
        ensure_free_root(&root)?;

        let now = Utc::now();
        let record = ClientRecord {
            id,
            name: request.name,
            root: root.clone(),
            storage: request.storage,
            minecraft_version: request.minecraft_version,
            loader: request.loader,
            source: request.source,
            java_override: None,
            jvm_override: None,
            attention: request.attention,
            created_at: now,
            updated_at: now,
            last_launched_at: None,
        };

        create_dir_all(&metadata::client_game_dir(&root))?;
        // Metadata first. See the module documentation.
        metadata::write_client_metadata(&root, &record)?;
        self.insert_client_row(&record)?;

        tracing::info!(
            instance_id = %record.id,
            storage = %record.storage,
            root = %record.root.display(),
            "created client instance"
        );
        Ok(record)
    }

    /// Adds an already-existing instance directory to the index.
    ///
    /// Used by reconciliation when a `.launcher/instance.json` is found that
    /// the database does not know about — the recovery path ADR 0002 exists
    /// for. The metadata file is left exactly as found.
    pub fn index_existing_client(&self, record: &ClientRecord) -> Result<(), RegistryError> {
        self.insert_client_row(record)
    }

    /// Reads one client instance.
    pub fn client(&self, id: InstanceId) -> Result<ClientRecord, RegistryError> {
        self.index
            .connection()
            .query_row(
                &format!("SELECT {CLIENT_COLUMNS} FROM instances WHERE id = ?1"),
                params![id.to_string()],
                client_from_row,
            )
            .map_err(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => RegistryError::InstanceUnknown { id },
                other => RegistryError::from(other),
            })?
            .map_err(RegistryError::from)
    }

    /// Lists every indexed client instance, newest first.
    pub fn list_clients(&self) -> Result<Vec<ClientRecord>, RegistryError> {
        let connection = self.index.connection();
        let mut statement = connection.prepare(&format!(
            "SELECT {CLIENT_COLUMNS} FROM instances ORDER BY created_at DESC, id"
        ))?;
        let rows = statement.query_map([], client_from_row)?;

        let mut records = Vec::new();
        for row in rows {
            records.push(row??);
        }
        Ok(records)
    }

    /// Persists changes to a client instance.
    ///
    /// `updated_at` is stamped here rather than by the caller so it cannot be
    /// forgotten. The metadata file is refreshed only when the root still
    /// exists — a persistent instance whose directory vanished keeps its index
    /// row and its warning, and must not fail the update that records that
    /// warning.
    pub fn update_client(&self, record: &mut ClientRecord) -> Result<(), RegistryError> {
        record.updated_at = Utc::now();

        if record.root.is_dir() {
            metadata::write_client_metadata(&record.root, record)?;
        } else {
            tracing::debug!(
                instance_id = %record.id,
                root = %record.root.display(),
                "instance root is missing; updating the index only"
            );
        }

        let changed = self.index.connection().execute(
            "UPDATE instances SET
                name = ?2, root = ?3, storage_class = ?4, minecraft_version = ?5,
                loader_kind = ?6, loader_version = ?7, source_kind = ?8, source_reference = ?9,
                java_override = ?10, jvm_override = ?11, attention = ?12,
                updated_at = ?13, last_launched_at = ?14
             WHERE id = ?1",
            params![
                record.id.to_string(),
                record.name,
                path_to_sql(&record.root),
                storage_class_to_sql(record.storage),
                record.minecraft_version,
                loader_kind_to_sql(record.loader.kind),
                record.loader.version,
                source_kind_to_sql(record.source.kind),
                record.source.reference,
                to_json_option(record.java_override.as_ref())?,
                to_json_option(record.jvm_override.as_ref())?,
                to_json(&record.attention)?,
                timestamp_to_sql(record.updated_at),
                record.last_launched_at.map(timestamp_to_sql),
            ],
        )?;

        if changed == 0 {
            return Err(RegistryError::InstanceUnknown { id: record.id });
        }
        Ok(())
    }

    /// Removes a client instance from the index, and optionally from disk.
    ///
    /// Deleting files is never inferred: [`FileDisposition::Keep`] is what
    /// reconciliation uses when the directory is already gone, and what
    /// "forget this instance" uses when the user wants to keep the files
    /// (ADR 0006).
    pub fn delete_client(
        &self,
        id: InstanceId,
        files: FileDisposition,
    ) -> Result<(), RegistryError> {
        let record = self.client(id)?;

        if files == FileDisposition::Remove {
            remove_tree(&record.root)?;
        }

        self.index.connection().execute(
            "DELETE FROM instances WHERE id = ?1",
            params![id.to_string()],
        )?;

        tracing::info!(instance_id = %id, ?files, "removed client instance from the index");
        Ok(())
    }

    /// The servers linked to a client instance.
    ///
    /// Derived from the servers table rather than stored on the client, so it
    /// cannot go stale (see [`ClientRecord`]).
    pub fn linked_server_ids(&self, client: InstanceId) -> Result<Vec<ServerId>, RegistryError> {
        let connection = self.index.connection();
        let mut statement = connection
            .prepare("SELECT id FROM servers WHERE linked_client_id = ?1 ORDER BY name")?;
        let rows =
            statement.query_map(params![client.to_string()], |row| row.get::<_, String>(0))?;

        let mut ids = Vec::new();
        for row in rows {
            let raw = row?;
            ids.push(parse_id(&raw, "servers.id").map_err(RegistryError::from)?);
        }
        Ok(ids)
    }

    // ---------------------------------------------------------------- servers

    /// Creates a server: directory, metadata, then index row.
    pub fn create_server(&self, request: NewServer) -> Result<ServerRecord, RegistryError> {
        let id = ServerId::new();
        let root = self.paths.server_root(request.storage, id);
        ensure_free_root(&root)?;

        let now = Utc::now();
        let record = ServerRecord {
            id,
            name: request.name,
            root: root.clone(),
            storage: request.storage,
            linked_client_id: request.linked_client_id,
            minecraft_version: request.minecraft_version,
            loader: request.loader,
            network: request.network,
            // Never pre-accepted. The user accepts the EULA globally and
            // explicitly, and only then does a new server inherit it
            eula_accepted: false,
            source: request.source,
            java_override: None,
            jvm_override: None,
            attention: request.attention,
            created_at: now,
            updated_at: now,
            last_started_at: None,
        };

        create_dir_all(&metadata::server_game_dir(&root))?;
        metadata::write_server_metadata(&root, &record)?;
        self.insert_server_row(&record)?;

        tracing::info!(
            server_id = %record.id,
            storage = %record.storage,
            root = %record.root.display(),
            "created server"
        );
        Ok(record)
    }

    /// Adds an already-existing server directory to the index.
    pub fn index_existing_server(&self, record: &ServerRecord) -> Result<(), RegistryError> {
        self.insert_server_row(record)
    }

    /// Reads one server.
    pub fn server(&self, id: ServerId) -> Result<ServerRecord, RegistryError> {
        self.index
            .connection()
            .query_row(
                &format!("SELECT {SERVER_COLUMNS} FROM servers WHERE id = ?1"),
                params![id.to_string()],
                server_from_row,
            )
            .map_err(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => RegistryError::ServerUnknown { id },
                other => RegistryError::from(other),
            })?
            .map_err(RegistryError::from)
    }

    /// Lists every indexed server, newest first.
    pub fn list_servers(&self) -> Result<Vec<ServerRecord>, RegistryError> {
        let connection = self.index.connection();
        let mut statement = connection.prepare(&format!(
            "SELECT {SERVER_COLUMNS} FROM servers ORDER BY created_at DESC, id"
        ))?;
        let rows = statement.query_map([], server_from_row)?;

        let mut records = Vec::new();
        for row in rows {
            records.push(row??);
        }
        Ok(records)
    }

    /// Persists changes to a server.
    pub fn update_server(&self, record: &mut ServerRecord) -> Result<(), RegistryError> {
        record.updated_at = Utc::now();

        if record.root.is_dir() {
            metadata::write_server_metadata(&record.root, record)?;
        }

        let changed = self.index.connection().execute(
            "UPDATE servers SET
                name = ?2, root = ?3, storage_class = ?4, linked_client_id = ?5,
                minecraft_version = ?6, loader_kind = ?7, loader_version = ?8, network = ?9,
                eula_accepted = ?10, source_kind = ?11, source_reference = ?12,
                java_override = ?13, jvm_override = ?14, attention = ?15,
                updated_at = ?16, last_started_at = ?17
             WHERE id = ?1",
            params![
                record.id.to_string(),
                record.name,
                path_to_sql(&record.root),
                storage_class_to_sql(record.storage),
                record.linked_client_id.map(|id| id.to_string()),
                record.minecraft_version,
                loader_kind_to_sql(record.loader.kind),
                record.loader.version,
                to_json(&record.network)?,
                record.eula_accepted,
                source_kind_to_sql(record.source.kind),
                record.source.reference,
                to_json_option(record.java_override.as_ref())?,
                to_json_option(record.jvm_override.as_ref())?,
                to_json(&record.attention)?,
                timestamp_to_sql(record.updated_at),
                record.last_started_at.map(timestamp_to_sql),
            ],
        )?;

        if changed == 0 {
            return Err(RegistryError::ServerUnknown { id: record.id });
        }
        Ok(())
    }

    /// Removes a server from the index, and optionally from disk.
    pub fn delete_server(&self, id: ServerId, files: FileDisposition) -> Result<(), RegistryError> {
        let record = self.server(id)?;

        if files == FileDisposition::Remove {
            remove_tree(&record.root)?;
        }

        self.index
            .connection()
            .execute("DELETE FROM servers WHERE id = ?1", params![id.to_string()])?;

        tracing::info!(server_id = %id, ?files, "removed server from the index");
        Ok(())
    }

    // ------------------------------------------------------------- statements

    fn insert_client_row(&self, record: &ClientRecord) -> Result<(), RegistryError> {
        self.index.connection().execute(
            "INSERT INTO instances
             (id, name, root, storage_class, minecraft_version, loader_kind, loader_version,
              source_kind, source_reference, java_override, jvm_override, attention,
              created_at, updated_at, last_launched_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                record.id.to_string(),
                record.name,
                path_to_sql(&record.root),
                storage_class_to_sql(record.storage),
                record.minecraft_version,
                loader_kind_to_sql(record.loader.kind),
                record.loader.version,
                source_kind_to_sql(record.source.kind),
                record.source.reference,
                to_json_option(record.java_override.as_ref())?,
                to_json_option(record.jvm_override.as_ref())?,
                to_json(&record.attention)?,
                timestamp_to_sql(record.created_at),
                timestamp_to_sql(record.updated_at),
                record.last_launched_at.map(timestamp_to_sql),
            ],
        )?;
        Ok(())
    }

    fn insert_server_row(&self, record: &ServerRecord) -> Result<(), RegistryError> {
        self.index.connection().execute(
            "INSERT INTO servers
             (id, name, root, storage_class, linked_client_id, minecraft_version, loader_kind,
              loader_version, network, eula_accepted, source_kind, source_reference,
              java_override, jvm_override, attention, created_at, updated_at, last_started_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
            params![
                record.id.to_string(),
                record.name,
                path_to_sql(&record.root),
                storage_class_to_sql(record.storage),
                record.linked_client_id.map(|id| id.to_string()),
                record.minecraft_version,
                loader_kind_to_sql(record.loader.kind),
                record.loader.version,
                to_json(&record.network)?,
                record.eula_accepted,
                source_kind_to_sql(record.source.kind),
                record.source.reference,
                to_json_option(record.java_override.as_ref())?,
                to_json_option(record.jvm_override.as_ref())?,
                to_json(&record.attention)?,
                timestamp_to_sql(record.created_at),
                timestamp_to_sql(record.updated_at),
                record.last_started_at.map(timestamp_to_sql),
            ],
        )?;
        Ok(())
    }
}

// ------------------------------------------------------------- row conversion
//
// Rows come back as `Result<Result<Record, IndexError>, rusqlite::Error>`: the
// outer error is SQLite's, the inner one is ours for a row SQLite handed over
// happily but this build cannot interpret — an enum name from a newer version,
// say. Collapsing the two would mean reporting "database error" for what is
// really "this row was written by a different Ephervex".

fn client_from_row(row: &Row<'_>) -> rusqlite::Result<Result<ClientRecord, IndexError>> {
    let id: String = row.get(0)?;
    let storage: String = row.get(3)?;
    let loader_kind: String = row.get(5)?;
    let source_kind: String = row.get(7)?;
    let java_override: Option<String> = row.get(9)?;
    let jvm_override: Option<String> = row.get(10)?;
    let attention: String = row.get(11)?;
    let created_at: String = row.get(12)?;
    let updated_at: String = row.get(13)?;
    let last_launched_at: Option<String> = row.get(14)?;

    let record = (|| {
        Ok(ClientRecord {
            id: parse_id(&id, "instances.id")?,
            name: row.get(1).unwrap_or_default(),
            root: PathBuf::from(row.get::<_, String>(2).unwrap_or_default()),
            storage: storage_class_from_sql(&storage)?,
            minecraft_version: row.get(4).unwrap_or_default(),
            loader: LoaderSelection {
                kind: loader_kind_from_sql(&loader_kind)?,
                version: row.get(6).unwrap_or_default(),
            },
            source: ImportSource {
                kind: source_kind_from_sql(&source_kind)?,
                reference: row.get(8).unwrap_or_default(),
            },
            java_override: from_json_option::<JavaOverride>(
                java_override.as_deref(),
                "instances.java_override",
            )?,
            jvm_override: from_json_option::<JvmSettings>(
                jvm_override.as_deref(),
                "instances.jvm_override",
            )?,
            attention: from_json::<Vec<AttentionReason>>(&attention, "instances.attention")?,
            created_at: timestamp_from_sql(&created_at, "instances.created_at")?,
            updated_at: timestamp_from_sql(&updated_at, "instances.updated_at")?,
            last_launched_at: last_launched_at
                .as_deref()
                .map(|value| timestamp_from_sql(value, "instances.last_launched_at"))
                .transpose()?,
        })
    })();

    Ok(record)
}

fn server_from_row(row: &Row<'_>) -> rusqlite::Result<Result<ServerRecord, IndexError>> {
    let id: String = row.get(0)?;
    let storage: String = row.get(3)?;
    let linked_client_id: Option<String> = row.get(4)?;
    let loader_kind: String = row.get(6)?;
    let network: String = row.get(8)?;
    let source_kind: String = row.get(10)?;
    let java_override: Option<String> = row.get(12)?;
    let jvm_override: Option<String> = row.get(13)?;
    let attention: String = row.get(14)?;
    let created_at: String = row.get(15)?;
    let updated_at: String = row.get(16)?;
    let last_started_at: Option<String> = row.get(17)?;

    let record = (|| {
        Ok(ServerRecord {
            id: parse_id(&id, "servers.id")?,
            name: row.get(1).unwrap_or_default(),
            root: PathBuf::from(row.get::<_, String>(2).unwrap_or_default()),
            storage: storage_class_from_sql(&storage)?,
            linked_client_id: linked_client_id
                .as_deref()
                .map(|value| parse_id(value, "servers.linked_client_id"))
                .transpose()?,
            minecraft_version: row.get(5).unwrap_or_default(),
            loader: LoaderSelection {
                kind: loader_kind_from_sql(&loader_kind)?,
                version: row.get(7).unwrap_or_default(),
            },
            network: from_json::<ServerNetworkSettings>(&network, "servers.network")?,
            eula_accepted: row.get(9).unwrap_or_default(),
            source: ImportSource {
                kind: source_kind_from_sql(&source_kind)?,
                reference: row.get(11).unwrap_or_default(),
            },
            java_override: from_json_option::<JavaOverride>(
                java_override.as_deref(),
                "servers.java_override",
            )?,
            jvm_override: from_json_option::<JvmSettings>(
                jvm_override.as_deref(),
                "servers.jvm_override",
            )?,
            attention: from_json::<Vec<AttentionReason>>(&attention, "servers.attention")?,
            created_at: timestamp_from_sql(&created_at, "servers.created_at")?,
            updated_at: timestamp_from_sql(&updated_at, "servers.updated_at")?,
            last_started_at: last_started_at
                .as_deref()
                .map(|value| timestamp_from_sql(value, "servers.last_started_at"))
                .transpose()?,
        })
    })();

    Ok(record)
}

// ------------------------------------------------------------------- encoding
//
// Enums are written as stable lowercase identifiers chosen here rather than via
// `Display`, which renders product names ("NeoForge") for the UI. The two must
// be allowed to diverge: renaming a product must never rewrite a database.

const fn storage_class_to_sql(value: StorageClass) -> &'static str {
    match value {
        StorageClass::Temporary => "temporary",
        StorageClass::Persistent => "persistent",
    }
}

fn storage_class_from_sql(value: &str) -> Result<StorageClass, IndexError> {
    match value {
        "temporary" => Ok(StorageClass::Temporary),
        "persistent" => Ok(StorageClass::Persistent),
        other => Err(IndexError::MalformedRow {
            column: "storage_class",
            value: other.to_owned(),
        }),
    }
}

const fn loader_kind_to_sql(value: LoaderKind) -> &'static str {
    match value {
        LoaderKind::Vanilla => "vanilla",
        LoaderKind::Forge => "forge",
        LoaderKind::NeoForge => "neoforge",
    }
}

fn loader_kind_from_sql(value: &str) -> Result<LoaderKind, IndexError> {
    match value {
        "vanilla" => Ok(LoaderKind::Vanilla),
        "forge" => Ok(LoaderKind::Forge),
        "neoforge" => Ok(LoaderKind::NeoForge),
        other => Err(IndexError::MalformedRow {
            column: "loader_kind",
            value: other.to_owned(),
        }),
    }
}

const fn source_kind_to_sql(value: ImportSourceKind) -> &'static str {
    match value {
        ImportSourceKind::Manual => "manual",
        ImportSourceKind::ModrinthPack => "modrinth_pack",
        ImportSourceKind::CurseForgePack => "curseforge_pack",
        ImportSourceKind::PrismExport => "prism_export",
    }
}

fn source_kind_from_sql(value: &str) -> Result<ImportSourceKind, IndexError> {
    match value {
        "manual" => Ok(ImportSourceKind::Manual),
        "modrinth_pack" => Ok(ImportSourceKind::ModrinthPack),
        "curseforge_pack" => Ok(ImportSourceKind::CurseForgePack),
        "prism_export" => Ok(ImportSourceKind::PrismExport),
        other => Err(IndexError::MalformedRow {
            column: "source_kind",
            value: other.to_owned(),
        }),
    }
}

fn path_to_sql(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn timestamp_to_sql(value: DateTime<Utc>) -> String {
    value.to_rfc3339()
}

fn timestamp_from_sql(value: &str, column: &'static str) -> Result<DateTime<Utc>, IndexError> {
    DateTime::parse_from_rfc3339(value)
        .map(|parsed| parsed.with_timezone(&Utc))
        .map_err(|_| IndexError::MalformedRow {
            column,
            value: value.to_owned(),
        })
}

fn parse_id<T: std::str::FromStr>(value: &str, column: &'static str) -> Result<T, IndexError> {
    value.parse().map_err(|_| IndexError::MalformedRow {
        column,
        value: value.to_owned(),
    })
}

fn to_json<T: serde::Serialize>(value: &T) -> Result<String, IndexError> {
    serde_json::to_string(value).map_err(|error| IndexError::MalformedRow {
        column: "json",
        value: error.to_string(),
    })
}

fn to_json_option<T: serde::Serialize>(value: Option<&T>) -> Result<Option<String>, IndexError> {
    value.map(to_json).transpose()
}

fn from_json<T: serde::de::DeserializeOwned>(
    value: &str,
    column: &'static str,
) -> Result<T, IndexError> {
    serde_json::from_str(value).map_err(|_| IndexError::MalformedRow {
        column,
        value: value.to_owned(),
    })
}

fn from_json_option<T: serde::de::DeserializeOwned>(
    value: Option<&str>,
    column: &'static str,
) -> Result<Option<T>, IndexError> {
    value.map(|raw| from_json(raw, column)).transpose()
}

/// Refuses to build an instance on top of somebody else's files.
///
/// An id collision is astronomically unlikely; a stale directory left by a
/// previous run at the same id is not, and silently merging into it would
/// produce an instance that reproduces the wrong thing.
fn ensure_free_root(root: &Path) -> Result<(), RegistryError> {
    match std::fs::read_dir(root) {
        Ok(mut entries) => {
            if entries.next().is_some() {
                Err(RegistryError::RootOccupied {
                    path: root.to_path_buf(),
                })
            } else {
                Ok(())
            }
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(RegistryError::Io {
            path: root.to_path_buf(),
            source,
        }),
    }
}

/// Removes a directory tree, treating "already gone" as success.
///
/// Reconciliation and user deletion both race with the OS clearing the
/// temporary root, and losing that race is not an error (ADR 0006).
fn remove_tree(root: &Path) -> Result<(), RegistryError> {
    match std::fs::remove_dir_all(root) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(RegistryError::Io {
            path: root.to_path_buf(),
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use ephervex_model::LoaderKind;

    use crate::test_support::TestEnvironment;

    #[test]
    fn a_created_instance_is_readable_listable_and_on_disk() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();

        let record = registry
            .create_client(NewClient::new(
                "Crash repro",
                "1.20.1",
                LoaderSelection::exact(LoaderKind::NeoForge, "47.1.106"),
            ))
            .expect("create");

        assert_eq!(registry.client(record.id).expect("read"), record);
        assert_eq!(registry.list_clients().expect("list"), vec![record.clone()]);
        assert!(metadata::client_metadata_path(&record.root).is_file());
        assert!(metadata::client_game_dir(&record.root).is_dir());
    }

    #[test]
    fn the_metadata_file_alone_is_enough_to_rebuild_the_record() {
        // ADR 0002: the index is a cache of authority, not the authority.
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let record = registry
            .create_client(
                NewClient::new(
                    "Persistent",
                    "1.12.2",
                    LoaderSelection::exact(LoaderKind::Forge, "14.23.5.2859"),
                )
                .with_storage(StorageClass::Persistent),
            )
            .expect("create");

        let from_disk = metadata::read_client_metadata(&record.root).expect("read metadata");
        assert_eq!(from_disk, record);
    }

    #[test]
    fn instances_default_to_the_temporary_root_and_persistent_ones_do_not() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();

        let temporary = registry
            .create_client(NewClient::new("temp", "1.20.1", LoaderSelection::vanilla()))
            .expect("create");
        let persistent = registry
            .create_client(
                NewClient::new("kept", "1.20.1", LoaderSelection::vanilla())
                    .with_storage(StorageClass::Persistent),
            )
            .expect("create");

        assert_eq!(temporary.storage, StorageClass::Temporary);
        assert!(
            temporary
                .root
                .starts_with(registry.paths().temporary_instances_dir())
        );
        assert!(
            persistent
                .root
                .starts_with(registry.paths().persistent_instances_dir())
        );
    }

    #[test]
    fn updates_are_persisted_to_both_the_index_and_the_metadata_file() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let mut record = registry
            .create_client(NewClient::new(
                "before",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create");

        record.name = "after".to_owned();
        record.add_attention(AttentionReason::BestEffortServerConversion);
        registry.update_client(&mut record).expect("update");

        assert_eq!(registry.client(record.id).expect("read").name, "after");
        assert_eq!(
            metadata::read_client_metadata(&record.root)
                .expect("metadata")
                .attention,
            record.attention
        );
    }

    #[test]
    fn deleting_an_instance_can_keep_or_remove_its_files() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();

        let kept = registry
            .create_client(NewClient::new("kept", "1.20.1", LoaderSelection::vanilla()))
            .expect("create");
        registry
            .delete_client(kept.id, FileDisposition::Keep)
            .expect("forget");
        assert!(kept.root.is_dir(), "files must survive a forget");
        assert!(matches!(
            registry.client(kept.id),
            Err(RegistryError::InstanceUnknown { .. })
        ));

        let removed = registry
            .create_client(NewClient::new(
                "removed",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create");
        registry
            .delete_client(removed.id, FileDisposition::Remove)
            .expect("delete");
        assert!(!removed.root.exists());
    }

    #[test]
    fn several_servers_may_link_to_one_client() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();

        let client = registry
            .create_client(NewClient::new(
                "client",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create client");
        let first = registry
            .create_server(
                NewServer::new("server a", "1.20.1", LoaderSelection::vanilla())
                    .linked_to(client.id),
            )
            .expect("create server");
        let second = registry
            .create_server(
                NewServer::new("server b", "1.20.1", LoaderSelection::vanilla())
                    .linked_to(client.id),
            )
            .expect("create server");
        registry
            .create_server(NewServer::new(
                "standalone",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create standalone");

        let mut linked = registry.linked_server_ids(client.id).expect("linked");
        linked.sort_unstable();
        let mut expected = vec![first.id, second.id];
        expected.sort_unstable();
        assert_eq!(linked, expected);
        assert_eq!(registry.list_servers().expect("list").len(), 3);
    }

    #[test]
    fn a_new_server_never_starts_with_the_eula_accepted() {
        // the launcher must never claim the user agreed.
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let server = registry
            .create_server(NewServer::new("test", "1.20.1", LoaderSelection::vanilla()))
            .expect("create");
        assert!(!server.eula_accepted);
        assert!(!server.network.online_mode);
    }

    #[test]
    fn records_survive_a_registry_restart_because_they_are_in_the_database() {
        let environment = TestEnvironment::new();
        let record = {
            let registry = environment.registry();
            registry
                .create_client(
                    NewClient::new("kept", "1.20.1", LoaderSelection::vanilla())
                        .with_storage(StorageClass::Persistent),
                )
                .expect("create")
        };

        let reopened = environment.registry();
        assert_eq!(reopened.client(record.id).expect("read"), record);
    }

    #[test]
    fn an_unknown_id_is_reported_as_unknown_rather_than_as_a_database_error() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let error = registry
            .client(InstanceId::new())
            .expect_err("nothing there");
        assert!(matches!(error, RegistryError::InstanceUnknown { .. }));
        assert_eq!(error.message_id(), "error-registry-instance-unknown");
    }

    #[test]
    fn a_row_this_build_cannot_interpret_is_reported_as_such() {
        // A database written by a build that knows a loader we do not must not
        // read back as "database error".
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let record = registry
            .create_client(NewClient::new("x", "1.20.1", LoaderSelection::vanilla()))
            .expect("create");

        registry
            .index()
            .connection()
            .execute(
                "UPDATE instances SET loader_kind = 'fabric' WHERE id = ?1",
                params![record.id.to_string()],
            )
            .expect("tamper");

        let error = registry.client(record.id).expect_err("must fail");
        assert!(matches!(
            error,
            RegistryError::Index(IndexError::MalformedRow {
                column: "loader_kind",
                ..
            })
        ));
    }

    #[test]
    fn enum_encodings_are_stable_and_independent_of_display_names() {
        // `LoaderKind::NeoForge` displays as the product name "NeoForge"; the
        // database must not depend on that.
        assert_eq!(loader_kind_to_sql(LoaderKind::NeoForge), "neoforge");
        assert_eq!(
            loader_kind_from_sql("neoforge").unwrap(),
            LoaderKind::NeoForge
        );
        assert_eq!(storage_class_to_sql(StorageClass::Temporary), "temporary");
        assert_eq!(
            source_kind_to_sql(ImportSourceKind::CurseForgePack),
            "curseforge_pack"
        );
        assert!(loader_kind_from_sql("fabric").is_err());
    }
}
