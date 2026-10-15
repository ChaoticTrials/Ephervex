//! Diagnostics as the window follows it: each shown host's result, the fix
//! under review, the failed run beside it, and its bundle choice, plus the one
//! diagnosis or fix the worker has not finished, whichever host it is for.
//!
//! Kept per host rather than per pane (ADR 0039): every pane on a host shows
//! the same result, and a second pane cannot start the same work twice.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::Sender;

use ephervex_core::{DiagnosticFix, InstalledHost};
use ephervex_i18n::{Arg, lookup, lookup_args};
use slint::{ComponentHandle as _, ModelRc, SharedString};

use super::{
    Workspace, clear_host_busy, copy_to_clipboard, isolation_host_kind, model,
    refuse_while_host_changes, send, string_model,
};
use crate::backend::{self, ActivityData, BundleExportData, BundlePartData, Command};
use crate::diagnosis_view::{
    CheckView, DiagnosisView, FindingRoute, FindingView, OutcomeView, PreviewView,
};
use crate::ui::{
    AppWindow, BundlePartRow, DiagnosticCheckRow, DiagnosticFindingRow, FixChangeRow, HostView,
};

/// How many of a finding's files a card lists before it says how many more.
const LISTED_FILES: usize = 8;

/// The work a Diagnostics surface asks the worker for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Work {
    Diagnose,
    /// Applying a fix, and the diagnosis after it.
    Apply,
}

impl Work {
    /// The reported operation whose stage the surface shows, and which
    /// stopping the work cancels.
    const fn operation(self) -> &'static str {
        match self {
            Self::Diagnose => "operation-diagnosing-host",
            Self::Apply => "operation-applying-fix",
        }
    }
}

#[derive(Debug, Clone)]
struct Pending {
    request: u64,
    host: InstalledHost,
    work: Work,
    /// The operation's id once it first reports, which is what Stop cancels.
    operation: Option<String>,
}

#[derive(Debug)]
struct Shown {
    view: DiagnosisView,
    checked_at: String,
    /// The failed run this quick diagnosis was made for, or `None` for one
    /// the user asked for.
    after_failure: Option<String>,
}

/// Why the newest diagnosis ended without a result.
#[derive(Debug)]
enum Ended {
    Failed(String),
    Canceled,
}

#[derive(Debug)]
enum Panel {
    Loading,
    Ready(PreviewView),
    Failed(String),
    Applying(PreviewView),
}

/// The fix whose preview is open.
#[derive(Debug)]
struct Review {
    title: String,
    fix: DiagnosticFix,
    request: u64,
    panel: Panel,
}

#[derive(Debug)]
struct RunNotice {
    title: String,
    failure: String,
    likely: String,
    report: Option<String>,
}

/// What the Diagnostics surface is choosing to put in a bundle for its host.
#[derive(Debug, Default)]
struct BundleChoice {
    /// Everything the core planner offered, in its order.
    parts: Vec<BundlePartData>,
    /// Chosen by name, which is the handle the planner gave each part.
    selected: HashSet<String>,
    /// Whether `parts` came from a plan yet, so a later plan of the same
    /// host keeps the user's choices rather than the defaults.
    planned: bool,
    /// The newest export asked for this host. A result for any other id is
    /// stale.
    last_request: u64,
    /// The export the worker has not finished.
    pending: Option<u64>,
}

impl BundleChoice {
    /// Takes a fresh plan. The first one chooses the newest game log, the
    /// newest crash report, and the host's own recorded state, rather than
    /// every file there is; a later one keeps whatever is still offered.
    fn plan(&mut self, parts: Vec<BundlePartData>) {
        if self.planned {
            self.selected
                .retain(|name| parts.iter().any(|part| &part.name == name));
        } else {
            let mut newest_log = false;
            let mut newest_crash = false;
            self.selected = parts
                .iter()
                .filter(|part| match part.kind {
                    "game_log" => !std::mem::replace(&mut newest_log, true),
                    "crash_report" => !std::mem::replace(&mut newest_crash, true),
                    "recipe" | "record" => true,
                    _ => false,
                })
                .map(|part| part.name.clone())
                .collect();
        }
        self.parts = parts;
        self.planned = true;
    }

    /// The chosen names, in the order the planner offered them.
    fn chosen(&self) -> Vec<String> {
        self.parts
            .iter()
            .filter(|part| self.selected.contains(&part.name))
            .map(|part| part.name.clone())
            .collect()
    }

    /// Whether an export may start now: none runs and something is chosen.
    fn can_dispatch(&self) -> bool {
        self.pending.is_none() && !self.selected.is_empty()
    }
}

