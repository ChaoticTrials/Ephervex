//! Installing mods chosen through a provider, against a loopback server.
//!
//! Everything here runs against `wiremock` on 127.0.0.1. No test contacts
//! Modrinth, so ordinary CI never depends on a live service.
//!
//! The behaviors under test are the ones that decide whether a user's `mods/`
//! directory is safe to point this at: what happens to a file that was already
//! there, what happens when two planned mods want the same name, and what is
//! left behind when any step fails.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use ephervex_download::{
    CacheLayout, DownloadManager, DownloaderConfig, HttpConfig, OperationReporter, RetryPolicy,
};
use ephervex_model::{Hash, HashAlgorithm, LoaderKind, LoaderSelection, ModProvider};
use ephervex_pack::{
    ArtifactFetch, ManagedModInstall, ModOrigin, OpenArtifacts, PackError, PackFileRecipe,
    PackRecipe, PackSide, ProviderFileIdentity, SideRequirement, SourceStore, install_managed,
};
use sha1::{Digest as _, Sha1};
use sha2::Sha512;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const LIBRARY_JAR: &[u8] = b"a fixture jar standing in for a required library";
const ROOT_JAR: &[u8] = b"a fixture jar standing in for the mod a user chose";

fn sha1_of(bytes: &[u8]) -> Hash {
    let mut hasher = Sha1::new();
    hasher.update(bytes);
    Hash::new(HashAlgorithm::Sha1, &hex::encode(hasher.finalize())).expect("valid sha1")
}

fn sha512_of(bytes: &[u8]) -> Hash {
    let mut hasher = Sha512::new();
    hasher.update(bytes);
    Hash::new(HashAlgorithm::Sha512, &hex::encode(hasher.finalize())).expect("valid sha512")
}

/// A manager that gives up immediately, so a deliberate failure costs no
/// backoff.
fn manager() -> DownloadManager {
    DownloadManager::new(DownloaderConfig {
        retry: RetryPolicy::none(),
        http: HttpConfig::for_loopback_tests(),
        ..DownloaderConfig::default()
    })
    .expect("client builds")
}

/// One instance's directories, as `ephervex-storage` lays them out.
struct Instance {
    base: TempDir,
    game_dir: PathBuf,
    store: SourceStore,
    cache: CacheLayout,
}

impl Instance {
    fn new() -> Self {
        let base = tempfile::tempdir().expect("temp dir");
        let game_dir = base.path().join("minecraft");
        let store = SourceStore::new(base.path().join(".launcher/source"));
        std::fs::create_dir_all(game_dir.join("mods")).expect("mods dir");
        std::fs::create_dir_all(store.dir()).expect("source dir");
        Self {
            cache: CacheLayout::new(base.path().join("cache")),
            base,
            game_dir,
            store,
        }
    }

    fn mods_dir(&self) -> PathBuf {
        self.game_dir.join("mods")
    }

    fn jar(&self, name: &str) -> PathBuf {
        self.mods_dir().join(name)
    }
}

fn recipe() -> PackRecipe {
    PackRecipe::for_manual_instance(
        "Repro",
        "1.20.1",
        LoaderSelection::exact(LoaderKind::NeoForge, "47.1.106"),
    )
}

/// One resolved mod, as the provider layer hands it over.
fn addition(
    server: &MockServer,
    route: &str,
    name: &str,
    body: &[u8],
    project: &str,
) -> PackFileRecipe {
    PackFileRecipe {
        logical_path: PathBuf::from("mods").join(name),
        hashes: vec![sha512_of(body), sha1_of(body)],
        downloads: vec![format!("{}{route}", server.uri())],
        size: Some(body.len() as u64),
        client: SideRequirement::Required,
        server: SideRequirement::Unknown,
        selected: None,
        origin: ModOrigin::UserSelected,
        identity: Some(ProviderFileIdentity {
            provider: Some(ModProvider::Modrinth),
            project_id: Some(project.to_owned()),
            version_id: Some(format!("{project}-v1")),
            file_id: None,
        }),
        original_identity: None,
        installed_despite_unsupported: None,
        relations: None,
    }
}

async fn serve(server: &MockServer, route: &str, body: &'static [u8]) {
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
        .mount(server)
        .await;
}

async fn install(
    instance: &Instance,
    recipe: &mut PackRecipe,
    additions: &[PackFileRecipe],
) -> Result<ephervex_pack::ManagedModInstallation, PackError> {
    install_letting_stand_in(instance, recipe, additions, &[]).await
}

/// Installs `additions`, letting a jar already in `mods/` stand in for the
/// ones named in `may_stand_in`.
async fn install_letting_stand_in(
    instance: &Instance,
    recipe: &mut PackRecipe,
    additions: &[PackFileRecipe],
    may_stand_in: &[&str],
) -> Result<ephervex_pack::ManagedModInstallation, PackError> {
    install_with_stand_ins(instance, recipe, additions, may_stand_in, &[]).await
}

/// Installs `additions`, letting a jar already in `mods/` stand in for the
/// ones named in `may_stand_in`, and for those in `must_stand_in` only that
/// way.
async fn install_with_stand_ins(
    instance: &Instance,
    recipe: &mut PackRecipe,
    additions: &[PackFileRecipe],
    may_stand_in: &[&str],
    must_stand_in: &[&str],
) -> Result<ephervex_pack::ManagedModInstallation, PackError> {
    let may_stand_in: BTreeSet<PathBuf> = may_stand_in.iter().map(PathBuf::from).collect();
    let must_stand_in: BTreeSet<PathBuf> = must_stand_in.iter().map(PathBuf::from).collect();
    install_managed(ManagedModInstall {
        recipe,
        additions,
        side: PackSide::Client,
        game_dir: &instance.game_dir,
        store: &instance.store,
        installed_may_stand_in: &may_stand_in,
        installed_must_stand_in: &must_stand_in,
        fetch: ArtifactFetch {
            manager: &manager(),
            cache: &instance.cache,
            credentials: &OpenArtifacts,
            reporter: &OperationReporter::silent("installing-mods"),
            cancel: &CancellationToken::new(),
        },
    })
    .await
}

#[tokio::test]
async fn a_mod_and_its_dependency_are_installed_and_recorded_together() {
    let server = MockServer::start().await;
    serve(&server, "/root.jar", ROOT_JAR).await;
    serve(&server, "/library.jar", LIBRARY_JAR).await;

    let instance = Instance::new();
    let mut recipe = recipe();
    let additions = vec![
        addition(&server, "/root.jar", "root-1.0.jar", ROOT_JAR, "ROOTPROJ"),
        addition(
            &server,
            "/library.jar",
            "library-2.0.jar",
            LIBRARY_JAR,
            "LIBPROJ",
        ),
    ];

    let report = install(&instance, &mut recipe, &additions)
        .await
        .expect("install");
    assert_eq!(report.installed.len(), 2);
    assert!(instance.jar("root-1.0.jar").is_file());
    assert!(instance.jar("library-2.0.jar").is_file());

    // The record survives the process: this is what a reopen reads.
    let stored = instance.store.read_recipe().expect("recipe on disk");
    let mods = stored.managed_mods();
    assert_eq!(mods.len(), 2);
    assert_eq!(mods[0].project_id, "ROOTPROJ");
    assert_eq!(mods[0].version_id, "ROOTPROJ-v1");
    assert_eq!(mods[0].file_name, "root-1.0.jar");
    assert!(!mods[0].hashes.is_empty());
}

