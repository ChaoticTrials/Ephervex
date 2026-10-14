//! Launcher state recorded from real packs, mods, servers, logs, and crash
//! reports. `SOURCES.md` documents where each file comes from.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use serde::Deserialize;

/// A project logo, named by address and by the SHA-256 of the bytes served.
#[derive(Deserialize)]
pub(crate) struct Logo {
    pub(crate) url: String,
    pub(crate) sha256: String,
}

#[derive(Deserialize)]
pub(crate) struct Instance {
    pub(crate) key: String,
    pub(crate) name: String,
    pub(crate) minecraft: String,
    pub(crate) loader_kind: String,
    pub(crate) loader: String,
    pub(crate) source: String,
    pub(crate) storage: String,
    pub(crate) mods: i32,
}

#[derive(Deserialize)]
pub(crate) struct Server {
    pub(crate) key: String,
    pub(crate) name: String,
    pub(crate) minecraft: String,
    pub(crate) loader_kind: String,
    pub(crate) loader: String,
    pub(crate) storage: String,
    pub(crate) port: u16,
    pub(crate) failed: bool,
    pub(crate) linked_client: String,
}

/// One row of the CaveStone mod list. `origin` is one of `pack`, `override`,
/// `user`, `dependency`, or `hand-placed`.
#[derive(Deserialize)]
pub(crate) struct Mod {
    pub(crate) project_id: String,
    pub(crate) title: String,
    pub(crate) version: String,
    pub(crate) file_name: String,
    pub(crate) origin: String,
    pub(crate) enabled: bool,
    pub(crate) deleted: bool,
    pub(crate) compatibility: String,
    pub(crate) icon: Logo,
}

#[derive(Deserialize)]
pub(crate) struct SearchHit {
    pub(crate) project_id: String,
    pub(crate) title: String,
    pub(crate) description: String,
    pub(crate) author: String,
    pub(crate) installed: bool,
    pub(crate) icon: Logo,
}

#[derive(Deserialize)]
pub(crate) struct Search {
    pub(crate) query: String,
    pub(crate) results: Vec<SearchHit>,
}

/// A snapshot the launcher captured of the CaveStone client.
#[derive(Deserialize)]
pub(crate) struct Snapshot {
    pub(crate) id: String,
    pub(crate) label: String,
    /// `manual`, `before_restore`, or `before_isolation`.
    pub(crate) origin: String,
    /// `copy` when the files were copied in full, `reflink` when they share blocks.
    pub(crate) copy_method: String,
    pub(crate) captured: String,
    pub(crate) size_bytes: u64,
    pub(crate) enabled: u32,
    pub(crate) disabled: u32,
}

#[derive(Deserialize)]
pub(crate) struct Recorded {
    pub(crate) instances: Vec<Instance>,
    pub(crate) servers: Vec<Server>,
    pub(crate) cavestone_mods: Vec<Mod>,
    pub(crate) mod_search: Search,
    pub(crate) snapshots: Vec<Snapshot>,
}

/// One release in the version list of the Manage Mod dialog.
#[derive(Deserialize)]
pub(crate) struct Release {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) number: String,
    pub(crate) channel: String,
    pub(crate) published: String,
}

/// A mod that another needs, or that needs it, with whether it is installed.
#[derive(Deserialize)]
pub(crate) struct Relation {
    pub(crate) name: String,
    pub(crate) kind: String,
    pub(crate) present: bool,
}

/// The data behind one Manage Mod dialog.
#[derive(Deserialize)]
pub(crate) struct Manage {
    pub(crate) project: String,
    pub(crate) title: String,
    pub(crate) path: String,
    pub(crate) installed_id: String,
    pub(crate) newest_id: String,
    pub(crate) versions: Vec<Release>,
    pub(crate) requires: Vec<Relation>,
    pub(crate) required_by: Vec<Relation>,
}

/// A mod that stops working when another is turned off.
#[derive(Deserialize)]
pub(crate) struct Breaks {
    pub(crate) name: String,
    pub(crate) path: String,
    pub(crate) distance: i32,
}

#[derive(Deserialize)]
pub(crate) struct Impact {
    pub(crate) name: String,
    pub(crate) path: String,
    pub(crate) breaks: Vec<Breaks>,
}

/// What an install would involve, as the launcher plans it.
#[derive(Deserialize)]
pub(crate) struct Plan {
    pub(crate) project: String,
    pub(crate) title: String,
    pub(crate) version_id: String,
    pub(crate) version: String,
    pub(crate) dependencies: usize,
    pub(crate) existing: usize,
    pub(crate) optional: usize,
    pub(crate) warnings: usize,
}

#[derive(Deserialize)]
pub(crate) struct Dialogs {
    pub(crate) manage: Manage,
    pub(crate) update: Manage,
    pub(crate) impact: Impact,
    pub(crate) plan: Plan,
}

#[derive(Deserialize)]
pub(crate) struct FileProblem {
    pub(crate) path: String,
    /// `Missing` or `Damaged`.
    pub(crate) state: String,
}

#[derive(Deserialize)]
pub(crate) struct FileChecks {
    pub(crate) total: usize,
    pub(crate) intact: usize,
    pub(crate) unverifiable: usize,
    pub(crate) problems: Vec<FileProblem>,
}

#[derive(Deserialize)]
pub(crate) struct ModChecks {
    pub(crate) enabled: usize,
    pub(crate) disabled: usize,
    pub(crate) answered: usize,
    pub(crate) unanswered: usize,
}

