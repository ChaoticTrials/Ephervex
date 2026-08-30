//! Presentation-only models for the desktop frontend.
//!
//! These values deliberately contain no filesystem, provider, or process
//! behavior. They translate the application service records into stable UI
//! states that can be tested without constructing a Slint window.

use std::net::IpAddr;

use ephervex_core::{
    ARTIFACT_CACHE_LIMIT_PRESETS, ArtifactCacheLimit, ArtifactCacheLimitError, BlockedProviderFile,
    ByteUnit, ClientRecord, DEFAULT_ARTIFACT_CACHE_LIMIT_BYTES, DiagnosticKind, DiagnosticScope,
    LaunchAndConnectAvailability, NetworkAccess, PackAnalysis, PackSide, ProviderFileIdentity,
    RuntimeClassification, ServerRecord, ServerStatus, format_binary_bytes,
};
use ephervex_i18n::{Arg, lookup, lookup_args};
use ephervex_model::{
    DefaultModCatalog, LoaderKind, ModProvider, Operation, OperationStatus, StorageClass,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstancePresentation {
    pub id: String,
    pub name: String,
    pub minecraft: String,
    pub loader: String,
    pub loader_kind: &'static str,
    pub storage: String,
    pub storage_kind: &'static str,
    pub source: String,
    pub path: String,
    pub linked_servers: usize,
    pub managed_mods: usize,
    pub needs_attention: bool,
    pub state: String,
    pub state_kind: &'static str,
    pub can_launch: bool,
    /// Whether a launch is under way or a game is running for this instance.
    ///
    /// Not `!can_launch`: an instance that needs attention cannot be launched
    /// and is not running either, and offering to stop it would be a lie.
    pub can_kill: bool,
    pub launch_and_connect: Vec<ConnectPresentation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectPresentation {
    pub server_id: String,
    pub server_name: String,
    pub target: String,
    pub enabled: bool,
    pub reason: String,
}

impl InstancePresentation {
    pub fn from_record(
        record: &ClientRecord,
        linked_servers: usize,
        managed_mods: usize,
        launch_state: Option<&ephervex_core::LaunchState>,
        connections: impl IntoIterator<
            Item = (ServerRecord, Result<LaunchAndConnectAvailability, String>),
        >,
    ) -> Self {
        let (state, state_kind) = match launch_state {
            Some(state) if state.game_pid.is_some() => (lookup("gui-state-running"), "running"),
            Some(_) => (lookup("gui-state-starting"), "starting"),
            None => (lookup("gui-state-ready"), "ready"),
        };
        Self {
            id: record.id.to_string(),
            name: record.name.clone(),
            minecraft: record.minecraft_version.clone(),
            loader: record.loader.to_string(),
            loader_kind: loader_kind_name(record.loader.kind),
            storage: storage_name(record.storage),
            storage_kind: storage_kind_name(record.storage),
            source: record.source.kind.to_string(),
            path: record.root.display().to_string(),
            linked_servers,
            managed_mods,
            needs_attention: record.needs_attention(),
            state,
            state_kind,
            can_launch: launch_state.is_none() && !record.needs_attention(),
            can_kill: launch_state.is_some(),
            launch_and_connect: connections
                .into_iter()
                .map(|(server, availability)| match availability {
                    Ok(availability) => ConnectPresentation {
                        server_id: server.id.to_string(),
                        server_name: server.name,
                        target: availability.target.to_string(),
                        enabled: !availability.client_running,
                        reason: if availability.client_running {
                            lookup("gui-client-already-running")
                        } else {
                            String::new()
                        },
                    },
                    Err(reason) => {
                        let target = format_connection_target(&server);
                        ConnectPresentation {
                            server_id: server.id.to_string(),
                            server_name: server.name,
                            target,
                            enabled: false,
                            reason,
                        }
                    }
                })
                .collect(),
        }
    }
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerPresentation {
    pub id: String,
    pub name: String,
    pub minecraft: String,
    pub loader: String,
    pub loader_kind: &'static str,
    pub storage: String,
    pub storage_kind: &'static str,
    pub relationship: String,
    pub linked_client_id: String,
    pub state: String,
    pub state_kind: &'static str,
    pub state_detail: String,
    pub address: String,
    pub path: String,
    pub installed: bool,
    pub needs_attention: bool,
    pub can_start: bool,
    pub can_stop: bool,
    pub can_restart: bool,
    pub can_kill: bool,
    pub can_send_command: bool,
    pub can_connect: bool,
    pub connect_reason: String,
    pub destructive_recovery: bool,
}

impl ServerPresentation {
    #[must_use]
    pub fn from_status(status: &ServerStatus) -> Self {
        let record = &status.record;
        let controlled = status.runtime.is_controlled();
        let (state_kind, state_id, detail_id, uncertain, needs_recovery) = match &status.runtime {
            RuntimeClassification::Stopped => (
                "stopped",
                "gui-state-stopped",
                "gui-state-ready-to-start",
                false,
                false,
            ),
            RuntimeClassification::Controlled { child, .. } => {
                if child.is_some() {
                    (
                        "running",
                        "gui-state-running",
                        "gui-state-controlled",
                        false,
                        false,
                    )
                } else {
                    (
                        "starting",
                        "gui-state-starting",
                        "gui-state-starting-detail",
                        false,
                        false,
                    )
                }
            }
            RuntimeClassification::UnknownRunning { .. } => (
                "unknown",
                "gui-state-unknown",
                "gui-state-process-may-run",
                true,
                false,
            ),
            RuntimeClassification::Orphaned { .. } => (
                "orphaned",
                "gui-state-orphaned",
                "gui-state-orphaned-detail",
                true,
                false,
            ),
            RuntimeClassification::ProvenStale { .. } => (
                "needs-recovery",
                "gui-state-needs-recovery",
                "gui-state-needs-recovery-detail",
                false,
                true,
            ),
        };
        let installed = record.installation.is_installed();
        Self {
            id: record.id.to_string(),
            name: record.name.clone(),
            minecraft: record.minecraft_version.clone(),
            loader: record.loader.to_string(),
            loader_kind: loader_kind_name(record.loader.kind),
            storage: storage_name(record.storage),
            storage_kind: storage_kind_name(record.storage),
            relationship: record
                .linked_client_id
                .map_or_else(|| lookup("gui-standalone"), |_| lookup("gui-linked-client")),
            linked_client_id: record
                .linked_client_id
                .map_or_else(String::new, |id| id.to_string()),
            state: lookup(state_id),
            state_kind,
            state_detail: lookup(detail_id),
            address: format_connection_target(record),
            path: record.root.display().to_string(),
            installed,
            needs_attention: record.needs_attention(),
            can_start: installed && !status.runtime.may_be_running() && !needs_recovery,
            can_stop: controlled,
            can_restart: controlled,
            can_kill: controlled,
            can_send_command: controlled,
            can_connect: false,
            connect_reason: String::new(),
            destructive_recovery: uncertain,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActivityPresentation {
    pub id: String,
    pub title: String,
    pub status: String,
    pub status_kind: &'static str,
    pub detail: String,
    pub progress: f32,
    pub indeterminate: bool,
    pub cancellable: bool,
    pub failed: bool,
}

impl From<&Operation> for ActivityPresentation {
    fn from(operation: &Operation) -> Self {
        let (status_kind, status_id, failed, failure) = match &operation.status {
            OperationStatus::Pending => ("queued", "gui-state-queued", false, String::new()),
            OperationStatus::Running => ("running", "gui-state-running", false, String::new()),
            OperationStatus::Canceled => ("canceled", "gui-state-canceled", false, String::new()),
            OperationStatus::Completed => {
                ("completed", "gui-state-completed", false, String::new())
            }
            OperationStatus::Failed(message) => {
                ("failed", "gui-state-failed", true, message.clone())
            }
        };
        let fraction = operation.progress.fraction();
        Self {
            id: operation.id.to_string(),
            title: operation.title.clone(),
            status: lookup(status_id),
            status_kind,
            detail: operation.current_step.clone().unwrap_or(failure),
            #[allow(clippy::cast_possible_truncation)]
            progress: fraction.unwrap_or_default() as f32,
            indeterminate: fraction.is_none(),
            cancellable: operation.cancellable && !operation.status.is_terminal(),
            failed,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropertyControl {
    Toggle,
    Integer { minimum: u16, maximum: u16 },
    IpAddress,
    Choice(&'static [&'static str]),
    Text,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PropertyDefinition {
    pub key: &'static str,
    pub label_message_id: &'static str,
    pub control: PropertyControl,
}

pub const SERVER_PROPERTY_DEFINITIONS: &[PropertyDefinition] = &[
    PropertyDefinition {
        key: "motd",
        label_message_id: "gui-message-of-the-day",
        control: PropertyControl::Text,
    },
    PropertyDefinition {
        key: "max-players",
        label_message_id: "gui-maximum-players",
        control: PropertyControl::Integer {
            minimum: 1,
            maximum: u16::MAX,
        },
    },
    PropertyDefinition {
        key: "difficulty",
        label_message_id: "gui-difficulty",
        control: PropertyControl::Choice(&["peaceful", "easy", "normal", "hard"]),
    },
    PropertyDefinition {
        key: "gamemode",
        label_message_id: "gui-game-mode",
        control: PropertyControl::Choice(&["survival", "creative", "adventure", "spectator"]),
    },
    PropertyDefinition {
        key: "allow-flight",
        label_message_id: "gui-allow-flight",
        control: PropertyControl::Toggle,
    },
    PropertyDefinition {
        key: "pvp",
        label_message_id: "gui-player-versus-player",
        control: PropertyControl::Toggle,
    },
    PropertyDefinition {
        key: "white-list",
        label_message_id: "gui-use-allowlist",
        control: PropertyControl::Toggle,
    },
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationResult {
    pub valid: bool,
    pub message: String,
}

#[must_use]
pub fn validate_port(value: &str) -> ValidationResult {
    if value.is_empty() {
        return invalid(&lookup("gui-port-required"));
    }
    if !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return invalid(&lookup("gui-port-digits-only"));
    }
    match value.parse::<u32>() {
        Ok(1..=65_535) => valid(),
        _ => invalid(&lookup("gui-port-range")),
    }
}

#[must_use]
pub fn validate_ip(value: &str, required: bool) -> ValidationResult {
    let value = value.trim();
    if value.is_empty() {
        return if required {
            invalid(&lookup("gui-ip-required"))
        } else {
            valid()
        };
    }
    if value.parse::<IpAddr>().is_ok() {
        valid()
    } else {
        invalid(&lookup("gui-ip-invalid"))
    }
}

#[must_use]
pub fn diagnostic_scope_label(scope: DiagnosticScope) -> String {
    match scope {
        DiagnosticScope::Client(_) => lookup("gui-client"),
        DiagnosticScope::Server(_) => lookup("gui-server"),
        DiagnosticScope::Launcher => lookup("gui-launcher"),
    }
}

#[must_use]
pub fn diagnostic_kind_label(kind: DiagnosticKind) -> String {
    match kind {
        DiagnosticKind::Log => lookup("gui-log"),
        DiagnosticKind::CrashReport => lookup("gui-crash-report"),
    }
}

#[must_use]
pub const fn loader_kind_name(kind: LoaderKind) -> &'static str {
    match kind {
        LoaderKind::Vanilla => "vanilla",
        LoaderKind::Forge => "forge",
        LoaderKind::NeoForge => "neoforge",
    }
}

#[must_use]
pub fn storage_name(storage: StorageClass) -> String {
    match storage {
        StorageClass::Temporary => lookup("gui-temporary"),
        StorageClass::Persistent => lookup("gui-persistent"),
    }
}

#[must_use]
pub const fn storage_kind_name(storage: StorageClass) -> &'static str {
    match storage {
        StorageClass::Temporary => "temporary",
        StorageClass::Persistent => "persistent",
    }
}

pub fn parse_network(
    access: &str,
    address: &str,
    online_mode: bool,
    port: &str,
) -> Result<ephervex_core::ServerNetworkSettings, ValidationResult> {
    let port_validation = validate_port(port);
    if !port_validation.valid {
        return Err(port_validation);
    }
    let access = match access {
        "Localhost" => NetworkAccess::Localhost,
        "LAN" => NetworkAccess::Lan,
        "Custom" => NetworkAccess::Custom,
        _ => return Err(invalid(&lookup("gui-network-access-required"))),
    };
    let address_validation = validate_ip(address, matches!(access, NetworkAccess::Custom));
    if !address_validation.valid {
        return Err(address_validation);
    }
    let port = port
        .parse::<u16>()
        .map_err(|_| invalid(&lookup("gui-port-range")))?;
    Ok(ephervex_core::ServerNetworkSettings {
        online_mode,
        access,
        port,
        bind_address: matches!(access, NetworkAccess::Custom).then(|| address.trim().to_owned()),
    })
}

fn format_connection_target(record: &ServerRecord) -> String {
    let host = match record.network.access {
        NetworkAccess::Localhost => "127.0.0.1".to_owned(),
        NetworkAccess::Lan => "0.0.0.0".to_owned(),
        NetworkAccess::Custom => record.network.bind_address.clone().unwrap_or_default(),
    };
    if host.contains(':') {
        format!("[{host}]:{}", record.network.port)
    } else {
        format!("{host}:{}", record.network.port)
    }
}

fn valid() -> ValidationResult {
    ValidationResult {
        valid: true,
        message: String::new(),
    }
}

fn invalid(message: &str) -> ValidationResult {
    ValidationResult {
        valid: false,
        message: message.to_owned(),
    }
}

/// Whether a result that was produced for one instance should still be shown.
///
/// Anything fetched in the background races the user. A managed-mod list is
/// requested for the instance whose workspace is open, arrives after a provider
/// round trip, and by then the user may have gone back and opened a different
/// one. Applying it anyway would put one instance's mods under another
/// instance's name — a wrong answer, not a slow one.
///
/// Discarding the late result is the whole remedy. Nothing is cancelled and no
/// navigation waits on a request: opening a workspace has already shown that
/// instance's durable state, so a discarded enrichment costs titles the user
/// was not waiting for.
///
/// An empty selection means no workspace is open, which no result belongs to.
#[must_use]
pub fn result_is_current(produced_for: &str, selected: &str) -> bool {
    !selected.is_empty() && produced_for == selected
}

/// What the mod browser is currently waiting for.
///
/// Every field is what was *submitted*, never what a control currently shows: a
/// user keeps typing and switches catalog while an answer is in flight, and an
/// answer belongs to the question that was actually asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrowserQuestion<'a> {
    pub instance: &'a str,
    pub provider: &'a str,
    /// The submitted search text, or the project a review was opened for.
    pub reference: &'a str,
}

/// How the settings view shows the recorded mod-catalog preference.
///
/// Three fields rather than one, because the view has three different things to
/// say: which chip is selected, what the file actually records, and whether the
/// user chose it at all. Collapsing them would either lose the difference
/// between Ephervex's default and a user picking the same catalog, or show a
/// name written by a newer Ephervex as though it were one of this build's.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CatalogPreferenceView {
    /// The catalog id whose chip is selected, empty when the recorded name is
    /// not one this build can resolve.
    pub selected: String,
    /// The recorded name exactly as stored, empty when nothing is recorded.
    pub configured: String,
    /// Whether the user chose it, as opposed to taking the default.
    pub chosen: bool,
}

impl CatalogPreferenceView {
    #[must_use]
    pub fn of(preference: &DefaultModCatalog) -> Self {
        Self {
            selected: preference
                .resolved()
                .map(ModProvider::as_str)
                .unwrap_or_default()
                .to_owned(),
            configured: preference.stored().to_owned(),
            chosen: preference.is_chosen(),
        }
    }
}

/// The catalog the browser should open on, or `None` to leave it alone.
///
/// Nothing moves when the browser is already showing the preferred catalog, and
/// nothing moves when the recorded name is one this build cannot resolve:
/// switching then would search a catalog the setting does not name and present
/// its results as though the preference had been honored. The settings screen
/// is where an unresolvable name is reported.
#[must_use]
pub fn catalog_to_open_on<'a>(preferred: &'a str, showing: &str) -> Option<&'a str> {
    (!preferred.is_empty() && preferred != showing).then_some(preferred)
}

/// What switching the mod browser's catalog resets every piece of in-flight
/// search state to.
///
/// Named as one value, applied together, rather than as separate `ui.set_*`
/// calls decided ad hoc at the call site: a search still in flight for the
/// catalog just left behind has to lose every way it could land on the one
/// now selected, in the same step that selects it, or a late answer for the
/// old catalog can still pass the staleness guard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogSwitchReset {
    /// The provider an arriving answer is now checked against. A search still
    /// carrying the one just left behind can never match this again.
    pub provider: String,
    /// The submitted-search identity, put back to nothing.
    ///
    /// Without this, switching away and back to the same catalog before an
    /// old answer returns reinstates the exact question it was waiting on
    /// (same instance, same provider, same text, same offset), and the old
    /// answer matches again. The submitted identity has to be retired for
    /// good, not just made momentarily wrong, so a switch back does not
    /// resurrect it. The visible query field is a different piece of state
    /// and keeps whatever the user typed.
    pub submitted: String,
    /// Never left `true`: the newly selected catalog must be searchable
    /// immediately, not stuck disabled behind whatever was still loading for
    /// the catalog just left.
    pub loading: bool,
    /// The offset the browser is waiting for a page at, put back to the
    /// start: the old catalog's continuation belonged to a question that no
    /// longer exists, and carrying it over would validate a page that was
    /// never asked for on the new catalog.
    pub requested_offset: i32,
    /// The request the browser is waiting on, put back to nothing.
    ///
    /// `submitted` alone cannot survive a round trip: switching away and back
    /// clears it, but a later search for the exact same words fills it back
    /// in with the exact same value, and at that point instance, provider,
    /// query, and offset can all agree with an old request again. This is
    /// the opaque per-request identity nothing else can reconstruct, so it
    /// has to be retired here too rather than left to be overwritten by
    /// coincidence.
    pub request_id: String,
}

impl CatalogSwitchReset {
    #[must_use]
    pub fn for_provider(provider: &str) -> Self {
        Self {
            provider: provider.to_owned(),
            submitted: String::new(),
            loading: false,
            requested_offset: 0,
            request_id: String::new(),
        }
    }
}

/// Whether an answer belongs to the question the browser is waiting on.
///
/// All three have to agree. The catalog is not optional: two catalogs spell
/// project ids their own way and answer the same words differently, so a page
/// that matched only the text could replace a page from the other one.
#[must_use]
pub fn browser_answer_is_current(
    waiting: BrowserQuestion<'_>,
    answer: BrowserQuestion<'_>,
) -> bool {
    result_is_current(answer.instance, waiting.instance)
        && answer.provider == waiting.provider
        && answer.reference == waiting.reference
}

/// Whether one page of search results belongs where the browser is currently
/// waiting for its next page.
///
/// Beyond identifying the question, a page has to land at the offset the
/// browser actually asked for, tracked independently of how many rows ended up
/// on screen: a provider can consume more of a page than this launcher goes on
/// to display, so comparing against the visible row count would reject the
/// very page that was requested. A first page arriving after the user has
/// already loaded more is rejected because its offset no longer matches what
/// is expected, and a later page arriving after a fresh search is rejected the
/// same way, because the fresh search reset what is expected back to zero.
///
/// `request_id` is checked on top of all of that, not instead of it: a search
/// resubmitted for the exact same instance, catalog, text, and offset (typing
/// the same query again, or switching catalog away and back before an old
/// answer returns and then asking the same thing) makes the other four fields
/// agree with an old, unrelated request. Only the opaque per-request identity
/// tells the two apart, so an answer belongs here only when the request that
/// produced it is the one this specific answer is being checked against.
#[must_use]
pub fn search_page_belongs(
    waiting: BrowserQuestion<'_>,
    answer: BrowserQuestion<'_>,
    offset: u32,
    expected_offset: u32,
    request_id: &str,
    expected_request_id: &str,
) -> bool {
    browser_answer_is_current(waiting, answer)
        && offset == expected_offset
        && request_id == expected_request_id
}

/// Whether a submitted mod search has actually finished, successfully.
///
/// Distinct from "quiet": a search that is still loading, was never
/// submitted, or ended in a notice (a credential problem, an unreachable
/// catalog) has not finished successfully, and nothing that depends on the
/// outcome can be claimed yet.
#[must_use]
pub const fn mod_search_finished_successfully(
    loading: bool,
    ran: bool,
    notice_is_empty: bool,
) -> bool {
    !loading && ran && notice_is_empty
}

/// Whether the mod browser has proven there is nothing that matches, as
/// opposed to merely having nothing to show yet.
///
/// A raw provider page can be consumed entirely by defensive filtering (a
/// class this launcher cannot confirm as a Mod, a file listing it cannot
/// read) while later raw pages still exist. Zero visible rows is therefore
/// not enough on its own: the final "nothing matches" claim only holds once
/// the search actually finished and the provider has no further page to
/// offer. While a continuation remains, the honest state is "nothing shown
/// yet", not "nothing matches", and Load More stays the way to find out.
#[must_use]
pub const fn mod_browser_has_no_results(
    finished_successfully: bool,
    visible_rows: usize,
    has_continuation: bool,
) -> bool {
    finished_successfully && visible_rows == 0 && !has_continuation
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::path::PathBuf;
    use std::{net::IpAddr, num::NonZeroU16};

    use chrono::Utc;
    use ephervex_core::{
        ImportSource, LaunchAndConnectAvailability, RuntimeClassification, ServerInstallation,
        ServerPersistenceHealth, ServerPreparation,
    };
    use ephervex_model::{
        ConnectionTarget, InstanceId, LoaderSelection, ServerId, ServerLaunch, ServerRelativePath,
    };

    use super::*;

    fn client(storage: StorageClass, loader: LoaderSelection) -> ClientRecord {
        ClientRecord {
            id: InstanceId::new(),
            name: "Reproduction".to_owned(),
            root: PathBuf::from("/tmp/reproduction"),
            storage,
            minecraft_version: "1.20.1".to_owned(),
            loader,
            source: ImportSource::manual(),
            java_override: None,
            jvm_override: None,
            attention: Vec::new(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            last_launched_at: None,
        }
    }

    fn server(runtime: RuntimeClassification) -> ServerStatus {
        ServerStatus {
            record: ServerRecord {
                id: ServerId::new(),
                name: "Test server".to_owned(),
                root: PathBuf::from("/tmp/server"),
                storage: StorageClass::Temporary,
                linked_client_id: None,
                minecraft_version: "1.20.1".to_owned(),
                loader: LoaderSelection::vanilla(),
                installation: ServerInstallation::Installed {
                    launch: ServerLaunch::Jar {
                        path: ServerRelativePath::new("server.jar").expect("safe relative path"),
                    },
                },
                network: ephervex_core::ServerNetworkSettings::default(),
                operator_setup: None,
                source: ImportSource::manual(),
                java_override: None,
                jvm_override: None,
                attention: Vec::new(),
                created_at: Utc::now(),
                updated_at: Utc::now(),
                last_started_at: None,
            },
            persistence: ServerPersistenceHealth::Healthy,
            runtime,
        }
    }

    #[test]
    fn instance_storage_and_supported_loader_names_are_exact() {
        let temporary = InstancePresentation::from_record(
            &client(StorageClass::Temporary, LoaderSelection::vanilla()),
            0,
            0,
            None,
            [],
        );
        assert_eq!(temporary.storage, "Temporary");
        assert_eq!(temporary.storage_kind, "temporary");
        assert_eq!(temporary.loader, "Vanilla");
        assert_eq!(
            [LoaderKind::Vanilla, LoaderKind::Forge, LoaderKind::NeoForge].map(loader_kind_name),
            ["vanilla", "forge", "neoforge"]
        );
    }

    #[test]
    fn instance_runtime_state_is_independent_from_launch_action_availability() {
        let record = client(StorageClass::Temporary, LoaderSelection::vanilla());
        let preparing = ephervex_core::LaunchState {
            owner_pid: std::process::id(),
            owner_start_ticks: None,
            game_pid: None,
            since: Utc::now(),
        };
        let starting = InstancePresentation::from_record(&record, 0, 0, Some(&preparing), []);
        assert_eq!(starting.state_kind, "starting");
        assert!(!starting.can_launch);
        // Stopping a launch that has not reached the game yet cancels the
        // preparation, which is what the action means in that state.
        assert!(starting.can_kill);

        let running = ephervex_core::LaunchState {
            game_pid: Some(std::process::id()),
            ..preparing
        };
        let running = InstancePresentation::from_record(&record, 0, 0, Some(&running), []);
        assert_eq!(running.state_kind, "running");
        assert!(!running.can_launch);
        assert!(running.can_kill);

        let idle = InstancePresentation::from_record(&record, 0, 0, None, []);
        assert!(idle.can_launch);
        assert!(!idle.can_kill);
    }

    #[test]
    fn an_instance_needing_attention_is_neither_launchable_nor_killable() {
        // The two are not each other's negation, and an instance that cannot be
        // launched must not be offered a way to stop something that is not
        // running.
        let mut record = client(StorageClass::Temporary, LoaderSelection::vanilla());
        record.add_attention(ephervex_model::AttentionReason::BestEffortServerConversion);
        let item = InstancePresentation::from_record(&record, 0, 0, None, []);
        assert!(item.needs_attention);
        assert!(!item.can_launch);
        assert!(!item.can_kill);
    }

    #[test]
    fn launch_and_connect_is_enabled_only_for_available_linked_targets() {
        let record = client(StorageClass::Temporary, LoaderSelection::vanilla());
        let linked = server(RuntimeClassification::Stopped).record;
        let availability = LaunchAndConnectAvailability {
            client: record.id,
            server: linked.id,
            target: ConnectionTarget {
                address: IpAddr::from([127, 0, 0, 1]),
                port: NonZeroU16::new(25_565).expect("nonzero port"),
            },
            server_preparation: ServerPreparation::Start,
            client_running: false,
        };
        let item =
            InstancePresentation::from_record(&record, 1, 0, None, [(linked, Ok(availability))]);
        assert_eq!(item.launch_and_connect.len(), 1);
        assert!(item.launch_and_connect[0].enabled);
    }

    #[test]
    fn server_actions_follow_runtime_ownership() {
        let stopped = ServerPresentation::from_status(&server(RuntimeClassification::Stopped));
        assert!(stopped.can_start);
        assert!(!stopped.can_stop);

        let uncertain =
            ServerPresentation::from_status(&server(RuntimeClassification::UnknownRunning {
                supervisor: super::super::test_support::process_evidence(),
                child: None,
            }));
        assert!(!uncertain.can_start);
        assert!(!uncertain.can_stop);
        assert!(uncertain.destructive_recovery);

        let supervisor = super::super::test_support::process_evidence();
        let starting =
            ServerPresentation::from_status(&server(RuntimeClassification::Controlled {
                supervisor: supervisor.clone(),
                child: None,
            }));
        assert_eq!(starting.state_kind, "starting");

        let mut child = supervisor;
        child.role = ephervex_core::ProcessRole::JavaChild;
        let running = ServerPresentation::from_status(&server(RuntimeClassification::Controlled {
            supervisor: super::super::test_support::process_evidence(),
            child: Some(child),
        }));
        assert_eq!(running.state_kind, "running");
    }

    #[test]
    fn typed_property_controls_keep_eula_out() {
        assert!(
            SERVER_PROPERTY_DEFINITIONS.iter().any(|item| {
                item.key == "allow-flight" && item.control == PropertyControl::Toggle
            })
        );
        assert!(
            SERVER_PROPERTY_DEFINITIONS
                .iter()
                .all(|item| item.key != "eula")
        );
    }

    #[test]
    fn ports_accept_digits_only_and_validate_the_semantic_range() {
        assert!(validate_port("25565").valid);
        assert!(!validate_port("25k").valid);
        assert!(!validate_port("0").valid);
        assert!(!validate_port("65536").valid);
    }

    #[test]
    fn custom_bind_addresses_are_validated_immediately() {
        assert!(validate_ip("127.0.0.1", true).valid);
        assert!(validate_ip("::1", true).valid);
        assert!(!validate_ip("localhost", true).valid);
        assert!(!validate_ip("", true).valid);
        assert!(validate_ip("", false).valid);
    }

    #[test]
    fn a_background_result_for_another_instance_is_not_shown_under_this_one() {
        let open = InstanceId::new();
        let other = InstanceId::new();

        assert!(
            result_is_current(&open.to_string(), &open.to_string()),
            "the answer to the question the open workspace asked is shown"
        );
        assert!(
            !result_is_current(&other.to_string(), &open.to_string()),
            "an answer about a different instance is discarded, not renamed"
        );
        assert!(
            !result_is_current(&other.to_string(), ""),
            "with no workspace open there is nothing for a result to belong to"
        );
    }
}

/// One entry in the artifact-cache limit selector.
///
/// The list is the mapping. A selector reports the index it is on, and this is
/// what that index means, so nothing has to parse a label — and in particular
/// nothing has to parse a *localized* label, which is how a settings screen ends
/// up behaving differently in another language.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheLimitChoice {
    /// What the selector shows. Preset sizes are exact technical values and are
    /// deliberately not translated; the two open-ended entries are.
    pub label: String,
    pub kind: CacheLimitKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheLimitKind {
    /// A fixed budget in bytes.
    Preset(u64),
    /// A budget the user types, in the unit they pick beside it.
    Custom,
    Unlimited,
}

/// Every choice the limit selector offers, in the order it shows them.
#[must_use]
pub fn cache_limit_choices() -> Vec<CacheLimitChoice> {
    let mut choices: Vec<CacheLimitChoice> = ARTIFACT_CACHE_LIMIT_PRESETS
        .into_iter()
        .map(|bytes| CacheLimitChoice {
            label: format_binary_bytes(bytes),
            kind: CacheLimitKind::Preset(bytes),
        })
        .collect();
    choices.push(CacheLimitChoice {
        label: lookup("gui-artifact-cache-custom"),
        kind: CacheLimitKind::Custom,
    });
    choices.push(CacheLimitChoice {
        label: lookup("gui-artifact-cache-unlimited"),
        kind: CacheLimitKind::Unlimited,
    });
    choices
}

/// Which entry the selector should open on for a stored budget.
///
/// A budget that is not one of the presets is Custom, which is what makes a
/// hand-edited or previously typed size come back as the user left it rather
/// than snapping to the nearest preset.
#[must_use]
pub fn cache_limit_selection(limit: ArtifactCacheLimit) -> usize {
    let choices = cache_limit_choices();
    let wanted = match limit {
        ArtifactCacheLimit::Unlimited => CacheLimitKind::Unlimited,
        ArtifactCacheLimit::Limited(bytes) => {
            if ARTIFACT_CACHE_LIMIT_PRESETS.contains(&bytes) {
                CacheLimitKind::Preset(bytes)
            } else {
                CacheLimitKind::Custom
            }
        }
    };
    choices
        .iter()
        .position(|choice| choice.kind == wanted)
        // The list always contains Custom, so this is a total function; falling
        // back to the first entry keeps it total without an unwrap.
        .unwrap_or_default()
}

/// The size and unit a Custom entry should start from for a stored budget.
///
/// The largest unit that divides the budget exactly, so `500 GiB` comes back as
/// `500` and `GiB` rather than as `512000` and `MiB`. A budget no offered unit
/// divides exactly is shown in mebibytes, rounded down, because that is the
/// finest the selector can express.
#[must_use]
pub fn cache_limit_custom_fields(limit: ArtifactCacheLimit) -> (String, String) {
    let bytes = limit.bytes().unwrap_or(DEFAULT_ARTIFACT_CACHE_LIMIT_BYTES);
    let unit = ByteUnit::ALL
        .into_iter()
        .rev()
        .find(|unit| bytes % unit.bytes() == 0)
        .unwrap_or(ByteUnit::Mebibytes);
    ((bytes / unit.bytes()).to_string(), unit.label().to_owned())
}

/// The units a custom size may be entered in.
#[must_use]
pub fn cache_limit_units() -> Vec<String> {
    ByteUnit::ALL
        .into_iter()
        .map(|unit| unit.label().to_owned())
        .collect()
}

/// The budget a selection describes, or why it cannot be one.
pub fn cache_limit_from_selection(
    index: usize,
    custom_value: &str,
    custom_unit: &str,
) -> Result<ArtifactCacheLimit, ArtifactCacheLimitError> {
    let choices = cache_limit_choices();
    match choices.get(index).map(|choice| choice.kind) {
        Some(CacheLimitKind::Preset(bytes)) => ArtifactCacheLimit::from_bytes(bytes),
        Some(CacheLimitKind::Unlimited) => Ok(ArtifactCacheLimit::Unlimited),
        // An index the list does not have can only come from a view and a
        // backend that disagree, and treating it as Custom asks the typed parser
        // what the fields say rather than guessing a budget.
        Some(CacheLimitKind::Custom) | None => {
            let unit = ByteUnit::from_label(custom_unit).ok_or_else(|| {
                ArtifactCacheLimitError::Malformed {
                    value: custom_unit.trim().to_owned(),
                }
            })?;
            ArtifactCacheLimit::parse_in_unit(custom_value, unit)
        }
    }
}

/// One optional client component the import wizard can offer, translated from
/// the normalized [`ephervex_core::OptionalComponent`] the pack analysis
/// carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportOptionPresentation {
    /// The component's stable logical identity, and the join key a selection
    /// is made by.
    pub logical_path: String,
    pub display: String,
    pub selected: bool,
}

/// One file a provider refused to serve, translated from
/// [`ephervex_core::BlockedProviderFile`] for the import modal's blocked-file
/// list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockedFilePresentation {
    /// The identity a skip decision or a watcher match is recorded against.
    pub identity: ProviderFileIdentity,
    /// The project's own name, falling back to its catalog id.
    pub project_line: String,
    /// The file's own name, falling back to its catalog id.
    pub file_line: String,
    /// A link that starts this exact file's download on the provider's own
    /// site, when enough was known to build one.
    pub browser_url: String,
}

impl BlockedFilePresentation {
    #[must_use]
    fn from_blocked(blocked: &BlockedProviderFile) -> Self {
        let project_id = blocked.identity.project_id.as_deref().unwrap_or_default();
        let file_id = blocked.identity.file_id.as_deref().unwrap_or_default();
        Self {
            identity: blocked.identity.clone(),
            project_line: blocked
                .project_name
                .clone()
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| project_id.to_owned()),
            file_line: blocked
                .file_name
                .clone()
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| file_id.to_owned()),
            browser_url: blocked.browser_url.clone().unwrap_or_default(),
        }
    }
}