#[tokio::test]
async fn a_jar_the_user_put_there_is_never_overwritten_or_adopted() {
    // The file in the way is very often the local build the instance exists to
    // debug. Overwriting it would destroy the reproduction; claiming it would
    // put a jar nothing can re-resolve into a reconstructed server.
    let server = MockServer::start().await;
    serve(&server, "/root.jar", ROOT_JAR).await;

    let instance = Instance::new();
    let occupied = instance.jar("root-1.0.jar");
    std::fs::write(&occupied, b"the developer's own build").expect("write");

    let mut recipe = recipe();
    let additions = vec![addition(
        &server,
        "/root.jar",
        "root-1.0.jar",
        ROOT_JAR,
        "ROOTPROJ",
    )];

    let error = install(&instance, &mut recipe, &additions)
        .await
        .expect_err("must refuse");
    assert!(matches!(error, PackError::DestinationOccupied { .. }));
    assert_eq!(error.failing_path().as_deref(), Some(occupied.as_path()));

    assert_eq!(
        std::fs::read(&occupied).expect("read"),
        b"the developer's own build",
        "the user's file is untouched"
    );
    assert!(
        !instance.store.has_recipe(),
        "nothing was recorded for a refused installation"
    );
    assert!(recipe.managed_mods().is_empty());
}

#[tokio::test]
async fn two_mods_that_would_land_on_one_file_name_are_refused_before_anything_is_written() {
    // Installing them in sequence would leave whichever landed last, quietly
    // producing an instance missing a mod the plan promised.
    let server = MockServer::start().await;
    serve(&server, "/root.jar", ROOT_JAR).await;
    serve(&server, "/library.jar", LIBRARY_JAR).await;

    let instance = Instance::new();
    let mut recipe = recipe();
    let additions = vec![
        addition(
            &server,
            "/root.jar",
            "shared-name.jar",
            ROOT_JAR,
            "ROOTPROJ",
        ),
        addition(
            &server,
            "/library.jar",
            "shared-name.jar",
            LIBRARY_JAR,
            "LIBPROJ",
        ),
    ];

    let error = install(&instance, &mut recipe, &additions)
        .await
        .expect_err("must refuse");
    assert!(matches!(
        error,
        PackError::DestinationCollision { ref name } if name == "mods/shared-name.jar"
    ));
    assert!(
        !instance.jar("shared-name.jar").exists(),
        "the collision is caught before a single copy"
    );
    assert!(!instance.store.has_recipe());
}

#[tokio::test]
async fn a_hash_mismatch_refuses_the_installation_and_leaves_nothing_behind() {
    let server = MockServer::start().await;
    // The server answers with something other than what the provider published.
    serve(&server, "/root.jar", b"not the bytes the hashes describe").await;
    serve(&server, "/library.jar", LIBRARY_JAR).await;

    let instance = Instance::new();
    let mut recipe = recipe();
    let additions = vec![
        addition(&server, "/root.jar", "root-1.0.jar", ROOT_JAR, "ROOTPROJ"),
        addition(
            &server,
            "/library.jar",
            "library-2.0.jar",
            LIBRARY_JAR,
            "LIBPROJ",
        ),
    ];

    let error = install(&instance, &mut recipe, &additions)
        .await
        .expect_err("a mismatching file is never installed");
    assert!(matches!(error, PackError::DownloadFailed { .. }));

    assert!(!instance.jar("root-1.0.jar").exists());
    assert!(
        !instance.jar("library-2.0.jar").exists(),
        "the sound half of a failed installation is rolled back too"
    );
    assert!(
        !instance.store.has_recipe(),
        "no recipe claims mods that are not there"
    );
}

#[tokio::test]
async fn a_recipe_that_cannot_be_written_fails_the_installation_and_removes_the_new_jars() {
    // Recording is not an afterthought: an instance whose mods are not in the
    // recipe cannot rebuild its server, so a failed write is a failed install.
    let server = MockServer::start().await;
    serve(&server, "/root.jar", ROOT_JAR).await;

    let instance = Instance::new();
    let mut recipe = recipe();

    // A recipe already on disk, which must survive intact.
    instance.store.write_recipe(&recipe).expect("seed recipe");
    let before = std::fs::read_to_string(instance.store.recipe_path()).expect("read");

    // A jar that was already in `mods/` and is none of this operation's
    // business. Rollback must not touch it.
    let bystander = instance.jar("someone-elses.jar");
    std::fs::write(&bystander, b"not ours").expect("write");

    if !read_only(instance.store.dir()) {
        writable(instance.store.dir());
        eprintln!(
            "skipped: this process can write into a read-only directory, \
             so the failed recipe write this test rolls back cannot be staged"
        );
        return;
    }
    let additions = vec![addition(
        &server,
        "/root.jar",
        "root-1.0.jar",
        ROOT_JAR,
        "ROOTPROJ",
    )];
    let result = install(&instance, &mut recipe, &additions).await;
    writable(instance.store.dir());

    assert!(
        matches!(result, Err(PackError::Unwritable { .. })),
        "expected the write failure to surface, got {result:?}"
    );
    assert!(
        !instance.jar("root-1.0.jar").exists(),
        "the jar this operation created was removed"
    );
    assert!(
        bystander.is_file(),
        "rollback removes only what this operation wrote"
    );
    assert_eq!(
        std::fs::read_to_string(instance.store.recipe_path()).expect("read"),
        before,
        "the previous recipe is preserved byte for byte"
    );
    assert!(
        recipe.managed_mods().is_empty(),
        "the in-memory recipe does not claim what the disk does not have"
    );
}

#[tokio::test]
async fn reinstalling_the_same_version_changes_nothing_and_downloads_nothing() {
    let server = MockServer::start().await;
    serve(&server, "/root.jar", ROOT_JAR).await;

    let instance = Instance::new();
    let mut recipe = recipe();
    let additions = vec![addition(
        &server,
        "/root.jar",
        "root-1.0.jar",
        ROOT_JAR,
        "ROOTPROJ",
    )];

    install(&instance, &mut recipe, &additions)
        .await
        .expect("first install");
    let after_first = std::fs::read_to_string(instance.store.recipe_path()).expect("read");

    let report = install(&instance, &mut recipe, &additions)
        .await
        .expect("second install");
    assert!(report.installed.is_empty());
    assert_eq!(report.already_present.len(), 1);
    assert!(!report.changed_anything());
    assert_eq!(
        std::fs::read_to_string(instance.store.recipe_path()).expect("read"),
        after_first,
        "an exact repeat rewrites nothing"
    );
    assert_eq!(recipe.managed_mods().len(), 1, "and records nothing twice");
}

