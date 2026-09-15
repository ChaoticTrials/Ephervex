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

/// Adds or updates the one entry representing `server` in
/// `game_dir/servers.dat`, so it shows the current name and address.
///
/// Best-effort: nothing here is allowed to fail the caller. A missing file is
/// created; one that cannot be read or written is logged and left as it was.
pub(crate) fn sync_entry(game_dir: &Path, server: ServerId, name: &str, address: &str) {
    let path = game_dir.join(SERVERS_DAT_FILE);
    let Some(mut root) = read_root(&path) else {
        return;
    };
    let mut list = take_server_list(&mut root);
    let id = server.to_string();

    let mut entry = HashMap::new();
    entry.insert(NAME_KEY.to_owned(), Value::String(name.to_owned()));
    entry.insert(IP_KEY.to_owned(), Value::String(address.to_owned()));
    entry.insert(MANAGED_ID_KEY.to_owned(), Value::String(id.clone()));

    match list
        .iter()
        .position(|item| managed_id(item).as_deref() == Some(id.as_str()))
    {
        Some(index) => list[index] = Value::Compound(entry),
        None => list.push(Value::Compound(entry)),
    }

    root.insert(SERVERS_KEY.to_owned(), Value::List(list));
    write_root(&path, &root);
}

/// Removes `server`'s entry from `game_dir/servers.dat`, if it has one.
///
/// Best-effort on the same terms as [`sync_entry`]. A file that never had an
/// entry for this server, or does not exist at all, is left untouched rather
/// than rewritten for no reason.
pub(crate) fn remove_entry(game_dir: &Path, server: ServerId) {
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
        return;
    }
    root.insert(SERVERS_KEY.to_owned(), Value::List(list));
    write_root(&path, &root);
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

    #[test]
    fn a_fresh_directory_gets_a_servers_dat_with_one_entry() {
        let dir = tempfile::tempdir().expect("temp dir");
        let id = sample_id();
        sync_entry(dir.path(), id, "Repro Server", "127.0.0.1:25565");

        let bytes = std::fs::read(dir.path().join(SERVERS_DAT_FILE)).expect("file written");
        let root: HashMap<String, Value> = fastnbt::from_bytes(&bytes).expect("valid nbt");
        let Some(Value::List(servers)) = root.get(SERVERS_KEY) else {
            panic!("no servers list");
        };
        assert_eq!(servers.len(), 1);
        let Value::Compound(entry) = &servers[0] else {
            panic!("not a compound");
        };
        assert_eq!(
            entry.get(NAME_KEY),
            Some(&Value::String("Repro Server".to_owned()))
        );
        assert_eq!(
            entry.get(IP_KEY),
            Some(&Value::String("127.0.0.1:25565".to_owned()))
        );
    }

    #[test]
    fn a_rename_updates_the_entry_in_place_rather_than_duplicating_it() {
        let dir = tempfile::tempdir().expect("temp dir");
        let id = sample_id();
        sync_entry(dir.path(), id, "Old Name", "127.0.0.1:25565");
        sync_entry(dir.path(), id, "New Name", "127.0.0.1:25566");

        let bytes = std::fs::read(dir.path().join(SERVERS_DAT_FILE)).expect("file written");
        let root: HashMap<String, Value> = fastnbt::from_bytes(&bytes).expect("valid nbt");
        let Some(Value::List(servers)) = root.get(SERVERS_KEY) else {
            panic!("no servers list");
        };
        assert_eq!(
            servers.len(),
            1,
            "the rename replaces the entry, not adds a second one"
        );
        let Value::Compound(entry) = &servers[0] else {
            panic!("not a compound");
        };
        assert_eq!(
            entry.get(NAME_KEY),
            Some(&Value::String("New Name".to_owned()))
        );
        assert_eq!(
            entry.get(IP_KEY),
            Some(&Value::String("127.0.0.1:25566".to_owned()))
        );
    }

    #[test]
    fn a_hand_added_server_survives_a_sync_and_a_removal() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut hand_added = HashMap::new();
        hand_added.insert(
            NAME_KEY.to_owned(),
            Value::String("Friend's Realm".to_owned()),
        );
        hand_added.insert(
            IP_KEY.to_owned(),
            Value::String("play.example.com".to_owned()),
        );
        let mut root = HashMap::new();
        root.insert(
            SERVERS_KEY.to_owned(),
            Value::List(vec![Value::Compound(hand_added)]),
        );
        std::fs::write(
            dir.path().join(SERVERS_DAT_FILE),
            fastnbt::to_bytes(&root).unwrap(),
        )
        .expect("seed file");

        let id = sample_id();
        sync_entry(dir.path(), id, "Repro Server", "127.0.0.1:25565");

        let bytes = std::fs::read(dir.path().join(SERVERS_DAT_FILE)).expect("file written");
        let root: HashMap<String, Value> = fastnbt::from_bytes(&bytes).expect("valid nbt");
        let Some(Value::List(servers)) = root.get(SERVERS_KEY) else {
            panic!("no servers list");
        };
        assert_eq!(servers.len(), 2, "the hand-added entry must still be there");

        remove_entry(dir.path(), id);
        let bytes = std::fs::read(dir.path().join(SERVERS_DAT_FILE)).expect("file written");
        let root: HashMap<String, Value> = fastnbt::from_bytes(&bytes).expect("valid nbt");
        let Some(Value::List(servers)) = root.get(SERVERS_KEY) else {
            panic!("no servers list");
        };
        assert_eq!(servers.len(), 1, "only the managed entry is removed");
        let Value::Compound(remaining) = &servers[0] else {
            panic!("not a compound");
        };
        assert_eq!(
            remaining.get(NAME_KEY),
            Some(&Value::String("Friend's Realm".to_owned()))
        );
    }

    #[test]
    fn removing_from_a_missing_file_does_nothing() {
        let dir = tempfile::tempdir().expect("temp dir");
        remove_entry(dir.path(), sample_id());
        assert!(!dir.path().join(SERVERS_DAT_FILE).exists());
    }

    #[test]
    fn an_unparsable_file_is_left_untouched() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join(SERVERS_DAT_FILE);
        std::fs::write(&path, b"not nbt at all").expect("seed a garbage file");

        sync_entry(dir.path(), sample_id(), "Repro Server", "127.0.0.1:25565");

        let bytes = std::fs::read(&path).expect("file still there");
        assert_eq!(
            bytes, b"not nbt at all",
            "the unreadable file must not be overwritten"
        );
    }
}
