//! Installing a published server archive as a server's complete content.
//!
//! A pack author who publishes server files has already decided what the
//! server contains. There is nothing to resolve, no side to plan, and no
//! optional file to ask about: the archive is the answer. So this module is
//! deliberately much smaller than [`crate::install`] - it extracts, and it
//! reports what it extracted.
//!
//! # What the archive is not allowed to decide
//!
//! Ephervex installs the loader and its libraries for every server, including
//! one built from an archive that ships them. Server archives commonly carry an
//! installer jar and a script that fetches the loader on first start, and
//! neither is ever run.
//!
//! That makes a handful of destinations launcher-owned, and `libraries/` is the
//! one that matters for more than tidiness: a loader installation replaces
//! `<server>/libraries` with a **symlink into the shared libraries store**, and
//! containment here is lexical, so an entry named `libraries/…` would be
//! written straight through that link into storage every other instance and
//! server shares. It is excluded by name, and any other entry whose path walks
//! an existing link is refused outright rather than followed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use tracing::{debug, info, instrument};

use ephervex_download::hash_file;
use ephervex_model::{Hash, HashAlgorithm, ImportSourceKind, LoaderSelection, ModOrigin};

use crate::archive::{EntryDecision, ExtractionLimits, ExtractionReport, PackArchive};
use crate::error::{PackError, PackResult};
use crate::managed::MODS_DIR;
use crate::recipe::{
    PackFileRecipe, PackRecipe, ProviderFileIdentity, ProviderRecipeMetadata, RECIPE_VERSION,
    RecipeRelation, SideRequirement,
};

/// The loader installation's own library tree, which is a link into the shared
/// store rather than a directory this server owns.
pub const LIBRARIES_DIR: &str = "libraries";

/// Minecraft's consent file. Consent is launcher-global and explicit (ADR
/// 0014), so an archive's copy is not a consent record whatever it says.
pub const EULA_FILE: &str = "eula.txt";

/// Directories a Minecraft server keeps at the top of its own game directory.
///
/// Used for one decision: an archive whose only top-level directory is one of
/// these is holding content, not wrapping it. Stripping such a directory would
/// scatter a server's mods across its root, and the archive itself carries no
/// evidence that would tell the two shapes apart.
const SERVER_OWNED_DIRECTORIES: &[&str] = &[
    "config",
    "defaultconfigs",
    LIBRARIES_DIR,
    "kubejs",
    "logs",
    "mods",
    "resourcepacks",
    "scripts",
    "shaderpacks",
    "versions",
    "world",
];

/// One request to install a server archive over an installed loader.
#[derive(Debug, Clone, Copy)]
pub struct ServerPackInstall<'a> {
    pub archive: &'a Path,
    /// The directory the server runs in, with the loader already installed.
    pub game_dir: &'a Path,
    pub limits: ExtractionLimits,
    /// Paths, relative to `game_dir`, that the loader installation produced and
    /// the archive must not replace. The launch jar or argument file goes here.
    pub reserved: &'a [PathBuf],
}

/// What installing a server archive produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledServerPack {
    pub report: ExtractionReport,
    /// The archive's own file name, for display and for the server's record.
    pub archive_name: Option<String>,
    /// The single wrapping directory that was stripped, where the archive had
    /// one. Reported because it is a judgment about the archive's shape and a
    /// user comparing the result to the zip deserves to see it.
    pub stripped_root: Option<String>,
    /// Entries left out because their destination belongs to the launcher.
    pub declined: Vec<PathBuf>,
    /// The jars now in `mods/`, in path order.
    pub mods: Vec<PathBuf>,
}

