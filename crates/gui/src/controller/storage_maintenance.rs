//! The maintenance section of the disk usage view: inspecting the shared
//! stores, checking and recovering them, and reviewing a cleanup before it is
//! confirmed.
//!
//! Every answer replaces what the view showed before it, and anything that
//! changes the disk marks the earlier answers out of date rather than leaving
//! them on screen as if they still described it. The view never starts a
//! cleanup: it shows the plan core made, and only the confirmation in that
//! review applies it.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::Sender;

use ephervex_core::{
    CleanupOutcome, CleanupPlan, FaultStanding, IntegrityReport, KeptReason, ManagedStorageGroup,
    ManagedStorageKind, RecoveryReport, StorageFault, StorageFaultKind, StorageInspection,
    UncheckedStorage, WithheldReason,
};
use ephervex_i18n::{Arg, lookup, lookup_args};
use ephervex_model::format_binary_bytes;
use slint::{ComponentHandle as _, SharedString};

use super::{DiskUsageState, model, send, string_model};
use crate::backend::{ActivityData, Command, StorageMaintenanceEvent};
use crate::ui::{AppWindow, StorageGroupRow};

/// How many damaged files the view names. The count above them is exact.
const LISTED_FAULTS: usize = 50;

/// The maintenance step that is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Inspect,
    Verify,
    Recover,
    Plan,
    Clean,
}

impl Step {
    const fn id(self) -> &'static str {
        match self {
            Self::Inspect => "inspect",
            Self::Verify => "verify",
            Self::Recover => "recover",
            Self::Plan => "plan",
            Self::Clean => "clean",
        }
    }

    const fn title(self) -> &'static str {
        match self {
            Self::Inspect => "operation-inspecting-storage",
            Self::Verify => "operation-verifying-storage",
            Self::Recover => "operation-recovering-storage",
            Self::Plan => "operation-planning-storage-cleanup",
            Self::Clean => "operation-cleaning-storage",
        }
    }
}

/// What the maintenance section is doing and what it knows.
#[derive(Debug, Default)]
pub(super) struct MaintenanceState {
    /// The step in progress. A second request while one runs is ignored: the
    /// buttons are disabled, and this is what holds the same line for a
    /// request that arrives anyway.
    running: Option<Step>,
    /// The running step's operation, once it reports, which is what Stop
    /// cancels.
    operation: Option<String>,
    /// Whether the last check found damage an installed version needs.
    can_recover: bool,
}

fn size(bytes: u64) -> String {
    format_binary_bytes(bytes)
}

fn count(id: &str, count: u64) -> String {
    lookup_args(
        id,
        [("count", Arg::from(i64::try_from(count).unwrap_or(i64::MAX)))],
    )
}

const fn kind_messages(kind: ManagedStorageKind) -> (&'static str, &'static str, &'static str) {
    match kind {
        ManagedStorageKind::DownloadedFiles => (
            "gui-storage-kind-downloaded-files",
            "gui-storage-kind-downloaded-files-detail",
            "gui-storage-why-downloaded-files",
        ),
        ManagedStorageKind::InstallerFiles => (
            "gui-storage-kind-installer-files",
            "gui-storage-kind-installer-files-detail",
            "gui-storage-why-installer-files",
        ),
        ManagedStorageKind::AssetObjects => (
            "gui-storage-kind-asset-objects",
            "gui-storage-kind-asset-objects-detail",
            "gui-storage-why-asset-objects",
        ),
        ManagedStorageKind::AssetIndexes => (
            "gui-storage-kind-asset-indexes",
            "gui-storage-kind-asset-indexes-detail",
            "gui-storage-why-asset-indexes",
        ),
        ManagedStorageKind::Libraries => (
            "gui-storage-kind-libraries",
            "gui-storage-kind-libraries-detail",
            "gui-storage-why-libraries",
        ),
        ManagedStorageKind::UnpackedNatives => (
            "gui-storage-kind-unpacked-natives",
            "gui-storage-kind-unpacked-natives-detail",
            "gui-storage-why-unpacked-natives",
        ),
    }
}