/// One part a diagnostic bundle can hold.
#[derive(Deserialize)]
pub(crate) struct BundlePart {
    pub(crate) name: String,
    pub(crate) kind: String,
    pub(crate) size_bytes: u64,
}

#[derive(Deserialize)]
pub(crate) struct ClientCrash {
    pub(crate) report: String,
    pub(crate) size_bytes: u64,
    pub(crate) summary: String,
}

/// What the launcher concluded about a server crash report.
#[derive(Deserialize)]
pub(crate) struct ServerCrash {
    pub(crate) report: String,
    pub(crate) failure: String,
    pub(crate) client_code_on_server: bool,
    pub(crate) suggested: usize,
}

#[derive(Deserialize)]
pub(crate) struct ServerCrashes {
    #[serde(rename = "fog-server")]
    pub(crate) fog: ServerCrash,
    #[serde(rename = "utility-mods-server")]
    pub(crate) utility_mods: ServerCrash,
}

/// What a diagnosis of the CaveStone client found, as the launcher's own
/// checks reported it.
#[derive(Deserialize)]
pub(crate) struct Diagnostics {
    pub(crate) files: FileChecks,
    pub(crate) mods: ModChecks,
    pub(crate) java: String,
    pub(crate) bundle: Vec<BundlePart>,
    pub(crate) client_crash: ClientCrash,
    pub(crate) server_crashes: ServerCrashes,
}

/// The CaveStone client's output from a run that ended in a debug crash.
pub(crate) const CLIENT_LOG: &str = include_str!("../../data/logs/client.log");
pub(crate) const CLIENT_CRASH: &str = include_str!("../../data/logs/crash-client.txt");
/// A server that stopped because a mod needs another that was left off it.
pub(crate) const UTILITY_SERVER_CRASH: &str =
    include_str!("../../data/logs/crash-utility-mods-server.txt");
/// The retained output of servers that started and stopped normally.
pub(crate) const PHOTON_SERVER_LOG: &str = include_str!("../../data/logs/photon-server.log");
pub(crate) const CAVESTONE_SERVER_LOG: &str = include_str!("../../data/logs/cavestone-server.log");

#[allow(clippy::expect_used)]
static DATA: LazyLock<Recorded> = LazyLock::new(|| {
    // The file is embedded at build time and checked by a test, so a parse
    // failure is a broken checkout rather than a runtime condition.
    serde_json::from_str(include_str!("../../data/hosts.json")).expect("recorded hosts parse")
});

#[allow(clippy::expect_used)]
static DIAGNOSTICS: LazyLock<Diagnostics> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../data/diagnostics.json"))
        .expect("recorded diagnostics parse")
});

#[allow(clippy::expect_used)]
static DIALOGS: LazyLock<Dialogs> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../data/dialogs.json")).expect("recorded dialogs parse")
});

pub(crate) fn data() -> &'static Recorded {
    &DATA
}

pub(crate) fn dialogs() -> &'static Dialogs {
    &DIALOGS
}

pub(crate) fn diagnostics() -> &'static Diagnostics {
    &DIAGNOSTICS
}

/// The recorded CaveStone mod titled `title`.
#[allow(clippy::expect_used)]
pub(crate) fn cavestone_mod(title: &str) -> &'static Mod {
    data()
        .cavestone_mods
        .iter()
        .find(|row| row.title == title)
        .expect("a recorded CaveStone mod with that title")
}

/// The jar name of the recorded CaveStone mod titled `title`.
pub(crate) fn jar(title: &str) -> String {
    cavestone_mod(title).file_name.clone()
}

/// The recorded server with `key`.
#[allow(clippy::expect_used)]
pub(crate) fn server(key: &str) -> &'static Server {
    data()
        .servers
        .iter()
        .find(|server| server.key == key)
        .expect("a recorded server with that key")
}

/// Every logo address with the digest its bytes must have, once each.
pub(crate) fn logo_sources() -> BTreeMap<String, String> {
    let recorded = data();
    let mods = recorded.cavestone_mods.iter().map(|row| &row.icon);
    let hits = recorded.mod_search.results.iter().map(|hit| &hit.icon);
    mods.chain(hits)
        .filter(|logo| !logo.url.is_empty())
        .map(|logo| (logo.url.clone(), logo.sha256.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_recorded_hosts_parse_and_name_their_logos_by_digest() {
        let recorded = data();
        assert!(!recorded.instances.is_empty() && !recorded.servers.is_empty());
        assert!(
            recorded
                .cavestone_mods
                .iter()
                .all(|row| row.icon.url.is_empty() == row.icon.sha256.is_empty())
        );
        assert!(logo_sources().values().all(|digest| digest.len() == 64));
    }

    #[test]
    fn recorded_text_holds_no_machine_paths() {
        for text in [
            CLIENT_LOG,
            CLIENT_CRASH,
            UTILITY_SERVER_CRASH,
            PHOTON_SERVER_LOG,
            CAVESTONE_SERVER_LOG,
        ] {
            assert!(!text.contains("/home/"), "a machine path is recorded");
        }
    }

    #[test]
    fn every_origin_in_the_mod_list_is_one_the_builders_know() {
        let known = ["pack", "override", "user", "dependency", "hand-placed"];
        assert!(
            data()
                .cavestone_mods
                .iter()
                .all(|row| known.contains(&row.origin.as_str()))
        );
    }
}
