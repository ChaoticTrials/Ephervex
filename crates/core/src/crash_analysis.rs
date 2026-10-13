//! Which of a host's mods a crash report points at.
//!
//! # Why this exists
//!
//! A mod that fails to load takes the game down before it starts, and the line
//! that reaches a console is the consequence rather than the cause. On a server
//! the usual shape is FML reporting that it cannot find the Minecraft server
//! thread, which is true, uninformative, and names nothing that can be acted
//! on. The crash report beside it is far more structured, and everything needed
//! to name a file in `mods/` is already in it.
//!
//! # What the report states
//!
//! Four independent forms of attribution, in descending order of how directly
//! they say a mod's name:
//!
//! - `-- Mod loading issue for: <id> --` names a mod id outright, and the
//!   section's `Mod file:` line gives the jar it came from.
//! - When the header carries no id, the run's log still names one:
//!   `Mod '<id>' encountered an error in a deferred task`.
//! - ModLauncher rewrites the class loader segment of a stack frame as
//!   `TRANSFORMER/<id>@<version>/`. Only that segment is read; the trailing
//!   `~[<jar>:...]` of a frame is not, because the jar comes from the mod list.
//! - `Mod List:` under `-- System Details --` is a table of
//!   `jar | display name | mod id | version | ...`, which turns any of the
//!   above into a file name the mods list can act on.
//!
//! The game and the mod loader write their own frames and mod list rows the
//! same way a mod does. They are evidence about the failure, never a mod that
//! can be disabled, so they are never reported as suspects. A host's own review
//! also drops a module whose jar is no file in its `mods/`.
//!
//! # What is inference, and what is not
//!
//! A missing class under one of Mojang's own client-only packages is not a
//! judgement call: those packages do not exist on a dedicated server, so a jar
//! that reached them is running client code where there is no client.
//!
//! [`SuspectReason::CompatTrigger`] is the one inference here. A frame in a
//! package named for another mod's compatibility support only runs when that
//! other mod is present, so that mod is worth trying without. It is reported as
//! something that triggered the failure and never as its cause, and it carries
//! the lowest confidence so that it sorts below anything the report stated
//! outright.
//!
//! Nothing in this module changes anything. It reads text and returns names.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use ephervex_storage::DiagnosticKind;
use ephervex_storage::diagnostics::MAX_VIEW_BYTES;

use crate::app::App;
use crate::diagnostics::{DiagnosticRequest, DiagnosticScope};
use crate::error::CoreResult;
use crate::mods::{InstalledHost, ModPresence};

/// Mojang's own client-only packages.
///
/// A dedicated server jar does not contain these, so a `NoClassDefFoundError`
/// or `ClassNotFoundException` naming one is conclusive evidence that client
/// code ran on a server rather than a hint that it might have.
const CLIENT_ONLY_PACKAGES: &[&str] = &[
    "net.minecraft.client.",
    "com.mojang.blaze3d.",
    "com.mojang.realmsclient.",
    "org.lwjgl.",
];

/// Mod ids that name the game or the mod loader rather than a mod in `mods/`.
///
/// Forge and NeoForge write their frames and `Mod List:` rows under these ids
/// exactly as they do for a mod, but none of them is a file the user can try
/// without.
const GAME_AND_LOADER_IDS: &[&str] = &[
    "minecraft",
    "forge",
    "neoforge",
    "fml",
    "fmlcore",
    "javafml",
    "lowcodefml",
    "mclanguage",
];

/// Where the frames worth attributing stop.
///
/// Everything above this header is the failure and the mod loading issues that
/// explain it. Below it are the environment tables and thread dumps, whose
/// frames belong to threads that were merely running at the time.
const SYSTEM_DETAILS_HEADER: &str = "-- System Details --";

/// How much a suspect is worth acting on, and the order they are shown in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SuspectConfidence {
    /// Worth trying, but inferred rather than stated.
    Low,
    /// The report placed it in the failing stack.
    Medium,
    /// The report or the log named it as the mod that failed.
    High,
}

impl SuspectConfidence {
    /// A stable identifier for `--json` output and frontend state.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// What the report said about a mod, which is also what makes it a suspect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuspectReason {
    /// A mod loading issue section, or the log, named this mod as the one that
    /// failed.
    Named,
    /// This mod's frames are in the failing stack.
    InFailingFrames,
    /// A failing frame is in another mod's compatibility support for this one,
    /// so this mod being present is what caused that code to run.
    CompatTrigger {
        /// The mod whose compatibility support named this one.
        triggered_by: String,
    },
}

impl SuspectReason {
    /// Stated evidence outranks inferred evidence, always.
    #[must_use]
    pub const fn confidence(&self) -> SuspectConfidence {
        match self {
            Self::Named => SuspectConfidence::High,
            Self::InFailingFrames => SuspectConfidence::Medium,
            Self::CompatTrigger { .. } => SuspectConfidence::Low,
        }
    }

    /// A stable identifier for `--json` output and frontend state.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Named => "named",
            Self::InFailingFrames => "in-failing-frames",
            Self::CompatTrigger { .. } => "compat-trigger",
        }
    }
}

