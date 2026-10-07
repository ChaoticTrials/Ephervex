//! Building a server from a published server archive.
//!
//! The other way to give a server its content is to replay the client's
//! retained recipe ([`App::materialize_linked_server_recipe`]). That path exists
//! because a client pack is all there is when a pack declares nothing about
//! servers, which is the CurseForge export format's normal state. It is a
//! reconstruction, and it can only ever be as good as what the client's manifest
//! happened to say.
//!
//! An archive published as a pack's server files is not a reconstruction. It is
//! what the author shipped, so nothing here resolves a catalog, plans a side,
//! or asks about an optional file: the archive is copied in and the server's
//! content is complete.
//!
//! Two things stay Ephervex's regardless. The loader and its libraries are
//! installed by the launcher for every server, because an archive's own way of
//! getting them is an installer jar or a download script and neither is ever
//! run. And EULA consent stays global and explicit, so an archive's `eula.txt`
//! is never read as one.

use std::path::{Path, PathBuf};

use std::collections::BTreeMap;

use ephervex_download::OperationReporter;
use ephervex_mod_platform::{ModPlatformError, RecognizedFile, curseforge_fingerprint};
use ephervex_model::{
    Hash, HashAlgorithm, ImportSourceKind, LoaderSelection, ModProvider, ServerLaunch,
};
use ephervex_pack::{
    ExtractionLimits, InstalledServerPack, PackError, ProviderFileIdentity, RecipeRelation,
    ServerPackInstall, ServerPackMod, SourceStore, install_server_pack, measure_installed_mods,
    server_pack_recipe,
};
use ephervex_storage::ImportSource;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, instrument};

use crate::CoreResult;
use crate::app::App;
use crate::mods::recipe_relations;

/// Where a new server's content comes from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ServerContentSource {
    /// Reconstruct from the linked client's retained recipe, or install no
    /// content at all for a standalone server. What every server did before
    /// archives were supported, and still the default.
    #[default]
    Recipe,
    /// Install this archive as the server's complete content.
    ///
    /// Available to a linked and a standalone server alike: the archive decides
    /// the content either way, and only the exact Minecraft and loader
    /// selection differs in where it comes from.
    Archive { path: PathBuf },
}

impl ServerContentSource {
    /// The archive this content source names, if it names one.
    #[must_use]
    pub fn archive(&self) -> Option<&Path> {
        match self {
            Self::Recipe => None,
            Self::Archive { path } => Some(path),
        }
    }
}

/// One request to install a server archive over an installed loader.
///
/// A struct rather than a long argument list, matching the install requests
/// `ephervex-pack` and `ephervex-loaders` already use: naming the fields at the
/// call site is what keeps `game_dir` and `server_source` from being passed the
/// wrong way round.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ServerArchiveInstall<'a> {
    pub archive: &'a Path,
    /// The server's display name, which becomes the recipe's own name.
    pub name: &'a str,
    pub minecraft_version: &'a str,
    pub loader: &'a LoaderSelection,
    /// What the loader installation produced, so the archive cannot replace it.
    pub launch: Option<&'a ServerLaunch>,
    /// The directory the server runs in, with the loader already installed.
    pub game_dir: &'a Path,
    /// The server's own `.launcher/source/`, where its recipe is written.
    pub server_source: &'a Path,
    /// The linked client's retained source, when this server has one. Read only
    /// to recognize the archive's jars by digest, never for content.
    pub client_source: Option<&'a Path>,
    pub reporter: Option<&'a OperationReporter>,
    pub cancel: &'a CancellationToken,
}

/// What installing a server archive produced, for a frontend to report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerPackOutcome {
    pub archive_name: Option<String>,
    /// The single wrapping directory the archive used, where it used one.
    pub stripped_root: Option<String>,
    pub files: u64,
    pub bytes: u64,
    /// Entries left out because their destination belongs to the launcher.
    pub declined: Vec<PathBuf>,
    pub mods: usize,
    /// What the catalogs made of those mods.
    pub identification: IdentificationOutcome,
}