const fn withheld_message(reason: WithheldReason) -> &'static str {
    match reason {
        WithheldReason::LoaderFilesNotRecorded => "gui-storage-withheld-loader",
        WithheldReason::HostReferencesUnknown => "gui-storage-withheld-unknown",
        WithheldReason::LinkedIntoHost => "gui-storage-withheld-linked",
        WithheldReason::SuppliedByHand => "gui-storage-withheld-supplied",
    }
}

fn labeled(id: &str, key: &'static str, value: String) -> String {
    lookup_args(id, [(key, Arg::from(value))])
}

fn inspection_row(group: &ManagedStorageGroup) -> StorageGroupRow {
    let (label, detail, _) = kind_messages(group.kind);
    let mut lines = Vec::new();
    if !group.required.is_empty() {
        lines.push(labeled(
            "gui-storage-line-required",
            "value",
            size(group.required.bytes),
        ));
    }
    if !group.unused.is_empty() {
        lines.push(labeled(
            "gui-storage-line-unused",
            "value",
            size(group.unused.bytes),
        ));
    }
    for (reason, usage) in &group.withheld {
        lines.push(lookup_args(
            "gui-storage-line-withheld",
            [
                ("value", Arg::from(size(usage.bytes))),
                ("reason", Arg::from(lookup(withheld_message(*reason)))),
            ],
        ));
    }
    StorageGroupRow {
        id: group.kind.as_str().into(),
        label: lookup(label).into(),
        detail: lookup(detail).into(),
        size: size(group.unused.bytes).into(),
        lines: string_model(lines),
        examples: string_model(Vec::new()),
    }
}

fn show_inspection(ui: &AppWindow, inspection: &StorageInspection) {
    let reclaimable = inspection.reclaimable_bytes();
    ui.set_storage_headline(
        if reclaimable > 0 {
            labeled("gui-storage-reclaimable", "value", size(reclaimable))
        } else {
            lookup("gui-storage-nothing-reclaimable")
        }
        .into(),
    );
    ui.set_storage_hosts(
        lookup_args(
            "gui-storage-hosts",
            [
                (
                    "clients",
                    Arg::from(i64::try_from(inspection.clients).unwrap_or(i64::MAX)),
                ),
                (
                    "servers",
                    Arg::from(i64::try_from(inspection.servers).unwrap_or(i64::MAX)),
                ),
                (
                    "value",
                    Arg::from(
                        inspection
                            .inspected_at
                            .with_timezone(&chrono::Local)
                            .format("%Y-%m-%d %H:%M")
                            .to_string(),
                    ),
                ),
            ],
        )
        .into(),
    );
    ui.set_storage_incomplete(!inspection.complete);
    ui.set_storage_groups(model(
        inspection.groups.iter().map(inspection_row).collect(),
    ));
    ui.set_storage_inspected(true);
}

fn state_word(fault: StorageFaultKind) -> String {
    lookup(match fault {
        StorageFaultKind::Corrupt => "gui-storage-fault-corrupt",
        StorageFaultKind::Missing => "gui-storage-fault-missing",
        StorageFaultKind::Unreadable => "gui-storage-fault-unreadable",
    })
}

fn fault_line(fault: &StorageFault) -> String {
    let path = Arg::from(fault.path.display().to_string());
    let state = Arg::from(state_word(fault.fault));
    match fault.standing {
        FaultStanding::Needed => lookup_args(
            "gui-storage-fault-needed",
            [("path", path), ("state", state)],
        ),
        FaultStanding::Unused => lookup_args(
            "gui-storage-fault-unneeded",
            [("path", path), ("state", state)],
        ),
        FaultStanding::Kept(reason) => lookup_args(
            "gui-storage-fault-kept",
            [
                ("path", path),
                ("state", state),
                ("reason", Arg::from(lookup(withheld_message(reason)))),
            ],
        ),
    }
}

