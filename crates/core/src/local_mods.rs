//! Putting a jar the user already has into a mod host, and taking one out.
//!
//! # Why this is not an install
//!
//! Installing a mod means asking a catalog for a project, resolving a release
//! that fits the instance, verifying it against the hashes the catalog
//! published, and recording all of that so a server built later contains the
//! same file. None of that is available here. What a user hands over is a file,
//! and the only true thing about it is its path.
//!
//! So a file added this way arrives exactly as a jar copied in with a file
//! manager does: it lands in `mods/`, it is listed as [`ModOrigin::HandPlaced`],
//! and identifying it is left to the digest lookup that already runs on a
//! refresh. Nothing here writes a recipe entry, because nothing here has
//! learned anything a recipe could record.
//!
//! # What is checked
//!
//! The path is untrusted. Its name is put through the same guard a provider's
//! file name goes through, so nothing can name a destination outside `mods/`;
//! the file has to be a regular file, a `.jar`, a real archive, and under a
//! ceiling. Those checks are cheap and they run twice, once to tell the user
//! what will happen and again when it happens, because the two are separated by
//! however long the user takes to answer.

use std::path::{Path, PathBuf};

use ephervex_model::ModOrigin;
use ephervex_pack::{ExtractionLimits, MODS_DIR, PackArchive, SourceStore, safe_file_name};
use tracing::{info, warn};

use crate::app::App;
use crate::error::{CoreError, CoreResult};
use crate::mods::{ModHost, disabled_path};

/// The largest jar Ephervex will copy into a mods folder.
///
/// A mod jar with its assets can be very large, so this is generous. It exists
/// so that a mis-aimed drop of a disk image or a video file is refused with an
/// explanation instead of filling the instance.
pub const MAX_LOCAL_MOD_BYTES: u64 = 512 * 1024 * 1024;

/// What a mod jar's archive may contain before it is refused.
///
/// Tighter than the pack default, which is sized for a modpack carrying every
/// file of an instance. Nothing here extracts anything: the archive is opened
/// only to prove it is one, and the entry count is the cheap half of that.
fn jar_limits() -> ExtractionLimits {
    ExtractionLimits {
        max_entries: 200_000,
        max_total_bytes: MAX_LOCAL_MOD_BYTES,
        max_entry_bytes: MAX_LOCAL_MOD_BYTES,
    }
}

/// Why one file cannot be added to a mods folder.
///
/// Typed rather than a sentence so the frontend localizes it, and one variant
/// per thing the user can actually do something about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LocalModRefusal {
    /// Not a regular file: a directory, a device, or something that vanished.
    NotAFile,
    /// The name does not end in `.jar`.
    NotAJar,
    /// The name cannot address a file inside `mods/` and nothing else.
    UnusableName,
    /// Not a readable archive, so no loader would read it either.
    NotAnArchive,
    /// Larger than [`MAX_LOCAL_MOD_BYTES`].
    TooLarge,
}

impl LocalModRefusal {
    /// The `ephervex-i18n` message id explaining this to a user.
    #[must_use]
    pub const fn message_id(self) -> &'static str {
        match self {
            Self::NotAFile => "local-mod-not-a-file",
            Self::NotAJar => "local-mod-not-a-jar",
            Self::UnusableName => "local-mod-unusable-name",
            Self::NotAnArchive => "local-mod-not-an-archive",
            Self::TooLarge => "local-mod-too-large",
        }
    }

    /// A stable identifier for `--json` output and frontend state.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotAFile => "not-a-file",
            Self::NotAJar => "not-a-jar",
            Self::UnusableName => "unusable-name",
            Self::NotAnArchive => "not-an-archive",
            Self::TooLarge => "too-large",
        }
    }
}

/// What one offered file would do to the mods folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalModOffer {
    /// It can be added, and nothing of that name is there.
    Ready {
        source: PathBuf,
        /// The name it would take inside `mods/`.
        file_name: String,
    },
    /// It can be added, but a jar of that name is already there.
    ///
    /// Separate from [`Self::Ready`] because only a person can decide this:
    /// the jar in the folder may be the one being replaced or may be an
    /// unrelated mod that happens to share a name, and nothing on disk says
    /// which.
    Clashing {
        source: PathBuf,
        file_name: String,
        /// Whether the jar already there is one the loader is ignoring.
        existing_is_disabled: bool,
    },
    /// It cannot be added at all.
    Refused {
        source: PathBuf,
        reason: LocalModRefusal,
    },
}

