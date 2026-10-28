//! Presentation metadata kept apart from host metadata: favorites, groups and
//! icons (ADR 0048).
//!
//! One launcher-level file under the data directory holds all of it. It is not
//! part of `instance.json` or the index, so a snapshot restore, a promotion, a
//! storage move or a reconciliation can never roll it back, and a diagnostic
//! bundle never carries it. Entries for hosts that no longer exist are dropped
//! by the next write.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read as _;
use std::path::{Path, PathBuf};

use ephervex_model::{GroupId, InstanceId, ServerId};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::atomic::{AtomicWriteOptions, write_bytes_atomic};
use crate::case_notes::CaseNotes;
use crate::lock::{FileGuard, GuardError};
use crate::paths::{PathError, Paths};

/// The file in the data directory.
pub const ORGANIZATION_FILE_NAME: &str = "organization.json";

/// The format version this build reads and writes (ADR 0033).
pub const ORGANIZATION_FORMAT_VERSION: u32 = 1;

/// The extension every stored icon has.
pub const ICON_FILE_EXTENSION: &str = "png";

/// The file in the data directory whose `flock` serializes updates.
const ORGANIZATION_LOCK_FILE: &str = "organization.lock";

/// How long an update waits on another process's before giving up.
const ORGANIZATION_LOCK_PATIENCE: std::time::Duration = std::time::Duration::from_secs(5);

/// The name every set-aside file starts with.
const SET_ASIDE_PREFIX: &str = "organization.unreadable-";

/// The copy of the file as it was before its latest change, restored when the
/// file itself cannot be used.
const PREVIOUS_ORGANIZATION_FILE_NAME: &str = "organization.old.json";

/// A file this process moved aside because it could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetAsideNotice {
    /// Where the unusable file now is.
    pub set_aside: PathBuf,
    /// Whether the previous copy took its place, rather than an empty
    /// organization.
    pub restored: bool,
}

/// Files set aside by this process that nobody has been told about yet.
static SET_ASIDE_NOTICES: std::sync::Mutex<Vec<SetAsideNotice>> = std::sync::Mutex::new(Vec::new());

/// The files this process moved aside since it last asked, oldest first, so a
/// frontend can tell the user whichever call did the moving.
pub fn take_set_aside_notices() -> Vec<SetAsideNotice> {
    std::mem::take(
        &mut *SET_ASIDE_NOTICES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    )
}

/// The largest organization file read. Far above any real collection, so a
/// damaged or hostile file cannot exhaust memory.
const MAX_ORGANIZATION_BYTES: u64 = 8 * 1024 * 1024;

/// The content hash that names one stored icon: 64 lowercase hex digits.
///
/// The only way to build a path inside the icon directory, so a damaged or
/// edited entry can never name a file outside it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IconId(String);

impl IconId {
    /// Wraps the SHA-256 digest of an icon's stored bytes.
    #[must_use]
    pub fn from_digest(digest: &[u8; 32]) -> Self {
        let mut hex = String::with_capacity(64);
        for byte in digest {
            hex.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0'));
            hex.push(char::from_digit(u32::from(byte & 0xf), 16).unwrap_or('0'));
        }
        Self(hex)
    }

    /// Accepts exactly the form [`Self::from_digest`] produces.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        (text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
            .then(|| Self(text.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The name of the stored PNG.
    #[must_use]
    pub fn file_name(&self) -> String {
        format!("{}.{ICON_FILE_EXTENSION}", self.0)
    }

    /// The icon a stored file name belongs to, when it is one of ours.
    fn from_file_name(name: &str) -> Option<Self> {
        Self::parse(name.strip_suffix(&format!(".{ICON_FILE_EXTENSION}"))?)
    }
}

impl std::fmt::Display for IconId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Everything the launcher remembers about how hosts are presented.
///
/// Every field is additive and defaults when absent. Fields this build does
/// not know survive a load and save.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Organization {
    pub format_version: u32,
    pub favorite_clients: BTreeSet<InstanceId>,
    pub favorite_servers: BTreeSet<ServerId>,
    /// Group names by id. A group with no members stays.
    pub groups: BTreeMap<GroupId, String>,
    /// The color slot each group was given when it was created, so a rename
    /// or another group's arrival never repaints it.
    pub group_tones: BTreeMap<GroupId, u8>,
    /// A client has at most one group; absence means ungrouped.
    pub client_groups: BTreeMap<InstanceId, GroupId>,
    /// Icon file hashes as written. Checked when resolved, so one damaged
    /// entry costs that icon and not the file.
    pub client_icons: BTreeMap<InstanceId, String>,
    pub server_icons: BTreeMap<ServerId, String>,
    /// What the user wrote down while reproducing a bug on an instance. An
    /// instance with nothing written has no entry.
    #[serde(deserialize_with = "lenient_cases")]
    pub client_cases: BTreeMap<InstanceId, CaseNotes>,
    #[serde(flatten)]
    unknown: BTreeMap<String, serde_json::Value>,
}

impl Default for Organization {
    fn default() -> Self {
        Self {
            format_version: ORGANIZATION_FORMAT_VERSION,
            favorite_clients: BTreeSet::new(),
            favorite_servers: BTreeSet::new(),
            groups: BTreeMap::new(),
            group_tones: BTreeMap::new(),
            client_groups: BTreeMap::new(),
            client_icons: BTreeMap::new(),
            server_icons: BTreeMap::new(),
            client_cases: BTreeMap::new(),
            unknown: BTreeMap::new(),
        }
    }
}

impl Organization {
    /// The group `client` is in, or `None` when it is ungrouped or its group
    /// no longer exists.
    #[must_use]
    pub fn group_of(&self, client: InstanceId) -> Option<(GroupId, &str)> {
        let id = *self.client_groups.get(&client)?;
        self.groups.get(&id).map(|name| (id, name.as_str()))
    }