fn unchecked_line(unchecked: &UncheckedStorage) -> String {
    match unchecked {
        UncheckedStorage::UnpackedNatives => lookup("gui-storage-unchecked-natives"),
        UncheckedStorage::LoaderFiles => lookup("gui-storage-unchecked-loader"),
        UncheckedStorage::VersionMetadataNotCached { version } => {
            labeled("gui-storage-unchecked-version", "version", version.clone())
        }
        UncheckedStorage::UnlistedLibraries { files } => {
            count("gui-storage-unchecked-libraries", *files)
        }
    }
}

fn show_integrity(ui: &AppWindow, state: &mut MaintenanceState, report: &IntegrityReport) {
    ui.set_storage_integrity(
        if report.is_sound() {
            count("gui-storage-integrity-sound", report.checked)
        } else {
            count(
                "gui-storage-integrity-faults",
                u64::try_from(report.faults.len()).unwrap_or(u64::MAX),
            )
        }
        .into(),
    );
    let mut faults: Vec<String> = report
        .faults
        .iter()
        .take(LISTED_FAULTS)
        .map(fault_line)
        .collect();
    if report.faults.len() > LISTED_FAULTS {
        faults.push(count(
            "gui-storage-faults-more",
            u64::try_from(report.faults.len() - LISTED_FAULTS).unwrap_or(u64::MAX),
        ));
    }
    ui.set_storage_faults(string_model(faults));
    ui.set_storage_unchecked(string_model(
        report.unchecked.iter().map(unchecked_line).collect(),
    ));
    ui.set_storage_checked(true);
    state.can_recover = report.recoverable().next().is_some();
    ui.set_storage_can_recover(state.can_recover);
}

fn recovery_lines(report: &RecoveryReport, failures: &[(String, String)]) -> Vec<String> {
    let mut lines = Vec::new();
    let number = |len: usize| u64::try_from(len).unwrap_or(u64::MAX);
    if report.recovered.is_empty() && report.still_damaged.is_empty() && failures.is_empty() {
        lines.push(lookup("gui-storage-recovery-nothing"));
    }
    if !report.recovered.is_empty() {
        lines.push(count(
            "gui-storage-recovery-recovered",
            number(report.recovered.len()),
        ));
    }
    if !report.still_damaged.is_empty() {
        lines.push(count(
            "gui-storage-recovery-still-damaged",
            number(report.still_damaged.len()),
        ));
    }
    for (version, detail) in failures {
        lines.push(lookup_args(
            "gui-storage-recovery-failure",
            [
                ("version", Arg::from(version.clone())),
                ("detail", Arg::from(detail.clone())),
            ],
        ));
    }
    if !report.left_for_cleanup.is_empty() {
        lines.push(count(
            "gui-storage-recovery-leftover",
            number(report.left_for_cleanup.len()),
        ));
    }
    if !report.left_in_place.is_empty() {
        lines.push(count(
            "gui-storage-recovery-kept",
            number(report.left_in_place.len()),
        ));
    }
    if report.canceled {
        lines.push(lookup("gui-storage-recovery-canceled"));
    }
    lines
}

fn plan_item_row(item: &ephervex_core::CleanupItem) -> StorageGroupRow {
    let (label, _, why) = kind_messages(item.kind);
    let lines = vec![lookup_args(
        "gui-storage-cleanup-item",
        [
            (
                "count",
                Arg::from(i64::try_from(item.usage.entries).unwrap_or(i64::MAX)),
            ),
            ("value", Arg::from(size(item.usage.bytes))),
        ],
    )];
    StorageGroupRow {
        id: item.kind.as_str().into(),
        label: lookup(label).into(),
        detail: lookup(why).into(),
        size: size(item.usage.bytes).into(),
        lines: string_model(lines),
        examples: string_model(
            item.largest
                .iter()
                .map(|path| {
                    labeled(
                        "gui-storage-cleanup-largest",
                        "value",
                        path.display().to_string(),
                    )
                })
                .collect(),
        ),
    }
}

