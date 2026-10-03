//! Making a temporary instance persistent, without making it a different one.
//!
//! # Why this is not duplication with a storage flag
//!
//! `Registry::duplicate_client` can already put a copy under the persistent
//! root, and that answers a different question. A copy is a new instance: new
//! id, new name, no launch history, and none of the linked servers. Everything
//! that made the original worth keeping -- the crash report, the tuned
//! `options.txt`, the half-bisected `mods/` directory, the server with a world
//! in it -- stays behind in a directory the operating system is about to clear.
//!
//! Promotion moves the instance instead. Afterwards it is the same instance at
//! a different address (ADR 0025).
//!
//! # Why the linked servers are asked about rather than assumed
//!
//! A server has its own storage class and its own world. A persistent client
//! whose server disappears on the next reboot is a surprise worth preventing,
//! so every temporary linked server is offered and selected by default; but
//! leaving one temporary is a legitimate answer, and this does not decide it.
//! A server the caller did not select is not touched and not even checked for
//! being busy, because nothing is going to happen to it.

use std::path::PathBuf;

use ephervex_download::OperationReporter;
use ephervex_model::{InstanceId, Progress, ServerId, StorageClass};
use ephervex_storage::{ClientRecord, ServerRecord};
use tracing::{info, instrument, warn};

use crate::app::App;
use crate::error::{CoreError, CoreResult};

/// A temporary linked server that can move with its client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromotableServer {
    pub id: ServerId,
    pub name: String,
    /// Where it would end up. Shown rather than described, because a user who
    /// configured a custom persistent root wants to see that root.
    pub destination: PathBuf,
}

/// What promoting one instance would involve.
///
/// Read before the operation so a frontend can show the destination and the
/// servers, and so the decision about each server is made with the list in
/// front of the user rather than inferred afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromotionPlan {
    pub instance: InstanceId,
    pub name: String,
    pub destination: PathBuf,
    /// Every temporary linked server, in the order the registry lists them.
    /// A persistent one is already where it needs to be and is not offered.
    pub servers: Vec<PromotableServer>,
}

/// Which of the offered servers should move too.
///
/// An explicit list rather than a boolean, because the answer is per server.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PromotionRequest {
    pub servers: Vec<ServerId>,
}

impl PromotionRequest {
    /// Every server the plan offered, which is the default a frontend presents.
    #[must_use]
    pub fn all_of(plan: &PromotionPlan) -> Self {
        Self {
            servers: plan.servers.iter().map(|server| server.id).collect(),
        }
    }

    /// No servers: the client moves alone.
    #[must_use]
    pub const fn client_only() -> Self {
        Self {
            servers: Vec::new(),
        }
    }
}

/// A server that was asked to move and did not.
///
/// Reported rather than raised, because the client has already moved by the
/// time a server is tried and undoing a finished multi-gigabyte move to punish
/// a later failure costs more than saying which one failed.
#[derive(Debug)]
pub struct PromotionWarning {
    pub server: ServerId,
    pub name: String,
    pub error: CoreError,
}

/// What a promotion did.
#[derive(Debug)]
pub struct PromotionOutcome {
    pub instance: ClientRecord,
    pub servers: Vec<ServerRecord>,
    pub warnings: Vec<PromotionWarning>,
}

impl App {
    /// What promoting this instance would move, and where to.
    ///
    /// Refuses the same things the promotion itself refuses, so a frontend that
    /// asks first never offers an operation that would then fail. The one
    /// exception is a busy server, which is checked when it is about to move:
    /// a plan is read while a user is deciding, and a server that was running
    /// then may not be running when they answer.
    #[instrument(skip(self), fields(instance = %instance))]
    pub fn plan_instance_promotion(&self, instance: InstanceId) -> CoreResult<PromotionPlan> {
        let record = self.client(instance)?;
        self.refuse_promoting(&record)?;

        let mut servers = Vec::new();
        for id in self.linked_server_ids(instance)? {
            let server = self.server(id)?;
            if server.storage == StorageClass::Persistent {
                continue;
            }
            servers.push(PromotableServer {
                id,
                name: server.name,
                destination: self.paths().server_root(StorageClass::Persistent, id),
            });
        }

        Ok(PromotionPlan {
            instance,
            name: record.name,
            destination: self
                .paths()
                .instance_root(StorageClass::Persistent, instance),
            servers,
        })
    }

