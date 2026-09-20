//! Presentation-only models for the desktop frontend.
//!
//! These values deliberately contain no filesystem, provider, or process
//! behavior. They translate the application service records into stable UI
//! states that can be tested without constructing a Slint window.

use std::collections::{BTreeMap, VecDeque};
use std::net::IpAddr;
use std::time::{Duration, Instant};

use ephervex_core::{
    ARTIFACT_CACHE_LIMIT_PRESETS, ArtifactCacheLimit, ArtifactCacheLimitError, BlockedProviderFile,
    ByteUnit, ClientRecord, DEFAULT_ARTIFACT_CACHE_LIMIT_BYTES, DeclaredProject,
    DependencyKnowledge, DependentMods, DiagnosticKind, DiagnosticScope, DuplicateFileReason,
    DuplicateProviderFile, ImpactedMod, LaunchAndConnectAvailability, ModPresence, ModRelations,
    NetworkAccess, PackAnalysis, PackSide, ProjectAvailability, ProviderFileIdentity, RelatedMod,
    RequiredMods, RuntimeClassification, ServerRecord, ServerStatus, format_binary_bytes,
};
use ephervex_i18n::{Arg, lookup, lookup_args};
use ephervex_model::{
    DefaultModCatalog, LoaderKind, ModProvider, ModRelationKind, Operation, OperationStatus,
    StorageClass,
};
use slint::SharedString;

use crate::ui::ToastRow;

/// Orders the release list so favorites come first.
///
/// Favorites keep the order the user recorded, and everything else keeps
/// Mojang's own newest-first manifest order. A favorite the current catalog has
/// no release for is not invented: this only reorders what was already offered.
#[must_use]
pub fn releases_with_favorites_first(releases: Vec<String>, favorites: &[String]) -> Vec<String> {
    let leading: Vec<String> = favorites
        .iter()
        .filter(|favorite| releases.contains(favorite))
        .cloned()
        .collect();
    let rest = releases
        .into_iter()
        .filter(|release| !leading.contains(release));
    leading.iter().cloned().chain(rest).collect()
}

/// The favorites list after starring or unstarring one release.
///
/// A new favorite goes on the end, because the order is the user's own and a
/// new entry has no claim on a position they chose for something else. Removing
/// one leaves the rest exactly where they were.
#[must_use]
pub fn toggled_favorite(favorites: &[String], version: &str) -> Vec<String> {
    if version.is_empty() {
        return favorites.to_vec();
    }
    if favorites.iter().any(|kept| kept == version) {
        return favorites
            .iter()
            .filter(|kept| kept.as_str() != version)
            .cloned()
            .collect();
    }
    let mut toggled = favorites.to_vec();
    toggled.push(version.to_owned());
    toggled
}

/// The loader choice one Minecraft release offers, and what a selection made
/// against a different release becomes under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoaderChoices {
    /// The loaders to offer, as their own untranslated product names.
    pub options: Vec<String>,
    /// The selection to keep. The one that was made, when the release still
    /// has it; Vanilla otherwise, because every release has that.
    pub selected: String,
    /// Whether the selection had to change, which is also what invalidates the
    /// build list that belonged to it.
    pub reset: bool,
}

/// Narrows the loader choice to the loaders a Minecraft release ever had.
///
/// NeoForge's first build is for Minecraft 1.20.2, so offering it for anything
/// older leads to a build list that never fills in and a Create button that
/// never enables, with nothing on screen saying why.
#[must_use]
pub fn loader_choices(minecraft: &str, current: &str) -> LoaderChoices {
    let options: Vec<String> = ephervex_core::App::available_loaders(minecraft)
        .into_iter()
        .map(|kind| kind.to_string())
        .collect();
    let kept = options.iter().any(|option| option == current);
    LoaderChoices {
        selected: if kept {
            current.to_owned()
        } else {
            LoaderKind::Vanilla.to_string()
        },
        reset: !kept,
        options,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstancePresentation {
    pub snapshot_deletion_note: String,
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
            snapshot_deletion_note: String::new(),
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
    pub snapshot_deletion_note: String,
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
            snapshot_deletion_note: String::new(),
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
        // Bytes when the stage moves bytes, and the item count otherwise, so a
        // stage that copies files still has a bar that moves.
        let fraction = operation
            .progress
            .fraction()
            .or_else(|| operation.items.fraction());
        Self {
            id: operation.id.to_string(),
            title: operation.title.clone(),
            status: lookup(status_id),
            status_kind,
            detail: if failure.is_empty() {
                operation_detail(operation)
            } else {
                failure
            },
            #[allow(clippy::cast_possible_truncation)]
            progress: fraction.unwrap_or_default() as f32,
            indeterminate: fraction.is_none(),
            cancellable: operation.cancellable && !operation.status.is_terminal(),
            failed,
        }
    }
}

/// A count as the message layer takes one. Saturating rather than wrapping: a
/// number this large is a bug elsewhere, and showing it as a huge one is more
/// honest than showing it as negative.
fn clamped(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// The one line under an operation's title.
///
/// The phase it is in, how far through that phase it is, and what it is working
/// on, in that order, with whatever is unknown left out. An operation that
/// names none of them says nothing rather than something empty.
fn operation_detail(operation: &Operation) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(stage) = operation.stage.as_deref() {
        parts.push(lookup(stage));
    }
    if let Some(total) = operation.items.total {
        parts.push(lookup_args(
            "gui-operation-item-count",
            [
                ("current", Arg::Count(clamped(operation.items.current))),
                ("total", Arg::Count(clamped(total))),
            ],
        ));
    }
    if let Some(step) = operation.current_step.as_deref() {
        parts.push(step.to_owned());
    }
    parts.join(" | ")
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
/// instance's name - a wrong answer, not a slow one.
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

/// Whether any target of a finished linked operation needs to be looked at.
///
/// What decides whether the per-target report is put on screen. An operation
/// that reached every target it was asked to has nothing a reader has to act
/// on, and interrupting them for it costs more than it says; one that did not
/// is the case the report exists for.
///
/// Skipped is not failed: a server deliberately left out of an operation was
/// reported as such before it ran.
#[must_use]
pub fn linked_result_needs_review<'a>(tones: impl IntoIterator<Item = &'a str>) -> bool {
    tones.into_iter().any(|tone| tone == "error")
}

/// The largest page size the mod browser offers.
///
/// Larger than what one provider request may take, deliberately: the request
/// ceiling is a budget on how much a single answer costs, and a reader asking
/// for a hundred results is asking for a hundred results, not for one request.
/// The difference is filled by asking again.
pub const MAX_RESULT_PAGE_SIZE: usize = 100;

/// Reads a page size the picker spells, or `None` for anything it does not
/// offer.
///
/// Parsed rather than indexed, so the list of choices is the markup's business
/// and adding one does not mean changing two places that must agree.
#[must_use]
pub fn result_page_size(label: &str) -> Option<usize> {
    let size = label.trim().parse::<usize>().ok()?;
    (1..=MAX_RESULT_PAGE_SIZE).contains(&size).then_some(size)
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

/// One mod named by a dependency question, as a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImpactRowPresentation {
    pub name: String,
    pub logical_path: String,
    /// How far from the mod that was asked about. One is a direct dependent.
    pub distance: i32,
}

impl From<&ImpactedMod> for ImpactRowPresentation {
    fn from(entry: &ImpactedMod) -> Self {
        Self {
            name: entry.name.clone(),
            logical_path: entry.logical_path.to_string_lossy().into_owned(),
            distance: i32::try_from(entry.distance).unwrap_or(i32::MAX),
        }
    }
}

/// What a frontend shows before a mod is turned off or its jar deleted.
///
/// Pure: every list and every decision is settled here, so the markup renders
/// what it is given and decides nothing. The four groups stay apart because
/// each one says something different and needs different words - "these stop
/// working", "these lose a feature", "Ephervex has not asked about these", and
/// "Ephervex cannot ask about these".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModImpactPresentation {
    pub host_kind: String,
    pub host_id: String,
    pub path: String,
    pub name: String,
    /// Whether the jar is being deleted rather than turned off. The dependents
    /// are the same either way; only the wording differs.
    pub deleting: bool,
    pub breaks: Vec<ImpactRowPresentation>,
    pub degrades: Vec<ImpactRowPresentation>,
    pub unknown: Vec<ImpactRowPresentation>,
    pub unidentifiable: Vec<ImpactRowPresentation>,
}