/// One mod a crash report points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrashSuspect {
    /// The mod id, as the report spells it.
    pub mod_id: String,
    /// The jar the mod list gave for this id, when the table named one. This is
    /// what lets a suspect be matched to a row in a host's mods.
    pub jar: Option<String>,
    /// The mod list's display name, when the table named one.
    pub display_name: Option<String>,
    pub reason: SuspectReason,
}

impl CrashSuspect {
    #[must_use]
    pub const fn confidence(&self) -> SuspectConfidence {
        self.reason.confidence()
    }

    /// The best name to show for this mod.
    #[must_use]
    pub fn label(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.mod_id)
    }
}

/// What one crash report, and the log beside it, say about a failed start.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CrashAnalysis {
    /// The thrown type and its message, when the report stated one.
    pub failure: Option<String>,
    /// The class that could not be found, in dotted form, when the failure was
    /// a missing class.
    pub missing_class: Option<String>,
    /// Whether [`Self::missing_class`] is one of Mojang's client-only packages.
    ///
    /// Named for what it means rather than for what it matched: on a server
    /// this is proof that client code ran, and it is the reason the suggestion
    /// is worth making at all.
    pub client_code_on_server: bool,
    /// Most confident first, then alphabetically so two readings of one report
    /// agree.
    pub suspects: Vec<CrashSuspect>,
}

impl CrashAnalysis {
    /// Whether the report pointed at anything at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.suspects.is_empty()
    }

    /// The suspects the report stated, rather than the ones it implied.
    ///
    /// This is what a "disable the suggested mods" action acts on: a compat
    /// trigger is offered separately and ticked off by hand.
    pub fn stated(&self) -> impl Iterator<Item = &CrashSuspect> {
        self.suspects
            .iter()
            .filter(|suspect| suspect.confidence() > SuspectConfidence::Low)
    }
}

/// One row of the report's own `Mod List:` table.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ModListRow {
    jar: String,
    display_name: String,
    mod_id: String,
}

/// Reads a crash report, and the tail of the run's log, for the mods to try
/// without.
///
/// `log` may be empty: it only adds the `Mod '<id>'` attribution that a mod
/// loading issue section without an id would otherwise leave unnamed.
#[must_use]
pub fn analyze_crash_report(report: &str, log: &str) -> CrashAnalysis {
    let mod_list = parse_mod_list(report);
    let failing = failing_region(report);

    let mut reasons: BTreeMap<String, SuspectReason> = BTreeMap::new();
    for (mod_id, reason) in named_mods(failing, log)
        .map(|mod_id| (mod_id, SuspectReason::Named))
        .chain(attributed_frames(failing))
        .chain(compat_triggers(failing, &mod_list))
        .filter(|(mod_id, _)| !is_game_or_loader(mod_id))
    {
        promote(&mut reasons, mod_id, reason);
    }

    let missing_class = missing_class(failing);
    let mut suspects: Vec<CrashSuspect> = reasons
        .into_iter()
        .map(|(mod_id, reason)| {
            let row = mod_list.get(&mod_id);
            CrashSuspect {
                jar: row.map(|row| row.jar.clone()),
                display_name: row.map(|row| row.display_name.clone()),
                mod_id,
                reason,
            }
        })
        .collect();
    // Descending confidence, then by the name shown, so the order is stable
    // between two readings of the same file.
    suspects.sort_by(|left, right| {
        right
            .confidence()
            .cmp(&left.confidence())
            .then_with(|| left.mod_id.cmp(&right.mod_id))
    });

    CrashAnalysis {
        failure: failure_line(failing),
        client_code_on_server: missing_class.as_deref().is_some_and(is_client_only_class),
        missing_class,
        suspects,
    }
}

/// Whether an id names the game or the mod loader instead of a mod.
fn is_game_or_loader(mod_id: &str) -> bool {
    GAME_AND_LOADER_IDS
        .iter()
        .any(|id| id.eq_ignore_ascii_case(mod_id))
}

/// Keeps the strongest evidence for a mod rather than the last one seen.
fn promote(reasons: &mut BTreeMap<String, SuspectReason>, mod_id: String, reason: SuspectReason) {
    match reasons.get(&mod_id) {
        Some(existing) if existing.confidence() >= reason.confidence() => {}
        _ => {
            reasons.insert(mod_id, reason);
        }
    }
}

/// The part of the report that describes the failure.
///
/// Everything below `-- System Details --` is the environment: tables, property
/// dumps, and the stacks of threads that happened to be running. Frames from
/// there attribute nothing, and the mod list itself would otherwise read as a
/// stack of two hundred suspects.
fn failing_region(report: &str) -> &str {
    report
        .find(SYSTEM_DETAILS_HEADER)
        .map_or(report, |end| &report[..end])
}

/// Mod ids the report or the log named outright.
fn named_mods<'a>(failing: &'a str, log: &'a str) -> impl Iterator<Item = String> + 'a {
    let from_report = failing
        .lines()
        .filter_map(|line| mod_section_id(line.trim()));
    // FML writes this when a mod's deferred work throws, which is the case
    // where the report's own section header carries no id at all.
    let from_log = log
        .lines()
        .filter(|line| line.contains("encountered an error"))
        .filter_map(|line| quoted_after(line, "Mod "));
    from_report.chain(from_log)
}