fn kept_line(kept: &ephervex_core::KeptStorage) -> String {
    let kind = lookup(kind_messages(kept.kind).0);
    match kept.reason {
        KeptReason::Required => lookup_args(
            "gui-storage-kept-required",
            [
                ("kind", Arg::from(kind)),
                ("value", Arg::from(size(kept.usage.bytes))),
            ],
        ),
        KeptReason::Withheld(reason) => lookup_args(
            "gui-storage-kept-withheld",
            [
                ("kind", Arg::from(kind)),
                ("value", Arg::from(size(kept.usage.bytes))),
                ("reason", Arg::from(lookup(withheld_message(reason)))),
            ],
        ),
    }
}

fn show_plan(ui: &AppWindow, plan_id: u64, plan: &CleanupPlan) {
    let reclaimable = plan.reclaimable_bytes();
    ui.set_storage_plan_id(i32::try_from(plan_id).unwrap_or(i32::MAX));
    ui.set_storage_plan_empty(plan.is_empty());
    ui.set_storage_plan_summary(
        if plan.is_empty() {
            lookup("gui-storage-nothing-reclaimable")
        } else {
            lookup_args(
                "gui-storage-cleanup-summary",
                [
                    (
                        "count",
                        Arg::from(i64::try_from(plan.entries()).unwrap_or(i64::MAX)),
                    ),
                    ("value", Arg::from(size(reclaimable))),
                ],
            )
        }
        .into(),
    );
    ui.set_storage_plan_items(model(plan.items().iter().map(plan_item_row).collect()));
    ui.set_storage_plan_kept(string_model(plan.kept().iter().map(kept_line).collect()));
    ui.set_storage_plan_incomplete(!plan.is_complete());
    ui.set_storage_plan_confirm(
        labeled("gui-storage-cleanup-confirm", "value", size(reclaimable)).into(),
    );
}

fn outcome_lines(outcome: &CleanupOutcome) -> Vec<String> {
    let mut lines = Vec::new();
    let entries = i64::try_from(outcome.removed.entries).unwrap_or(i64::MAX);
    if outcome.removed.entries == 0 {
        lines.push(lookup("gui-storage-cleanup-nothing-removed"));
    } else if outcome.is_complete() && outcome.removed.bytes == outcome.planned.bytes {
        lines.push(lookup_args(
            "gui-storage-cleanup-removed",
            [
                ("count", Arg::from(entries)),
                ("value", Arg::from(size(outcome.removed.bytes))),
            ],
        ));
    } else {
        lines.push(lookup_args(
            "gui-storage-cleanup-partial",
            [
                ("count", Arg::from(entries)),
                ("value", Arg::from(size(outcome.removed.bytes))),
                ("planned", Arg::from(size(outcome.planned.bytes))),
            ],
        ));
    }
    if outcome.failed_entries > 0 {
        lines.push(count("gui-storage-cleanup-failed", outcome.failed_entries));
        for failure in &outcome.failures {
            lines.push(lookup_args(
                "gui-storage-cleanup-failure-line",
                [
                    ("path", Arg::from(failure.path.display().to_string())),
                    ("cause", Arg::from(failure.cause.to_string())),
                ],
            ));
        }
    }
    for (id, number) in [
        ("gui-storage-cleanup-skipped", outcome.no_longer_unused),
        ("gui-storage-cleanup-gone", outcome.already_gone),
        ("gui-storage-cleanup-not-reached", outcome.not_attempted),
    ] {
        if number > 0 {
            lines.push(count(id, number));
        }
    }
    if outcome.can_retry() {
        lines.push(lookup("gui-storage-cleanup-retry"));
    }
    lines
}

