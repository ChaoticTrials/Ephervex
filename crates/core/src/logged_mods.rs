//! Rebuilding a pack from the hashed mod list a crash report or log carries.
//!
//! Each hashed jar is looked up by its digest: Modrinth by SHA-1, CurseForge by
//! its Murmur2 fingerprint, which only counts as a match when CurseForge's own
//! SHA-1 for the file agrees. The catalog the user prefers is asked first and
//! the other one only about what the first did not know. A jar neither knows is
//! left out and named on the instance, never replaced by a guess.
//!
//! CurseForge matches become pending pack files and go through the same exact
//! resolution a CurseForge export does, so a file whose author forbids
//! third-party downloads is offered for manual download the same way.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ephervex_download::{AuthenticatedSource, OperationReporter, hash_file};
use ephervex_mod_platform::{
    CurseForgeOperation, ModPlatformError, ModProviderApi as _, ModVersionInfo, RecognizedFile,
    SideSupport,
};
use ephervex_model::{AttentionReason, Hash, HashAlgorithm, ModOrigin, ModProvider};
use ephervex_pack::{
    BlockedProviderFile, LoggedModRow, LoggedSide, MODS_DIR, PackAnalysis, PackError,
    PackFileRecipe, PackSide, PendingProviderFile, ProviderFileIdentity, SideRequirement,
    disabled_path, safe_file_name,
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, instrument, warn};

use crate::app::App;
use crate::curseforge_pack::DeclaredBy;
use crate::error::CoreResult;

/// How many digests one catalog request carries.
const LOOKUP_BATCH: usize = 100;

impl App {
    /// Turns the analysis's hashed mod list into recipe entries.
    ///
    /// Returns the CurseForge credential when CurseForge was asked, for the
    /// downloads that follow.
    #[instrument(skip_all)]
    pub(crate) async fn resolve_logged_mods(
        &self,
        analysis: &mut PackAnalysis,
        skip_blocked: &[ProviderFileIdentity],
        curseforge: Option<&CurseForgeOperation<'_>>,
        cancel: &CancellationToken,
    ) -> CoreResult<Option<Arc<AuthenticatedSource>>> {
        // Worked on a copy, so a failure part way leaves the analysis as it
        // was and a retry starts from the same place.
        let mut working = analysis.clone();
        let credential = self
            .resolve_logged_mods_in(&mut working, skip_blocked, curseforge, cancel)
            .await?;
        *analysis = working;
        Ok(credential)
    }

    async fn resolve_logged_mods_in(
        &self,
        analysis: &mut PackAnalysis,
        skip_blocked: &[ProviderFileIdentity],
        curseforge: Option<&CurseForgeOperation<'_>>,
        cancel: &CancellationToken,
    ) -> CoreResult<Option<Arc<AuthenticatedSource>>> {
        let Some(logged) = analysis.logged_mods.clone() else {
            return Ok(None);
        };
        if !logged.requested || logged.resolved {
            return Ok(None);
        }
        let side = logged.list.side;
        // A pack carrying its own log already has some of these jars, byte
        // for byte, so only the rest are looked up.
        let mut seen = sha1_digests(
            analysis
                .recipe
                .files
                .iter()
                // An optional file the user may not take is looked up too.
                .filter(|file| file.requirement(PackSide::Client) == SideRequirement::Required)
                .flat_map(|file| &file.hashes)
                .chain(analysis.blocked.iter().flat_map(|file| &file.hashes))
                .chain(
                    logged
                        .shipped_jars
                        .iter()
                        .filter_map(|jar| jar.sha1.as_ref()),
                ),
        );
        let pack_files = analysis.recipe.files.len();
        let mut wanted: Vec<LoggedModRow> = logged
            .list
            .hashed()
            .filter(|row| {
                row.sha1
                    .as_ref()
                    .is_some_and(|sha1| seen.insert(sha1.digest().to_owned()))
            })
            .cloned()
            .collect();

        let first = self.browsing_catalog()?;
        let second = match first {
            ModProvider::CurseForge => ModProvider::Modrinth,
            _ => ModProvider::CurseForge,
        };
        let mut credential = None;
        let mut not_asked = Vec::new();
        // Blocked CurseForge files held back while the other catalog is asked
        // whether it can serve the same bytes.
        let mut held_blocked = Vec::new();
        for (position, provider) in [first, second].into_iter().enumerate() {
            if wanted.is_empty() || cancel.is_cancelled() {
                break;
            }
            let another_follows = position == 0;
            match provider {
                ModProvider::Modrinth => {
                    let found = self.find_on_modrinth(&wanted, side, cancel).await?;
                    let matched: BTreeSet<String> =
                        found.iter().map(|(digest, _)| digest.clone()).collect();
                    keep_unmatched(&mut wanted, &matched);
                    analysis
                        .recipe
                        .files
                        .extend(found.into_iter().map(|(_, entry)| entry));
                }
                ModProvider::CurseForge => {
                    let Some((authenticated, held)) = self
                        .find_on_curseforge(
                            curseforge,
                            analysis,
                            &mut wanted,
                            skip_blocked,
                            another_follows,
                            cancel,
                        )
                        .await?
                    else {
                        not_asked.push(ModProvider::CurseForge);
                        continue;
                    };
                    credential = Some(authenticated);
                    held_blocked = held;
                }
                _ => {}
            }
        }
        restore_held_blocked(analysis, held_blocked, &mut wanted);
        if cancel.is_cancelled() {
            return Err(PackError::Canceled.into());
        }
        settle_found_files(analysis, &logged, pack_files, &mut wanted);

        record_missing(analysis, wanted, not_asked);
        Ok(credential)
    }

    /// Asks CurseForge about `wanted` and resolves its matches into the
    /// analysis, leaving in `wanted` every row that did not become a file.
    ///
    /// `None` when no key is configured: CurseForge cannot be asked, and that
    /// is not a reason to refuse what the other catalog found. With
    /// `hold_blocked`, blocked matches are returned rather than kept, and their
    /// rows stay wanted, so the other catalog can be asked for the same bytes.
    async fn find_on_curseforge(
        &self,
        held: Option<&CurseForgeOperation<'_>>,
        analysis: &mut PackAnalysis,
        wanted: &mut Vec<LoggedModRow>,
        skip_blocked: &[ProviderFileIdentity],
        hold_blocked: bool,
        cancel: &CancellationToken,
    ) -> CoreResult<Option<(Arc<AuthenticatedSource>, Vec<BlockedProviderFile>)>> {
        let begun;
        let operation = if let Some(held) = held {
            held
        } else {
            begun = match self.curseforge_api()?.begin_operation() {
                Ok(operation) => operation,
                Err(ModPlatformError::CredentialMissing { .. }) => {
                    debug!("no CurseForge key, so the mod list was not looked up there");
                    return Ok(None);
                }
                Err(error) => return Err(error.into()),
            };
            &begun
        };
        let side = analysis
            .logged_mods
            .as_ref()
            .map_or(LoggedSide::Client, |logged| logged.list.side);
        let pending = fingerprint_matches(operation, wanted, side, cancel).await?;
        let blocked_before = analysis.blocked.len();
        analysis
            .pending
            .extend(pending.iter().map(|(_, entry)| entry.clone()));
        self.resolve_curseforge_pending(
            operation,
            analysis,
            skip_blocked,
            DeclaredBy::ModList,
            cancel,
        )
        .await?;
        let credential = self.curseforge_artifact_credential(operation.credential().clone())?;
        let held = if hold_blocked {
            analysis.blocked.split_off(blocked_before)
        } else {
            Vec::new()
        };

        // A match counts once it became a file, a blocked file, or a file the
        // caller chose to go without. One CurseForge would not describe, one
        // that lost to another file of its project, and a held blocked one are
        // still wanted.
        let mut landed = sha1_digests(analysis.recipe.files.iter().flat_map(|file| &file.hashes));
        landed.extend(sha1_digests(
            analysis.blocked.iter().flat_map(|file| &file.hashes),
        ));
        landed.extend(
            pending
                .iter()
                .filter(|(_, entry)| skip_blocked.contains(&entry.identity))
                .map(|(digest, _)| digest.clone()),
        );
        keep_unmatched(wanted, &landed);
        Ok(Some((credential, held)))
    }

