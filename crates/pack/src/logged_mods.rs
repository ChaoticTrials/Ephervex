//! Reading the hashed mod list a crash report or `latest.log` can carry.
//!
//! LibX can replace the crash report's `Mod List:` with a table that also
//! records each jar's SHA-1 and CurseForge fingerprint (Murmur2):
//!
//! ```text
//!     Mod List:
//!         example-1.0.jar |Example |example |1.0 |Manifest: NOSIGNATURE|SHA1: <40 hex>|Murmur2: <u32>
//! ```
//!
//! A jar the game did not load from `mods/`, such as one nested in another
//! jar, is written with `-` in place of both hashes. The same report is also
//! printed into `latest.log` when the game crashes, so both files are read the
//! same way.
//!
//! Everything here is untrusted text. The caller bounds how much of it is read,
//! and this module bounds how many rows it keeps.

use ephervex_model::{Hash, HashAlgorithm, LoaderKind, LoaderSelection};

/// The most rows one table may contribute. Generous for any real pack and the
/// same order as the declared-file limit an archive is held to.
pub const MAX_LOGGED_ROWS: usize = 10_000;

/// Where a crash report begins, in a report file and in a log that printed it.
const REPORT_HEADER: &str = "---- Minecraft Crash Report ----";
const MOD_LIST_HEADER: &str = "Mod List:";

/// Which side wrote the report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoggedSide {
    Client,
    /// A dedicated server.
    Server,
}

/// One jar from the hashed table.
///
/// The first four columns are fixed width in the report and longer values are
/// cut, so `file_name`, `mod_id`, and `version` may be truncated. They are for
/// display and for recognizing an unhashed jar, never for lookups.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggedModRow {
    pub file_name: String,
    pub display_name: String,
    pub mod_id: String,
    pub version: String,
    pub sha1: Option<Hash>,
    pub murmur2: Option<u32>,
}

/// The width LibX gives the file name column. A name exactly this long may
/// have been cut.
const FILE_NAME_COLUMN: usize = 50;

impl LoggedModRow {
    /// Whether the report hashed this jar, which is what makes it findable.
    #[must_use]
    pub const fn is_hashed(&self) -> bool {
        self.sha1.is_some()
    }

    /// How the row is named to a user: its file, and its mod id when it has
    /// one.
    #[must_use]
    pub fn label(&self) -> String {
        if self.mod_id.is_empty() {
            self.file_name.clone()
        } else {
            format!("{} ({})", self.file_name, self.mod_id)
        }
    }

    /// Whether `name` is the file this row was written for, allowing for the
    /// report having cut a long name to the column width.
    #[must_use]
    pub fn names_file(&self, name: &str) -> bool {
        if self.file_name.chars().count() >= FILE_NAME_COLUMN {
            name.starts_with(&self.file_name)
        } else {
            name == self.file_name
        }
    }
}

/// The hashed mod list and the game it was running in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggedModList {
    /// Exactly as the report wrote it.
    pub minecraft_version: String,
    pub loader: LoaderSelection,
    pub side: LoggedSide,
    pub rows: Vec<LoggedModRow>,
}

impl LoggedModList {
    /// The rows the report hashed.
    pub fn hashed(&self) -> impl Iterator<Item = &LoggedModRow> {
        self.rows.iter().filter(|row| row.is_hashed())
    }
}

/// A hashed mod list and where it was read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggedMods {
    /// The name of the file it was read from.
    pub source: String,
    pub list: LoggedModList,
    /// Whether the file is a crash report rather than a log.
    pub crash_report: bool,
    /// Whether the catalogs have been asked about the list's hashes yet.
    pub resolved: bool,
    /// Catalogs that could not be asked, such as CurseForge without an API
    /// key, so a missing mod may still be on one of them.
    pub catalogs_not_asked: Vec<ephervex_model::ModProvider>,
}

/// Why a text could not be read as a hashed mod list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoggedModListError {
    /// No `Mod List:` table carries a single hash.
    HashesMissing,
    /// The report around the table does not name a Minecraft version.
    MinecraftVersionMissing,
    /// The report names no Forge or NeoForge build.
    LoaderMissing,
}

