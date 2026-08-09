//! Presentation-only models for the desktop frontend.
//!
//! These values deliberately contain no filesystem, provider, or process
//! behavior. They translate the application service records into stable UI
//! states that can be tested without constructing a Slint window.

use std::net::IpAddr;

use ephervex_core::{
    ClientRecord, DiagnosticKind, DiagnosticScope, LaunchAndConnectAvailability, NetworkAccess,
    PreferenceKey, RuntimeClassification, ServerRecord, ServerStatus,
};
use ephervex_model::{LoaderKind, Operation, OperationStatus, StorageClass};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstancePresentation {
    pub id: String,
    pub name: String,
    pub minecraft: String,
    pub loader: String,
    pub loader_kind: &'static str,
    pub storage: &'static str,
    pub source: String,
    pub path: String,
    pub linked_servers: usize,
    pub managed_mods: usize,
    pub needs_attention: bool,
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
        launch_running: bool,
        connections: impl IntoIterator<
            Item = (ServerRecord, Result<LaunchAndConnectAvailability, String>),
        >,
    ) -> Self {
        Self {
            id: record.id.to_string(),
            name: record.name.clone(),
            minecraft: record.minecraft_version.clone(),
            loader: record.loader.to_string(),
            loader_kind: loader_kind_name(record.loader.kind),
            storage: storage_name(record.storage),
            source: record.source.kind.to_string(),
            path: record.root.display().to_string(),
            linked_servers,
            managed_mods,
            needs_attention: record.needs_attention(),
            can_launch: !launch_running && !record.needs_attention(),
            launch_and_connect: connections
                .into_iter()
                .map(|(server, availability)| match availability {
                    Ok(availability) => ConnectPresentation {
                        server_id: server.id.to_string(),
                        server_name: server.name,
                        target: availability.target.to_string(),
                        enabled: !availability.client_running,
                        reason: if availability.client_running {
                            "The client is already running.".to_owned()
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
    pub storage: &'static str,
    pub relationship: String,
    pub linked_client_id: String,
    pub state: &'static str,
    pub state_detail: &'static str,
    pub address: String,
    pub path: String,
    pub installed: bool,
    pub needs_attention: bool,
    pub can_start: bool,
    pub can_stop: bool,
    pub can_restart: bool,
    pub can_kill: bool,
    pub can_send_command: bool,
    pub destructive_recovery: bool,
}

impl ServerPresentation {
    #[must_use]
    pub fn from_status(status: &ServerStatus) -> Self {
        let record = &status.record;
        let controlled = status.runtime.is_controlled();
        let (state, detail, uncertain, needs_recovery) = match &status.runtime {
            RuntimeClassification::Stopped => ("Stopped", "Ready to start", false, false),
            RuntimeClassification::Controlled { .. } => {
                ("Running", "Controlled by Ephervex", false, false)
            }
            RuntimeClassification::UnknownRunning { .. } => {
                ("Unknown", "A process may still be running", true, false)
            }
            RuntimeClassification::Orphaned { .. } => (
                "Orphaned",
                "Java is running without its supervisor",
                true,
                false,
            ),
            RuntimeClassification::ProvenStale { .. } => (
                "Needs recovery",
                "Recorded processes have exited",
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
            relationship: record
                .linked_client_id
                .map_or_else(|| "Standalone".to_owned(), |_| "Linked client".to_owned()),
            linked_client_id: record
                .linked_client_id
                .map_or_else(String::new, |id| id.to_string()),
            state,
            state_detail: detail,
            address: format_connection_target(record),
            path: record.root.display().to_string(),
            installed,
            needs_attention: record.needs_attention(),
            can_start: installed && !status.runtime.may_be_running() && !needs_recovery,
            can_stop: controlled,
            can_restart: controlled,
            can_kill: controlled,
            can_send_command: controlled,
            destructive_recovery: uncertain,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActivityPresentation {
    pub id: String,
    pub title: String,
    pub status: &'static str,
    pub detail: String,
    pub progress: f32,
    pub indeterminate: bool,
    pub cancellable: bool,
    pub failed: bool,
}

impl From<&Operation> for ActivityPresentation {
    fn from(operation: &Operation) -> Self {
        let (status, failed, failure) = match &operation.status {
            OperationStatus::Pending => ("Queued", false, String::new()),
            OperationStatus::Running => ("Running", false, String::new()),
            OperationStatus::Canceled => ("Canceled", false, String::new()),
            OperationStatus::Completed => ("Completed", false, String::new()),
            OperationStatus::Failed(message) => ("Failed", true, message.clone()),
        };
        let fraction = operation.progress.fraction();
        Self {
            id: operation.id.to_string(),
            title: operation.title.clone(),
            status,
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
    pub label: &'static str,
    pub control: PropertyControl,
}

pub const SERVER_PROPERTY_DEFINITIONS: &[PropertyDefinition] = &[
    PropertyDefinition {
        key: "motd",
        label: "Message of the day",
        control: PropertyControl::Text,
    },
    PropertyDefinition {
        key: "max-players",
        label: "Maximum players",
        control: PropertyControl::Integer {
            minimum: 1,
            maximum: u16::MAX,
        },
    },
    PropertyDefinition {
        key: "difficulty",
        label: "Difficulty",
        control: PropertyControl::Choice(&["peaceful", "easy", "normal", "hard"]),
    },
    PropertyDefinition {
        key: "gamemode",
        label: "Game mode",
        control: PropertyControl::Choice(&["survival", "creative", "adventure", "spectator"]),
    },
    PropertyDefinition {
        key: "allow-flight",
        label: "Allow flight",
        control: PropertyControl::Toggle,
    },
    PropertyDefinition {
        key: "pvp",
        label: "Player versus player",
        control: PropertyControl::Toggle,
    },
    PropertyDefinition {
        key: "white-list",
        label: "Use allowlist",
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
        return invalid("Enter a port from 1 to 65535.");
    }
    if !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return invalid("Ports contain digits only.");
    }
    match value.parse::<u32>() {
        Ok(1..=65_535) => valid(),
        _ => invalid("Port must be from 1 to 65535."),
    }
}

#[must_use]
pub fn validate_ip(value: &str, required: bool) -> ValidationResult {
    let value = value.trim();
    if value.is_empty() {
        return if required {
            invalid("Enter an IPv4 or IPv6 address.")
        } else {
            valid()
        };
    }
    if value.parse::<IpAddr>().is_ok() {
        valid()
    } else {
        invalid("Enter a valid IPv4 or IPv6 address.")
    }
}

#[must_use]
pub fn known_vanilla_preferences() -> &'static [PreferenceKey] {
    PreferenceKey::ALL
}

#[must_use]
pub const fn diagnostic_scope_label(scope: DiagnosticScope) -> &'static str {
    match scope {
        DiagnosticScope::Client(_) => "Client",
        DiagnosticScope::Server(_) => "Server",
        DiagnosticScope::Launcher => "Launcher",
    }
}

#[must_use]
pub const fn diagnostic_kind_label(kind: DiagnosticKind) -> &'static str {
    match kind {
        DiagnosticKind::Log => "Log",
        DiagnosticKind::CrashReport => "Crash report",
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
pub const fn storage_name(storage: StorageClass) -> &'static str {
    match storage {
        StorageClass::Temporary => "Temporary",
        StorageClass::Persistent => "Persistent",
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
        _ => return Err(invalid("Choose a network access mode.")),
    };
    let address_validation = validate_ip(address, matches!(access, NetworkAccess::Custom));
    if !address_validation.valid {
        return Err(address_validation);
    }
    let port = port
        .parse::<u16>()
        .map_err(|_| invalid("Port must be from 1 to 65535."))?;
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
            false,
            [],
        );
        assert_eq!(temporary.storage, "Temporary");
        assert_eq!(temporary.loader, "Vanilla");
        assert_eq!(
            [LoaderKind::Vanilla, LoaderKind::Forge, LoaderKind::NeoForge].map(loader_kind_name),
            ["vanilla", "forge", "neoforge"]
        );
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
            InstancePresentation::from_record(&record, 1, 0, false, [(linked, Ok(availability))]);
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
}