impl ModImpactPresentation {
    /// Builds the view for one answered question.
    pub fn new(host_kind: &str, host_id: &str, deleting: bool, impact: &DependentMods) -> Self {
        Self {
            host_kind: host_kind.to_owned(),
            host_id: host_id.to_owned(),
            path: impact.target.logical_path.to_string_lossy().into_owned(),
            name: impact.target.name.clone(),
            deleting,
            breaks: impact.breaks.iter().map(Into::into).collect(),
            degrades: impact.degrades.iter().map(Into::into).collect(),
            unknown: impact.unknown.iter().map(Into::into).collect(),
            unidentifiable: impact.unidentifiable.iter().map(Into::into).collect(),
        }
    }

    /// Whether there is nothing here worth stopping the user for.
    #[must_use]
    pub fn is_silent(&self) -> bool {
        self.breaks.is_empty()
            && self.degrades.is_empty()
            && self.unknown.is_empty()
            && self.unidentifiable.is_empty()
    }

    /// Whether turning the named mod off takes other mods with it.
    ///
    /// The cascade offer appears only when there is something to cascade; with
    /// nothing proven, the dialog is telling the user what it does not know.
    #[must_use]
    pub fn has_cascade(&self) -> bool {
        !self.breaks.is_empty()
    }

    /// Every mod the cascade would turn off, the target last.
    ///
    /// The order is the one the batch applies: what everything needs goes
    /// last, so no interrupted prefix leaves a set that cannot load.
    #[must_use]
    pub fn cascade_paths(&self) -> Vec<String> {
        let mut paths: Vec<String> = self
            .breaks
            .iter()
            .map(|entry| entry.logical_path.clone())
            .collect();
        paths.push(self.path.clone());
        paths
    }
}

/// What a frontend shows before a mod is turned back on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModRequirementPresentation {
    pub host_kind: String,
    pub host_id: String,
    pub path: String,
    pub name: String,
    /// Installed here and turned off. Turning them on is offered.
    pub disabled: Vec<ImpactRowPresentation>,
    /// Not installed here at all. Reported and never fetched: installing one
    /// reaches a catalog, which a toggle must not do.
    pub absent: Vec<ImpactRowPresentation>,
    /// Loading now, and declared incompatible with the mod being turned on.
    /// A warning, never a refusal.
    pub incompatible: Vec<ImpactRowPresentation>,
}

impl ModRequirementPresentation {
    /// Builds the view for one answered question.
    pub fn new(host_kind: &str, host_id: &str, needs: &RequiredMods) -> Self {
        Self {
            host_kind: host_kind.to_owned(),
            host_id: host_id.to_owned(),
            path: needs.target.logical_path.to_string_lossy().into_owned(),
            name: needs.target.name.clone(),
            disabled: needs.disabled.iter().map(Into::into).collect(),
            absent: needs.absent.iter().map(Into::into).collect(),
            incompatible: needs.incompatible.iter().map(Into::into).collect(),
        }
    }

    /// Whether there is nothing here worth stopping the user for.
    #[must_use]
    pub fn is_silent(&self) -> bool {
        self.disabled.is_empty() && self.absent.is_empty() && self.incompatible.is_empty()
    }

    /// Whether turning the named mod on brings others with it.
    #[must_use]
    pub fn has_cascade(&self) -> bool {
        !self.disabled.is_empty()
    }

    /// Every mod the cascade would turn on, the target last.
    ///
    /// Reverse of a disable: what the others need goes first, so no
    /// interrupted prefix loads a mod before what it needs.
    #[must_use]
    pub fn cascade_paths(&self) -> Vec<String> {
        let mut paths: Vec<String> = self
            .disabled
            .iter()
            .map(|entry| entry.logical_path.clone())
            .collect();
        paths.push(self.path.clone());
        paths
    }
}