/// Starts a step, unless one is running. Returns whether it started.
fn begin(ui: &AppWindow, state: &mut MaintenanceState, step: Step) -> bool {
    if state.running.is_some() {
        return false;
    }
    state.running = Some(step);
    state.operation = None;
    ui.set_storage_maintenance_busy(step.id().into());
    ui.set_storage_maintenance_progress(SharedString::default());
    ui.set_storage_result(string_model(Vec::new()));
    true
}

/// Forgets what was shown about the stores, because the disk has changed since.
fn forget_what_was_shown(ui: &AppWindow, state: &mut MaintenanceState) {
    ui.set_storage_inspected(false);
    ui.set_storage_checked(false);
    ui.set_storage_groups(model(Vec::new()));
    ui.set_storage_faults(string_model(Vec::new()));
    ui.set_storage_unchecked(string_model(Vec::new()));
    state.can_recover = false;
    ui.set_storage_can_recover(false);
}

pub(super) fn register(
    ui: &AppWindow,
    commands: &Sender<Command>,
    state: &Rc<RefCell<DiskUsageState>>,
) {
    let start = |step: Step, command: fn() -> Command| {
        let sender = commands.clone();
        let state = Rc::clone(state);
        let weak = ui.as_weak();
        move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            if begin(&ui, &mut state.borrow_mut().maintenance, step) {
                send(&sender, command());
            }
        }
    };
    ui.on_inspect_storage(start(Step::Inspect, || Command::InspectStorage));
    ui.on_verify_storage(start(Step::Verify, || Command::VerifyStorage));
    ui.on_recover_storage(start(Step::Recover, || Command::RecoverStorage));
    ui.on_review_storage_cleanup(start(Step::Plan, || Command::PlanStorageCleanup));

    let sender = commands.clone();
    let confirming = Rc::clone(state);
    let weak = ui.as_weak();
    ui.on_confirm_storage_cleanup(move |plan| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let Ok(plan) = u64::try_from(plan) else {
            return;
        };
        if begin(&ui, &mut confirming.borrow_mut().maintenance, Step::Clean) {
            send(&sender, Command::ApplyStorageCleanup { plan });
        }
    });

    let sender = commands.clone();
    let stopping = Rc::clone(state);
    ui.on_stop_storage_maintenance(move || {
        if let Some(operation) = stopping.borrow().maintenance.operation.clone() {
            send(&sender, Command::CancelOperation(operation));
        }
    });
}

/// Follows the running step's activity, for its progress line and for Stop.
pub(super) fn apply_activity(
    ui: &AppWindow,
    state: &mut MaintenanceState,
    activity: &ActivityData,
) {
    let Some(step) = state.running else {
        return;
    };
    if activity.item.title != lookup(step.title())
        || !matches!(activity.item.status_kind, "queued" | "running")
    {
        return;
    }
    state.operation = Some(activity.item.id.clone());
    if !activity.item.detail.is_empty() {
        ui.set_storage_maintenance_progress(activity.item.detail.clone().into());
    }
}