    /// The Modrinth releases these rows are, as recipe entries keyed by the
    /// row's SHA-1.
    async fn find_on_modrinth(
        &self,
        rows: &[LoggedModRow],
        side: LoggedSide,
        cancel: &CancellationToken,
    ) -> CoreResult<Vec<(String, PackFileRecipe)>> {
        let catalog = self.modrinth_catalog()?;
        let digests: Vec<Hash> = rows.iter().filter_map(|row| row.sha1.clone()).collect();
        let mut recognized = Vec::new();
        for chunk in digests.chunks(LOOKUP_BATCH) {
            recognized.extend(catalog.recognize(chunk, cancel).await?);
        }
        if recognized.is_empty() {
            return Ok(Vec::new());
        }
        let mut ids: Vec<String> = recognized
            .iter()
            .map(|(_, file)| file.version_id.clone())
            .collect();
        ids.sort_unstable();
        ids.dedup();
        let reporter = OperationReporter::silent("look-up-logged-mods");
        let mut versions = BTreeMap::new();
        for chunk in ids.chunks(LOOKUP_BATCH) {
            for version in catalog.versions_by_id(chunk, &reporter, cancel).await? {
                versions.insert(version.id.clone(), version);
            }
        }

        let hosts = self.modrinth_artifact_hosts();
        let mut found = Vec::new();
        for (digest, file) in recognized {
            let Some(version) = versions.get(&file.version_id).filter(|version| {
                version
                    .file
                    .hashes
                    .iter()
                    .any(|hash| hash.algorithm == HashAlgorithm::Sha1 && hash.matches(&digest))
            }) else {
                // The digest belongs to a release's secondary file, which a
                // primary-file lookup cannot install as the same bytes.
                continue;
            };
            let Some(url) = version.file.distribution.download_url() else {
                continue;
            };
            // A name or address this launcher will not use leaves the jar to
            // the other catalog rather than failing the rest.
            let Some(name) = safe_file_name(&version.file.file_name) else {
                debug!(file = %version.file.file_name, "left out a release with an unsafe file name");
                continue;
            };
            let url = match hosts.validate(url) {
                Ok(url) => url,
                Err(error) => {
                    debug!(%error, "left out a release served from a host outside the allowlist");
                    continue;
                }
            };
            found.push((
                digest,
                logged_entry(
                    version,
                    &file,
                    PathBuf::from(MODS_DIR).join(name),
                    url.to_string(),
                    side,
                ),
            ));
        }
        Ok(found)
    }
}

/// The CurseForge files these rows are, as pending pack files keyed by the
/// row's SHA-1.
async fn fingerprint_matches(
    operation: &ephervex_mod_platform::CurseForgeOperation<'_>,
    rows: &[LoggedModRow],
    side: LoggedSide,
    cancel: &CancellationToken,
) -> CoreResult<Vec<(String, PendingProviderFile)>> {
    let wanted: Vec<(u32, Hash)> = rows
        .iter()
        .filter_map(|row| Some((row.murmur2?, row.sha1.clone()?)))
        .collect();
    let mut found = Vec::new();
    for chunk in wanted.chunks(LOOKUP_BATCH) {
        for (digest, file) in operation.fingerprints(chunk, cancel).await? {
            found.push((
                digest,
                PendingProviderFile {
                    identity: ProviderFileIdentity {
                        provider: Some(ModProvider::CurseForge),
                        project_id: Some(file.project_id.clone()),
                        version_id: Some(file.version_id.clone()),
                        file_id: file.file_id.clone(),
                    },
                    client: client_requirement(side, file.client_side),
                    server: match side {
                        LoggedSide::Server => SideRequirement::Required,
                        LoggedSide::Client => SideRequirement::Unknown,
                    },
                },
            ));
        }
    }
    Ok(found)
}

/// What making an instance match a mod list did to the jars it does not name,
/// each relative to the game directory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnlistedMods {
    pub disabled: Vec<PathBuf>,
    /// Jars the list does not name that stayed enabled: their disabled name
    /// was taken, or they could not be read.
    pub left_enabled: Vec<PathBuf>,
}

/// Disables every jar in `mods/` that `list` does not name.
///
/// A jar is named by its SHA-1, or, for one the report could not hash, by its
/// file name. A second jar with the same bytes as one already kept is
/// disabled too. A jar the recipe installed is known by the hash it was
/// verified against, and a jar the pack ships by the hash taken from the
/// archive; only the others are read.
pub(crate) async fn disable_unlisted_mods(
    game_dir: &Path,
    logged: &ephervex_pack::LoggedMods,
    recipe: &ephervex_pack::PackRecipe,
    cancel: &CancellationToken,
) -> CoreResult<UnlistedMods> {
    let list = &logged.list;
    let listed: BTreeSet<String> =
        sha1_digests(list.rows.iter().filter_map(|row| row.sha1.as_ref()));
    // Shipped jars first, so a declared file at the same path, which the
    // installer laid down and the layer then replaced, is not trusted.
    let mut verified: BTreeMap<PathBuf, String> = BTreeMap::new();
    for jar in &logged.shipped_jars {
        if let Some(sha1) = &jar.sha1 {
            verified.insert(
                game_dir.join(MODS_DIR).join(&jar.file_name),
                sha1.digest().to_owned(),
            );
        }
    }
    for file in &recipe.files {
        if let Some(sha1) = file
            .hashes
            .iter()
            .find(|hash| hash.algorithm == HashAlgorithm::Sha1)
        {
            verified
                .entry(game_dir.join(&file.logical_path))
                .or_insert_with(|| sha1.digest().to_owned());
        }
    }
    let mods = game_dir.join(MODS_DIR);
    let unreadable = |source| PackError::Unreadable {
        path: mods.clone(),
        source,
    };
    let mut names = Vec::new();
    match tokio::fs::read_dir(&mods).await {
        Ok(mut entries) => {
            while let Some(entry) = entries.next_entry().await.map_err(unreadable)? {
                names.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(UnlistedMods::default());
        }
        Err(source) => return Err(unreadable(source).into()),
    }
    names.sort();

    let mut outcome = UnlistedMods::default();
    let mut kept = BTreeSet::new();
    for name in names {
        if cancel.is_cancelled() {
            return Err(PackError::Canceled.into());
        }
        let path = mods.join(&name);
        let relative = PathBuf::from(MODS_DIR).join(&name);
        let is_jar = path.extension().is_some_and(|ext| ext == "jar")
            && tokio::fs::metadata(&path)
                .await
                .is_ok_and(|metadata| metadata.is_file());
        if !is_jar
            || list
                .rows
                .iter()
                .any(|row| row.sha1.is_none() && row.names_file(&name))
        {
            continue;
        }
        let digest = match verified.get(&path) {
            Some(digest) => digest.clone(),
            None => match hash_file(&path, HashAlgorithm::Sha1).await {
                Ok(hash) => hash.digest().to_owned(),
                // One unreadable jar is reported, not a reason to undo the
                // instance.
                Err(error) => {
                    warn!(%error, jar = %name, "left a jar that could not be read enabled");
                    outcome.left_enabled.push(relative);
                    continue;
                }
            },
        };
        // A listed jar stays, once: the pack may ship the same bytes the
        // catalog lookup also installed.
        if listed.contains(&digest) && kept.insert(digest) {
            continue;
        }
        let target = disabled_path(&path);
        let renamed = {
            let (from, to) = (path.clone(), target);
            tokio::task::spawn_blocking(move || crate::mods::rename_without_replacing(&from, &to))
                .await
                .map_err(|_| PackError::WorkerFailed)?
        };
        match renamed {
            Ok(()) => outcome.disabled.push(relative),
            // The pack ships a disabled copy under that name already, which is
            // its file and is not replaced. The jar stays as it is.
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                warn!(jar = %name, "left a jar the mod list does not name enabled, since its disabled name is taken");
                outcome.left_enabled.push(relative);
            }
            Err(source) => {
                return Err(PackError::Unwritable {
                    path: path.clone(),
                    source,
                }
                .into());
            }
        }
    }
    Ok(outcome)
}