/// The mod id a crash report section header names, if it names one.
///
/// FML has spelled this header two ways across versions, and a report is read
/// long after the version that wrote it: `-- Mod loading issue for: <id> --`
/// and `-- MOD <id> --`. The variant carrying no id at all is what makes the
/// log's own attribution worth reading.
fn mod_section_id(line: &str) -> Option<String> {
    let inner = line.strip_prefix("--")?.strip_suffix("--")?.trim();
    let id = inner
        .strip_prefix("Mod loading issue for:")
        .or_else(|| inner.strip_prefix("MOD "))?
        .trim();
    // A header is one token. Anything else is prose that begins the same way.
    (!id.is_empty() && !id.contains(char::is_whitespace)).then(|| id.to_ascii_lowercase())
}

/// Mod ids named by the `TRANSFORMER/<id>@` segment of the failing stack's frames.
///
/// A frame's `~[jar:...]` suffix is not read: the jar a suspect belongs to
/// comes from the mod list.
fn attributed_frames(failing: &str) -> impl Iterator<Item = (String, SuspectReason)> + '_ {
    failing
        .lines()
        .filter(|line| line.trim_start().starts_with("at "))
        .filter_map(|line| {
            let rest = line.split_once("TRANSFORMER/")?.1;
            let id = rest.split(['@', '/']).next()?.trim();
            (!id.is_empty()).then(|| (id.to_owned(), SuspectReason::InFailingFrames))
        })
}

/// Mods whose presence caused another mod's compatibility code to run.
///
/// A package segment of `compat.<id>` or `<id>compat` is a naming convention
/// rather than a guarantee, so a candidate only becomes a suspect when the
/// report's own mod list confirms that a mod by that id is actually installed.
fn compat_triggers(
    failing: &str,
    mod_list: &BTreeMap<String, ModListRow>,
) -> Vec<(String, SuspectReason)> {
    let mut found = Vec::new();
    for line in failing
        .lines()
        .filter(|line| line.trim_start().starts_with("at "))
    {
        let Some((owner, class)) = frame_owner_and_class(line) else {
            continue;
        };
        let segments: Vec<String> = class.split('.').map(str::to_ascii_lowercase).collect();
        for (index, segment) in segments.iter().enumerate() {
            let Some(candidate) = compat_target(segment, segments.get(index + 1)) else {
                continue;
            };
            // A mod cannot trigger its own compatibility layer, and the mod
            // list is what separates a real mod id from a package that merely
            // reads like one.
            if candidate != owner && mod_list.contains_key(&candidate) {
                found.push((
                    candidate,
                    SuspectReason::CompatTrigger {
                        triggered_by: owner.clone(),
                    },
                ));
            }
        }
    }
    found
}

/// The mod id a class name segment marks as a compatibility target.
///
/// Both spellings the convention appears in: a `compat` segment followed by the
/// mod it supports (`...compat.iris.IrisCompat`), and a segment that carries
/// the mod's name itself (`IrisCompat`, `CompatIris`). Comparison is
/// case-insensitive because the package half of a class name is lowercase and
/// the type half is not.
fn compat_target(segment: &str, next: Option<&String>) -> Option<String> {
    if segment == "compat" {
        return next
            .filter(|following| !following.is_empty() && *following != "compat")
            .cloned();
    }
    let target = segment
        .strip_suffix("compat")
        .or_else(|| segment.strip_prefix("compat"))?;
    (!target.is_empty()).then(|| target.to_owned())
}

/// The mod a frame belongs to and the class it names.
fn frame_owner_and_class(line: &str) -> Option<(String, &str)> {
    let rest = line.split_once("TRANSFORMER/")?.1;
    let (owner, rest) = rest.split_once('@')?;
    let class = rest.split_once('/')?.1;
    // The frame is `<class>.<method>(<file>:<line>)`; the package segments are
    // all that the compat convention is spelled in.
    let class = class.split('(').next().unwrap_or(class);
    Some((owner.trim().to_ascii_lowercase(), class))
}

/// The `Mod List:` table, keyed by mod id.
fn parse_mod_list(report: &str) -> BTreeMap<String, ModListRow> {
    let mut rows = BTreeMap::new();
    let mut in_table = false;
    for line in report.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Mod List:") {
            in_table = true;
            continue;
        }
        if !in_table {
            continue;
        }
        match parse_mod_list_row(trimmed) {
            Some(row) => {
                rows.insert(row.mod_id.clone(), row);
            }
            // Blank lines occur inside the block; anything else is the next
            // section and ends the table.
            None if trimmed.is_empty() => {}
            None => in_table = false,
        }
    }
    rows
}

/// One `jar | display name | mod id | version | ...` row.
///
/// The column count has grown between FML versions, so only the leading four
/// are required and anything further is ignored.
fn parse_mod_list_row(line: &str) -> Option<ModListRow> {
    let mut fields = line.split('|').map(str::trim);
    let jar = fields.next()?;
    let display_name = fields.next()?;
    let mod_id = fields.next()?;
    // A version column has to be present for this to be a table row rather than
    // a sentence that happens to contain a pipe.
    fields.next()?;
    if !jar.to_ascii_lowercase().ends_with(".jar") || mod_id.is_empty() || mod_id.contains(' ') {
        return None;
    }
    Some(ModListRow {
        jar: jar.to_owned(),
        display_name: display_name.to_owned(),
        mod_id: mod_id.to_ascii_lowercase(),
    })
}