impl App {
    /// Installs an archive as a server's content and writes the server's recipe.
    ///
    /// Runs after the loader installation, over the same directory: the
    /// launch descriptor the loader produced is one of the destinations the
    /// archive is not allowed to replace.
    #[instrument(skip_all, fields(game_dir = %request.game_dir.display()))]
    pub(crate) async fn install_server_archive(
        &self,
        request: ServerArchiveInstall<'_>,
    ) -> CoreResult<ServerPackOutcome> {
        let ServerArchiveInstall {
            archive,
            name,
            minecraft_version,
            loader,
            launch,
            game_dir,
            server_source,
            client_source,
            reporter,
            cancel,
        } = request;
        if let Some(reporter) = reporter {
            reporter.set_stage(Some("operation-stage-installing-server-pack".to_owned()));
        }
        let reserved = launch_reserved(launch);

        let archive_path = archive.to_path_buf();
        let destination = game_dir.to_path_buf();
        // `zip` is a blocking API and this runs on a runtime, so the whole
        // extraction goes to a blocking thread rather than stalling the
        // reactor for the length of a multi-gigabyte archive.
        let installed: InstalledServerPack = tokio::task::spawn_blocking(move || {
            install_server_pack(ServerPackInstall {
                archive: &archive_path,
                game_dir: &destination,
                limits: ExtractionLimits::default(),
                reserved: &reserved,
            })
        })
        .await
        .map_err(|_| PackError::Canceled)??;

        if cancel.is_cancelled() {
            return Err(PackError::Canceled.into());
        }
        let mut mods = measure_installed_mods(game_dir, &installed.mods).await?;
        let identification = self
            .identify_installed_mods(&mut mods, game_dir, client_source, cancel)
            .await;
        let recipe = server_pack_recipe(
            name,
            minecraft_version,
            loader.clone(),
            installed.archive_name.clone(),
            &mods,
        );
        SourceStore::new(server_source).write_recipe(&recipe)?;

        info!(
            files = installed.report.files,
            declined = installed.declined.len(),
            mods = mods.len(),
            "installed a server from a published archive"
        );
        Ok(ServerPackOutcome {
            archive_name: installed.archive_name,
            stripped_root: installed.stripped_root,
            files: installed.report.files,
            bytes: installed.report.bytes,
            declined: installed.declined,
            mods: mods.len(),
            identification,
        })
    }
}

/// Where an installed jar's catalog identity came from, if anywhere.
///
/// Reported so a frontend can say why a mod has no provider without guessing,
/// and so a run against an unreachable catalog is distinguishable from one that
/// simply found nothing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IdentificationOutcome {
    /// Matched against a recipe the launcher already had. No request made.
    pub from_local_records: usize,
    pub from_curseforge: usize,
    pub from_modrinth: usize,
    /// Recognized by nothing. Recorded with a digest and no provider.
    pub unidentified: usize,
    /// A catalog that could not be asked at all: no credential, or it did not
    /// answer. Identification degrades; the server is complete either way.
    pub curseforge_unavailable: bool,
    pub modrinth_unavailable: bool,
}

impl App {
    /// Gives each installed jar the best catalog identity that can be
    /// established for it.
    ///
    /// Cheapest and most certain first. What the launcher already recorded
    /// costs no request and cannot be wrong about a digest it wrote itself;
    /// only what is left over is worth asking a catalog about.
    ///
    /// No step here can fail the installation. A server built from an archive
    /// is complete before this runs, and a mod with no provider is a mod with
    /// no provider -- which the model has always been able to say.
    #[instrument(skip_all, fields(mods = mods.len()))]
    pub(crate) async fn identify_installed_mods(
        &self,
        mods: &mut [ServerPackMod],
        game_dir: &Path,
        client_source: Option<&Path>,
        cancel: &CancellationToken,
    ) -> IdentificationOutcome {
        let mut outcome = IdentificationOutcome::default();
        if mods.is_empty() {
            return outcome;
        }

        outcome.from_local_records = apply_local_records(mods, client_source);
        if !cancel.is_cancelled() {
            match self.recognize_with_curseforge(mods, game_dir, cancel).await {
                Ok(matched) => outcome.from_curseforge = matched,
                Err(error) => {
                    debug!(%error, "curseforge did not identify this server's mods");
                    outcome.curseforge_unavailable = true;
                }
            }
        }
        if !cancel.is_cancelled() {
            match self.recognize_with_modrinth(mods, cancel).await {
                Ok(matched) => outcome.from_modrinth = matched,
                Err(error) => {
                    debug!(%error, "modrinth did not identify this server's mods");
                    outcome.modrinth_unavailable = true;
                }
            }
        }

        outcome.unidentified = mods.iter().filter(|entry| entry.identity.is_none()).count();
        info!(
            local = outcome.from_local_records,
            curseforge = outcome.from_curseforge,
            modrinth = outcome.from_modrinth,
            unidentified = outcome.unidentified,
            "identified a server archive's mods"
        );
        outcome
    }

