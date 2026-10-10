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

use std::path::{Path, PathBuf};

use ephervex_download::{
    CacheLayout, DownloadManager, DownloaderConfig, HttpConfig, OperationReporter, RetryPolicy,
};
use ephervex_model::{Hash, HashAlgorithm, LoaderKind, LoaderSelection, ModOrigin, ModProvider};
use ephervex_pack::{
    ArtifactFetch, ManagedMutation, ManagedMutationReport, ManagedProject, OpenArtifacts,
    PackError, PackFileRecipe, PackRecipe, PackSide, ProviderFileIdentity, SideRequirement,
    SourceStore, mutate_managed,
};
use sha1::{Digest as _, Sha1};
use sha2::Sha512;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ROOT_V1: &[u8] = b"a fixture jar standing in for version one of the chosen mod";
const ROOT_V2: &[u8] = b"a fixture jar standing in for version two of the chosen mod";
const ROOT_V3: &[u8] = b"a fixture jar standing in for version three of the chosen mod";
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
        original_identity: None,
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

async fn mutate(
    instance: &Instance,
    recipe: &mut PackRecipe,
    upserts: &[PackFileRecipe],
    removals: &[ManagedProject],
) -> Result<ManagedMutationReport, PackError> {
    mutate_on(PackSide::Client, instance, recipe, upserts, removals).await
}