/// The thrown type and message the report stated.
///
/// Searched a header at a time rather than a line at a time, because the
/// headers are not equally informative and `Description:` is written first. The
/// exception is the failure; the description is the category it fell into.
fn failure_line(failing: &str) -> Option<String> {
    ["Exception message:", "Failure message:", "Description:"]
        .into_iter()
        .find_map(|header| {
            failing.lines().find_map(|line| {
                let rest = line.trim().strip_prefix(header)?.trim();
                (!rest.is_empty()).then(|| rest.to_owned())
            })
        })
}

/// The class a missing-class failure named, in dotted form.
///
/// The JVM spells it with slashes in a `NoClassDefFoundError` and with dots in a
/// `ClassNotFoundException`; both mean the same class.
fn missing_class(failing: &str) -> Option<String> {
    failing.lines().find_map(|line| {
        let rest = line
            .split_once("NoClassDefFoundError:")
            .or_else(|| line.split_once("ClassNotFoundException:"))?
            .1;
        let name = rest
            .split_whitespace()
            .next()?
            .trim_end_matches(['.', ',', ')', ']'])
            .replace('/', ".");
        (!name.is_empty()).then_some(name)
    })
}

/// Whether a class lives in one of Mojang's client-only packages.
fn is_client_only_class(class: &str) -> bool {
    CLIENT_ONLY_PACKAGES
        .iter()
        .any(|package| class.starts_with(package))
}

/// The text between the first `'` pair after a marker.
fn quoted_after(line: &str, marker: &str) -> Option<String> {
    let rest = line.split_once(marker)?.1;
    let rest = rest.strip_prefix('\'')?;
    let id = rest.split_once('\'')?.0.trim();
    (!id.is_empty()).then(|| id.to_owned())
}

/// One mandatory dependency the Forge or NeoForge loader refused to start
/// without, as its mod sorter logged it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoaderReportedDependency {
    /// The mod id that is missing or at the wrong version.
    pub mod_id: String,
    /// The mod id that asked for it.
    pub requested_by: String,
    /// The version range as the requesting mod declared it, exact.
    pub expected_range: String,
    /// The version found, or `None` when nothing was installed.
    pub actual: Option<String>,
}

/// What FML's mod sorter logs before it refuses to load. Forge and NeoForge
/// share the wording, and each following line names one dependency.
const UNMET_MANDATORY_HEADER: &str = "Missing or unsupported mandatory dependencies:";

/// The mandatory dependencies a Forge or NeoForge run's log says it lacked.
///
/// Empty for any other loader and for a log that reached no such refusal.
/// Nothing is inferred: every field is the loader's own text.
#[must_use]
pub fn loader_dependency_failures(log: &str) -> Vec<LoaderReportedDependency> {
    let mut found: Vec<LoaderReportedDependency> = Vec::new();
    let mut lines = log.lines();
    while let Some(line) = lines.next() {
        if !line.contains(UNMET_MANDATORY_HEADER) {
            continue;
        }
        for entry in lines.by_ref() {
            let Some(parsed) = loader_dependency_line(entry.trim()) else {
                break;
            };
            if !found.contains(&parsed) {
                found.push(parsed);
            }
        }
    }
    found
}

/// `Mod ID: 'a', Requested by: 'b', Expected range: '[1,)', Actual version: 'c'`
fn loader_dependency_line(line: &str) -> Option<LoaderReportedDependency> {
    let rest = line.strip_prefix("Mod ID: '")?;
    let (mod_id, rest) = rest.split_once("', Requested by: '")?;
    let (requested_by, rest) = rest.split_once("', Expected range: '")?;
    let (expected_range, rest) = rest.split_once("', Actual version: '")?;
    let actual = rest.strip_suffix('\'')?;
    Some(LoaderReportedDependency {
        mod_id: mod_id.to_owned(),
        requested_by: requested_by.to_owned(),
        expected_range: expected_range.to_owned(),
        actual: (actual != "[MISSING]").then(|| actual.to_owned()),
    })
}

/// A suspect, and the row in the host's own mods it corresponds to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuspectedMod {
    pub suspect: CrashSuspect,
    /// Where the jar belongs, relative to the game directory, when the host's
    /// record names it. `None` for a mod the record does not have, such as one
    /// dropped into `mods/` by hand: it is still worth naming, but the mods
    /// list has no row to toggle.
    pub logical_path: Option<PathBuf>,
    /// What the host's mods list currently says about the file, when it has a
    /// row for it. A suspect already `Disabled` is not worth suggesting again.
    pub presence: Option<ModPresence>,
}

impl SuspectedMod {
    /// Whether disabling this suspect is an action the host can actually take.
    #[must_use]
    pub fn is_actionable(&self) -> bool {
        self.logical_path.is_some() && self.presence == Some(ModPresence::Installed)
    }
}

