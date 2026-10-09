//! Keeping a linked server's entry in its client's `servers.dat` matched to
//! Ephervex's own record of it.
//!
//! A linked server exists to be connected to, and a client that already
//! knows exactly which server that is should not make the user retype the
//! address by hand. `servers.dat` is the uncompressed NBT file Minecraft's
//! client reads for its Multiplayer screen: a root compound holding a
//! `servers` list, each entry a compound with at least `name` and `ip`.
//!
//! # What this does not touch
//!
//! Every other entry in the file - including ones the user added by hand -
//! survives untouched: the whole file is read, only the one entry naming
//! this exact server is added, replaced, or removed, and the rest is written
//! back exactly as it was read. A file this module cannot parse is left
//! alone entirely rather than replaced with one that only has this entry.
//!
//! # Finding the entry again
//!
//! Minecraft rewrites each entry from its own fields when it saves the
//! server list, so the hidden id this module tags an entry with does not
//! survive a save made in game. An entry that has lost its tag is found by
//! the name and address it was last given instead. Only an entry carrying no
//! tag at all is eligible for that: one tagged for another server is never
//! taken over. Fields the game keeps on an entry, such as its icon, survive
//! an update.

use std::collections::HashMap;
use std::path::Path;

use ephervex_model::ServerId;
use fastnbt::Value;
use tracing::warn;

const SERVERS_DAT_FILE: &str = "servers.dat";
const SERVERS_KEY: &str = "servers";
const NAME_KEY: &str = "name";
const IP_KEY: &str = "ip";
/// Not part of the format Minecraft reads - an unrecognized compound field is
/// silently ignored by the game - but it is what lets a later sync find and
/// update the exact entry a rename or a port change should replace, instead
/// of leaving the old name behind as an orphaned duplicate.
const MANAGED_ID_KEY: &str = "ephervexLinkedServerId";

/// A server's name and address as a client's Multiplayer screen lists them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ListedServer {
    pub name: String,
    pub address: String,
}

/// Adds or updates the one entry representing `server` in
/// `game_dir/servers.dat`, so it shows the `current` name and address.
///
/// `previous` is what the entry showed before this change, used to find an
/// entry the game has rewritten without its tag. An untagged entry already
/// showing `current` is also taken as this server's, so repeating a sync never
/// appends a second copy.
///
/// Best-effort: nothing here is allowed to fail the caller. A missing file is
/// created; one that cannot be read or written is logged and left as it was.
pub(crate) fn sync_entry(
    game_dir: &Path,
    server: ServerId,
    current: &ListedServer,
    previous: Option<&ListedServer>,
) {
    let path = game_dir.join(SERVERS_DAT_FILE);
    let Some(mut root) = read_root(&path) else {
        return;
    };
    let mut list = take_server_list(&mut root);
    let id = server.to_string();

    let candidates: Vec<&ListedServer> = previous.into_iter().chain([current]).collect();
    let (index, mut fields) = match locate(&list, &id, &candidates) {
        Some(index) => match list.remove(index) {
            Value::Compound(fields) => (index, fields),
            _ => (index, HashMap::new()),
        },
        None => (list.len(), HashMap::new()),
    };
    fill_entry(&mut fields, &id, current);
    list.insert(index, Value::Compound(fields));

    root.insert(SERVERS_KEY.to_owned(), Value::List(list));
    write_root(&path, &root);
}

/// Removes `server`'s entry from `game_dir/servers.dat`, if it has one.
///
/// An entry that has lost its tag is removed by the `listed` name and address
/// it showed, one entry at most.
///
/// Best-effort on the same terms as [`sync_entry`]. A file that never had an
/// entry for this server, or does not exist at all, is left untouched rather
/// than rewritten for no reason.
pub(crate) fn remove_entry(game_dir: &Path, server: ServerId, listed: &ListedServer) {
    let path = game_dir.join(SERVERS_DAT_FILE);
    if !path.exists() {
        return;
    }
    let Some(mut root) = read_root(&path) else {
        return;
    };
    let mut list = take_server_list(&mut root);
    let id = server.to_string();
    let before = list.len();
    list.retain(|item| managed_id(item).as_deref() != Some(id.as_str()));
    if list.len() == before {
        let Some(index) = locate_untagged(&list, &[listed]) else {
            return;
        };
        list.remove(index);
    }
    root.insert(SERVERS_KEY.to_owned(), Value::List(list));
    write_root(&path, &root);
}