    #[must_use]
    pub fn client_icon(&self, client: InstanceId) -> Option<IconId> {
        IconId::parse(self.client_icons.get(&client)?)
    }

    #[must_use]
    pub fn server_icon(&self, server: ServerId) -> Option<IconId> {
        IconId::parse(self.server_icons.get(&server)?)
    }

    /// Every icon some host still names.
    #[must_use]
    pub fn referenced_icons(&self) -> BTreeSet<IconId> {
        self.client_icons
            .values()
            .chain(self.server_icons.values())
            .filter_map(|text| IconId::parse(text))
            .collect()
    }

    /// Drops what names a host that is not in `clients` or `servers`.
    ///
    /// Group definitions stay: an empty group is the user's to delete.
    pub fn retain_hosts(&mut self, clients: &BTreeSet<InstanceId>, servers: &BTreeSet<ServerId>) {
        self.favorite_clients.retain(|id| clients.contains(id));
        self.favorite_servers.retain(|id| servers.contains(id));
        self.client_groups.retain(|id, _| clients.contains(id));
        self.client_icons.retain(|id, _| clients.contains(id));
        self.server_icons.retain(|id, _| servers.contains(id));
        self.client_cases.retain(|id, _| clients.contains(id));
    }

    /// The case written down for `client`; empty when nothing is.
    #[must_use]
    pub fn case_of(&self, client: InstanceId) -> CaseNotes {
        self.client_cases.get(&client).cloned().unwrap_or_default()
    }

    /// Changes the case of `client`. An emptied case leaves no entry.
    pub fn change_case<T>(
        &mut self,
        client: InstanceId,
        change: impl FnOnce(&mut CaseNotes) -> T,
    ) -> T {
        let mut case = self.case_of(client);
        let result = change(&mut case);
        if case.is_empty() {
            self.client_cases.remove(&client);
        } else {
            self.client_cases.insert(client, case);
        }
        result
    }

    /// Brings every case back within its bounds after a read.
    fn repair_cases(&mut self) {
        self.client_cases = std::mem::take(&mut self.client_cases)
            .into_iter()
            .map(|(client, case)| (client, case.repaired()))
            .filter(|(_, case)| !case.is_empty())
            .collect();
    }