/// What one shown host's Diagnostics surface holds.
#[derive(Debug, Default)]
struct DiagnosisHost {
    shown: Option<Shown>,
    ended: Option<Ended>,
    /// The host changed after `shown` was made.
    stale: bool,
    progress: String,
    review: Option<Review>,
    outcome: Option<OutcomeView>,
    run: Option<RunNotice>,
    bundle: BundleChoice,
}

#[derive(Debug, Default)]
pub(super) struct DiagnosisFlowState {
    hosts: HashMap<InstalledHost, DiagnosisHost>,
    last_request: u64,
    /// The newest export id handed out to any host. It is never reset, so an
    /// id is not reused by another host or by a host that left and came back.
    last_bundle_request: u64,
    last_preview: u64,
    /// The one diagnosis or fix the worker has not finished. A second
    /// request, from any view of any host, is refused here before a second
    /// command can queue behind it.
    pending: Option<Pending>,
    /// Per client, where [`backend::run_clock`] stood when it was last started.
    /// A failure of a run that ended before that is stale.
    run_floors: HashMap<InstalledHost, u64>,
}

impl DiagnosisFlowState {
    /// Whether a failed client run that ended at `ended` is not older than the
    /// host's last start.
    pub(super) fn client_run_is_current(&self, host: InstalledHost, ended: u64) -> bool {
        self.run_floors
            .get(&host)
            .is_none_or(|floor| ended >= *floor)
    }

    /// Notes that a client was started when [`backend::run_clock`] read
    /// `floor`, so a failure of a run that already ended is stale from here on.
    pub(super) fn note_client_start(&mut self, host: InstalledHost, floor: u64) {
        if matches!(host, InstalledHost::Instance(_)) {
            self.run_floors.insert(host, floor);
        }
    }

    /// Makes sure the surface has a place for `host`.
    fn open(&mut self, host: InstalledHost) {
        self.hosts.entry(host).or_default();
    }

    fn host(&self, host: InstalledHost) -> Option<&DiagnosisHost> {
        self.hosts.get(&host)
    }

    fn host_mut(&mut self, host: InstalledHost) -> Option<&mut DiagnosisHost> {
        self.hosts.get_mut(&host)
    }

    /// Forgets the surfaces of hosts the workspace no longer shows. Work still
    /// running finishes; its result has nowhere to go.
    pub(super) fn follow(&mut self, shown: &[InstalledHost]) {
        self.hosts.retain(|host, _| shown.contains(host));
    }

    /// Claims the single in-flight work, or refuses while one runs.
    fn dispatch(&mut self, host: InstalledHost, work: Work) -> Option<u64> {
        if self.pending.is_some() {
            return None;
        }
        self.last_request = self.last_request.wrapping_add(1);
        self.pending = Some(Pending {
            request: self.last_request,
            host,
            work,
            operation: None,
        });
        Some(self.last_request)
    }

    /// Starts a bundle export for a shown host with an id no host has had.
    fn dispatch_bundle(&mut self, host: InstalledHost) -> Option<u64> {
        let bundle = &mut self.hosts.get_mut(&host)?.bundle;
        if !bundle.can_dispatch() {
            return None;
        }
        self.last_bundle_request = self.last_bundle_request.wrapping_add(1);
        let request = self.last_bundle_request;
        bundle.last_request = request;
        bundle.pending = Some(request);
        Some(request)
    }

    /// Releases the work the worker reports finished.
    fn settle(&mut self, request: u64) -> Option<Pending> {
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.request == request)
        {
            self.pending.take()
        } else {
            None
        }
    }

    /// Whether a result belongs on a surface as it is now: the host is shown,
    /// and the result answers the newest request.
    fn accepts(&self, host: InstalledHost, request: u64) -> bool {
        self.hosts.contains_key(&host) && request == self.last_request
    }

    fn working_on(&self, host: InstalledHost) -> Option<Work> {
        self.pending
            .as_ref()
            .filter(|pending| pending.host == host)
            .map(|pending| pending.work)
    }
}