/// The provider-neutral facts an import analysis has enough information to
/// show, independent of whether provider resolution has finished.
///
/// Built from a local analysis alone, `optional` is empty and `optional_count`
/// still names how many decisions the pack will eventually ask for: a
/// CurseForge export's pending files are counted before they have logical
/// paths to select by. See [`ephervex_core::PackAnalysis::optional_components`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportPresentation {
    pub format: String,
    pub name: String,
    pub version: String,
    pub author: String,
    pub minecraft_version: String,
    pub loader: String,
    pub required_count: usize,
    pub optional_count: usize,
    pub optional: Vec<ImportOptionPresentation>,
    pub needs_provider_resolution: bool,
    /// Declared files a provider refused to serve and that neither the shared
    /// cache nor an explicit skip has cleared yet. Import stays disabled
    /// while this is non-empty.
    pub blocked: Vec<BlockedFilePresentation>,
}

impl ImportPresentation {
    #[must_use]
    pub fn from_analysis(analysis: &PackAnalysis) -> Self {
        Self {
            format: analysis.format.to_string(),
            name: analysis.name().to_owned(),
            version: analysis.recipe.original_version.clone().unwrap_or_default(),
            author: analysis.author().unwrap_or_default().to_owned(),
            minecraft_version: analysis.minecraft_version().to_owned(),
            loader: analysis.loader().to_string(),
            required_count: analysis.required_file_count(PackSide::Client),
            optional_count: analysis.optional_file_count(PackSide::Client),
            optional: analysis
                .optional_components(PackSide::Client)
                .into_iter()
                .map(|component| ImportOptionPresentation {
                    logical_path: component.logical_path.to_string_lossy().into_owned(),
                    display: component.display_name(),
                    selected: component.starts_selected(),
                })
                .collect(),
            needs_provider_resolution: analysis.needs_provider_resolution(),
            blocked: analysis
                .blocked
                .iter()
                .map(BlockedFilePresentation::from_blocked)
                .collect(),
        }
    }