    /// Asks CurseForge which of the still-unknown files it recognizes.
    ///
    /// CurseForge indexes by a fingerprint of its own rather than by a digest,
    /// so each candidate is read once more to compute one. Only files nothing
    /// has identified yet are read, and each is dropped before the next, so the
    /// cost is one jar in memory rather than a whole `mods/` directory.
    async fn recognize_with_curseforge(
        &self,
        mods: &mut [ServerPackMod],
        game_dir: &Path,
        cancel: &CancellationToken,
    ) -> CoreResult<usize> {
        let api = self.curseforge_api()?;
        // Asked before anything is read: with no key the request could only be
        // refused, and fingerprinting a hundred jars for it would be wasted.
        if !api.has_credential()? {
            return Err(ModPlatformError::CredentialMissing {
                provider: ModProvider::CurseForge,
            }
            .into());
        }

        let mut wanted: Vec<(u32, Hash)> = Vec::new();
        for entry in mods.iter().filter(|entry| entry.identity.is_none()) {
            if cancel.is_cancelled() {
                return Err(PackError::Canceled.into());
            }
            let Some(digest) = sha1_of(entry) else {
                continue;
            };
            let path = game_dir.join(&entry.logical_path);
            // A jar that cannot be read is one this pass cannot identify. It is
            // still installed, and the installation already succeeded.
            let Ok(bytes) = tokio::fs::read(&path).await else {
                continue;
            };
            wanted.push((curseforge_fingerprint(&bytes), digest.clone()));
        }
        if wanted.is_empty() {
            return Ok(0);
        }

        let operation = api.begin_operation()?;
        let recognized = operation.fingerprints(&wanted, cancel).await?;
        Ok(apply_recognized(mods, &recognized))
    }

    /// Asks Modrinth about whatever is still unidentified, by digest.
    ///
    /// One request for the whole batch, and no credential: Modrinth indexes its
    /// files by SHA-1, which the launcher already computed.
    async fn recognize_with_modrinth(
        &self,
        mods: &mut [ServerPackMod],
        cancel: &CancellationToken,
    ) -> CoreResult<usize> {
        let digests: Vec<Hash> = mods
            .iter()
            .filter(|entry| entry.identity.is_none())
            .filter_map(sha1_of)
            .collect();
        if digests.is_empty() {
            return Ok(0);
        }
        let recognized = self.modrinth_catalog()?.recognize(&digests, cancel).await?;
        Ok(apply_recognized(mods, &recognized))
    }
}

/// The SHA-1 this entry was measured with, which is the key both catalogs are
/// matched on.
fn sha1_of(entry: &ServerPackMod) -> Option<Hash> {
    entry
        .hashes
        .iter()
        .find(|hash| hash.algorithm == HashAlgorithm::Sha1)
        .cloned()
}

