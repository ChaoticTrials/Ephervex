//! Checking that an instance still holds the files it is supposed to.
//!
//! # Why presence was never enough
//!
//! Installing decides what to do from whether a destination exists. That is the
//! right question for an install - a jar that is there was put there by an
//! earlier run of the same operation - and the wrong one for trusting an
//! instance. A jar truncated by a full disk, or left half written by a machine
//! that lost power, exists.
//!
//! The recipe already records the strongest digest each provider published for
//! every file it declares, so nothing has to be fetched or asked to answer
//! this. What was missing is the walk.
//!
//! # What it deliberately does not do
//!
//! Nothing here writes, moves, deletes or downloads. A report is a reading, and
//! acting on it is a separate request, because "this file is wrong" and "replace
//! this file" are different decisions and only one of them is the launcher's.

use std::path::{Path, PathBuf};

use ephervex_download::verify::file_matches_hashes;
use ephervex_download::{CacheLayout, DownloadManager, DownloaderConfig, OperationReporter};
use ephervex_minecraft::{
    LaunchFeatures, MinecraftError, PlannedArtifact, Platform, VERSION_MANIFEST_CACHE_KEY,
    VanillaInstaller,
};
use ephervex_model::{Hash, InstanceId, LoaderKind, Progress};
use tokio_util::sync::CancellationToken;
use tracing::{info, instrument, warn};

use crate::app::App;
use crate::error::CoreResult;
use crate::mods::ModHost;

/// What checking one recorded file found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Integrity {
    /// The file is there and its bytes match what the recipe recorded.
    Intact,
    /// The recipe wants it and there is no file.
    Absent,
    /// The file is there and its bytes are not the ones recorded. The one
    /// answer presence alone can never give.
    Corrupt,
    /// The file is there and the recipe recorded no digest to check it
    /// against, so nothing about it is proven either way.
    ///
    /// True of every mod a pack shipped inside its own override tree: the pack
    /// carried the bytes rather than a reference to them, so there is no
    /// published hash and never will be.
    Unverifiable,
    /// The file could not be read at all.
    Unreadable,
}

impl Integrity {
    /// A stable identifier for `--json` output.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Intact => "intact",
            Self::Absent => "absent",
            Self::Corrupt => "corrupt",
            Self::Unverifiable => "unverifiable",
            Self::Unreadable => "unreadable",
        }
    }

    /// Whether this file needs attention.
    #[must_use]
    pub const fn is_sound(&self) -> bool {
        matches!(self, Self::Intact | Self::Unverifiable)
    }

    /// Whether replacing the file from its recorded source would help.
    ///
    /// A file with no recorded digest is excluded: there would be no way to
    /// tell afterwards whether the replacement was any better.
    #[must_use]
    pub const fn is_repairable(&self) -> bool {
        matches!(self, Self::Absent | Self::Corrupt | Self::Unreadable)
    }
}

/// One recorded file, and what checking it found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedFile {
    pub logical_path: PathBuf,
    pub integrity: Integrity,
    /// Whether the recipe records an address this file could be fetched from
    /// again. A pack's shipped jar does not.
    pub has_source: bool,
}

/// What one pass over a host's recorded files found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationReport {
    pub files: Vec<VerifiedFile>,
}

impl VerificationReport {
    /// Whether every recorded file is where it should be, with the bytes it
    /// should have.
    #[must_use]
    pub fn is_sound(&self) -> bool {
        self.files.iter().all(|file| file.integrity.is_sound())
    }

    /// The files a repair would act on.
    pub fn repairable(&self) -> impl Iterator<Item = &VerifiedFile> {
        self.files
            .iter()
            .filter(|file| file.integrity.is_repairable() && file.has_source)
    }

    /// The files something is wrong with that a repair cannot fix.
    ///
    /// Reported separately because telling somebody to repair an instance that
    /// repairing cannot mend is worse than telling them nothing.
    pub fn unrepairable(&self) -> impl Iterator<Item = &VerifiedFile> {
        self.files
            .iter()
            .filter(|file| !file.integrity.is_sound() && !file.has_source)
    }

    /// How many files came back with each answer.
    #[must_use]
    pub fn count(&self, integrity: &Integrity) -> usize {
        self.files
            .iter()
            .filter(|file| &file.integrity == integrity)
            .count()
    }
}