    /// "`CurseForge` - Example Pack 1.2.0"
    #[must_use]
    pub fn header_line(&self) -> String {
        let title = if self.version.is_empty() {
            self.name.clone()
        } else {
            format!("{} {}", self.name, self.version)
        };
        lookup_args(
            "gui-import-pack-header",
            [
                ("first", Arg::from(self.format.as_str())),
                ("second", Arg::from(title.as_str())),
            ],
        )
    }

    /// "by Example Author", or empty when the format names no author.
    #[must_use]
    pub fn author_line(&self) -> String {
        if self.author.is_empty() {
            String::new()
        } else {
            lookup_args(
                "gui-import-author",
                [("value", Arg::from(self.author.as_str()))],
            )
        }
    }

    /// "Minecraft 1.21.1"
    #[must_use]
    pub fn minecraft_line(&self) -> String {
        lookup_args(
            "gui-minecraft-version-label",
            [("value", Arg::from(self.minecraft_version.as_str()))],
        )
    }

    #[must_use]
    pub fn required_line(&self) -> String {
        lookup_args(
            "gui-import-required-count",
            [("count", Arg::from(self.required_count))],
        )
    }

    #[must_use]
    pub fn optional_line(&self) -> String {
        lookup_args(
            "gui-import-optional-count",
            [("count", Arg::from(self.optional_count))],
        )
    }
}