/// Puts back the blocked CurseForge files the other catalog could not serve
/// either, for a manual download, and stops wanting their rows.
fn restore_held_blocked(
    analysis: &mut PackAnalysis,
    held: Vec<BlockedProviderFile>,
    wanted: &mut Vec<LoggedModRow>,
) {
    for blocked in held {
        let names = |row: &LoggedModRow| {
            row.sha1
                .as_ref()
                .is_some_and(|sha1| blocked.hashes.contains(sha1))
        };
        if wanted.iter().any(names) {
            wanted.retain(|row| !names(row));
            analysis.blocked.push(blocked);
        }
    }
}

/// Names the rows no catalog could provide on the analysis, and marks the
/// list as looked up.
fn record_missing(
    analysis: &mut PackAnalysis,
    missing: Vec<LoggedModRow>,
    not_asked: Vec<ModProvider>,
) {
    info!(missing = missing.len(), "looked up a mod list's hashes");
    for row in &missing {
        analysis
            .attention
            .push(AttentionReason::MissingLoggedFile(row.label()));
    }
    if let Some(logged) = analysis.logged_mods.as_mut() {
        logged.resolved = true;
        logged.catalogs_not_asked = not_asked;
    }
    analysis.missing_logged_files = missing;
}

/// Fits the files found for the list in with the pack's own.
///
/// The list names the version the game ran, so a pack file, declared or
/// blocked, of a project a found file belongs to is dropped, unless the list
/// names that file's own bytes too. Each destination then holds one file:
/// a name the pack ships in its overrides stays the pack's, a found file comes
/// before a pack file of the same name, and the jar behind a found file that
/// cannot be placed is wanted again, which names it as missing.
///
/// `pack_files` is how many of the recipe's files the pack declared; the rest
/// were found for the list.
fn settle_found_files(
    analysis: &mut PackAnalysis,
    logged: &ephervex_pack::LoggedMods,
    pack_files: usize,
    wanted: &mut Vec<LoggedModRow>,
) {
    let listed = sha1_digests(logged.list.rows.iter().filter_map(|row| row.sha1.as_ref()));
    let project = |identity: Option<&ProviderFileIdentity>| {
        let identity = identity?;
        Some((identity.provider?, identity.project_id.clone()?))
    };
    let found = analysis
        .recipe
        .files
        .split_off(pack_files.min(analysis.recipe.files.len()));

    // Found files are placed first: a name the pack ships stays the pack's,
    // and among found files the first takes a name.
    let mut paths: BTreeSet<PathBuf> = logged
        .shipped_jars
        .iter()
        .map(|jar| PathBuf::from(MODS_DIR).join(&jar.file_name))
        .collect();
    let mut displaced = BTreeSet::new();
    let mut placed = Vec::with_capacity(found.len());
    for entry in found {
        if paths.insert(entry.logical_path.clone()) {
            placed.push(entry);
        } else {
            displaced.extend(sha1_digests(entry.hashes.iter()));
        }
    }

    // Only a placed file supersedes the pack's version of its project.
    let placed_projects: BTreeSet<(ModProvider, String)> = placed
        .iter()
        .filter_map(|entry| project(entry.identity.as_ref()))
        .collect();
    let superseded = |identity: Option<&ProviderFileIdentity>, hashes: &[Hash]| {
        project(identity).is_some_and(|project| placed_projects.contains(&project))
            && sha1_digests(hashes.iter()).is_disjoint(&listed)
    };
    analysis
        .recipe
        .files
        .retain(|entry| !superseded(entry.identity.as_ref(), &entry.hashes));
    // A blocked file is kept only when the list names it: one it does not
    // name would be disabled anyway, so it is not asked for by hand.
    // A blocked file without a SHA-1 cannot be told apart, so it stays.
    analysis.blocked.retain(|blocked| {
        let digests = sha1_digests(blocked.hashes.iter());
        digests.is_empty() || !digests.is_disjoint(&listed)
    });
    analysis.recipe.files.retain(|entry| {
        let unique = paths.insert(entry.logical_path.clone());
        if !unique {
            // A pack file the list names is wanted again too.
            displaced.extend(sha1_digests(entry.hashes.iter()));
        }
        unique
    });
    analysis.recipe.files.extend(placed);
    // A blocked file is placed by name once it is supplied by hand.
    analysis.blocked.retain(|blocked| {
        let unique = blocked
            .file_name
            .as_deref()
            .and_then(safe_file_name)
            .is_none_or(|name| paths.insert(PathBuf::from(MODS_DIR).join(name)));
        if !unique {
            displaced.extend(sha1_digests(blocked.hashes.iter()));
        }
        unique
    });
    let mut seen = sha1_digests(wanted.iter().filter_map(|row| row.sha1.as_ref()));
    wanted.extend(
        logged
            .list
            .hashed()
            .filter(|row| {
                row.sha1.as_ref().is_some_and(|sha1| {
                    displaced.contains(sha1.digest()) && seen.insert(sha1.digest().to_owned())
                })
            })
            .cloned(),
    );
}

/// Drops the rows whose SHA-1 is in `matched`, a set of lowercase digests.
fn keep_unmatched(rows: &mut Vec<LoggedModRow>, matched: &BTreeSet<String>) {
    rows.retain(|row| {
        row.sha1
            .as_ref()
            .is_none_or(|sha1| !matched.contains(sha1.digest()))
    });
}

/// The lowercase SHA-1 digests among `hashes`.
fn sha1_digests<'a>(hashes: impl Iterator<Item = &'a Hash>) -> BTreeSet<String> {
    hashes
        .filter(|hash| hash.algorithm == HashAlgorithm::Sha1)
        .map(|hash| hash.digest().to_owned())
        .collect()
}