fn finding_row(host: InstalledHost, key: String, finding: &FindingView) -> DiagnosticFindingRow {
    // A server has no runtime settings surface to send anyone to.
    let route = finding.route.filter(|route| {
        !(matches!(host, InstalledHost::Server(_)) && *route == FindingRoute::HostSettings)
    });
    let more = finding.files.len().saturating_sub(LISTED_FILES);
    DiagnosticFindingRow {
        key: key.into(),
        problem: finding.class == ephervex_core::FindingClass::Problem,
        title: finding.title.as_str().into(),
        detail: finding.detail.as_str().into(),
        chain: string_model(finding.chain.clone()),
        files: string_model(finding.files.iter().take(LISTED_FILES).cloned().collect()),
        more_files: if more > 0 {
            lookup_args("gui-diagnosis-more-files", [("count", Arg::from(more))]).into()
        } else {
            SharedString::default()
        },
        fix_label: finding.fix_label.as_str().into(),
        route: route.map_or("", FindingRoute::as_str).into(),
        route_label: route
            .map_or_else(String::new, |route| lookup(route.label()))
            .into(),
        install_label: finding
            .install
            .as_ref()
            .map_or_else(String::new, |_| lookup("gui-diagnosis-install-latest"))
            .into(),
        install_provider: finding
            .install
            .as_ref()
            .map_or("", |(provider, _)| provider.as_str())
            .into(),
        install_project: finding
            .install
            .as_ref()
            .map_or("", |(_, project)| project.as_str())
            .into(),
    }
}

fn check_row(check: &CheckView) -> DiagnosticCheckRow {
    DiagnosticCheckRow {
        label: check.label.as_str().into(),
        value: check.value.as_str().into(),
        state: check.state.into(),
        state_label: lookup(match check.state {
            "problem" => "gui-diagnosis-check-problem",
            "customized" => "gui-diagnosis-check-customized",
            "skipped" => "gui-diagnosis-check-skipped",
            _ => "gui-diagnosis-check-ok",
        })
        .into(),
    }
}

/// The key a finding's actions pass back: its section and its position.
fn finding_key(problem: bool, index: usize) -> String {
    format!("{}{index}", if problem { 'p' } else { 'c' })
}

fn finding_by_key<'a>(view: &'a DiagnosisView, key: &str) -> Option<&'a FindingView> {
    let (list, index) = match key.split_at_checked(1)? {
        ("p", index) => (&view.problems, index),
        ("c", index) => (&view.customized, index),
        _ => return None,
    };
    list.get(index.parse::<usize>().ok()?)
}

fn summary(shown: &Shown) -> (String, String) {
    let view = &shown.view;
    let title = if !view.problems.is_empty() {
        lookup_args(
            "gui-diagnosis-problems",
            [("count", Arg::from(view.problems.len()))],
        )
    } else if view.incomplete {
        lookup("gui-diagnosis-incomplete")
    } else {
        lookup("gui-diagnosis-healthy")
    };
    let time = Arg::from(shown.checked_at.as_str());
    let detail = if shown.after_failure.is_some() {
        lookup_args("gui-diagnosis-summary-quick", [("time", time)])
    } else {
        lookup_args(
            "gui-diagnosis-summary",
            [("time", time), ("count", Arg::from(view.customized.len()))],
        )
    };
    (title, detail)
}

fn write_result(view: &mut HostView, host: InstalledHost, shown: Option<&Shown>) {
    let Some(shown) = shown else {
        view.diagnosis_failure = SharedString::default();
        view.diagnosis_healthy = false;
        view.diagnosis_summary = SharedString::default();
        view.diagnosis_detail = SharedString::default();
        view.diagnosis_problems = ModelRc::default();
        view.diagnosis_customized = ModelRc::default();
        view.diagnosis_checks = ModelRc::default();
        view.diagnosis_crash_report = SharedString::default();
        return;
    };
    let (title, detail) = summary(shown);
    let rows = |problem: bool, findings: &[FindingView]| {
        model(
            findings
                .iter()
                .enumerate()
                .map(|(index, finding)| finding_row(host, finding_key(problem, index), finding))
                .collect(),
        )
    };
    view.diagnosis_failure = shown.after_failure.clone().unwrap_or_default().into();
    view.diagnosis_healthy = shown.view.healthy;
    view.diagnosis_summary = title.into();
    view.diagnosis_detail = detail.into();
    view.diagnosis_problems = rows(true, &shown.view.problems);
    view.diagnosis_customized = rows(false, &shown.view.customized);
    view.diagnosis_checks = model(shown.view.checks.iter().map(check_row).collect());
    view.diagnosis_crash_report = shown.view.crash_report.clone().unwrap_or_default().into();
}