/// A coarse category for an import analysis/resolution failure, used only to
/// decide what the modal shows alongside the error text (for example, an
/// affordance to open Providers & APIs). Classified from the core error's own
/// stable message id, never from its localized prose: the id is a contract,
/// the text is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImportErrorKind {
    /// No CurseForge API key is configured at all.
    MissingCredential,
    /// A key is configured, but the provider refused it.
    RejectedCredential,
    /// The exact file the pack declares could not be found or served.
    FileUnavailable,
    /// The exact file's author does not permit third-party downloads.
    DistributionRestricted,
    /// The archive or its manifest could not be understood.
    MalformedPack,
    /// The pack declares a mod loader Ephervex does not support.
    UnsupportedLoader,
    /// Any other failure: an ordinary network/provider problem, or a local
    /// setup failure with no core error behind it at all.
    #[default]
    Other,
}

impl ImportErrorKind {
    #[must_use]
    pub fn from_message_id(message_id: &str) -> Self {
        match message_id {
            "error-mod-credential-missing" | "error-mod-credential-unreadable" => {
                Self::MissingCredential
            }
            "error-mod-credential-rejected" => Self::RejectedCredential,
            "error-mod-file-unknown" | "error-mod-file-unavailable" => Self::FileUnavailable,
            "error-mod-file-distribution-restricted" => Self::DistributionRestricted,
            "error-pack-manifest-missing"
            | "error-pack-manifest-invalid"
            | "error-pack-manifest-unsupported-version"
            | "error-pack-manifest-type-unsupported" => Self::MalformedPack,
            "error-pack-loader-unsupported" => Self::UnsupportedLoader,
            _ => Self::Other,
        }
    }

