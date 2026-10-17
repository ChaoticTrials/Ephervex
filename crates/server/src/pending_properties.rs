//! Property values saved while the server runs, written again just before its
//! next process starts.
//!
//! Minecraft reads `server.properties` once at startup and writes its own copy
//! back when a command such as `/whitelist on` changes a setting, so a value
//! saved into the file while it runs could be lost before it is ever read. The
//! value is written to the file at once and also kept here until a new process
//! is about to start.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::eula::atomic_write;
use crate::{PropertiesDocument, ServerError, ServerResult};

pub const PENDING_PROPERTIES_FILE: &str = "server-properties-pending.json";
pub const PENDING_PROPERTIES_FORMAT_VERSION: u32 = 1;
const PROPERTIES_LOCK_FILE: &str = "server-properties.lock";

/// Values saved while a process ran that no process has read yet.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PendingProperties {
    values: BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize)]
struct PendingDocument {
    format: u32,
    values: BTreeMap<String, String>,
}

impl PendingProperties {
    /// Reads what `server_root` keeps; nothing kept is an empty set.
    pub fn read(server_root: &Path) -> ServerResult<Self> {
        let path = pending_properties_path(server_root);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(source) => return Err(ServerError::Io { path, source }),
        };
        match serde_json::from_slice::<PendingDocument>(&bytes) {
            Ok(document) if document.format <= PENDING_PROPERTIES_FORMAT_VERSION => Ok(Self {
                values: document.values,
            }),
            _ => Err(ServerError::PendingPropertiesUnreadable { path }),
        }
    }

    /// Replaces what `server_root` keeps, removing the file once nothing is left.
    pub fn store(&self, server_root: &Path) -> ServerResult<()> {
        let path = pending_properties_path(server_root);
        if self.values.is_empty() {
            return match std::fs::remove_file(&path) {
                Err(source) if source.kind() != std::io::ErrorKind::NotFound => {
                    Err(ServerError::Io { path, source })
                }
                _ => Ok(()),
            };
        }
        let body = serde_json::to_vec(&PendingDocument {
            format: PENDING_PROPERTIES_FORMAT_VERSION,
            values: self.values.clone(),
        })
        .map_err(|error| ServerError::Io {
            path: path.clone(),
            source: std::io::Error::other(error),
        })?;
        atomic_write(&path, &body)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.values.keys().map(String::as_str)
    }

    /// Sets every kept value in `document`.
    pub fn apply_to(&self, document: &mut PropertiesDocument) -> ServerResult<()> {
        for (key, value) in &self.values {
            document.set(key, value)?;
        }
        Ok(())
    }
}

/// Serializes every read-modify-write of a server's `server.properties` that
/// involves kept values, whichever process makes it.
#[derive(Debug)]
pub struct PropertiesLock {
    _file: std::fs::File,
}

impl PropertiesLock {
    pub fn acquire(server_root: &Path) -> ServerResult<Self> {
        use rustix::fs::{FlockOperation, flock};

        let path = server_root.join(".launcher").join(PROPERTIES_LOCK_FILE);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| ServerError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|source| ServerError::Io {
                path: path.clone(),
                source,
            })?;
        flock(&file, FlockOperation::LockExclusive).map_err(|errno| ServerError::Io {
            path,
            source: errno.into(),
        })?;
        Ok(Self { _file: file })
    }
}

#[must_use]
pub fn pending_properties_path(server_root: &Path) -> PathBuf {
    server_root.join(".launcher").join(PENDING_PROPERTIES_FILE)
}