#[tokio::test]
async fn a_recorded_jar_that_has_gone_missing_is_restored_from_the_verified_cache() {
    let server = MockServer::start().await;
    serve(&server, "/root.jar", ROOT_JAR).await;

    let instance = Instance::new();
    let mut recipe = recipe();
    let additions = vec![addition(
        &server,
        "/root.jar",
        "root-1.0.jar",
        ROOT_JAR,
        "ROOTPROJ",
    )];

    install(&instance, &mut recipe, &additions)
        .await
        .expect("first install");
    let recorded = std::fs::read_to_string(instance.store.recipe_path()).expect("read");

    // Stand in for a user deleting a mod to bisect a crash and then asking for
    // it back.
    std::fs::remove_file(instance.jar("root-1.0.jar")).expect("remove");

    let report = install(&instance, &mut recipe, &additions)
        .await
        .expect("repair");
    assert_eq!(report.repaired.len(), 1);
    assert!(report.installed.is_empty());
    assert_eq!(
        report.reused_from_cache, 1,
        "the artifact came from the verified cache, not the network"
    );
    assert!(instance.jar("root-1.0.jar").is_file());
    assert_eq!(
        std::fs::read_to_string(instance.store.recipe_path()).expect("read"),
        recorded,
        "repair is not an update; the record does not change"
    );
}

#[tokio::test]
async fn a_second_instance_reuses_the_verified_shared_cache() {
    let server = MockServer::start().await;
    serve(&server, "/root.jar", ROOT_JAR).await;

    let first = Instance::new();
    let mut recipe_one = recipe();
    let additions = vec![addition(
        &server,
        "/root.jar",
        "root-1.0.jar",
        ROOT_JAR,
        "ROOTPROJ",
    )];
    let report = install(&first, &mut recipe_one, &additions)
        .await
        .expect("first install");
    assert_eq!(report.reused_from_cache, 0, "nothing was cached yet");

    // A second instance sharing one cache - bisecting a bug looks exactly like
    // this - pays no network cost.
    let second = Instance {
        base: tempfile::tempdir().expect("temp dir"),
        game_dir: first
            .game_dir
            .parent()
            .expect("parent")
            .join("second/minecraft"),
        store: SourceStore::new(
            first
                .game_dir
                .parent()
                .expect("parent")
                .join("second/.launcher/source"),
        ),
        cache: first.cache.clone(),
    };
    std::fs::create_dir_all(second.mods_dir()).expect("mods dir");
    std::fs::create_dir_all(second.store.dir()).expect("source dir");

    let mut recipe_two = recipe();
    let report = install(&second, &mut recipe_two, &additions)
        .await
        .expect("second install");
    assert_eq!(report.installed.len(), 1);
    assert_eq!(
        report.reused_from_cache, 1,
        "the artifact was already verified in the shared cache"
    );
    assert!(second.jar("root-1.0.jar").is_file());
}

#[tokio::test]
async fn an_installed_mod_reaches_the_server_plan_without_the_original_archive() {
    // The reason managed mods are recipe entries: a mod added through the
    // browser must not be invisible when a server is built later.
    let server = MockServer::start().await;
    serve(&server, "/library.jar", LIBRARY_JAR).await;

    let instance = Instance::new();
    let mut recipe = recipe();
    let mut entry = addition(
        &server,
        "/library.jar",
        "library-2.0.jar",
        LIBRARY_JAR,
        "LIBPROJ",
    );
    // A project that declares it belongs on both sides.
    entry.server = SideRequirement::Required;

    install(&instance, &mut recipe, &[entry])
        .await
        .expect("install");

    let reopened = instance.store.read_recipe().expect("recipe on disk");
    let plan = reopened.plan(PackSide::Server, &ephervex_pack::OptionalSelection::none());
    assert_eq!(
        plan.files.len(),
        1,
        "a browser-installed mod is part of what the server is supposed to contain"
    );
    assert_eq!(plan.minecraft_version, "1.20.1");
}

/// Makes a directory refuse new files, so a write into it fails the way a full
/// disk or a permission change would.
///
/// Reports whether the refusal actually holds for this process. A privileged
/// process is not subject to the mode bits, and neither is one on a filesystem
/// that does not enforce them, so on those the barrier is not a barrier. Probed
/// on the directory itself rather than inferred from the user id, because
/// privilege is not the only way it fails to hold.
#[must_use]
fn read_only(dir: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    let mut permissions = std::fs::metadata(dir).expect("metadata").permissions();
    permissions.set_mode(0o555);
    std::fs::set_permissions(dir, permissions).expect("set permissions");

    let probe = dir.join("write-probe");
    match std::fs::write(&probe, b"probe") {
        Ok(()) => {
            std::fs::remove_file(&probe).expect("remove the probe");
            false
        }
        Err(_) => true,
    }
}

fn writable(dir: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    let mut permissions = std::fs::metadata(dir).expect("metadata").permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(dir, permissions).expect("set permissions");
}

fn root_addition(server: &MockServer, name: &str) -> PackFileRecipe {
    addition(server, "/root.jar", name, ROOT_JAR, "ROOTPROJ")
}

#[tokio::test]
async fn a_request_at_another_path_for_the_recorded_version_leaves_one_copy() {
    // The recorded artifact is the same one wherever the caller would have put
    // it. A second copy beside it is two builds of one mod to the loader, with
    // a recipe that only knows the first.
    let server = MockServer::start().await;
    serve(&server, "/root.jar", ROOT_JAR).await;
    let instance = Instance::new();
    let mut recipe = recipe();
    install(
        &instance,
        &mut recipe,
        &[root_addition(&server, "root-1.0.jar")],
    )
    .await
    .expect("first install");
    let recorded = std::fs::read_to_string(instance.store.recipe_path()).expect("read");

    let report = install(
        &instance,
        &mut recipe,
        &[root_addition(&server, "root-renamed.jar")],
    )
    .await
    .expect("request at another path");

    assert_eq!(report.already_present, [Path::new("mods/root-1.0.jar")]);
    assert!(!report.changed_anything());
    assert!(!instance.jar("root-renamed.jar").exists());
    assert!(instance.jar("root-1.0.jar").is_file());
    assert_eq!(
        std::fs::read_to_string(instance.store.recipe_path()).expect("read"),
        recorded
    );
}

#[tokio::test]
async fn a_missing_recorded_jar_is_restored_where_the_recipe_records_it() {
    let server = MockServer::start().await;
    serve(&server, "/root.jar", ROOT_JAR).await;
    let instance = Instance::new();
    let mut recipe = recipe();
    install(
        &instance,
        &mut recipe,
        &[root_addition(&server, "root-1.0.jar")],
    )
    .await
    .expect("first install");
    std::fs::remove_file(instance.jar("root-1.0.jar")).expect("delete the jar");

    let report = install(
        &instance,
        &mut recipe,
        &[root_addition(&server, "root-renamed.jar")],
    )
    .await
    .expect("request at another path");

    assert_eq!(report.repaired, [Path::new("mods/root-1.0.jar")]);
    assert!(instance.jar("root-1.0.jar").is_file());
    assert!(!instance.jar("root-renamed.jar").exists());
    assert_eq!(
        recipe.managed_mods()[0].file_name,
        "root-1.0.jar",
        "the record keeps describing the installed file"
    );
}