    /// The Slint-facing kebab-case name, matching this codebase's convention
    /// for string-typed status properties (`import-status-kind` and the
    /// like).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MissingCredential => "missing-credential",
            Self::RejectedCredential => "rejected-credential",
            Self::FileUnavailable => "file-unavailable",
            Self::DistributionRestricted => "distribution-restricted",
            Self::MalformedPack => "malformed-pack",
            Self::UnsupportedLoader => "unsupported-loader",
            Self::Other => "other",
        }
    }
}

/// A localized import failure, carrying both the text the modal shows and
/// the stable category it was classified from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportFailure {
    pub message: String,
    pub kind: ImportErrorKind,
}

#[cfg(test)]
mod import_tests {
    use super::*;

    fn presentation() -> ImportPresentation {
        ImportPresentation {
            format: "CurseForge".to_owned(),
            name: "Example Pack".to_owned(),
            version: "1.2.0".to_owned(),
            author: "Example Author".to_owned(),
            minecraft_version: "1.21.1".to_owned(),
            loader: "NeoForge 21.1.42".to_owned(),
            required_count: 18,
            optional_count: 3,
            optional: vec![
                ImportOptionPresentation {
                    logical_path: "mods/optional-a.jar".to_owned(),
                    display: "mods/optional-a.jar".to_owned(),
                    selected: false,
                },
                ImportOptionPresentation {
                    logical_path: "mods/optional-b.jar".to_owned(),
                    display: "mods/optional-b.jar".to_owned(),
                    selected: true,
                },
            ],
            needs_provider_resolution: false,
            blocked: Vec::new(),
        }
    }