impl LocalModOffer {
    /// The file offered, whatever became of it.
    #[must_use]
    pub fn source(&self) -> &Path {
        match self {
            Self::Ready { source, .. }
            | Self::Clashing { source, .. }
            | Self::Refused { source, .. } => source,
        }
    }

    /// The name it would take in `mods/`, for anything that can take one.
    #[must_use]
    pub fn file_name(&self) -> Option<&str> {
        match self {
            Self::Ready { file_name, .. } | Self::Clashing { file_name, .. } => Some(file_name),
            Self::Refused { .. } => None,
        }
    }
}

/// What a set of offered files would do to one host's mods folder.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalModPlan {
    pub offers: Vec<LocalModOffer>,
}

impl LocalModPlan {
    /// The files whose names are already taken, which is the only question a
    /// frontend has to put to the user.
    pub fn clashing(&self) -> impl Iterator<Item = &LocalModOffer> {
        self.offers
            .iter()
            .filter(|offer| matches!(offer, LocalModOffer::Clashing { .. }))
    }

    /// Whether anything at all could be added.
    #[must_use]
    pub fn has_anything_to_add(&self) -> bool {
        self.offers
            .iter()
            .any(|offer| !matches!(offer, LocalModOffer::Refused { .. }))
    }

    /// The sources that need no question asked.
    pub fn ready_sources(&self) -> impl Iterator<Item = &Path> {
        self.offers.iter().filter_map(|offer| match offer {
            LocalModOffer::Ready { source, .. } => Some(source.as_path()),
            _ => None,
        })
    }
}

/// What adding a set of files actually did.
///
/// One bad file does not fail a batch: a user who drops six jars and one text
/// file wanted the six jars. Everything that did not happen is named, so the
/// frontend can say so rather than reporting a success that was partial.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalModReport {
    /// Names now in `mods/` that were not there before.
    pub added: Vec<String>,
    /// Names that were already there and now hold the new file.
    pub replaced: Vec<String>,
    /// Files that could not be added, and why.
    pub refused: Vec<(PathBuf, LocalModRefusal)>,
}

impl LocalModReport {
    /// Whether the mods folder changed at all.
    #[must_use]
    pub fn changed_anything(&self) -> bool {
        !self.added.is_empty() || !self.replaced.is_empty()
    }

    /// How many files are now in the folder because of this.
    #[must_use]
    pub fn written(&self) -> usize {
        self.added.len() + self.replaced.len()
    }
}

/// The destination name for one offered file, or why it has none.
fn destination_name(source: &Path) -> Result<String, LocalModRefusal> {
    let metadata = std::fs::metadata(source).map_err(|_| LocalModRefusal::NotAFile)?;
    if !metadata.is_file() {
        return Err(LocalModRefusal::NotAFile);
    }
    if metadata.len() > MAX_LOCAL_MOD_BYTES {
        return Err(LocalModRefusal::TooLarge);
    }

    let raw = source
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(LocalModRefusal::UnusableName)?;
    // Checked before the safe-name guard so that dropping a `.zip` or a `.txt`
    // is told apart from dropping something that cannot name a file at all.
    if !Path::new(raw)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("jar"))
    {
        return Err(LocalModRefusal::NotAJar);
    }
    let safe = safe_file_name(raw).ok_or(LocalModRefusal::UnusableName)?;
    let name = safe
        .to_str()
        .ok_or(LocalModRefusal::UnusableName)?
        .to_owned();

    // Last, because it is the only check that reads the file.
    PackArchive::open(source, jar_limits()).map_err(|error| {
        warn!(%error, source = %source.display(), "a file offered as a mod is not a readable archive");
        LocalModRefusal::NotAnArchive
    })?;
    Ok(name)
}