/// Extracts a server archive into an installed server directory.
///
/// Synchronous, like every other extraction in this crate: `zip` is a blocking
/// API and only the caller knows whether it is on a runtime.
#[instrument(skip(request), fields(archive = %request.archive.display(), game_dir = %request.game_dir.display()))]
pub fn install_server_pack(request: ServerPackInstall<'_>) -> PackResult<InstalledServerPack> {
    let ServerPackInstall {
        archive,
        game_dir,
        limits,
        reserved,
    } = request;

    let mut pack = PackArchive::open(archive, limits)?;
    let stripped_root = pack.single_root().filter(|root| {
        !SERVER_OWNED_DIRECTORIES
            .iter()
            .any(|owned| root.eq_ignore_ascii_case(owned))
    });
    let prefix = stripped_root.clone().unwrap_or_default();

    let mut declined = Vec::new();
    let mut links = LinkCache::new(game_dir);
    let report = pack.extract_prefix_filtered(&prefix, game_dir, &mut |relative| {
        if is_launcher_owned(relative, reserved) {
            declined.push(relative.to_path_buf());
            return EntryDecision::Skip;
        }
        if links.walks_a_link(relative) {
            return EntryDecision::Refuse;
        }
        EntryDecision::Extract
    })?;

    let mods = installed_mods(game_dir)?;
    info!(
        files = report.files,
        declined = declined.len(),
        mods = mods.len(),
        "installed a server archive"
    );
    Ok(InstalledServerPack {
        report,
        archive_name: archive
            .file_name()
            .map(|name| name.to_string_lossy().into_owned()),
        stripped_root,
        declined,
        mods,
    })
}

/// Whether a destination belongs to the launcher rather than to the archive.
fn is_launcher_owned(relative: &Path, reserved: &[PathBuf]) -> bool {
    if relative.starts_with(LIBRARIES_DIR) || relative == Path::new(EULA_FILE) {
        return true;
    }
    reserved.iter().any(|owned| relative.starts_with(owned))
}

/// Remembers which destinations under one root are symlinks.
///
/// Every entry would otherwise re-`lstat` the same handful of parent
/// directories, and an archive with thousands of entries under `config/` would
/// pay for that once per entry.
#[derive(Debug)]
struct LinkCache<'a> {
    root: &'a Path,
    known: BTreeMap<PathBuf, bool>,
}

impl<'a> LinkCache<'a> {
    const fn new(root: &'a Path) -> Self {
        Self {
            root,
            known: BTreeMap::new(),
        }
    }

    /// Whether the entry, or anything it would be written underneath, is
    /// already a link on disk.
    fn walks_a_link(&mut self, relative: &Path) -> bool {
        relative
            .ancestors()
            .filter(|ancestor| !ancestor.as_os_str().is_empty())
            .any(|ancestor| self.is_link(ancestor))
    }

    fn is_link(&mut self, relative: &Path) -> bool {
        if let Some(known) = self.known.get(relative) {
            return *known;
        }
        let link = std::fs::symlink_metadata(self.root.join(relative))
            .is_ok_and(|metadata| metadata.is_symlink());
        self.known.insert(relative.to_path_buf(), link);
        link
    }
}

/// The jars a server archive left in `mods/`.
///
/// This enumerates a directory Ephervex itself just wrote from an archive it
/// extracted, which is a different thing from the client rule that nothing
/// scans a mutable `mods/`: there is no hand-placed jar here to adopt, because
/// the directory did not exist a moment ago.
///
/// Symlinks are ignored rather than followed. Nothing legitimate puts one here,
/// and a `mods/` entry that is a link is not a jar this server owns.
fn installed_mods(game_dir: &Path) -> PackResult<Vec<PathBuf>> {
    let dir = game_dir.join(MODS_DIR);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(PackError::Unreadable { path: dir, source }),
    };

    let mut jars = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| PackError::Unreadable {
            path: dir.clone(),
            source,
        })?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !Path::new(name)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("jar"))
        {
            continue;
        }
        let metadata = entry.metadata().map_err(|source| PackError::Unreadable {
            path: entry.path(),
            source,
        })?;
        if !metadata.is_file() {
            continue;
        }
        jars.push(Path::new(MODS_DIR).join(name));
    }
    jars.sort();
    debug!(mods = jars.len(), "read a server archive's mods directory");
    Ok(jars)
}

/// One jar a server archive installed, once its bytes have been measured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerPackMod {
    /// Where the jar sits, relative to the server game directory.
    pub logical_path: PathBuf,
    /// Digests the launcher computed itself. An archive publishes none, so
    /// these are the only identity the file has until a catalog recognizes it.
    pub hashes: Vec<Hash>,
    pub size: Option<u64>,
    /// The catalog release this jar turned out to be, where something
    /// recognized it. `None` is a real answer: a file nothing knows about is
    /// recorded as one rather than given an invented identity.
    pub identity: Option<ProviderFileIdentity>,
    /// What the answer that identified this jar declared about other projects.
    /// `None` when nothing answered for this exact file, which is not the same
    /// as an answer declaring nothing; see [`PackFileRecipe::relations`].
    pub relations: Option<Vec<RecipeRelation>>,
}

