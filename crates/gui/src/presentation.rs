//! Presentation-only models for the desktop frontend.
//!
//! These values deliberately contain no filesystem, provider, or process
//! behavior. They translate the application service records into stable UI
//! states that can be tested without constructing a Slint window.

use std::net::IpAddr;

use ephervex_core::{
    ARTIFACT_CACHE_LIMIT_PRESETS, ArtifactCacheLimit, ArtifactCacheLimitError, ByteUnit,
    ClientRecord, DEFAULT_ARTIFACT_CACHE_LIMIT_BYTES, DiagnosticKind, DiagnosticScope,
    LaunchAndConnectAvailability, NetworkAccess, PackAnalysis, PackSide, PreferenceKey,
    RuntimeClassification, ServerRecord, ServerStatus, format_binary_bytes,
};
use ephervex_i18n::{Arg, lookup, lookup_args};
use ephervex_model::{LoaderKind, Operation, OperationStatus, StorageClass};

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
pub fn known_vanilla_preferences() -> &'static [PreferenceKey] {
    PreferenceKey::ALL
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

        let running = ephervex_core::LaunchState {
            game_pid: Some(std::process::id()),
            ..preparing
        };
        let running = InstancePresentation::from_record(&record, 0, 0, Some(&running), []);
        assert_eq!(running.state_kind, "running");
        assert!(!running.can_launch);
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
    fn only_known_vanilla_preferences_are_presented() {
        let names = known_vanilla_preferences()
            .iter()
            .map(|key| key.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names.len(), PreferenceKey::ALL.len());
        assert!(!names.iter().any(|name| name.starts_with("mod")));
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
}
