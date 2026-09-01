//! The launch service both frontends call.
//!
//! # What this layer adds over `ephervex-minecraft`
//!
//! The Minecraft crate knows how to install a version and build a command line.
//! It deliberately knows nothing about instances, accounts, Java runtimes, or
//! configuration files — those are application questions, and answering them
//! here is what keeps each provider replaceable (ADR 0005).
//!
//! This module sequences authentication, Java selection, installation, and process
//! launching as one
//! pipeline, and owns the three decisions none of them can make alone:
//!
//! * **Which account, and what it presents as.** Both kinds must launch, and
//!   they differ in more than one argument, so the conversion to a
//!   [`PlayerIdentity`] matches on `Credentials` rather than checking whether a
//!   token happens to be present. An expired Microsoft token is refreshed
//!   *before* the launch rather than failing part-way through it.
//! * **Which runtime.** `plan_java` already answers "which Java, and why";
//!   this passes its answer to the command builder and its memory settings to
//!   the configured `-Xms`/`-Xmx`.
//! * **That one instance launches once.** The lock is taken before anything is
//!   downloaded and held until the game exits, so a second `ephervex launch` of
//!   the same instance is refused rather than racing it into the same `saves/`.
//!
//! # The order is the guarantee
//!
//! ```text
//! lock -> record -> session -> metadata -> java -> install -> command -> spawn
//! ```
//!
//! The lock is first because everything after it writes somewhere. The session
//! is refreshed *before* the install rather than after, so a launch that is
//! going to fail on authentication fails in a second rather than after
//! downloading six hundred megabytes of assets.

use std::path::PathBuf;

use ephervex_auth::{AuthObserver, Credentials, Session};
use ephervex_download::{OperationReporter, ProgressSink};
use ephervex_loaders::{ClientInstallRequest, InstalledClient};
use ephervex_minecraft::{
    GameExit, GameProcess, Installation, JvmOptions, LaunchCommand, LaunchError, LaunchEvent,
    LaunchFeatures, LaunchObserver, LaunchSettings, Platform, PlayerIdentity, VersionMetadata,
};
use ephervex_model::{InstanceId, Operation, OperationId};
use ephervex_storage::{ClientRecord, LaunchLock, LaunchState};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, instrument};

use crate::app::{App, JavaPlan, JavaPlanRequest, JavaTarget};
use crate::error::CoreResult;

/// What a caller is asking for when it launches an instance.
#[derive(Debug, Clone, Default)]
pub struct LaunchOptions {
    /// Launcher features to enable: demo mode, a custom window size.
    pub features: LaunchFeatures,
    /// A Java executable for this launch only. Outranks every stored setting.
    pub java_path: Option<PathBuf>,
    /// Whether to download a managed runtime when nothing installed qualifies.
    pub install_java_if_missing: bool,
    /// Launch as this account rather than the default one.
    ///
    /// An account reference of the kind [`App::resolve_account`] accepts: an
    /// id, or a name. `None` uses the default, which is what every launch did
    /// before there was a way to say otherwise. Choosing one here does not
    /// change the default.
    pub account: Option<String>,
    /// Prepare everything and return the command without starting the game.
    ///
    /// Everything means everything: the version is installed, the runtime is
    /// selected, and the session is established, because the point of a dry run
    /// is to show the command that *would* run — and a command whose classpath
    /// names files that were never downloaded shows something else.
    pub dry_run: bool,
}

/// What a launch did.
#[derive(Debug)]
pub struct LaunchOutcome {
    /// The id every launcher log line for this launch carries.
    ///
    /// Shown to the user so a report of "it failed" can be turned into the
    /// events that describe why, without asking anybody to correlate
    /// timestamps by hand.
    pub operation: OperationId,
    pub instance: InstanceId,
    pub minecraft_version: String,
    /// Which runtime ran it, and why that one.
    pub java: Box<JavaPlan>,
    /// The command line. Its `Debug` and `Display` are redacted; only
    /// `redacted_arguments` may be printed.
    pub command: Box<LaunchCommand>,
    pub installation: Box<Installation>,
    /// The player name the game was told.
    pub account: String,
    /// Whether that identity was proved to Mojang. False for an offline
    /// account, which may not join an online-mode server.
    pub authenticated: bool,
    /// How the game finished, or `None` for a dry run.
    pub exit: Option<GameExit>,
}

impl LaunchOutcome {
    /// Whether the game exited cleanly. `false` for a dry run, which never ran.
    #[must_use]
    pub fn is_success(&self) -> bool {
        self.exit.is_some_and(|exit| exit.is_success())
    }
}