/// Whether `text` is a crash report file, as opposed to a log that may have
/// printed one.
#[must_use]
pub fn is_crash_report(text: &str) -> bool {
    text.trim_start_matches('\u{feff}')
        .trim_start()
        .starts_with(REPORT_HEADER)
}

/// Whether `text` has a `Mod List:` table at all, hashed or not.
///
/// Tells a crash report from some other text file, so a file that is neither a
/// pack nor a report is reported as unrecognized rather than as a report
/// without hashes.
#[must_use]
pub fn has_mod_list(text: &str) -> bool {
    text.lines()
        .any(|line| line.trim_start().starts_with(MOD_LIST_HEADER))
}

/// Reads the last hashed `Mod List:` table in `text` and the System Details
/// of the report it belongs to.
///
/// `latest.log` lists mods once at startup without hashes, and again inside
/// every crash report it prints, so the last table that has a hash is the one
/// describing the files the game actually had.
///
/// # Errors
///
/// [`LoggedModListError::HashesMissing`] when no table has a hash, and the
/// other variants when the report does not say which game to build.
pub fn parse_logged_mod_list(text: &str) -> Result<LoggedModList, LoggedModListError> {
    let lines: Vec<&str> = text.lines().collect();
    let (header, rows) = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.trim_start().starts_with(MOD_LIST_HEADER))
        .rev()
        .map(|(index, line)| (index, parse_table(indent(line), &lines[index + 1..])))
        .find(|(_, rows)| rows.iter().any(LoggedModRow::is_hashed))
        .ok_or(LoggedModListError::HashesMissing)?;

    let report = report_around(&lines, header);
    let minecraft_version =
        detail(report, "Minecraft Version:").ok_or(LoggedModListError::MinecraftVersionMissing)?;
    let loader = loader(report).ok_or(LoggedModListError::LoaderMissing)?;
    let side = match detail(report, "Type:") {
        Some(kind) if kind.starts_with("Dedicated Server") => LoggedSide::Server,
        _ => LoggedSide::Client,
    };

    Ok(LoggedModList {
        minecraft_version: minecraft_version.to_owned(),
        loader,
        side,
        rows,
    })
}

fn indent(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// The rows under a header: lines indented deeper than it that hold columns.
fn parse_table(header_indent: usize, lines: &[&str]) -> Vec<LoggedModRow> {
    lines
        .iter()
        .take_while(|line| indent(line) > header_indent && line.contains('|'))
        .filter_map(|line| parse_row(line))
        .take(MAX_LOGGED_ROWS)
        .collect()
}

fn parse_row(line: &str) -> Option<LoggedModRow> {
    let columns: Vec<&str> = line.split('|').map(str::trim).collect();
    let mut sha1 = None;
    let mut murmur2 = None;
    for column in &columns {
        if let Some(value) = column.strip_prefix("SHA1:") {
            sha1 = Hash::new(HashAlgorithm::Sha1, value.trim());
        } else if let Some(value) = column.strip_prefix("Murmur2:") {
            // `-` does not parse, which is the "not hashed" answer.
            murmur2 = value.trim().parse::<u32>().ok();
        }
    }
    let column = |index: usize| columns.get(index).copied().unwrap_or_default().to_owned();
    let file_name = column(0);
    if file_name.is_empty() {
        return None;
    }
    Some(LoggedModRow {
        file_name,
        display_name: column(1),
        mod_id: column(2),
        version: column(3),
        sha1,
        murmur2,
    })
}

/// The lines of the crash report that contains line `at`: from its header to
/// the next report's header, or the whole text when no header precedes it.
fn report_around<'a>(lines: &'a [&'a str], at: usize) -> &'a [&'a str] {
    let start = lines[..at]
        .iter()
        .rposition(|line| line.contains(REPORT_HEADER))
        .unwrap_or(0);
    let end = lines[at..]
        .iter()
        .position(|line| line.contains(REPORT_HEADER))
        .map_or(lines.len(), |offset| at + offset);
    &lines[start..end]
}

/// The value of a System Details line such as `Minecraft Version: 1.21.1`.
fn detail<'a>(report: &[&'a str], key: &str) -> Option<&'a str> {
    report.iter().find_map(|line| {
        let value = line.trim_start().strip_prefix(key)?.trim();
        (!value.is_empty()).then_some(value)
    })
}