/// Shows an answer from the maintenance operations.
///
/// `invalidate` marks the disk usage figures out of date, which a recovery and
/// a cleanup both warrant: they change what the disk holds.
pub(super) fn apply(
    ui: &AppWindow,
    disk_usage: &mut DiskUsageState,
    event: StorageMaintenanceEvent,
    commands: &Sender<Command>,
) {
    match event {
        StorageMaintenanceEvent::Inspected(inspection) => show_inspection(ui, &inspection),
        StorageMaintenanceEvent::Verified(report) => {
            show_integrity(ui, &mut disk_usage.maintenance, &report);
        }
        StorageMaintenanceEvent::Recovered { report, failures } => {
            forget_what_was_shown(ui, &mut disk_usage.maintenance);
            // A stopped recovery only looked at the files it was fixing, so it
            // has no whole-store result to show; recovery stays offered for
            // what is still damaged.
            if report.canceled {
                let more = !report.still_damaged.is_empty();
                disk_usage.maintenance.can_recover = more;
                ui.set_storage_can_recover(more);
            } else {
                show_integrity(ui, &mut disk_usage.maintenance, &report.after);
            }
            ui.set_storage_result(string_model(recovery_lines(&report, &failures)));
            changed_the_disk(ui, disk_usage, commands);
        }
        StorageMaintenanceEvent::Planned { plan, preview } => {
            show_plan(ui, plan, &preview);
            ui.set_modal("storage-cleanup".into());
        }
        StorageMaintenanceEvent::Cleaned(outcome) => {
            forget_what_was_shown(ui, &mut disk_usage.maintenance);
            ui.set_storage_result(string_model(outcome_lines(&outcome)));
            changed_the_disk(ui, disk_usage, commands);
        }
        StorageMaintenanceEvent::PlanOutdated => {
            ui.invoke_present_toast(lookup("gui-storage-cleanup-outdated").into(), true);
        }
        StorageMaintenanceEvent::Settled => {
            disk_usage.maintenance.running = None;
            disk_usage.maintenance.operation = None;
            ui.set_storage_maintenance_busy(SharedString::default());
            ui.set_storage_maintenance_progress(SharedString::default());
        }
    }
}

/// What every change to the shared stores has to tell the rest of the view.
fn changed_the_disk(ui: &AppWindow, disk_usage: &mut DiskUsageState, commands: &Sender<Command>) {
    super::invalidate_disk_usage(ui, disk_usage);
    // The storage page's own figure is of the same folders.
    send(commands, Command::MeasureArtifactCache);
}

#[cfg(test)]
mod tests {
    use ephervex_core::Usage;

    use super::*;

    fn usage(entries: u64, bytes: u64) -> Usage {
        Usage {
            entries,
            files: entries,
            bytes,
        }
    }

    #[test]
    fn a_clean_outcome_says_what_was_freed_and_offers_no_retry() {
        let outcome = CleanupOutcome {
            planned: usage(2, 300),
            removed: usage(2, 300),
            ..CleanupOutcome::default()
        };
        let lines = outcome_lines(&outcome);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("300 B"), "{lines:?}");
    }

    #[test]
    fn a_partial_outcome_never_claims_the_planned_amount() {
        let outcome = CleanupOutcome {
            planned: usage(3, 3000),
            removed: usage(2, 200),
            failed_entries: 1,
            failed_bytes: 2800,
            failures: vec![ephervex_core::CleanupFailure {
                kind: ManagedStorageKind::Libraries,
                path: "libraries/a/b.jar".into(),
                cause: std::io::ErrorKind::PermissionDenied,
            }],
            no_longer_unused: 1,
            ..CleanupOutcome::default()
        };

        let lines = outcome_lines(&outcome);

        assert!(
            lines[0].contains(&size(200)) && lines[0].contains(&size(3000)),
            "{lines:?}"
        );
        assert!(
            lines.iter().any(
                |line| line.contains("libraries/a/b.jar") && line.contains("permission denied")
            ),
            "the failure names its path and cause: {lines:?}"
        );
        assert!(lines.contains(&lookup("gui-storage-cleanup-retry")));
        assert!(
            lines
                .iter()
                .any(|line| line == &count("gui-storage-cleanup-skipped", 1))
        );
    }

    #[test]
    fn a_cleanup_that_removed_nothing_says_so() {
        let outcome = CleanupOutcome {
            planned: usage(1, 10),
            failed_entries: 1,
            failed_bytes: 10,
            ..CleanupOutcome::default()
        };
        assert_eq!(
            outcome_lines(&outcome)[0],
            lookup("gui-storage-cleanup-nothing-removed")
        );
    }
}