/// One recorded relation, as a mod's own page lists it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RelationRowPresentation {
    /// What a reader reads: the catalog's own title where one is known, and
    /// the bare project id only where none is.
    pub name: String,
    /// The exact provider project id, kept beside a resolved title as the
    /// technical identity behind it. Empty when the name is already the id.
    pub detail: String,
    /// What the catalog called the relation.
    pub kind: String,
    /// How that jar stands here, where one is here at all. Empty when it is
    /// loading, because a list of mods that are fine has nothing to say about
    /// each of them.
    pub state: String,
    /// Whether the state is one the reader should notice.
    pub attention: bool,
}

impl RelationRowPresentation {
    /// One row, given whatever the catalog has since said about the projects
    /// no jar here matches.
    fn new(related: &RelatedMod, declared: &BTreeMap<String, DeclaredProject>) -> Self {
        let (state, attention) = match related.presence {
            Some(ModPresence::Installed) => (String::new(), false),
            Some(ModPresence::Disabled) => (lookup("gui-mod-relation-disabled"), true),
            // A jar this host records and does not currently have. Restoring
            // it is the answer, and the row says so.
            Some(ModPresence::Missing) => (lookup("gui-mod-relation-absent"), true),
            // Nothing here matches the declaration at all, so what the row may
            // honestly say depends on what the catalog knows about it.
            None => (unmatched_state(related, declared), true),
        };
        let (name, detail) = declared_name(related, declared);
        Self {
            name,
            detail,
            kind: lookup(match related.kind {
                ModRelationKind::Required => "gui-mod-relation-required",
                ModRelationKind::Optional => "gui-mod-relation-optional",
                ModRelationKind::Incompatible => "gui-mod-relation-incompatible",
                ModRelationKind::Embedded => "gui-mod-relation-embedded",
            }),
            state,
            attention,
        }
    }
}

/// What to call a relation, and what identity to keep beside the name.
///
/// A project id is what the catalog stated and what everything resolves
/// against, but it is not what anybody recognizes a mod by. Where the catalog
/// has since answered with a title, the title leads and the id stays as the
/// technical detail under it. Where it has not, the id is all there is and is
/// shown once rather than twice.
fn declared_name(
    related: &RelatedMod,
    declared: &BTreeMap<String, DeclaredProject>,
) -> (String, String) {
    // A jar of this host answers for itself. It is already named by the mod
    // list, and its project id is not the question the reader has here.
    let Some(project_id) = related
        .project_id
        .as_deref()
        .filter(|_| related.logical_path.is_none())
    else {
        return (related.name.clone(), String::new());
    };
    declared
        .get(project_id)
        .and_then(|project| project.title.clone())
        .map_or_else(
            || (related.name.clone(), String::new()),
            |title| (title, project_id.to_owned()),
        )
}

/// What to say about a declaration no jar of this host matches.
///
/// "Not installed here" is reserved for the one case where installing it is
/// the answer. A project the catalog does not know, one with no release for
/// this host, and one nothing has been able to ask about are each named for
/// what they are, because a reader who acts on the wrong one of them goes
/// looking for a mod that cannot be had.
fn unmatched_state(related: &RelatedMod, declared: &BTreeMap<String, DeclaredProject>) -> String {
    let Some(project_id) = related.project_id.as_deref() else {
        // The catalog named a jar and no project, so there is nothing to look
        // up and nothing a catalog could offer.
        return lookup("gui-mod-relation-file-only");
    };
    lookup(
        match declared
            .get(project_id)
            .map(|project| &project.availability)
        {
            Some(ProjectAvailability::Installable { .. }) => "gui-mod-relation-absent",
            Some(ProjectAvailability::NoCompatibleRelease) => "gui-mod-relation-no-release",
            Some(ProjectAvailability::UnknownToCatalog) => "gui-mod-relation-unknown-project",
            Some(ProjectAvailability::NotChecked) | None => "gui-mod-relation-unchecked",
        },
    )
}

/// What one mod's own page says about its place among the others.
///
/// Direct relations in both directions, which is what makes the warning
/// dialogs checkable: a reader who wants to know why disabling a library took
/// three mods with it can open each of them and see the edge that said so.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModRelationPresentation {
    pub requires: Vec<RelationRowPresentation>,
    pub required_by: Vec<RelationRowPresentation>,
    /// What to say instead of an empty "Requires" list. Empty when the list
    /// speaks for itself.
    pub requires_note: String,
    /// Whether a refresh would turn that note into an answer.
    pub refreshable: bool,
}