fn write_review(view: &mut HostView, review: Option<&Review>) {
    let (state, preview, error) = match review.map(|review| &review.panel) {
        None => ("", None, String::new()),
        Some(Panel::Loading) => ("loading", None, String::new()),
        Some(Panel::Ready(preview)) => ("ready", Some(preview), String::new()),
        Some(Panel::Applying(preview)) => ("applying", Some(preview), String::new()),
        Some(Panel::Failed(error)) => ("failed", None, error.clone()),
    };
    view.fix_state = state.into();
    view.fix_title = review
        .map(|review| review.title.clone())
        .unwrap_or_default()
        .into();
    view.fix_error = error.into();
    view.fix_changes = model(
        preview
            .map(|preview| {
                preview
                    .changes
                    .iter()
                    .map(|change| FixChangeRow {
                        action: change.action.as_str().into(),
                        name: change.name.as_str().into(),
                        detail: change.detail.as_str().into(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
    );
    view.fix_notes = string_model(
        preview
            .map(|preview| preview.notes.clone())
            .unwrap_or_default(),
    );
    view.fix_blocked = preview
        .map(|preview| preview.blocked.clone())
        .unwrap_or_default()
        .into();
}

/// Puts a host's Diagnostics into every view of it, as the flow holds it.
fn render(workspace: &Workspace, state: &DiagnosisFlowState, host: InstalledHost) {
    let Some(surface) = state.host(host) else {
        return;
    };
    let shown_state = if state.working_on(host).is_some() {
        "working"
    } else {
        match surface.ended {
            Some(Ended::Failed(_)) => "failed",
            Some(Ended::Canceled) => "canceled",
            None if surface.shown.is_some() => "result",
            None => "unchecked",
        }
    };
    let error = match &surface.ended {
        Some(Ended::Failed(message)) => message.clone(),
        Some(Ended::Canceled) => lookup("gui-diagnosis-canceled"),
        None => String::new(),
    };
    let bundle = bundle_view(&surface.bundle);
    workspace.update(host, |view| {
        view.diagnosis_state = shown_state.into();
        view.diagnosis_progress = surface.progress.as_str().into();
        view.diagnosis_error = error.into();
        view.diagnosis_stale = surface.stale;
        write_result(view, host, surface.shown.as_ref());
        write_review(view, surface.review.as_ref());
        let outcome = surface.outcome.as_ref();
        view.fix_result = outcome
            .map(|outcome| outcome.message.clone())
            .unwrap_or_default()
            .into();
        view.fix_result_failed = outcome.is_some_and(|outcome| outcome.failed);
        view.fix_result_lines = string_model(
            outcome
                .map(|outcome| outcome.lines.clone())
                .unwrap_or_default(),
        );
        let run = surface.run.as_ref();
        view.run_failure_title = run.map(|run| run.title.clone()).unwrap_or_default().into();
        view.run_failure = run
            .map(|run| run.failure.clone())
            .unwrap_or_default()
            .into();
        view.run_likely_cause = run.map(|run| run.likely.clone()).unwrap_or_default().into();
        view.run_crash_report = run
            .and_then(|run| run.report.clone())
            .unwrap_or_default()
            .into();
        write_bundle(view, bundle);
    });
}

/// Opens a host's Diagnostics surface: keeps what it already holds, and asks
/// for the bundle offer again, since the logs it can carry change whenever the
/// game runs.
pub(super) fn open(workspace: &Workspace, commands: &Sender<Command>, host: InstalledHost) {
    let mut state = workspace.diagnosis.borrow_mut();
    state.open(host);
    render(workspace, &state, host);
    drop(state);
    send(commands, Command::PlanDiagnosticBundle(host));
}

pub(super) fn apply_diagnosed(
    workspace: &Workspace,
    state: &mut DiagnosisFlowState,
    host: InstalledHost,
    request: u64,
    view: DiagnosisView,
) {
    if !state.accepts(host, request) {
        return;
    }
    let Some(surface) = state.host_mut(host) else {
        return;
    };
    surface.shown = Some(Shown {
        view,
        checked_at: chrono::Local::now().format("%H:%M").to_string(),
        after_failure: None,
    });
    surface.ended = None;
    surface.stale = false;
    // A preview names a finding of the result it came from.
    if !matches!(
        surface.review.as_ref().map(|review| &review.panel),
        Some(Panel::Applying(_))
    ) {
        surface.review = None;
    }
    render(workspace, state, host);
}

pub(super) fn apply_diagnosis_failed(
    workspace: &Workspace,
    state: &mut DiagnosisFlowState,
    host: InstalledHost,
    request: u64,
    message: String,
    canceled: bool,
) {
    if !state.accepts(host, request) {
        return;
    }
    let Some(surface) = state.host_mut(host) else {
        return;
    };
    surface.ended = Some(if canceled {
        Ended::Canceled
    } else {
        Ended::Failed(message)
    });
    render(workspace, state, host);
}

pub(super) fn apply_fix_previewed(
    workspace: &Workspace,
    state: &mut DiagnosisFlowState,
    host: InstalledHost,
    request: u64,
    preview: Result<PreviewView, String>,
) {
    let Some(review) = state
        .host_mut(host)
        .and_then(|surface| surface.review.as_mut())
        .filter(|review| review.request == request && matches!(review.panel, Panel::Loading))
    else {
        return;
    };
    review.panel = match preview {
        Ok(preview) => Panel::Ready(preview),
        Err(error) => Panel::Failed(lookup_args(
            "gui-fix-preview-failed",
            [("error", Arg::from(error))],
        )),
    };
    render(workspace, state, host);
}

pub(super) fn apply_fix_applied(
    workspace: &Workspace,
    state: &mut DiagnosisFlowState,
    host: InstalledHost,
    request: u64,
    outcome: OutcomeView,
) {
    if !state.accepts(host, request) {
        return;
    }
    let Some(surface) = state.host_mut(host) else {
        return;
    };
    surface.review = None;
    surface.stale = surface.stale || outcome.changed;
    surface.outcome = Some(outcome);
    render(workspace, state, host);
}

/// Releases finished work and whatever it held on screen.
pub(super) fn settle(
    ui: &AppWindow,
    workspace: &Workspace,
    state: &mut DiagnosisFlowState,
    request: u64,
) {
    let Some(pending) = state.settle(request) else {
        return;
    };
    if pending.work == Work::Apply && ui.get_host_busy_id() == pending.host.id() {
        clear_host_busy(ui);
    }
    if let Some(surface) = state.host_mut(pending.host) {
        surface.progress.clear();
        // A fix that ended without reporting, such as one refused before it
        // started, leaves its preview to be reviewed again.
        if let Some(review) = surface.review.as_mut()
            && let Panel::Applying(preview) = &review.panel
        {
            review.panel = Panel::Ready(preview.clone());
        }
    }
    render(workspace, state, pending.host);
}

/// Records the running work's operation, so it can be stopped, and shows the
/// stage it reports.
pub(super) fn apply_activity(
    ui: &AppWindow,
    workspace: &Workspace,
    state: &mut DiagnosisFlowState,
    activity: &ActivityData,
) {
    let Some(pending) = state.pending.as_mut() else {
        return;
    };
    if activity.entity != pending.host.id()
        || activity.item.title != lookup(pending.work.operation())
        || !matches!(activity.item.status_kind, "queued" | "running")
    {
        return;
    }
    pending.operation = Some(activity.item.id.clone());
    if activity.item.detail.is_empty() {
        return;
    }
    let (host, work) = (pending.host, pending.work);
    if work == Work::Apply && ui.get_host_busy_id() == host.id() {
        ui.set_host_busy_text(activity.item.detail.clone().into());
    }
    if let Some(surface) = state.host_mut(host) {
        activity.item.detail.clone_into(&mut surface.progress);
        render(workspace, state, host);
    }
}

/// Shows beside a shown host that its run failed, and what a quick diagnosis
/// made of it. Never navigates: the notice offers the way to Diagnostics.
pub(super) fn apply_run_failed(
    workspace: &Workspace,
    state: &mut DiagnosisFlowState,
    host: InstalledHost,
    failure: String,
    view: Option<DiagnosisView>,
) {
    // The notice sits on the host's other surfaces too, so a shown host gets
    // a place even if its Diagnostics surface was never opened.
    if !workspace.state.borrow().shown().contains(&host) {
        return;
    }
    state.open(host);
    let working = state.working_on(host).is_some();
    let Some(surface) = state.host_mut(host) else {
        return;
    };
    let crashed = view
        .as_ref()
        .is_some_and(|view| view.crash_report.is_some());
    let likely = view.as_ref().map_or_else(String::new, |view| {
        view.problems.first().map_or_else(
            || lookup("gui-run-no-cause"),
            |cause| {
                lookup_args(
                    "gui-run-likely-cause",
                    [("cause", Arg::from(cause.title.as_str()))],
                )
            },
        )
    });
    surface.run = Some(RunNotice {
        title: lookup(match host {
            InstalledHost::Server(_) => "gui-run-failed-server",
            InstalledHost::Instance(_) if crashed => "gui-run-crashed",
            InstalledHost::Instance(_) => "gui-run-failed-client",
        }),
        failure: failure.clone(),
        likely,
        report: view.as_ref().and_then(|view| view.crash_report.clone()),
    });
    // Work the user started is newer than anything this pass could say.
    if let Some(view) = view
        && !working
    {
        surface.shown = Some(Shown {
            view,
            checked_at: chrono::Local::now().format("%H:%M").to_string(),
            after_failure: Some(failure),
        });
        surface.ended = None;
        surface.stale = false;
        surface.review = None;
        surface.outcome = None;
    }
    render(workspace, state, host);
}

/// Forgets a failed run's notice once the host is launched again.
pub(super) fn forget_run(workspace: &Workspace, host: InstalledHost) {
    let mut state = workspace.diagnosis.borrow_mut();
    if let Some(surface) = state.host_mut(host)
        && surface.run.take().is_some()
    {
        render(workspace, &state, host);
    }
}

/// The bundle choice as it stands, ready for a host view.
struct BundleView {
    count: i32,
    selection: SharedString,
    parts: ModelRc<BundlePartRow>,
}

fn bundle_view(bundle: &BundleChoice) -> BundleView {
    let chosen: Vec<&BundlePartData> = bundle
        .parts
        .iter()
        .filter(|part| bundle.selected.contains(&part.name))
        .collect();
    let bytes: u64 = chosen.iter().map(|part| part.size_bytes).sum();
    BundleView {
        count: i32::try_from(chosen.len()).unwrap_or(i32::MAX),
        selection: lookup_args(
            "gui-bundle-selection",
            [
                ("count", Arg::from(chosen.len())),
                (
                    "size",
                    Arg::from(ephervex_model::format_binary_bytes(bytes)),
                ),
            ],
        )
        .into(),
        parts: model(
            bundle
                .parts
                .iter()
                .map(|part| BundlePartRow {
                    name: part.name.clone().into(),
                    kind: lookup(bundle_kind_label(part.kind)).into(),
                    size: ephervex_model::format_binary_bytes(part.size_bytes).into(),
                    selected: bundle.selected.contains(&part.name),
                })
                .collect(),
        ),
    }
}

fn write_bundle(view: &mut HostView, bundle: BundleView) {
    view.bundle_selected_count = bundle.count;
    view.bundle_selection = bundle.selection;
    view.bundle_parts = bundle.parts;
}

/// The word for one core bundle part kind.
fn bundle_kind_label(kind: &str) -> &'static str {
    match kind {
        "game_log" => "gui-bundle-kind-game-log",
        "crash_report" => "gui-bundle-kind-crash-report",
        "launcher_log" => "gui-bundle-kind-launcher-log",
        "recipe" => "gui-bundle-kind-recipe",
        _ => "gui-bundle-kind-record",
    }
}

/// Takes the planner's offer for a shown host.
pub(super) fn apply_bundle_plan(
    workspace: &Workspace,
    state: &mut DiagnosisFlowState,
    host: InstalledHost,
    parts: Vec<BundlePartData>,
) {
    let Some(surface) = state.host_mut(host) else {
        return;
    };
    surface.bundle.plan(parts);
    render(workspace, state, host);
}

pub(super) fn apply_bundle_exported(
    workspace: &Workspace,
    state: &DiagnosisFlowState,
    host: InstalledHost,
    request: u64,
    report: &BundleExportData,
) {
    if !accepts_export(state, host, request) {
        return;
    }
    let path = report.path.display().to_string();
    let mut result = lookup_args(
        "gui-bundle-exported",
        [
            ("count", Arg::from(report.included)),
            (
                "size",
                Arg::from(ephervex_model::format_binary_bytes(report.bytes)),
            ),
            ("path", Arg::from(path.as_str())),
        ],
    );
    if !report.unreadable.is_empty() {
        result.push(' ');
        result.push_str(&lookup_args(
            "gui-bundle-unreadable",
            [("value", Arg::from(report.unreadable.join(", ")))],
        ));
    }
    workspace.update(host, |view| {
        view.bundle_error = SharedString::default();
        view.bundle_result = result.into();
        view.bundle_result_path = path.into();
    });
}

pub(super) fn apply_bundle_export_failed(
    workspace: &Workspace,
    state: &DiagnosisFlowState,
    host: InstalledHost,
    request: u64,
    message: &str,
) {
    if !accepts_export(state, host, request) {
        return;
    }
    workspace.update(host, |view| {
        view.bundle_result = SharedString::default();
        view.bundle_result_path = SharedString::default();
        view.bundle_error = message.into();
    });
}

/// Whether an export's outcome answers the newest export of a shown host.
fn accepts_export(state: &DiagnosisFlowState, host: InstalledHost, request: u64) -> bool {
    state
        .host(host)
        .is_some_and(|surface| request == surface.bundle.last_request)
}

/// Releases a finished export, whichever shown host it was for.
pub(super) fn settle_bundle(workspace: &Workspace, state: &mut DiagnosisFlowState, request: u64) {
    for (&host, surface) in &mut state.hosts {
        if surface.bundle.pending == Some(request) {
            surface.bundle.pending = None;
            workspace.update(host, |view| view.bundle_exporting = false);
        }
    }
}

pub(super) fn register(ui: &AppWindow, commands: &Sender<Command>, workspace: &Workspace) {
    register_work(ui, commands, workspace);
    register_review(ui, commands, workspace);
    register_apply(ui, commands, workspace);
    register_bundle(ui, commands, workspace);
}

fn register_work(ui: &AppWindow, commands: &Sender<Command>, workspace: &Workspace) {
    let sender = commands.clone();
    let views = workspace.clone();
    let weak = ui.as_weak();
    ui.on_diagnose_host(move |kind, id| {
        let (Some(host), Some(ui)) = (backend::parse_mod_host(&kind, &id), weak.upgrade()) else {
            return;
        };
        let mut state = views.diagnosis.borrow_mut();
        if state.host(host).is_none() {
            return;
        }
        let Some(request) = state.dispatch(host, Work::Diagnose) else {
            ui.invoke_present_toast(lookup("gui-diagnosis-wait").into(), true);
            return;
        };
        if let Some(surface) = state.host_mut(host) {
            surface.ended = None;
            surface.progress.clear();
            surface.review = None;
            surface.outcome = None;
        }
        render(&views, &state, host);
        send(
            &sender,
            Command::DiagnoseHost {
                host,
                request,
                integrity: true,
            },
        );
    });

    let sender = commands.clone();
    let views = workspace.clone();
    ui.on_stop_diagnosis(move || {
        let operation = views
            .diagnosis
            .borrow()
            .pending
            .as_ref()
            .and_then(|pending| pending.operation.clone());
        if let Some(operation) = operation {
            send(&sender, Command::CancelOperation(operation));
        }
    });

    let views = workspace.clone();
    let weak = ui.as_weak();
    ui.on_copy_diagnosis_report(move |kind, id| {
        let (Some(host), Some(ui)) = (backend::parse_mod_host(&kind, &id), weak.upgrade()) else {
            return;
        };
        let report = views
            .diagnosis
            .borrow()
            .host(host)
            .and_then(|surface| surface.shown.as_ref())
            .map(|shown| shown.view.report.clone());
        if let Some(report) = report {
            copy_to_clipboard(&ui, report);
        }
    });

    let views = workspace.clone();
    ui.on_dismiss_run_failure(move |kind, id| {
        if let Some(host) = backend::parse_mod_host(&kind, &id) {
            forget_run(&views, host);
        }
    });
}

/// Opens the review of a change another surface proposes for a host, through
/// the same preview and apply as a finding's fix (ADR 0043).
pub(super) fn review_change(
    workspace: &Workspace,
    commands: &Sender<Command>,
    host: InstalledHost,
    title: String,
    fix: DiagnosticFix,
) {
    let mut state = workspace.diagnosis.borrow_mut();
    if state.working_on(host) == Some(Work::Apply) {
        return;
    }
    // The surface proposing the change may be the first to show this host
    // anything of Diagnostics.
    if !workspace.state.borrow().shown().contains(&host) {
        return;
    }
    state.open(host);
    state.last_preview = state.last_preview.wrapping_add(1);
    let request = state.last_preview;
    let Some(surface) = state.host_mut(host) else {
        return;
    };
    surface.review = Some(Review {
        title,
        fix: fix.clone(),
        request,
        panel: Panel::Loading,
    });
    surface.outcome = None;
    render(workspace, &state, host);
    send(commands, Command::PreviewFix { host, request, fix });
}

fn register_review(ui: &AppWindow, commands: &Sender<Command>, workspace: &Workspace) {
    let sender = commands.clone();
    let views = workspace.clone();
    ui.on_review_fix(move |kind, id, key| {
        let Some(host) = backend::parse_mod_host(&kind, &id) else {
            return;
        };
        let mut state = views.diagnosis.borrow_mut();
        if state.working_on(host) == Some(Work::Apply) {
            return;
        }
        state.last_preview = state.last_preview.wrapping_add(1);
        let request = state.last_preview;
        let Some(surface) = state.host_mut(host) else {
            return;
        };
        let Some((title, fix)) = surface
            .shown
            .as_ref()
            .and_then(|shown| finding_by_key(&shown.view, &key))
            .and_then(|finding| Some((finding.title.clone(), finding.fix.clone()?)))
        else {
            return;
        };
        surface.review = Some(Review {
            title,
            fix: fix.clone(),
            request,
            panel: Panel::Loading,
        });
        surface.outcome = None;
        render(&views, &state, host);
        send(&sender, Command::PreviewFix { host, request, fix });
    });

    let views = workspace.clone();
    ui.on_cancel_fix(move |kind, id| {
        let Some(host) = backend::parse_mod_host(&kind, &id) else {
            return;
        };
        let mut state = views.diagnosis.borrow_mut();
        let Some(surface) = state.host_mut(host) else {
            return;
        };
        if matches!(
            surface.review.as_ref().map(|review| &review.panel),
            Some(Panel::Applying(_))
        ) {
            return;
        }
        surface.review = None;
        render(&views, &state, host);
    });
}

fn register_apply(ui: &AppWindow, commands: &Sender<Command>, workspace: &Workspace) {
    let sender = commands.clone();
    let views = workspace.clone();
    let weak = ui.as_weak();
    ui.on_apply_fix(move |kind, id| {
        let (Some(host), Some(ui)) = (backend::parse_mod_host(&kind, &id), weak.upgrade()) else {
            return;
        };
        let mut state = views.diagnosis.borrow_mut();
        let Some(Review {
            fix,
            panel: Panel::Ready(preview),
            ..
        }) = state.host(host).and_then(|surface| surface.review.as_ref())
        else {
            return;
        };
        let (fix, preview) = (fix.clone(), preview.clone());
        if state.pending.is_some() {
            ui.invoke_present_toast(lookup("gui-diagnosis-wait").into(), true);
            return;
        }
        if refuse_while_host_changes(&weak) {
            return;
        }
        let integrity = matches!(fix, DiagnosticFix::RestoreFiles(_))
            || state
                .host(host)
                .and_then(|surface| surface.shown.as_ref())
                .is_some_and(|shown| shown.view.integrity_checked);
        let Some(request) = state.dispatch(host, Work::Apply) else {
            return;
        };
        if let Some(surface) = state.host_mut(host) {
            surface.progress.clear();
            surface.outcome = None;
            if let Some(review) = surface.review.as_mut() {
                review.panel = Panel::Applying(preview);
            }
        }
        render(&views, &state, host);
        ui.set_host_busy_kind(isolation_host_kind(host).into());
        ui.set_host_busy_id(host.id().into());
        ui.set_host_busy_text(lookup("operation-applying-fix").into());
        send(
            &sender,
            Command::ApplyFix {
                host,
                request,
                fix,
                integrity,
            },
        );
    });
}

fn register_bundle(ui: &AppWindow, commands: &Sender<Command>, workspace: &Workspace) {
    let views = workspace.clone();
    ui.on_toggle_bundle_part(move |kind, id, name, selected| {
        let Some(host) = backend::parse_mod_host(&kind, &id) else {
            return;
        };
        let mut state = views.diagnosis.borrow_mut();
        let Some(surface) = state.host_mut(host) else {
            return;
        };
        if surface.bundle.pending.is_some() {
            return;
        }
        if selected {
            surface.bundle.selected.insert(name.into());
        } else {
            surface.bundle.selected.remove(name.as_str());
        }
        render(&views, &state, host);
    });

    let views = workspace.clone();
    ui.on_select_all_bundle_parts(move |kind, id, selected| {
        let Some(host) = backend::parse_mod_host(&kind, &id) else {
            return;
        };
        let mut state = views.diagnosis.borrow_mut();
        let Some(surface) = state.host_mut(host) else {
            return;
        };
        if surface.bundle.pending.is_some() {
            return;
        }
        surface.bundle.selected = if selected {
            surface
                .bundle
                .parts
                .iter()
                .map(|part| part.name.clone())
                .collect()
        } else {
            HashSet::new()
        };
        render(&views, &state, host);
    });

    let sender = commands.clone();
    let views = workspace.clone();
    ui.on_export_bundle(move |kind, id| {
        let Some(host) = backend::parse_mod_host(&kind, &id) else {
            return;
        };
        let mut state = views.diagnosis.borrow_mut();
        // An empty selection is refused rather than written as an archive
        // holding nothing but its manifest.
        let Some(request) = state.dispatch_bundle(host) else {
            return;
        };
        let names = state
            .host(host)
            .map(|s| s.bundle.chosen())
            .unwrap_or_default();
        views.update(host, |view| {
            view.bundle_exporting = true;
            view.bundle_error = SharedString::default();
        });
        send(
            &sender,
            Command::ExportDiagnosticBundle {
                host,
                request,
                names,
            },
        );
    });

    let sender = commands.clone();
    ui.on_open_bundle_folder(move |path| {
        if !path.is_empty() {
            send(
                &sender,
                Command::OpenBundleFolder(PathBuf::from(path.as_str())),
            );
        }
    });
}