/// What a host's most recent crash says about its own mods.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrashReview {
    /// The report this was read from, as the diagnostics list names it.
    pub report_name: String,
    /// When the report was written, where the filesystem reports it. This is
    /// what lets a frontend decide whether a crash belongs to the run the user
    /// just watched or to an older one.
    pub reported_at: Option<DateTime<Utc>>,
    pub analysis: CrashAnalysis,
    /// Every suspect, most confident first, matched against the host's mods.
    pub mods: Vec<SuspectedMod>,
}

impl CrashReview {
    /// The suspects a "disable the suggested mods" action would act on.
    ///
    /// Stated evidence only, and only files the host still has enabled. A
    /// compat trigger is deliberately excluded: it is an inference, and it is
    /// offered as a separate choice rather than folded into a bulk action.
    pub fn suggested(&self) -> impl Iterator<Item = &SuspectedMod> {
        self.mods.iter().filter(|entry| {
            entry.is_actionable() && entry.suspect.confidence() > SuspectConfidence::Low
        })
    }
}

impl App {
    /// Reads the crash report of a host's last run and names the mods it
    /// points at.
    ///
    /// `Ok(None)` when the last run did not crash, which is the ordinary case
    /// and not a failure: the newest report was written before the host last
    /// started, or there is none. A report that parses to nothing still returns a
    /// review, so a frontend can say that a crash happened and that it named no
    /// mod, rather than silently showing the same thing as no crash at all.
    pub fn crash_review(&self, host: InstalledHost) -> CoreResult<Option<CrashReview>> {
        let scope = diagnostic_scope(host);
        let Some(report) = self
            .diagnostic_entries(scope, DiagnosticKind::CrashReport)?
            .into_iter()
            .next()
        else {
            return Ok(None);
        };
        let last_started = match host {
            InstalledHost::Instance(id) => self.current_client(id)?.last_launched_at,
            InstalledHost::Server(id) => self.server(id)?.last_started_at,
        };
        let from_last_run = match (report.modified, last_started) {
            (Some(reported), Some(started)) => reported >= started,
            (_, None) => true,
            (None, Some(_)) => false,
        };
        if !from_last_run {
            return Ok(None);
        }

        // The whole report rather than the default window: the `Mod List:`
        // table is the last thing in the file and is what turns an attribution
        // into a file name, so a truncated read would lose exactly the part
        // that makes the rest actionable.
        let text = self
            .read_diagnostic(
                &DiagnosticRequest::new(scope, DiagnosticKind::CrashReport, &report.name)
                    .with_budget(MAX_VIEW_BYTES),
            )?
            .text;
        let mut analysis = analyze_crash_report(&text, &self.recent_log_text(scope));

        let known = self.host_mod_snapshot(host)?;
        // A module the mod list gives a jar for that is no file in this host's
        // `mods/` (such as one bundled inside another jar) is not something
        // to disable. A suspect the table gave no jar for stays: it
        // may be a mod that failed before it was listed.
        analysis.suspects.retain(|suspect| {
            suspect.jar.as_deref().is_none_or(|jar| {
                known
                    .mods
                    .iter()
                    .any(|entry| entry.file_name.eq_ignore_ascii_case(jar))
            })
        });
        let mods = analysis
            .suspects
            .iter()
            .map(|suspect| {
                let row = suspect.jar.as_deref().and_then(|jar| {
                    known
                        .mods
                        .iter()
                        .find(|entry| entry.file_name.eq_ignore_ascii_case(jar))
                });
                SuspectedMod {
                    suspect: suspect.clone(),
                    logical_path: row.map(|entry| entry.logical_path.clone()),
                    presence: row.map(|entry| entry.presence),
                }
            })
            .collect();

        Ok(Some(CrashReview {
            report_name: report.name,
            reported_at: report.modified,
            analysis,
            mods,
        }))
    }

    /// The tail of the run's own log, for the attribution a report can omit.
    ///
    /// Best-effort by design: the log is a second opinion, and a host whose
    /// log has already rotated away still has a readable crash report.
    fn recent_log_text(&self, scope: DiagnosticScope) -> String {
        self.read_diagnostic(&DiagnosticRequest::new(
            scope,
            DiagnosticKind::Log,
            "latest.log",
        ))
        .map(|view| view.text)
        .unwrap_or_default()
    }
}