impl App {
    /// What copying these files into a host's mods folder would do.
    ///
    /// Reads and writes nothing. Its whole purpose is to let a frontend ask
    /// about the name clashes before it asks the filesystem to do anything, so
    /// that the decision is made by the person who can make it.
    pub fn plan_local_mods(&self, host: ModHost, sources: &[PathBuf]) -> CoreResult<LocalModPlan> {
        let layout = self.mod_host_layout(host)?;
        let dir = layout.game_dir.join(MODS_DIR);

        let mut offers = Vec::with_capacity(sources.len());
        for source in sources {
            let file_name = match destination_name(source) {
                Ok(name) => name,
                Err(reason) => {
                    offers.push(LocalModOffer::Refused {
                        source: source.clone(),
                        reason,
                    });
                    continue;
                }
            };
            let installed = dir.join(&file_name);
            // The disabled spelling counts as taken. Writing beside it would
            // leave two copies of one mod in the folder with one of them
            // invisible to the loader and to anybody reading the directory.
            let disabled = disabled_path(&installed);
            offers.push(if installed.is_file() {
                LocalModOffer::Clashing {
                    source: source.clone(),
                    file_name,
                    existing_is_disabled: false,
                }
            } else if disabled.is_file() {
                LocalModOffer::Clashing {
                    source: source.clone(),
                    file_name,
                    existing_is_disabled: true,
                }
            } else {
                LocalModOffer::Ready {
                    source: source.clone(),
                    file_name,
                }
            });
        }
        Ok(LocalModPlan { offers })
    }

    /// Copies files into a host's mods folder, replacing what is in the way.
    ///
    /// Every source named here is written, so a caller that asked the user
    /// about a clash passes only what they agreed to. Each file is validated
    /// again first: the answer given to [`Self::plan_local_mods`] describes a
    /// folder as it was, and the user has been thinking since.
    ///
    /// Each file is staged inside the instance's own launcher directory and
    /// renamed into place, so an interrupted copy never leaves half a jar where
    /// a loader would find it.
    pub fn add_local_mods(&self, host: ModHost, sources: &[PathBuf]) -> CoreResult<LocalModReport> {
        self.refuse_running_mod_host(host)?;
        let layout = self.mod_host_layout(host)?;
        let dir = layout.game_dir.join(MODS_DIR);
        let staging = ephervex_storage::metadata::staging_dir(&layout.root);

        let mut report = LocalModReport::default();
        for source in sources {
            let file_name = match destination_name(source) {
                Ok(name) => name,
                Err(reason) => {
                    report.refused.push((source.clone(), reason));
                    continue;
                }
            };
            let installed = dir.join(&file_name);
            let existed = installed.is_file() || disabled_path(&installed).is_file();
            publish_local_mod(source, &staging, &dir, &file_name)?;
            if existed {
                report.replaced.push(file_name);
            } else {
                report.added.push(file_name);
            }
        }

        if report.changed_anything() {
            info!(
                host = %host.id(),
                added = report.added.len(),
                replaced = report.replaced.len(),
                refused = report.refused.len(),
                "added mods from files the user supplied"
            );
        }
        Ok(report)
    }

    /// Deletes a jar somebody put in `mods/` themselves.
    ///
    /// The one destructive thing offered for a file the launcher does not own,
    /// and offered precisely because it does not own it: there is no catalog to
    /// fetch it from again and no record to repair it from, so nothing but the
    /// user can decide the file has served its purpose.
    ///
    /// Refuses anything the launcher installed. Taking one of those out of an
    /// instance is a removal, which resolves what its dependencies leave
    /// orphaned and retires the files so the change can be undone; deleting the
    /// jar underneath that record would leave the instance describing a mod it
    /// does not have.
    ///
    /// The adopted recipe entry goes with the file. That entry exists only to
    /// remember what a digest lookup found out about a jar, so once the jar is
    /// gone it describes nothing and would otherwise leave the list showing a
    /// mod the instance cannot produce.
    pub fn delete_hand_placed_mod(&self, host: ModHost, logical_path: &Path) -> CoreResult<()> {
        self.refuse_running_mod_host(host)?;
        let snapshot = self.host_mod_snapshot(host)?;
        let entry = snapshot
            .mods
            .iter()
            .find(|entry| entry.logical_path == logical_path)
            .ok_or_else(|| CoreError::ModNotManaged {
                path: logical_path.to_path_buf(),
            })?;
        if entry.origin != ModOrigin::HandPlaced {
            return Err(CoreError::ModNotHandPlaced {
                path: logical_path.to_path_buf(),
            });
        }

        let layout = self.mod_host_layout(host)?;
        let installed = layout.game_dir.join(logical_path);
        let disabled = disabled_path(&installed);
        // Both spellings, because a jar the user turned off is still their jar
        // and a deletion that left the disabled twin behind would put the row
        // straight back on the next refresh.
        for path in [&installed, &disabled] {
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(CoreError::ModFileNotDeleted {
                        path: logical_path.to_path_buf(),
                        source,
                    });
                }
            }
        }

        forget_adopted_record(&layout, logical_path);
        info!(
            host = %host.id(),
            path = %logical_path.display(),
            "deleted a jar that was added by hand"
        );
        Ok(())
    }
}