    /// Drops assignments and color slots of groups that are not defined.
    pub fn drop_dangling_group_assignments(&mut self) {
        let groups = &self.groups;
        self.client_groups
            .retain(|_, group| groups.contains_key(group));
        self.group_tones
            .retain(|group, _| groups.contains_key(group));
    }
}

/// What a load found.
#[derive(Debug)]
pub struct OrganizationLoad {
    pub organization: Organization,
    /// Where an unreadable file was moved so it is not overwritten, when this
    /// load found one.
    pub set_aside: Option<PathBuf>,
    /// Whether the previous copy replaced the unreadable file.
    pub restored: bool,
}

/// Failures reading or writing the organization file.
#[derive(Debug, thiserror::Error)]
pub enum OrganizationError {
    #[error("could not read the organization file {path}")]
    Unreadable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not write the organization file {path}")]
    Unwritable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Another process kept the file locked for longer than an update waits.
    #[error("another process is updating the organization file ({path})")]
    Busy { path: PathBuf },
    #[error("could not serialize the organization file")]
    Serialization(#[source] serde_json::Error),
    #[error(transparent)]
    Path(#[from] PathError),
}

impl OrganizationError {
    /// The `ephervex-i18n` message id describing this failure to a user.
    #[must_use]
    pub const fn message_id(&self) -> &'static str {
        match self {
            Self::Unreadable { .. } => "error-organization-unreadable",
            Self::Unwritable { .. } | Self::Serialization(_) => "error-organization-unwritable",
            Self::Busy { .. } => "error-organization-busy",
            Self::Path(error) => error.message_id(),
        }
    }
}

fn guard_error(error: GuardError) -> OrganizationError {
    match error {
        GuardError::Busy { path } => OrganizationError::Busy { path },
        GuardError::Unusable { path, source } => OrganizationError::Unwritable { path, source },
    }
}

/// How a stored file came out of being read.
enum Stored {
    Missing,
    Valid(Box<Organization>),
    /// Present but not an organization file this build can use.
    Unusable,
}

/// Reads the cases one entry at a time, so an entry that is not a case, or
/// names no instance, costs itself and not the file.
fn lenient_cases<'de, D>(deserializer: D) -> Result<BTreeMap<InstanceId, CaseNotes>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let serde_json::Value::Object(entries) = serde_json::Value::deserialize(deserializer)? else {
        return Ok(BTreeMap::new());
    };
    Ok(entries
        .into_iter()
        .filter_map(|(client, case)| {
            Some((
                client.parse::<InstanceId>().ok()?,
                serde_json::from_value::<CaseNotes>(case).ok()?,
            ))
        })
        .collect())
}