fn diagnostic_scope(host: InstalledHost) -> DiagnosticScope {
    match host {
        InstalledHost::Instance(id) => DiagnosticScope::Client(id),
        InstalledHost::Server(id) => DiagnosticScope::Server(id),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use super::*;

    /// The block FML's mod sorter logs, as `latest.log` carries it.
    const UNMET_DEPENDENCY_LOG: &str = "\
[12:00:01] [main/INFO] [net.neoforged.fml.loading.ModSorter/LOADING]: Found 3 mod requirements (3 mandatory, 0 optional)
[12:00:01] [main/INFO] [net.neoforged.fml.loading.ModSorter/LOADING]: Found 2 mod requirements missing (2 mandatory, 0 optional)
[12:00:01] [main/ERROR] [net.neoforged.fml.loading.ModSorter/LOADING]: Missing or unsupported mandatory dependencies:
\tMod ID: 'bar', Requested by: 'foo', Expected range: '[3.0,)', Actual version: '2.4'
\tMod ID: 'registrate', Requested by: 'flywheel', Expected range: '[1.3,)', Actual version: '[MISSING]'
[12:00:02] [main/INFO] [net.neoforged.fml.loading.ImmediateWindowHandler/]: Loading ImmediateWindowProvider fmlearlywindow
";

    #[test]
    fn a_crash_older_than_the_committed_launch_is_not_reviewed_while_the_index_row_lags() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = App::open_at(ephervex_storage::Paths::rooted_at(base.path())).expect("app");
        let client = app
            .create_client_for_test(ephervex_storage::NewClient::new(
                "lagging",
                "1.21.1",
                ephervex_model::LoaderSelection::vanilla(),
            ))
            .expect("client");
        let reports =
            ephervex_storage::metadata::client_game_dir(&client.root).join("crash-reports");
        std::fs::create_dir_all(&reports).expect("crash reports directory");
        std::fs::write(
            reports.join("crash-2026-08-30_18.09.07-client.txt"),
            "---- Minecraft Crash Report ----\n",
        )
        .expect("write crash report");
        app.commit_client_without_index_for_test(client.id, |committed| {
            committed.last_launched_at = Some(chrono::Utc::now() + chrono::TimeDelta::hours(1));
        })
        .expect("commit");
        assert!(
            app.client(client.id)
                .expect("row")
                .last_launched_at
                .is_none()
        );

        let review = app
            .crash_review(InstalledHost::Instance(client.id))
            .expect("review");

        assert!(
            review.is_none(),
            "the report predates the launch the metadata records"
        );
    }

    #[test]
    fn the_loader_s_unmet_dependencies_are_read_exactly_as_it_logged_them() {
        let found = loader_dependency_failures(UNMET_DEPENDENCY_LOG);

        assert_eq!(
            found,
            [
                LoaderReportedDependency {
                    mod_id: "bar".to_owned(),
                    requested_by: "foo".to_owned(),
                    expected_range: "[3.0,)".to_owned(),
                    actual: Some("2.4".to_owned()),
                },
                LoaderReportedDependency {
                    mod_id: "registrate".to_owned(),
                    requested_by: "flywheel".to_owned(),
                    expected_range: "[1.3,)".to_owned(),
                    actual: None,
                },
            ]
        );
    }

    #[test]
    fn a_log_without_the_refusal_reports_no_loader_dependency() {
        assert!(loader_dependency_failures(DEFERRED_TASK_LOG).is_empty());
        assert!(loader_dependency_failures("").is_empty());
    }

    /// A mod loading failure whose section header carries no id, where the
    /// failing frames are one mod's compatibility support for another.
    const DEFERRED_TASK_REPORT: &str = r"---- Minecraft Crash Report ----
// Who set us up the TNT?

Time: 2026-08-30 18:09:07
Description: Mod loading error has occurred

java.lang.Exception: Mod Loading has failed
	at net.neoforged.fml.ModLoader.lambda$dispatchParallelTask$0(ModLoader.java:200) ~[fmlcore-1.21.1.jar:?]

-- Mod loading issue --
Details:
	Mod file: <No mod information provided>
	Failure message: An unexpected error occurred during mod loading
	Exception message: java.lang.NoClassDefFoundError: net/minecraft/client/multiplayer/ClientLevel
Stacktrace:
	at TRANSFORMER/sophisticatedcore@1.4.104/net.p3pp3rf1y.sophisticatedcore.compat.iris.IrisCompat.isRenderingWorld(IrisCompat.java:18) ~[sophisticatedcore-1.21.1-1.4.104.jar:1.4.104]
	at TRANSFORMER/sophisticatedcore@1.4.104/net.p3pp3rf1y.sophisticatedcore.client.ClientEventHandler.onRender(ClientEventHandler.java:64) ~[sophisticatedcore-1.21.1-1.4.104.jar:1.4.104]

-- System Details --
Details:
	Minecraft Version: 1.21.1
	Mod List: 
		client-1.21.1-20240808.144430-srg.jar             |Minecraft                     |minecraft                     |1.21.1              |COMMON_SET|Manifest: NOSIGNATURE
		sophisticatedcore-1.21.1-1.4.104.jar              |Sophisticated Core            |sophisticatedcore             |1.4.104             |DONE      |Manifest: NOSIGNATURE
		iris-neoforge-1.8.8+mc1.21.1.jar                  |Iris                          |iris                          |1.8.8               |DONE      |Manifest: NOSIGNATURE
		utilitix-1.21.1-1.13.2.jar                        |UtilitiX                      |utilitix                      |1.13.2              |DONE      |Manifest: NOSIGNATURE
";

    const DEFERRED_TASK_LOG: &str = "[18:09:07] [Thread-1/ERROR] [ne.ne.fm.ModLoader/LOADING]: \
Mod 'sophisticatedcore' encountered an error in a deferred task\n";

    /// The same failure, reported the other way FML spells it: the section
    /// header names the mod and gives its jar outright.
    const NAMED_SECTION_REPORT: &str = r"---- Minecraft Crash Report ----

Time: 2026-08-30 18:04:11
Description: Mod loading error has occurred

-- Mod loading issue for: euphoria_patcher --
Details:
	Mod file: /srv/servers/atm10/server/mods/euphoria_patcher-1.6.3-neoforge.jar
	Failure message: Euphoria Patcher (euphoria_patcher) has failed to load correctly
	Exception message: java.lang.ClassNotFoundException: com.mojang.blaze3d.pipeline.RenderPipeline$Snippet
Stacktrace:
	at TRANSFORMER/euphoria_patcher@1.6.3/me.example.euphoria.EuphoriaPatcher.setup(EuphoriaPatcher.java:88) ~[euphoria_patcher-1.6.3-neoforge.jar:1.6.3]

-- System Details --
Details:
	Mod List: 
		euphoria_patcher-1.6.3-neoforge.jar               |Euphoria Patcher              |euphoria_patcher              |1.6.3               |ERROR     |Manifest: NOSIGNATURE
		utilitix-1.21.1-1.13.2.jar                        |UtilitiX                      |utilitix                      |1.13.2              |DONE      |Manifest: NOSIGNATURE
";

    fn suspect<'a>(analysis: &'a CrashAnalysis, mod_id: &str) -> &'a CrashSuspect {
        analysis
            .suspects
            .iter()
            .find(|suspect| suspect.mod_id == mod_id)
            .unwrap_or_else(|| panic!("{mod_id} is not among {:?}", analysis.suspects))
    }

    #[test]
    fn a_section_that_names_a_mod_makes_it_the_leading_suspect() {
        let analysis = analyze_crash_report(NAMED_SECTION_REPORT, "");

        let leading = analysis.suspects.first().expect("a suspect was found");
        assert_eq!(leading.mod_id, "euphoria_patcher");
        assert_eq!(leading.reason, SuspectReason::Named);
        assert_eq!(leading.confidence(), SuspectConfidence::High);
        assert_eq!(
            leading.jar.as_deref(),
            Some("euphoria_patcher-1.6.3-neoforge.jar"),
            "the mod list turns the id into a file the mods list can act on"
        );
        assert_eq!(leading.label(), "Euphoria Patcher");
    }

    #[test]
    fn an_issue_with_no_id_is_named_by_the_run_log_instead() {
        let without_log = analyze_crash_report(DEFERRED_TASK_REPORT, "");
        assert_eq!(
            suspect(&without_log, "sophisticatedcore").reason,
            SuspectReason::InFailingFrames,
            "the frames are all the report itself offers"
        );

        let with_log = analyze_crash_report(DEFERRED_TASK_REPORT, DEFERRED_TASK_LOG);
        assert_eq!(
            suspect(&with_log, "sophisticatedcore").reason,
            SuspectReason::Named,
            "the log states what the report's header left out"
        );
    }

    #[test]
    fn a_missing_mojang_client_class_is_reported_as_client_code_on_a_server() {
        let analysis = analyze_crash_report(DEFERRED_TASK_REPORT, DEFERRED_TASK_LOG);
        assert_eq!(
            analysis.missing_class.as_deref(),
            Some("net.minecraft.client.multiplayer.ClientLevel"),
            "the slashes a NoClassDefFoundError uses mean the same class"
        );
        assert!(analysis.client_code_on_server);

        // The other spelling, and another of Mojang's client-only packages.
        let named = analyze_crash_report(NAMED_SECTION_REPORT, "");
        assert_eq!(
            named.missing_class.as_deref(),
            Some("com.mojang.blaze3d.pipeline.RenderPipeline$Snippet")
        );
        assert!(named.client_code_on_server);
    }

    #[test]
    fn a_server_side_failure_is_not_reported_as_client_code() {
        let report = DEFERRED_TASK_REPORT.replace(
            "net/minecraft/client/multiplayer/ClientLevel",
            "net/minecraft/server/level/ServerLevel",
        );
        let analysis = analyze_crash_report(&report, "");
        assert_eq!(
            analysis.missing_class.as_deref(),
            Some("net.minecraft.server.level.ServerLevel")
        );
        assert!(!analysis.client_code_on_server);
    }

    #[test]
    fn a_compat_package_names_the_mod_whose_presence_triggered_it() {
        let analysis = analyze_crash_report(DEFERRED_TASK_REPORT, DEFERRED_TASK_LOG);

        let iris = suspect(&analysis, "iris");
        assert_eq!(
            iris.reason,
            SuspectReason::CompatTrigger {
                triggered_by: "sophisticatedcore".to_owned()
            }
        );
        assert_eq!(iris.confidence(), SuspectConfidence::Low);
        assert_eq!(
            iris.jar.as_deref(),
            Some("iris-neoforge-1.8.8+mc1.21.1.jar")
        );
    }

    #[test]
    fn an_inference_never_outranks_what_the_report_stated() {
        let analysis = analyze_crash_report(DEFERRED_TASK_REPORT, DEFERRED_TASK_LOG);

        assert_eq!(
            analysis.suspects.first().map(|suspect| &suspect.mod_id),
            Some(&"sophisticatedcore".to_owned())
        );
        let stated: Vec<&str> = analysis
            .stated()
            .map(|suspect| suspect.mod_id.as_str())
            .collect();
        assert_eq!(
            stated,
            ["sophisticatedcore"],
            "iris is offered separately, never disabled by a suggestion"
        );
    }

    #[test]
    fn a_compat_package_naming_something_that_is_not_installed_is_not_a_suspect() {
        let report = DEFERRED_TASK_REPORT
            .lines()
            .filter(|line| !line.contains("|iris "))
            .collect::<Vec<_>>()
            .join("\n");
        let analysis = analyze_crash_report(&report, DEFERRED_TASK_LOG);

        assert!(
            !analysis.suspects.iter().any(|s| s.mod_id == "iris"),
            "the mod list is what separates a real mod id from a package that reads like one"
        );
    }

    #[test]
    fn the_mod_list_is_not_read_as_a_stack_of_suspects() {
        let analysis = analyze_crash_report(DEFERRED_TASK_REPORT, DEFERRED_TASK_LOG);

        assert!(
            !analysis.suspects.iter().any(|s| s.mod_id == "utilitix"),
            "an installed mod the failure never mentions is not a suspect"
        );
        assert!(!analysis.suspects.iter().any(|s| s.mod_id == "minecraft"));
    }

    #[test]
    fn a_crash_that_points_at_no_mod_suggests_nothing() {
        let report = "---- Minecraft Crash Report ----\n\n\
Description: Ticking entity\n\n\
java.lang.NullPointerException: Ticking entity\n\
\tat net.minecraft.world.entity.Entity.tick(Entity.java:100) ~[client-1.21.1.jar:?]\n";
        let analysis = analyze_crash_report(report, "");

        assert!(analysis.is_empty());
        assert!(!analysis.client_code_on_server);
        assert_eq!(analysis.failure.as_deref(), Some("Ticking entity"));
    }

    /// A crash whose failing stack runs through the game and the loader on its
    /// way into one mod, with all three listed in the mod table.
    const GAME_AND_LOADER_FRAMES_REPORT: &str = r"---- Minecraft Crash Report ----