/// Forwards download progress into the launch event stream.
///
/// So a frontend subscribes to one thing. `Operation` is the workspace's single
/// progress abstraction, and this is the adapter that keeps the
/// launch from inventing a second one.
#[derive(Debug)]
struct DownloadProgress {
    observer: LaunchObserver,
}

impl ProgressSink for DownloadProgress {
    fn operation_changed(&self, operation: &Operation) {
        self.observer
            .emit(LaunchEvent::Downloading(Box::new(operation.clone())));
    }
}

impl App {
    /// Installs and launches a client instance, streaming what happens
    ///
    /// Emits [`LaunchEvent`]s through `observer` — the same events the GUI will
    /// consume in the GUI — and returns once the game has exited. Canceling
    /// stops the launch, and stops the game if it has already started.
    ///
    /// Failures are typed. `CoreError::requires_interactive_login()` is the
    /// `LaunchError::AuthenticationRequired`, and `required_java_major()` with
    /// `managed_java_would_help()` is its `MissingJava`; there is one definition
    /// of each rather than a second one that could drift.
    pub async fn launch(
        &mut self,
        instance: InstanceId,
        options: &LaunchOptions,
        observer: &LaunchObserver,
        cancel: &CancellationToken,
    ) -> CoreResult<LaunchOutcome> {
        // One operation for the whole launch, including the downloads it makes:
        // the reporter is the workspace's single progress abstraction, and its
        // id is what ties this span's log lines to what the user was shown.
        // Created out here so the span below can carry the id from the moment it
        // opens, which is what puts it on every line the launch produces.
        let reporter = OperationReporter::new(
            "operation-launching-minecraft",
            true,
            std::sync::Arc::new(DownloadProgress {
                observer: observer.clone(),
            }),
        );
        // A launch installs artifacts into the shared cache and then hands their
        // paths to a JVM, so the lease has to survive until the game has exited.
        // Spelled out rather than routed through `App::with_artifact_cache`
        // because the work borrows `self` mutably; the order is identical.
        let lease = self.artifact_cache_lease().await?;
        let outcome = self
            .launch_operation(instance, options, observer, cancel, &reporter)
            .await;
        drop(lease);
        self.maintain_artifact_cache().await;
        outcome
    }

