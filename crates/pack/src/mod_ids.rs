//! The mod ids a jar declares, and the refusal to give a loader two jars that
//! declare the same one.
//!
//! Two catalogs publish one mod under unrelated project ids, and often at
//! different releases, so nothing in a provider identity says that a jar from
//! one catalog is the mod a jar from the other already provides. The jar does:
//! every loader identifies a mod by the id in its metadata, and refuses to
//! start when two jars in `mods/` declare the same one. That id is therefore
//! what an install checks before it writes.
//!
//! Jars are untrusted input. Only the metadata entries are read, into memory,
//! under a small size ceiling, and nothing is extracted. A jar that cannot be
//! read declares nothing: the loader would not read it either.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use ephervex_model::{ModOrigin, ModProvider};
use tracing::debug;

use crate::archive::{ExtractionLimits, PackArchive};
use crate::blocking::off_runtime;
use crate::error::{PackError, PackResult};
use crate::managed::InstalledStandIn;
use crate::recipe::PackRecipe;

/// Where Fabric reads a mod's metadata. Quilt reads it as well.
const FABRIC_METADATA: &str = "fabric.mod.json";
/// Where Quilt reads a mod's own metadata.
const QUILT_METADATA: &str = "quilt.mod.json";
/// Where Forge, and NeoForge before 20.5, read a mod's metadata.
const FORGE_METADATA: &str = "META-INF/mods.toml";
/// Where NeoForge from 20.5 on reads a mod's metadata.
const NEOFORGE_METADATA: &str = "META-INF/neoforge.mods.toml";

/// The longest mod id any of these loaders accepts.
const MAX_MOD_ID_LEN: usize = 64;

/// The loaders that share one namespace of mod ids.
///
/// Quilt loads Fabric metadata, and NeoForge began as Forge and kept its
/// format, so two jars clash only when one family would load both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum LoaderFamily {
    Fabric,
    Forge,
}

/// One mod id as a loader family reads it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct DeclaredModId {
    family: LoaderFamily,
    id: String,
}

/// A jar a change is about to write.
#[derive(Debug, Clone)]
pub(crate) struct IncomingJar {
    /// Where the jar will be written, relative to the game directory.
    pub(crate) logical_path: PathBuf,
    /// The verified artifact that will be copied there.
    pub(crate) staged: PathBuf,
}

/// Refuses a change that would leave the loader two jars declaring one mod id.
///
/// `incoming` are the jars about to be written enabled. `leaving` are the
/// logical paths whose jars the change moves out of the game directory first,
/// so they are not counted against it. Every enabled `.jar` already beside an
/// incoming one is, whoever put it there: a jar from the other catalog, the
/// pack's, or one added by hand all make the game refuse to start.
pub(crate) async fn require_distinct_mod_ids(
    recipe: &PackRecipe,
    incoming: Vec<IncomingJar>,
    game_dir: &Path,
    leaving: BTreeSet<PathBuf>,
) -> PackResult<()> {
    settle_mod_ids(recipe, incoming, game_dir, leaving, BTreeSet::new())
        .await
        .map(|_| ())
}

/// [`require_distinct_mod_ids`], except that an incoming jar named in
/// `may_stand_in` is left out rather than refused when one enabled jar already
/// beside it declares every mod id it does.
///
/// Every id, from one jar: a jar declaring only some of them would leave the
/// rest missing. What is left out is returned, and must not be written.
pub(crate) async fn settle_mod_ids(
    recipe: &PackRecipe,
    incoming: Vec<IncomingJar>,
    game_dir: &Path,
    leaving: BTreeSet<PathBuf>,
    may_stand_in: BTreeSet<PathBuf>,
) -> PackResult<Vec<InstalledStandIn>> {
    if incoming.is_empty() {
        return Ok(Vec::new());
    }
    let game_dir = game_dir.to_path_buf();
    let judged =
        off_runtime(move || Ok(judge(&incoming, &game_dir, &leaving, &may_stand_in))).await?;
    judged.map_err(|clash| match clash {
        Clash::WithinChange {
            mod_id,
            first,
            second,
        } => PackError::DuplicateModIdInChange {
            mod_id,
            first: first.to_string_lossy().into_owned(),
            second: second.to_string_lossy().into_owned(),
        },
        Clash::WithExisting {
            mod_id,
            incoming,
            existing,
            existing_path,
            could_stand_in,
        } => PackError::DuplicateModId {
            mod_id,
            incoming: incoming.to_string_lossy().into_owned(),
            provider: recorded_provider(recipe, &existing),
            path: existing_path,
            could_stand_in,
        },
    })
}