#[tokio::test]
async fn a_recorded_mod_that_is_turned_off_stays_off_when_it_is_installed_again() {
    let server = MockServer::start().await;
    serve(&server, "/root.jar", ROOT_JAR).await;
    let instance = Instance::new();
    let mut recipe = recipe();
    let additions = [root_addition(&server, "root-1.0.jar")];
    install(&instance, &mut recipe, &additions)
        .await
        .expect("first install");
    std::fs::rename(
        instance.jar("root-1.0.jar"),
        instance.jar("root-1.0.jar.disabled"),
    )
    .expect("turn the mod off");

    let report = install(&instance, &mut recipe, &additions)
        .await
        .expect("install again");

    assert_eq!(report.already_present, [Path::new("mods/root-1.0.jar")]);
    assert!(report.repaired.is_empty());
    assert!(
        !instance.jar("root-1.0.jar").exists(),
        "turning a mod off is the user's decision, not damage"
    );
    assert_eq!(
        std::fs::read(instance.jar("root-1.0.jar.disabled")).expect("read"),
        ROOT_JAR
    );
}

#[tokio::test]
async fn a_recorded_jar_with_other_bytes_is_left_alone_and_reported() {
    // Very often the local build the instance exists to test. A repeated
    // install must neither claim it matches nor overwrite it.
    let server = MockServer::start().await;
    serve(&server, "/root.jar", ROOT_JAR).await;
    let instance = Instance::new();
    let mut recipe = recipe();
    let additions = [root_addition(&server, "root-1.0.jar")];
    install(&instance, &mut recipe, &additions)
        .await
        .expect("first install");
    let recorded = std::fs::read_to_string(instance.store.recipe_path()).expect("read");
    std::fs::write(instance.jar("root-1.0.jar"), b"the developer's own build").expect("modify");

    let report = install(&instance, &mut recipe, &additions)
        .await
        .expect("install again");

    assert_eq!(report.modified, [Path::new("mods/root-1.0.jar")]);
    assert!(report.already_present.is_empty());
    assert!(!report.changed_anything());
    assert_eq!(
        std::fs::read(instance.jar("root-1.0.jar")).expect("read"),
        b"the developer's own build"
    );
    assert_eq!(
        std::fs::read_to_string(instance.store.recipe_path()).expect("read"),
        recorded
    );
}

#[tokio::test]
async fn a_repair_restores_the_recorded_bytes_even_when_the_request_names_another_file() {
    // The same project and version can offer more than one file. The recipe's
    // hashes describe the recorded one, so that is what a repair must write.
    let server = MockServer::start().await;
    serve(&server, "/root.jar", ROOT_JAR).await;
    serve(&server, "/library.jar", LIBRARY_JAR).await;
    let instance = Instance::new();
    let mut recipe = recipe();
    install(
        &instance,
        &mut recipe,
        &[root_addition(&server, "root-1.0.jar")],
    )
    .await
    .expect("first install");
    std::fs::remove_file(instance.jar("root-1.0.jar")).expect("delete the jar");
    let other_file = addition(
        &server,
        "/library.jar",
        "root-other-file.jar",
        LIBRARY_JAR,
        "ROOTPROJ",
    );

    let report = install(&instance, &mut recipe, &[other_file])
        .await
        .expect("repair");

    assert_eq!(report.repaired, [Path::new("mods/root-1.0.jar")]);
    assert_eq!(
        std::fs::read(instance.jar("root-1.0.jar")).expect("read"),
        ROOT_JAR
    );
    assert!(!instance.jar("root-other-file.jar").exists());
}

#[tokio::test]
async fn a_recorded_jar_with_no_address_is_restored_from_the_requests_address() {
    // A hand-placed jar a catalog recognized is recorded with no download
    // address. The provider being asked serves exactly its bytes.
    let server = MockServer::start().await;
    serve(&server, "/root.jar", ROOT_JAR).await;
    let instance = Instance::new();
    let mut recipe = recipe();
    let additions = [root_addition(&server, "root-1.0.jar")];
    install(&instance, &mut recipe, &additions)
        .await
        .expect("first install");
    recipe.files[0].downloads.clear();
    std::fs::remove_file(instance.jar("root-1.0.jar")).expect("delete the jar");
    std::fs::remove_dir_all(instance.base.path().join("cache")).expect("empty the cache");

    let report = install(&instance, &mut recipe, &additions)
        .await
        .expect("repair");

    assert_eq!(report.repaired, [Path::new("mods/root-1.0.jar")]);
    assert_eq!(
        std::fs::read(instance.jar("root-1.0.jar")).expect("read"),
        ROOT_JAR
    );
}

// -- destinations ---------------------------------------------------------

/// A directory outside the instance that nothing may be written into.
fn outside(instance: &Instance) -> PathBuf {
    let dir = instance.base.path().join("outside");
    std::fs::create_dir_all(&dir).expect("outside dir");
    dir
}

fn dir_is_empty(dir: &Path) -> bool {
    std::fs::read_dir(dir).expect("read dir").next().is_none()
}

#[cfg(unix)]
#[tokio::test]
async fn a_dangling_link_at_the_destination_is_refused_and_never_followed() {
    let server = MockServer::start().await;
    serve(&server, "/root.jar", ROOT_JAR).await;
    let instance = Instance::new();
    let target = outside(&instance).join("created.jar");
    std::os::unix::fs::symlink(&target, instance.jar("root-1.0.jar")).expect("link");
    let mut recipe = recipe();

    let error = install(
        &instance,
        &mut recipe,
        &[root_addition(&server, "root-1.0.jar")],
    )
    .await
    .expect_err("must refuse");

    assert!(matches!(error, PackError::ManagedDestinationLinked { .. }));
    assert!(!target.exists(), "nothing was written through the link");
    assert!(recipe.managed_mods().is_empty());
    assert!(!instance.store.has_recipe());
}

#[cfg(unix)]
#[tokio::test]
async fn a_linked_mods_directory_is_refused_and_never_written_through() {
    let server = MockServer::start().await;
    serve(&server, "/root.jar", ROOT_JAR).await;
    let instance = Instance::new();
    let outside = outside(&instance);
    std::fs::remove_dir(instance.mods_dir()).expect("remove mods");
    std::os::unix::fs::symlink(&outside, instance.mods_dir()).expect("link");
    let mut recipe = recipe();

    let error = install(
        &instance,
        &mut recipe,
        &[root_addition(&server, "root-1.0.jar")],
    )
    .await
    .expect_err("must refuse");

    assert!(matches!(error, PackError::ManagedDestinationLinked { .. }));
    assert!(dir_is_empty(&outside));
}