    #[instrument(
        name = "launch",
        skip(self, options, observer, cancel, reporter),
        fields(instance = %instance, operation = %reporter.id())
    )]
    async fn launch_operation(
        &mut self,
        instance: InstanceId,
        options: &LaunchOptions,
        observer: &LaunchObserver,
        cancel: &CancellationToken,
        reporter: &OperationReporter,
    ) -> CoreResult<LaunchOutcome> {
        let outcome = self
            .launch_inner(instance, options, observer, cancel, reporter)
            .await;
        if let Err(error) = &outcome {
            // Logged inside the span, so the technical detail a user pastes
            // into a report is findable by the id they were shown.
            tracing::warn!(
                message_id = error.message_id(),
                detail = %error,
                "launch failed"
            );
            observer.emit(LaunchEvent::Failed {
                message_id: error.message_id(),
                detail: error.to_string(),
            });
        }
        outcome
    }

    async fn launch_inner(
        &mut self,
        instance: InstanceId,
        options: &LaunchOptions,
        observer: &LaunchObserver,
        cancel: &CancellationToken,
        reporter: &OperationReporter,
    ) -> CoreResult<LaunchOutcome> {
        observer.emit(LaunchEvent::Preparing {
            operation: reporter.id(),
        });

        let mut record = self.client(instance)?;

        // Taken before anything writes, and held until the game exits. A
        // second launch of the same instance is refused here rather than
        // discovered when two processes fight over `saves/`.
        let mut lock = LaunchLock::acquire(&record.root)?;

        // Establish identity before metadata or artifacts are fetched. A
        // missing account is locally knowable and should fail without network.
        let identity = self
            .launch_identity(options.account.as_deref(), cancel)
            .await?;

        let metadata = self.minecraft_version(&record.minecraft_version).await?;
        let java = self
            .plan_java(
                &record.minecraft_version,
                &JavaPlanRequest {
                    target: JavaTarget::Client(instance),
                    explicit_path: options.java_path.clone(),
                    install_if_missing: options.install_java_if_missing,
                },
            )
            .await?;

        let installed = self
            .install_version(
                &record,
                &metadata,
                &java.runtime.executable,
                options,
                reporter,
                cancel,
            )
            .await?;

        let command = ephervex_minecraft::build_command(
            &installed.metadata,
            &installed.installation.plan,
            &LaunchSettings {
                java: java.runtime.executable.clone(),
                game_directory: game_directory(&record),
                identity: identity.clone(),
                features: options.features.clone(),
                jvm: JvmOptions {
                    min_memory_mib: java.jvm.min_memory_mib,
                    max_memory_mib: java.jvm.max_memory_mib,
                    extra_arguments: java.jvm.jvm_args.clone(),
                },
            },
        )?;

        let mut outcome = LaunchOutcome {
            operation: reporter.id(),
            instance,
            minecraft_version: record.minecraft_version.clone(),
            java: Box::new(java),
            account: identity.name.clone(),
            authenticated: identity.is_authenticated(),
            command: Box::new(command),
            installation: Box::new(installed.installation),
            exit: None,
        };

        if options.dry_run {
            debug!(instance = %instance, "dry run; not starting the game");
            return Ok(outcome);
        }
        if cancel.is_cancelled() {
            return Err(LaunchError::Canceled.into());
        }

        let process = GameProcess::start(&outcome.command, observer)?;
        lock.record_game_pid(process.pid())?;

        // Recorded before the wait, so a launch that never returns — a game
        // that hangs, a machine that loses power — still shows as having been
        // launched.
        record.last_launched_at = Some(chrono::Utc::now());
        if let Err(error) = self.update_client_record(&mut record) {
            tracing::warn!(%error, "could not record the launch time");
        }

        info!(
            instance = %instance,
            version = %outcome.minecraft_version,
            pid = process.pid(),
            authenticated = outcome.authenticated,
            "launched minecraft"
        );
        outcome.exit = Some(process.wait(observer, cancel).await?);
        Ok(outcome)
    }

    /// Downloads and unpacks everything the version needs.
    ///
    /// Reports into the launch's own operation rather than starting a second
    /// one, so the progress a user watches and the id in the log are the same
    /// operation.
    async fn install_version(
        &self,
        record: &ClientRecord,
        metadata: &VersionMetadata,
        java: &std::path::Path,
        options: &LaunchOptions,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> CoreResult<InstalledClient> {
        // The installer sets the step as it works — the version and stage it is
        // fetching — and those labels are technical values rather than message
        // ids, so nothing is set here that would be shown untranslated.
        let context = options.features.rule_context(Platform::host());
        Ok(self
            .loaders()?
            .provider(record.loader.kind)
            .install_client(ClientInstallRequest {
                minecraft: &record.minecraft_version,
                exact_loader: record.loader.version.as_deref(),
                parent: metadata,
                java,
                context: &context,
                reporter,
                cancel,
            })
            .await?)
    }

    /// The identity the game is told it is running as.
    ///
    /// Matches on [`Credentials`] rather than asking whether a token is
    /// present, because "no token" would otherwise mean both *offline* and
    /// *expired* — and those want opposite things (`ephervex-auth`'s
    /// `Credentials` exists to force this distinction).
    async fn launch_identity(
        &mut self,
        account: Option<&str>,
        cancel: &CancellationToken,
    ) -> CoreResult<PlayerIdentity> {
        // refresh an expired Microsoft token before the launch rather than
        // failing on it. `authenticated_session` is that operation, and it is
        // the same one "app restart preserves login" uses. A chosen account
        // takes the same path with the account named instead of looked up.
        let session = match account {
            Some(reference) => {
                self.session_for_account(reference, &AuthObserver::silent(), cancel)
                    .await?
            }
            None => {
                self.authenticated_session(&AuthObserver::silent(), cancel)
                    .await?
            }
        };
        Ok(identity_of(session))
    }

    /// What is currently launching or running in an instance, if anything.
    ///
    /// Reads the instance's own lock file rather than an in-process set, so it
    /// is true across the GUI, the CLI, and a second terminal. A lock whose
    /// owner is gone reports `None` — a permanently "running" instance is worse
    /// than none at all.
    pub fn launch_state(&self, instance: InstanceId) -> CoreResult<Option<LaunchState>> {
        Ok(ephervex_storage::running_state(
            &self.client(instance)?.root,
        ))
    }

    /// Writes an updated instance record.
    fn update_client_record(&self, record: &mut ClientRecord) -> CoreResult<()> {
        Ok(self.registry_for_services().update_client(record)?)
    }
}

/// The player identity one session presents.
fn identity_of(session: &Session) -> PlayerIdentity {
    let name = session.account.name.clone();
    let uuid = session.account.id.to_string();
    match &session.credentials {
        Credentials::Microsoft { token, .. } => {
            PlayerIdentity::microsoft(name, uuid, token.expose())
        }
        // Not a Microsoft identity with the token missing: it proves nothing,
        // and the command line says so.
        _ => PlayerIdentity::offline(name, uuid),
    }
}