/// Drops the recipe entry adoption wrote for a hand-placed jar, if there is one.
///
/// Best effort on purpose. The file is already gone, and a recipe that could not
/// be rewritten is a stale record rather than a lost one: the next refresh finds
/// no jar behind it and the list does not show it either way.
fn forget_adopted_record(layout: &crate::mods::ModHostLayout, logical_path: &Path) {
    let Some(mut recipe) = layout.recipe.clone() else {
        return;
    };
    if !recipe.forget_hand_placed(logical_path) {
        return;
    }
    let store = SourceStore::new(ephervex_storage::metadata::source_dir(&layout.root));
    if let Err(error) = store.write_recipe(&recipe) {
        warn!(%error, "a deleted jar's record could not be dropped");
    }
}

/// Copies one file through staging and renames it into `mods/`.
fn publish_local_mod(source: &Path, staging: &Path, dir: &Path, file_name: &str) -> CoreResult<()> {
    let failed = |error: std::io::Error| CoreError::ModFileNotAdded {
        path: source.to_path_buf(),
        source: error,
    };

    std::fs::create_dir_all(staging).map_err(failed)?;
    std::fs::create_dir_all(dir).map_err(failed)?;

    // Named after the destination rather than after anything unique: one call
    // writes each name at most once, and a leftover from an interrupted run is
    // a file this overwrites rather than one it has to work around.
    let staged = staging.join(file_name);
    std::fs::copy(source, &staged).map_err(failed)?;

    let installed = dir.join(file_name);
    // The disabled twin goes first. Leaving it would hide a second copy of the
    // mod the user just added behind a suffix, where the loader ignores it and
    // the list shows it once.
    let _ = std::fs::remove_file(disabled_path(&installed));
    if let Err(error) = std::fs::rename(&staged, &installed) {
        let _ = std::fs::remove_file(&staged);
        return Err(CoreError::ModFileNotAdded {
            path: source.to_path_buf(),
            source: error,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use std::io::Write as _;

    use ephervex_model::{LoaderSelection, ModOrigin};
    use ephervex_storage::{NewClient, Paths};

    use super::*;
    use crate::mods::ModPresence;

    /// An instance with an empty mods folder, and the paths a test needs.
    struct Fixture {
        _base: tempfile::TempDir,
        app: App,
        host: ModHost,
        root: PathBuf,
        mods_dir: PathBuf,
    }

    fn fixture() -> Fixture {
        let base = tempfile::tempdir().expect("temp dir");
        let app = App::open_at(Paths::rooted_at(base.path())).expect("app opens");
        let record = app
            .create_client_for_test(NewClient::new(
                "Repro",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create");
        let mods_dir = ephervex_storage::metadata::client_game_dir(&record.root).join(MODS_DIR);
        std::fs::create_dir_all(&mods_dir).expect("mods dir");
        Fixture {
            _base: base,
            app,
            host: ModHost::Instance(record.id),
            root: record.root,
            mods_dir,
        }
    }

    /// A real, if empty, zip archive under whatever name is asked for.
    ///
    /// The validation opens the file, so a test fixture that is only the word
    /// "jar" would be refused for the right reason and prove nothing about the
    /// behavior under test.
    fn jar_at(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        let file = std::fs::File::create(&path).expect("create");
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file(
            "META-INF/MANIFEST.MF",
            zip::write::SimpleFileOptions::default(),
        )
        .expect("entry");
        zip.write_all(b"Manifest-Version: 1.0\n").expect("write");
        zip.finish().expect("finish");
        path
    }

    fn names_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .expect("read dir")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_jar_the_user_chose_is_copied_in_and_listed_as_added_by_hand() {
        let host = fixture();
        let chosen = tempfile::tempdir().expect("temp dir");
        let source = jar_at(chosen.path(), "from-a-forum-post.jar");

        let report = host
            .app
            .add_local_mods(host.host, &[source])
            .expect("add the jar");

        assert_eq!(report.added, vec!["from-a-forum-post.jar".to_owned()]);
        assert!(report.replaced.is_empty());
        assert!(report.refused.is_empty());
        assert_eq!(names_in(&host.mods_dir), vec!["from-a-forum-post.jar"]);

        let snapshot = host.app.host_mod_snapshot(host.host).expect("snapshot");
        assert_eq!(snapshot.mods.len(), 1);
        assert_eq!(snapshot.mods[0].origin, ModOrigin::HandPlaced);
    }

    #[test]
    fn adding_a_jar_records_nothing_in_the_recipe() {
        // Nothing here learned what the file is, so there is nothing a recipe
        // could honestly say about it. Identification is a separate run.
        let host = fixture();
        let chosen = tempfile::tempdir().expect("temp dir");
        let source = jar_at(chosen.path(), "unknown.jar");

        host.app
            .add_local_mods(host.host, &[source])
            .expect("add the jar");

        let layout = host.app.mod_host_layout(host.host).expect("layout");
        assert!(
            layout.recipe.is_none(),
            "an instance created by hand gains no recipe from a copied-in jar"
        );
    }

    #[test]
    fn nothing_but_a_readable_jar_archive_is_copied_in() {
        let host = fixture();
        let chosen = tempfile::tempdir().expect("temp dir");

        let not_a_jar = chosen.path().join("notes.txt");
        std::fs::write(&not_a_jar, b"not a mod").expect("write");
        let not_an_archive = chosen.path().join("truncated.jar");
        std::fs::write(&not_an_archive, b"this is not a zip").expect("write");
        let a_directory = chosen.path().join("a-directory.jar");
        std::fs::create_dir(&a_directory).expect("mkdir");
        let good = jar_at(chosen.path(), "real.jar");

        let report = host
            .app
            .add_local_mods(
                host.host,
                &[
                    not_a_jar.clone(),
                    not_an_archive.clone(),
                    a_directory.clone(),
                    good,
                ],
            )
            .expect("the batch itself succeeds");

        assert_eq!(
            report.added,
            vec!["real.jar".to_owned()],
            "one bad file must not cost the user the good ones"
        );
        let refused: Vec<(PathBuf, LocalModRefusal)> = report.refused;
        assert_eq!(
            refused,
            vec![
                (not_a_jar, LocalModRefusal::NotAJar),
                (not_an_archive, LocalModRefusal::NotAnArchive),
                (a_directory, LocalModRefusal::NotAFile),
            ]
        );
        assert_eq!(names_in(&host.mods_dir), vec!["real.jar"]);
    }

    #[test]
    fn a_name_that_would_leave_the_mods_folder_is_refused() {
        // The path comes from a desktop drag, which is untrusted input like any
        // other. Nothing is sanitized into a name that still writes somewhere.
        let host = fixture();
        let chosen = tempfile::tempdir().expect("temp dir");
        let nested = chosen.path().join("nested");
        std::fs::create_dir_all(&nested).expect("mkdir");
        let source = jar_at(&nested, "fine.jar");

        let plan = host
            .app
            .plan_local_mods(host.host, std::slice::from_ref(&source))
            .expect("plan");
        assert!(
            matches!(plan.offers.as_slice(), [LocalModOffer::Ready { file_name, .. }] if file_name == "fine.jar"),
            "a jar in a subdirectory is still just a file name: {:?}",
            plan.offers
        );

        host.app.add_local_mods(host.host, &[source]).expect("add");
        assert_eq!(names_in(&host.mods_dir), vec!["fine.jar"]);
        assert!(
            !host.mods_dir.join("nested").exists(),
            "nothing below `mods/` is created"
        );
    }

    #[test]
    fn a_name_already_taken_is_reported_rather_than_overwritten_unasked() {
        let host = fixture();
        let chosen = tempfile::tempdir().expect("temp dir");
        let source = jar_at(chosen.path(), "library.jar");
        std::fs::write(host.mods_dir.join("library.jar"), b"the one already here")
            .expect("existing");

        let plan = host
            .app
            .plan_local_mods(host.host, &[source])
            .expect("plan");

        assert_eq!(plan.clashing().count(), 1);
        assert!(
            !matches!(plan.offers[0], LocalModOffer::Ready { .. }),
            "a taken name is a question, not a decision this layer makes"
        );
        assert_eq!(
            std::fs::read(host.mods_dir.join("library.jar")).expect("read"),
            b"the one already here",
            "planning writes nothing"
        );
    }

    #[test]
    fn a_jar_that_is_turned_off_still_owns_its_name() {
        // Writing beside it would leave two copies of one mod in the folder,
        // one of them invisible to the loader and to anybody reading it.
        let host = fixture();
        let chosen = tempfile::tempdir().expect("temp dir");
        let source = jar_at(chosen.path(), "library.jar");
        std::fs::write(host.mods_dir.join("library.jar.disabled"), b"turned off")
            .expect("existing");

        let plan = host
            .app
            .plan_local_mods(host.host, std::slice::from_ref(&source))
            .expect("plan");
        assert!(
            matches!(
                plan.offers.as_slice(),
                [LocalModOffer::Clashing {
                    existing_is_disabled: true,
                    ..
                }]
            ),
            "{:?}",
            plan.offers
        );

        let report = host.app.add_local_mods(host.host, &[source]).expect("add");
        assert_eq!(report.replaced, vec!["library.jar".to_owned()]);
        assert_eq!(
            names_in(&host.mods_dir),
            vec!["library.jar"],
            "the disabled twin goes with it rather than staying hidden"
        );
    }

    #[test]
    fn deleting_a_jar_added_by_hand_removes_both_of_its_spellings() {
        let host = fixture();
        std::fs::write(host.mods_dir.join("dropped-in.jar.disabled"), b"jar").expect("jar");

        let snapshot = host.app.host_mod_snapshot(host.host).expect("snapshot");
        assert_eq!(snapshot.mods[0].presence, ModPresence::Disabled);

        host.app
            .delete_hand_placed_mod(host.host, Path::new("mods/dropped-in.jar"))
            .expect("delete");

        assert!(names_in(&host.mods_dir).is_empty());
        assert!(
            host.app
                .host_mod_snapshot(host.host)
                .expect("snapshot")
                .mods
                .is_empty()
        );
    }

    #[test]
    fn a_recorded_mod_whose_jar_is_gone_is_marked_absent_and_kept_in_the_recipe() {
        // Which is what the mod list filters on, and what a repair reads. The
        // record staying is deliberate: the instance still wants the mod
        // (ADR 0013), so only an explicit deletion forgets it.
        let host = fixture();
        std::fs::write(host.mods_dir.join("still-here.jar"), b"jar").expect("jar");
        record_mod(&host, "still-here.jar", ModOrigin::UserSelected);
        record_mod_appending(
            &host,
            "deleted-while-bisecting.jar",
            ModOrigin::PackDeclared,
        );

        let snapshot = host.app.host_mod_snapshot(host.host).expect("snapshot");

        let absent: Vec<&str> = snapshot
            .missing()
            .map(|entry| entry.file_name.as_str())
            .collect();
        assert_eq!(absent, vec!["deleted-while-bisecting.jar"]);
        assert_eq!(
            snapshot
                .mods
                .iter()
                .filter(|entry| entry.presence != ModPresence::Missing)
                .count(),
            1,
            "and exactly one row is a file that is actually there"
        );

        let named = host
            .app
            .absent_recorded_mods(host.host)
            .expect("absent mods");
        assert_eq!(named.len(), 1, "so a copy can ask about it by name");
        assert_eq!(named[0].name, "deleted-while-bisecting.jar");
    }

    #[test]
    fn deleting_a_path_no_mod_claims_is_refused() {
        let host = fixture();
        std::fs::write(host.mods_dir.join("dropped-in.jar"), b"jar").expect("jar");

        let error = host
            .app
            .delete_hand_placed_mod(host.host, Path::new("mods/nothing-here.jar"))
            .expect_err("a path nothing claims is refused");
        assert!(
            matches!(error, CoreError::ModNotManaged { .. }),
            "{error:?}"
        );
    }

    /// Writes a recipe recording one mod at `name`, with the given ownership.
    fn record_mod(host: &Fixture, name: &str, origin: ModOrigin) {
        record_into(host, name, origin, false);
    }

    /// The same, joining whatever the recipe already records.
    fn record_mod_appending(host: &Fixture, name: &str, origin: ModOrigin) {
        record_into(host, name, origin, true);
    }

    fn record_into(host: &Fixture, name: &str, origin: ModOrigin, keep_existing: bool) {
        let store = SourceStore::new(ephervex_storage::metadata::source_dir(&host.root));
        let mut recipe = if keep_existing && store.has_recipe() {
            store.read_recipe().expect("the recipe")
        } else {
            ephervex_pack::PackRecipe::for_manual_instance(
                "Repro",
                "1.20.1",
                LoaderSelection::vanilla(),
            )
        };
        recipe
            .record_managed(ephervex_pack::PackFileRecipe {
                logical_path: Path::new(MODS_DIR).join(name),
                hashes: Vec::new(),
                downloads: Vec::new(),
                size: Some(3),
                client: ephervex_pack::SideRequirement::Required,
                server: ephervex_pack::SideRequirement::Unknown,
                selected: None,
                origin,
                identity: Some(ephervex_pack::ProviderFileIdentity {
                    provider: Some(ephervex_model::ModProvider::Modrinth),
                    project_id: Some(format!("PROJ-{name}")),
                    version_id: Some("vvvv".to_owned()),
                    file_id: None,
                }),
            })
            .expect("record");
        store.write_recipe(&recipe).expect("write the recipe");
    }

    #[test]
    fn deleting_a_jar_the_launcher_installed_is_refused() {
        // Taking one of those out is a removal, which retires the jar and
        // collects what its dependencies leave orphaned. Deleting it underneath
        // that record would leave the instance describing a mod it lacks.
        let host = fixture();
        std::fs::write(host.mods_dir.join("installed.jar"), b"jar").expect("jar");
        record_mod(&host, "installed.jar", ModOrigin::UserSelected);

        let error = host
            .app
            .delete_hand_placed_mod(host.host, Path::new("mods/installed.jar"))
            .expect_err("a launcher-owned mod is not deleted this way");

        assert!(
            matches!(error, CoreError::ModNotHandPlaced { .. }),
            "{error:?}"
        );
        assert_eq!(names_in(&host.mods_dir), vec!["installed.jar"]);
    }

    #[test]
    fn deleting_a_jar_added_by_hand_forgets_the_record_adoption_wrote() {
        // The ghost row this exists to prevent: adoption records an identified
        // hand-placed jar, and a record left behind by a deleted file comes
        // back as a mod the instance cannot produce.
        let host = fixture();
        std::fs::write(host.mods_dir.join("identified.jar"), b"jar").expect("jar");
        record_mod(&host, "identified.jar", ModOrigin::HandPlaced);

        let before = host.app.host_mod_snapshot(host.host).expect("snapshot");
        assert_eq!(before.mods.len(), 1, "recorded, and its jar is there");

        host.app
            .delete_hand_placed_mod(host.host, Path::new("mods/identified.jar"))
            .expect("delete");

        let layout = host.app.mod_host_layout(host.host).expect("layout");
        assert!(
            layout
                .recipe
                .expect("the recipe survives")
                .recorded_mods()
                .is_empty(),
            "the record goes with the file"
        );
        assert!(
            host.app
                .host_mod_snapshot(host.host)
                .expect("snapshot")
                .mods
                .is_empty(),
            "and no row comes back"
        );
    }
}