#[cfg(unix)]
#[tokio::test]
async fn a_repair_never_writes_through_a_dangling_link_at_the_recorded_path() {
    let server = MockServer::start().await;
    serve(&server, "/root.jar", ROOT_JAR).await;
    let instance = Instance::new();
    let mut recipe = recipe();
    let additions = [root_addition(&server, "root-1.0.jar")];
    install(&instance, &mut recipe, &additions)
        .await
        .expect("first install");
    std::fs::remove_file(instance.jar("root-1.0.jar")).expect("delete the jar");
    let target = outside(&instance).join("created.jar");
    std::os::unix::fs::symlink(&target, instance.jar("root-1.0.jar")).expect("link");

    let report = install(&instance, &mut recipe, &additions)
        .await
        .expect("a link is reported, not repaired");

    assert_eq!(report.modified, [Path::new("mods/root-1.0.jar")]);
    assert!(report.repaired.is_empty());
    assert!(!target.exists(), "nothing was written through the link");
}

#[cfg(unix)]
#[tokio::test]
async fn a_repair_beneath_a_linked_directory_is_refused() {
    let server = MockServer::start().await;
    serve(&server, "/root.jar", ROOT_JAR).await;
    let instance = Instance::new();
    let mut recipe = recipe();
    let additions = [root_addition(&server, "root-1.0.jar")];
    install(&instance, &mut recipe, &additions)
        .await
        .expect("first install");
    let outside = outside(&instance);
    std::fs::remove_dir_all(instance.mods_dir()).expect("remove mods");
    std::os::unix::fs::symlink(&outside, instance.mods_dir()).expect("link");

    let error = install(&instance, &mut recipe, &additions)
        .await
        .expect_err("must refuse");

    assert!(matches!(error, PackError::ManagedDestinationLinked { .. }));
    assert!(dir_is_empty(&outside));
}

#[tokio::test]
async fn an_unrecorded_disabled_twin_occupies_the_destination() {
    let server = MockServer::start().await;
    serve(&server, "/root.jar", ROOT_JAR).await;
    let instance = Instance::new();
    let twin = instance.jar("root-1.0.jar.disabled");
    std::fs::write(&twin, b"the developer's own build, turned off").expect("write");
    let mut recipe = recipe();

    let error = install(
        &instance,
        &mut recipe,
        &[root_addition(&server, "root-1.0.jar")],
    )
    .await
    .expect_err("must refuse");

    assert!(matches!(error, PackError::DestinationOccupied { .. }));
    assert_eq!(error.failing_path().as_deref(), Some(twin.as_path()));
    assert!(!instance.jar("root-1.0.jar").exists());
    assert!(recipe.managed_mods().is_empty());
}

#[tokio::test]
async fn a_logical_path_outside_the_game_directory_is_refused_before_anything_is_written() {
    let server = MockServer::start().await;
    serve(&server, "/root.jar", ROOT_JAR).await;
    let instance = Instance::new();
    let escaped = instance.base.path().join("outside.jar");
    let absolute = instance.base.path().join("absolute.jar");

    for logical in [
        PathBuf::from("../outside.jar"),
        PathBuf::from("mods/../../outside.jar"),
        absolute.clone(),
        PathBuf::from(".launcher/source/recipe.jar"),
        PathBuf::from("mods/./root-1.0.jar"),
        PathBuf::new(),
    ] {
        let mut recipe = recipe();
        let mut file = root_addition(&server, "root-1.0.jar");
        file.logical_path = logical.clone();

        let error = install(&instance, &mut recipe, &[file])
            .await
            .expect_err("must refuse");

        assert!(
            matches!(error, PackError::ManagedPathUnsafe { .. }),
            "{logical:?} gave {error:?}"
        );
        assert!(recipe.managed_mods().is_empty());
    }
    assert!(!escaped.exists());
    assert!(!absolute.exists());
    assert!(!instance.store.has_recipe());
}