fn read_stored(path: &Path) -> Result<Stored, OrganizationError> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Stored::Missing),
        Err(source) => {
            return Err(OrganizationError::Unreadable {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let mut bytes = Vec::new();
    file.take(MAX_ORGANIZATION_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| OrganizationError::Unreadable {
            path: path.to_path_buf(),
            source,
        })?;
    if bytes.len() as u64 > MAX_ORGANIZATION_BYTES {
        return Ok(Stored::Unusable);
    }
    Ok(match serde_json::from_slice::<Organization>(&bytes) {
        Ok(mut organization) if organization.format_version <= ORGANIZATION_FORMAT_VERSION => {
            organization.format_version = ORGANIZATION_FORMAT_VERSION;
            organization.repair_cases();
            Stored::Valid(Box::new(organization))
        }
        _ => Stored::Unusable,
    })
}

impl Organization {
    /// Reads the organization, starting from empty when there is none.
    ///
    /// A file this build cannot use is moved aside, never overwritten, and the
    /// previous copy takes its place when it is usable. Otherwise the launcher
    /// carries on with an empty organization.
    pub fn load(paths: &Paths) -> Result<OrganizationLoad, OrganizationError> {
        let path = paths.organization_file();
        match read_stored(&path)? {
            Stored::Missing => Ok(OrganizationLoad {
                organization: Self::default(),
                set_aside: None,
                restored: false,
            }),
            Stored::Valid(organization) => Ok(OrganizationLoad {
                organization: *organization,
                set_aside: None,
                restored: false,
            }),
            Stored::Unusable => {
                let _lock = take_lock(paths)?;
                Self::load_locked(paths)
            }
        }
    }

    /// [`Self::load`] for a caller that already holds the lock.
    fn load_locked(paths: &Paths) -> Result<OrganizationLoad, OrganizationError> {
        let path = paths.organization_file();
        match read_stored(&path)? {
            Stored::Missing => Ok(OrganizationLoad {
                organization: Self::default(),
                set_aside: None,
                restored: false,
            }),
            Stored::Valid(organization) => Ok(OrganizationLoad {
                organization: *organization,
                set_aside: None,
                restored: false,
            }),
            // The damaged file is copied aside before anything replaces it, so
            // a failure or crash part way leaves it in place to be found again
            // rather than leaving no file at all.
            Stored::Unusable => {
                let set_aside = set_aside(&path)?;
                let previous =
                    if let Ok(Stored::Valid(previous)) = read_stored(&previous_file(paths)) {
                        previous.write_locked(paths)?;
                        Some(*previous)
                    } else {
                        std::fs::remove_file(&path).map_err(|source| {
                            OrganizationError::Unwritable {
                                path: path.clone(),
                                source,
                            }
                        })?;
                        None
                    };
                let restored = previous.is_some();
                SET_ASIDE_NOTICES
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(SetAsideNotice {
                        set_aside: set_aside.clone(),
                        restored,
                    });
                warn!(
                    path = %path.display(),
                    set_aside = %set_aside.display(),
                    restored,
                    "the organization file was not usable and was moved aside"
                );
                Ok(OrganizationLoad {
                    organization: previous.unwrap_or_default(),
                    set_aside: Some(set_aside),
                    restored,
                })
            }
        }
    }

    /// Applies `change` to the organization on disk and writes the result.
    ///
    /// The file is reloaded under an exclusive lock first, so a change another
    /// process wrote in the meantime is kept. `change` runs under the lock and
    /// may create files in the icon directory; icons neither the result nor
    /// the previous copy names are removed. Nothing is written when `change`
    /// fails or leaves the organization as it was. Otherwise the file as it
    /// was becomes the previous copy first.
    pub fn update<T, E: From<OrganizationError>>(
        paths: &Paths,
        change: impl FnOnce(&mut Self) -> Result<T, E>,
    ) -> Result<T, E> {
        let _lock = take_lock(paths)?;
        let mut organization = Self::load_locked(paths)?.organization;
        let before = organization.clone();
        let superseded = read_previous(paths);
        let result = change(&mut organization)?;
        let exists = paths.organization_file().exists();
        let mut previous = superseded.clone();
        if organization != before || !exists {
            if exists {
                before.write_previous(paths)?;
                previous = before.referenced_icons();
            }
            organization.write_locked(paths)?;
        }
        let mut released = before.referenced_icons();
        released.extend(superseded);
        collect_unreferenced_icons(paths, &organization, &previous, &released);
        Ok(result)
    }

    /// Stores one icon's PNG under its content name, unless it is already
    /// there. Meant for the closure of [`Self::update`], so the file is never
    /// collected before the organization names it.
    pub fn store_icon(paths: &Paths, icon: &IconId, png: &[u8]) -> Result<(), OrganizationError> {
        let path = paths.icon_file(icon);
        if path.is_file() {
            return Ok(());
        }
        crate::paths::create_dir_all(&paths.icons_dir())?;
        write_bytes_atomic(&path, png, AtomicWriteOptions::private())
            .map_err(|source| OrganizationError::Unwritable { path, source })
    }

    fn write_locked(&self, paths: &Paths) -> Result<(), OrganizationError> {
        self.write_to(paths.organization_file())
    }

    fn write_previous(&self, paths: &Paths) -> Result<(), OrganizationError> {
        self.write_to(previous_file(paths))
    }

    fn write_to(&self, path: PathBuf) -> Result<(), OrganizationError> {
        let bytes = serde_json::to_vec(self).map_err(OrganizationError::Serialization)?;
        // A file the next read would refuse is never written.
        if bytes.len() as u64 > MAX_ORGANIZATION_BYTES {
            return Err(OrganizationError::Unwritable {
                path,
                source: std::io::Error::other("the organization file would be too large"),
            });
        }
        write_bytes_atomic(&path, &bytes, AtomicWriteOptions::private())
            .map_err(|source| OrganizationError::Unwritable { path, source })
    }
}

fn previous_file(paths: &Paths) -> PathBuf {
    paths.data_dir().join(PREVIOUS_ORGANIZATION_FILE_NAME)
}

fn take_lock(paths: &Paths) -> Result<FileGuard, OrganizationError> {
    crate::paths::create_dir_all(paths.data_dir())?;
    FileGuard::take(
        paths.data_dir(),
        ORGANIZATION_LOCK_FILE,
        ORGANIZATION_LOCK_PATIENCE,
    )
    .map_err(guard_error)
}

/// The icons the previous copy names, or none when it is missing or cannot be
/// used, because only a usable copy is ever restored.
fn read_previous(paths: &Paths) -> BTreeSet<IconId> {
    match read_stored(&previous_file(paths)) {
        Ok(Stored::Valid(previous)) => previous.referenced_icons(),
        _ => BTreeSet::new(),
    }
}

/// Keeps a copy of an unusable file under a name nothing else uses. The file
/// itself stays until something replaces or removes it.
fn set_aside(path: &Path) -> Result<PathBuf, OrganizationError> {
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    for attempt in 0_u32.. {
        let name = if attempt == 0 {
            format!("{SET_ASIDE_PREFIX}{stamp}.json")
        } else {
            format!("{SET_ASIDE_PREFIX}{stamp}-{attempt}.json")
        };
        let target = directory.join(name);
        if target.exists() {
            continue;
        }
        return std::fs::hard_link(path, &target)
            .or_else(|_| std::fs::copy(path, &target).map(|_| ()))
            .map(|()| target)
            .map_err(|source| OrganizationError::Unwritable {
                path: path.to_path_buf(),
                source,
            });
    }
    unreachable!("a free name is found within a bounded number of attempts")
}

/// Whether a file was set aside and may still name icons.
fn set_aside_file_exists(paths: &Paths) -> bool {
    std::fs::read_dir(paths.data_dir()).is_ok_and(|entries| {
        entries.flatten().any(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(SET_ASIDE_PREFIX))
        })
    })
}