/// The catalog the recipe says installed the jar at `logical`, if one did.
///
/// A jar added by hand is the user's, whatever a catalog later recognized it
/// as, so it names no catalog.
fn recorded_provider(recipe: &PackRecipe, logical: &Path) -> Option<ModProvider> {
    recipe
        .files
        .iter()
        .find(|file| file.logical_path == logical && file.origin != ModOrigin::HandPlaced)
        .and_then(|file| file.identity.as_ref())
        .and_then(|identity| identity.provider)
}

#[derive(Debug)]
enum Clash {
    WithinChange {
        mod_id: String,
        first: PathBuf,
        second: PathBuf,
    },
    WithExisting {
        mod_id: String,
        incoming: PathBuf,
        /// Relative to the game directory.
        existing: PathBuf,
        existing_path: PathBuf,
        /// Whether `existing` declares every mod id `incoming` does.
        could_stand_in: bool,
    },
}

/// The incoming jars an installed jar stands in for, or the first clash that
/// nothing may stand in for.
fn judge(
    incoming: &[IncomingJar],
    game_dir: &Path,
    leaving: &BTreeSet<PathBuf>,
    may_stand_in: &BTreeSet<PathBuf>,
) -> Result<Vec<InstalledStandIn>, Clash> {
    let declared: Vec<BTreeSet<DeclaredModId>> = incoming
        .iter()
        .map(|jar| declared_mod_ids(&jar.staged))
        .collect();
    let mut claimed: BTreeMap<&DeclaredModId, &Path> = BTreeMap::new();
    for (jar, ids) in incoming.iter().zip(&declared) {
        for id in ids {
            if let Some(first) = claimed.get(id)
                && *first != jar.logical_path
            {
                return Err(Clash::WithinChange {
                    mod_id: id.id.clone(),
                    first: first.to_path_buf(),
                    second: jar.logical_path.clone(),
                });
            }
            claimed.insert(id, &jar.logical_path);
        }
    }
    if claimed.is_empty() {
        return Ok(Vec::new());
    }

    let written: BTreeSet<&Path> = incoming
        .iter()
        .map(|jar| jar.logical_path.as_path())
        .collect();
    let folders: BTreeSet<&Path> = incoming
        .iter()
        .filter_map(|jar| jar.logical_path.parent())
        .collect();
    let existing: Vec<(PathBuf, BTreeSet<DeclaredModId>)> = folders
        .into_iter()
        .flat_map(|folder| enabled_jars(game_dir, folder))
        .filter(|existing| !written.contains(existing.as_path()) && !leaving.contains(existing))
        .map(|existing| {
            let provided = declared_mod_ids(&game_dir.join(&existing));
            (existing, provided)
        })
        .collect();

    let mut stood_in = Vec::new();
    for (jar, ids) in incoming.iter().zip(&declared) {
        let mut clashing = existing
            .iter()
            .filter(|(_, provided)| !provided.is_disjoint(ids));
        let Some(first) = clashing.next() else {
            continue;
        };
        let covering = std::iter::once(first)
            .chain(clashing)
            .find(|(_, provided)| ids.is_subset(provided));
        match covering {
            Some((installed, _)) if may_stand_in.contains(&jar.logical_path) => {
                stood_in.push(InstalledStandIn {
                    planned: jar.logical_path.clone(),
                    installed: installed.clone(),
                    mod_ids: ids.iter().map(|id| id.id.clone()).collect(),
                });
            }
            _ => {
                let (existing, provided) = covering.unwrap_or(first);
                let mod_id = ids
                    .intersection(provided)
                    .next()
                    .map(|id| id.id.clone())
                    .unwrap_or_default();
                return Err(Clash::WithExisting {
                    mod_id,
                    incoming: jar.logical_path.clone(),
                    existing: existing.clone(),
                    existing_path: game_dir.join(existing),
                    could_stand_in: covering.is_some(),
                });
            }
        }
    }
    Ok(stood_in)
}