fn fill_entry(entry: &mut HashMap<String, Value>, id: &str, listed: &ListedServer) {
    entry.insert(NAME_KEY.to_owned(), Value::String(listed.name.clone()));
    entry.insert(IP_KEY.to_owned(), Value::String(listed.address.clone()));
    entry.insert(MANAGED_ID_KEY.to_owned(), Value::String(id.to_owned()));
}

/// The entry tagged with `id`, else the first untagged one showing one of
/// `candidates`, tried in order.
fn locate(list: &[Value], id: &str, candidates: &[&ListedServer]) -> Option<usize> {
    list.iter()
        .position(|item| managed_id(item).as_deref() == Some(id))
        .or_else(|| locate_untagged(list, candidates))
}

fn locate_untagged(list: &[Value], candidates: &[&ListedServer]) -> Option<usize> {
    candidates.iter().find_map(|listed| {
        list.iter().position(|item| {
            let Value::Compound(fields) = item else {
                return false;
            };
            !fields.contains_key(MANAGED_ID_KEY)
                && fields.get(NAME_KEY) == Some(&Value::String(listed.name.clone()))
                && fields.get(IP_KEY) == Some(&Value::String(listed.address.clone()))
        })
    })
}

fn managed_id(entry: &Value) -> Option<String> {
    match entry {
        Value::Compound(fields) => match fields.get(MANAGED_ID_KEY) {
            Some(Value::String(id)) => Some(id.clone()),
            _ => None,
        },
        _ => None,
    }
}

fn take_server_list(root: &mut HashMap<String, Value>) -> Vec<Value> {
    match root.remove(SERVERS_KEY) {
        Some(Value::List(list)) => list,
        _ => Vec::new(),
    }
}

/// The parsed root compound, or an empty one for a file that does not exist
/// yet. `None` for a file that exists but could not be read or understood -
/// the caller must not write anything back in that case, or it would replace
/// content this module failed to make sense of.
fn read_root(path: &Path) -> Option<HashMap<String, Value>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Some(HashMap::new()),
        Err(error) => {
            warn!(path = %path.display(), %error, "could not read servers.dat; leaving it untouched");
            return None;
        }
    };
    match fastnbt::from_bytes(&bytes) {
        Ok(root) => Some(root),
        Err(error) => {
            warn!(path = %path.display(), %error, "could not parse servers.dat; leaving it untouched");
            None
        }
    }
}