/// Reads `server.properties`; a missing file is an empty document.
pub fn read_properties_file(path: &Path) -> ServerResult<PropertiesDocument> {
    match std::fs::read_to_string(path) {
        Ok(text) => PropertiesDocument::parse(&text).map_err(|malformed| malformed.in_file(path)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(PropertiesDocument::default())
        }
        Err(source) => Err(ServerError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Saves `values` for a server whose process is running: each one that differs
/// from what its next start would read is written to the file and kept until
/// then. Returns the keys that changed; nothing is written when none did.
pub fn save_while_running(
    server_root: &Path,
    properties_path: &Path,
    values: &[(String, String)],
) -> ServerResult<Vec<String>> {
    let _lock = PropertiesLock::acquire(server_root)?;
    let previous = PendingProperties::read(server_root)?;
    let mut pending = previous.clone();
    let mut document = read_properties_file(properties_path)?;
    // Compared with what the next start reads, which the running server may have overwritten.
    pending.apply_to(&mut document)?;
    let mut changed = Vec::new();
    for (key, value) in values {
        if document.get(key).as_deref() == Some(value.as_str()) {
            continue;
        }
        document.set(key, value)?;
        pending.values.insert(key.clone(), value.clone());
        changed.push(key.clone());
    }
    if changed.is_empty() {
        return Ok(changed);
    }
    // Kept first, so a failed file write still reaches the next start.
    pending.store(server_root)?;
    if let Err(error) = atomic_write(properties_path, document.render().as_bytes()) {
        if let Err(restore) = previous.store(server_root) {
            tracing::warn!(%restore, "kept server properties were not restored");
        }
        return Err(error);
    }
    Ok(changed)
}

/// Writes the kept values into `server.properties` and forgets them, just
/// before a new process reads the file.
pub fn apply_pending_properties(server_root: &Path, properties_path: &Path) -> ServerResult<()> {
    let _lock = PropertiesLock::acquire(server_root)?;
    let pending = PendingProperties::read(server_root)?;
    if pending.is_empty() {
        return Ok(());
    }
    let mut document = read_properties_file(properties_path)?;
    pending.apply_to(&mut document)?;
    atomic_write(properties_path, document.render().as_bytes())?;
    PendingProperties::default().store(server_root)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::SERVER_PORT_KEY;

    fn values(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    struct Server {
        root: tempfile::TempDir,
        properties: PathBuf,
    }

    fn server(properties: &str) -> Server {
        let root = tempfile::tempdir().expect("root");
        let game = root.path().join("server");
        std::fs::create_dir_all(&game).expect("game directory");
        let path = game.join("server.properties");
        std::fs::write(&path, properties).expect("properties");
        Server {
            root,
            properties: path,
        }
    }

    impl Server {
        fn file(&self) -> String {
            std::fs::read_to_string(&self.properties).expect("properties")
        }

        fn kept(&self) -> Vec<String> {
            PendingProperties::read(self.root.path())
                .expect("kept")
                .keys()
                .map(str::to_owned)
                .collect()
        }
    }

    #[test]
    fn a_value_saved_while_running_is_written_and_kept_for_the_next_start() {
        let server = server("# heading\nmotd=old\nunknown=kept\n");

        let changed = save_while_running(
            server.root.path(),
            &server.properties,
            &values(&[("motd", "new"), ("unknown", "kept")]),
        )
        .expect("save");

        assert_eq!(changed, ["motd"]);
        assert_eq!(server.file(), "# heading\nmotd=new\nunknown=kept\n");
        assert_eq!(server.kept(), ["motd"]);
    }

    #[test]
    fn a_save_that_changes_nothing_writes_nothing() {
        let server = server("motd=same\n");

        let changed = save_while_running(
            server.root.path(),
            &server.properties,
            &values(&[("motd", "same")]),
        )
        .expect("save");

        assert!(changed.is_empty());
        assert!(server.kept().is_empty());
        assert!(!pending_properties_path(server.root.path()).exists());
    }

    #[test]
    fn a_value_the_running_server_wrote_over_is_restored_before_the_next_start() {
        let server = server("motd=old\nwhite-list=false\n");
        save_while_running(
            server.root.path(),
            &server.properties,
            &values(&[("motd", "new")]),
        )
        .expect("save");
        // What `/whitelist on` makes the running server write: its own copy.
        std::fs::write(&server.properties, "motd=old\nwhite-list=true\n").expect("overwrite");

        apply_pending_properties(server.root.path(), &server.properties).expect("apply");

        assert_eq!(server.file(), "motd=new\nwhite-list=true\n");
        assert!(server.kept().is_empty());
        assert!(!pending_properties_path(server.root.path()).exists());
    }

    #[test]
    fn a_later_save_compares_with_what_the_next_start_reads() {
        let server = server("motd=old\n");
        save_while_running(
            server.root.path(),
            &server.properties,
            &values(&[("motd", "new")]),
        )
        .expect("first save");
        std::fs::write(&server.properties, "motd=old\n").expect("overwrite");

        let changed = save_while_running(
            server.root.path(),
            &server.properties,
            &values(&[("motd", "new"), ("pvp", "false")]),
        )
        .expect("second save");

        assert_eq!(changed, ["pvp"]);
        assert_eq!(server.file(), "motd=new\npvp=false\n");
        assert_eq!(server.kept(), ["motd", "pvp"]);
    }

    #[test]
    fn a_typed_network_key_is_refused_and_nothing_is_written_or_kept() {
        let server = server("motd=old\n");

        let error = save_while_running(
            server.root.path(),
            &server.properties,
            &values(&[("motd", "new"), (SERVER_PORT_KEY, "25570")]),
        )
        .expect_err("protected");

        assert!(matches!(error, ServerError::ProtectedProperty { .. }));
        assert_eq!(server.file(), "motd=old\n");
        assert!(server.kept().is_empty());
    }

    #[test]
    fn applying_with_nothing_kept_leaves_a_missing_file_missing() {
        let root = tempfile::tempdir().expect("root");
        let properties = root.path().join("server").join("server.properties");

        apply_pending_properties(root.path(), &properties).expect("apply");

        assert!(!properties.exists());
    }

    #[test]
    fn kept_values_from_a_newer_format_are_refused() {
        let server = server("motd=old\n");
        let path = pending_properties_path(server.root.path());
        std::fs::create_dir_all(path.parent().expect("parent")).expect("launcher");
        std::fs::write(&path, br#"{"format":2,"values":{"motd":"new"}}"#).expect("pending");

        let error = apply_pending_properties(server.root.path(), &server.properties)
            .expect_err("newer format");

        assert!(matches!(
            error,
            ServerError::PendingPropertiesUnreadable { .. }
        ));
        assert_eq!(server.file(), "motd=old\n");
    }

    #[test]
    fn kept_values_are_stored_as_compact_json() {
        let server = server("motd=old\n");
        save_while_running(
            server.root.path(),
            &server.properties,
            &values(&[("motd", "new")]),
        )
        .expect("save");

        let stored =
            std::fs::read_to_string(pending_properties_path(server.root.path())).expect("stored");

        assert_eq!(stored, r#"{"format":1,"values":{"motd":"new"}}"#);
    }
}
