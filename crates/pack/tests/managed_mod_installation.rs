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

use std::path::{Path, PathBuf};

use ephervex_download::{
    CacheLayout, DownloadManager, DownloaderConfig, HttpConfig, OperationReporter, RetryPolicy,
};
use ephervex_model::{Hash, HashAlgorithm, LoaderKind, LoaderSelection, ModProvider};
use ephervex_pack::{
    ManagedModInstall, ModOrigin, PackError, PackFileRecipe, PackRecipe, PackSide,
    ProviderFileIdentity, SideRequirement, SourceStore, install_managed,
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
    _base: TempDir,
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
            _base: base,
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
    install_managed(ManagedModInstall {
        recipe,
        additions,
        game_dir: &instance.game_dir,
        store: &instance.store,
        manager: &manager(),
        cache: &instance.cache,
        reporter: &OperationReporter::silent("installing-mods"),
        cancel: &CancellationToken::new(),
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

    read_only(instance.store.dir());
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

    // A second instance sharing one cache — bisecting a bug looks exactly like
    // this — pays no network cost.
    let second = Instance {
        _base: tempfile::tempdir().expect("temp dir"),
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
fn read_only(dir: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    let mut permissions = std::fs::metadata(dir).expect("metadata").permissions();
    permissions.set_mode(0o555);
    std::fs::set_permissions(dir, permissions).expect("set permissions");
}

fn writable(dir: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    let mut permissions = std::fs::metadata(dir).expect("metadata").permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(dir, permissions).expect("set permissions");
}