/// The exact loader build from the `NeoForge:` or `Forge:` System Details line.
fn loader(report: &[&str]) -> Option<LoaderSelection> {
    if let Some(build) = detail(report, "NeoForge:") {
        return Some(LoaderSelection::exact(LoaderKind::NeoForge, build));
    }
    // Forge writes its build as a Maven coordinate.
    let build = detail(report, "Forge:")?;
    let build = build.strip_prefix("net.minecraftforge:").unwrap_or(build);
    Some(LoaderSelection::exact(LoaderKind::Forge, build))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    const SHA1_A: &str = "5a2eeeadcdfa33fb277bf1c2f21487fae1366771";

    fn row(file: &str, id: &str, sha1: &str, murmur2: &str) -> String {
        format!(
            "\t\t{file:<50.50}|{id:<30.30}|{id:<30.30}|1.0                 |Manifest: NOSIGNATURE|SHA1: {sha1:<40}|Murmur2: {murmur2}"
        )
    }

    fn report(details: &str, rows: &[String]) -> String {
        format!(
            "---- Minecraft Crash Report ----\n// Oops.\n\n-- System Details --\nDetails:\n{details}\n\tMod List: \n{}\n\tCrash Report UUID: 1\n",
            rows.join("\n")
        )
    }

    const NEOFORGE_CLIENT: &str = "\tMinecraft Version: 26.1.2\n\tMinecraft Version ID: 26.1.2\n\tType: Integrated Server (map_client.txt)\n\tFML: 11.0.15\n\tNeoForge: 26.1.2.109";

    #[test]
    fn hashed_rows_carry_both_hashes_and_unhashed_rows_carry_neither() {
        let text = report(
            NEOFORGE_CLIENT,
            &[
                row("example-1.0.jar", "example", SHA1_A, "3047319314"),
                row("nested-1.0.jar", "nested", "-", "-"),
            ],
        );
        let list = parse_logged_mod_list(&text).expect("hashed list");
        assert_eq!(list.rows.len(), 2);
        assert_eq!(list.rows[0].file_name, "example-1.0.jar");
        assert_eq!(list.rows[0].mod_id, "example");
        assert_eq!(list.rows[0].sha1.as_ref().map(Hash::digest), Some(SHA1_A));
        assert_eq!(list.rows[0].murmur2, Some(3_047_319_314));
        assert_eq!(list.rows[1].sha1, None);
        assert_eq!(list.rows[1].murmur2, None);
        assert_eq!(list.hashed().count(), 1);
    }

    #[test]
    fn the_game_is_read_exactly_from_the_system_details() {
        let text = report(NEOFORGE_CLIENT, &[row("a.jar", "a", SHA1_A, "1")]);
        let list = parse_logged_mod_list(&text).expect("hashed list");
        assert_eq!(list.minecraft_version, "26.1.2");
        assert_eq!(
            list.loader,
            LoaderSelection::exact(LoaderKind::NeoForge, "26.1.2.109")
        );
        assert_eq!(list.side, LoggedSide::Client);
    }

    #[test]
    fn a_forge_build_is_read_without_its_maven_group() {
        let details = "\tMinecraft Version: 1.20.1\n\tForge: net.minecraftforge:47.2.0";
        let list = parse_logged_mod_list(&report(details, &[row("a.jar", "a", SHA1_A, "1")]))
            .expect("hashed list");
        assert_eq!(
            list.loader,
            LoaderSelection::exact(LoaderKind::Forge, "47.2.0")
        );
    }

    #[test]
    fn a_dedicated_server_report_is_marked_as_the_server_side() {
        let details = "\tMinecraft Version: 26.1.2\n\tType: Dedicated Server (map_server.txt)\n\tNeoForge: 26.1.2.109";
        let list = parse_logged_mod_list(&report(details, &[row("a.jar", "a", SHA1_A, "1")]))
            .expect("hashed list");
        assert_eq!(list.side, LoggedSide::Server);
    }

    #[test]
    fn a_table_without_any_hash_is_refused() {
        let text = report(NEOFORGE_CLIENT, &[row("a.jar", "a", "-", "-")]);
        assert_eq!(
            parse_logged_mod_list(&text),
            Err(LoggedModListError::HashesMissing)
        );
    }

    #[test]
    fn the_stock_mod_list_without_hash_columns_is_refused() {
        let text = report(
            NEOFORGE_CLIENT,
            &["\t\texample-1.0.jar |Example |example |1.0 |Manifest: NOSIGNATURE".to_owned()],
        );
        assert_eq!(
            parse_logged_mod_list(&text),
            Err(LoggedModListError::HashesMissing)
        );
    }

    #[test]
    fn a_report_without_a_loader_build_is_refused() {
        let details = "\tMinecraft Version: 26.1.2";
        assert_eq!(
            parse_logged_mod_list(&report(details, &[row("a.jar", "a", SHA1_A, "1")])),
            Err(LoggedModListError::LoaderMissing)
        );
    }

    #[test]
    fn a_report_without_a_minecraft_version_is_refused() {
        let details = "\tNeoForge: 26.1.2.109";
        assert_eq!(
            parse_logged_mod_list(&report(details, &[row("a.jar", "a", SHA1_A, "1")])),
            Err(LoggedModListError::MinecraftVersionMissing)
        );
    }

    #[test]
    fn the_last_hashed_table_in_a_log_wins_over_the_startup_list() {
        let first = report(NEOFORGE_CLIENT, &[row("old.jar", "old", SHA1_A, "1")]);
        let second = report(
            "\tMinecraft Version: 26.1.2\n\tNeoForge: 26.1.2.110",
            &[row("new.jar", "new", SHA1_A, "2")],
        );
        let text = format!(
            "[17:02:15] [main/INFO]: \n     Mod List:\n\t\tName Version (Mod Id)\n\n\t\tOld 1.0 (old)\n{first}[17:08:03] [Render thread/INFO]: {second}"
        );
        let list = parse_logged_mod_list(&text).expect("hashed list");
        assert_eq!(list.rows[0].file_name, "new.jar");
        assert_eq!(list.loader.version.as_deref(), Some("26.1.2.110"));
    }

    #[test]
    fn a_long_file_name_cut_by_the_report_still_names_its_jar() {
        let long = "a-very-long-mod-file-name-that-the-report-cuts-off-somewhere.jar";
        let text = report(NEOFORGE_CLIENT, &[row(long, "long", "-", "-")]);
        let list = parse_logged_mod_list(&format!(
            "{text}{}",
            report(NEOFORGE_CLIENT, &[row("a.jar", "a", SHA1_A, "1")])
        ))
        .expect("hashed list");
        assert_eq!(list.rows.len(), 1);

        let cut = LoggedModRow {
            file_name: long.chars().take(FILE_NAME_COLUMN).collect(),
            display_name: String::new(),
            mod_id: String::new(),
            version: String::new(),
            sha1: None,
            murmur2: None,
        };
        assert!(cut.names_file(long));
        assert!(!cut.names_file("other.jar"));
    }

    #[test]
    fn a_malformed_hash_is_treated_as_missing() {
        let text = report(
            NEOFORGE_CLIENT,
            &[
                row("bad.jar", "bad", "not-a-hash", "nope"),
                row("a.jar", "a", SHA1_A, "1"),
            ],
        );
        let list = parse_logged_mod_list(&text).expect("hashed list");
        assert_eq!(list.rows[0].sha1, None);
        assert_eq!(list.rows[0].murmur2, None);
    }
}