/// Gives each installed jar the digest it needs to be recorded at all.
///
/// An archive publishes no digests, so the launcher computes them. SHA-1 is
/// what both supported catalogs index versions by, which is what lets these
/// files be recognized later without being read a second time.
///
/// Async and separate from [`install_server_pack`] because extraction is
/// blocking work a caller runs off the reactor, and this is not.
pub async fn measure_installed_mods(
    game_dir: &Path,
    mods: &[PathBuf],
) -> PackResult<Vec<ServerPackMod>> {
    let mut measured = Vec::with_capacity(mods.len());
    for logical_path in mods {
        let path = game_dir.join(logical_path);
        let size = tokio::fs::metadata(&path).await.ok().map(|meta| meta.len());
        let hash = hash_file(&path, HashAlgorithm::Sha1)
            .await
            .map_err(|source| PackError::FileUnmeasurable {
                path: path.clone(),
                source: Box::new(source),
            })?;
        measured.push(ServerPackMod {
            logical_path: logical_path.clone(),
            hashes: vec![hash],
            size,
            identity: None,
            relations: None,
        });
    }
    debug!(mods = measured.len(), "measured a server archive's mods");
    Ok(measured)
}

/// The recipe a server built from an archive keeps.
///
/// The archive is the authority, so every entry is `Required` on the server and
/// carries an explicit selection: nothing here is waiting on a decision, and a
/// later managed-mod operation reads this file to learn what the server has.
/// The client side is `Unknown` because a server archive says nothing about
/// clients and inventing an answer would be a claim the archive never made.
///
/// No download address is recorded. The bytes came out of an archive rather
/// than from a catalog, and a recipe entry that names no address is exactly the
/// honest statement that this content cannot be re-fetched from anywhere.
#[must_use]
pub fn server_pack_recipe(
    name: impl Into<String>,
    minecraft_version: impl Into<String>,
    loader: LoaderSelection,
    archive_name: Option<String>,
    mods: &[ServerPackMod],
) -> PackRecipe {
    PackRecipe {
        recipe_version: RECIPE_VERSION,
        source_kind: ImportSourceKind::ServerPack,
        original_name: name.into(),
        original_version: None,
        author: None,
        summary: None,
        archive_name,
        minecraft_version: minecraft_version.into(),
        loader,
        files: mods
            .iter()
            .map(|entry| PackFileRecipe {
                logical_path: entry.logical_path.clone(),
                hashes: entry.hashes.clone(),
                downloads: Vec::new(),
                size: entry.size,
                client: SideRequirement::Unknown,
                server: SideRequirement::Required,
                selected: Some(true),
                origin: ModOrigin::PackDeclared,
                identity: entry.identity.clone(),
                original_identity: None,
                relations: entry.relations.clone(),
            })
            .collect(),
        override_layers: Vec::new(),
        provider_metadata: ProviderRecipeMetadata::new(ImportSourceKind::ServerPack, None),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use std::io::Write as _;

    use super::*;

    fn zip_file(path: &Path, entries: &[(&str, &[u8])]) {
        let file = std::fs::File::create(path).expect("create archive");
        let mut writer = zip::ZipWriter::new(file);
        let options: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, body) in entries {
            if name.ends_with('/') {
                writer.add_directory(*name, options).expect("directory");
                continue;
            }
            writer.start_file(*name, options).expect("entry");
            writer.write_all(body).expect("write");
        }
        writer.finish().expect("finish");
    }

    fn install(
        archive: &Path,
        game_dir: &Path,
        reserved: &[PathBuf],
    ) -> PackResult<InstalledServerPack> {
        install_server_pack(ServerPackInstall {
            archive,
            game_dir,
            limits: ExtractionLimits::default(),
            reserved,
        })
    }

    #[test]
    fn an_archive_wrapped_in_one_directory_installs_without_it() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("pack-server.zip");
        zip_file(
            &archive,
            &[
                ("Caveopolis/mods/one.jar", b"one"),
                ("Caveopolis/config/a.toml", b"a=1"),
            ],
        );
        let game_dir = dir.path().join("server");

        let installed = install(&archive, &game_dir, &[]).expect("install");

        assert_eq!(installed.stripped_root.as_deref(), Some("Caveopolis"));
        assert!(game_dir.join("mods/one.jar").is_file());
        assert!(game_dir.join("config/a.toml").is_file());
        assert!(!game_dir.join("Caveopolis").exists());
    }

    #[test]
    fn an_archive_with_files_at_its_root_keeps_every_top_level_name() {
        // What ServerPackCreator produces: mods/, config/, and loose files
        // beside them, with no wrapper to strip.
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("pack-server.zip");
        zip_file(
            &archive,
            &[
                ("mods/one.jar", b"one"),
                ("config/a.toml", b"a=1"),
                ("start.sh", b"#!/bin/sh\n"),
            ],
        );
        let game_dir = dir.path().join("server");

        let installed = install(&archive, &game_dir, &[]).expect("install");

        assert_eq!(installed.stripped_root, None);
        assert!(game_dir.join("mods/one.jar").is_file());
        assert!(
            game_dir.join("start.sh").is_file(),
            "nothing runs it, and leaving it out would hide what the author shipped"
        );
    }

    #[test]
    fn two_top_level_directories_are_not_mistaken_for_a_wrapper() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("pack-server.zip");
        zip_file(
            &archive,
            &[("mods/one.jar", b"one"), ("config/a.toml", b"a=1")],
        );
        let game_dir = dir.path().join("server");

        let installed = install(&archive, &game_dir, &[]).expect("install");

        assert_eq!(installed.stripped_root, None);
        assert!(game_dir.join("mods/one.jar").is_file());
        assert!(game_dir.join("config/a.toml").is_file());
    }

    #[test]
    fn an_archive_holding_only_mods_keeps_them_in_mods() {
        // The wrapper rule is destructive when it guesses wrong: stripping
        // `mods` here would put every jar at the server root and the server
        // would silently load none of them.
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("pack-server.zip");
        zip_file(
            &archive,
            &[("mods/one.jar", b"one"), ("mods/nested/deep.jar", b"deep")],
        );
        let game_dir = dir.path().join("server");

        let installed = install(&archive, &game_dir, &[]).expect("install");

        assert_eq!(installed.stripped_root, None);
        assert!(game_dir.join("mods/one.jar").is_file());
        assert_eq!(installed.mods, vec![PathBuf::from("mods/one.jar")]);
    }

    #[test]
    fn a_single_directory_of_loose_files_is_not_treated_as_a_wrapper() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("pack-server.zip");
        zip_file(
            &archive,
            &[("Caveopolis/a.txt", b"a"), ("Caveopolis/b.txt", b"b")],
        );
        let game_dir = dir.path().join("server");

        let installed = install(&archive, &game_dir, &[]).expect("install");

        assert_eq!(installed.stripped_root, None);
        assert!(game_dir.join("Caveopolis/a.txt").is_file());
    }

    #[test]
    fn the_archives_libraries_are_left_out_so_the_installed_loaders_survive() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("pack-server.zip");
        zip_file(
            &archive,
            &[
                ("mods/one.jar", b"one"),
                ("libraries/net/example/stale.jar", b"stale"),
            ],
        );
        let game_dir = dir.path().join("server");
        std::fs::create_dir_all(game_dir.join("libraries/net/example")).expect("libraries");
        std::fs::write(
            game_dir.join("libraries/net/example/stale.jar"),
            b"installed",
        )
        .expect("installed library");

        let installed = install(&archive, &game_dir, &[]).expect("install");

        assert!(
            installed
                .declined
                .iter()
                .any(|path| path.starts_with(LIBRARIES_DIR))
        );
        assert_eq!(
            std::fs::read(game_dir.join("libraries/net/example/stale.jar")).expect("read"),
            b"installed",
            "the loader's own library is what a server launches against"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_archive_never_writes_through_the_shared_library_link() {
        // A loader installation replaces `libraries` with a link into the
        // shared store. Containment here is lexical, so this is the test that
        // the exclusion is what keeps other instances' libraries safe.
        let dir = tempfile::tempdir().expect("temp dir");
        let shared = dir.path().join("shared-libraries");
        std::fs::create_dir_all(shared.join("net/example")).expect("shared store");
        std::fs::write(shared.join("net/example/real.jar"), b"shared").expect("shared library");

        let archive = dir.path().join("pack-server.zip");
        zip_file(&archive, &[("libraries/net/example/real.jar", b"archive")]);

        let game_dir = dir.path().join("server");
        std::fs::create_dir_all(&game_dir).expect("server");
        std::os::unix::fs::symlink(&shared, game_dir.join("libraries")).expect("link");

        install(&archive, &game_dir, &[]).expect("install");

        assert_eq!(
            std::fs::read(shared.join("net/example/real.jar")).expect("read"),
            b"shared",
            "the shared store belongs to every instance, not to one archive"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_entry_that_would_be_written_through_any_other_link_is_refused() {
        let dir = tempfile::tempdir().expect("temp dir");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&outside).expect("outside");

        let archive = dir.path().join("pack-server.zip");
        zip_file(
            &archive,
            &[("config/escaped.toml", b"a=1"), ("mods/one.jar", b"one")],
        );

        let game_dir = dir.path().join("server");
        std::fs::create_dir_all(&game_dir).expect("server");
        std::os::unix::fs::symlink(&outside, game_dir.join("config")).expect("link");

        let error = install(&archive, &game_dir, &[]).expect_err("refused");

        assert!(matches!(error, PackError::EntryPathUnsafe { .. }));
        assert!(!outside.join("escaped.toml").exists());
    }

    #[test]
    fn the_installed_launch_descriptor_is_not_replaced_by_the_archives_own() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("pack-server.zip");
        zip_file(&archive, &[("neoforge-21.1.77.jar", b"archive")]);
        let game_dir = dir.path().join("server");
        std::fs::create_dir_all(&game_dir).expect("server");
        std::fs::write(game_dir.join("neoforge-21.1.77.jar"), b"installed").expect("launch jar");

        let reserved = vec![PathBuf::from("neoforge-21.1.77.jar")];
        let installed = install(&archive, &game_dir, &reserved).expect("install");

        assert_eq!(installed.declined, reserved);
        assert_eq!(
            std::fs::read(game_dir.join("neoforge-21.1.77.jar")).expect("read"),
            b"installed"
        );
    }

    #[test]
    fn an_archives_eula_is_never_installed_as_consent() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("pack-server.zip");
        zip_file(
            &archive,
            &[("eula.txt", b"eula=true\n"), ("mods/one.jar", b"one")],
        );
        let game_dir = dir.path().join("server");

        let installed = install(&archive, &game_dir, &[]).expect("install");

        assert!(!game_dir.join(EULA_FILE).exists());
        assert!(installed.declined.contains(&PathBuf::from(EULA_FILE)));
    }

    #[test]
    fn an_entry_that_escapes_the_server_is_refused_before_anything_is_written() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("pack-server.zip");
        zip_file(&archive, &[("../escaped.txt", b"no")]);
        let game_dir = dir.path().join("server");

        let error = install(&archive, &game_dir, &[]).expect_err("refused");

        assert!(matches!(error, PackError::EntryPathUnsafe { .. }));
        assert!(!dir.path().join("escaped.txt").exists());
    }

    #[test]
    fn an_entry_naming_launcher_metadata_is_refused() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("pack-server.zip");
        zip_file(&archive, &[(".launcher/server.json", b"{}")]);
        let game_dir = dir.path().join("server");

        let error = install(&archive, &game_dir, &[]).expect_err("refused");

        assert!(matches!(error, PackError::EntryReserved { .. }));
    }

    #[test]
    fn the_extraction_budget_still_bounds_a_server_archive() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("pack-server.zip");
        zip_file(&archive, &[("mods/one.jar", &vec![0u8; 4096])]);
        let game_dir = dir.path().join("server");

        let error = install_server_pack(ServerPackInstall {
            archive: &archive,
            game_dir: &game_dir,
            limits: ExtractionLimits {
                max_entries: 100,
                max_total_bytes: 64,
                max_entry_bytes: 64,
                ..ExtractionLimits::default()
            },
            reserved: &[],
        })
        .expect_err("refused");

        assert!(matches!(error, PackError::LimitExceeded { .. }));
    }

    #[test]
    fn only_jars_directly_in_mods_are_reported_as_the_servers_mods() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("pack-server.zip");
        zip_file(
            &archive,
            &[
                ("mods/two.jar", b"two"),
                ("mods/one.jar", b"one"),
                ("mods/README.txt", b"read me"),
                ("mods/nested/deep.jar", b"deep"),
                ("config/a.toml", b"a=1"),
            ],
        );
        let game_dir = dir.path().join("server");

        let installed = install(&archive, &game_dir, &[]).expect("install");

        assert_eq!(
            installed.mods,
            vec![PathBuf::from("mods/one.jar"), PathBuf::from("mods/two.jar")]
        );
    }

    /// An identity established for a jar has to survive into the recipe, which
    /// is the only place anything later reads it from. Establishing one and
    /// then writing `None` looks entirely successful in the logs.
    #[test]
    fn an_established_identity_reaches_the_recipe() {
        let identified = ServerPackMod {
            logical_path: PathBuf::from("mods/jei.jar"),
            hashes: vec![
                Hash::new(ephervex_model::HashAlgorithm::Sha1, &"a".repeat(40)).expect("sha1"),
            ],
            size: Some(3),
            identity: Some(ProviderFileIdentity {
                provider: Some(ephervex_model::ModProvider::CurseForge),
                project_id: Some("238222".to_owned()),
                version_id: Some("4712354".to_owned()),
                file_id: Some("4712354".to_owned()),
            }),
            relations: None,
        };
        let unidentified = ServerPackMod {
            logical_path: PathBuf::from("mods/mystery.jar"),
            hashes: vec![
                Hash::new(ephervex_model::HashAlgorithm::Sha1, &"b".repeat(40)).expect("sha1"),
            ],
            size: Some(3),
            identity: None,
            relations: None,
        };

        let recipe = server_pack_recipe(
            "Repro Server",
            "1.21.1",
            LoaderSelection::vanilla(),
            None,
            &[identified.clone(), unidentified],
        );

        assert_eq!(recipe.files[0].identity, identified.identity);
        assert_eq!(
            recipe.files[1].identity, None,
            "a jar nothing recognized is recorded as such, not given one"
        );

        // And it has to survive the step every mod view actually reads, which
        // is where a provider stops being "No provider" on screen.
        let recorded = recipe.recorded_mods_for(crate::recipe::PackSide::Server);
        assert_eq!(
            recorded[0].identity.provider(),
            Some(ephervex_model::ModProvider::CurseForge)
        );
        assert_eq!(recorded[1].identity.provider(), None);
    }

    /// The relations an identifying answer declared are recorded with the
    /// entry, and an answer declaring nothing stays distinct from no answer.
    #[test]
    fn each_jars_relation_knowledge_reaches_the_recipe_unchanged() {
        let requires = RecipeRelation {
            kind: ephervex_model::ModRelationKind::Required,
            target: crate::recipe::RelationTarget::Project {
                project_id: "306612".to_owned(),
                version_id: None,
            },
        };
        let jar = |name: &str, relations: Option<Vec<RecipeRelation>>| ServerPackMod {
            logical_path: PathBuf::from(format!("mods/{name}.jar")),
            hashes: Vec::new(),
            size: Some(3),
            identity: None,
            relations,
        };

        let recipe = server_pack_recipe(
            "Repro Server",
            "1.21.1",
            LoaderSelection::vanilla(),
            None,
            &[
                jar("feature", Some(vec![requires.clone()])),
                jar("library", Some(Vec::new())),
                jar("mystery", None),
            ],
        );

        assert_eq!(recipe.files[0].recorded_relations(), Some(&[requires][..]));
        assert_eq!(recipe.files[1].recorded_relations(), Some(&[][..]));
        assert_eq!(recipe.files[2].recorded_relations(), None);
    }

    #[tokio::test]
    async fn a_recipe_records_every_installed_jar_as_required_and_selected() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("pack-server.zip");
        zip_file(&archive, &[("mods/one.jar", b"one")]);
        let game_dir = dir.path().join("server");
        let installed = install(&archive, &game_dir, &[]).expect("install");

        let mods = measure_installed_mods(&game_dir, &installed.mods)
            .await
            .expect("measure");
        let recipe = server_pack_recipe(
            "Repro Server",
            "1.21.1",
            LoaderSelection {
                kind: ephervex_model::LoaderKind::NeoForge,
                version: Some("21.1.77".to_owned()),
            },
            installed.archive_name.clone(),
            &mods,
        );

        assert_eq!(recipe.source_kind, ImportSourceKind::ServerPack);
        assert_eq!(recipe.recipe_version, RECIPE_VERSION);
        assert_eq!(recipe.files.len(), 1);
        let file = &recipe.files[0];
        assert_eq!(file.server, SideRequirement::Required);
        assert_eq!(file.client, SideRequirement::Unknown);
        assert_eq!(file.selected, Some(true));
        assert!(file.downloads.is_empty(), "an archive publishes no address");
        assert_eq!(
            file.hashes.len(),
            1,
            "the launcher computed the only digest there is"
        );
        assert_eq!(file.size, Some(3));
        assert!(recipe.override_layers.is_empty());
    }
}