async fn mutate_on(
    side: PackSide,
    instance: &Instance,
    recipe: &mut PackRecipe,
    upserts: &[PackFileRecipe],
    removals: &[ManagedProject],
) -> Result<ManagedMutationReport, PackError> {
    mutate_managed(ManagedMutation {
        recipe,
        upserts,
        removals,
        side,
        game_dir: &instance.game_dir,
        retired_dir: &instance.retired_dir(),
        store: &instance.store,
        fetch: ArtifactFetch {
            manager: &manager(),
            cache: &instance.cache,
            credentials: &OpenArtifacts,
            reporter: &OperationReporter::silent("mutating-mods"),
            cancel: &CancellationToken::new(),
        },
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

fn original_version_of(recipe: &PackRecipe, project: &str) -> Option<String> {
    recipe
        .managed_project(ModProvider::Modrinth, project)
        .and_then(|entry| entry.original_identity.as_ref())
        .and_then(|identity| identity.version_id.clone())
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

// -- remembering the pack's original version -------------------------------

#[tokio::test]
async fn a_pack_owned_mods_first_version_change_records_what_the_pack_declared() {
    let server = MockServer::start().await;
    let instance = Instance::new();
    let mut recipe = installed(&server, &instance).await;
    // The library is `RequiredDependency` in `installed()`; give it a pack
    // origin here to exercise the case this test is about.
    for file in &mut recipe.files {
        if file.logical_path == Path::new("mods/root-1.0.jar") {
            file.origin = ModOrigin::PackDeclared;
        }
    }
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
    .expect("version change");

    assert_eq!(
        original_version_of(&recipe, "ROOTPROJ").as_deref(),
        Some("ROOTV1"),
        "the pack's own release is preserved the first time this mod moves away from it"
    );
}

#[tokio::test]
async fn a_second_version_change_does_not_overwrite_the_remembered_original() {
    let server = MockServer::start().await;
    let instance = Instance::new();
    let mut recipe = installed(&server, &instance).await;
    for file in &mut recipe.files {
        if file.logical_path == Path::new("mods/root-1.0.jar") {
            file.origin = ModOrigin::PackDeclared;
        }
    }
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
    .expect("first version change");

    serve(&server, "/root-3.jar", ROOT_V3).await;
    mutate(
        &instance,
        &mut recipe,
        &[entry(
            &server,
            "/root-3.jar",
            "root-3.0.jar",
            ROOT_V3,
            "ROOTPROJ",
            "ROOTV3",
            ModOrigin::UserSelected,
        )],
        &[],
    )
    .await
    .expect("second version change");

    assert_eq!(version_of(&recipe, "ROOTPROJ").as_deref(), Some("ROOTV3"));
    assert_eq!(
        original_version_of(&recipe, "ROOTPROJ").as_deref(),
        Some("ROOTV1"),
        "the original is set once and never overwritten by a later move"
    );
}

#[tokio::test]
async fn a_user_selected_mods_version_change_never_records_an_original() {
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
    .expect("version change");

    assert_eq!(original_version_of(&recipe, "ROOTPROJ"), None);
}

// -- mods the user turned off ---------------------------------------------

#[tokio::test]
async fn a_mod_turned_off_stays_off_when_its_version_changes() {
    let server = MockServer::start().await;
    let instance = Instance::new();
    let mut recipe = installed(&server, &instance).await;
    std::fs::rename(
        instance.jar("root-1.0.jar"),
        instance.jar("root-1.0.jar.disabled"),
    )
    .expect("turn the mod off");
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
        instance.jars(),
        vec!["library-2.0.jar", "root-2.0.jar.disabled"],
        "the old jar is retired and the new build arrives just as turned off"
    );
    let stored = instance.store.read_recipe().expect("recipe on disk");
    assert_eq!(version_of(&stored, "ROOTPROJ").as_deref(), Some("ROOTV2"));
    assert_eq!(
        path_of(&stored, "ROOTPROJ"),
        Some(PathBuf::from("mods/root-2.0.jar")),
        "the recipe records the enabled name whichever way round the file is"
    );
}

#[tokio::test]
async fn requesting_the_installed_version_of_a_disabled_mod_leaves_it_off() {
    let server = MockServer::start().await;
    let instance = Instance::new();
    let mut recipe = installed(&server, &instance).await;
    std::fs::rename(
        instance.jar("root-1.0.jar"),
        instance.jar("root-1.0.jar.disabled"),
    )
    .expect("turn the mod off");
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

    assert!(
        !report.changed_anything(),
        "a mod that is present but turned off is not a jar to put back"
    );
    assert_eq!(report.unchanged.len(), 1);
    assert_eq!(
        instance.jars(),
        vec!["library-2.0.jar", "root-1.0.jar.disabled"]
    );
    assert_eq!(instance.store.read_recipe().expect("recipe"), before);
}

#[tokio::test]
async fn removing_a_disabled_mod_takes_its_file_with_it() {
    let server = MockServer::start().await;
    let instance = Instance::new();
    let mut recipe = installed(&server, &instance).await;
    std::fs::rename(
        instance.jar("root-1.0.jar"),
        instance.jar("root-1.0.jar.disabled"),
    )
    .expect("turn the mod off");

    let report = mutate(
        &instance,
        &mut recipe,
        &[],
        &[ManagedProject::new(ModProvider::Modrinth, "ROOTPROJ")],
    )
    .await
    .expect("removal");

    assert_eq!(report.removed, vec![PathBuf::from("mods/root-1.0.jar")]);
    assert_eq!(
        instance.jars(),
        vec!["library-2.0.jar"],
        "no orphan is left for the mod list to show as somebody else's jar"
    );
    let stored = instance.store.read_recipe().expect("recipe");
    assert_eq!(path_of(&stored, "ROOTPROJ"), None);
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

    let predicted = ephervex_pack::projected(&recipe, &upserts, &removals, PackSide::Client);
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

#[tokio::test]
async fn a_removal_may_not_name_a_jar_that_was_added_by_hand() {
    // A hand-placed jar a catalog recognized carries the same provider and
    // project id an installed mod does, so it answers a lookup by project
    // exactly as one. Origin is what says whose file it is.
    let server = MockServer::start().await;
    let instance = Instance::new();
    let mut recipe = recipe();
    let adopted = entry(
        &server,
        "/adopted.jar",
        "from-a-forum-post.jar",
        ROOT_V1,
        "HANDPROJ",
        "v1",
        ModOrigin::HandPlaced,
    );
    recipe.record_managed(adopted).expect("record");
    std::fs::write(instance.jar("from-a-forum-post.jar"), ROOT_V1).expect("jar");

    let error = mutate(
        &instance,
        &mut recipe,
        &[],
        &[ManagedProject::new(ModProvider::Modrinth, "HANDPROJ")],
    )
    .await
    .expect_err("a provider removal must not reach the user's own jar");

    assert!(
        matches!(error, PackError::HandPlacedNotMutable { .. }),
        "unexpected refusal: {error:?}"
    );
    assert_eq!(instance.jars(), vec!["from-a-forum-post.jar".to_owned()]);
}

#[tokio::test]
async fn installing_a_project_may_not_replace_a_jar_that_was_added_by_hand() {
    // The same hole from the other side: without the origin check this
    // retires the user's jar as though it were a previous release.
    let server = MockServer::start().await;
    serve(&server, "/v2.jar", ROOT_V2).await;
    let instance = Instance::new();
    let mut recipe = recipe();
    let adopted = entry(
        &server,
        "/adopted.jar",
        "from-a-forum-post.jar",
        ROOT_V1,
        "HANDPROJ",
        "v1",
        ModOrigin::HandPlaced,
    );
    recipe.record_managed(adopted).expect("record");
    std::fs::write(instance.jar("from-a-forum-post.jar"), ROOT_V1).expect("jar");

    let newer = entry(
        &server,
        "/v2.jar",
        "the-same-mod-2.0.jar",
        ROOT_V2,
        "HANDPROJ",
        "v2",
        ModOrigin::UserSelected,
    );
    let error = mutate(&instance, &mut recipe, &[newer], &[])
        .await
        .expect_err("a provider install must not retire the user's own jar");

    assert!(
        matches!(error, PackError::HandPlacedNotMutable { .. }),
        "unexpected refusal: {error:?}"
    );
    assert_eq!(
        instance.jars(),
        vec!["from-a-forum-post.jar".to_owned()],
        "the user's jar is untouched and the new one was never written"
    );
}

fn root_v1(server: &MockServer, name: &str) -> PackFileRecipe {
    entry(
        server,
        "/root-1.jar",
        name,
        ROOT_V1,
        "ROOTPROJ",
        "ROOTV1",
        ModOrigin::UserSelected,
    )
}

#[tokio::test]
async fn a_request_at_another_path_for_the_recorded_version_leaves_one_copy() {
    let server = MockServer::start().await;
    let instance = Instance::new();
    let mut recipe = installed(&server, &instance).await;
    let before = instance.store.read_recipe().expect("recipe");
    let jars = instance.jars();

    let report = mutate(
        &instance,
        &mut recipe,
        &[root_v1(&server, "root-renamed.jar")],
        &[],
    )
    .await
    .expect("request at another path");

    assert_eq!(report.unchanged, [Path::new("mods/root-1.0.jar")]);
    assert!(!report.changed_anything());
    assert_eq!(instance.jars(), jars, "no second copy appears");
    assert_eq!(instance.store.read_recipe().expect("recipe"), before);
}

#[tokio::test]
async fn a_missing_recorded_jar_is_put_back_where_the_recipe_records_it() {
    let server = MockServer::start().await;
    let instance = Instance::new();
    let mut recipe = installed(&server, &instance).await;
    std::fs::remove_file(instance.jar("root-1.0.jar")).expect("delete the jar");

    let report = mutate(
        &instance,
        &mut recipe,
        &[root_v1(&server, "root-renamed.jar")],
        &[],
    )
    .await
    .expect("repair at the recorded path");

    assert_eq!(report.repaired, [Path::new("mods/root-1.0.jar")]);
    assert!(instance.jar("root-1.0.jar").is_file());
    assert!(!instance.jar("root-renamed.jar").exists());
    assert_eq!(
        path_of(&recipe, "ROOTPROJ"),
        Some(PathBuf::from("mods/root-1.0.jar"))
    );
}

#[tokio::test]
async fn planning_a_path_change_at_the_recorded_version_agrees_with_carrying_it_out() {
    let server = MockServer::start().await;
    let instance = Instance::new();
    let mut recipe = installed(&server, &instance).await;
    let upserts = vec![root_v1(&server, "root-renamed.jar")];

    let predicted = ephervex_pack::projected(&recipe, &upserts, &[], PackSide::Client);
    mutate(&instance, &mut recipe, &upserts, &[])
        .await
        .expect("mutation");

    assert_eq!(predicted, recipe);
}

#[tokio::test]
async fn a_recorded_jar_with_other_bytes_is_left_alone_and_reported() {
    let server = MockServer::start().await;
    let instance = Instance::new();
    let mut recipe = installed(&server, &instance).await;
    let before = instance.store.read_recipe().expect("recipe");
    std::fs::write(instance.jar("root-1.0.jar"), b"the developer's own build").expect("modify");

    let report = mutate(
        &instance,
        &mut recipe,
        &[root_v1(&server, "root-1.0.jar")],
        &[],
    )
    .await
    .expect("request the recorded version");

    assert_eq!(report.modified, [Path::new("mods/root-1.0.jar")]);
    assert!(report.unchanged.is_empty());
    assert!(!report.changed_anything());
    assert_eq!(
        std::fs::read(instance.jar("root-1.0.jar")).expect("read"),
        b"the developer's own build"
    );
    assert_eq!(instance.store.read_recipe().expect("recipe"), before);
}

#[tokio::test]
async fn a_repair_restores_the_recorded_bytes_even_when_the_request_names_another_file() {
    let server = MockServer::start().await;
    let instance = Instance::new();
    let mut recipe = installed(&server, &instance).await;
    let before = instance.store.read_recipe().expect("recipe");
    serve(&server, "/root-other.jar", ROOT_V2).await;
    std::fs::remove_file(instance.jar("root-1.0.jar")).expect("delete the jar");
    // The same project and version, but a different file of it.
    let other_file = entry(
        &server,
        "/root-other.jar",
        "root-other-file.jar",
        ROOT_V2,
        "ROOTPROJ",
        "ROOTV1",
        ModOrigin::UserSelected,
    );

    let report = mutate(&instance, &mut recipe, &[other_file], &[])
        .await
        .expect("repair");

    assert_eq!(report.repaired, [Path::new("mods/root-1.0.jar")]);
    assert_eq!(
        std::fs::read(instance.jar("root-1.0.jar")).expect("read"),
        ROOT_V1
    );
    assert!(!instance.jar("root-other-file.jar").exists());
    assert_eq!(instance.store.read_recipe().expect("recipe"), before);
}

#[tokio::test]
async fn a_recorded_jar_with_no_address_is_restored_from_the_requests_address() {
    let server = MockServer::start().await;
    let instance = Instance::new();
    let mut recipe = installed(&server, &instance).await;
    let root = recipe
        .files
        .iter_mut()
        .find(|file| file.logical_path == Path::new("mods/root-1.0.jar"))
        .expect("recorded root");
    root.downloads.clear();
    std::fs::remove_file(instance.jar("root-1.0.jar")).expect("delete the jar");
    std::fs::remove_dir_all(instance.root.join("cache")).expect("empty the cache");

    let report = mutate(
        &instance,
        &mut recipe,
        &[root_v1(&server, "root-1.0.jar")],
        &[],
    )
    .await
    .expect("repair");

    assert_eq!(report.repaired, [Path::new("mods/root-1.0.jar")]);
    assert_eq!(
        std::fs::read(instance.jar("root-1.0.jar")).expect("read"),
        ROOT_V1
    );
}

// -- destinations ---------------------------------------------------------

/// A directory outside the instance that nothing may be written into.
fn outside(instance: &Instance) -> PathBuf {
    let dir = instance.root.join("outside");
    std::fs::create_dir_all(&dir).expect("outside dir");
    dir
}

fn dir_is_empty(dir: &Path) -> bool {
    std::fs::read_dir(dir).expect("read dir").next().is_none()
}

#[cfg(unix)]
#[tokio::test]
async fn a_new_mod_is_never_written_through_a_dangling_link() {
    let server = MockServer::start().await;
    serve(&server, "/root-1.jar", ROOT_V1).await;
    let instance = Instance::new();
    let target = outside(&instance).join("created.jar");
    std::os::unix::fs::symlink(&target, instance.jar("root-1.0.jar")).expect("link");
    let mut recipe = recipe();

    let error = mutate(
        &instance,
        &mut recipe,
        &[root_v1(&server, "root-1.0.jar")],
        &[],
    )
    .await
    .expect_err("must refuse");

    assert!(matches!(error, PackError::ManagedDestinationLinked { .. }));
    assert!(!target.exists());
    assert!(!instance.store.has_recipe());
}

#[cfg(unix)]
#[tokio::test]
async fn a_linked_mods_directory_is_refused_for_installs() {
    let server = MockServer::start().await;
    serve(&server, "/root-1.jar", ROOT_V1).await;
    let instance = Instance::new();
    let outside = outside(&instance);
    std::fs::remove_dir(instance.mods_dir()).expect("remove mods");
    std::os::unix::fs::symlink(&outside, instance.mods_dir()).expect("link");
    let mut recipe = recipe();

    let error = mutate(
        &instance,
        &mut recipe,
        &[root_v1(&server, "root-1.0.jar")],
        &[],
    )
    .await
    .expect_err("must refuse");

    assert!(matches!(error, PackError::ManagedDestinationLinked { .. }));
    assert!(dir_is_empty(&outside));
}

#[cfg(unix)]
#[tokio::test]
async fn a_replacement_is_never_written_through_a_dangling_link() {
    let server = MockServer::start().await;
    let instance = Instance::new();
    let mut recipe = installed(&server, &instance).await;
    serve(&server, "/root-2.jar", ROOT_V2).await;
    let target = outside(&instance).join("created.jar");
    std::os::unix::fs::symlink(&target, instance.jar("root-2.0.jar")).expect("link");

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
    .expect_err("must refuse");

    assert!(matches!(error, PackError::ManagedDestinationLinked { .. }));
    assert!(!target.exists());
    assert_eq!(
        std::fs::read(instance.jar("root-1.0.jar")).expect("old jar"),
        ROOT_V1,
        "the installed version is untouched"
    );
    assert_eq!(version_of(&recipe, "ROOTPROJ").as_deref(), Some("ROOTV1"));
}

#[cfg(unix)]
#[tokio::test]
async fn a_repair_never_writes_through_a_dangling_link_at_the_recorded_path() {
    let server = MockServer::start().await;
    let instance = Instance::new();
    let mut recipe = installed(&server, &instance).await;
    std::fs::remove_file(instance.jar("root-1.0.jar")).expect("delete the jar");
    let target = outside(&instance).join("created.jar");
    std::os::unix::fs::symlink(&target, instance.jar("root-1.0.jar")).expect("link");

    let report = mutate(
        &instance,
        &mut recipe,
        &[root_v1(&server, "root-1.0.jar")],
        &[],
    )
    .await
    .expect("a link is reported, not repaired");

    assert_eq!(report.modified, [Path::new("mods/root-1.0.jar")]);
    assert!(report.repaired.is_empty());
    assert!(!target.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn a_repair_beneath_a_linked_directory_is_refused_for_mutations() {
    let server = MockServer::start().await;
    let instance = Instance::new();
    let mut recipe = installed(&server, &instance).await;
    let outside = outside(&instance);
    std::fs::remove_dir_all(instance.mods_dir()).expect("remove mods");
    std::os::unix::fs::symlink(&outside, instance.mods_dir()).expect("link");

    let error = mutate(
        &instance,
        &mut recipe,
        &[root_v1(&server, "root-1.0.jar")],
        &[],
    )
    .await
    .expect_err("must refuse");

    assert!(matches!(error, PackError::ManagedDestinationLinked { .. }));
    assert!(dir_is_empty(&outside));
}

#[cfg(unix)]
#[tokio::test]
async fn a_removal_never_moves_a_file_out_of_a_linked_directory() {
    let server = MockServer::start().await;
    let instance = Instance::new();
    let mut recipe = installed(&server, &instance).await;
    let outside = outside(&instance);
    std::fs::rename(instance.mods_dir(), outside.join("mods")).expect("move mods away");
    std::os::unix::fs::symlink(outside.join("mods"), instance.mods_dir()).expect("link");

    let error = mutate(
        &instance,
        &mut recipe,
        &[],
        &[ManagedProject::new(ModProvider::Modrinth, "ROOTPROJ")],
    )
    .await
    .expect_err("must refuse");

    assert!(matches!(error, PackError::ManagedDestinationLinked { .. }));
    assert!(outside.join("mods/root-1.0.jar").is_file());
    assert!(
        recipe
            .managed_project(ModProvider::Modrinth, "ROOTPROJ")
            .is_some()
    );
}

#[tokio::test]
async fn an_unrecorded_disabled_twin_occupies_a_new_mods_destination() {
    let server = MockServer::start().await;
    serve(&server, "/root-1.jar", ROOT_V1).await;
    let instance = Instance::new();
    let twin = instance.jar("root-1.0.jar.disabled");
    std::fs::write(&twin, b"somebody else's file, turned off").expect("write");
    let mut recipe = recipe();

    let error = mutate(
        &instance,
        &mut recipe,
        &[root_v1(&server, "root-1.0.jar")],
        &[],
    )
    .await
    .expect_err("must refuse");

    assert!(matches!(error, PackError::DestinationOccupied { .. }));
    assert_eq!(error.failing_path().as_deref(), Some(twin.as_path()));
}

#[tokio::test]
async fn a_logical_path_outside_the_game_directory_is_refused_for_mutations() {
    let server = MockServer::start().await;
    serve(&server, "/root-1.jar", ROOT_V1).await;
    let instance = Instance::new();
    let absolute = instance.root.join("absolute.jar");

    for logical in [
        PathBuf::from("../outside.jar"),
        absolute.clone(),
        PathBuf::from(".launcher/source/recipe.jar"),
    ] {
        let mut recipe = recipe();
        let mut file = root_v1(&server, "root-1.0.jar");
        file.logical_path = logical.clone();

        let error = mutate(&instance, &mut recipe, &[file], &[])
            .await
            .expect_err("must refuse");

        assert!(
            matches!(error, PackError::ManagedPathUnsafe { .. }),
            "{logical:?} gave {error:?}"
        );
    }
    assert!(!instance.root.join("outside.jar").exists());
    assert!(!absolute.exists());
    assert!(!instance.store.has_recipe());
}

#[tokio::test]
async fn a_recorded_logical_path_outside_the_game_directory_is_never_retired_or_replaced() {
    let server = MockServer::start().await;
    let instance = Instance::new();
    let mut recipe = installed(&server, &instance).await;
    let victim = instance.root.join("victim.jar");
    std::fs::write(&victim, b"not a mod of this instance").expect("write");
    recipe
        .files
        .iter_mut()
        .find(|file| file.logical_path == Path::new("mods/root-1.0.jar"))
        .expect("recorded root")
        .logical_path = PathBuf::from("../victim.jar");
    serve(&server, "/root-2.jar", ROOT_V2).await;

    let removal = mutate(
        &instance,
        &mut recipe,
        &[],
        &[ManagedProject::new(ModProvider::Modrinth, "ROOTPROJ")],
    )
    .await
    .expect_err("removal must refuse");
    let replacement = mutate(
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
    .expect_err("replacement must refuse");

    assert!(matches!(removal, PackError::ManagedPathUnsafe { .. }));
    assert!(matches!(replacement, PackError::ManagedPathUnsafe { .. }));
    assert!(
        victim.is_file(),
        "the file outside the instance is untouched"
    );
}

// -- one entry per project ------------------------------------------------

fn duplicate_project(error: &PackError) -> Option<&str> {
    match error {
        PackError::DuplicateManagedProject { project_id, .. } => Some(project_id),
        _ => None,
    }
}

#[tokio::test]
async fn two_new_entries_for_one_project_are_refused_before_any_jar_is_written() {
    let server = MockServer::start().await;
    serve(&server, "/root-1.jar", ROOT_V1).await;
    let instance = Instance::new();
    let mut recipe = recipe();

    let error = mutate(
        &instance,
        &mut recipe,
        &[
            entry(
                &server,
                "/root-1.jar",
                "root-a.jar",
                ROOT_V1,
                "ROOTPROJ",
                "ROOTV1",
                ModOrigin::UserSelected,
            ),
            entry(
                &server,
                "/root-1.jar",
                "root-b.jar",
                ROOT_V1,
                "ROOTPROJ",
                "ROOTV1",
                ModOrigin::UserSelected,
            ),
        ],
        &[],
    )
    .await
    .expect_err("one project cannot be installed twice");

    assert_eq!(duplicate_project(&error), Some("ROOTPROJ"));
    assert!(instance.jars().is_empty(), "no jar is written");
    assert!(!instance.store.has_recipe());
    assert!(recipe.files.is_empty());
}

#[tokio::test]
async fn two_replacements_for_one_installed_project_leave_the_instance_untouched() {
    let server = MockServer::start().await;
    let instance = Instance::new();
    let mut recipe = installed(&server, &instance).await;
    serve(&server, "/root-2.jar", ROOT_V2).await;
    serve(&server, "/root-3.jar", ROOT_V3).await;
    let before = instance.store.read_recipe().expect("recipe");

    let error = mutate(
        &instance,
        &mut recipe,
        &[
            entry(
                &server,
                "/root-2.jar",
                "root-2.0.jar",
                ROOT_V2,
                "ROOTPROJ",
                "ROOTV2",
                ModOrigin::UserSelected,
            ),
            entry(
                &server,
                "/root-3.jar",
                "root-3.0.jar",
                ROOT_V3,
                "ROOTPROJ",
                "ROOTV3",
                ModOrigin::UserSelected,
            ),
        ],
        &[],
    )
    .await
    .expect_err("one project cannot take two versions");

    assert_eq!(duplicate_project(&error), Some("ROOTPROJ"));
    assert_eq!(instance.jars(), vec!["library-2.0.jar", "root-1.0.jar"]);
    assert_eq!(instance.store.read_recipe().expect("recipe"), before);
    assert_eq!(version_of(&recipe, "ROOTPROJ").as_deref(), Some("ROOTV1"));
}

#[tokio::test]
async fn removing_a_project_the_recipe_records_twice_is_refused_and_keeps_both_jars() {
    let server = MockServer::start().await;
    let instance = Instance::new();
    let mut recipe = installed(&server, &instance).await;
    // Stored state that no operation of this crate produces: a second jar and a
    // second record for the same project.
    std::fs::write(instance.jar("root-copy.jar"), ROOT_V1).expect("write");
    let mut copy = recipe
        .managed_project(ModProvider::Modrinth, "ROOTPROJ")
        .expect("recorded")
        .clone();
    copy.logical_path = PathBuf::from("mods/root-copy.jar");
    recipe.files.push(copy);
    let before = recipe.clone();

    let error = mutate(
        &instance,
        &mut recipe,
        &[],
        &[ManagedProject::new(ModProvider::Modrinth, "ROOTPROJ")],
    )
    .await
    .expect_err("a removal would strand the second jar");

    assert_eq!(duplicate_project(&error), Some("ROOTPROJ"));
    assert_eq!(
        instance.jars(),
        vec!["library-2.0.jar", "root-1.0.jar", "root-copy.jar"]
    );
    assert_eq!(recipe, before);
}

#[tokio::test]
async fn replacing_a_project_the_recipe_records_twice_is_refused() {
    let server = MockServer::start().await;
    let instance = Instance::new();
    let mut recipe = installed(&server, &instance).await;
    serve(&server, "/root-2.jar", ROOT_V2).await;
    std::fs::write(instance.jar("root-copy.jar"), ROOT_V1).expect("write");
    let mut copy = recipe
        .managed_project(ModProvider::Modrinth, "ROOTPROJ")
        .expect("recorded")
        .clone();
    copy.logical_path = PathBuf::from("mods/root-copy.jar");
    recipe.files.push(copy);

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
    .expect_err("a replacement would strand the second jar");

    assert_eq!(duplicate_project(&error), Some("ROOTPROJ"));
    assert!(!instance.jar("root-2.0.jar").exists());
    assert!(instance.jar("root-1.0.jar").is_file());
    assert!(instance.jar("root-copy.jar").is_file());
}

#[tokio::test]
async fn naming_one_project_twice_in_removals_or_in_both_lists_is_refused() {
    let server = MockServer::start().await;
    let instance = Instance::new();
    let mut recipe = installed(&server, &instance).await;
    serve(&server, "/root-2.jar", ROOT_V2).await;
    let root = ManagedProject::new(ModProvider::Modrinth, "ROOTPROJ");

    let twice = mutate(&instance, &mut recipe, &[], &[root.clone(), root.clone()])
        .await
        .expect_err("a project cannot be removed twice");
    let both = mutate(
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
        &[root],
    )
    .await
    .expect_err("a project cannot be replaced and removed at once");

    assert_eq!(duplicate_project(&twice), Some("ROOTPROJ"));
    assert_eq!(duplicate_project(&both), Some("ROOTPROJ"));
    assert_eq!(instance.jars(), vec!["library-2.0.jar", "root-1.0.jar"]);
    assert_eq!(version_of(&recipe, "ROOTPROJ").as_deref(), Some("ROOTV1"));
}

// -- recording the side a mod is installed on ------------------------------

/// The chosen mod's entry as a server install requests it.
fn root_v1_for_server(server: &MockServer, requirement: SideRequirement) -> PackFileRecipe {
    let mut file = root_v1(server, "root-1.0.jar");
    file.server = requirement;
    file.selected = requirement.needs_decision().then_some(true);
    file
}

/// Records the chosen mod as one the server's own record does not take.
fn leave_server_out(recipe: &mut PackRecipe, requirement: SideRequirement) {
    for file in &mut recipe.files {
        if file.logical_path == Path::new("mods/root-1.0.jar") {
            file.server = requirement;
            file.selected = None;
        }
    }
}

fn entry_of<'a>(recipe: &'a PackRecipe, project: &str) -> &'a PackFileRecipe {
    recipe
        .managed_project(ModProvider::Modrinth, project)
        .expect("entry")
}

#[tokio::test]
async fn a_missing_jar_put_back_on_a_side_the_record_left_out_records_that_side() {
    for left_out in [SideRequirement::Unsupported, SideRequirement::Unknown] {
        let server = MockServer::start().await;
        let instance = Instance::new();
        let mut recipe = installed(&server, &instance).await;
        leave_server_out(&mut recipe, left_out);
        std::fs::remove_file(instance.jar("root-1.0.jar")).expect("delete the jar");
        let upserts = vec![root_v1_for_server(&server, SideRequirement::Required)];
        let predicted = ephervex_pack::projected(&recipe, &upserts, &[], PackSide::Server);

        let report = mutate_on(PackSide::Server, &instance, &mut recipe, &upserts, &[])
            .await
            .expect("install on the server");

        assert_eq!(report.repaired, [Path::new("mods/root-1.0.jar")]);
        assert!(instance.jar("root-1.0.jar").is_file());
        let recorded = entry_of(&recipe, "ROOTPROJ");
        assert!(
            recorded.is_installed_on(PackSide::Server, &ephervex_pack::OptionalSelection::none()),
            "a jar was written, so the recipe has to say the server has it"
        );
        assert_eq!(recorded.client, SideRequirement::Required);
        assert_eq!(recorded.origin, ModOrigin::UserSelected);
        assert_eq!(predicted, recipe, "the preview matches what was committed");
        assert_eq!(instance.store.read_recipe().expect("recipe"), recipe);
    }
}

#[tokio::test]
async fn a_present_jar_on_a_side_the_record_left_out_only_changes_the_record() {
    let server = MockServer::start().await;
    let instance = Instance::new();
    let mut recipe = installed(&server, &instance).await;
    leave_server_out(&mut recipe, SideRequirement::Unsupported);
    let upserts = vec![root_v1_for_server(&server, SideRequirement::Required)];
    let predicted = ephervex_pack::projected(&recipe, &upserts, &[], PackSide::Server);

    let report = mutate_on(PackSide::Server, &instance, &mut recipe, &upserts, &[])
        .await
        .expect("install on the server");

    assert_eq!(report.recorded, [Path::new("mods/root-1.0.jar")]);
    assert!(report.installed.is_empty() && report.repaired.is_empty());
    assert!(report.changed_anything());
    assert_eq!(
        entry_of(&recipe, "ROOTPROJ").server,
        SideRequirement::Required
    );
    assert_eq!(predicted, recipe);
    assert_eq!(instance.store.read_recipe().expect("recipe"), recipe);
}

#[tokio::test]
async fn an_unselected_optional_mod_installed_on_a_side_is_recorded_as_selected() {
    let server = MockServer::start().await;
    let instance = Instance::new();
    let mut recipe = installed(&server, &instance).await;
    leave_server_out(&mut recipe, SideRequirement::Optional);
    std::fs::remove_file(instance.jar("root-1.0.jar")).expect("delete the jar");
    let upserts = vec![root_v1_for_server(&server, SideRequirement::Optional)];

    mutate_on(PackSide::Server, &instance, &mut recipe, &upserts, &[])
        .await
        .expect("install on the server");

    let recorded = entry_of(&recipe, "ROOTPROJ");
    assert_eq!(recorded.server, SideRequirement::Optional);
    assert_eq!(recorded.selected, Some(true));
}

#[tokio::test]
async fn a_side_other_than_the_instances_own_is_not_rewritten_by_a_same_version_request() {
    let server = MockServer::start().await;
    let instance = Instance::new();
    let mut recipe = installed(&server, &instance).await;
    leave_server_out(&mut recipe, SideRequirement::Unsupported);
    let before = recipe.clone();
    let upserts = vec![root_v1_for_server(&server, SideRequirement::Required)];

    let report = mutate(&instance, &mut recipe, &upserts, &[])
        .await
        .expect("client request");

    assert_eq!(report.unchanged.len(), 1);
    assert!(report.recorded.is_empty());
    assert_eq!(recipe, before);
}

#[tokio::test]
async fn a_modified_jar_is_not_recorded_as_installed_on_a_side_the_record_left_out() {
    let server = MockServer::start().await;
    let instance = Instance::new();
    let mut recipe = installed(&server, &instance).await;
    leave_server_out(&mut recipe, SideRequirement::Unsupported);
    std::fs::write(instance.jar("root-1.0.jar"), b"the developer's own build").expect("modify");
    let before = recipe.clone();
    let upserts = vec![root_v1_for_server(&server, SideRequirement::Required)];
    let predicted = ephervex_pack::projected(&recipe, &upserts, &[], PackSide::Server);

    let report = mutate_on(PackSide::Server, &instance, &mut recipe, &upserts, &[])
        .await
        .expect("request");

    assert_eq!(report.modified.len(), 1);
    assert!(report.recorded.is_empty());
    assert_eq!(recipe, before);
    assert_eq!(
        entry_of(&predicted, "ROOTPROJ").server,
        SideRequirement::Required,
        "a preview does not read the disk, so it cannot see the modified jar"
    );
}

#[tokio::test]
async fn planning_a_pack_owned_version_change_records_the_original_release_like_carrying_it_out() {
    let server = MockServer::start().await;
    let instance = Instance::new();
    let mut recipe = installed(&server, &instance).await;
    for file in &mut recipe.files {
        if file.logical_path == Path::new("mods/root-1.0.jar") {
            file.origin = ModOrigin::PackDeclared;
        }
    }
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

    let predicted = ephervex_pack::projected(&recipe, &upserts, &[], PackSide::Client);
    mutate(&instance, &mut recipe, &upserts, &[])
        .await
        .expect("version change");

    assert_eq!(
        original_version_of(&predicted, "ROOTPROJ").as_deref(),
        Some("ROOTV1")
    );
    assert_eq!(predicted, recipe);
}