impl ModRelationPresentation {
    /// Builds the view for one mod.
    ///
    /// `declared` is what the catalog answered about the projects this mod
    /// names and the host does not have, keyed by project id. An empty map is
    /// the honest state before anything has been asked, and reads as such.
    #[must_use]
    pub fn new(relations: &ModRelations, declared: &BTreeMap<String, DeclaredProject>) -> Self {
        let knowledge = relations.target.knowledge;
        Self {
            requires: relations
                .requires
                .iter()
                .map(|related| RelationRowPresentation::new(related, declared))
                .collect(),
            required_by: relations
                .required_by
                .iter()
                .map(|related| RelationRowPresentation::new(related, declared))
                .collect(),
            // Only the outgoing side depends on this mod's catalog having been
            // asked. What other mods name is recorded on their entries, and is
            // just as true when nothing has asked about this one.
            requires_note: match knowledge {
                DependencyKnowledge::Recorded if relations.requires.is_empty() => {
                    lookup("gui-mod-requires-nothing")
                }
                DependencyKnowledge::Recorded => String::new(),
                DependencyKnowledge::Unrecorded => lookup("gui-mod-requires-unrecorded"),
                DependencyKnowledge::Unidentified | DependencyKnowledge::Unidentifiable => {
                    lookup("gui-mod-requires-unanswerable")
                }
            },
            refreshable: knowledge.a_refresh_would_help(),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use ephervex_core::{DependencyKnowledge, ImpactedMod, ModOrigin, ModPresence};

    fn impacted(name: &str, distance: u32) -> ImpactedMod {
        ImpactedMod {
            logical_path: std::path::PathBuf::from(format!("mods/{name}")),
            name: name.to_owned(),
            presence: ModPresence::Installed,
            origin: ModOrigin::UserSelected,
            project_id: None,
            knowledge: DependencyKnowledge::Recorded,
            distance,
        }
    }

    fn library_impact() -> DependentMods {
        DependentMods {
            target: impacted("LibX.jar", 0),
            breaks: vec![
                impacted("SkyblockBuilder.jar", 1),
                impacted("SkyGUIs.jar", 1),
                impacted("UtilitiX.jar", 1),
            ],
            degrades: Vec::new(),
            unknown: Vec::new(),
            unidentifiable: Vec::new(),
        }
    }

    #[test]
    fn a_cascade_turns_the_named_mod_off_last() {
        // Any prefix of this list has to leave a set the loader can start, so
        // what everything needs cannot go first.
        let view = ModImpactPresentation::new("client", "an-id", false, &library_impact());
        assert!(view.has_cascade());
        assert_eq!(
            view.cascade_paths().last().map(String::as_str),
            Some("mods/LibX.jar")
        );
        assert_eq!(view.cascade_paths().len(), 4);
    }

    #[test]
    fn enabling_turns_what_it_needs_on_first() {
        let needs = RequiredMods {
            target: impacted("SkyGUIs.jar", 0),
            disabled: vec![impacted("LibX.jar", 1), impacted("SkyblockBuilder.jar", 1)],
            absent: Vec::new(),
            incompatible: Vec::new(),
        };
        let view = ModRequirementPresentation::new("client", "an-id", &needs);
        let paths = view.cascade_paths();
        assert_eq!(paths.first().map(String::as_str), Some("mods/LibX.jar"));
        assert_eq!(paths.last().map(String::as_str), Some("mods/SkyGUIs.jar"));
    }

    #[test]
    fn a_mod_nothing_depends_on_asks_nothing() {
        let view = ModImpactPresentation::new(
            "client",
            "an-id",
            false,
            &DependentMods {
                target: impacted("UtilitiX.jar", 0),
                breaks: Vec::new(),
                degrades: Vec::new(),
                unknown: Vec::new(),
                unidentifiable: Vec::new(),
            },
        );
        assert!(view.is_silent(), "nothing to put in front of the user");
        assert!(!view.has_cascade());
    }

    #[test]
    fn an_unproven_list_alone_is_still_worth_showing_and_offers_no_cascade() {
        // The dialog is telling the user what it does not know. Offering to
        // turn off mods it cannot prove anything about would be a guess.
        let view = ModImpactPresentation::new(
            "client",
            "an-id",
            false,
            &DependentMods {
                target: impacted("LibX.jar", 0),
                breaks: Vec::new(),
                degrades: Vec::new(),
                unknown: vec![impacted("Mystery.jar", 1)],
                unidentifiable: Vec::new(),
            },
        );
        assert!(!view.is_silent());
        assert!(!view.has_cascade());
    }

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

    #[test]
    fn a_page_size_is_taken_only_from_the_sizes_the_picker_offers() {
        assert_eq!(result_page_size("10"), Some(10));
        assert_eq!(result_page_size(" 100 "), Some(100));
        // Nothing outside the offered range: a fill aiming at zero, or at more
        // than the largest choice, would keep asking a provider for pages.
        assert_eq!(result_page_size("0"), None);
        assert_eq!(result_page_size("250"), None);
        assert_eq!(result_page_size("-25"), None);
        assert_eq!(result_page_size("twenty five"), None);
    }

    #[test]
    fn only_a_target_that_refused_the_operation_is_worth_interrupting_for() {
        assert!(!linked_result_needs_review(["ok", "ok"]));
        // Skipped is not failed: a server deliberately left out was reported as
        // such before the operation ran.
        assert!(!linked_result_needs_review(["ok", "muted"]));
        assert!(linked_result_needs_review(["ok", "error"]));
        assert!(!linked_result_needs_review([]));
    }

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
            allow_pack_mod_version_changes: false,
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
    fn favorites_lead_the_release_list_without_reordering_the_rest() {
        let releases = || {
            ["1.21.4", "1.21.1", "1.20.1", "1.16.3", "1.12.2"]
                .map(str::to_owned)
                .to_vec()
        };

        // The user's own order, then Mojang's for everything else.
        let ordered =
            releases_with_favorites_first(releases(), &["1.16.3".to_owned(), "1.21.1".to_owned()]);
        assert_eq!(ordered, ["1.16.3", "1.21.1", "1.21.4", "1.20.1", "1.12.2"]);

        // A favorite the catalog has no release for is not invented into the
        // list, and does not disturb the ones that are there.
        let missing =
            releases_with_favorites_first(releases(), &["1.99.9".to_owned(), "1.12.2".to_owned()]);
        assert_eq!(missing, ["1.12.2", "1.21.4", "1.21.1", "1.20.1", "1.16.3"]);

        // No favorites is the manifest's own order, unchanged.
        assert_eq!(releases_with_favorites_first(releases(), &[]), releases());
    }

    #[test]
    fn starring_appends_and_unstarring_leaves_the_rest_in_place() {
        let favorites = ["1.21.1".to_owned(), "1.16.3".to_owned()];

        let added = toggled_favorite(&favorites, "1.12.2");
        assert_eq!(added, ["1.21.1", "1.16.3", "1.12.2"]);

        // Removing the first must not disturb the order the user chose for the
        // ones that stay.
        let removed = toggled_favorite(&added, "1.21.1");
        assert_eq!(removed, ["1.16.3", "1.12.2"]);

        // Nothing is named, so nothing changes.
        assert_eq!(toggled_favorite(&favorites, ""), favorites);
    }

    #[test]
    fn the_loader_choice_follows_the_release_it_is_made_against() {
        let modern = loader_choices("1.21.1", "NeoForge");
        assert_eq!(modern.options, ["Vanilla", "Forge", "NeoForge"]);
        assert_eq!(modern.selected, "NeoForge");
        assert!(!modern.reset);

        // 1.20.1 is the last release before NeoForge's first build, so it is
        // the edge the rule has to get right.
        let legacy = loader_choices("1.20.1", "NeoForge");
        assert_eq!(legacy.options, ["Vanilla", "Forge"]);
        assert_eq!(legacy.selected, "Vanilla");
        assert!(legacy.reset);

        // Forge exists on both sides of that edge and is left alone.
        let forge = loader_choices("1.12.2", "Forge");
        assert_eq!(forge.selected, "Forge");
        assert!(!forge.reset);
    }

    #[test]
    fn instance_runtime_state_is_independent_from_launch_action_availability() {
        let record = client(StorageClass::Temporary, LoaderSelection::vanilla());
        let preparing = ephervex_core::LaunchState {
            owner_pid: std::process::id(),
            owner_start_ticks: None,
            game_pid: None,
            game_start_ticks: None,
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

    fn related(name: &str, kind: ModRelationKind, presence: Option<ModPresence>) -> RelatedMod {
        RelatedMod {
            kind,
            name: name.to_owned(),
            logical_path: presence.map(|_| std::path::PathBuf::from(format!("mods/{name}"))),
            presence,
            project_id: None,
        }
    }

    /// A declaration naming a project this host has no jar for.
    fn declares_project(project_id: &str) -> RelatedMod {
        RelatedMod {
            kind: ModRelationKind::Required,
            name: project_id.to_owned(),
            logical_path: None,
            presence: None,
            project_id: Some(project_id.to_owned()),
        }
    }

    fn answered(
        project_id: &str,
        availability: ProjectAvailability,
    ) -> BTreeMap<String, DeclaredProject> {
        BTreeMap::from([(
            project_id.to_owned(),
            DeclaredProject {
                title: Some("Absent Library".to_owned()),
                availability,
            },
        )])
    }

    fn relations_requiring(requires: Vec<RelatedMod>) -> ephervex_core::ModRelations {
        ephervex_core::ModRelations {
            target: impacted("MouseTweaks.jar", 0),
            requires,
            required_by: Vec::new(),
        }
    }

    #[test]
    fn a_relation_says_what_it_was_and_how_that_jar_stands() {
        let view = ModRelationPresentation::new(
            &ephervex_core::ModRelations {
                target: impacted("SkyblockBuilder.jar", 0),
                requires: vec![
                    related(
                        "LibX.jar",
                        ModRelationKind::Required,
                        Some(ModPresence::Installed),
                    ),
                    related(
                        "SkyGUIs.jar",
                        ModRelationKind::Optional,
                        Some(ModPresence::Disabled),
                    ),
                    related("AABBCCDD", ModRelationKind::Required, None),
                ],
                required_by: Vec::new(),
            },
            &BTreeMap::new(),
        );

        assert_eq!(view.requires[0].kind, lookup("gui-mod-relation-required"));
        assert!(
            view.requires[0].state.is_empty() && !view.requires[0].attention,
            "a mod that is simply loading has nothing to report about itself"
        );
        assert_eq!(view.requires[1].kind, lookup("gui-mod-relation-optional"));
        assert_eq!(view.requires[1].state, lookup("gui-mod-relation-disabled"));
        assert_eq!(
            view.requires[2].state,
            lookup("gui-mod-relation-file-only"),
            "a relation that named a jar and no project has nothing to install"
        );
        assert!(view.requires_note.is_empty(), "the list speaks for itself");
    }

    #[test]
    fn a_declared_project_says_which_kind_of_missing_it_is() {
        let cases = [
            (
                ProjectAvailability::Installable {
                    version_id: "abc".to_owned(),
                },
                "gui-mod-relation-absent",
            ),
            (
                ProjectAvailability::NoCompatibleRelease,
                "gui-mod-relation-no-release",
            ),
            (
                ProjectAvailability::UnknownToCatalog,
                "gui-mod-relation-unknown-project",
            ),
            (
                ProjectAvailability::NotChecked,
                "gui-mod-relation-unchecked",
            ),
        ];
        for (availability, expected) in cases {
            let view = ModRelationPresentation::new(
                &relations_requiring(vec![declares_project("200200")]),
                &answered("200200", availability.clone()),
            );
            assert_eq!(
                view.requires[0].state,
                lookup(expected),
                "{availability:?} has to read as itself"
            );
            assert!(
                view.requires[0].attention,
                "every declaration without a jar here is worth noticing"
            );
        }
    }

    #[test]
    fn a_declaration_nothing_has_answered_for_is_not_called_installable() {
        let view = ModRelationPresentation::new(
            &relations_requiring(vec![declares_project("200200")]),
            &BTreeMap::new(),
        );
        assert_eq!(
            view.requires[0].state,
            lookup("gui-mod-relation-unchecked"),
            "before a catalog answers, the row must not promise the project is there to install"
        );
        assert_ne!(view.requires[0].state, lookup("gui-mod-relation-absent"));
    }

    #[test]
    fn a_resolved_declaration_is_named_by_its_title_and_keeps_its_id() {
        let view = ModRelationPresentation::new(
            &relations_requiring(vec![declares_project("200200")]),
            &answered("200200", ProjectAvailability::NoCompatibleRelease),
        );
        assert_eq!(view.requires[0].name, "Absent Library");
        assert_eq!(
            view.requires[0].detail, "200200",
            "the exact id the catalog stated stays beside the name it resolved to"
        );

        // Nothing answered, so the id is the whole of what is known and is
        // shown once rather than as its own technical detail.
        let unresolved = ModRelationPresentation::new(
            &relations_requiring(vec![declares_project("200200")]),
            &BTreeMap::new(),
        );
        assert_eq!(unresolved.requires[0].name, "200200");
        assert!(unresolved.requires[0].detail.is_empty());
    }

    #[test]
    fn a_jar_this_host_already_has_is_named_by_the_list_and_carries_no_id() {
        let view = ModRelationPresentation::new(
            &relations_requiring(vec![related(
                "LibX.jar",
                ModRelationKind::Required,
                Some(ModPresence::Installed),
            )]),
            &BTreeMap::new(),
        );
        assert_eq!(view.requires[0].name, "LibX.jar");
        assert!(
            view.requires[0].detail.is_empty(),
            "a jar that is here is not a project the reader has to look up"
        );
    }

    #[test]
    fn a_recorded_jar_this_host_has_lost_is_still_simply_missing() {
        let view = ModRelationPresentation::new(
            &relations_requiring(vec![related(
                "LibX.jar",
                ModRelationKind::Required,
                Some(ModPresence::Missing),
            )]),
            &BTreeMap::new(),
        );
        assert_eq!(
            view.requires[0].state,
            lookup("gui-mod-relation-absent"),
            "this host records the jar and does not have it, which is exactly what restoring fixes"
        );
    }

    #[test]
    fn an_empty_requires_list_says_which_kind_of_empty_it_is() {
        let asked = ModRelationPresentation::new(
            &ephervex_core::ModRelations {
                target: impacted("LibX.jar", 0),
                requires: Vec::new(),
                required_by: Vec::new(),
            },
            &BTreeMap::new(),
        );
        assert_eq!(asked.requires_note, lookup("gui-mod-requires-nothing"));
        assert!(!asked.refreshable);

        let mut target = impacted("LibX.jar", 0);
        target.knowledge = DependencyKnowledge::Unrecorded;
        let unasked = ModRelationPresentation::new(
            &ephervex_core::ModRelations {
                target,
                requires: Vec::new(),
                required_by: Vec::new(),
            },
            &BTreeMap::new(),
        );
        assert_eq!(unasked.requires_note, lookup("gui-mod-requires-unrecorded"));
        assert!(
            unasked.refreshable,
            "a mod nothing has asked about is one the user can do something about"
        );

        let mut target = impacted("Mystery.jar", 0);
        target.knowledge = DependencyKnowledge::Unidentifiable;
        let unanswerable = ModRelationPresentation::new(
            &ephervex_core::ModRelations {
                target,
                requires: Vec::new(),
                required_by: Vec::new(),
            },
            &BTreeMap::new(),
        );
        assert_eq!(
            unanswerable.requires_note,
            lookup("gui-mod-requires-unanswerable")
        );
        assert!(!unanswerable.refreshable, "no catalog can answer for it");
    }
}

/// One entry in the artifact-cache limit selector.
///
/// The list is the mapping. A selector reports the index it is on, and this is
/// what that index means, so nothing has to parse a label - and in particular
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

/// Two files a manifest declared for the same project, translated from
/// [`ephervex_core::DuplicateProviderFile`] for the import modal's
/// informational duplicate list.
///
/// Purely informational: the file that fits was already kept automatically,
/// and this exists only to say so - and why - before the user commits to
/// importing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateFilePresentation {
    /// The project's own name, falling back to its catalog id.
    pub project_line: String,
    /// The file that was kept and will be installed.
    pub kept_line: String,
    /// The file that was discarded in its favor.
    pub discarded_line: String,
    /// Whether the kept file won because it declares the pack's own
    /// Minecraft version, as opposed to being the more recently published
    /// candidate when neither declared it (or both did).
    pub matched_minecraft_version: bool,
}

impl DuplicateFilePresentation {
    #[must_use]
    fn from_duplicate(duplicate: &DuplicateProviderFile) -> Self {
        let project_id = duplicate.kept.project_id.as_deref().unwrap_or_default();
        let kept_id = duplicate.kept.file_id.as_deref().unwrap_or_default();
        let discarded_id = duplicate.discarded.file_id.as_deref().unwrap_or_default();
        Self {
            project_line: duplicate
                .project_name
                .clone()
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| project_id.to_owned()),
            kept_line: duplicate
                .kept_file_name
                .clone()
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| kept_id.to_owned()),
            discarded_line: duplicate
                .discarded_file_name
                .clone()
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| discarded_id.to_owned()),
            matched_minecraft_version: duplicate.reason
                == DuplicateFileReason::MatchesMinecraftVersion,
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
    /// Projects the manifest declared twice under different file ids, and
    /// which file of each pair was kept (ADR 0031). Purely informational -
    /// import is never blocked by this - shown so the choice is visible
    /// before the user commits, not just recorded afterward.
    pub duplicate_files: Vec<DuplicateFilePresentation>,
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
            duplicate_files: analysis
                .duplicate_files
                .iter()
                .map(DuplicateFilePresentation::from_duplicate)
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
            duplicate_files: Vec::new(),
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

/// How many notifications are on screen at once.
const VISIBLE_TOASTS: usize = 3;
/// How many wait behind them before the oldest is dropped.
const MAX_WAITING_TOASTS: usize = 12;
/// How long an ordinary notification stays, and how long one reporting a
/// failure stays. A failure is worth more of the reader's attention and is the
/// one they are most likely to have looked away from.
const NORMAL_DWELL: Duration = Duration::from_millis(4_500);
const ERROR_DWELL: Duration = Duration::from_millis(9_000);

/// One notification on screen or waiting to be.
#[derive(Debug, Clone)]
struct ToastGroup {
    id: i32,
    message: SharedString,
    error: bool,
    /// How many identical notifications this one stands for, counting itself.
    repeats: i32,
    /// When this one stops being shown. Meaningless until it is.
    expires_at: Instant,
}

impl ToastGroup {
    /// Whether another notification is the same one to a reader.
    ///
    /// The sentence and its severity are all a notification carries, so they
    /// are all identity can be. Two unrelated failures that print the same
    /// sentence are the same notification to somebody reading it, which is the
    /// only point of view this collapse serves.
    fn same_as(&self, message: &SharedString, error: bool) -> bool {
        self.error == error && self.message == *message
    }
}

/// The notifications the window is showing, and the ones waiting behind them.
///
/// Held here rather than in the markup because the waiting list is here: a
/// notification that arrives while every panel is full has to be recognized
/// as one already waiting, and the markup cannot see that list.
#[derive(Debug)]
pub struct ToastStack {
    next_id: i32,
    visible: Vec<ToastGroup>,
    waiting: VecDeque<ToastGroup>,
}

impl Default for ToastStack {
    fn default() -> Self {
        Self {
            next_id: 1,
            visible: Vec::with_capacity(VISIBLE_TOASTS),
            waiting: VecDeque::new(),
        }
    }
}

impl ToastStack {
    /// Adds one notification, collapsing it into an identical one that is
    /// already here, showing it if there is room, and queueing it if not.
    ///
    /// A repeat restarts the dwell of the panel it joined: the count on it just
    /// changed, and a count nobody has time to read is worse than not
    /// collapsing at all.
    pub fn present(&mut self, message: SharedString, error: bool, now: Instant) {
        if let Some(group) = self
            .visible
            .iter_mut()
            .find(|group| group.same_as(&message, error))
        {
            group.repeats += 1;
            group.expires_at = now + dwell(error);
            return;
        }
        // A repeat of one that is still waiting has no dwell to restart; it
        // takes one when it is shown.
        if let Some(group) = self
            .waiting
            .iter_mut()
            .find(|group| group.same_as(&message, error))
        {
            group.repeats += 1;
            return;
        }
        if self.visible.len() < VISIBLE_TOASTS {
            let group = self.group(message, error, now);
            self.visible.push(group);
            return;
        }
        if self.waiting.len() == MAX_WAITING_TOASTS {
            self.waiting.pop_front();
        }
        let group = self.group(message, error, now);
        self.waiting.push_back(group);
    }

    /// Takes one notification off the screen, by the id it was given.
    ///
    /// Every identical one still waiting goes with it. Dismissing a panel that
    /// says a thing happened three times has to mean that thing, not the third
    /// of three copies.
    pub fn dismiss(&mut self, id: i32, now: Instant) {
        let Some(at) = self.visible.iter().position(|group| group.id == id) else {
            return;
        };
        let dropped = self.visible.remove(at);
        self.waiting
            .retain(|group| !group.same_as(&dropped.message, dropped.error));
        self.promote(now);
    }

    /// Takes off whatever has been shown for long enough.
    pub fn expire(&mut self, now: Instant) {
        self.visible.retain(|group| group.expires_at > now);
        self.promote(now);
    }

    /// What the window draws, oldest first.
    #[must_use]
    pub fn rows(&self) -> Vec<ToastRow> {
        self.visible
            .iter()
            .map(|group| ToastRow {
                id: group.id,
                message: group.message.clone(),
                error: group.error,
                repeats: group.repeats,
            })
            .collect()
    }

    /// Whether anything is on screen or waiting to be.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.visible.is_empty() && self.waiting.is_empty()
    }

    fn group(&mut self, message: SharedString, error: bool, now: Instant) -> ToastGroup {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        ToastGroup {
            id,
            message,
            error,
            repeats: 1,
            expires_at: now + dwell(error),
        }
    }

    /// Fills whatever room there is from the front of the waiting list, each
    /// one starting its time on screen now rather than when it arrived.
    fn promote(&mut self, now: Instant) {
        while self.visible.len() < VISIBLE_TOASTS {
            let Some(mut group) = self.waiting.pop_front() else {
                break;
            };
            group.expires_at = now + dwell(group.error);
            self.visible.push(group);
        }
    }
}

const fn dwell(error: bool) -> Duration {
    if error { ERROR_DWELL } else { NORMAL_DWELL }
}

#[cfg(test)]
mod toast_tests {
    #![allow(clippy::expect_used, clippy::unchecked_time_subtraction)]
    use super::*;

    /// A moment before a deadline, and a moment after one.
    const JUST_SHORT: Duration = Duration::from_millis(1);

    #[test]
    fn three_notifications_are_shown_and_the_rest_wait_their_turn() {
        let mut stack = ToastStack::default();
        let start = Instant::now();
        for index in 0..5 {
            stack.present(format!("failure {index}").into(), true, start);
        }

        assert_eq!(shown(&stack), ["failure 0", "failure 1", "failure 2"]);

        stack.dismiss(stack.rows()[0].id, start);
        assert_eq!(
            shown(&stack),
            ["failure 1", "failure 2", "failure 3"],
            "one leaving lets the next in"
        );
    }

    #[test]
    fn a_notification_leaves_once_it_has_been_shown_for_long_enough() {
        let mut stack = ToastStack::default();
        let start = Instant::now();
        stack.present("done".into(), false, start);
        stack.present("failed".into(), true, start);

        stack.expire(start + NORMAL_DWELL - JUST_SHORT);
        assert_eq!(shown(&stack), ["done", "failed"]);

        stack.expire(start + NORMAL_DWELL + Duration::from_millis(1));
        assert_eq!(
            shown(&stack),
            ["failed"],
            "a failure is worth longer than an ordinary result"
        );

        stack.expire(start + ERROR_DWELL + Duration::from_millis(1));
        assert!(stack.is_empty());
    }

    #[test]
    fn a_promoted_notification_starts_its_time_on_screen_when_it_is_shown() {
        let mut stack = ToastStack::default();
        let start = Instant::now();
        for index in 0..4 {
            stack.present(format!("done {index}").into(), false, start);
        }

        let late = start + NORMAL_DWELL + Duration::from_millis(1);
        stack.expire(late);

        assert_eq!(shown(&stack), ["done 3"]);
        stack.expire(late + NORMAL_DWELL - JUST_SHORT);
        assert_eq!(
            shown(&stack),
            ["done 3"],
            "the one that waited is not cut short by the time it spent waiting"
        );
    }

    #[test]
    fn the_oldest_waiting_notification_is_dropped_once_too_many_have_piled_up() {
        let mut stack = ToastStack::default();
        let start = Instant::now();
        let total = VISIBLE_TOASTS + MAX_WAITING_TOASTS + 1;
        for index in 0..total {
            stack.present(format!("done {index}").into(), false, start);
        }

        let mut drained = Vec::new();
        while !stack.is_empty() {
            drained.extend(shown(&stack));
            for row in stack.rows() {
                stack.dismiss(row.id, start);
            }
        }

        assert_eq!(
            drained.len(),
            total - 1,
            "one was dropped rather than growing the list without a bound"
        );
        assert!(
            !drained.contains(&"done 3".to_owned()),
            "and it is the one that had waited longest: {drained:?}"
        );
    }

    #[test]
    fn dismissing_names_the_notification_rather_than_its_place_in_the_stack() {
        let mut stack = ToastStack::default();
        let start = Instant::now();
        stack.present("first".into(), false, start);
        stack.present("second".into(), false, start);
        let second = stack.rows()[1].id;

        stack.dismiss(second, start);

        assert_eq!(shown(&stack), ["first"]);
    }

    #[test]
    fn an_identical_notification_bumps_the_count_instead_of_opening_a_second_panel() {
        let mut stack = ToastStack::default();
        let start = Instant::now();
        for _ in 0..3 {
            stack.present("nothing was bound".into(), true, start);
        }

        assert_eq!(shown(&stack), ["nothing was bound"]);
        assert_eq!(stack.rows()[0].repeats, 3);
    }

    #[test]
    fn a_repeat_restarts_the_dwell_so_the_new_count_can_be_read() {
        let mut stack = ToastStack::default();
        let start = Instant::now();
        stack.present("done".into(), false, start);

        let late = start + NORMAL_DWELL - JUST_SHORT;
        stack.present("done".into(), false, late);
        stack.expire(start + NORMAL_DWELL + JUST_SHORT);

        assert_eq!(shown(&stack), ["done"], "the panel just changed");
        stack.expire(late + NORMAL_DWELL + JUST_SHORT);
        assert!(stack.is_empty());
    }

    #[test]
    fn two_notifications_that_differ_only_in_severity_are_not_the_same_one() {
        let mut stack = ToastStack::default();
        let start = Instant::now();
        stack.present("the same words".into(), false, start);
        stack.present("the same words".into(), true, start);

        assert_eq!(shown(&stack), ["the same words", "the same words"]);
        assert_eq!(stack.rows().len(), 2);
    }

    #[test]
    fn a_repeat_that_arrives_with_every_panel_full_bumps_the_copy_that_is_waiting() {
        let mut stack = ToastStack::default();
        let start = Instant::now();
        for index in 0..VISIBLE_TOASTS {
            stack.present(format!("done {index}").into(), false, start);
        }
        stack.present("waiting".into(), false, start);
        stack.present("waiting".into(), false, start);

        for row in stack.rows() {
            stack.dismiss(row.id, start);
        }

        assert_eq!(shown(&stack), ["waiting"]);
        assert_eq!(
            stack.rows()[0].repeats,
            2,
            "a repeat is collapsed before it is ever shown, not after"
        );
    }

    #[test]
    fn dismissing_a_collapsed_notification_drops_the_copies_still_waiting() {
        let mut stack = ToastStack::default();
        let start = Instant::now();
        stack.present("nothing was bound".into(), true, start);
        for index in 0..VISIBLE_TOASTS {
            stack.present(format!("something else {index}").into(), true, start);
        }
        stack.present("nothing was bound".into(), true, start);

        stack.dismiss(stack.rows()[0].id, start);

        assert!(
            !shown(&stack).contains(&"nothing was bound".to_owned()),
            "pressing the X once has to be the end of it: {:?}",
            shown(&stack)
        );
    }

    /// What the window would draw, in order.
    fn shown(stack: &ToastStack) -> Vec<String> {
        stack
            .rows()
            .into_iter()
            .map(|row| row.message.to_string())
            .collect()
    }
}

/// What the "About & Licenses" screen shows for the software Ephervex is built
/// on.
///
/// Built from the inventory compiled into this build, which is generated from
/// the real dependency graph. Nothing here decides what a dependency is
/// licensed under; it decides how to show what was generated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThirdPartyLicenses {
    /// One line naming how many packages and how many distinct license texts.
    pub summary: String,
    /// Every dependency, in the order the inventory records them.
    pub packages: Vec<ThirdPartyPackage>,
    /// Set only when this build's own list could not be read, which means a
    /// damaged build rather than a missing report.
    pub error: Option<String>,
}

/// One dependency, as a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThirdPartyPackage {
    pub name: String,
    pub version: String,
    /// The crate's own SPDX expression, shown exactly as its manifest declares
    /// it.
    pub license: String,
    /// The full text or texts this package is covered by, already joined for
    /// display. More than one is ordinary: a package offered under two
    /// licenses ships both.
    pub text: String,
}

