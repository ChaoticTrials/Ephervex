//! Changing and removing mods without leaving an instance half-mutated.
//!
//! Everything runs against `wiremock` on 127.0.0.1, so no test contacts a real
//! catalog or CDN.
//!
//! The behaviors under test are the ones that decide whether a user's instance
//! survives a failed operation: what is in `mods/` when a download dies partway,
//! what is left when the recipe cannot be written, and whether an upgrade whose
//! file name did not change can lose the mod entirely.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use ephervex_download::{
    CacheLayout, DownloadManager, DownloaderConfig, HttpConfig, OperationReporter, RetryPolicy,
};
use ephervex_model::{Hash, HashAlgorithm, LoaderKind, LoaderSelection, ModOrigin, ModProvider};
use ephervex_pack::{
    ManagedMutation, ManagedMutationReport, ManagedProject, PackError, PackFileRecipe, PackRecipe,
    PackSide, ProviderFileIdentity, SideRequirement, SourceStore, mutate_managed,
};
use sha1::{Digest as _, Sha1};
use sha2::Sha512;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ROOT_V1: &[u8] = b"a fixture jar standing in for version one of the chosen mod";
const ROOT_V2: &[u8] = b"a fixture jar standing in for version two of the chosen mod";
const LIBRARY: &[u8] = b"a fixture jar standing in for a required library";
const OTHER_LIBRARY: &[u8] = b"a fixture jar standing in for a second library";

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
    root: PathBuf,
    game_dir: PathBuf,
    store: SourceStore,
    cache: CacheLayout,
}

