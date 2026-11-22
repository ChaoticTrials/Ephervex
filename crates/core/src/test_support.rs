//! Credential storage for the tests of crates that link this one.
//!
//! Compiled only with the `test-support` feature, which a dev-dependency
//! enables. A test that opens an [`App`](crate::App) on a temporary root
//! isolates files but not the desktop keyring, so reading an account status
//! would reach the developer's Secret Service and can raise an unlock prompt.
//!
//! It also builds client fixtures that need no provider metadata, so a
//! frontend test can exercise a client reader without a network, fixes the
//! Java inventory, so no test probes a runtime installed on the machine, and
//! builds images for the theme folders a frontend test writes.

use std::sync::{Arc, OnceLock};

use ephervex_auth::{CredentialStore, MemoryStore};
use ephervex_java::JavaRuntime;
use ephervex_model::InstanceId;
use ephervex_storage::{ClientRecord, NewClient};

use crate::App;
use crate::error::CoreResult;

static STORE: OnceLock<Arc<dyn CredentialStore>> = OnceLock::new();

/// Replaces the desktop keyring with an empty in-memory store for the rest of
/// the process.
///
/// Applies to every [`App`](crate::App), including ones the code under test
/// opens itself. Calling it again keeps the store already installed.
pub fn use_in_memory_credential_store() {
    STORE.get_or_init(|| Arc::new(MemoryStore::new()));
}

pub(crate) fn installed_store() -> Option<Arc<dyn CredentialStore>> {
    STORE.get().map(Arc::clone)
}

/// Creates a client without checking its Minecraft or loader selection against
/// provider metadata.
pub fn create_client_offline(app: &App, request: NewClient) -> CoreResult<ClientRecord> {
    app.create_client_for_test(request)
}

/// Commits a change to a client's canonical metadata alone, leaving its index
/// row as it was: the state after an index write failed behind a committed
/// update.
pub fn commit_client_without_index(
    app: &App,
    id: InstanceId,
    change: impl FnOnce(&mut ClientRecord),
) -> CoreResult<ClientRecord> {
    app.commit_client_without_index_for_test(id, change)
}

/// Makes a client's index row name a loader this build does not know, so the
/// listings leave it out and report it as unreadable.
pub fn mark_client_row_written_by_newer(app: &App, id: InstanceId) -> CoreResult<()> {
    app.mark_client_row_written_by_newer_for_test(id)
}

/// Creates a client whose mods folder holds `mods/LibX.jar` and
/// `mods/UtilitiX.jar`, with UtilitiX recorded as requiring LibX, so turning
/// LibX off or deleting it would take UtilitiX along.
pub fn create_client_with_library_and_dependent(app: &App) -> CoreResult<ClientRecord> {
    use ephervex_model::ModRelationKind;
    use ephervex_model::{Hash, HashAlgorithm, LoaderSelection, ModOrigin, ModProvider};
    use ephervex_pack::{
        PackFileRecipe, PackRecipe, ProviderFileIdentity, RecipeRelation, RelationTarget,
        SideRequirement, SourceStore,
    };
    use std::path::PathBuf;

    let record = app.create_client_for_test(NewClient::new(
        "Library Example",
        "1.21.1",
        LoaderSelection::vanilla(),
    ))?;
    // The digest of an empty file, which no fixture check here compares.
    let empty: Vec<Hash> = Hash::new(
        HashAlgorithm::Sha1,
        "da39a3ee5e6b4b0d3255bfef95601890afd80709",
    )
    .into_iter()
    .collect();
    let recorded = |name: &str, project: &str, requires: Option<&str>| PackFileRecipe {
        logical_path: PathBuf::from("mods").join(name),
        hashes: empty.clone(),
        downloads: Vec::new(),
        size: Some(3),
        client: SideRequirement::Required,
        server: SideRequirement::Required,
        selected: None,
        origin: ModOrigin::UserSelected,
        identity: Some(ProviderFileIdentity {
            provider: Some(ModProvider::Modrinth),
            project_id: Some(project.to_owned()),
            version_id: Some(format!("{project}-release")),
            file_id: None,
        }),
        original_identity: None,
        installed_despite_unsupported: None,
        relations: Some(
            requires
                .map(|project_id| RecipeRelation {
                    kind: ModRelationKind::Required,
                    target: RelationTarget::Project {
                        project_id: project_id.to_owned(),
                        version_id: None,
                    },
                })
                .into_iter()
                .collect(),
        ),
    };
    let mut recipe =
        PackRecipe::for_manual_instance("Library Example", "1.21.1", record.loader.clone());
    recipe.files = vec![
        recorded("LibX.jar", "LIBX", None),
        recorded("UtilitiX.jar", "UTILITIX", Some("LIBX")),
    ];
    SourceStore::new(ephervex_storage::metadata::source_dir(&record.root)).write_recipe(&recipe)?;
    let mods = ephervex_storage::metadata::client_game_dir(&record.root).join("mods");
    let unwritable =
        |path: PathBuf| move |source| ephervex_pack::PackError::Unwritable { path, source };
    std::fs::create_dir_all(&mods).map_err(unwritable(mods.clone()))?;
    for jar in ["LibX.jar", "UtilitiX.jar"] {
        let path = mods.join(jar);
        std::fs::write(&path, b"jar").map_err(unwritable(path.clone()))?;
    }
    Ok(record)
}

/// A storage cleanup plan with these items, as a frontend shows one for review.
///
/// It lists what it was given and removes nothing: a plan core makes carries
/// the entries behind its items, and a frontend test has none to give.
pub fn cleanup_plan_for_review(
    items: Vec<crate::CleanupItem>,
    kept: Vec<crate::KeptStorage>,
    complete: bool,
) -> crate::CleanupPlan {
    crate::CleanupPlan::for_review_tests(items, kept, complete)
}

/// Replaces an app's Java inventory with these runtimes, as if a scan had
/// found exactly them. A later refresh scans the machine again.
pub fn use_java_inventory(app: &App, runtimes: Vec<JavaRuntime>) {
    app.use_java_inventory_for_test(runtimes);
}

/// A launch or start refusal of its Java, as a frontend receives one, so a
/// frontend test can render it without planning a launch.
pub fn java_refusal_error(
    error: ephervex_java::JavaError,
    mode: crate::JavaSelectionMode,
    attempted: Option<crate::JavaRuntimeSummary>,
    expected: crate::ExpectedRuntime,
    mismatch: Option<crate::JavaMismatch>,
) -> crate::CoreError {
    crate::CoreError::JavaRefused(Box::new(crate::JavaRefusal {
        error,
        chosen: crate::RefusedJavaChoice::Selected(mode),
        attempted,
        expected,
        mismatch,
    }))
}

/// A `width` by `height` PNG filled with one straight-alpha RGBA color.
///
/// # Panics
///
/// If encoding fails, which a test fixture cannot recover from.
#[must_use]
#[allow(clippy::expect_used)]
pub fn solid_png(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(width, height, image::Rgba(rgba));
    let mut bytes = Vec::new();
    image
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .expect("a solid image encodes as PNG");
    bytes
}
