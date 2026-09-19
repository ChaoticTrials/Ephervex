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

use std::path::PathBuf;

use ephervex_download::OperationReporter;
use ephervex_download::verify::file_matches_hashes;
use ephervex_model::{Hash, Progress};
use tokio_util::sync::CancellationToken;
use tracing::{info, instrument};

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
}

/// What one file on disk is, against the digests recorded for it.
async fn check(path: &std::path::Path, expected: &[Hash]) -> Integrity {
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