/// Fills in identities from a recipe the launcher already holds.
///
/// A linked server's client keeps one, and it records a digest and a catalog
/// identity for every file it installed. A server archive is largely the same
/// files, so this answers most of them offline, with no credential and no
/// request -- and it is the only step that cannot be wrong, because the digest
/// it matches on is one this launcher computed and wrote itself.
///
/// Reading the client's recipe here is a lookup, not a source of content: what
/// the server contains was decided by the archive and is already on disk
/// (ADR 0024). The matched entry's relation knowledge is copied as it stands,
/// so a client that never asked still reads as unasked on the server.
fn apply_local_records(mods: &mut [ServerPackMod], client_source: Option<&Path>) -> usize {
    let Some(source) = client_source else {
        return 0;
    };
    let store = SourceStore::new(source);
    if !store.has_recipe() {
        return 0;
    }
    let Ok(recipe) = store.read_recipe() else {
        return 0;
    };

    let mut known: BTreeMap<String, (ProviderFileIdentity, Option<Vec<RecipeRelation>>)> =
        BTreeMap::new();
    for file in &recipe.files {
        let Some(identity) = file
            .identity
            .as_ref()
            .filter(|identity| !identity.is_empty())
        else {
            continue;
        };
        for hash in &file.hashes {
            if hash.algorithm == HashAlgorithm::Sha1 {
                known.insert(
                    hash.digest().to_owned(),
                    (identity.clone(), file.relations.clone()),
                );
            }
        }
    }

    let mut matched = 0;
    for entry in mods.iter_mut().filter(|entry| entry.identity.is_none()) {
        let Some(digest) = sha1_of(entry) else {
            continue;
        };
        if let Some((identity, relations)) = known.get(digest.digest()) {
            entry.identity = Some(identity.clone());
            entry.relations.clone_from(relations);
            matched += 1;
        }
    }
    matched
}

/// Writes a catalog's answers onto the entries they are about.
///
/// The answer that identified a jar is about that exact file, so what it
/// declared is recorded with it, including a declaration of nothing.
fn apply_recognized(
    mods: &mut [ServerPackMod],
    recognized: &BTreeMap<String, RecognizedFile>,
) -> usize {
    let mut matched = 0;
    for entry in mods.iter_mut().filter(|entry| entry.identity.is_none()) {
        let Some(digest) = sha1_of(entry) else {
            continue;
        };
        let Some(found) = recognized.get(digest.digest()) else {
            continue;
        };
        entry.identity = Some(ProviderFileIdentity {
            provider: Some(found.provider),
            project_id: Some(found.project_id.clone()),
            version_id: Some(found.version_id.clone()),
            file_id: found.file_id.clone(),
        });
        entry.relations = Some(recipe_relations(&found.dependencies));
        matched += 1;
    }
    matched
}

/// The destinations a loader installation produced, which pack content must
/// not replace: the launch jar or argument file.
pub(crate) fn launch_reserved(launch: Option<&ServerLaunch>) -> Vec<PathBuf> {
    launch
        .map(|launch| vec![launch.path().as_path().to_path_buf()])
        .unwrap_or_default()
}