/// The jars a loader would read from one folder of the game directory, as
/// logical paths, in name order.
///
/// Only the `.jar` spelling: a disabled jar is invisible to the loader. A link
/// is followed, because the loader follows it too.
fn enabled_jars(game_dir: &Path, folder: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(game_dir.join(folder)) else {
        return Vec::new();
    };
    let mut jars: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .and_then(|name| Path::new(name).extension())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("jar"))
        })
        .filter(|entry| std::fs::metadata(entry.path()).is_ok_and(|meta| meta.is_file()))
        .map(|entry| folder.join(entry.file_name()))
        .collect();
    jars.sort();
    jars
}

/// What reading one jar's metadata may cost.
///
/// The central directory of a large mod runs to tens of thousands of entries,
/// so the entry count is generous; the metadata files themselves are a few
/// kilobytes, so what is read into memory is not.
fn metadata_limits() -> ExtractionLimits {
    ExtractionLimits {
        max_entries: 200_000,
        max_total_bytes: 4 * 1024 * 1024,
        max_entry_bytes: 1024 * 1024,
        max_manifest_bytes: 1024 * 1024,
        ..ExtractionLimits::default()
    }
}

/// Every mod id the jar at `path` declares, for every loader family whose
/// metadata it carries.
fn declared_mod_ids(path: &Path) -> BTreeSet<DeclaredModId> {
    let mut declared = BTreeSet::new();
    let mut archive = match PackArchive::open(path, metadata_limits()) {
        Ok(archive) => archive,
        Err(error) => {
            debug!(path = %path.display(), %error, "jar not readable for mod ids");
            return declared;
        }
    };
    let mut read = |entry: &str| match archive.read_root_entry(entry) {
        Ok(bytes) => bytes,
        Err(error) => {
            debug!(path = %path.display(), entry, %error, "jar metadata not readable");
            None
        }
    };
    let fabric = read(FABRIC_METADATA);
    let quilt = read(QUILT_METADATA);
    let forge = read(FORGE_METADATA);
    let neoforge = read(NEOFORGE_METADATA);

    let ids = [
        (LoaderFamily::Fabric, fabric.as_deref().and_then(fabric_id)),
        (LoaderFamily::Fabric, quilt.as_deref().and_then(quilt_id)),
    ]
    .into_iter()
    .filter_map(|(family, id)| id.map(|id| (family, id)))
    .chain(
        [forge, neoforge]
            .into_iter()
            .flatten()
            .flat_map(|bytes| forge_ids(&bytes))
            .map(|id| (LoaderFamily::Forge, id)),
    );
    for (family, id) in ids {
        if is_plausible_mod_id(&id) {
            declared.insert(DeclaredModId { family, id });
        }
    }
    declared
}

/// Rejects what no loader would accept as an id, including an unexpanded
/// build-time placeholder such as `${mod_id}`.
fn is_plausible_mod_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_MOD_ID_LEN
        && !id.contains("${")
        && !id.chars().any(char::is_whitespace)
}

fn json(bytes: &[u8]) -> Option<serde_json::Value> {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    serde_json::from_slice(bytes).ok()
}

/// The `id` of a `fabric.mod.json`.
fn fabric_id(bytes: &[u8]) -> Option<String> {
    json(bytes)?.get("id")?.as_str().map(ToOwned::to_owned)
}

