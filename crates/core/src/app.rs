//! The application services both frontends call.
//!
//! This is the application-service seam: the GUI and CLI depend on
//! `ephervex-core` and nothing below it, so provider crates can be reshaped
//! without either frontend noticing. Anything a frontend needs to *do* — as
//! opposed to merely display — belongs here.
//!
//! Keeping it honest matters more than it looks. A frontend that reaches past
//! this layer into a provider crate compiles perfectly well; the boundary is
//! only real for as long as nothing crosses it. The rule is therefore
//! structural: `ephervex-cli` and `ephervex-gui` do not list any provider crate
//! in their manifests, so a shortcut is a manifest change someone has to
//! justify rather than an import someone can add absent-mindedly.

use ephervex_model::{InstanceId, ServerId};
use ephervex_storage::{
    ClientRecord, Config, FileDisposition, NewClient, NewServer, Paths, ReconciliationReport,
    Registry, ServerRecord, Storage,
};
use tracing::instrument;

use crate::error::CoreResult;

/// The application, opened and ready to serve a frontend.
///
/// Construction resolves paths, loads configuration, applies the configured
/// persistent-instance-root override, creates the base directories, and opens
/// the index. It deliberately does **not** reconcile: reconciliation mutates
/// the index, and a caller that only wants to read should not have to know
/// that. Call [`App::reconcile`] explicitly at startup.
#[derive(Debug)]
pub struct App {
    storage: Storage,
}

impl App {
    /// Opens the application using the XDG roots resolved from the environment.
    ///
    /// This is the normal startup path for both frontends.
    pub fn open() -> CoreResult<Self> {
        Ok(Self {
            storage: ephervex_storage::open_from_environment()?,
        })
    }

    /// Opens the application against explicit roots.
    ///
    /// Exists so tests — and, later, a `--data-dir` style override — can point
    /// the whole application at a temporary directory instead of the
    /// developer's real home.
    pub fn open_at(paths: Paths) -> CoreResult<Self> {
        Ok(Self {
            storage: ephervex_storage::open(paths)?,
        })
    }

    /// The effective configuration.
    #[must_use]
    pub fn config(&self) -> &Config {
        &self.storage.config
    }

    /// The resolved application directories.
    #[must_use]
    pub fn paths(&self) -> &Paths {
        self.storage.registry.paths()
    }

    /// Brings the index back in line with what is actually on disk.
    ///
    /// A vanished temporary root is removed silently — that is what an OS temp
    /// cleanup looks like, not an error. A vanished persistent root keeps its
    /// record and raises a warning instead (ADR 0006).
    #[instrument(skip(self))]
    pub fn reconcile(&self) -> CoreResult<ReconciliationReport> {
        Ok(ephervex_storage::reconcile(&self.storage.registry)?)
    }

    // -- client instances --------------------------------------------------

    /// Creates a client instance and materializes its directory.
    #[instrument(skip(self, request))]
    pub fn create_client(&self, request: NewClient) -> CoreResult<ClientRecord> {
        Ok(self.registry().create_client(request)?)
    }

    /// Fetches one client instance.
    pub fn client(&self, id: InstanceId) -> CoreResult<ClientRecord> {
        Ok(self.registry().client(id)?)
    }

    /// Lists every indexed client instance.
    pub fn list_clients(&self) -> CoreResult<Vec<ClientRecord>> {
        Ok(self.registry().list_clients()?)
    }

    /// Deletes a client instance.
    ///
    /// `files` is explicit rather than inferred: removing a user's game
    /// directory is not something to decide on their behalf from context.
    #[instrument(skip(self))]
    pub fn delete_client(&self, id: InstanceId, files: FileDisposition) -> CoreResult<()> {
        Ok(self.registry().delete_client(id, files)?)
    }

    /// The servers linked to a client.
    ///
    /// Derived from the server rows rather than stored on the client, so it
    /// cannot go stale when a server is deleted.
    pub fn linked_server_ids(&self, client: InstanceId) -> CoreResult<Vec<ServerId>> {
        Ok(self.registry().linked_server_ids(client)?)
    }

    // -- servers -----------------------------------------------------------

    /// Creates a server, optionally linked to a client.
    #[instrument(skip(self, request))]
    pub fn create_server(&self, request: NewServer) -> CoreResult<ServerRecord> {
        Ok(self.registry().create_server(request)?)
    }

    /// Lists every indexed server.
    pub fn list_servers(&self) -> CoreResult<Vec<ServerRecord>> {
        Ok(self.registry().list_servers()?)
    }

    /// Deletes a server.
    #[instrument(skip(self))]
    pub fn delete_server(&self, id: ServerId, files: FileDisposition) -> CoreResult<()> {
        Ok(self.registry().delete_server(id, files)?)
    }

    /// The registry, for services inside this crate.
    ///
    /// Private on purpose: exposing it would let a frontend reach the whole
    /// storage API through `App` and re-open the boundary this layer closes.
    fn registry(&self) -> &Registry {
        &self.storage.registry
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use ephervex_model::{LoaderSelection, StorageClass};

    use super::*;

    fn app_rooted_at(base: &std::path::Path) -> App {
        App::open_at(Paths::rooted_at(base)).expect("app should open against a temporary root")
    }

    #[test]
    fn the_services_layer_walks_the_instance_lifecycle() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());

        assert!(app.list_clients().expect("list").is_empty());

        let created = app
            .create_client(NewClient::new(
                "Bug Repro",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create");
        assert_eq!(created.storage, StorageClass::Temporary);

        let fetched = app.client(created.id).expect("fetch");
        assert_eq!(fetched.name, "Bug Repro");
        assert_eq!(app.list_clients().expect("list").len(), 1);

        app.delete_client(created.id, FileDisposition::Remove)
            .expect("delete");
        assert!(app.list_clients().expect("list").is_empty());
    }

    #[test]
    fn reconciliation_is_reachable_without_touching_the_registry_directly() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());

        let created = app
            .create_client(NewClient::new(
                "Doomed",
                "1.21.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create");

        // Stand in for an OS temp cleanup.
        std::fs::remove_dir_all(&created.root).expect("remove the instance root");

        let report = app.reconcile().expect("reconcile");
        assert_eq!(report.removed_stale_temporary.len(), 1);
        assert!(app.list_clients().expect("list").is_empty());
    }

    #[test]
    fn configuration_and_paths_are_readable_through_the_services_layer() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());

        assert_eq!(
            app.config().schema_version,
            ephervex_storage::CONFIG_SCHEMA_VERSION
        );
        assert!(app.paths().config_file().starts_with(base.path()));
    }
}