Time: 2026-08-30 18:12:40
Description: Ticking block entity

java.lang.NullPointerException: Ticking block entity
	at TRANSFORMER/utilitix@1.13.2/me.example.utilitix.TickingThing.tick(TickingThing.java:41) ~[utilitix-1.21.1-1.13.2.jar:1.13.2]
	at TRANSFORMER/minecraft@1.21.1/net.minecraft.world.level.Level.tickBlockEntities(Level.java:512) ~[client-1.21.1-20240808.144430-srg.jar:?]
	at TRANSFORMER/neoforge@21.1.80/net.neoforged.neoforge.event.EventHooks.onTick(EventHooks.java:90) ~[neoforge-21.1.80-universal.jar:?]
	at TRANSFORMER/FMLCORE@4.0.30/net.neoforged.fml.ModLoader.run(ModLoader.java:200) ~[fmlcore-1.21.1-4.0.30.jar:?]

-- System Details --
Details:
	Mod List: 
		client-1.21.1-20240808.144430-srg.jar             |Minecraft                     |minecraft                     |1.21.1              |COMMON_SET|Manifest: NOSIGNATURE
		neoforge-21.1.80-universal.jar                    |NeoForge                      |neoforge                      |21.1.80             |COMMON_SET|Manifest: NOSIGNATURE
		utilitix-1.21.1-1.13.2.jar                        |UtilitiX                      |utilitix                      |1.13.2              |DONE      |Manifest: NOSIGNATURE