    #[test]
    fn the_header_line_names_the_provider_and_the_pack_with_its_version() {
        let header = presentation().header_line();
        assert!(header.contains("CurseForge"), "{header}");
        assert!(header.contains("Example Pack 1.2.0"), "{header}");
    }

    #[test]
    fn a_pack_with_no_version_omits_the_trailing_space() {
        let mut facts = presentation();
        facts.version.clear();
        let header = facts.header_line();
        assert!(header.contains("Example Pack"), "{header}");
        assert!(!header.contains("Example Pack "), "{header}");
    }

    #[test]
    fn an_author_the_format_did_not_declare_produces_no_line() {
        let mut facts = presentation();
        facts.author.clear();
        assert_eq!(facts.author_line(), "");
    }

    #[test]
    fn required_and_optional_counts_read_back_from_the_composed_lines() {
        let facts = presentation();
        assert!(
            facts.required_line().contains("18"),
            "{}",
            facts.required_line()
        );
        assert!(
            facts.optional_line().contains('3'),
            "{}",
            facts.optional_line()
        );
    }

    #[test]
    fn a_pending_curseforge_analysis_has_no_selectable_optional_list_yet() {
        // Before provider resolution, `optional_components()` reports nothing
        // even though the pack declares optional files: they have no logical
        // path to select by until the provider names them.
        let mut facts = presentation();
        facts.optional.clear();
        facts.needs_provider_resolution = true;
        assert!(facts.optional.is_empty());
        assert_eq!(facts.optional_count, 3);
    }

    #[test]
    fn each_classified_message_id_maps_to_its_own_error_kind() {
        let cases = [
            (
                "error-mod-credential-missing",
                ImportErrorKind::MissingCredential,
            ),
            (
                "error-mod-credential-unreadable",
                ImportErrorKind::MissingCredential,
            ),
            (
                "error-mod-credential-rejected",
                ImportErrorKind::RejectedCredential,
            ),
            ("error-mod-file-unknown", ImportErrorKind::FileUnavailable),
            (
                "error-mod-file-unavailable",
                ImportErrorKind::FileUnavailable,
            ),
            (
                "error-mod-file-distribution-restricted",
                ImportErrorKind::DistributionRestricted,
            ),
            (
                "error-pack-manifest-missing",
                ImportErrorKind::MalformedPack,
            ),
            (
                "error-pack-manifest-invalid",
                ImportErrorKind::MalformedPack,
            ),
            (
                "error-pack-manifest-unsupported-version",
                ImportErrorKind::MalformedPack,
            ),
            (
                "error-pack-manifest-type-unsupported",
                ImportErrorKind::MalformedPack,
            ),
            (
                "error-pack-loader-unsupported",
                ImportErrorKind::UnsupportedLoader,
            ),
            ("error-pack-download-failed", ImportErrorKind::Other),
            ("error-some-unrelated-id", ImportErrorKind::Other),
        ];
        for (message_id, expected) in cases {
            assert_eq!(
                ImportErrorKind::from_message_id(message_id),
                expected,
                "{message_id}"
            );
        }
    }

    #[test]
    fn an_unclassified_message_id_defaults_to_the_same_kind_as_a_setup_failure() {
        // A setup failure (the backend thread never reached a core error at
        // all) and an ordinary unclassified provider failure must look the
        // same to the modal: neither gets a special affordance.
        assert_eq!(
            ImportErrorKind::from_message_id("gui-import-resolution-unavailable"),
            ImportErrorKind::default()
        );
    }
}

