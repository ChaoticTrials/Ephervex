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
use ephervex_storage::{
    ClientCommit, ClientCommitWarning, ClientRecord, ServerCommitWarning, ServerRecord,
};
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

/// A condition on a moved server's saved metadata the user should hear about.
#[derive(Debug)]
pub struct PromotedServerWarning {
    pub server: ServerId,
    pub name: String,
    pub warning: ServerCommitWarning,
}

/// The localized sentence telling a user what a moved server's warning means,
/// led by the server it is about.
#[must_use]
pub fn promoted_server_warning_message(warning: &PromotedServerWarning) -> String {
    ephervex_i18n::lookup_args(
        "server-made-persistent-warning",
        [
            ("name", ephervex_i18n::Arg::from(warning.name.as_str())),
            (
                "warning",
                ephervex_i18n::Arg::from(crate::server::server_commit_warning_message(
                    &warning.warning,
                )),
            ),
        ],
    )
}

/// What a promotion did.
#[derive(Debug)]
pub struct PromotionOutcome {
    pub instance: ClientRecord,
    /// Conditions on the saved instance metadata the user should hear about.
    pub instance_warnings: Vec<ClientCommitWarning>,
    pub servers: Vec<ServerRecord>,
    /// Conditions on the saved metadata of the servers that moved.
    pub server_commit_warnings: Vec<PromotedServerWarning>,
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
        // The row as indexed: a promotion whose index write failed is retried
        // against it, which repairs the row instead of refusing a move that
        // already happened.
        let record = self.registry().indexed_client(instance)?;
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
        // Claimed before anything is read or checked, and kept until the last
        // move ends: a launch records itself under the same claim, so none can
        // begin on a tree that is being copied and then removed.
        let claim = self
            .registry()
            .claim_host(ephervex_storage::RecordRef::Client(instance))?;
        let record = self.registry().indexed_client(instance)?;
        self.refuse_promoting(&record)?;

        // Every selected server is checked before anything moves, so a busy one
        // refuses the whole operation rather than being discovered halfway
        // through and reported as a partial success.
        let selected = self.selected_servers(instance, request)?;
        let mut server_claims = Vec::with_capacity(selected.len());
        for server in &selected {
            server_claims.push(
                self.registry()
                    .claim_host(ephervex_storage::RecordRef::Server(server.id))?,
            );
            crate::server::refuse_if_running(server)?;
        }

        reporter.set_stage(Some("operation-stage-relocating-files".to_owned()));
        let ClientCommit {
            record: moved,
            warnings: instance_warnings,
        } = self.registry_for_services().relocate_client(
            instance,
            &claim,
            StorageClass::Persistent,
            &mut file_progress(reporter),
        )?;