/// The `quilt_loader.id` of a `quilt.mod.json`.
fn quilt_id(bytes: &[u8]) -> Option<String> {
    json(bytes)?
        .get("quilt_loader")?
        .get("id")?
        .as_str()
        .map(ToOwned::to_owned)
}

/// The `modId` of every `[[mods]]` table in a `mods.toml`.
fn forge_ids(bytes: &[u8]) -> Vec<String> {
    let Some(document) = std::str::from_utf8(bytes)
        .ok()
        .map(|text| text.strip_prefix('\u{feff}').unwrap_or(text))
        .and_then(|text| text.parse::<toml::Table>().ok())
    else {
        return Vec::new();
    };
    document
        .get("mods")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|table| table.get("modId")?.as_str().map(ToOwned::to_owned))
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use std::io::Write;

    use super::*;

    fn jar(path: &Path, entries: &[(&str, &str)]) {
        let file = std::fs::File::create(path).expect("create");
        let mut zip = zip::ZipWriter::new(file);
        for (name, body) in entries {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .expect("entry");
            zip.write_all(body.as_bytes()).expect("write");
        }
        zip.finish().expect("finish");
    }

    fn ids_of(entries: &[(&str, &str)]) -> Vec<(LoaderFamily, String)> {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("a.jar");
        jar(&path, entries);
        declared_mod_ids(&path)
            .into_iter()
            .map(|declared| (declared.family, declared.id))
            .collect()
    }

    #[test]
    fn a_fabric_jar_declares_its_id() {
        assert_eq!(
            ids_of(&[(
                FABRIC_METADATA,
                r#"{"schemaVersion":1,"id":"libx","version":"1"}"#
            )]),
            vec![(LoaderFamily::Fabric, "libx".to_owned())]
        );
    }

    #[test]
    fn a_quilt_jar_declares_its_id_in_the_fabric_namespace() {
        assert_eq!(
            ids_of(&[(QUILT_METADATA, r#"{"quilt_loader":{"id":"libx"}}"#)]),
            vec![(LoaderFamily::Fabric, "libx".to_owned())]
        );
    }

    #[test]
    fn a_forge_jar_declares_every_mod_it_carries() {
        let toml =
            "modLoader=\"javafml\"\n[[mods]]\nmodId=\"libx\"\n[[mods]]\nmodId=\"libx_extra\"\n";
        assert_eq!(
            ids_of(&[(FORGE_METADATA, toml)]),
            vec![
                (LoaderFamily::Forge, "libx".to_owned()),
                (LoaderFamily::Forge, "libx_extra".to_owned()),
            ]
        );
    }

    #[test]
    fn a_neoforge_jar_shares_the_forge_namespace() {
        assert_eq!(
            ids_of(&[(NEOFORGE_METADATA, "[[mods]]\nmodId=\"libx\"\n")]),
            vec![(LoaderFamily::Forge, "libx".to_owned())]
        );
    }

    #[test]
    fn an_unexpanded_placeholder_is_not_an_id() {
        assert!(ids_of(&[(FORGE_METADATA, "[[mods]]\nmodId=\"${mod_id}\"\n")]).is_empty());
    }

    #[test]
    fn metadata_that_does_not_parse_declares_nothing() {
        assert!(ids_of(&[(FABRIC_METADATA, "{ not json"), (FORGE_METADATA, "[[mods")]).is_empty());
    }

    #[test]
    fn a_file_that_is_not_an_archive_declares_nothing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("a.jar");
        std::fs::write(&path, b"jar").expect("write");
        assert!(declared_mod_ids(&path).is_empty());
    }

    #[test]
    fn oversized_metadata_is_not_read() {
        let padding = " ".repeat(2 * 1024 * 1024);
        let body = format!(r#"{{"id":"libx"{padding}}}"#);
        assert!(ids_of(&[(FABRIC_METADATA, &body)]).is_empty());
    }

    struct Folder {
        dir: tempfile::TempDir,
        staging: tempfile::TempDir,
    }

    impl Folder {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("temp dir");
            std::fs::create_dir_all(dir.path().join("mods")).expect("mods");
            Self {
                dir,
                staging: tempfile::tempdir().expect("temp dir"),
            }
        }

        fn installed(&self, name: &str, id: &str) {
            jar(
                &self.dir.path().join("mods").join(name),
                &[(FABRIC_METADATA, &format!(r#"{{"id":"{id}"}}"#))],
            );
        }

        fn incoming(&self, name: &str, id: &str) -> IncomingJar {
            let staged = self.staging.path().join(name);
            jar(
                &staged,
                &[(FABRIC_METADATA, &format!(r#"{{"id":"{id}"}}"#))],
            );
            IncomingJar {
                logical_path: Path::new("mods").join(name),
                staged,
            }
        }

        fn clash(&self, incoming: &[IncomingJar], leaving: &[&str]) -> Option<Clash> {
            let leaving = leaving
                .iter()
                .map(|name| Path::new("mods").join(name))
                .collect();
            judge(incoming, self.dir.path(), &leaving, &BTreeSet::new()).err()
        }

        fn letting_stand_in(
            &self,
            incoming: &[IncomingJar],
            may_stand_in: &[&str],
        ) -> Result<Vec<InstalledStandIn>, Clash> {
            let may_stand_in = may_stand_in
                .iter()
                .map(|name| Path::new("mods").join(name))
                .collect();
            judge(incoming, self.dir.path(), &BTreeSet::new(), &may_stand_in)
        }

        /// A Forge jar at `path` declaring every id in `ids`.
        fn forge_jar(path: &Path, ids: &[&str]) {
            let toml = ids.iter().fold(String::new(), |toml, id| {
                toml + "[[mods]]\nmodId=\"" + id + "\"\n"
            });
            jar(path, &[(FORGE_METADATA, &toml)]);
        }
    }

    #[test]
    fn a_jar_declaring_an_installed_mods_id_clashes_with_it() {
        let folder = Folder::new();
        folder.installed("libx-7.0.3.jar", "libx");
        let incoming = [folder.incoming("libx-7.0.4.jar", "libx")];

        match folder.clash(&incoming, &[]) {
            Some(Clash::WithExisting {
                mod_id, existing, ..
            }) => {
                assert_eq!(mod_id, "libx");
                assert_eq!(existing, Path::new("mods/libx-7.0.3.jar"));
            }
            other => panic!("expected a clash with the installed jar, got {other:?}"),
        }
    }

    #[test]
    fn a_disabled_jar_does_not_clash() {
        let folder = Folder::new();
        folder.installed("libx-7.0.3.jar.disabled", "libx");
        let incoming = [folder.incoming("libx-7.0.4.jar", "libx")];
        assert!(folder.clash(&incoming, &[]).is_none());
    }

    #[test]
    fn a_jar_the_change_moves_away_does_not_clash() {
        let folder = Folder::new();
        folder.installed("libx-7.0.3.jar", "libx");
        let incoming = [folder.incoming("libx-7.0.4.jar", "libx")];
        assert!(folder.clash(&incoming, &["libx-7.0.3.jar"]).is_none());
    }

    #[test]
    fn different_ids_do_not_clash() {
        let folder = Folder::new();
        folder.installed("other.jar", "other");
        let incoming = [folder.incoming("libx-7.0.4.jar", "libx")];
        assert!(folder.clash(&incoming, &[]).is_none());
    }

    #[test]
    fn the_same_id_in_another_loader_family_does_not_clash() {
        let folder = Folder::new();
        jar(
            &folder.dir.path().join("mods/libx-forge.jar"),
            &[(FORGE_METADATA, "[[mods]]\nmodId=\"libx\"\n")],
        );
        let incoming = [folder.incoming("libx-fabric.jar", "libx")];
        assert!(folder.clash(&incoming, &[]).is_none());
    }

    #[test]
    fn two_incoming_jars_declaring_one_id_clash_with_each_other() {
        let folder = Folder::new();
        let incoming = [
            folder.incoming("libx-a.jar", "libx"),
            folder.incoming("libx-b.jar", "libx"),
        ];
        match folder.clash(&incoming, &[]) {
            Some(Clash::WithinChange {
                mod_id,
                first,
                second,
            }) => {
                assert_eq!(mod_id, "libx");
                assert_eq!(first, Path::new("mods/libx-a.jar"));
                assert_eq!(second, Path::new("mods/libx-b.jar"));
            }
            other => panic!("expected a clash within the change, got {other:?}"),
        }
    }

    #[test]
    fn an_installed_jar_declaring_every_id_stands_in_when_allowed() {
        let folder = Folder::new();
        folder.installed("libx-dev.jar", "libx");
        folder.installed("other.jar", "other");
        let incoming = [
            folder.incoming("root.jar", "root"),
            folder.incoming("libx-7.0.4.jar", "libx"),
        ];

        let stood_in = folder
            .letting_stand_in(&incoming, &["libx-7.0.4.jar"])
            .expect("the installed jar stands in");

        assert_eq!(
            stood_in,
            [InstalledStandIn {
                planned: PathBuf::from("mods/libx-7.0.4.jar"),
                installed: PathBuf::from("mods/libx-dev.jar"),
                mod_ids: vec!["libx".to_owned()],
            }]
        );
    }

    #[test]
    fn an_installed_jar_that_could_stand_in_is_still_a_clash_unless_allowed() {
        let folder = Folder::new();
        folder.installed("libx-dev.jar", "libx");
        let incoming = [folder.incoming("libx-7.0.4.jar", "libx")];

        match folder.letting_stand_in(&incoming, &["root.jar"]) {
            Err(Clash::WithExisting { could_stand_in, .. }) => assert!(could_stand_in),
            other => panic!("expected a clash with the installed jar, got {other:?}"),
        }
    }

    #[test]
    fn an_installed_jar_declaring_only_some_of_the_ids_never_stands_in() {
        let folder = Folder::new();
        Folder::forge_jar(&folder.dir.path().join("mods/libx-dev.jar"), &["libx"]);
        let staged = folder.staging.path().join("libx-7.0.4.jar");
        Folder::forge_jar(&staged, &["libx", "libx_extra"]);
        let incoming = [IncomingJar {
            logical_path: PathBuf::from("mods/libx-7.0.4.jar"),
            staged,
        }];

        match folder.letting_stand_in(&incoming, &["libx-7.0.4.jar"]) {
            Err(Clash::WithExisting {
                mod_id,
                existing,
                could_stand_in,
                ..
            }) => {
                assert_eq!(mod_id, "libx");
                assert_eq!(existing, Path::new("mods/libx-dev.jar"));
                assert!(!could_stand_in);
            }
            other => panic!("expected a clash with the installed jar, got {other:?}"),
        }
    }

    #[test]
    fn the_jar_that_could_stand_in_is_the_one_named_when_another_shares_an_id() {
        let folder = Folder::new();
        Folder::forge_jar(&folder.dir.path().join("mods/a-partial.jar"), &["libx"]);
        Folder::forge_jar(
            &folder.dir.path().join("mods/b-whole.jar"),
            &["libx", "libx_extra"],
        );
        let staged = folder.staging.path().join("libx-7.0.4.jar");
        Folder::forge_jar(&staged, &["libx", "libx_extra"]);
        let incoming = [IncomingJar {
            logical_path: PathBuf::from("mods/libx-7.0.4.jar"),
            staged,
        }];

        match folder.clash(&incoming, &[]) {
            Some(Clash::WithExisting {
                existing,
                could_stand_in,
                ..
            }) => {
                assert_eq!(existing, Path::new("mods/b-whole.jar"));
                assert!(could_stand_in);
            }
            other => panic!("expected a clash with the installed jar, got {other:?}"),
        }
    }
}