/// Removes stored icons neither the organization nor its previous copy names.
/// Only files this module could have written are touched, and a failure only
/// leaves a file for the next write.
///
/// While a set-aside file exists it may name icons the user restores it for,
/// and what it names is not known here, so only `released`, the icons this
/// change stopped either file from naming, are removed.
fn collect_unreferenced_icons(
    paths: &Paths,
    after: &Organization,
    previous: &BTreeSet<IconId>,
    released: &BTreeSet<IconId>,
) {
    let directory = paths.icons_dir();
    let Ok(entries) = std::fs::read_dir(&directory) else {
        return;
    };
    let mut referenced = after.referenced_icons();
    referenced.extend(previous.iter().cloned());
    let cautious = set_aside_file_exists(paths);
    for entry in entries.flatten() {
        let Some(icon) = entry.file_name().to_str().and_then(IconId::from_file_name) else {
            continue;
        };
        let collectable = !referenced.contains(&icon) && (!cautious || released.contains(&icon));
        if collectable
            && let Err(error) = std::fs::remove_file(entry.path())
            && error.kind() != std::io::ErrorKind::NotFound
        {
            warn!(path = %entry.path().display(), %error, "could not remove an unused icon");
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn paths() -> (tempfile::TempDir, Paths) {
        let base = tempfile::tempdir().expect("temp dir");
        let paths = Paths::rooted_at(base.path());
        (base, paths)
    }

    fn icon(byte: u8) -> IconId {
        IconId::from_digest(&[byte; 32])
    }

    #[test]
    fn an_icon_id_is_exactly_sixty_four_lowercase_hex_digits() {
        assert_eq!(icon(0xab).as_str().len(), 64);
        assert!(IconId::parse(icon(0xab).as_str()).is_some());
        assert!(IconId::parse("../../etc/passwd").is_none());
        assert!(IconId::parse(&"A".repeat(64)).is_none());
        assert!(IconId::parse(&"a".repeat(63)).is_none());
    }

    #[test]
    fn a_missing_file_loads_as_an_empty_organization() {
        let (_base, paths) = paths();
        let loaded = Organization::load(&paths).expect("load");
        assert_eq!(loaded.organization, Organization::default());
        assert!(loaded.set_aside.is_none());
        assert!(!paths.organization_file().exists());
    }

    #[test]
    fn a_case_is_written_with_its_instance_and_dropped_with_it() {
        let (_base, paths) = paths();
        let kept = InstanceId::new();
        let gone = InstanceId::new();
        Organization::update(&paths, |organization| {
            for client in [kept, gone] {
                organization
                    .change_case(client, |case| case.add_step("Open the world"))
                    .expect("step");
                organization.change_case(client, CaseNotes::count_reproduced);
            }
            Ok::<_, OrganizationError>(())
        })
        .expect("update");

        let loaded = Organization::load(&paths).expect("load").organization;
        assert_eq!(loaded.case_of(kept).steps.len(), 1);
        assert_eq!(loaded.case_of(kept).reproduced, 1);

        Organization::update(&paths, |organization| {
            organization.retain_hosts(&BTreeSet::from([kept]), &BTreeSet::new());
            Ok::<_, OrganizationError>(())
        })
        .expect("prune");
        let loaded = Organization::load(&paths).expect("load").organization;
        assert!(loaded.case_of(gone).is_empty());
        assert!(!loaded.case_of(kept).is_empty());
    }

    #[test]
    fn a_bad_case_entry_costs_that_entry_and_not_the_file() {
        let (_base, paths) = paths();
        let client = InstanceId::new();
        std::fs::create_dir_all(paths.organization_file().parent().expect("parent")).expect("dir");
        std::fs::write(
            paths.organization_file(),
            format!(
                "{{\"format_version\":1,\"favorite_clients\":[\"{client}\"],\"client_cases\":{{\"{client}\":\"nope\",\"not-an-id\":{{\"note\":\"x\"}}}}}}"
            ),
        )
        .expect("write");

        let loaded = Organization::load(&paths).expect("load");
        assert!(loaded.set_aside.is_none(), "the file stays in use");
        assert!(loaded.organization.favorite_clients.contains(&client));
        assert!(loaded.organization.client_cases.is_empty());
    }

    #[test]
    fn an_organization_the_next_read_would_refuse_is_not_written() {
        let (_base, paths) = paths();
        let result = Organization::update(&paths, |organization| {
            let heavy = "\u{1F600}".repeat(crate::case_notes::MAX_CASE_NOTE_CHARS);
            for _ in 0..300 {
                organization
                    .change_case(InstanceId::new(), |case| case.set_note(&heavy))
                    .expect("each note is within its bound");
            }
            Ok::<_, OrganizationError>(())
        });
        assert!(matches!(result, Err(OrganizationError::Unwritable { .. })));
        assert!(!paths.organization_file().exists());
    }

    #[test]
    fn an_emptied_case_leaves_no_entry() {
        let mut organization = Organization::default();
        let client = InstanceId::new();
        let id = organization
            .change_case(client, |case| case.add_step("Step"))
            .expect("step");
        assert_eq!(organization.client_cases.len(), 1);
        organization.change_case(client, |case| case.remove_step(id));
        assert!(organization.client_cases.is_empty());
    }

    #[test]
    fn a_hand_edited_case_file_is_repaired_when_read() {
        let (_base, paths) = paths();
        let client = InstanceId::new();
        let long = "x".repeat(crate::case_notes::MAX_CASE_NOTE_CHARS + 100);
        let text = format!(
            "{{\"format_version\":1,\"client_cases\":{{\"{client}\":{{\"steps\":[{{\"id\":1,\"text\":\"ok\"}},{{\"id\":1,\"text\":\"repeat\"}},{{\"id\":2,\"text\":\"  \"}}],\"note\":\"{long}\"}}}}}}"
        );
        std::fs::create_dir_all(paths.organization_file().parent().expect("parent")).expect("dir");
        std::fs::write(paths.organization_file(), text).expect("write");

        let case = Organization::load(&paths)
            .expect("load")
            .organization
            .case_of(client);
        assert_eq!(case.steps.len(), 1);
        assert_eq!(case.steps[0].text, "ok");
        assert_eq!(
            case.note.chars().count(),
            crate::case_notes::MAX_CASE_NOTE_CHARS
        );
    }

    #[test]
    fn what_is_written_is_read_back_and_written_compactly() {
        let (_base, paths) = paths();
        let client = InstanceId::new();
        let server = ServerId::new();
        let group = GroupId::new();
        Organization::update(&paths, |organization| {
            organization.favorite_clients.insert(client);
            organization.favorite_servers.insert(server);
            organization.groups.insert(group, "Regression".to_owned());
            organization.group_tones.insert(group, 3);
            organization.client_groups.insert(client, group);
            Ok::<_, OrganizationError>(())
        })
        .expect("update");

        let loaded = Organization::load(&paths).expect("load").organization;
        assert!(loaded.favorite_clients.contains(&client));
        assert!(loaded.favorite_servers.contains(&server));
        assert_eq!(loaded.group_of(client), Some((group, "Regression")));
        assert_eq!(loaded.group_tones.get(&group), Some(&3));

        let text = std::fs::read_to_string(paths.organization_file()).expect("read");
        assert!(
            !text.contains('\n'),
            "machine-owned JSON is compact: {text}"
        );
        assert!(
            !text.contains(": "),
            "machine-owned JSON is compact: {text}"
        );
    }

    #[test]
    fn fields_this_build_does_not_know_are_tolerated_and_kept() {
        let (_base, paths) = paths();
        std::fs::create_dir_all(paths.data_dir()).expect("dir");
        std::fs::write(
            paths.organization_file(),
            r#"{"format_version":1,"future_setting":{"a":1}}"#,
        )
        .expect("write");
        let client = InstanceId::new();
        Organization::update(&paths, |organization| {
            organization.favorite_clients.insert(client);
            Ok::<_, OrganizationError>(())
        })
        .expect("update");
        let text = std::fs::read_to_string(paths.organization_file()).expect("read");
        assert!(text.contains("future_setting"), "{text}");
    }

    #[test]
    fn a_file_that_is_not_json_is_set_aside_and_never_overwritten() {
        let (_base, paths) = paths();
        std::fs::create_dir_all(paths.data_dir()).expect("dir");
        std::fs::write(paths.organization_file(), "{ not json").expect("write");

        let loaded = Organization::load(&paths).expect("a damaged file does not stop the launcher");
        assert_eq!(loaded.organization, Organization::default());
        let set_aside = loaded.set_aside.expect("reported");
        assert_eq!(
            std::fs::read_to_string(set_aside).expect("kept"),
            "{ not json"
        );
        assert!(!paths.organization_file().exists());
    }

    #[test]
    fn a_file_from_a_newer_format_is_set_aside_rather_than_misread() {
        let (_base, paths) = paths();
        std::fs::create_dir_all(paths.data_dir()).expect("dir");
        std::fs::write(paths.organization_file(), r#"{"format_version":2}"#).expect("write");
        let loaded = Organization::load(&paths).expect("load");
        assert!(loaded.set_aside.is_some());
    }

    #[test]
    fn an_update_over_a_damaged_file_keeps_the_damaged_copy() {
        let (_base, paths) = paths();
        std::fs::create_dir_all(paths.data_dir()).expect("dir");
        std::fs::write(paths.organization_file(), "garbage").expect("write");
        let client = InstanceId::new();
        Organization::update(&paths, |organization| {
            organization.favorite_clients.insert(client);
            Ok::<_, OrganizationError>(())
        })
        .expect("update");

        let kept = std::fs::read_dir(paths.data_dir())
            .expect("list")
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("organization.unreadable-")
            })
            .count();
        assert_eq!(kept, 1);
        assert!(
            Organization::load(&paths)
                .expect("load")
                .organization
                .favorite_clients
                .contains(&client)
        );
    }

    fn favorite(paths: &Paths, client: InstanceId) {
        Organization::update(paths, |organization| {
            organization.favorite_clients.insert(client);
            Ok::<_, OrganizationError>(())
        })
        .expect("update");
    }

    #[test]
    fn a_damaged_file_is_replaced_by_the_copy_from_before_its_latest_change() {
        let (_base, paths) = paths();
        let (first, second) = (InstanceId::new(), InstanceId::new());
        favorite(&paths, first);
        favorite(&paths, second);
        std::fs::write(paths.organization_file(), "{ torn").expect("damage");

        let loaded = Organization::load(&paths).expect("load");
        assert!(loaded.restored);
        assert!(loaded.organization.favorite_clients.contains(&first));
        assert!(!loaded.organization.favorite_clients.contains(&second));
        let set_aside = loaded.set_aside.expect("set aside");
        assert_eq!(std::fs::read_to_string(set_aside).unwrap(), "{ torn");
        let reread = Organization::load(&paths).expect("reload");
        assert!(
            reread.set_aside.is_none(),
            "the restored copy is the file now"
        );
        assert_eq!(reread.organization, loaded.organization);
    }

    #[test]
    fn with_no_usable_previous_copy_a_damaged_file_starts_empty() {
        let (_base, paths) = paths();
        favorite(&paths, InstanceId::new());
        favorite(&paths, InstanceId::new());
        std::fs::write(paths.organization_file(), "garbage").expect("damage");
        std::fs::write(
            paths.data_dir().join(PREVIOUS_ORGANIZATION_FILE_NAME),
            "garbage",
        )
        .expect("damage");

        let loaded = Organization::load(&paths).expect("load");
        assert!(!loaded.restored);
        assert_eq!(loaded.organization, Organization::default());
        assert!(!paths.organization_file().exists());
        assert_eq!(
            std::fs::read_to_string(loaded.set_aside.expect("set aside")).unwrap(),
            "garbage"
        );
    }

    #[test]
    fn with_a_set_aside_file_present_a_replaced_icon_still_goes_once_neither_file_names_it() {
        let (_base, paths) = paths();
        let client = InstanceId::new();
        std::fs::create_dir_all(paths.icons_dir()).expect("dir");
        std::fs::write(paths.data_dir().join("organization.unreadable-x.json"), "?")
            .expect("write");
        favorite(&paths, client);
        for byte in [1, 2] {
            std::fs::write(paths.icon_file(&icon(byte)), b"png").expect("write");
            Organization::update(&paths, |organization| {
                organization
                    .client_icons
                    .insert(client, icon(byte).to_string());
                Ok::<_, OrganizationError>(())
            })
            .expect("update");
        }
        let unknown = icon(9);
        std::fs::write(paths.icon_file(&unknown), b"png").expect("write");
        assert!(paths.icon_file(&icon(1)).exists());

        favorite(&paths, InstanceId::new());
        assert!(!paths.icon_file(&icon(1)).exists());
        assert!(paths.icon_file(&icon(2)).exists());
        assert!(
            paths.icon_file(&unknown).exists(),
            "what only a set-aside file may name stays"
        );
    }

    #[test]
    fn an_icon_the_previous_copy_names_is_kept_until_the_next_change() {
        let (_base, paths) = paths();
        let client = InstanceId::new();
        std::fs::create_dir_all(paths.icons_dir()).expect("dir");
        for byte in [1, 2] {
            std::fs::write(paths.icon_file(&icon(byte)), b"png").expect("write");
            Organization::update(&paths, |organization| {
                organization
                    .client_icons
                    .insert(client, icon(byte).to_string());
                Ok::<_, OrganizationError>(())
            })
            .expect("update");
        }
        assert!(
            paths.icon_file(&icon(1)).exists(),
            "the previous copy names it"
        );

        favorite(&paths, client);
        assert!(!paths.icon_file(&icon(1)).exists());
        assert!(paths.icon_file(&icon(2)).exists());
    }

    #[test]
    fn concurrent_updates_do_not_lose_each_other() {
        let (_base, paths) = paths();
        let ids: Vec<InstanceId> = (0..8).map(|_| InstanceId::new()).collect();
        std::thread::scope(|scope| {
            for id in &ids {
                let paths = &paths;
                scope.spawn(move || {
                    // A heavily loaded machine can outlast the lock's patience;
                    // Busy changes nothing, so the update is simply retried.
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
                    loop {
                        let result = Organization::update(paths, |organization| {
                            organization.favorite_clients.insert(*id);
                            Ok::<_, OrganizationError>(())
                        });
                        match result {
                            Err(OrganizationError::Busy { .. })
                                if std::time::Instant::now() < deadline => {}
                            other => break other.expect("update"),
                        }
                    }
                });
            }
        });
        let loaded = Organization::load(&paths).expect("load").organization;
        assert_eq!(loaded.favorite_clients.len(), ids.len());
    }

    #[test]
    fn a_failed_change_writes_nothing() {
        let (_base, paths) = paths();
        let result: Result<(), OrganizationError> = Organization::update(&paths, |organization| {
            organization.favorite_clients.insert(InstanceId::new());
            Err(OrganizationError::Busy {
                path: PathBuf::new(),
            })
        });
        assert!(result.is_err());
        assert!(!paths.organization_file().exists());
    }

    #[test]
    fn retaining_hosts_drops_stale_entries_but_keeps_empty_groups() {
        let kept = InstanceId::new();
        let gone = InstanceId::new();
        let kept_server = ServerId::new();
        let gone_server = ServerId::new();
        let group = GroupId::new();
        let mut organization = Organization::default();
        organization.groups.insert(group, "Empty later".to_owned());
        for client in [kept, gone] {
            organization.favorite_clients.insert(client);
            organization.client_groups.insert(client, group);
            organization
                .client_icons
                .insert(client, icon(1).to_string());
        }
        for server in [kept_server, gone_server] {
            organization.favorite_servers.insert(server);
            organization
                .server_icons
                .insert(server, icon(2).to_string());
        }

        organization.retain_hosts(&BTreeSet::from([kept]), &BTreeSet::from([kept_server]));

        assert_eq!(organization.favorite_clients, BTreeSet::from([kept]));
        assert_eq!(organization.favorite_servers, BTreeSet::from([kept_server]));
        assert_eq!(organization.client_groups.len(), 1);
        assert_eq!(organization.client_icons.len(), 1);
        assert_eq!(organization.server_icons.len(), 1);
        assert_eq!(organization.groups.len(), 1);
    }

    #[test]
    fn icons_nothing_names_are_removed_and_unrelated_files_are_left() {
        let (_base, paths) = paths();
        let client = InstanceId::new();
        let kept = icon(3);
        let dropped = icon(4);
        std::fs::create_dir_all(paths.icons_dir()).expect("dir");
        std::fs::write(paths.icon_file(&kept), b"png").expect("write");
        std::fs::write(paths.icon_file(&dropped), b"png").expect("write");
        std::fs::write(paths.icons_dir().join("notes.txt"), b"mine").expect("write");

        Organization::update(&paths, |organization| {
            organization.client_icons.insert(client, kept.to_string());
            Ok::<_, OrganizationError>(())
        })
        .expect("update");

        assert!(paths.icon_file(&kept).exists());
        assert!(!paths.icon_file(&dropped).exists());
        assert!(paths.icons_dir().join("notes.txt").exists());
    }

    #[test]
    fn icons_a_set_aside_file_names_are_kept_so_restoring_it_still_works() {
        let (_base, paths) = paths();
        let client = InstanceId::new();
        let icon = icon(7);
        std::fs::create_dir_all(paths.icons_dir()).expect("dir");
        std::fs::write(paths.icon_file(&icon), b"png").expect("write");
        std::fs::write(
            paths.organization_file(),
            format!(r#"{{"client_icons":{{"{client}":"{icon}"}}, broken"#),
        )
        .expect("write");

        Organization::update(&paths, |organization| {
            organization.favorite_clients.insert(InstanceId::new());
            Ok::<_, OrganizationError>(())
        })
        .expect("update");

        assert!(paths.icon_file(&icon).exists());
        assert!(
            take_set_aside_notices()
                .iter()
                .any(|notice| notice.set_aside.starts_with(paths.data_dir()))
        );
    }

    #[test]
    fn a_damaged_icon_entry_costs_that_icon_and_not_the_file() {
        let client = InstanceId::new();
        let mut organization = Organization::default();
        organization
            .client_icons
            .insert(client, "../../outside".to_owned());
        assert!(organization.client_icon(client).is_none());
        assert!(organization.referenced_icons().is_empty());
    }
}