#[tokio::test]
async fn a_recorded_logical_path_outside_the_game_directory_is_never_repaired() {
    let server = MockServer::start().await;
    serve(&server, "/root.jar", ROOT_JAR).await;
    let instance = Instance::new();
    let mut recipe = recipe();
    let additions = [root_addition(&server, "root-1.0.jar")];
    install(&instance, &mut recipe, &additions)
        .await
        .expect("first install");
    recipe.files[0].logical_path = PathBuf::from("../outside.jar");

    let error = install(&instance, &mut recipe, &additions)
        .await
        .expect_err("must refuse");

    assert!(matches!(error, PackError::ManagedPathUnsafe { .. }));
    assert!(!instance.base.path().join("outside.jar").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn a_linked_disabled_twin_is_refused_and_named_as_the_link() {
    let server = MockServer::start().await;
    serve(&server, "/root.jar", ROOT_JAR).await;
    let instance = Instance::new();
    let target = outside(&instance).join("created.jar");
    let twin = instance.jar("root-1.0.jar.disabled");
    std::os::unix::fs::symlink(&target, &twin).expect("link");
    let mut recipe = recipe();

    let error = install(
        &instance,
        &mut recipe,
        &[root_addition(&server, "root-1.0.jar")],
    )
    .await
    .expect_err("must refuse");

    assert!(matches!(error, PackError::ManagedDestinationLinked { .. }));
    assert_eq!(error.failing_path().as_deref(), Some(twin.as_path()));
    assert!(!target.exists());
    assert!(!instance.jar("root-1.0.jar").exists());
}

#[tokio::test]
async fn one_project_requested_at_two_paths_is_refused_before_any_jar_is_written() {
    // Both jars would be written but only the first recorded, leaving a jar the
    // recipe does not describe.
    let server = MockServer::start().await;
    serve(&server, "/root.jar", ROOT_JAR).await;

    let instance = Instance::new();
    let mut recipe = recipe();
    let additions = vec![
        addition(&server, "/root.jar", "root-a.jar", ROOT_JAR, "ROOTPROJ"),
        addition(&server, "/root.jar", "root-b.jar", ROOT_JAR, "ROOTPROJ"),
    ];

    let error = install(&instance, &mut recipe, &additions)
        .await
        .expect_err("must refuse");
    assert!(matches!(
        error,
        PackError::DuplicateManagedProject { ref project_id, .. } if project_id == "ROOTPROJ"
    ));
    assert!(!instance.jar("root-a.jar").exists());
    assert!(!instance.jar("root-b.jar").exists());
    assert!(!instance.store.has_recipe());
}

#[tokio::test]
async fn a_project_the_recipe_records_twice_is_refused_when_it_is_requested_again() {
    let server = MockServer::start().await;
    serve(&server, "/root.jar", ROOT_JAR).await;

    let instance = Instance::new();
    let mut recipe = recipe();
    let first = addition(&server, "/root.jar", "root-a.jar", ROOT_JAR, "ROOTPROJ");
    let mut second = first.clone();
    second.logical_path = PathBuf::from("mods/root-b.jar");
    recipe.files.push(first.clone());
    recipe.files.push(second);

    let error = install(&instance, &mut recipe, &[first])
        .await
        .expect_err("must refuse");
    assert!(matches!(error, PackError::DuplicateManagedProject { .. }));
    assert!(!instance.jar("root-a.jar").exists());
}

// -- recording the side a mod is installed on ------------------------------

/// Records the installed mod as one the client's own record does not take.
fn leave_client_out(recipe: &mut PackRecipe, requirement: SideRequirement) {
    for file in &mut recipe.files {
        file.client = requirement;
        file.selected = None;
    }
}

fn recorded_root(recipe: &PackRecipe) -> &PackFileRecipe {
    recipe
        .managed_project(ModProvider::Modrinth, "ROOTPROJ")
        .expect("entry")
}

async fn installed_root(server: &MockServer, instance: &Instance) -> (PackRecipe, PackFileRecipe) {
    serve(server, "/root.jar", ROOT_JAR).await;
    let mut recipe = recipe();
    let root = addition(server, "/root.jar", "root-1.0.jar", ROOT_JAR, "ROOTPROJ");
    install(instance, &mut recipe, std::slice::from_ref(&root))
        .await
        .expect("first install");
    (recipe, root)
}

#[tokio::test]
async fn a_missing_jar_put_back_on_a_client_the_record_left_out_records_the_client() {
    for left_out in [
        SideRequirement::Unsupported,
        SideRequirement::Unknown,
        SideRequirement::Optional,
    ] {
        let server = MockServer::start().await;
        let instance = Instance::new();
        let (mut recipe, root) = installed_root(&server, &instance).await;
        leave_client_out(&mut recipe, left_out);
        std::fs::remove_file(instance.jar("root-1.0.jar")).expect("delete the jar");

        let report = install(&instance, &mut recipe, &[root])
            .await
            .expect("install on the client");

        assert_eq!(report.repaired, [Path::new("mods/root-1.0.jar")]);
        assert!(instance.jar("root-1.0.jar").is_file());
        let recorded = recorded_root(&recipe);
        assert!(
            recorded.is_installed_on(PackSide::Client, &ephervex_pack::OptionalSelection::none()),
            "a jar was written, so the recipe has to say the client has it ({left_out:?})"
        );
        assert_eq!(recorded.origin, ModOrigin::UserSelected);
        assert_eq!(instance.store.read_recipe().expect("recipe"), recipe);
    }
}

#[tokio::test]
async fn a_present_jar_on_a_client_the_record_left_out_only_changes_the_record() {
    let server = MockServer::start().await;
    let instance = Instance::new();
    let (mut recipe, root) = installed_root(&server, &instance).await;
    leave_client_out(&mut recipe, SideRequirement::Unsupported);

    let report = install(&instance, &mut recipe, &[root])
        .await
        .expect("install on the client");

    assert_eq!(report.recorded, [Path::new("mods/root-1.0.jar")]);
    assert!(report.installed.is_empty() && report.repaired.is_empty());
    assert!(report.changed_anything());
    assert_eq!(recorded_root(&recipe).client, SideRequirement::Required);
    assert_eq!(instance.store.read_recipe().expect("recipe"), recipe);
}

#[tokio::test]
async fn an_unselected_optional_mod_installed_on_the_client_is_recorded_as_selected() {
    let server = MockServer::start().await;
    let instance = Instance::new();
    let (mut recipe, mut root) = installed_root(&server, &instance).await;
    leave_client_out(&mut recipe, SideRequirement::Optional);
    std::fs::remove_file(instance.jar("root-1.0.jar")).expect("delete the jar");
    root.client = SideRequirement::Optional;
    root.selected = Some(true);

    install(&instance, &mut recipe, &[root])
        .await
        .expect("install on the client");

    let recorded = recorded_root(&recipe);
    assert_eq!(recorded.client, SideRequirement::Optional);
    assert_eq!(recorded.selected, Some(true));
}

#[tokio::test]
async fn a_modified_jar_is_not_recorded_as_installed_on_a_client_the_record_left_out() {
    let server = MockServer::start().await;
    let instance = Instance::new();
    let (mut recipe, root) = installed_root(&server, &instance).await;
    leave_client_out(&mut recipe, SideRequirement::Unsupported);
    std::fs::write(instance.jar("root-1.0.jar"), b"the developer's own build").expect("modify");
    let before = recipe.clone();

    let report = install(&instance, &mut recipe, &[root])
        .await
        .expect("request");

    assert_eq!(report.modified.len(), 1);
    assert!(report.recorded.is_empty());
    assert_eq!(recipe, before);
}

#[tokio::test]
async fn a_record_that_already_takes_the_client_is_left_as_recorded_by_a_repeat_install() {
    let server = MockServer::start().await;
    let instance = Instance::new();
    let (mut recipe, root) = installed_root(&server, &instance).await;
    let before = recipe.clone();

    let report = install(&instance, &mut recipe, std::slice::from_ref(&root))
        .await
        .expect("repeat");
    assert!(report.recorded.is_empty());
    assert_eq!(report.already_present.len(), 1);

    std::fs::remove_file(instance.jar("root-1.0.jar")).expect("delete the jar");
    install(&instance, &mut recipe, &[root])
        .await
        .expect("repair");
    assert_eq!(recipe, before, "a plain repair does not touch the record");
}

#[tokio::test]
async fn a_present_jar_recorded_with_an_undeclared_server_takes_the_release_declaration() {
    for declared in [
        SideRequirement::Required,
        SideRequirement::Optional,
        SideRequirement::Unsupported,
    ] {
        let server = MockServer::start().await;
        let instance = Instance::new();
        let (mut recipe, mut root) = installed_root(&server, &instance).await;
        assert_eq!(recorded_root(&recipe).server, SideRequirement::Unknown);
        root.server = declared;

        let report = install(&instance, &mut recipe, &[root])
            .await
            .expect("install on the client");

        assert_eq!(
            report.recorded,
            [Path::new("mods/root-1.0.jar")],
            "{declared:?}"
        );
        let recorded = recorded_root(&recipe);
        assert_eq!(recorded.server, declared);
        assert_eq!(recorded.client, SideRequirement::Required);
        assert_eq!(instance.store.read_recipe().expect("recipe"), recipe);
    }
}

#[tokio::test]
async fn a_record_that_answered_the_other_side_keeps_its_answer_when_the_release_declares_otherwise()
 {
    let server = MockServer::start().await;
    let instance = Instance::new();
    let (mut recipe, mut root) = installed_root(&server, &instance).await;
    for file in &mut recipe.files {
        file.server = SideRequirement::Optional;
        file.selected = Some(false);
    }
    root.server = SideRequirement::Unsupported;
    let before = recipe.clone();

    let report = install(&instance, &mut recipe, &[root])
        .await
        .expect("install on the client");

    assert!(report.recorded.is_empty());
    assert_eq!(recipe, before);
}

// -- a second copy of one mod ---------------------------------------------

/// A jar declaring one NeoForge mod id. `release` varies the bytes the way two
/// releases of one mod do.
fn neoforge_jar(mod_id: &str, release: &str) -> &'static [u8] {
    use std::io::Write as _;
    let mut bytes = Vec::new();
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut bytes));
    let options =
        zip::write::SimpleFileOptions::default().last_modified_time(zip::DateTime::default());
    zip.start_file("META-INF/neoforge.mods.toml", options)
        .expect("entry");
    write!(
        zip,
        "modLoader=\"javafml\"\n[[mods]]\nmodId=\"{mod_id}\"\nversion=\"{release}\"\n"
    )
    .expect("write");
    zip.finish().expect("finish");
    Box::leak(bytes.into_boxed_slice())
}