/// Writes `root` back atomically: a `.tmp` sibling written in full, then
/// renamed over the real file, so a crash mid-write cannot leave Minecraft's
/// client reading a half-written `servers.dat`.
fn write_root(path: &Path, root: &HashMap<String, Value>) {
    let bytes = match fastnbt::to_bytes(root) {
        Ok(bytes) => bytes,
        Err(error) => {
            warn!(path = %path.display(), %error, "could not encode servers.dat");
            return;
        }
    };
    if let Some(parent) = path.parent()
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        warn!(path = %parent.display(), %error, "could not create the game directory for servers.dat");
        return;
    }
    let temporary = path.with_extension("dat.tmp");
    if let Err(error) = std::fs::write(&temporary, &bytes) {
        warn!(path = %temporary.display(), %error, "could not write servers.dat");
        return;
    }
    if let Err(error) = std::fs::rename(&temporary, path) {
        warn!(path = %path.display(), %error, "could not publish servers.dat");
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn sample_id() -> ServerId {
        ServerId::new()
    }

    fn listed(name: &str, address: &str) -> ListedServer {
        ListedServer {
            name: name.to_owned(),
            address: address.to_owned(),
        }
    }

    fn text(value: &str) -> Value {
        Value::String(value.to_owned())
    }

    /// An entry as Minecraft saves it: only the fields the game itself
    /// knows, so no managed id.
    fn game_entry(name: &str, address: &str) -> Value {
        let mut fields = HashMap::new();
        fields.insert(NAME_KEY.to_owned(), text(name));
        fields.insert(IP_KEY.to_owned(), text(address));
        Value::Compound(fields)
    }

    fn write_entries(dir: &Path, entries: Vec<Value>) {
        let mut root = HashMap::new();
        root.insert(SERVERS_KEY.to_owned(), Value::List(entries));
        std::fs::write(
            dir.join(SERVERS_DAT_FILE),
            fastnbt::to_bytes(&root).unwrap(),
        )
        .expect("seed file");
    }

    fn read_entries(dir: &Path) -> Vec<HashMap<String, Value>> {
        let bytes = std::fs::read(dir.join(SERVERS_DAT_FILE)).expect("file written");
        let root: HashMap<String, Value> = fastnbt::from_bytes(&bytes).expect("valid nbt");
        let Some(Value::List(servers)) = root.get(SERVERS_KEY) else {
            panic!("no servers list");
        };
        servers
            .iter()
            .map(|entry| match entry {
                Value::Compound(fields) => fields.clone(),
                _ => panic!("not a compound"),
            })
            .collect()
    }

    #[test]
    fn a_fresh_directory_gets_a_servers_dat_with_one_entry() {
        let dir = tempfile::tempdir().expect("temp dir");
        let id = sample_id();
        sync_entry(
            dir.path(),
            id,
            &listed("Repro Server", "127.0.0.1:25565"),
            None,
        );

        let entries = read_entries(dir.path());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].get(NAME_KEY), Some(&text("Repro Server")));
        assert_eq!(entries[0].get(IP_KEY), Some(&text("127.0.0.1:25565")));
    }

    #[test]
    fn a_rename_updates_the_entry_in_place_rather_than_duplicating_it() {
        let dir = tempfile::tempdir().expect("temp dir");
        let id = sample_id();
        let old = listed("Old Name", "127.0.0.1:25565");
        sync_entry(dir.path(), id, &old, None);
        sync_entry(
            dir.path(),
            id,
            &listed("New Name", "127.0.0.1:25566"),
            Some(&old),
        );

        let entries = read_entries(dir.path());
        assert_eq!(
            entries.len(),
            1,
            "the rename replaces the entry, not adds a second one"
        );
        assert_eq!(entries[0].get(NAME_KEY), Some(&text("New Name")));
        assert_eq!(entries[0].get(IP_KEY), Some(&text("127.0.0.1:25566")));
    }

    #[test]
    fn a_hand_added_server_survives_a_sync_and_a_removal() {
        let dir = tempfile::tempdir().expect("temp dir");
        write_entries(
            dir.path(),
            vec![game_entry("Friend's Realm", "play.example.com")],
        );

        let id = sample_id();
        let managed = listed("Repro Server", "127.0.0.1:25565");
        sync_entry(dir.path(), id, &managed, None);
        assert_eq!(
            read_entries(dir.path()).len(),
            2,
            "the hand-added entry must still be there"
        );

        remove_entry(dir.path(), id, &managed);
        let entries = read_entries(dir.path());
        assert_eq!(entries.len(), 1, "only the managed entry is removed");
        assert_eq!(entries[0].get(NAME_KEY), Some(&text("Friend's Realm")));
    }

    #[test]
    fn removing_from_a_missing_file_does_nothing() {
        let dir = tempfile::tempdir().expect("temp dir");
        remove_entry(dir.path(), sample_id(), &listed("Server", "127.0.0.1"));
        assert!(!dir.path().join(SERVERS_DAT_FILE).exists());
    }

    #[test]
    fn an_unparsable_file_is_left_untouched() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join(SERVERS_DAT_FILE);
        std::fs::write(&path, b"not nbt at all").expect("seed a garbage file");

        sync_entry(
            dir.path(),
            sample_id(),
            &listed("Repro Server", "127.0.0.1:25565"),
            None,
        );

        let bytes = std::fs::read(&path).expect("file still there");
        assert_eq!(
            bytes, b"not nbt at all",
            "the unreadable file must not be overwritten"
        );
    }

    #[test]
    fn a_rename_finds_an_entry_the_game_rewrote_without_its_tag() {
        let dir = tempfile::tempdir().expect("temp dir");
        let old = listed("Old Name", "127.0.0.1:25565");
        write_entries(
            dir.path(),
            vec![
                game_entry("Friend's Realm", "play.example.com"),
                game_entry(&old.name, &old.address),
            ],
        );

        sync_entry(
            dir.path(),
            sample_id(),
            &listed("New Name", "127.0.0.1:25565"),
            Some(&old),
        );

        let entries = read_entries(dir.path());
        assert_eq!(entries.len(), 2, "no duplicate and no orphan");
        assert_eq!(entries[0].get(NAME_KEY), Some(&text("Friend's Realm")));
        assert_eq!(entries[1].get(NAME_KEY), Some(&text("New Name")));
    }

    #[test]
    fn a_port_change_finds_an_entry_the_game_rewrote_without_its_tag() {
        let dir = tempfile::tempdir().expect("temp dir");
        let old = listed("Repro Server", "127.0.0.1:25565");
        write_entries(dir.path(), vec![game_entry(&old.name, &old.address)]);

        sync_entry(
            dir.path(),
            sample_id(),
            &listed("Repro Server", "127.0.0.1:25570"),
            Some(&old),
        );

        let entries = read_entries(dir.path());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].get(IP_KEY), Some(&text("127.0.0.1:25570")));
    }

    #[test]
    fn a_repeated_sync_does_not_duplicate_an_untagged_entry_already_up_to_date() {
        let dir = tempfile::tempdir().expect("temp dir");
        let current = listed("Repro Server", "127.0.0.1:25565");
        write_entries(
            dir.path(),
            vec![game_entry(&current.name, &current.address)],
        );

        sync_entry(dir.path(), sample_id(), &current, None);

        assert_eq!(read_entries(dir.path()).len(), 1);
    }

    #[test]
    fn removal_deletes_an_untagged_entry_by_its_name_and_address() {
        let dir = tempfile::tempdir().expect("temp dir");
        let managed = listed("Repro Server", "127.0.0.1:25565");
        write_entries(
            dir.path(),
            vec![
                game_entry("Friend's Realm", "play.example.com"),
                game_entry(&managed.name, &managed.address),
            ],
        );

        remove_entry(dir.path(), sample_id(), &managed);

        let entries = read_entries(dir.path());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].get(NAME_KEY), Some(&text("Friend's Realm")));
    }

    #[test]
    fn removal_deletes_at_most_one_untagged_entry_with_the_same_name_and_address() {
        let dir = tempfile::tempdir().expect("temp dir");
        let managed = listed("Repro Server", "127.0.0.1:25565");
        write_entries(
            dir.path(),
            vec![
                game_entry(&managed.name, &managed.address),
                game_entry(&managed.name, &managed.address),
            ],
        );

        remove_entry(dir.path(), sample_id(), &managed);

        assert_eq!(read_entries(dir.path()).len(), 1);
    }

    #[test]
    fn an_entry_tagged_for_another_server_is_never_taken_over_or_removed() {
        let dir = tempfile::tempdir().expect("temp dir");
        let shared = listed("Repro Server", "127.0.0.1:25565");
        let other = sample_id();
        sync_entry(dir.path(), other, &shared, None);

        let mine = sample_id();
        sync_entry(dir.path(), mine, &shared, Some(&shared));
        assert_eq!(
            read_entries(dir.path()).len(),
            2,
            "each server keeps its own entry"
        );

        remove_entry(dir.path(), mine, &shared);
        let entries = read_entries(dir.path());
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].get(MANAGED_ID_KEY),
            Some(&text(&other.to_string()))
        );
    }

    #[test]
    fn fields_the_game_keeps_on_an_entry_survive_an_update() {
        let dir = tempfile::tempdir().expect("temp dir");
        let old = listed("Repro Server", "127.0.0.1:25565");
        let Value::Compound(mut fields) = game_entry(&old.name, &old.address) else {
            panic!("not a compound");
        };
        fields.insert("icon".to_owned(), text("base64-icon"));
        fields.insert("acceptTextures".to_owned(), Value::Byte(1));
        write_entries(dir.path(), vec![Value::Compound(fields)]);

        sync_entry(
            dir.path(),
            sample_id(),
            &listed("Renamed", "127.0.0.1:25565"),
            Some(&old),
        );

        let entries = read_entries(dir.path());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].get(NAME_KEY), Some(&text("Renamed")));
        assert_eq!(entries[0].get("icon"), Some(&text("base64-icon")));
        assert_eq!(entries[0].get("acceptTextures"), Some(&Value::Byte(1)));
    }
}