/// The record's own note of where its content came from./// The record's own note of where its content came from.
///
/// The archive's file name is display evidence, not an address: nothing
/// re-reads it, and the file is usually gone by the time anyone looks.
#[must_use]
pub(crate) fn archive_import_source(archive: &Path) -> ImportSource {
    archive.file_name().map_or_else(
        || ImportSource {
            kind: ImportSourceKind::ServerPack,
            reference: None,
        },
        |name| ImportSource::imported(ImportSourceKind::ServerPack, name.to_string_lossy()),
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use std::io::Write as _;
    use std::sync::Arc;

    use ephervex_auth::MemoryStore;
    use ephervex_mod_platform::ModDependency;
    use ephervex_model::{
        LoaderKind, LoaderSelection, ModRelationKind, SecretString, ServerRelativePath,
    };
    use ephervex_pack::{PackRecipe, SideRequirement};
    use ephervex_storage::Paths;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::app::App;

    fn test_app(base: &Path) -> App {
        App::open_at(Paths::rooted_at(base)).expect("open app")
    }

    fn zip_file(path: &Path, entries: &[(&str, &[u8])]) {
        let file = std::fs::File::create(path).expect("create archive");
        let mut writer = zip::ZipWriter::new(file);
        let options: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, body) in entries {
            writer.start_file(*name, options).expect("entry");
            writer.write_all(body).expect("write");
        }
        writer.finish().expect("finish");
    }

    fn neoforge() -> LoaderSelection {
        LoaderSelection {
            kind: LoaderKind::NeoForge,
            version: Some("21.1.77".to_owned()),
        }
    }

    #[tokio::test]
    async fn an_archive_becomes_the_servers_content_and_its_own_recipe() {
        let directory = tempfile::tempdir().expect("temp dir");
        let app = test_app(directory.path());
        let archive = directory.path().join("pack-server.zip");
        zip_file(
            &archive,
            &[
                ("mods/one.jar", b"one"),
                ("config/a.toml", b"a=1"),
                ("start.sh", b"#!/bin/sh\n"),
            ],
        );
        let game_dir = directory.path().join("server");
        let server_source = directory.path().join("source");

        let outcome = app
            .install_server_archive(ServerArchiveInstall {
                archive: &archive,
                name: "Repro Server",
                minecraft_version: "1.21.1",
                loader: &neoforge(),
                launch: None,
                game_dir: &game_dir,
                server_source: &server_source,
                client_source: None,
                reporter: None,
                cancel: &CancellationToken::new(),
            })
            .await
            .expect("install");

        assert_eq!(outcome.mods, 1);
        assert_eq!(outcome.files, 3);
        assert_eq!(
            outcome.archive_name.as_deref(),
            Some("pack-server.zip"),
            "the record shows which archive a server came from"
        );
        assert!(game_dir.join("mods/one.jar").is_file());
        assert!(game_dir.join("config/a.toml").is_file());

        let recipe: PackRecipe = serde_json::from_slice(
            &std::fs::read(server_source.join("recipe.json")).expect("read"),
        )
        .expect("recipe");
        assert_eq!(recipe.source_kind, ImportSourceKind::ServerPack);
        assert_eq!(recipe.minecraft_version, "1.21.1");
        assert_eq!(recipe.loader, neoforge());
        assert_eq!(recipe.files.len(), 1, "only mods are recorded as content");
        assert_eq!(recipe.files[0].server, SideRequirement::Required);
        assert!(recipe.files[0].downloads.is_empty());
    }

    #[tokio::test]
    async fn the_loaders_launch_file_survives_an_archive_that_carries_its_own() {
        let directory = tempfile::tempdir().expect("temp dir");
        let app = test_app(directory.path());
        let archive = directory.path().join("pack-server.zip");
        zip_file(
            &archive,
            &[("server.jar", b"archive"), ("mods/one.jar", b"one")],
        );
        let game_dir = directory.path().join("server");
        std::fs::create_dir_all(&game_dir).expect("server");
        std::fs::write(game_dir.join("server.jar"), b"installed").expect("launch jar");
        let launch = ServerLaunch::Jar {
            path: ServerRelativePath::new("server.jar").expect("relative"),
        };

        app.install_server_archive(ServerArchiveInstall {
            archive: &archive,
            name: "Repro Server",
            minecraft_version: "1.21.1",
            loader: &neoforge(),
            launch: Some(&launch),
            game_dir: &game_dir,
            server_source: &directory.path().join("source"),
            client_source: None,
            reporter: None,
            cancel: &CancellationToken::new(),
        })
        .await
        .expect("install");

        assert_eq!(
            std::fs::read(game_dir.join("server.jar")).expect("read"),
            b"installed",
            "a server launches the loader Ephervex installed, never the archive's copy"
        );
    }

    fn measured(path: &str, digest: &str) -> ServerPackMod {
        ServerPackMod {
            logical_path: PathBuf::from(path),
            hashes: vec![Hash::new(HashAlgorithm::Sha1, digest).expect("sha1")],
            size: Some(3),
            identity: None,
            relations: None,
        }
    }

    fn curseforge_identity(project: &str, file: &str) -> ProviderFileIdentity {
        ProviderFileIdentity {
            provider: Some(ModProvider::CurseForge),
            project_id: Some(project.to_owned()),
            version_id: Some(file.to_owned()),
            file_id: Some(file.to_owned()),
        }
    }

    /// The client's own recipe answers most of a server archive without any
    /// request at all: the two share the same jars, and the launcher wrote
    /// those digests itself.
    #[test]
    fn a_client_recipe_identifies_the_jars_it_shares_with_the_archive() {
        let directory = tempfile::tempdir().expect("temp dir");
        let source = directory.path().join("source");
        let store = SourceStore::new(&source);
        let mut recipe =
            PackRecipe::for_manual_instance("Repro", "1.21.1", LoaderSelection::vanilla());
        recipe.files.push(ephervex_pack::PackFileRecipe {
            logical_path: PathBuf::from("mods/known.jar"),
            hashes: vec![Hash::new(HashAlgorithm::Sha1, &"a".repeat(40)).expect("sha1")],
            downloads: Vec::new(),
            size: None,
            client: SideRequirement::Required,
            server: SideRequirement::Required,
            selected: None,
            origin: ephervex_pack::ModOrigin::PackDeclared,
            identity: Some(curseforge_identity("238222", "4712354")),
            original_identity: None,
            relations: None,
        });
        store.write_recipe(&recipe).expect("write");

        let mut mods = vec![
            measured("mods/known.jar", &"a".repeat(40)),
            measured("mods/server-only.jar", &"b".repeat(40)),
        ];

        let matched = apply_local_records(&mut mods, Some(&source));

        assert_eq!(matched, 1);
        assert_eq!(
            mods[0].identity,
            Some(curseforge_identity("238222", "4712354"))
        );
        assert_eq!(
            mods[1].identity, None,
            "a jar the client never had stays for a catalog to answer"
        );
    }

    /// A standalone server has no client, and an entry with no identity is a
    /// real answer rather than a failure.
    #[test]
    fn no_client_record_leaves_every_jar_for_the_catalogs() {
        let mut mods = vec![measured("mods/one.jar", &"c".repeat(40))];

        assert_eq!(apply_local_records(&mut mods, None), 0);
        assert_eq!(mods[0].identity, None);
    }

    /// An entry the launcher already answered for is never asked about again,
    /// so a later catalog cannot overwrite a more certain answer.
    #[test]
    fn a_catalog_never_replaces_an_identity_already_established() {
        let mut mods = vec![measured("mods/one.jar", &"d".repeat(40))];
        mods[0].identity = Some(curseforge_identity("1", "2"));

        let recognized = BTreeMap::from([(
            "d".repeat(40),
            RecognizedFile {
                provider: ModProvider::Modrinth,
                project_id: "wrong".to_owned(),
                version_id: "wrong".to_owned(),
                file_id: None,
                dependencies: Vec::new(),
            },
        )]);

        assert_eq!(apply_recognized(&mut mods, &recognized), 0);
        assert_eq!(mods[0].identity, Some(curseforge_identity("1", "2")));
        assert_eq!(
            mods[0].relations, None,
            "an answer about another release says nothing about this one"
        );
    }

    /// A catalog's answer is written onto the entry whose digest it is about,
    /// and onto no other.
    #[test]
    fn a_recognized_digest_identifies_only_its_own_entry() {
        let mut mods = vec![
            measured("mods/one.jar", &"e".repeat(40)),
            measured("mods/two.jar", &"f".repeat(40)),
        ];
        let recognized = BTreeMap::from([(
            "f".repeat(40),
            RecognizedFile {
                provider: ModProvider::Modrinth,
                project_id: "AABBCCDD".to_owned(),
                version_id: "vvvv".to_owned(),
                file_id: None,
                dependencies: Vec::new(),
            },
        )]);

        assert_eq!(apply_recognized(&mut mods, &recognized), 1);
        assert_eq!(mods[0].identity, None);
        assert_eq!(
            mods[1].identity,
            Some(ProviderFileIdentity {
                provider: Some(ModProvider::Modrinth),
                project_id: Some("AABBCCDD".to_owned()),
                version_id: Some("vvvv".to_owned()),
                file_id: None,
            })
        );
    }

    fn sha1_hex(bytes: &[u8]) -> String {
        use sha1::{Digest as _, Sha1};
        hex::encode(Sha1::digest(bytes))
    }

    /// An application whose catalogs both answer from `server`. CurseForge
    /// holds a key only when `keyed`, which is the no-credential case otherwise.
    fn app_against(base: &Path, server: &MockServer, keyed: bool) -> App {
        let mut app = test_app(base);
        app.use_loopback_catalog(&format!("{}/v2", server.uri()))
            .expect("a loopback modrinth");
        let store = MemoryStore::new();
        if keyed {
            ephervex_auth::set_curseforge_api_key(
                &store,
                &SecretString::new("test-curseforge-key-do-not-use"),
            )
            .expect("store the key");
        }
        app.use_loopback_curseforge(&format!("{}/v1", server.uri()), Arc::new(store))
            .expect("a loopback curseforge");
        app
    }

    /// Installs an archive of `jars` and returns the recipe the server keeps.
    async fn server_recipe(
        app: &App,
        dir: &Path,
        jars: &[(&str, &[u8])],
        client_source: Option<&Path>,
    ) -> PackRecipe {
        let archive = dir.join("pack-server.zip");
        zip_file(&archive, jars);
        let server_source = dir.join("server-source");
        app.install_server_archive(ServerArchiveInstall {
            archive: &archive,
            name: "Repro Server",
            minecraft_version: "1.21.1",
            loader: &neoforge(),
            launch: None,
            game_dir: &dir.join("server"),
            server_source: &server_source,
            client_source,
            reporter: None,
            cancel: &CancellationToken::new(),
        })
        .await
        .expect("install");
        SourceStore::new(&server_source)
            .read_recipe()
            .expect("the written recipe")
    }

    fn relations_of<'a>(recipe: &'a PackRecipe, name: &str) -> Option<&'a [RecipeRelation]> {
        recipe
            .files
            .iter()
            .find(|file| file.file_name() == name)
            .unwrap_or_else(|| panic!("{name} is recorded"))
            .recorded_relations()
    }

    fn declared(kind: ModRelationKind, project: &str) -> ModDependency {
        ModDependency {
            kind,
            project_id: Some(project.to_owned()),
            version_id: None,
            file_name: None,
        }
    }

    const FEATURE: &[u8] = b"feature";
    const LIBRARY: &[u8] = b"library";
    const MODDED: &[u8] = b"modded";
    const MYSTERY: &[u8] = b"mystery";

    fn archive_jars() -> [(&'static str, &'static [u8]); 4] {
        [
            ("mods/feature.jar", FEATURE),
            ("mods/library.jar", LIBRARY),
            ("mods/modded.jar", MODDED),
            ("mods/mystery.jar", MYSTERY),
        ]
    }

    async fn curseforge_recognizes_feature_and_library(server: &MockServer) {
        let (feature, library) = (sha1_hex(FEATURE), sha1_hex(LIBRARY));
        Mock::given(method("POST"))
            .and(path("/v1/fingerprints"))
            .respond_with(ResponseTemplate::new(200).set_body_string(format!(
                r#"{{"data":{{"exactMatches":[{{"id":238222,"file":{{"id":4712354,"modId":238222,"isAvailable":true,"fileName":"feature.jar","hashes":[{{"value":"{feature}","algo":1}}],"dependencies":[{{"modId":306612,"relationType":3}},{{"modId":32274,"relationType":2}}]}}}},{{"id":306612,"file":{{"id":4593548,"modId":306612,"isAvailable":true,"fileName":"library.jar","hashes":[{{"value":"{library}","algo":1}}],"dependencies":[]}}}}]}}}}"#
            )))
            .mount(server)
            .await;
    }

    async fn modrinth_recognizes_modded(server: &MockServer, dependencies: &str) {
        let modded = sha1_hex(MODDED);
        Mock::given(method("POST"))
            .and(path("/v2/version_files"))
            .respond_with(ResponseTemplate::new(200).set_body_string(format!(
                r#"{{"{modded}":{{"id":"vModded","project_id":"MODDED01","date_published":"2024-01-01T00:00:00Z","files":[],"dependencies":[{dependencies}]}}}}"#
            )))
            .mount(server)
            .await;
    }

    /// The answers that identify a server archive's jars already say what each
    /// exact file declares, so the recipe records it without another request.
    #[tokio::test]
    async fn a_server_recipe_records_what_each_identifying_answer_declared() {
        let directory = tempfile::tempdir().expect("temp dir");
        let server = MockServer::start().await;
        curseforge_recognizes_feature_and_library(&server).await;
        modrinth_recognizes_modded(
            &server,
            r#"{"project_id":"FEATURE1","dependency_type":"required"}"#,
        )
        .await;
        let app = app_against(directory.path(), &server, true);

        let recipe = server_recipe(&app, directory.path(), &archive_jars(), None).await;

        assert_eq!(
            relations_of(&recipe, "feature.jar"),
            Some(
                &recipe_relations(&[
                    declared(ModRelationKind::Required, "306612"),
                    declared(ModRelationKind::Optional, "32274"),
                ])[..]
            )
        );
        assert_eq!(
            relations_of(&recipe, "library.jar"),
            Some(&[][..]),
            "curseforge answered that this file declares nothing"
        );
        assert_eq!(
            relations_of(&recipe, "modded.jar"),
            Some(&recipe_relations(&[declared(ModRelationKind::Required, "FEATURE1")])[..])
        );
        assert_eq!(
            relations_of(&recipe, "mystery.jar"),
            None,
            "nothing answered for this jar, so nothing is known about it"
        );
        assert_eq!(
            server.received_requests().await.unwrap_or_default().len(),
            2,
            "one request per catalog, the same ones that identify the jars"
        );
    }

    /// A catalog that cannot be asked leaves its jars unasked rather than
    /// declaring nothing, and costs no other catalog's answers.
    #[tokio::test]
    async fn an_unavailable_catalog_leaves_its_jars_unasked() {
        let directory = tempfile::tempdir().expect("temp dir");
        let server = MockServer::start().await;
        curseforge_recognizes_feature_and_library(&server).await;
        modrinth_recognizes_modded(&server, "").await;
        let app = app_against(directory.path(), &server, false);

        let recipe = server_recipe(&app, directory.path(), &archive_jars(), None).await;

        assert_eq!(relations_of(&recipe, "feature.jar"), None);
        assert_eq!(relations_of(&recipe, "library.jar"), None);
        assert_eq!(relations_of(&recipe, "mystery.jar"), None);
        assert_eq!(
            relations_of(&recipe, "modded.jar"),
            Some(&[][..]),
            "modrinth still answered for its own jar"
        );
    }

    /// A linked client's record is copied as it stands: what it recorded,
    /// what it recorded as nothing, and what it never asked.
    #[tokio::test]
    async fn a_linked_clients_relation_knowledge_is_copied_as_it_stands() {
        let directory = tempfile::tempdir().expect("temp dir");
        let server = MockServer::start().await;
        let app = app_against(directory.path(), &server, true);
        let requires = recipe_relations(&[declared(ModRelationKind::Required, "306612")]);
        let client_source = directory.path().join("client-source");
        let mut client =
            PackRecipe::for_manual_instance("Repro", "1.21.1", LoaderSelection::vanilla());
        for (name, bytes, relations) in [
            ("feature", FEATURE, Some(requires.clone())),
            ("library", LIBRARY, Some(Vec::new())),
            ("modded", MODDED, None),
        ] {
            client.files.push(ephervex_pack::PackFileRecipe {
                logical_path: PathBuf::from(format!("mods/{name}.jar")),
                hashes: vec![Hash::new(HashAlgorithm::Sha1, &sha1_hex(bytes)).expect("sha1")],
                downloads: Vec::new(),
                size: None,
                client: SideRequirement::Required,
                server: SideRequirement::Required,
                selected: None,
                origin: ephervex_pack::ModOrigin::PackDeclared,
                identity: Some(curseforge_identity("238222", name)),
                original_identity: None,
                relations,
            });
        }
        SourceStore::new(&client_source)
            .write_recipe(&client)
            .expect("write the client recipe");

        let recipe = server_recipe(
            &app,
            directory.path(),
            &[
                ("mods/feature.jar", FEATURE),
                ("mods/library.jar", LIBRARY),
                ("mods/modded.jar", MODDED),
            ],
            Some(&client_source),
        )
        .await;

        assert_eq!(relations_of(&recipe, "feature.jar"), Some(&requires[..]));
        assert_eq!(relations_of(&recipe, "library.jar"), Some(&[][..]));
        assert_eq!(
            relations_of(&recipe, "modded.jar"),
            None,
            "the client never asked, and the server does not pretend it did"
        );
        assert!(
            server
                .received_requests()
                .await
                .unwrap_or_default()
                .is_empty(),
            "every jar was answered offline"
        );
    }

    #[test]
    fn the_record_names_the_archive_a_server_was_built_from() {
        let source = archive_import_source(Path::new("/tmp/Repro_server_pack_1.2.3.zip"));

        assert_eq!(source.kind, ImportSourceKind::ServerPack);
        assert_eq!(
            source.reference.as_deref(),
            Some("Repro_server_pack_1.2.3.zip"),
            "the file name is evidence for a person, not an address to re-read"
        );
    }
}