impl ThirdPartyLicenses {
    /// Reads the inventory this build carries.
    #[must_use]
    pub fn bundled() -> Self {
        match ephervex_core::App::third_party_licenses() {
            Ok(inventory) => Self::from_inventory(inventory),
            // The technical reason belongs in the log, not on a screen: there
            // is nothing a user can do with a parse offset, and the one thing
            // they can do is reinstall.
            Err(error) => {
                tracing::error!(%error, "the bundled third-party license inventory is unusable");
                Self {
                    summary: String::new(),
                    packages: Vec::new(),
                    error: Some(lookup("gui-third-party-broken")),
                }
            }
        }
    }

    fn from_inventory(inventory: &ephervex_core::LicenseInventory) -> Self {
        let packages = inventory
            .dependencies()
            .iter()
            .map(|dependency| ThirdPartyPackage {
                name: dependency.name.clone(),
                version: dependency.version.clone(),
                license: dependency.license.clone(),
                text: inventory
                    .texts_for(dependency)
                    .into_iter()
                    .map(|text| text.text.trim_end().to_owned())
                    .collect::<Vec<_>>()
                    .join("\n\n"),
            })
            .collect::<Vec<_>>();
        Self {
            summary: lookup_args(
                "gui-third-party-summary",
                [
                    ("crates", Arg::from(packages.len())),
                    ("licenses", Arg::from(inventory.licenses().len())),
                ],
            ),
            packages,
            error: None,
        }
    }
}