impl Instance {
    fn new() -> Self {
        let base = tempfile::tempdir().expect("temp dir");
        let root = base.path().to_path_buf();
        let game_dir = root.join("minecraft");
        let store = SourceStore::new(root.join(".launcher/source"));
        std::fs::create_dir_all(game_dir.join("mods")).expect("mods dir");
        std::fs::create_dir_all(store.dir()).expect("source dir");
        Self {
            cache: CacheLayout::new(root.join("cache")),
            _base: base,
            root,
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

    fn retired_dir(&self) -> PathBuf {
        self.root.join(".launcher/retired")
    }

    /// Every jar the game would actually load, sorted.
    fn jars(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.mods_dir())
            .expect("mods dir")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

fn recipe() -> PackRecipe {
    PackRecipe::for_manual_instance(
        "Repro",
        "1.20.1",
        LoaderSelection::exact(LoaderKind::NeoForge, "47.1.106"),
    )
}

/// One recipe entry, as the resolver hands it over.
fn entry(
    server: &MockServer,
    route: &str,
    name: &str,
    body: &[u8],
    project: &str,
    version: &str,
    origin: ModOrigin,
) -> PackFileRecipe {
    PackFileRecipe {
        logical_path: PathBuf::from("mods").join(name),
        hashes: vec![sha512_of(body), sha1_of(body)],
        downloads: vec![format!("{}{route}", server.uri())],
        size: Some(body.len() as u64),
        client: SideRequirement::Required,
        server: SideRequirement::Unknown,
        selected: None,
        origin,
        identity: Some(ProviderFileIdentity {
            provider: Some(ModProvider::Modrinth),
            project_id: Some(project.to_owned()),
            version_id: Some(version.to_owned()),
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

async fn mutate(
    instance: &Instance,
    recipe: &mut PackRecipe,
    upserts: &[PackFileRecipe],
    removals: &[ManagedProject],
) -> Result<ManagedMutationReport, PackError> {
    mutate_managed(ManagedMutation {
        recipe,
        upserts,
        removals,
        game_dir: &instance.game_dir,
        retired_dir: &instance.retired_dir(),
        store: &instance.store,
        manager: &manager(),
        cache: &instance.cache,
        reporter: &OperationReporter::silent("mutating-mods"),
        cancel: &CancellationToken::new(),
    })
    .await
}

/// An instance holding version one of a mod plus its library.
async fn installed(server: &MockServer, instance: &Instance) -> PackRecipe {
    serve(server, "/root-1.jar", ROOT_V1).await;
    serve(server, "/library.jar", LIBRARY).await;
    let mut recipe = recipe();
    let upserts = vec![
        entry(
            server,
            "/root-1.jar",
            "root-1.0.jar",
            ROOT_V1,
            "ROOTPROJ",
            "ROOTV1",
            ModOrigin::UserSelected,
        ),
        entry(
            server,
            "/library.jar",
            "library-2.0.jar",
            LIBRARY,
            "LIBPROJ",
            "LIBV1",
            ModOrigin::RequiredDependency,
        ),
    ];
    mutate(instance, &mut recipe, &upserts, &[])
        .await
        .expect("initial install");
    recipe
}

fn version_of(recipe: &PackRecipe, project: &str) -> Option<String> {
    recipe
        .managed_project(ModProvider::Modrinth, project)
        .and_then(|entry| entry.identity.as_ref())
        .and_then(|identity| identity.version_id.clone())
}

fn path_of(recipe: &PackRecipe, project: &str) -> Option<PathBuf> {
    recipe
        .managed_project(ModProvider::Modrinth, project)
        .map(|entry| entry.logical_path.clone())
}

// -- changing a version ---------------------------------------------------

#[tokio::test]
async fn an_upgrade_replaces_the_jar_the_record_and_the_hashes_together() {
    let server = MockServer::start().await;
    let instance = Instance::new();
    let mut recipe = installed(&server, &instance).await;
    serve(&server, "/root-2.jar", ROOT_V2).await;

    let report = mutate(
        &instance,
        &mut recipe,
        &[entry(
            &server,
            "/root-2.jar",
            "root-2.0.jar",
            ROOT_V2,
            "ROOTPROJ",
            "ROOTV2",
            ModOrigin::UserSelected,
        )],
        &[],
    )
    .await
    .expect("upgrade");

    assert_eq!(report.replaced.len(), 1);
    assert_eq!(
        report.replaced[0].previous_path,
        PathBuf::from("mods/root-1.0.jar")
    );
    assert_eq!(
        instance.jars(),
        vec!["library-2.0.jar", "root-2.0.jar"],
        "the old jar is gone and exactly one build of the mod is loadable"
    );

    // What a reopen reads, which is what "Create Server" would build from.
    let stored = instance.store.read_recipe().expect("recipe on disk");
    assert_eq!(version_of(&stored, "ROOTPROJ").as_deref(), Some("ROOTV2"));
    assert_eq!(
        path_of(&stored, "ROOTPROJ"),
        Some(PathBuf::from("mods/root-2.0.jar"))
    );
    let entry = stored
        .managed_project(ModProvider::Modrinth, "ROOTPROJ")
        .expect("recorded");
    assert_eq!(
        entry.hashes,
        vec![sha512_of(ROOT_V2), sha1_of(ROOT_V2)],
        "the replacement's own hashes, not the ones it replaced"
    );
    assert_eq!(entry.size, Some(ROOT_V2.len() as u64));
}

#[tokio::test]
async fn a_downgrade_is_the_same_operation_and_is_equally_exact() {
    let server = MockServer::start().await;
    let instance = Instance::new();
    let mut recipe = installed(&server, &instance).await;
    serve(&server, "/root-2.jar", ROOT_V2).await;

    let to_v2 = entry(
        &server,
        "/root-2.jar",
        "root-2.0.jar",
        ROOT_V2,
        "ROOTPROJ",
        "ROOTV2",
        ModOrigin::UserSelected,
    );
    mutate(&instance, &mut recipe, &[to_v2], &[])
        .await
        .expect("upgrade");

    let back_to_v1 = entry(
        &server,
        "/root-1.jar",
        "root-1.0.jar",
        ROOT_V1,
        "ROOTPROJ",
        "ROOTV1",
        ModOrigin::UserSelected,
    );
    mutate(&instance, &mut recipe, &[back_to_v1], &[])
        .await
        .expect("downgrade");

    assert_eq!(instance.jars(), vec!["library-2.0.jar", "root-1.0.jar"]);
    let stored = instance.store.read_recipe().expect("recipe");
    assert_eq!(version_of(&stored, "ROOTPROJ").as_deref(), Some("ROOTV1"));
}

#[tokio::test]
async fn a_replacement_that_keeps_the_same_file_name_still_ends_up_at_the_new_version() {
    // The case that quietly breaks a naive implementation: writing the new jar
    // first destroys the old one, and there is nothing left to restore.
    let server = MockServer::start().await;
    let instance = Instance::new();
    let mut recipe = installed(&server, &instance).await;
    serve(&server, "/root-2.jar", ROOT_V2).await;

    mutate(
        &instance,
        &mut recipe,
        &[entry(
            &server,
            "/root-2.jar",
            "root-1.0.jar",
            ROOT_V2,
            "ROOTPROJ",
            "ROOTV2",
            ModOrigin::UserSelected,
        )],
        &[],
    )
    .await
    .expect("in-place replacement");

    assert_eq!(instance.jars(), vec!["library-2.0.jar", "root-1.0.jar"]);
    assert_eq!(
        std::fs::read(instance.jar("root-1.0.jar")).expect("jar"),
        ROOT_V2,
        "the file kept its name and changed its contents"
    );
    let stored = instance.store.read_recipe().expect("recipe");
    assert_eq!(version_of(&stored, "ROOTPROJ").as_deref(), Some("ROOTV2"));
}

#[tokio::test]
async fn one_project_can_never_end_up_with_two_recorded_versions() {
    let server = MockServer::start().await;
    let instance = Instance::new();
    let mut recipe = installed(&server, &instance).await;
    serve(&server, "/root-2.jar", ROOT_V2).await;

    mutate(
        &instance,
        &mut recipe,
        &[entry(
            &server,
            "/root-2.jar",
            "root-2.0.jar",
            ROOT_V2,
            "ROOTPROJ",
            "ROOTV2",
            ModOrigin::UserSelected,
        )],
        &[],
    )
    .await
    .expect("upgrade");

    let stored = instance.store.read_recipe().expect("recipe");
    let entries: Vec<&PackFileRecipe> = stored
        .files
        .iter()
        .filter(|file| {
            file.identity
                .as_ref()
                .and_then(|identity| identity.project_id.as_deref())
                == Some("ROOTPROJ")
        })
        .collect();
    assert_eq!(entries.len(), 1, "one project, one entry: {entries:?}");
}

#[tokio::test]
async fn a_replacement_keeps_the_ownership_the_instance_recorded() {
    // An update must not promote a dependency into a root, which would make it
    // survive the removal of the mod that wanted it.
    let server = MockServer::start().await;
    let instance = Instance::new();
    let mut recipe = installed(&server, &instance).await;
    serve(&server, "/library-3.jar", OTHER_LIBRARY).await;

    mutate(
        &instance,
        &mut recipe,
        &[entry(
            &server,
            "/library-3.jar",
            "library-3.0.jar",
            OTHER_LIBRARY,
            "LIBPROJ",
            "LIBV2",
            // The caller happens to hand over a root's origin; the recorded one
            // is what survives.
            ModOrigin::UserSelected,
        )],
        &[],
    )
    .await
    .expect("dependency moved");

    let stored = instance.store.read_recipe().expect("recipe");
    let entry = stored
        .managed_project(ModProvider::Modrinth, "LIBPROJ")
        .expect("recorded");
    assert_eq!(entry.origin, ModOrigin::RequiredDependency);
}

#[tokio::test]
async fn a_failed_download_leaves_the_installed_version_exactly_as_it_was() {
    let server = MockServer::start().await;
    let instance = Instance::new();
    let mut recipe = installed(&server, &instance).await;
    // No route is served for the replacement, so fetching it fails.

    let before = instance.store.read_recipe().expect("recipe");
    let error = mutate(
        &instance,
        &mut recipe,
        &[entry(
            &server,
            "/never-served.jar",
            "root-2.0.jar",
            ROOT_V2,
            "ROOTPROJ",
            "ROOTV2",
            ModOrigin::UserSelected,
        )],
        &[],
    )
    .await
    .expect_err("the download must fail");
    assert!(matches!(error, PackError::DownloadFailed { .. }));

    assert_eq!(
        instance.jars(),
        vec!["library-2.0.jar", "root-1.0.jar"],
        "the old jar was never touched"
    );
    assert_eq!(
        std::fs::read(instance.jar("root-1.0.jar")).expect("jar"),
        ROOT_V1
    );
    assert_eq!(
        instance.store.read_recipe().expect("recipe"),
        before,
        "and the recipe still describes it"
    );
}

#[tokio::test]
async fn a_failed_publication_restores_the_replaced_jar_and_leaves_no_duplicate() {
    let server = MockServer::start().await;
    let instance = Instance::new();
    let mut recipe = installed(&server, &instance).await;
    serve(&server, "/root-2.jar", ROOT_V2).await;

    // A directory where the recipe file belongs is the simplest way to make the
    // final write fail after every jar is already in place.
    let recipe_path = instance.store.dir().join(ephervex_pack::RECIPE_FILE);
    std::fs::remove_file(&recipe_path).expect("remove the recipe file");
    std::fs::create_dir(&recipe_path).expect("occupy its path");

    let error = mutate(
        &instance,
        &mut recipe,
        &[entry(
            &server,
            "/root-2.jar",
            "root-2.0.jar",
            ROOT_V2,
            "ROOTPROJ",
            "ROOTV2",
            ModOrigin::UserSelected,
        )],
        &[],
    )
    .await
    .expect_err("publication must fail");
    assert!(matches!(error, PackError::Unwritable { .. }));

    assert_eq!(
        instance.jars(),
        vec!["library-2.0.jar", "root-1.0.jar"],
        "the replacement was withdrawn and the old jar came back"
    );
    assert_eq!(
        std::fs::read(instance.jar("root-1.0.jar")).expect("jar"),
        ROOT_V1
    );
}

#[tokio::test]
async fn a_failed_publication_of_an_in_place_replacement_restores_the_old_contents() {
    // The same failure where the new jar reuses the old one's name. Restoring
    // has to happen after the new file is withdrawn or it lands on an occupied
    // path.
    let server = MockServer::start().await;
    let instance = Instance::new();
    let mut recipe = installed(&server, &instance).await;
    serve(&server, "/root-2.jar", ROOT_V2).await;

    let recipe_path = instance.store.dir().join(ephervex_pack::RECIPE_FILE);
    std::fs::remove_file(&recipe_path).expect("remove");
    std::fs::create_dir(&recipe_path).expect("occupy");

    mutate(
        &instance,
        &mut recipe,
        &[entry(
            &server,
            "/root-2.jar",
            "root-1.0.jar",
            ROOT_V2,
            "ROOTPROJ",
            "ROOTV2",
            ModOrigin::UserSelected,
        )],
        &[],
    )
    .await
    .expect_err("publication must fail");

    assert_eq!(instance.jars(), vec!["library-2.0.jar", "root-1.0.jar"]);
    assert_eq!(
        std::fs::read(instance.jar("root-1.0.jar")).expect("jar"),
        ROOT_V1,
        "the file has its original contents back"
    );
}

#[tokio::test]
async fn requesting_the_installed_version_writes_nothing() {
    let server = MockServer::start().await;
    let instance = Instance::new();
    let mut recipe = installed(&server, &instance).await;
    let before = instance.store.read_recipe().expect("recipe");

    let report = mutate(
        &instance,
        &mut recipe,
        &[entry(
            &server,
            "/root-1.jar",
            "root-1.0.jar",
            ROOT_V1,
            "ROOTPROJ",
            "ROOTV1",
            ModOrigin::UserSelected,
        )],
        &[],
    )
    .await
    .expect("no-op");

    assert!(!report.changed_anything());
    assert_eq!(report.unchanged.len(), 1);
    assert_eq!(instance.store.read_recipe().expect("recipe"), before);
}

#[tokio::test]
async fn the_recorded_version_is_put_back_when_its_jar_has_gone_missing() {
    let server = MockServer::start().await;
    let instance = Instance::new();
    let mut recipe = installed(&server, &instance).await;
    std::fs::remove_file(instance.jar("root-1.0.jar")).expect("delete the jar");

    let report = mutate(
        &instance,
        &mut recipe,
        &[entry(
            &server,
            "/root-1.jar",
            "root-1.0.jar",
            ROOT_V1,
            "ROOTPROJ",
            "ROOTV1",
            ModOrigin::UserSelected,
        )],
        &[],
    )
    .await
    .expect("repair");

    assert_eq!(report.repaired.len(), 1);
    assert!(report.changed_anything(), "putting a mod back is a change");
    assert!(instance.jar("root-1.0.jar").is_file());
    assert_eq!(version_of(&recipe, "ROOTPROJ").as_deref(), Some("ROOTV1"));
}

// -- removal ---------------------------------------------------------------

#[tokio::test]
async fn removing_a_mod_takes_its_jar_and_its_record_and_nothing_else() {
    let server = MockServer::start().await;
    let instance = Instance::new();
    let mut recipe = installed(&server, &instance).await;

    let report = mutate(
        &instance,
        &mut recipe,
        &[],
        &[ManagedProject::new(ModProvider::Modrinth, "ROOTPROJ")],
    )
    .await
    .expect("removal");

    assert_eq!(report.removed, vec![PathBuf::from("mods/root-1.0.jar")]);
    assert_eq!(instance.jars(), vec!["library-2.0.jar"]);
    let stored = instance.store.read_recipe().expect("recipe");
    assert_eq!(path_of(&stored, "ROOTPROJ"), None);
    assert!(
        path_of(&stored, "LIBPROJ").is_some(),
        "a removal the caller did not ask for is not performed"
    );
}

#[tokio::test]
async fn a_root_and_the_dependency_nothing_else_needs_go_together() {
    let server = MockServer::start().await;
    let instance = Instance::new();
    let mut recipe = installed(&server, &instance).await;

    mutate(
        &instance,
        &mut recipe,
        &[],
        &[
            ManagedProject::new(ModProvider::Modrinth, "ROOTPROJ"),
            ManagedProject::new(ModProvider::Modrinth, "LIBPROJ"),
        ],
    )
    .await
    .expect("removal");

    assert!(instance.jars().is_empty());
    assert!(
        instance
            .store
            .read_recipe()
            .expect("recipe")
            .files
            .is_empty(),
        "the desired state is empty, so a server built from it would be too"
    );
}

#[tokio::test]
async fn removing_a_mod_whose_jar_is_already_gone_still_drops_the_record() {
    let server = MockServer::start().await;
    let instance = Instance::new();
    let mut recipe = installed(&server, &instance).await;
    std::fs::remove_file(instance.jar("root-1.0.jar")).expect("delete");

    mutate(
        &instance,
        &mut recipe,
        &[],
        &[ManagedProject::new(ModProvider::Modrinth, "ROOTPROJ")],
    )
    .await
    .expect("removal");

    let stored = instance.store.read_recipe().expect("recipe");
    assert_eq!(path_of(&stored, "ROOTPROJ"), None);
}

#[tokio::test]
async fn a_removal_naming_something_ephervex_did_not_install_is_refused() {
    let server = MockServer::start().await;
    let instance = Instance::new();
    let mut recipe = installed(&server, &instance).await;
    let before = instance.store.read_recipe().expect("recipe");

    let error = mutate(
        &instance,
        &mut recipe,
        &[],
        &[ManagedProject::new(ModProvider::Modrinth, "NEVERSEEN")],
    )
    .await
    .expect_err("must refuse");
    assert!(matches!(error, PackError::ManagedIdentityMissing { .. }));
    assert_eq!(instance.store.read_recipe().expect("recipe"), before);
}

#[tokio::test]
async fn a_jar_the_user_dropped_in_is_never_moved_or_deleted() {
    let server = MockServer::start().await;
    let instance = Instance::new();
    let mut recipe = installed(&server, &instance).await;
    let local = instance.jar("local-debug-build.jar");
    std::fs::write(&local, b"the build this instance exists to test").expect("write");

    mutate(
        &instance,
        &mut recipe,
        &[],
        &[ManagedProject::new(ModProvider::Modrinth, "ROOTPROJ")],
    )
    .await
    .expect("removal");

    assert!(local.is_file(), "an unrecorded jar is not ours to touch");
    assert_eq!(
        std::fs::read(&local).expect("jar"),
        b"the build this instance exists to test"
    );
}

#[tokio::test]
async fn a_new_mod_may_not_land_on_a_jar_ephervex_did_not_install() {
    let server = MockServer::start().await;
    serve(&server, "/root-2.jar", ROOT_V2).await;
    let instance = Instance::new();
    let mut recipe = installed(&server, &instance).await;
    std::fs::write(instance.jar("newcomer.jar"), b"somebody else's file").expect("write");

    let error = mutate(
        &instance,
        &mut recipe,
        &[entry(
            &server,
            "/root-2.jar",
            "newcomer.jar",
            ROOT_V2,
            "OTHERPROJ",
            "OTHERV1",
            ModOrigin::UserSelected,
        )],
        &[],
    )
    .await
    .expect_err("must refuse");
    assert!(matches!(error, PackError::DestinationOccupied { .. }));
    assert_eq!(
        std::fs::read(instance.jar("newcomer.jar")).expect("jar"),
        b"somebody else's file"
    );
}

#[tokio::test]
async fn a_mod_may_take_the_path_the_same_mutation_frees() {
    // Removing one mod and installing another at its exact path is one coherent
    // desired state, and refusing it would make an ordinary rename impossible.
    let server = MockServer::start().await;
    serve(&server, "/root-2.jar", ROOT_V2).await;
    let instance = Instance::new();
    let mut recipe = installed(&server, &instance).await;

    mutate(
        &instance,
        &mut recipe,
        &[entry(
            &server,
            "/root-2.jar",
            "library-2.0.jar",
            ROOT_V2,
            "OTHERPROJ",
            "OTHERV1",
            ModOrigin::UserSelected,
        )],
        &[ManagedProject::new(ModProvider::Modrinth, "LIBPROJ")],
    )
    .await
    .expect("the swap is one operation");

    assert_eq!(
        std::fs::read(instance.jar("library-2.0.jar")).expect("jar"),
        ROOT_V2
    );
    let stored = instance.store.read_recipe().expect("recipe");
    assert_eq!(path_of(&stored, "LIBPROJ"), None);
    assert_eq!(
        path_of(&stored, "OTHERPROJ"),
        Some(PathBuf::from("mods/library-2.0.jar"))
    );
}

// -- what is left behind ---------------------------------------------------

#[tokio::test]
async fn a_finished_mutation_leaves_nothing_in_the_retired_directory() {
    let server = MockServer::start().await;
    let instance = Instance::new();
    let mut recipe = installed(&server, &instance).await;
    serve(&server, "/root-2.jar", ROOT_V2).await;

    mutate(
        &instance,
        &mut recipe,
        &[entry(
            &server,
            "/root-2.jar",
            "root-2.0.jar",
            ROOT_V2,
            "ROOTPROJ",
            "ROOTV2",
            ModOrigin::UserSelected,
        )],
        &[],
    )
    .await
    .expect("upgrade");

    let left: Vec<PathBuf> = std::fs::read_dir(instance.retired_dir())
        .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
        .unwrap_or_default();
    assert!(left.is_empty(), "scratch left behind: {left:?}");
}

#[tokio::test]
async fn a_mutation_never_writes_outside_the_instance() {
    // Retirement moves files into launcher-owned scratch inside the same
    // instance, which is what makes the move atomic and the undo possible.
    let server = MockServer::start().await;
    let instance = Instance::new();
    let mut recipe = installed(&server, &instance).await;

    mutate(
        &instance,
        &mut recipe,
        &[],
        &[ManagedProject::new(ModProvider::Modrinth, "ROOTPROJ")],
    )
    .await
    .expect("removal");

    assert!(
        instance.retired_dir().starts_with(&instance.root),
        "the retired directory is inside the instance"
    );
    assert!(
        instance
            .retired_dir()
            .starts_with(instance.root.join(".launcher")),
        "and inside launcher-owned metadata the game never reads"
    );
}

#[tokio::test]
async fn the_mutated_state_is_what_a_server_would_be_built_from() {
    let server = MockServer::start().await;
    let instance = Instance::new();
    let mut recipe = installed(&server, &instance).await;
    serve(&server, "/root-2.jar", ROOT_V2).await;

    mutate(
        &instance,
        &mut recipe,
        &[entry(
            &server,
            "/root-2.jar",
            "root-2.0.jar",
            ROOT_V2,
            "ROOTPROJ",
            "ROOTV2",
            ModOrigin::UserSelected,
        )],
        &[ManagedProject::new(ModProvider::Modrinth, "LIBPROJ")],
    )
    .await
    .expect("mutation");

    let stored = instance.store.read_recipe().expect("recipe");
    let client: Vec<String> = stored
        .plan(PackSide::Client, &ephervex_pack::OptionalSelection::none())
        .files
        .iter()
        .map(|file| file.display_name())
        .collect();
    assert_eq!(client, vec!["mods/root-2.0.jar".to_owned()]);
}

#[tokio::test]
async fn planning_the_result_agrees_with_carrying_it_out() {
    let server = MockServer::start().await;
    let instance = Instance::new();
    let mut recipe = installed(&server, &instance).await;
    serve(&server, "/root-2.jar", ROOT_V2).await;

    let upserts = vec![entry(
        &server,
        "/root-2.jar",
        "root-2.0.jar",
        ROOT_V2,
        "ROOTPROJ",
        "ROOTV2",
        ModOrigin::UserSelected,
    )];
    let removals = vec![ManagedProject::new(ModProvider::Modrinth, "LIBPROJ")];

    let predicted = ephervex_pack::projected(&recipe, &upserts, &removals);
    mutate(&instance, &mut recipe, &upserts, &removals)
        .await
        .expect("mutation");

    let names = |recipe: &PackRecipe| -> Vec<PathBuf> {
        recipe
            .recorded_mods()
            .into_iter()
            .map(|item| item.logical_path)
            .collect()
    };
    assert_eq!(
        names(&predicted),
        names(&recipe),
        "a preview that disagrees with the operation is worse than none"
    );
}