/// What checking the shared stores an instance launches from found.
///
/// Separate from the instance's own files because the two are different kinds
/// of thing. A mod lives in the instance and belongs to it; a library lives in
/// a store every instance shares, is addressed by a Maven coordinate rather
/// than by its digest, and is therefore the one class of managed artifact that
/// nothing re-checks once it is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedArtifactReport {
    /// Nothing could be checked, because no metadata for this release is
    /// cached. Not a fault: nothing has asked about the version yet.
    pub metadata_cached: bool,
    pub files: Vec<VerifiedFile>,
    /// Whether a loader adds libraries this pass did not cover.
    ///
    /// Vanilla's own artifacts are planned from the release's metadata, which
    /// is cached; a loader's extra libraries come from its own documents, and
    /// claiming to have checked them would be a promise this pass does not
    /// keep.
    pub loader_libraries_uncovered: bool,
}

impl SharedArtifactReport {
    /// Whether anything checked is actually wrong.
    ///
    /// An absent shared artifact is not a fault. These stores are filled as a
    /// launch needs them, so an instance that has been created and not yet run
    /// is missing most of them and is in no way broken; saying otherwise would
    /// cry wolf at the ordinary state of a fresh instance. Bytes that are
    /// present and wrong are the fault, because nothing else will ever notice
    /// them: a file already on a Maven path is not re-fetched, and not checked
    /// again either.
    #[must_use]
    pub fn is_sound(&self) -> bool {
        !self
            .files
            .iter()
            .any(|file| matches!(file.integrity, Integrity::Corrupt | Integrity::Unreadable))
    }

    /// The files whose bytes are wrong.
    pub fn faults(&self) -> impl Iterator<Item = &VerifiedFile> {
        self.files
            .iter()
            .filter(|file| matches!(file.integrity, Integrity::Corrupt | Integrity::Unreadable))
    }

    /// The files a launch has not fetched yet.
    pub fn not_yet_fetched(&self) -> impl Iterator<Item = &VerifiedFile> {
        self.files
            .iter()
            .filter(|file| file.integrity == Integrity::Absent)
    }
}

/// What one repair pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepairReport {
    /// Files replaced with the bytes the recipe records.
    pub repaired: Vec<PathBuf>,
    /// Files that were wrong and that nothing here can fetch again. A pack
    /// shipped these inside itself rather than pointing at them, so there is no
    /// address to go back to and re-importing the pack is the way out.
    pub unrepairable: Vec<PathBuf>,
    /// Files a repair was attempted for and did not fix.
    ///
    /// Kept apart from the ones nothing could have fixed, because one says the
    /// catalog or the network was the problem and the other says the pack was.
    pub failed: Vec<PathBuf>,
}

impl RepairReport {
    /// Whether the instance now holds what it recorded.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.unrepairable.is_empty() && self.failed.is_empty()
    }

    /// Whether anything about the instance changed.
    #[must_use]
    pub fn changed_anything(&self) -> bool {
        !self.repaired.is_empty()
    }
}