/// The rules that keep a late catalog answer from landing on a question the
/// The recorded catalog preference as the settings view and the browser read
/// it.
#[cfg(test)]
mod catalog_preference_tests {
    use super::*;

    #[test]
    fn nothing_recorded_selects_the_built_in_catalog_and_shows_no_override() {
        let view = CatalogPreferenceView::of(&DefaultModCatalog::BuiltIn);
        assert_eq!(view.selected, "modrinth");
        assert_eq!(view.configured, "");
        assert!(!view.chosen);
    }

    #[test]
    fn a_recorded_catalog_is_shown_as_the_users_own_choice() {
        let view = CatalogPreferenceView::of(&DefaultModCatalog::Selected(ModProvider::CurseForge));
        assert_eq!(view.selected, "curseforge");
        assert_eq!(view.configured, "curseforge");
        assert!(view.chosen);

        // The same catalog Ephervex would have used anyway is still a decision,
        // and the view has to be able to say so.
        let matching =
            CatalogPreferenceView::of(&DefaultModCatalog::Selected(ModProvider::Modrinth));
        assert_eq!(matching.selected, "modrinth");
        assert!(matching.chosen);
    }

    #[test]
    fn a_catalog_this_build_cannot_resolve_is_shown_as_itself_with_nothing_selected() {
        let view = CatalogPreferenceView::of(&DefaultModCatalog::Unrecognized(
            "a-later-catalog".to_owned(),
        ));
        assert_eq!(
            view.selected, "",
            "no chip may claim to be the catalog the setting names"
        );
        assert_eq!(view.configured, "a-later-catalog");
        assert!(view.chosen);
    }

    #[test]
    fn opening_an_instance_moves_the_browser_to_the_preferred_catalog() {
        assert_eq!(
            catalog_to_open_on("curseforge", "modrinth"),
            Some("curseforge")
        );
    }

    #[test]
    fn a_browser_already_on_the_preferred_catalog_is_left_alone() {
        // Nothing to reset, so an in-flight search for that same catalog keeps
        // its identity rather than being retired for no reason.
        assert_eq!(catalog_to_open_on("curseforge", "curseforge"), None);
    }

    #[test]
    fn an_unresolvable_preference_never_moves_the_browser_to_another_catalog() {
        assert_eq!(catalog_to_open_on("", "modrinth"), None);
    }

    #[test]
    fn opening_on_the_preferred_catalog_retires_the_in_flight_request() {
        // The same reset a chip click applies, so an answer for the catalog
        // the browser was on cannot land under the one it opened on.
        let reset = CatalogSwitchReset::for_provider("curseforge");
        assert!(!search_page_belongs(
            BrowserQuestion {
                instance: "client-1",
                provider: &reset.provider,
                reference: &reset.submitted,
            },
            BrowserQuestion {
                instance: "client-1",
                provider: "modrinth",
                reference: "jei",
            },
            reset.requested_offset.try_into().unwrap_or(0),
            0,
            &reset.request_id,
            "7",
        ));
    }
}

/// browser has moved on from.
#[cfg(test)]
mod browser_staleness_tests {
    use super::*;