    /// Moves a temporary instance, and the servers named in `request`, into the
    /// persistent root.
    ///
    /// One way only. Demotion would hand a user's instance back to a cleanup
    /// process they do not control, and deleting a persistent instance is
    /// already the explicit way to stop keeping one (ADR 0025).
    #[instrument(skip(self, request, reporter), fields(instance = %instance))]
    pub fn promote_instance(
        &self,
        instance: InstanceId,
        request: &PromotionRequest,
        reporter: &OperationReporter,
    ) -> CoreResult<PromotionOutcome> {
        let record = self.client(instance)?;
        self.refuse_promoting(&record)?;

        // Every selected server is checked before anything moves, so a busy one
        // refuses the whole operation rather than being discovered halfway
        // through and reported as a partial success.
        let selected = self.selected_servers(instance, request)?;
        for server in &selected {
            crate::server::refuse_if_running(server)?;
        }

        reporter.set_stage(Some("operation-stage-relocating-files".to_owned()));
        let moved = self.registry_for_services().relocate_client(
            instance,
            StorageClass::Persistent,
            &mut file_progress(reporter),
        )?;

        let mut servers = Vec::new();
        let mut warnings = Vec::new();
        for server in selected {
            match self.registry_for_services().relocate_server(
                server.id,
                StorageClass::Persistent,
                &mut file_progress(reporter),
            ) {
                Ok(commit) => servers.push(commit.record),
                Err(error) => {
                    warn!(
                        server_id = %server.id,
                        %error,
                        "a linked server could not be moved with its instance"
                    );
                    warnings.push(PromotionWarning {
                        server: server.id,
                        name: server.name,
                        error: error.into(),
                    });
                }
            }
        }

        info!(
            instance_id = %instance,
            root = %moved.root.display(),
            servers = servers.len(),
            warnings = warnings.len(),
            "made an instance persistent"
        );
        Ok(PromotionOutcome {
            instance: moved,
            servers,
            warnings,
        })
    }

    /// The servers this request names, refusing one the plan never offered.
    fn selected_servers(
        &self,
        instance: InstanceId,
        request: &PromotionRequest,
    ) -> CoreResult<Vec<ServerRecord>> {
        let linked = self.linked_server_ids(instance)?;
        let mut selected = Vec::new();
        for id in &request.servers {
            if !linked.contains(id) {
                return Err(CoreError::ServerNotLinkedToInstance {
                    server: *id,
                    instance,
                });
            }
            let server = self.server(*id)?;
            // A persistent server is already where it would be moved to.
            if server.storage != StorageClass::Persistent {
                selected.push(server);
            }
        }
        Ok(selected)
    }

    /// The refusals that apply to the instance itself.
    fn refuse_promoting(&self, record: &ClientRecord) -> CoreResult<()> {
        if record.storage == StorageClass::Persistent {
            return Err(CoreError::InstanceAlreadyPersistent { id: record.id });
        }
        self.refuse_while_running(record.id)
    }
}