/// The same addition, as another catalog publishes it.
fn from_catalog(mut file: PackFileRecipe, provider: ModProvider) -> PackFileRecipe {
    if let Some(identity) = file.identity.as_mut() {
        identity.provider = Some(provider);
    }
    file
}

/// Installs release 7.0.3 of a library from `installed_from`, then a mod from
/// `requested_from` whose dependency is that catalog's release 7.0.4 of the
/// same library.
async fn install_the_other_catalogs_release(
    installed_from: ModProvider,
    requested_from: ModProvider,
) -> (Instance, PackRecipe, PackError) {
    let older = neoforge_jar("libx", "7.0.3");
    let newer = neoforge_jar("libx", "7.0.4");
    let server = MockServer::start().await;
    serve(&server, "/libx-7.0.3.jar", older).await;
    serve(&server, "/libx-7.0.4.jar", newer).await;
    serve(&server, "/root.jar", ROOT_JAR).await;

    let instance = Instance::new();
    let mut recipe = recipe();
    install(
        &instance,
        &mut recipe,
        &[from_catalog(
            addition(
                &server,
                "/libx-7.0.3.jar",
                "libx-7.0.3.jar",
                older,
                "LIBX-A",
            ),
            installed_from,
        )],
    )
    .await
    .expect("installs the library");

    let requested = [
        from_catalog(
            addition(&server, "/root.jar", "root-1.0.jar", ROOT_JAR, "ROOT-B"),
            requested_from,
        ),
        from_catalog(
            addition(
                &server,
                "/libx-7.0.4.jar",
                "libx-7.0.4.jar",
                newer,
                "LIBX-B",
            ),
            requested_from,
        ),
    ];
    let error = install(&instance, &mut recipe, &requested)
        .await
        .expect_err("a second copy of the library is refused");
    (instance, recipe, error)
}

fn mod_jars(instance: &Instance) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(instance.mods_dir())
        .expect("mods dir")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn a_curseforge_dependency_that_is_another_release_of_a_modrinth_mod_is_refused() {
    let (instance, recipe, error) =
        install_the_other_catalogs_release(ModProvider::Modrinth, ModProvider::CurseForge).await;

    match &error {
        PackError::DuplicateModId {
            mod_id,
            incoming,
            path,
            provider,
            could_stand_in,
        } => {
            assert_eq!(mod_id, "libx");
            assert_eq!(incoming, "mods/libx-7.0.4.jar");
            assert_eq!(path, &instance.jar("libx-7.0.3.jar"));
            assert_eq!(*provider, Some(ModProvider::Modrinth));
            assert!(*could_stand_in);
        }
        other => panic!("expected a duplicate mod id, got {other:?}"),
    }
    assert_eq!(error.message_id(), "error-mod-duplicate-mod-id-recorded");
    assert_eq!(mod_jars(&instance), ["libx-7.0.3.jar"]);
    assert_eq!(recipe.managed_mods().len(), 1);
    assert_eq!(
        instance
            .store
            .read_recipe()
            .expect("recipe")
            .managed_mods()
            .len(),
        1,
        "nothing was recorded for a refused installation"
    );
}

#[tokio::test]
async fn a_modrinth_dependency_that_is_another_release_of_a_curseforge_mod_is_refused() {
    let (instance, _, error) =
        install_the_other_catalogs_release(ModProvider::CurseForge, ModProvider::Modrinth).await;

    assert!(
        matches!(
            &error,
            PackError::DuplicateModId {
                provider: Some(ModProvider::CurseForge),
                ..
            }
        ),
        "{error:?}"
    );
    assert_eq!(mod_jars(&instance), ["libx-7.0.3.jar"]);
}

#[tokio::test]
async fn a_mod_whose_id_a_jar_added_by_hand_already_provides_is_refused() {
    let release = neoforge_jar("libx", "7.0.4");
    let server = MockServer::start().await;
    serve(&server, "/libx.jar", release).await;
    let instance = Instance::new();
    std::fs::write(
        instance.jar("libx-dev.jar"),
        neoforge_jar("libx", "the developer's build"),
    )
    .expect("write");
    let mut recipe = recipe();

    let error = install(
        &instance,
        &mut recipe,
        &[addition(
            &server,
            "/libx.jar",
            "libx-7.0.4.jar",
            release,
            "LIBX",
        )],
    )
    .await
    .expect_err("a second copy is refused");

    assert!(
        matches!(&error, PackError::DuplicateModId { provider: None, .. }),
        "{error:?}"
    );
    assert_eq!(error.message_id(), "error-mod-duplicate-mod-id");
    assert_eq!(error.failing_path(), Some(instance.jar("libx-dev.jar")));
    assert_eq!(mod_jars(&instance), ["libx-dev.jar"]);
    assert!(!instance.store.has_recipe());
}

#[tokio::test]
async fn a_disabled_copy_of_the_mod_does_not_stop_an_install() {
    let release = neoforge_jar("libx", "7.0.4");
    let server = MockServer::start().await;
    serve(&server, "/libx.jar", release).await;
    let instance = Instance::new();
    std::fs::write(
        instance.jar("libx-7.0.3.jar.disabled"),
        neoforge_jar("libx", "7.0.3"),
    )
    .expect("write");
    let mut recipe = recipe();

    install(
        &instance,
        &mut recipe,
        &[addition(
            &server,
            "/libx.jar",
            "libx-7.0.4.jar",
            release,
            "LIBX",
        )],
    )
    .await
    .expect("the loader never reads the disabled copy");

    assert_eq!(
        mod_jars(&instance),
        ["libx-7.0.3.jar.disabled", "libx-7.0.4.jar"]
    );
}

#[tokio::test]
async fn two_jars_of_one_installation_that_declare_one_mod_id_are_refused() {
    let first = neoforge_jar("libx", "a");
    let second = neoforge_jar("libx", "b");
    let server = MockServer::start().await;
    serve(&server, "/a.jar", first).await;
    serve(&server, "/b.jar", second).await;
    let instance = Instance::new();
    let mut recipe = recipe();

    let error = install(
        &instance,
        &mut recipe,
        &[
            addition(&server, "/a.jar", "libx-a.jar", first, "LIBX-A"),
            addition(&server, "/b.jar", "libx-b.jar", second, "LIBX-B"),
        ],
    )
    .await
    .expect_err("two copies are refused");

    assert!(
        matches!(&error, PackError::DuplicateModIdInChange { mod_id, .. } if mod_id == "libx"),
        "{error:?}"
    );
    assert!(mod_jars(&instance).is_empty());
}