/// `<instance>/minecraft/`, where saves, options, logs, and crash reports live.
fn game_directory(record: &ClientRecord) -> PathBuf {
    ephervex_storage::metadata::client_game_dir(&record.root)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use ephervex_model::{LoaderKind, LoaderSelection, StorageClass};
    use ephervex_storage::{NewClient, Paths};

    use super::*;

    fn app_rooted_at(base: &std::path::Path) -> App {
        App::open_at(Paths::rooted_at(base)).expect("app opens")
    }

    fn vanilla(app: &App, version: &str) -> ClientRecord {
        app.create_client_for_test(
            NewClient::new("Repro", version, LoaderSelection::vanilla())
                .with_storage(StorageClass::Temporary),
        )
        .expect("create")
    }

    #[tokio::test]
    async fn launching_with_no_account_asks_for_one_rather_than_downloading_anything() {
        // The session is established before the install precisely so this
        // failure costs a second rather than six hundred megabytes.
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        let instance = vanilla(&app, "1.20.1").id;

        let error = app
            .launch(
                instance,
                &LaunchOptions {
                    dry_run: true,
                    ..LaunchOptions::default()
                },
                &LaunchObserver::silent(),
                &CancellationToken::new(),
            )
            .await
            .expect_err("nobody is signed in");

        assert!(
            error.requires_interactive_login(),
            "authentication failures retain the shared LaunchError::AuthenticationRequired type"
        );
        assert_eq!(error.message_id(), "error-auth-not-signed-in");
    }

    #[tokio::test]
    async fn a_loader_instance_dispatches_to_the_matching_provider() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());

        let forge = app
            .create_client_for_test(NewClient::new(
                "Forge repro",
                "1.20.1",
                LoaderSelection::exact(LoaderKind::Forge, "47.4.0"),
            ))
            .expect("create");
        let record = app.client(forge.id).expect("record");
        assert_eq!(
            app.loaders()
                .expect("registry")
                .provider(record.loader.kind)
                .kind(),
            LoaderKind::Forge
        );
    }

    #[tokio::test]
    async fn a_second_launch_of_one_instance_is_refused_while_the_first_holds_it() {
        // Two Minecraft processes in one game directory corrupt each other's
        // saves silently, which is the worst way for this to be discovered.
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        app.add_offline_account("Notch").expect("account");
        let instance = vanilla(&app, "1.20.1").id;

        let held = LaunchLock::acquire(&app.client(instance).expect("record").root)
            .expect("the first launch takes the lock");

        let error = app
            .launch(
                instance,
                &LaunchOptions::default(),
                &LaunchObserver::silent(),
                &CancellationToken::new(),
            )
            .await
            .expect_err("one instance launches once");
        assert_eq!(error.message_id(), "error-launch-already-running");

        drop(held);
    }

    #[test]
    fn a_microsoft_session_becomes_an_authenticated_identity_and_an_offline_one_does_not() {
        // The Microsoft arm cannot reach a real session until the Azure
        // application is approved, so it is proved here against a constructed
        // one, so an approved compatibility override can use this branch directly.
        let offline = identity_of(&Session::offline(ephervex_auth::Account::offline(
            uuid::Uuid::parse_str("b50ad385-829d-3141-a216-7e7d7539ba7f").expect("uuid"),
            "Notch",
        )));
        assert!(!offline.is_authenticated());
        assert_eq!(offline.user_type, "legacy");
        assert_eq!(
            offline.access_token,
            ephervex_minecraft::OFFLINE_ACCESS_TOKEN
        );
        assert_eq!(offline.uuid, "b50ad385-829d-3141-a216-7e7d7539ba7f");

        let microsoft = identity_of(&Session {
            account: ephervex_auth::Account {
                id: uuid::Uuid::parse_str("069a79f4-44e9-4726-a5be-fca90e38aaf5").expect("uuid"),
                name: "Notch".to_owned(),
                kind: ephervex_model::AccountKind::Microsoft,
                authenticated_at: chrono::Utc::now(),
            },
            credentials: Credentials::Microsoft {
                token: ephervex_auth::SecretString::new("a-real-token"),
                expires_at: chrono::Utc::now() + chrono::TimeDelta::hours(24),
                entitlements: ephervex_auth::Entitlements::default(),
                storage: ephervex_auth::CredentialStorage::Keyring,
            },
        });
        assert!(microsoft.is_authenticated());
        assert_eq!(microsoft.user_type, "msa");
        assert_eq!(microsoft.access_token, "a-real-token");
        assert!(
            !format!("{microsoft:?}").contains("a-real-token"),
            "an identity is logged; a token is not "
        );
    }

    #[test]
    fn the_game_directory_is_instance_local_and_never_the_shared_cache() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let record = vanilla(&app, "1.20.1");

        let game = game_directory(&record);
        assert!(game.starts_with(&record.root));
        assert!(!game.starts_with(app.paths().cache_dir()));
        assert_eq!(
            game.file_name().and_then(std::ffi::OsStr::to_str),
            Some("minecraft")
        );
    }
}