/// Publishes a file count as the move copies, so a multi-gigabyte instance is
/// not a dialog that appears to have stopped.
///
/// The total is deliberately absent: counting the tree first means walking it
/// twice, and what a waiting user needs is evidence of movement rather than a
/// percentage.
fn file_progress(reporter: &OperationReporter) -> impl FnMut(usize) + '_ {
    move |files| {
        reporter.set_items(Progress {
            current: files as u64,
            total: None,
        });
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use ephervex_model::LoaderSelection;
    use ephervex_storage::{ClientRecord, NewClient, NewServer, Paths};

    fn app_rooted_at(base: &std::path::Path) -> App {
        App::open_at(Paths::rooted_at(base)).expect("app opens against a temporary root")
    }

    fn temporary_client(app: &App, name: &str) -> ClientRecord {
        app.create_client_for_test(NewClient::new(name, "1.20.1", LoaderSelection::vanilla()))
            .expect("create")
    }

    /// A linked server with a real root on disk, without the network an
    /// installation would need.
    fn linked_server(
        app: &App,
        base: &std::path::Path,
        client: InstanceId,
        name: &str,
    ) -> ServerId {
        let staging = base.join(format!("staged-{name}"));
        std::fs::create_dir_all(staging.join("server")).expect("staging");
        let record = app
            .registry_for_services()
            .prepare_server(NewServer::linked(name, client))
            .expect("prepare");
        app.registry_for_services()
            .publish_staged_server(&record, &staging)
            .expect("publish");
        record.id
    }

    fn silent() -> OperationReporter {
        OperationReporter::silent("operation-promoting-instance")
    }

    #[test]
    fn promoting_an_already_persistent_instance_is_refused() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let client = app
            .create_client_for_test(
                NewClient::new("Kept", "1.20.1", LoaderSelection::vanilla())
                    .with_storage(StorageClass::Persistent),
            )
            .expect("create");

        let error = app
            .plan_instance_promotion(client.id)
            .expect_err("nothing to promote");

        assert!(
            matches!(error, CoreError::InstanceAlreadyPersistent { .. }),
            "expected a typed refusal, got {error}"
        );
    }

    #[test]
    fn a_promotion_offers_every_temporary_linked_server_and_no_persistent_one() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let client = temporary_client(&app, "Repro");
        let temporary = linked_server(&app, base.path(), client.id, "temporary");

        // A persistent linked server is already where a promotion would put it.
        let mut persistent = app
            .registry_for_services()
            .prepare_server(NewServer::linked("persistent", client.id))
            .expect("prepare");
        persistent.storage = StorageClass::Persistent;
        persistent.root = app
            .paths()
            .server_root(StorageClass::Persistent, persistent.id);
        let staging = base.path().join("staged-persistent");
        std::fs::create_dir_all(staging.join("server")).expect("staging");
        app.registry_for_services()
            .publish_staged_server(&persistent, &staging)
            .expect("publish");

        let plan = app.plan_instance_promotion(client.id).expect("plan");

        assert_eq!(
            plan.servers
                .iter()
                .map(|server| server.id)
                .collect::<Vec<_>>(),
            vec![temporary],
            "only a server that would actually move is offered"
        );
        assert_eq!(
            plan.destination,
            app.paths()
                .instance_root(StorageClass::Persistent, client.id)
        );
    }

    #[test]
    fn a_promotion_moves_the_instance_and_the_servers_it_was_given() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let client = temporary_client(&app, "Repro");
        let server = linked_server(&app, base.path(), client.id, "server");
        let plan = app.plan_instance_promotion(client.id).expect("plan");

        let outcome = app
            .promote_instance(client.id, &PromotionRequest::all_of(&plan), &silent())
            .expect("promote");

        assert!(outcome.warnings.is_empty());
        assert_eq!(outcome.instance.id, client.id);
        assert_eq!(outcome.instance.storage, StorageClass::Persistent);
        assert_eq!(outcome.servers.len(), 1);
        assert_eq!(outcome.servers[0].id, server);
        assert_eq!(outcome.servers[0].storage, StorageClass::Persistent);
        assert_eq!(
            app.linked_server_ids(client.id).expect("links"),
            vec![server],
            "moving does not detach a server from its client"
        );
    }

    #[test]
    fn a_linked_server_left_out_of_a_promotion_stays_temporary() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let client = temporary_client(&app, "Repro");
        let server = linked_server(&app, base.path(), client.id, "server");
        let before = app.server(server).expect("server").root;

        let outcome = app
            .promote_instance(client.id, &PromotionRequest::client_only(), &silent())
            .expect("promote");

        assert_eq!(outcome.instance.storage, StorageClass::Persistent);
        assert!(outcome.servers.is_empty());
        let untouched = app.server(server).expect("server");
        assert_eq!(untouched.storage, StorageClass::Temporary);
        assert_eq!(untouched.root, before, "an unselected server is not moved");
    }

    #[test]
    fn a_promotion_naming_somebody_elses_server_is_refused_before_anything_moves() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let client = temporary_client(&app, "Repro");
        let other = temporary_client(&app, "Other");
        let stranger = linked_server(&app, base.path(), other.id, "stranger");

        let error = app
            .promote_instance(
                client.id,
                &PromotionRequest {
                    servers: vec![stranger],
                },
                &silent(),
            )
            .expect_err("a server of another instance is not this promotion's to move");

        assert!(
            matches!(error, CoreError::ServerNotLinkedToInstance { .. }),
            "expected a typed refusal, got {error}"
        );
        assert_eq!(
            app.client(client.id).expect("record").storage,
            StorageClass::Temporary,
            "a refused promotion moves nothing"
        );
    }

    #[test]
    fn a_running_instance_refuses_to_be_promoted() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let client = temporary_client(&app, "Repro");
        let _held = ephervex_storage::LaunchLock::acquire(&client.root).expect("hold the lock");

        let error = app
            .plan_instance_promotion(client.id)
            .expect_err("a running instance cannot be moved underneath itself");

        assert!(
            matches!(error, CoreError::InstanceRunning { .. }),
            "expected an already-running refusal, got {error}"
        );
    }

    /// The record has to be what the next process reads, not just what this one
    /// returned.
    #[test]
    fn a_promoted_instance_is_still_persistent_after_a_restart() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let client = temporary_client(&app, "Repro");
        std::fs::write(client.root.join("minecraft/options.txt"), "guiScale:2\n")
            .expect("a setting worth keeping");

        let outcome = app
            .promote_instance(client.id, &PromotionRequest::client_only(), &silent())
            .expect("promote");

        drop(app);
        let reopened = app_rooted_at(base.path());
        let record = reopened.client(client.id).expect("read");
        assert_eq!(record.storage, StorageClass::Persistent);
        assert_eq!(record.root, outcome.instance.root);
        assert_eq!(
            std::fs::read_to_string(record.root.join("minecraft/options.txt")).expect("read"),
            "guiScale:2\n"
        );
        assert!(
            reopened.reconcile().expect("reconcile").is_empty(),
            "a finished promotion leaves nothing for reconciliation to fix"
        );
    }
}