        let mut servers = Vec::new();
        let mut server_commit_warnings = Vec::new();
        let mut warnings = Vec::new();
        for (server, server_claim) in selected.into_iter().zip(&server_claims) {
            match self.registry_for_services().relocate_server(
                server.id,
                server_claim,
                StorageClass::Persistent,
                &mut file_progress(reporter),
            ) {
                Ok(commit) => {
                    server_commit_warnings.extend(commit.warnings.into_iter().map(|warning| {
                        PromotedServerWarning {
                            server: server.id,
                            name: commit.record.name.clone(),
                            warning,
                        }
                    }));
                    servers.push(commit.record);
                }
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
            instance_warnings,
            servers,
            server_commit_warnings,
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
            // Named twice, it still moves once: a second claim would refuse itself.
            if selected
                .iter()
                .any(|server: &ServerRecord| server.id == *id)
            {
                continue;
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

    /// Promotes an instance whose index writes fail, so the row keeps naming
    /// the vacated temporary root. Writes are accepted again on return.
    fn promote_with_failing_index(app: &App, client: &ClientRecord) -> PromotionOutcome {
        let index = app.registry().index().connection();
        index
            .execute_batch(
                "CREATE TRIGGER refuse_index_writes BEFORE UPDATE ON instances
                 BEGIN SELECT RAISE(ABORT, 'index is read-only'); END;",
            )
            .expect("install failing trigger");
        let outcome = app
            .promote_instance(client.id, &PromotionRequest::client_only(), &silent())
            .expect("the published metadata is a committed promotion");
        index
            .execute_batch("DROP TRIGGER refuse_index_writes;")
            .expect("accept index writes again");
        outcome
    }

    #[test]
    fn a_promoted_instance_whose_index_write_failed_is_duplicated_and_deleted_at_its_persistent_root()
     {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let client = temporary_client(&app, "Repro");
        let persistent = app
            .paths()
            .instance_root(StorageClass::Persistent, client.id);
        let outcome = promote_with_failing_index(&app, &client);
        assert_eq!(outcome.instance.root, persistent);
        assert_eq!(
            app.registry().indexed_client(client.id).expect("row").root,
            client.root,
            "the row still names the vacated root"
        );

        let read = app.client(client.id).expect("read");
        assert_eq!(read.root, persistent);
        assert_eq!(read.storage, StorageClass::Persistent);
        let copy = app
            .duplicate_client(
                client.id,
                &ephervex_storage::DuplicateClient::default(),
                ephervex_model::AbsentModPolicy::default(),
            )
            .expect("the files are copied from where they are");
        assert!(copy.record.root.is_dir());
        assert_eq!(copy.record.storage, StorageClass::Persistent);

        app.delete_client(client.id, ephervex_storage::FileDisposition::Remove)
            .expect("delete");

        assert!(
            !persistent.exists(),
            "the deleted instance's files are gone"
        );
        assert!(app.client(client.id).is_err());
    }

    #[tokio::test]
    async fn a_launch_of_a_promoted_instance_whose_index_write_failed_is_seen_at_its_persistent_root()
     {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        app.add_offline_account("Notch").expect("account");
        let client = temporary_client(&app, "Repro");
        let persistent = app
            .paths()
            .instance_root(StorageClass::Persistent, client.id);
        promote_with_failing_index(&app, &client);
        let _held = ephervex_storage::LaunchLock::acquire(&persistent).expect("a launch is held");

        let running = app.running_processes().expect("query");
        assert_eq!(running.len(), 1, "the running launch is listed");
        let error = app
            .launch(
                client.id,
                &crate::LaunchOptions::default(),
                &ephervex_minecraft::LaunchObserver::silent(),
                &tokio_util::sync::CancellationToken::new(),
            )
            .await
            .expect_err("the instance is already launched");
        assert_eq!(error.message_id(), "error-launch-already-running");
        app.delete_client(client.id, ephervex_storage::FileDisposition::Remove)
            .expect_err("a running instance is not deleted");
        assert!(persistent.is_dir());
    }

    #[test]
    fn a_promotion_whose_index_write_failed_can_be_repeated_to_repair_the_row() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let client = temporary_client(&app, "Repro");
        let persistent = app
            .paths()
            .instance_root(StorageClass::Persistent, client.id);
        promote_with_failing_index(&app, &client);

        let repeated = app
            .promote_instance(client.id, &PromotionRequest::client_only(), &silent())
            .expect("the move that already happened is not refused");

        assert!(repeated.instance_warnings.is_empty());
        let row = app.registry().indexed_client(client.id).expect("row");
        assert_eq!(row.root, persistent);
        assert_eq!(row.storage, StorageClass::Persistent);
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
    fn a_moved_server_whose_index_update_fails_is_reported_as_moved_with_a_warning() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let client = temporary_client(&app, "Repro");
        let server = linked_server(&app, base.path(), client.id, "server");
        let plan = app.plan_instance_promotion(client.id).expect("plan");
        app.registry_for_services()
            .index()
            .connection()
            .execute_batch(
                "CREATE TRIGGER refuse_server_updates BEFORE UPDATE ON servers
                 BEGIN SELECT RAISE(ABORT, 'index unavailable'); END;",
            )
            .expect("install failure");

        let outcome = app
            .promote_instance(client.id, &PromotionRequest::all_of(&plan), &silent())
            .expect("promote");

        assert!(outcome.warnings.is_empty(), "the server did move");
        assert_eq!(outcome.servers.len(), 1);
        assert_eq!(outcome.servers[0].id, server);
        let [warning] = outcome.server_commit_warnings.as_slice() else {
            panic!(
                "expected one warning, got {:?}",
                outcome.server_commit_warnings
            );
        };
        assert_eq!(warning.server, server);
        assert_eq!(warning.name, "server");
        assert!(matches!(
            warning.warning,
            ServerCommitWarning::IndexOutOfSync { .. }
        ));
        assert!(
            promoted_server_warning_message(warning).starts_with("server: The change was saved")
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

    #[test]
    fn a_promotion_never_removes_the_lock_of_a_launch_that_holds_the_instance() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let client = temporary_client(&app, "Repro");
        let first = ephervex_storage::LaunchLock::acquire(&client.root).expect("first launch");
        let promote = || {
            app.promote_instance(client.id, &PromotionRequest::client_only(), &silent())
                .expect_err("a held instance is not moved")
        };

        let error = promote();
        assert!(
            matches!(error, CoreError::InstanceRunning { .. }),
            "expected an already-running refusal, got {error}"
        );
        assert!(first.path().is_file(), "the live lock is still there");
        drop(first);

        let _alongside = ephervex_storage::LaunchLock::acquire_additional(&client.root, "bob")
            .expect("an additional launch");
        let error = promote();
        assert!(
            matches!(error, CoreError::InstanceRunning { .. }),
            "expected an already-running refusal, got {error}"
        );
        let record = app.client(client.id).expect("record");
        assert_eq!(record.storage, StorageClass::Temporary);
        assert_eq!(record.root, client.root);
    }

    #[test]
    fn a_promotion_is_refused_while_another_operation_holds_the_instance() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let client = temporary_client(&app, "Repro");
        let operation = app
            .registry()
            .claim_host(ephervex_storage::RecordRef::Client(client.id))
            .expect("claim");

        let error = app
            .promote_instance(client.id, &PromotionRequest::client_only(), &silent())
            .expect_err("a launch or restore holds the instance");

        assert_eq!(error.message_id(), "error-registry-host-busy", "{error}");
        drop(operation);
        assert_eq!(
            app.client(client.id).expect("record").root,
            client.root,
            "nothing moved"
        );
    }

    #[test]
    fn a_promotion_moves_nothing_when_a_selected_server_is_held_by_another_operation() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let client = temporary_client(&app, "Repro");
        let server = linked_server(&app, base.path(), client.id, "Repro Server");
        let _start = app
            .registry()
            .claim_host(ephervex_storage::RecordRef::Server(server))
            .expect("a start claims the server");

        let error = app
            .promote_instance(
                client.id,
                &PromotionRequest {
                    servers: vec![server],
                },
                &silent(),
            )
            .expect_err("the server is not stopped for this promotion");

        assert_eq!(error.message_id(), "error-registry-host-busy", "{error}");
        assert_eq!(
            app.client(client.id).expect("record").storage,
            StorageClass::Temporary,
            "the client stayed where it was"
        );
    }

    #[test]
    fn a_server_named_twice_in_a_promotion_request_moves_once() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let client = temporary_client(&app, "Repro");
        let server = linked_server(&app, base.path(), client.id, "Repro Server");

        let outcome = app
            .promote_instance(
                client.id,
                &PromotionRequest {
                    servers: vec![server, server],
                },
                &silent(),
            )
            .expect("a repeated id is not a conflict with itself");

        assert_eq!(outcome.servers.len(), 1);
        assert!(outcome.warnings.is_empty());
        assert_eq!(
            app.server(server).expect("record").storage,
            StorageClass::Persistent
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