/// A mod and the library it needs, with a developer's own build of that library
/// already in `mods/`.
async fn a_mod_needing_a_library_the_developer_built(
    server: &MockServer,
) -> (Instance, [PackFileRecipe; 2]) {
    let release = neoforge_jar("libx", "7.0.4");
    serve(server, "/libx.jar", release).await;
    serve(server, "/root.jar", ROOT_JAR).await;
    let instance = Instance::new();
    std::fs::write(
        instance.jar("libx-dev.jar"),
        neoforge_jar("libx", "the developer's build"),
    )
    .expect("write");
    let requested = [
        addition(server, "/root.jar", "root-1.0.jar", ROOT_JAR, "ROOT"),
        addition(server, "/libx.jar", "libx-7.0.4.jar", release, "LIBX"),
    ];
    (instance, requested)
}

#[tokio::test]
async fn a_jar_already_in_mods_stands_in_for_a_dependency_when_the_request_allows_it() {
    let server = MockServer::start().await;
    let (instance, requested) = a_mod_needing_a_library_the_developer_built(&server).await;
    let mut recipe = recipe();

    let report =
        install_letting_stand_in(&instance, &mut recipe, &requested, &["mods/libx-7.0.4.jar"])
            .await
            .expect("the developer's build stands in for the library");

    assert_eq!(
        report.stood_in,
        [ephervex_pack::InstalledStandIn {
            planned: PathBuf::from("mods/libx-7.0.4.jar"),
            installed: PathBuf::from("mods/libx-dev.jar"),
            mod_ids: vec!["libx".to_owned()],
        }]
    );
    assert_eq!(report.installed, [PathBuf::from("mods/root-1.0.jar")]);
    assert_eq!(mod_jars(&instance), ["libx-dev.jar", "root-1.0.jar"]);
    let recorded = instance.store.read_recipe().expect("recipe");
    assert!(
        recorded
            .managed_project(ModProvider::Modrinth, "ROOT")
            .is_some()
    );
    assert!(
        recorded
            .managed_project(ModProvider::Modrinth, "LIBX")
            .is_none(),
        "the library that was not written is not recorded"
    );
}

#[tokio::test]
async fn a_dependency_a_jar_already_in_mods_provides_is_refused_unless_the_request_allows_it() {
    let server = MockServer::start().await;
    let (instance, requested) = a_mod_needing_a_library_the_developer_built(&server).await;
    let mut recipe = recipe();

    let error =
        install_letting_stand_in(&instance, &mut recipe, &requested, &["mods/root-1.0.jar"])
            .await
            .expect_err("only the files named may be stood in for");

    assert!(
        matches!(
            &error,
            PackError::DuplicateModId {
                could_stand_in: true,
                ..
            }
        ),
        "{error:?}"
    );
    assert_eq!(mod_jars(&instance), ["libx-dev.jar"]);
    assert!(!instance.store.has_recipe());
}

#[tokio::test]
async fn a_dependency_allowed_a_stand_in_is_installed_when_nothing_stands_in_for_it() {
    let server = MockServer::start().await;
    let (instance, requested) = a_mod_needing_a_library_the_developer_built(&server).await;
    std::fs::rename(
        instance.jar("libx-dev.jar"),
        instance.jar("libx-dev.jar.disabled"),
    )
    .expect("disable the build");
    let mut recipe = recipe();

    let report =
        install_letting_stand_in(&instance, &mut recipe, &requested, &["mods/libx-7.0.4.jar"])
            .await
            .expect("installs the library");

    assert!(report.stood_in.is_empty());
    assert_eq!(
        mod_jars(&instance),
        ["libx-7.0.4.jar", "libx-dev.jar.disabled", "root-1.0.jar"]
    );
}

#[tokio::test]
async fn a_jar_providing_only_some_of_a_dependencys_mod_ids_does_not_stand_in_for_it() {
    use std::io::Write as _;
    let mut bytes = Vec::new();
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut bytes));
    zip.start_file(
        "META-INF/neoforge.mods.toml",
        zip::write::SimpleFileOptions::default(),
    )
    .expect("entry");
    zip.write_all(b"[[mods]]\nmodId=\"libx\"\n[[mods]]\nmodId=\"libx_extra\"\n")
        .expect("write");
    zip.finish().expect("finish");
    let release: &'static [u8] = Box::leak(bytes.into_boxed_slice());

    let server = MockServer::start().await;
    serve(&server, "/libx.jar", release).await;
    let instance = Instance::new();
    std::fs::write(
        instance.jar("libx-dev.jar"),
        neoforge_jar("libx", "the developer's build"),
    )
    .expect("write");
    let mut recipe = recipe();

    let error = install_letting_stand_in(
        &instance,
        &mut recipe,
        &[addition(
            &server,
            "/libx.jar",
            "libx-7.0.4.jar",
            release,
            "LIBX",
        )],
        &["mods/libx-7.0.4.jar"],
    )
    .await
    .expect_err("libx_extra would be missing");

    assert!(
        matches!(
            &error,
            PackError::DuplicateModId {
                could_stand_in: false,
                ..
            }
        ),
        "{error:?}"
    );
    assert_eq!(mod_jars(&instance), ["libx-dev.jar"]);
}

#[tokio::test]
async fn a_dependency_that_may_only_be_stood_in_for_is_left_out_when_a_jar_does() {
    let server = MockServer::start().await;
    let (instance, requested) = a_mod_needing_a_library_the_developer_built(&server).await;
    let mut recipe = recipe();

    let report = install_with_stand_ins(
        &instance,
        &mut recipe,
        &requested,
        &["mods/libx-7.0.4.jar"],
        &["mods/libx-7.0.4.jar"],
    )
    .await
    .expect("the developer's build stands in for the library");

    assert_eq!(report.stood_in.len(), 1, "{report:?}");
    assert_eq!(mod_jars(&instance), ["libx-dev.jar", "root-1.0.jar"]);
}

#[tokio::test]
async fn a_dependency_that_may_only_be_stood_in_for_is_refused_when_nothing_does() {
    let server = MockServer::start().await;
    let (instance, requested) = a_mod_needing_a_library_the_developer_built(&server).await;
    std::fs::rename(
        instance.jar("libx-dev.jar"),
        instance.jar("libx-dev.jar.disabled"),
    )
    .expect("disable the build");
    let mut recipe = recipe();

    let error = install_with_stand_ins(
        &instance,
        &mut recipe,
        &requested,
        &["mods/libx-7.0.4.jar"],
        &["mods/libx-7.0.4.jar"],
    )
    .await
    .expect_err("the library may not be installed");

    assert!(
        matches!(
            &error,
            PackError::StandInUnavailable { incoming } if incoming == "mods/libx-7.0.4.jar"
        ),
        "{error:?}"
    );
    assert_eq!(mod_jars(&instance), ["libx-dev.jar.disabled"]);
    assert!(!instance.store.has_recipe());
}