    /// The identity of one browser question.
    fn question<'a>(
        instance: &'a str,
        provider: &'a str,
        reference: &'a str,
    ) -> BrowserQuestion<'a> {
        BrowserQuestion {
            instance,
            provider,
            reference,
        }
    }

    /// A request id neither side of a check cares about, for tests about one
    /// of the other identity fields. Equal on both sides, the same way an
    /// unrelated test passes matching offsets: the dimension being tested is
    /// isolated by holding every other one still.
    const IGNORED_REQUEST_ID: &str = "ignored";

    #[test]
    fn a_late_page_for_a_superseded_search_cannot_replace_the_newer_one() {
        // Search A starts, search B starts, A finishes last. A must not land.
        let waiting = question("client-1", "curseforge", "create");
        assert!(!search_page_belongs(
            waiting,
            question("client-1", "curseforge", "jei"),
            0,
            0,
            IGNORED_REQUEST_ID,
            IGNORED_REQUEST_ID,
        ));
        assert!(search_page_belongs(
            waiting,
            question("client-1", "curseforge", "create"),
            0,
            0,
            IGNORED_REQUEST_ID,
            IGNORED_REQUEST_ID,
        ));
    }

    #[test]
    fn the_same_words_asked_of_another_catalog_are_a_different_question() {
        let waiting = question("client-1", "curseforge", "jei");
        assert!(!search_page_belongs(
            waiting,
            question("client-1", "modrinth", "jei"),
            0,
            0,
            IGNORED_REQUEST_ID,
            IGNORED_REQUEST_ID,
        ));
    }

    #[test]
    fn a_resubmitted_identical_search_is_told_apart_only_by_its_request_id() {
        // Request #1: Modrinth "jei" at offset 0. The user then switches to
        // CurseForge and back to Modrinth before it returns, and submits a
        // genuinely fresh search for the exact same words. Instance,
        // provider, query, and offset now agree between #1 and #2; only the
        // request id still tells them apart.
        let request_1 = question("client-1", "modrinth", "jei");
        let request_1_id = "1";
        let request_2 = question("client-1", "modrinth", "jei");
        let request_2_id = "2";

        // What the browser is waiting on after submitting #2.
        let waiting = request_2;
        let expected_request_id = request_2_id;

        // 1. The fresh answer #2 is accepted.
        assert!(search_page_belongs(
            waiting,
            request_2,
            0,
            0,
            request_2_id,
            expected_request_id,
        ));

        // 2. The old answer #1, arriving after #2 was submitted, is rejected
        // even though instance, provider, query, and offset all agree with
        // what the browser is waiting on.
        assert!(!search_page_belongs(
            waiting,
            request_1,
            0,
            0,
            request_1_id,
            expected_request_id,
        ));

        // 3. The same check, unchanged, is what protects the browser whether
        // #1 arrives while #2 is still in flight or only after #2 has
        // already completed and its rows are already on screen: nothing
        // about accepting #2 makes the expectation any less specific, so #1
        // is still rejected exactly the same way.
        assert!(!search_page_belongs(
            waiting,
            request_1,
            0,
            0,
            request_1_id,
            expected_request_id,
        ));

        // 4. A "load more" response is rejected on request id alone, even
        // when its own provider/query/offset tuple matches exactly.
        let load_more_answer = question("client-1", "modrinth", "jei");
        assert!(!search_page_belongs(
            waiting,
            load_more_answer,
            10,
            10,
            "some-other-load-more-request",
            expected_request_id,
        ));

        // 5. The exact expected request id, paired with the correct existing
        // tuple, is accepted.
        assert!(search_page_belongs(
            waiting,
            request_2,
            0,
            0,
            expected_request_id,
            expected_request_id,
        ));
    }

    #[test]
    fn switching_catalog_resets_every_field_a_stale_answer_could_slip_past() {
        // A Modrinth "jei" search is still in flight when the user switches
        // the catalog chip to CurseForge, then switches back to Modrinth
        // before that old search ever returns. No new search is submitted
        // anywhere in between.
        let modrinth_search_in_flight = question("client-1", "modrinth", "jei");
        let request_1_id = "1";

        let switch_to_curseforge = CatalogSwitchReset::for_provider("curseforge");
        assert_eq!(switch_to_curseforge.provider, "curseforge");
        assert_eq!(
            switch_to_curseforge.submitted, "",
            "the submitted-search identity is retired, not merely made \
             momentarily wrong: a later switch back must not resurrect it"
        );
        assert_eq!(
            switch_to_curseforge.request_id, "",
            "the expected request id is retired the same way, in case a later \
             search resubmits words identical enough to fill submitted back in"
        );
        assert!(
            !switch_to_curseforge.loading,
            "the newly selected catalog must be searchable immediately"
        );
        assert_eq!(
            switch_to_curseforge.requested_offset, 0,
            "the old catalog's pagination continuation does not carry over"
        );

        // What the first switch leaves the browser waiting on: nothing
        // submitted yet, checked against CurseForge, offset back at zero, no
        // request id that could still match anything.
        let after_first_switch = question(
            "client-1",
            &switch_to_curseforge.provider,
            &switch_to_curseforge.submitted,
        );
        let offset_after_first_switch =
            u32::try_from(switch_to_curseforge.requested_offset).unwrap_or(0);

        // A first-page Modrinth answer arriving at offset zero (the same
        // offset the reset now expects) still must not land.
        assert!(!search_page_belongs(
            after_first_switch,
            modrinth_search_in_flight,
            0,
            offset_after_first_switch,
            request_1_id,
            &switch_to_curseforge.request_id,
        ));
        // A Modrinth "load more" answer, mid-page, is rejected doubly: wrong
        // catalog and an offset the switch already moved past.
        assert!(!search_page_belongs(
            after_first_switch,
            modrinth_search_in_flight,
            20,
            offset_after_first_switch,
            request_1_id,
            &switch_to_curseforge.request_id,
        ));

        // A fresh CurseForge search submitted right after the first switch is
        // not stale: it matches exactly what a real submission would set, so
        // the newly selected catalog is searchable immediately.
        let curseforge_search = question("client-1", "curseforge", "jei");
        assert!(search_page_belongs(
            curseforge_search,
            curseforge_search,
            0,
            0,
            IGNORED_REQUEST_ID,
            IGNORED_REQUEST_ID,
        ));

        // The user switches back to Modrinth before the old "jei" search
        // ever returns. If the submitted identity had only been made
        // momentarily wrong instead of retired, this second switch would
        // reinstate exactly the question that old search is still waiting
        // to answer.
        let switch_back_to_modrinth = CatalogSwitchReset::for_provider("modrinth");
        assert_eq!(switch_back_to_modrinth.provider, "modrinth");
        assert_eq!(switch_back_to_modrinth.submitted, "");
        assert_eq!(switch_back_to_modrinth.request_id, "");
        let after_switch_back = question(
            "client-1",
            &switch_back_to_modrinth.provider,
            &switch_back_to_modrinth.submitted,
        );
        let offset_after_switch_back =
            u32::try_from(switch_back_to_modrinth.requested_offset).unwrap_or(0);

        // The old Modrinth answer is still rejected on the roundtrip: the
        // catalog matches again, but the submitted text and request id do
        // not, because both were retired rather than merely superseded.
        assert!(!search_page_belongs(
            after_switch_back,
            modrinth_search_in_flight,
            0,
            offset_after_switch_back,
            request_1_id,
            &switch_back_to_modrinth.request_id,
        ));

        // A fresh Modrinth "jei" search submitted after switching back is not
        // stale: it is a genuinely new question, indistinguishable in
        // content from the old one but not confused with it, and lands
        // normally.
        let fresh_modrinth_search = question("client-1", "modrinth", "jei");
        assert!(search_page_belongs(
            fresh_modrinth_search,
            fresh_modrinth_search,
            0,
            0,
            IGNORED_REQUEST_ID,
            IGNORED_REQUEST_ID,
        ));
    }

    #[test]
    fn an_answer_for_another_instance_is_discarded() {
        let waiting = question("client-1", "curseforge", "jei");
        assert!(!search_page_belongs(
            waiting,
            question("client-2", "curseforge", "jei"),
            0,
            0,
            IGNORED_REQUEST_ID,
            IGNORED_REQUEST_ID,
        ));
        // Nothing selected at all accepts nothing.
        assert!(!browser_answer_is_current(
            question("", "curseforge", "jei"),
            question("", "curseforge", "jei")
        ));
    }

    #[test]
    fn a_page_only_lands_at_the_offset_the_browser_is_expecting() {
        let waiting = question("client-1", "curseforge", "jei");
        let answer = question("client-1", "curseforge", "jei");
        // A "load more" was sent for offset ten, and this is its answer.
        assert!(search_page_belongs(
            waiting,
            answer,
            10,
            10,
            IGNORED_REQUEST_ID,
            IGNORED_REQUEST_ID,
        ));
        // A re-delivered first page does not match what is now expected, even
        // though it identifies the same question.
        assert!(!search_page_belongs(
            waiting,
            answer,
            0,
            10,
            IGNORED_REQUEST_ID,
            IGNORED_REQUEST_ID,
        ));
        // A late "load more" page for offset ten arriving after a fresh search
        // reset the expectation back to zero must not land either, regardless
        // of how many raw entries the provider had consumed to build it.
        assert!(!search_page_belongs(
            waiting,
            answer,
            10,
            0,
            IGNORED_REQUEST_ID,
            IGNORED_REQUEST_ID,
        ));
    }

    #[test]
    fn a_late_review_cannot_populate_the_project_a_user_has_moved_on_to() {
        // Project A is reviewed, then project B; A's answer arrives last and
        // must not fill in B's confirmation.
        let waiting = question("client-1", "curseforge", "238222");
        assert!(!browser_answer_is_current(
            waiting,
            question("client-1", "curseforge", "238223")
        ));
        // The same project id on the other catalog is a different project.
        assert!(!browser_answer_is_current(
            waiting,
            question("client-1", "modrinth", "238222")
        ));
        assert!(browser_answer_is_current(
            waiting,
            question("client-1", "curseforge", "238222")
        ));
    }
}

/// Whether the mod browser's final "nothing matches" state has actually been
/// proven, as opposed to merely having nothing on screen yet.
#[cfg(test)]
mod mod_browser_empty_state_tests {
    use super::*;

    /// A search that finished cleanly: not loading, ran, nothing failed.
    fn finished(visible_rows: usize, has_continuation: bool) -> bool {
        mod_browser_has_no_results(true, visible_rows, has_continuation)
    }

    #[test]
    fn zero_rows_with_a_continuation_is_not_yet_a_final_no_results() {
        // A raw page can be consumed entirely by defensive filtering while
        // later raw pages still exist; nothing on screen is not the same
        // claim as nothing matching, and Load More is still how to find out.
        assert!(!finished(0, true));
    }

    #[test]
    fn zero_rows_with_no_continuation_is_the_final_no_results() {
        assert!(finished(0, false));
    }

    #[test]
    fn rows_on_screen_are_never_reported_as_no_results() {
        assert!(!finished(1, false));
        assert!(!finished(1, true));
    }

    #[test]
    fn only_a_search_that_finished_successfully_can_reach_the_final_state() {
        assert!(!mod_browser_has_no_results(false, 0, false));
    }

    #[test]
    fn loading_never_counts_as_finished() {
        assert!(!mod_search_finished_successfully(true, true, true));
    }

    #[test]
    fn never_submitted_never_counts_as_finished() {
        assert!(!mod_search_finished_successfully(false, false, true));
    }

    #[test]
    fn a_notice_takes_priority_over_a_finished_search() {
        // A credential problem or an unreachable catalog is its own state
        // and outranks a "nothing matches" claim.
        assert!(!mod_search_finished_successfully(false, true, false));
    }

    #[test]
    fn not_loading_ran_and_no_notice_is_what_finished_means() {
        assert!(mod_search_finished_successfully(false, true, true));
    }
}