";

    #[test]
    fn game_and_loader_frames_are_never_reported_as_suspects() {
        let analysis = analyze_crash_report(GAME_AND_LOADER_FRAMES_REPORT, "");

        let ids: Vec<&str> = analysis
            .suspects
            .iter()
            .map(|s| s.mod_id.as_str())
            .collect();
        assert_eq!(ids, ["utilitix"]);
        assert_eq!(
            analysis
                .stated()
                .map(|s| s.mod_id.as_str())
                .collect::<Vec<_>>(),
            ["utilitix"]
        );
    }

    #[test]
    fn a_loading_issue_section_for_the_loader_names_no_suspect() {
        let report = "---- Minecraft Crash Report ----\n\n\
-- Mod loading issue for: neoforge --\n\
Details:\n\
\tFailure message: NeoForge has failed to load correctly\n";
        let analysis = analyze_crash_report(report, "");

        assert!(analysis.is_empty());
    }

    #[test]
    fn a_frame_in_a_mod_is_still_attributed_when_the_game_also_appears() {
        let analysis = analyze_crash_report(GAME_AND_LOADER_FRAMES_REPORT, "");

        let found = suspect(&analysis, "utilitix");
        assert_eq!(found.reason, SuspectReason::InFailingFrames);
        assert_eq!(found.jar.as_deref(), Some("utilitix-1.21.1-1.13.2.jar"));
    }

    #[test]
    fn the_exception_is_preferred_over_the_category_it_fell_into() {
        let analysis = analyze_crash_report(NAMED_SECTION_REPORT, "");
        assert_eq!(
            analysis.failure.as_deref(),
            Some(
                "java.lang.ClassNotFoundException: com.mojang.blaze3d.pipeline.RenderPipeline$Snippet"
            ),
            "`Description:` is written first and says the least"
        );
    }

    #[test]
    fn a_report_with_no_recognizable_structure_is_read_without_failing() {
        for text in [
            "",
            "not a crash report at all",
            "-- -- | | |\nMod List:\n||",
        ] {
            let analysis = analyze_crash_report(text, "");
            assert!(analysis.is_empty(), "{text:?}");
        }
    }
}