/// Whether the client installs a jar the report listed.
///
/// A client report lists what the client loaded, server-only mods included,
/// because a single-player world runs them in the integrated server. A server
/// report lists the server's mods, and one the catalog says no client runs is
/// left to the server.
fn client_requirement(side: LoggedSide, client_side: SideSupport) -> SideRequirement {
    match (side, client_side) {
        (LoggedSide::Server, SideSupport::Unsupported) => SideRequirement::Unsupported,
        _ => SideRequirement::Required,
    }
}

/// One Modrinth release as the recipe entry that installs it.
fn logged_entry(
    version: &ModVersionInfo,
    file: &RecognizedFile,
    logical_path: PathBuf,
    url: String,
    side: LoggedSide,
) -> PackFileRecipe {
    PackFileRecipe {
        logical_path,
        hashes: version.file.hashes.clone(),
        downloads: vec![url],
        size: version.file.size,
        client: client_requirement(side, file.client_side),
        server: match side {
            // The server ran it, whatever the catalog says.
            LoggedSide::Server => SideRequirement::Required,
            LoggedSide::Client => crate::mods::side_requirement(file.server_side),
        },
        selected: None,
        origin: ModOrigin::PackDeclared,
        identity: Some(ProviderFileIdentity {
            provider: Some(ModProvider::Modrinth),
            project_id: Some(version.project_id.clone()),
            version_id: Some(version.id.clone()),
            file_id: None,
        }),
        original_identity: None,
        installed_despite_unsupported: None,
        relations: Some(crate::mods::recipe_relations(&version.dependencies)),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::path::Path;

    use ephervex_auth::MemoryStore;
    use ephervex_model::{ImportSourceKind, SecretString, StorageClass};
    use ephervex_storage::Paths;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::error::CoreError;
    use crate::import::{ImportRequest, ImportedPack};

    const ALPHA: &[u8] = b"alpha mod jar";
    const BRAVO: &[u8] = b"bravo mod jar";
    const CHARLIE: &[u8] = b"charlie mod jar";
    const BRAVO_PROJECT: u32 = 238_222;
    const BRAVO_FILE: u32 = 4_712_354;
    const MODS_CLASS_ID: u32 = 6;
    const RESOURCE_PACKS_CLASS_ID: u32 = 12;

    fn sha1_hex(bytes: &[u8]) -> String {
        use sha1::{Digest as _, Sha1};
        hex::encode(Sha1::digest(bytes))
    }

    fn row(file: &str, id: &str, body: &[u8]) -> String {
        format!(
            "\t\t{file:<50.50}|{id:<30.30}|{id:<30.30}|1.0                 |Manifest: NOSIGNATURE|SHA1: {}|Murmur2: {}",
            sha1_hex(body),
            ephervex_mod_platform::curseforge_fingerprint(body)
        )
    }

    /// A crash report in the shape the game writes, with LibX's hashed table.
    fn report(kind: &str, rows: &[String]) -> String {
        format!(
            "---- Minecraft Crash Report ----\n// Oops.\n\n-- System Details --\nDetails:\n\tMinecraft Version: 1.21.1\n\tType: {kind}\n\tNeoForge: 21.1.77\n\tMod List: \n{}\n\tnested-1.0.jar                                    |Nested                        |nested                        |1.0                 |Manifest: NOSIGNATURE|SHA1: -                                       |Murmur2: -\n\tCrash Report UUID: 1\n",
            rows.join("\n")
        )
    }

    fn client_report() -> String {
        report(
            "Integrated Server (map_client.txt)",
            &[
                row("alpha-1.0.jar", "alpha", ALPHA),
                row("bravo-1.0.jar", "bravo", BRAVO),
                row("charlie-1.0.jar", "charlie", CHARLIE),
            ],
        )
    }

    fn app_against(base: &Path, server: &MockServer, keyed: bool) -> App {
        let mut app = App::open_at(Paths::rooted_at(base)).expect("open app");
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

    /// Modrinth knows ALPHA, served as a full release with its bytes.
    async fn modrinth_knows_alpha(server: &MockServer, environment: &str) {
        modrinth_knows(server, &[(ALPHA, "alpha")], environment).await;
    }

    /// Modrinth knows each `(body, name)` jar as release `v<name>` of project
    /// `<NAME>`, and serves its bytes.
    async fn modrinth_knows(server: &MockServer, jars: &[(&[u8], &str)], environment: &str) {
        let mut files = serde_json::Map::new();
        let mut versions = Vec::new();
        for (body, name) in jars {
            let digest = sha1_hex(body);
            let (version, project) = (format!("v{name}"), name.to_ascii_uppercase());
            files.insert(
                digest.clone(),
                json!({
                    "id": version, "project_id": project,
                    "date_published": "2024-01-01T00:00:00Z",
                    "environment": environment,
                    "files": [], "dependencies": []
                }),
            );
            versions.push(json!({
                "id": version, "project_id": project, "name": format!("{name} 1.0"),
                "version_number": "1.0", "version_type": "release",
                "game_versions": ["1.21.1"], "loaders": ["neoforge"],
                "date_published": "2024-01-01T00:00:00Z",
                "files": [{
                    "hashes": {"sha1": digest},
                    "url": format!("{}/files/{name}-1.0.jar", server.uri()),
                    "filename": format!("{name}-1.0.jar"), "primary": true, "size": body.len(),
                }],
                "dependencies": []
            }));
            serve_bytes(server, &format!("/files/{name}-1.0.jar"), body).await;
        }
        Mock::given(method("POST"))
            .and(path("/v2/version_files"))
            .respond_with(ResponseTemplate::new(200).set_body_json(files))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v2/versions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(versions))
            .mount(server)
            .await;
    }

    /// CurseForge knows BRAVO, with the environment `game_versions` declares.
    async fn curseforge_knows_bravo(server: &MockServer, game_versions: &[&str]) {
        curseforge_matches_bravo(server, game_versions, Bravo::Served).await;
    }

    /// What CurseForge does with the BRAVO file it fingerprints.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Bravo {
        Served,
        /// The author forbids third-party downloads.
        Restricted,
        /// The exact-file lookup no longer describes it.
        Gone,
        /// Its project is a resource pack, not a mod.
        NotAMod,
    }

    async fn curseforge_matches_bravo(
        server: &MockServer,
        game_versions: &[&str],
        bravo_is: Bravo,
    ) {
        let bravo = sha1_hex(BRAVO);
        let download = (bravo_is != Bravo::Restricted)
            .then(|| format!("{}/files/bravo-1.0.jar", server.uri()));
        let file = json!({
            "id": BRAVO_FILE, "gameId": 432, "modId": BRAVO_PROJECT, "isAvailable": true,
            "displayName": "bravo-1.0.jar", "fileName": "bravo-1.0.jar",
            "releaseType": 1, "fileStatus": 4, "fileLength": BRAVO.len(),
            "downloadUrl": download,
            "gameVersions": game_versions,
            "hashes": [{"value": bravo, "algo": 1}],
            "dependencies": []
        });
        Mock::given(method("POST"))
            .and(path("/v1/fingerprints"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {"exactMatches": [{"id": BRAVO_PROJECT, "file": file.clone()}]}
            })))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/categories"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": [
                {"id": MODS_CLASS_ID, "gameId": 432, "name": "Mods", "slug": "mc-mods", "isClass": true},
                {"id": RESOURCE_PACKS_CLASS_ID, "gameId": 432, "name": "Resource Packs", "slug": "texture-packs", "isClass": true}
            ]})))
            .mount(server)
            .await;
        let class = if bravo_is == Bravo::NotAMod {
            RESOURCE_PACKS_CLASS_ID
        } else {
            MODS_CLASS_ID
        };
        Mock::given(method("POST"))
            .and(path("/v1/mods"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": [
                {"id": BRAVO_PROJECT, "gameId": 432, "name": "Bravo", "slug": "bravo", "classId": class}
            ]})))
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/mods/files"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": if bravo_is == Bravo::Gone { vec![] } else { vec![file] }
            })))
            .mount(server)
            .await;
        serve_bytes(server, "/files/bravo-1.0.jar", BRAVO).await;
    }

    async fn serve_bytes(server: &MockServer, route: &str, body: &[u8]) {
        Mock::given(method("GET"))
            .and(path(route))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(body.to_vec(), "application/java-archive"),
            )
            .mount(server)
            .await;
    }

    fn write(dir: &Path, name: &str, text: &str) -> std::path::PathBuf {
        let file = dir.join(name);
        std::fs::write(&file, text).expect("write the report");
        file
    }

    async fn import(app: &App, file: &Path) -> CoreResult<ImportedPack> {
        app.import_pack(
            &ImportRequest::new(file).with_storage(StorageClass::Temporary),
            &OperationReporter::silent("import-mod-list"),
            &CancellationToken::new(),
        )
        .await
    }

    fn mods_of(imported: &ImportedPack) -> Vec<String> {
        let mods =
            ephervex_storage::metadata::client_game_dir(&imported.record.root).join(MODS_DIR);
        let mut names: Vec<String> = std::fs::read_dir(mods)
            .map(|entries| {
                entries
                    .map(|entry| {
                        entry
                            .expect("entry")
                            .file_name()
                            .to_string_lossy()
                            .into_owned()
                    })
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    /// The SHA-1 digests one lookup endpoint was asked about, in order.
    async fn asked(server: &MockServer, route: &str) -> Vec<String> {
        server
            .received_requests()
            .await
            .expect("recorded")
            .iter()
            .filter(|request| request.url.path() == route)
            .flat_map(|request| {
                let body: serde_json::Value =
                    serde_json::from_slice(&request.body).expect("a JSON body");
                let member = if route.ends_with("fingerprints") {
                    "fingerprints"
                } else {
                    "hashes"
                };
                body[member]
                    .as_array()
                    .expect("a list")
                    .iter()
                    .map(|value| match value {
                        serde_json::Value::String(text) => text.clone(),
                        other => other.to_string(),
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    #[tokio::test]
    async fn a_crash_report_is_rebuilt_from_the_preferred_catalog_and_then_the_other() {
        let directory = tempfile::tempdir().expect("temp dir");
        let server = MockServer::start().await;
        modrinth_knows_alpha(&server, "client_and_server").await;
        curseforge_knows_bravo(&server, &["1.21.1", "NeoForge"]).await;
        let app = app_against(directory.path(), &server, true);
        let file = write(
            directory.path(),
            "crash-2026-11-02_17.08.01-client.txt",
            &client_report(),
        );

        let imported = import(&app, &file).await.expect("import");

        assert_eq!(mods_of(&imported), ["alpha-1.0.jar", "bravo-1.0.jar"]);
        assert_eq!(imported.record.source.kind, ImportSourceKind::LoggedModList);
        assert_eq!(imported.record.minecraft_version, "1.21.1");
        assert_eq!(
            imported.record.loader.version.as_deref(),
            Some("21.1.77"),
            "the exact loader build the report names"
        );
        assert_eq!(
            asked(&server, "/v2/version_files").await.len(),
            3,
            "modrinth, the default catalog, is asked about every hashed jar"
        );
        assert_eq!(
            asked(&server, "/v1/fingerprints").await,
            [
                ephervex_mod_platform::curseforge_fingerprint(BRAVO).to_string(),
                ephervex_mod_platform::curseforge_fingerprint(CHARLIE).to_string(),
            ],
            "curseforge is asked only about what modrinth did not know"
        );
        assert_eq!(
            imported.analysis.missing_logged_files.len(),
            1,
            "charlie is in neither catalog"
        );
        assert!(
            imported
                .record
                .attention
                .contains(&AttentionReason::MissingLoggedFile(
                    "charlie-1.0.jar (charlie)".to_owned()
                )),
            "{:?}",
            imported.record.attention
        );
    }

    #[tokio::test]
    async fn the_preferred_catalog_decides_which_one_is_asked_first() {
        let directory = tempfile::tempdir().expect("temp dir");
        let server = MockServer::start().await;
        modrinth_knows_alpha(&server, "client_and_server").await;
        curseforge_knows_bravo(&server, &["1.21.1"]).await;
        let mut app = app_against(directory.path(), &server, true);
        app.set_default_mod_catalog(Some(ModProvider::CurseForge))
            .expect("prefer curseforge");
        let file = write(directory.path(), "latest.log", &client_report());

        let imported = import(&app, &file).await.expect("import");

        assert_eq!(mods_of(&imported), ["alpha-1.0.jar", "bravo-1.0.jar"]);
        assert_eq!(asked(&server, "/v1/fingerprints").await.len(), 3);
        assert_eq!(
            asked(&server, "/v2/version_files").await,
            [sha1_hex(ALPHA), sha1_hex(CHARLIE)],
            "modrinth is asked only about what curseforge did not know"
        );
    }

    #[tokio::test]
    async fn without_a_curseforge_key_only_modrinth_is_asked_and_the_rest_is_named() {
        let directory = tempfile::tempdir().expect("temp dir");
        let server = MockServer::start().await;
        modrinth_knows_alpha(&server, "client_and_server").await;
        curseforge_knows_bravo(&server, &["1.21.1"]).await;
        let app = app_against(directory.path(), &server, false);
        let file = write(directory.path(), "crash-a-client.txt", &client_report());

        let imported = import(&app, &file).await.expect("import");

        assert_eq!(mods_of(&imported), ["alpha-1.0.jar"]);
        assert!(asked(&server, "/v1/fingerprints").await.is_empty());
        assert_eq!(imported.analysis.missing_logged_files.len(), 2);
        assert_eq!(
            imported
                .analysis
                .logged_mods
                .as_ref()
                .map(|logged| logged.catalogs_not_asked.clone()),
            Some(vec![ModProvider::CurseForge]),
            "the user is told which catalog was never searched"
        );
    }

    #[tokio::test]
    async fn a_server_report_keeps_server_only_mods_off_the_client() {
        let directory = tempfile::tempdir().expect("temp dir");
        let server = MockServer::start().await;
        modrinth_knows_alpha(&server, "client_and_server").await;
        curseforge_knows_bravo(&server, &["1.21.1", "Server"]).await;
        let app = app_against(directory.path(), &server, true);
        let file = write(
            directory.path(),
            "crash-2026-11-02_17.08.01-server.txt",
            &report(
                "Dedicated Server (map_server.txt)",
                &[
                    row("alpha-1.0.jar", "alpha", ALPHA),
                    row("bravo-1.0.jar", "bravo", BRAVO),
                ],
            ),
        );

        let imported = import(&app, &file).await.expect("import");

        assert_eq!(mods_of(&imported), ["alpha-1.0.jar"]);
        let bravo = imported
            .recipe()
            .files
            .iter()
            .find(|entry| entry.file_name() == "bravo-1.0.jar")
            .expect("bravo is recorded for the server");
        assert_eq!(bravo.client, SideRequirement::Unsupported);
        assert_eq!(bravo.server, SideRequirement::Required);
    }

    #[tokio::test]
    async fn the_report_is_kept_in_the_instance_under_its_own_name() {
        let directory = tempfile::tempdir().expect("temp dir");
        let server = MockServer::start().await;
        modrinth_knows_alpha(&server, "client_and_server").await;
        let app = app_against(directory.path(), &server, false);
        let name = "crash-2026-11-02_17.08.01-client.txt";
        let text = client_report();
        let file = write(directory.path(), name, &text);

        let imported = import(&app, &file).await.expect("import");

        let game = ephervex_storage::metadata::client_game_dir(&imported.record.root);
        assert_eq!(
            std::fs::read_to_string(game.join("crash-reports").join(name)).expect("kept"),
            text
        );
    }

    #[tokio::test]
    async fn a_log_is_kept_in_the_instance_as_its_latest_log() {
        let directory = tempfile::tempdir().expect("temp dir");
        let server = MockServer::start().await;
        modrinth_knows_alpha(&server, "client_and_server").await;
        let app = app_against(directory.path(), &server, false);
        let text = format!(
            "[17:02:15] [main/INFO]: Loading\n[17:08:03] [Render thread/INFO]: {}",
            client_report()
        );
        let file = write(directory.path(), "shared.log", &text);

        let imported = import(&app, &file).await.expect("import");

        let game = ephervex_storage::metadata::client_game_dir(&imported.record.root);
        assert_eq!(
            std::fs::read_to_string(game.join("logs/latest.log")).expect("kept"),
            text
        );
    }

    #[tokio::test]
    async fn a_report_without_hashes_is_refused_and_creates_nothing() {
        let directory = tempfile::tempdir().expect("temp dir");
        let server = MockServer::start().await;
        let app = app_against(directory.path(), &server, false);
        let file = write(
            directory.path(),
            "crash-a-client.txt",
            &report("Integrated Server (map_client.txt)", &[]),
        );

        let error = import(&app, &file).await.expect_err("refused");

        assert!(
            matches!(
                error,
                CoreError::Pack(PackError::ModListHashesMissing { .. })
            ),
            "{error:?}"
        );
        assert!(app.list_clients().expect("list").is_empty());
        assert!(
            server
                .received_requests()
                .await
                .unwrap_or_default()
                .is_empty()
        );
    }

    #[test]
    fn text_that_is_no_crash_report_is_reported_as_a_broken_archive() {
        let directory = tempfile::tempdir().expect("temp dir");
        let app = App::open_at(Paths::rooted_at(directory.path())).expect("open app");
        let file = write(
            directory.path(),
            "pack.mrpack",
            "<html><body>Rate limited</body></html>\n",
        );

        let error = app.analyze_pack(&file).expect_err("refused");

        assert!(
            matches!(error, CoreError::Pack(PackError::ArchiveInvalid { .. })),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn two_jars_of_one_curseforge_project_are_both_kept() {
        let directory = tempfile::tempdir().expect("temp dir");
        let server = MockServer::start().await;
        let (bravo, charlie) = (sha1_hex(BRAVO), sha1_hex(CHARLIE));
        let file = |id: u32, name: &str, digest: &str, body: &[u8]| {
            json!({
                "id": id, "gameId": 432, "modId": BRAVO_PROJECT, "isAvailable": true,
                "displayName": name, "fileName": name,
                "releaseType": 1, "fileStatus": 4, "fileLength": body.len(),
                "downloadUrl": format!("{}/files/{name}", server.uri()),
                "gameVersions": ["1.21.1"],
                "hashes": [{"value": digest, "algo": 1}],
                "dependencies": []
            })
        };
        let (main, api) = (
            file(BRAVO_FILE, "bravo-1.0.jar", &bravo, BRAVO),
            file(BRAVO_FILE + 1, "charlie-1.0.jar", &charlie, CHARLIE),
        );
        Mock::given(method("POST"))
            .and(path("/v1/fingerprints"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"data": {"exactMatches": [
                    {"id": BRAVO_PROJECT, "file": main.clone()},
                    {"id": BRAVO_PROJECT, "file": api.clone()}
                ]}}),
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/categories"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": [
                {"id": MODS_CLASS_ID, "gameId": 432, "name": "Mods", "slug": "mc-mods", "isClass": true}
            ]})))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/mods"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": [
                {"id": BRAVO_PROJECT, "gameId": 432, "name": "Bravo", "slug": "bravo", "classId": MODS_CLASS_ID}
            ]})))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/mods/files"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": [main, api]})))
            .mount(&server)
            .await;
        serve_bytes(&server, "/files/bravo-1.0.jar", BRAVO).await;
        serve_bytes(&server, "/files/charlie-1.0.jar", CHARLIE).await;
        let mut app = app_against(directory.path(), &server, true);
        app.set_default_mod_catalog(Some(ModProvider::CurseForge))
            .expect("prefer curseforge");
        let report = write(
            directory.path(),
            "crash-a-client.txt",
            &report(
                "Integrated Server (map_client.txt)",
                &[
                    row("bravo-1.0.jar", "bravo", BRAVO),
                    row("charlie-1.0.jar", "bravo_api", CHARLIE),
                ],
            ),
        );

        let imported = import(&app, &report).await.expect("import");

        assert_eq!(mods_of(&imported), ["bravo-1.0.jar", "charlie-1.0.jar"]);
        assert!(imported.analysis.missing_logged_files.is_empty());
    }

    #[tokio::test]
    async fn a_curseforge_file_that_is_gone_is_asked_of_modrinth_instead_of_failing() {
        let directory = tempfile::tempdir().expect("temp dir");
        let server = MockServer::start().await;
        modrinth_knows(
            &server,
            &[(ALPHA, "alpha"), (BRAVO, "bravo")],
            "client_and_server",
        )
        .await;
        curseforge_matches_bravo(&server, &["1.21.1"], Bravo::Gone).await;
        let mut app = app_against(directory.path(), &server, true);
        app.set_default_mod_catalog(Some(ModProvider::CurseForge))
            .expect("prefer curseforge");
        let file = write(directory.path(), "crash-a-client.txt", &client_report());

        let imported = import(&app, &file).await.expect("import");

        assert_eq!(mods_of(&imported), ["alpha-1.0.jar", "bravo-1.0.jar"]);
        assert!(
            asked(&server, "/v2/version_files")
                .await
                .contains(&sha1_hex(BRAVO)),
            "the file curseforge no longer describes is asked of modrinth"
        );
    }

    #[tokio::test]
    async fn a_curseforge_file_that_is_gone_everywhere_is_named_not_fatal() {
        let directory = tempfile::tempdir().expect("temp dir");
        let server = MockServer::start().await;
        modrinth_knows_alpha(&server, "client_and_server").await;
        curseforge_matches_bravo(&server, &["1.21.1"], Bravo::Gone).await;
        let app = app_against(directory.path(), &server, true);
        let file = write(directory.path(), "crash-a-client.txt", &client_report());

        let imported = import(&app, &file).await.expect("import");

        assert_eq!(mods_of(&imported), ["alpha-1.0.jar"]);
        assert_eq!(
            imported.analysis.missing_logged_files.len(),
            2,
            "bravo and charlie"
        );
    }

    #[tokio::test]
    async fn a_restricted_curseforge_file_is_downloaded_from_modrinth_when_it_has_the_same_bytes() {
        let directory = tempfile::tempdir().expect("temp dir");
        let server = MockServer::start().await;
        modrinth_knows(
            &server,
            &[(ALPHA, "alpha"), (BRAVO, "bravo")],
            "client_and_server",
        )
        .await;
        curseforge_matches_bravo(&server, &["1.21.1"], Bravo::Restricted).await;
        let mut app = app_against(directory.path(), &server, true);
        app.set_default_mod_catalog(Some(ModProvider::CurseForge))
            .expect("prefer curseforge");
        let file = write(directory.path(), "crash-a-client.txt", &client_report());

        let imported = import(&app, &file).await.expect("import");

        assert_eq!(mods_of(&imported), ["alpha-1.0.jar", "bravo-1.0.jar"]);
        assert!(imported.analysis.blocked.is_empty());
    }

    #[tokio::test]
    async fn a_restricted_curseforge_file_no_other_catalog_has_waits_for_a_manual_download() {
        let directory = tempfile::tempdir().expect("temp dir");
        let server = MockServer::start().await;
        modrinth_knows_alpha(&server, "client_and_server").await;
        curseforge_matches_bravo(&server, &["1.21.1"], Bravo::Restricted).await;
        let mut app = app_against(directory.path(), &server, true);
        app.set_default_mod_catalog(Some(ModProvider::CurseForge))
            .expect("prefer curseforge");
        let file = write(directory.path(), "crash-a-client.txt", &client_report());
        let mut analysis = app.analyze_pack(&file).expect("analysis");
        assert!(
            analysis.needs_provider_resolution(),
            "a mod list is not described until the catalogs are asked"
        );

        app.resolve_pack(&mut analysis, &[], &CancellationToken::new())
            .await
            .expect("resolve");

        assert!(!analysis.needs_provider_resolution());
        assert_eq!(analysis.blocked.len(), 1);
        assert_eq!(
            analysis.blocked[0].file_name.as_deref(),
            Some("bravo-1.0.jar")
        );
        assert_eq!(
            analysis.missing_logged_files.len(),
            1,
            "only charlie is unknown"
        );
        let error = import(&app, &file)
            .await
            .expect_err("blocked until decided");
        assert!(
            matches!(
                error,
                CoreError::Pack(PackError::ProviderFilesBlocked { .. })
            ),
            "{error:?}"
        );
    }

    #[test]
    fn a_binary_file_that_is_not_a_zip_is_reported_as_a_broken_archive() {
        let directory = tempfile::tempdir().expect("temp dir");
        let app = App::open_at(Paths::rooted_at(directory.path())).expect("open app");
        let file = directory.path().join("pack.mrpack");
        std::fs::write(&file, [0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00]).expect("write");

        let error = app.analyze_pack(&file).expect_err("refused");

        assert!(
            matches!(error, CoreError::Pack(PackError::ArchiveInvalid { .. })),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn a_curseforge_match_that_is_not_a_mod_is_asked_of_modrinth_instead_of_failing() {
        let directory = tempfile::tempdir().expect("temp dir");
        let server = MockServer::start().await;
        modrinth_knows(
            &server,
            &[(ALPHA, "alpha"), (BRAVO, "bravo")],
            "client_and_server",
        )
        .await;
        curseforge_matches_bravo(&server, &["1.21.1"], Bravo::NotAMod).await;
        let mut app = app_against(directory.path(), &server, true);
        app.set_default_mod_catalog(Some(ModProvider::CurseForge))
            .expect("prefer curseforge");
        let file = write(directory.path(), "crash-a-client.txt", &client_report());

        let imported = import(&app, &file).await.expect("import");

        assert_eq!(mods_of(&imported), ["alpha-1.0.jar", "bravo-1.0.jar"]);
        let resource_packs = ephervex_storage::metadata::client_game_dir(&imported.record.root)
            .join("resourcepacks");
        assert!(!resource_packs.join("bravo-1.0.jar").exists());
    }

    const ALPHA_OLD: &[u8] = b"alpha mod jar, an older build";
    const DELTA: &[u8] = b"delta mod jar the log never names";

    fn mrpack(path: &Path, entries: &[(&str, &[u8])]) {
        use std::io::Write as _;
        let file = std::fs::File::create(path).expect("create archive");
        let mut writer = zip::ZipWriter::new(file);
        let options: zip::write::FileOptions<'_, '_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        let index = br#"{"formatVersion": 1, "game": "minecraft", "versionId": "1.0",
            "name": "Repro Pack", "files": [],
            "dependencies": {"minecraft": "1.21.1", "neoforge": "21.1.77"}}"#;
        writer
            .start_file("modrinth.index.json", options)
            .expect("start index");
        writer.write_all(index).expect("write index");
        for (name, body) in entries {
            writer.start_file(*name, options).expect("start entry");
            writer.write_all(body).expect("write entry");
        }
        writer.finish().expect("finish archive");
    }

    /// A pack shipping an old alpha, bravo as logged, and delta the log does
    /// not name, with a log listing alpha's newer build, bravo, and charlie.
    fn pack_carrying_a_log(dir: &Path) -> std::path::PathBuf {
        let log = format!("[17:08:03] [Render thread/INFO]: {}", client_report());
        let archive = dir.join("repro.mrpack");
        mrpack(
            &archive,
            &[
                ("overrides/mods/alpha-0.9.jar", ALPHA_OLD),
                ("overrides/mods/bravo.jar", BRAVO),
                ("overrides/mods/delta.jar", DELTA),
                ("overrides/logs/latest.log", log.as_bytes()),
            ],
        );
        archive
    }

    async fn import_pack(app: &App, archive: &Path, apply: bool) -> ImportedPack {
        app.import_pack(
            &ImportRequest::new(archive)
                .with_storage(StorageClass::Temporary)
                .applying_logged_mods(apply),
            &OperationReporter::silent("import-pack"),
            &CancellationToken::new(),
        )
        .await
        .expect("import")
    }

    #[test]
    fn a_pack_carrying_a_hashed_log_says_so_before_import() {
        let directory = tempfile::tempdir().expect("temp dir");
        let app = App::open_at(Paths::rooted_at(directory.path())).expect("open app");
        let archive = pack_carrying_a_log(directory.path());

        let analysis = app.analyze_pack(&archive).expect("analysis");

        let logged = analysis.logged_mods.as_ref().expect("the carried log");
        assert_eq!(logged.source, "logs/latest.log");
        assert_eq!(logged.list.hashed().count(), 3);
        assert!(
            !analysis.needs_provider_resolution(),
            "a carried log is looked up only once the user asks for it"
        );
    }

    #[test]
    fn a_log_of_another_game_version_is_not_offered() {
        let directory = tempfile::tempdir().expect("temp dir");
        let app = App::open_at(Paths::rooted_at(directory.path())).expect("open app");
        let other =
            client_report().replace("Minecraft Version: 1.21.1", "Minecraft Version: 1.20.1");
        let archive = directory.path().join("repro.mrpack");
        mrpack(&archive, &[("overrides/logs/latest.log", other.as_bytes())]);

        let analysis = app.analyze_pack(&archive).expect("analysis");

        assert!(analysis.logged_mods.is_none());
    }

    #[tokio::test]
    async fn a_pack_imports_as_published_unless_asked_to_match_its_log() {
        let directory = tempfile::tempdir().expect("temp dir");
        let server = MockServer::start().await;
        modrinth_knows_alpha(&server, "client_and_server").await;
        let app = app_against(directory.path(), &server, false);
        let archive = pack_carrying_a_log(directory.path());

        let imported = import_pack(&app, &archive, false).await;

        assert_eq!(
            mods_of(&imported),
            ["alpha-0.9.jar", "bravo.jar", "delta.jar"]
        );
        assert!(imported.unlisted.disabled.is_empty());
        assert!(
            server
                .received_requests()
                .await
                .unwrap_or_default()
                .is_empty(),
            "no catalog is asked about a log the user did not choose"
        );
    }

    #[tokio::test]
    async fn a_pack_made_to_match_its_log_adds_what_it_lacks_and_disables_the_rest() {
        let directory = tempfile::tempdir().expect("temp dir");
        let server = MockServer::start().await;
        modrinth_knows_alpha(&server, "client_and_server").await;
        let app = app_against(directory.path(), &server, false);
        let archive = pack_carrying_a_log(directory.path());

        let imported = import_pack(&app, &archive, true).await;

        assert_eq!(
            mods_of(&imported),
            [
                "alpha-0.9.jar.disabled",
                "alpha-1.0.jar",
                "bravo.jar",
                "delta.jar.disabled"
            ]
        );
        assert_eq!(
            imported.unlisted.disabled,
            [
                std::path::PathBuf::from("mods/alpha-0.9.jar"),
                std::path::PathBuf::from("mods/delta.jar")
            ]
        );
        assert_eq!(
            asked(&server, "/v2/version_files").await,
            [sha1_hex(ALPHA), sha1_hex(CHARLIE)],
            "bravo ships with the pack, so it is not looked up"
        );
        assert_eq!(imported.analysis.missing_logged_files.len(), 1, "charlie");
    }

    #[test]
    fn a_pack_whose_log_has_no_hashes_offers_its_newest_crash_report() {
        let directory = tempfile::tempdir().expect("temp dir");
        let app = App::open_at(Paths::rooted_at(directory.path())).expect("open app");
        let older = report(
            "Integrated Server (map_client.txt)",
            &[row("old-1.0.jar", "old", ALPHA_OLD)],
        );
        let archive = directory.path().join("repro.mrpack");
        mrpack(
            &archive,
            &[
                (
                    "overrides/logs/latest.log",
                    b"[17:02:15] [main/INFO]: no crash here\n",
                ),
                (
                    "overrides/crash-reports/crash-2026-01-01_10.00.00-client.txt",
                    older.as_bytes(),
                ),
                (
                    "overrides/crash-reports/crash-2026-02-01_10.00.00-client.txt",
                    client_report().as_bytes(),
                ),
            ],
        );

        let analysis = app.analyze_pack(&archive).expect("analysis");

        let logged = analysis.logged_mods.as_ref().expect("the carried report");
        assert_eq!(
            logged.source,
            "crash-reports/crash-2026-02-01_10.00.00-client.txt"
        );
        assert!(logged.crash_report);
        assert_eq!(logged.list.hashed().count(), 3);
    }

    #[test]
    fn a_dedicated_server_report_in_a_client_pack_is_not_offered() {
        let directory = tempfile::tempdir().expect("temp dir");
        let app = App::open_at(Paths::rooted_at(directory.path())).expect("open app");
        let server_report = report(
            "Dedicated Server (map_server.txt)",
            &[row("alpha-1.0.jar", "alpha", ALPHA)],
        );
        let archive = directory.path().join("repro.mrpack");
        mrpack(
            &archive,
            &[(
                "overrides/crash-reports/crash-2026-02-01_10.00.00-server.txt",
                server_report.as_bytes(),
            )],
        );

        let analysis = app.analyze_pack(&archive).expect("analysis");

        assert!(analysis.logged_mods.is_none());
    }

    #[test]
    fn an_older_crash_report_is_used_when_the_newest_has_no_hashed_list() {
        let directory = tempfile::tempdir().expect("temp dir");
        let app = App::open_at(Paths::rooted_at(directory.path())).expect("open app");
        let unhashed = report(
            "Integrated Server (map_client.txt)",
            &["\t\tearly.jar |Early |early |1.0 |Manifest: NOSIGNATURE".to_owned()],
        );
        let archive = directory.path().join("repro.mrpack");
        mrpack(
            &archive,
            &[
                (
                    "overrides/crash-reports/crash-2026-01-01_10.00.00-client.txt",
                    client_report().as_bytes(),
                ),
                (
                    "overrides/crash-reports/crash-2026-02-01_10.00.00-client.txt",
                    unhashed.as_bytes(),
                ),
            ],
        );

        let analysis = app.analyze_pack(&archive).expect("analysis");

        assert_eq!(
            analysis
                .logged_mods
                .as_ref()
                .map(|logged| logged.source.as_str()),
            Some("crash-reports/crash-2026-01-01_10.00.00-client.txt")
        );
    }

    #[tokio::test]
    async fn a_jar_whose_disabled_name_the_pack_already_ships_stays_and_the_import_completes() {
        let directory = tempfile::tempdir().expect("temp dir");
        let server = MockServer::start().await;
        modrinth_knows_alpha(&server, "client_and_server").await;
        let app = app_against(directory.path(), &server, false);
        let log = client_report();
        let archive = directory.path().join("repro.mrpack");
        mrpack(
            &archive,
            &[
                ("overrides/mods/delta.jar", DELTA),
                ("overrides/mods/delta.jar.disabled", ALPHA_OLD),
                ("overrides/logs/latest.log", log.as_bytes()),
            ],
        );

        let imported = import_pack(&app, &archive, true).await;

        assert_eq!(
            mods_of(&imported),
            ["alpha-1.0.jar", "delta.jar", "delta.jar.disabled"]
        );
        assert!(imported.unlisted.disabled.is_empty());
        assert_eq!(
            imported.unlisted.left_enabled,
            [std::path::PathBuf::from("mods/delta.jar")],
            "the jar that could not be disabled is reported"
        );
    }

    #[tokio::test]
    async fn a_catalog_file_cannot_take_the_name_of_a_jar_the_pack_ships() {
        let directory = tempfile::tempdir().expect("temp dir");
        let server = MockServer::start().await;
        modrinth_knows_alpha(&server, "client_and_server").await;
        let app = app_against(directory.path(), &server, false);
        let log = client_report();
        let archive = directory.path().join("repro.mrpack");
        mrpack(
            &archive,
            &[
                // The pack's own patched build under the catalog file's name.
                ("overrides/mods/alpha-1.0.jar", ALPHA_OLD),
                ("overrides/logs/latest.log", log.as_bytes()),
            ],
        );

        let imported = import_pack(&app, &archive, true).await;

        assert_eq!(mods_of(&imported), ["alpha-1.0.jar.disabled"]);
        assert!(
            imported
                .analysis
                .missing_logged_files
                .iter()
                .any(|row| row.mod_id == "alpha"),
            "the logged alpha could not be placed, so it is named as missing"
        );
    }
}