impl App {
    /// Checks every file this host's recipe records against its own digests.
    ///
    /// Reaches no network and writes nothing: the recipe already carries the
    /// hashes, so this is a read of the instance and nothing else.
    ///
    /// Hashing is real work on a large pack, so the operation reports progress
    /// and honors cancellation between files.
    #[instrument(skip(self, reporter, cancel), fields(host = %host.id()))]
    pub async fn verify_host(
        &self,
        host: ModHost,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> CoreResult<VerificationReport> {
        let layout = self.mod_host_layout(host)?;
        let Some(recipe) = layout.recipe.as_ref() else {
            // Nothing recorded is nothing to check, and is not a fault: an
            // instance made by hand acquires a recipe when something is
            // installed into it.
            return Ok(VerificationReport { files: Vec::new() });
        };

        let total = u64::try_from(recipe.files.len()).unwrap_or(u64::MAX);
        reporter.set_items(Progress {
            current: 0,
            total: Some(total),
        });

        let mut files = Vec::with_capacity(recipe.files.len());
        for (index, file) in recipe.files.iter().enumerate() {
            if cancel.is_cancelled() {
                break;
            }
            files.push(VerifiedFile {
                logical_path: file.logical_path.clone(),
                integrity: check(&layout.game_dir.join(&file.logical_path), &file.hashes).await,
                has_source: !file.downloads.is_empty(),
            });
            reporter.set_items_at(
                Progress {
                    current: u64::try_from(index + 1).unwrap_or(u64::MAX),
                    total: Some(total),
                },
                Some(file.display_name()),
            );
        }

        let report = VerificationReport { files };
        info!(
            intact = report.count(&Integrity::Intact),
            absent = report.count(&Integrity::Absent),
            corrupt = report.count(&Integrity::Corrupt),
            unverifiable = report.count(&Integrity::Unverifiable),
            "checked what this host holds against what it recorded"
        );
        Ok(report)
    }

    /// Checks the shared artifacts this instance launches from.
    ///
    /// Libraries and the client jar live on Maven-style paths rather than
    /// being addressed by their own digests, so nothing re-checks them once
    /// they are written. Their digests are in the release's metadata, which is
    /// what this reads.
    ///
    /// Offline, like every other verification here: it reads the metadata that
    /// is already cached and says so when none is, rather than becoming an
    /// operation that needs a connection. Launching the instance once caches
    /// it.
    ///
    /// Vanilla's own artifacts only. A loader's extra libraries come from that
    /// loader's own documents, and the report says plainly that they were not
    /// covered rather than leaving the omission to be discovered.
    #[instrument(skip(self), fields(instance = %instance))]
    pub async fn verify_shared_artifacts(
        &self,
        instance: InstanceId,
    ) -> CoreResult<SharedArtifactReport> {
        let record = self.client(instance)?;
        let loader_libraries_uncovered = record.loader.kind != LoaderKind::Vanilla;
        let Some(metadata) = self
            .minecraft()?
            .cached_metadata(&record.minecraft_version)
            .await?
        else {
            return Ok(SharedArtifactReport {
                metadata_cached: false,
                files: Vec::new(),
                loader_libraries_uncovered,
            });
        };

        let installer = VanillaInstaller::new(
            DownloadManager::new(DownloaderConfig::default()).map_err(|source| {
                MinecraftError::ManifestUnavailable {
                    source: ephervex_download::MetadataCacheError::Unavailable {
                        key: VERSION_MANIFEST_CACHE_KEY.to_owned(),
                        source: Box::new(source),
                    },
                }
            })?,
            CacheLayout::new(self.paths().cache_dir()),
        );
        let plan = installer.plan(
            &metadata,
            &LaunchFeatures::default().rule_context(Platform::host()),
        )?;

        let mut planned: Vec<&PlannedArtifact> = vec![&plan.client_jar, &plan.asset_index];
        planned.extend(plan.libraries.iter());
        planned.extend(plan.natives.iter().map(|natives| &natives.artifact));
        if let Some(logging) = plan.logging.as_ref() {
            planned.push(&logging.artifact);
        }

        let mut files = Vec::with_capacity(planned.len());
        for artifact in planned {
            files.push(VerifiedFile {
                logical_path: PathBuf::from(&artifact.label),
                integrity: check(
                    &artifact.path,
                    std::slice::from_ref(&artifact.artifact.sha1),
                )
                .await,
                // Every one of these names an address in the release metadata,
                // so anything wrong with one is a re-download away from right.
                has_source: true,
            });
        }

        info!(
            checked = files.len(),
            faults = files
                .iter()
                .filter(|file| !file.integrity.is_sound())
                .count(),
            "checked the shared artifacts this instance launches from"
        );
        Ok(SharedArtifactReport {
            metadata_cached: true,
            files,
            loader_libraries_uncovered,
        })
    }

    /// Replaces every file that is absent or wrong with the one recorded.
    ///
    /// Verification first, because repairing what is not broken would rewrite
    /// files for nothing, and because the two answers a repair needs - what is
    /// wrong, and whether anything can be done about it - are the ones a
    /// verification already gives.
    ///
    /// Each file is fetched and verified before anything on disk is touched,
    /// so a repair that cannot obtain a replacement leaves the broken file
    /// exactly where it was rather than removing it and failing. One file's
    /// failure does not abandon the rest: a pass that mended four of five and
    /// says so is more use than one that reports only the fifth.
    #[instrument(skip(self, reporter, cancel), fields(host = %host.id()))]
    pub async fn repair_host(
        &self,
        host: ModHost,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> CoreResult<RepairReport> {
        let layout = self.mod_host_layout(host)?;
        let found = self.verify_host(host, reporter, cancel).await?;
        let mut report = RepairReport {
            unrepairable: found
                .unrepairable()
                .map(|file| file.logical_path.clone())
                .collect(),
            ..RepairReport::default()
        };

        let wanted: Vec<PathBuf> = found
            .repairable()
            .map(|file| file.logical_path.clone())
            .collect();
        for path in wanted {
            if cancel.is_cancelled() {
                break;
            }
            // A file that is present, and wrong, has to move out of the way
            // first: writing the replacement refuses to overwrite, which is
            // the rule that keeps an install from touching a jar it did not
            // put there. Moving rather than deleting is what makes the failure
            // path recoverable (ADR 0020) - the bytes go back if the
            // replacement cannot be obtained.
            let retired = match retire(&layout, &path) {
                Ok(retired) => retired,
                Err(error) => {
                    warn!(%error, path = %path.display(), "a broken file could not be moved aside");
                    report.failed.push(path);
                    continue;
                }
            };
            match self.restore_mod_file(host, &path, reporter, cancel).await {
                Ok(()) => {
                    // Deleting the outgoing copy is allowed to fail: a leftover
                    // in launcher-owned scratch is invisible to the game, and
                    // the next mutation clears it.
                    if let Some(retired) = retired {
                        let _ = std::fs::remove_file(retired);
                    }
                    report.repaired.push(path);
                }
                Err(error) => {
                    warn!(%error, path = %path.display(), "a file could not be repaired");
                    if let Some(retired) = retired {
                        restore_retired(&retired, &layout.game_dir.join(&path));
                    }
                    report.failed.push(path);
                }
            }
        }

        info!(
            repaired = report.repaired.len(),
            failed = report.failed.len(),
            unrepairable = report.unrepairable.len(),
            "replaced what this host was missing or holding wrongly"
        );
        Ok(report)
    }
}

/// Moves a broken jar into launcher-owned scratch, out of the installer's way.
///
/// Returns where it went, or nothing when there was no file there to move -
/// which is the ordinary case for a file that is simply absent.
fn retire(
    layout: &crate::mods::ModHostLayout,
    logical_path: &Path,
) -> std::io::Result<Option<PathBuf>> {
    let installed = layout.game_dir.join(logical_path);
    let present = if installed.is_file() {
        installed
    } else {
        let disabled = ephervex_pack::disabled_path(&installed);
        if disabled.is_file() {
            disabled
        } else {
            return Ok(None);
        }
    };

    let directory = ephervex_storage::metadata::retired_dir(&layout.root);
    std::fs::create_dir_all(&directory)?;
    let name = present
        .file_name()
        .unwrap_or(std::ffi::OsStr::new("mod.jar"));
    let destination = directory.join(name);
    std::fs::rename(&present, &destination)?;
    Ok(Some(destination))
}

/// Puts a retired jar back where it came from.
///
/// Best effort and reported rather than returned: the repair has already
/// failed by the time this runs, and a second failure changes only how much of
/// the instance is intact, not whether the operation went wrong.
fn restore_retired(retired: &Path, destination: &Path) {
    if let Err(error) = std::fs::rename(retired, destination) {
        warn!(
            %error,
            path = %destination.display(),
            "a broken file could not be put back after a failed repair"
        );
    }
}

/// What one file on disk is, against the digests recorded for it.
async fn check(path: &Path, expected: &[Hash]) -> Integrity {
    // A disabled jar is installed and recorded; it is renamed out of the
    // loader's way, and verifying it is still verifying the instance.
    let path = if path.is_file() {
        path.to_path_buf()
    } else {
        let disabled = ephervex_pack::disabled_path(path);
        if disabled.is_file() {
            disabled
        } else {
            return Integrity::Absent;
        }
    };
    if expected.is_empty() {
        return Integrity::Unverifiable;
    }
    match file_matches_hashes(&path, expected).await {
        Ok(true) => Integrity::Intact,
        Ok(false) => Integrity::Corrupt,
        Err(_) => Integrity::Unreadable,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use std::path::Path;

    use ephervex_model::{HashAlgorithm, InstanceId, LoaderKind, LoaderSelection, ModOrigin};
    use ephervex_pack::{
        PackFileRecipe, PackRecipe, ProviderFileIdentity, SideRequirement, SourceStore,
    };
    use ephervex_storage::{NewClient, Paths};
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::app::App;

    /// The digest of the bytes every sound jar in these tests holds.
    const BODY: &[u8] = b"a jar";

    fn digest_of(body: &[u8]) -> Hash {
        use sha1::Digest as _;
        let mut hasher = sha1::Sha1::new();
        hasher.update(body);
        Hash::new(HashAlgorithm::Sha1, &hex::encode(hasher.finalize())).expect("a valid sha1")
    }

    fn recorded(name: &str, hashes: Vec<Hash>, downloads: Vec<String>) -> PackFileRecipe {
        PackFileRecipe {
            logical_path: PathBuf::from("mods").join(name),
            hashes,
            downloads,
            size: Some(BODY.len() as u64),
            client: SideRequirement::Required,
            server: SideRequirement::Required,
            selected: None,
            origin: ModOrigin::UserSelected,
            identity: Some(ProviderFileIdentity {
                provider: Some(ephervex_model::ModProvider::Modrinth),
                project_id: Some(name.to_owned()),
                version_id: Some(format!("{name}V1")),
                file_id: None,
            }),
            original_identity: None,
            relations: None,
        }
    }

    /// An instance holding one sound jar, one truncated jar, one jar the recipe
    /// wants and nothing put there, and one the pack shipped with no digest.
    fn instance(base: &Path) -> (App, InstanceId, PathBuf) {
        let app = App::open_at(Paths::rooted_at(base)).expect("app opens");
        let record = app
            .create_client_for_test(NewClient::new(
                "Verification Example",
                "1.21.1",
                LoaderSelection::exact(LoaderKind::NeoForge, "21.1.77"),
            ))
            .expect("create");

        let address = vec!["https://cdn.modrinth.com/data/x/a.jar".to_owned()];
        let mut recipe = PackRecipe::for_manual_instance(
            "Verification Example",
            "1.21.1",
            record.loader.clone(),
        );
        recipe.files = vec![
            recorded("sound.jar", vec![digest_of(BODY)], address.clone()),
            recorded("truncated.jar", vec![digest_of(BODY)], address.clone()),
            recorded("gone.jar", vec![digest_of(BODY)], address),
            // What a pack that shipped the bytes records: no digest, no
            // address, and nothing to check it against.
            recorded("shipped.jar", Vec::new(), Vec::new()),
        ];
        SourceStore::new(ephervex_storage::metadata::source_dir(&record.root))
            .write_recipe(&recipe)
            .expect("write recipe");

        let mods = ephervex_storage::metadata::client_game_dir(&record.root).join("mods");
        std::fs::create_dir_all(&mods).expect("mods dir");
        std::fs::write(mods.join("sound.jar"), BODY).expect("write");
        std::fs::write(mods.join("truncated.jar"), b"a ja").expect("write");
        std::fs::write(mods.join("shipped.jar"), BODY).expect("write");
        (app, record.id, record.root)
    }

    async fn verify(app: &App, instance: InstanceId) -> VerificationReport {
        app.verify_host(
            ModHost::Instance(instance),
            &OperationReporter::silent("test-verify"),
            &CancellationToken::new(),
        )
        .await
        .expect("verify")
    }

    fn integrity(report: &VerificationReport, name: &str) -> Integrity {
        report
            .files
            .iter()
            .find(|file| file.logical_path == Path::new("mods").join(name))
            .map_or_else(
                || panic!("no entry for {name}"),
                |file| file.integrity.clone(),
            )
    }

    #[tokio::test]
    async fn a_file_that_is_there_with_the_wrong_bytes_is_not_intact() {
        // The whole reason this exists. Deciding from presence alone calls a
        // truncated jar installed, and the game finds out instead.
        let base = tempfile::tempdir().expect("temp dir");
        let (app, id, _root) = instance(base.path());

        let report = verify(&app, id).await;
        assert_eq!(integrity(&report, "sound.jar"), Integrity::Intact);
        assert_eq!(integrity(&report, "truncated.jar"), Integrity::Corrupt);
        assert_eq!(integrity(&report, "gone.jar"), Integrity::Absent);
        assert!(!report.is_sound());
    }

    #[tokio::test]
    async fn a_file_with_no_recorded_digest_is_unproven_rather_than_sound_or_broken() {
        let base = tempfile::tempdir().expect("temp dir");
        let (app, id, _root) = instance(base.path());

        let report = verify(&app, id).await;
        assert_eq!(integrity(&report, "shipped.jar"), Integrity::Unverifiable);
        assert!(
            !report
                .unrepairable()
                .any(|file| file.logical_path == Path::new("mods/shipped.jar")),
            "nothing is wrong with it, so it is not something a repair failed at"
        );
    }

    #[tokio::test]
    async fn a_repair_is_offered_only_where_replacing_the_file_could_help() {
        let base = tempfile::tempdir().expect("temp dir");
        let (app, id, root) = instance(base.path());
        // A pack's own shipped jar, broken. There is no address to fetch it
        // from, so saying "repair this" would be a promise nothing can keep.
        let mods = ephervex_storage::metadata::client_game_dir(&root).join("mods");
        std::fs::write(mods.join("shipped.jar"), BODY).expect("write");

        let report = verify(&app, id).await;
        let repairable: Vec<&Path> = report
            .repairable()
            .map(|file| file.logical_path.as_path())
            .collect();
        assert_eq!(
            repairable,
            [Path::new("mods/truncated.jar"), Path::new("mods/gone.jar")],
            "the two with an address, and only those"
        );
    }

    #[tokio::test]
    async fn a_disabled_jar_is_still_part_of_what_the_instance_holds() {
        // Turning a mod off renames it out of the loader's way. It is still
        // installed and still recorded, so calling it absent would report a
        // fault for something the user deliberately did.
        let base = tempfile::tempdir().expect("temp dir");
        let (app, id, root) = instance(base.path());
        let mods = ephervex_storage::metadata::client_game_dir(&root).join("mods");
        std::fs::rename(mods.join("sound.jar"), mods.join("sound.jar.disabled")).expect("disable");

        assert_eq!(
            integrity(&verify(&app, id).await, "sound.jar"),
            Integrity::Intact
        );
    }

    #[tokio::test]
    async fn a_repair_leaves_a_broken_file_alone_when_it_cannot_replace_it() {
        // Nothing here can reach the loopback-free addresses these fixtures
        // carry, which is the point: a repair that cannot obtain a replacement
        // has to leave the broken bytes where they are rather than remove them
        // and fail. Losing a file to a failed repair is worse than keeping a
        // wrong one, because a wrong one can still be looked at.
        let base = tempfile::tempdir().expect("temp dir");
        let (app, id, root) = instance(base.path());
        let mods = ephervex_storage::metadata::client_game_dir(&root).join("mods");

        let report = app
            .repair_host(
                ModHost::Instance(id),
                &OperationReporter::silent("test-repair"),
                &CancellationToken::new(),
            )
            .await
            .expect("a repair reports rather than fails");

        assert!(!report.is_complete());
        assert!(report.repaired.is_empty(), "no address here can be reached");
        assert_eq!(
            report.failed.len(),
            2,
            "the truncated one and the absent one"
        );
        assert_eq!(
            std::fs::read(mods.join("truncated.jar")).expect("read"),
            b"a ja",
            "the broken bytes are still there to look at"
        );
    }

    #[tokio::test]
    async fn a_failed_repair_puts_the_broken_file_back_rather_than_losing_it() {
        // A corrupt file has to move out of the installer's way before its
        // replacement can be written, which is the one step that can lose it.
        // These fixtures can reach no address, so every repair here fails, and
        // the bytes still have to be where they were.
        let base = tempfile::tempdir().expect("temp dir");
        let (app, id, root) = instance(base.path());
        let mods = ephervex_storage::metadata::client_game_dir(&root).join("mods");

        let report = app
            .repair_host(
                ModHost::Instance(id),
                &OperationReporter::silent("test-repair"),
                &CancellationToken::new(),
            )
            .await
            .expect("repair");
        assert!(report.repaired.is_empty());

        assert_eq!(
            std::fs::read(mods.join("truncated.jar")).expect("the file is still there"),
            b"a ja",
            "a failed repair leaves the bytes it moved aside back where they were"
        );
        let retired = ephervex_storage::metadata::retired_dir(&root);
        assert!(
            !retired.is_dir()
                || std::fs::read_dir(&retired)
                    .expect("scratch")
                    .next()
                    .is_none(),
            "nothing is left stranded in launcher scratch"
        );
    }

    #[tokio::test]
    async fn a_repair_names_what_nothing_can_fetch_apart_from_what_failed() {
        // Two different answers: the pack shipped this one inside itself so no
        // address exists, versus the address exists and could not be reached.
        // One says re-import the pack and the other says try again.
        //
        // The shipped jar has to be *gone* to be a fault at all. With no digest
        // recorded there is nothing to prove its bytes wrong with, so corrupting
        // it proves nothing and it stays unproven rather than becoming broken.
        let base = tempfile::tempdir().expect("temp dir");
        let (app, id, root) = instance(base.path());
        let mods = ephervex_storage::metadata::client_game_dir(&root).join("mods");
        std::fs::remove_file(mods.join("shipped.jar")).expect("remove");

        let report = app
            .repair_host(
                ModHost::Instance(id),
                &OperationReporter::silent("test-repair"),
                &CancellationToken::new(),
            )
            .await
            .expect("repair");

        assert_eq!(
            report.unrepairable,
            [PathBuf::from("mods/shipped.jar")],
            "no digest and no address, so a repair was never possible for it"
        );
        assert!(
            !report.failed.contains(&PathBuf::from("mods/shipped.jar")),
            "it is not something a repair attempted and failed at"
        );
    }

    #[tokio::test]
    async fn shared_artifacts_are_not_checked_when_no_metadata_is_cached() {
        // Nothing has asked about this release, so there is nothing to check
        // against. Saying so is the answer; reaching for it would make a
        // verification that promises to stay local into one that does not.
        let base = tempfile::tempdir().expect("temp dir");
        let (app, id, _root) = instance(base.path());

        let report = app
            .verify_shared_artifacts(id)
            .await
            .expect("an uncached release is reported, not fetched");
        assert!(!report.metadata_cached);
        assert!(report.files.is_empty());
        assert!(
            report.is_sound(),
            "not having looked is not the same as having found something wrong"
        );
    }

    #[test]
    fn a_shared_artifact_that_is_merely_not_downloaded_yet_is_not_a_fault() {
        // These stores fill as a launch needs them, so a freshly created
        // instance is missing most of them and is in no way broken. Bytes that
        // are present and wrong are the fault, because a file already on a
        // Maven path is never re-fetched and never checked again.
        let report = SharedArtifactReport {
            metadata_cached: true,
            files: vec![
                VerifiedFile {
                    logical_path: PathBuf::from("client.jar"),
                    integrity: Integrity::Absent,
                    has_source: true,
                },
                VerifiedFile {
                    logical_path: PathBuf::from("a-library.jar"),
                    integrity: Integrity::Intact,
                    has_source: true,
                },
            ],
            loader_libraries_uncovered: false,
        };
        assert!(report.is_sound());
        assert_eq!(report.not_yet_fetched().count(), 1);
        assert_eq!(report.faults().count(), 0);

        let broken = SharedArtifactReport {
            files: vec![VerifiedFile {
                logical_path: PathBuf::from("a-library.jar"),
                integrity: Integrity::Corrupt,
                has_source: true,
            }],
            ..report
        };
        assert!(
            !broken.is_sound(),
            "wrong bytes are what nothing else notices"
        );
    }

    #[tokio::test]
    async fn an_instance_with_nothing_recorded_verifies_as_sound() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = App::open_at(Paths::rooted_at(base.path())).expect("app opens");
        let record = app
            .create_client_for_test(NewClient::new(
                "Nothing Recorded",
                "1.21.1",
                LoaderSelection::exact(LoaderKind::NeoForge, "21.1.77"),
            ))
            .expect("create");

        let report = verify(&app, record.id).await;
        assert!(report.files.is_empty());
        assert!(report.is_sound(), "nothing recorded is nothing wrong");
    }
}
