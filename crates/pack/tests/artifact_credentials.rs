//! When a provider credential is asked for, and what a refusal of one means.
//!
//! Two invariants that are easy to state and easy to break, so they are checked
//! against a supplier that counts what it was asked rather than against the
//! wording of a final error message:
//!
//! * a credential is requested only for an artifact that is genuinely going to
//!   be fetched - never for one the shared cache already holds, and never
//!   because reading the cache went wrong;
//! * a refusal is attributed to the credential only when the request actually
//!   carried one, so an ordinary `403` from a host that was never offered a
//!   secret stays an ordinary download failure.
//!
//! Everything runs against `wiremock` on loopback. No test here contacts a real
//! provider.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ephervex_download::{
    AuthenticatedSource, CacheLayout, ContentStore, DomainAllowlist, DownloadManager,
    DownloaderConfig, FileExtension, HttpConfig, OperationReporter,
};
use ephervex_model::{
    Hash, HashAlgorithm, LoaderKind, LoaderSelection, ModOrigin, ModProvider, SecretString,
};
use ephervex_pack::{
    ArtifactCredentials, ArtifactFetch, CredentialUnavailable, FileInstall, OptionalSelection,
    PackError, PackFileRecipe, PackRecipe, PackSide, ProviderFileIdentity, SideRequirement,
    install_files,
};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{header, method, path as path_matcher};
use wiremock::{Mock, MockServer, ResponseTemplate};

const BODY: &[u8] = b"a jar an instance is supposed to contain";
const KEY: &str = "test-provider-key-do-not-use";
const HEADER: &str = "x-api-key";

/// What a provider layer would have said about a refused credential.
///
/// A distinct type so a test can prove the supplier's own answer came back
/// rather than something reconstructed from a status.
#[derive(Debug, thiserror::Error)]
#[error("the configured key was refused with HTTP {status}")]
struct KeyRefused {
    status: u16,
}

/// A credential supplier that records every question it is asked.
///
/// The point of the counters: "no credential was needed" is a structural claim
/// about what was called, and an assertion on a final error message cannot make
/// it.
#[derive(Debug)]
struct CountingCredentials {
    credential: Arc<AuthenticatedSource>,
    /// Which provider this supplier authenticates. Everything else is open.
    authenticates: ModProvider,
    asked: AtomicUsize,
    consulted_about_refusals: AtomicUsize,
}

impl CountingCredentials {
    fn for_provider(authenticates: ModProvider) -> Self {
        Self {
            credential: Arc::new(
                AuthenticatedSource::new(
                    reqwest::header::HeaderName::from_static(HEADER),
                    SecretString::new(KEY),
                    DomainAllowlist::for_loopback_tests("127.0.0.1"),
                    &HttpConfig::for_loopback_tests(),
                )
                .expect("a credential builds its containment client"),
            ),
            authenticates,
            asked: AtomicUsize::new(0),
            consulted_about_refusals: AtomicUsize::new(0),
        }
    }

    fn asked(&self) -> usize {
        self.asked.load(Ordering::Relaxed)
    }

    fn consulted_about_refusals(&self) -> usize {
        self.consulted_about_refusals.load(Ordering::Relaxed)
    }
}

impl ArtifactCredentials for CountingCredentials {
    fn credential_for(
        &self,
        identity: Option<&ProviderFileIdentity>,
    ) -> Result<Option<Arc<AuthenticatedSource>>, CredentialUnavailable> {
        self.asked.fetch_add(1, Ordering::Relaxed);
        if identity.and_then(|identity| identity.provider) == Some(self.authenticates) {
            return Ok(Some(Arc::clone(&self.credential)));
        }
        Ok(None)
    }

    fn rejected(
        &self,
        identity: Option<&ProviderFileIdentity>,
        status: u16,
    ) -> Option<CredentialUnavailable> {
        self.consulted_about_refusals
            .fetch_add(1, Ordering::Relaxed);
        (identity.and_then(|identity| identity.provider) == Some(self.authenticates))
            .then(|| CredentialUnavailable::new(KeyRefused { status }))
    }
}

fn sha1(bytes: &[u8]) -> Hash {
    use sha1::Digest as _;
    let mut digest = sha1::Sha1::new();
    digest.update(bytes);
    Hash::new(HashAlgorithm::Sha1, &hex::encode(digest.finalize())).expect("sha1")
}

fn recipe(address: &str, provider: Option<ModProvider>) -> PackRecipe {
    let mut recipe = PackRecipe::for_manual_instance(
        "artifact credentials".to_owned(),
        "1.21.1".to_owned(),
        LoaderSelection::exact(LoaderKind::NeoForge, "21.1.100".to_owned()),
    );
    recipe.files.push(PackFileRecipe {
        logical_path: PathBuf::from("mods/wanted.jar"),
        hashes: vec![sha1(BODY)],
        downloads: vec![address.to_owned()],
        size: Some(BODY.len() as u64),
        client: SideRequirement::Required,
        server: SideRequirement::Required,
        selected: None,
        origin: ModOrigin::PackDeclared,
        identity: provider.map(|provider| ProviderFileIdentity {
            provider: Some(provider),
            project_id: Some("238222".to_owned()),
            version_id: Some("4593548".to_owned()),
            file_id: Some("4593548".to_owned()),
        }),
        original_identity: None,
        installed_despite_unsupported: None,
        relations: None,
    });
    recipe
}

/// The shared cache path the installer will resolve for a recipe entry.
fn cache_path(layout: &CacheLayout, entry: &PackFileRecipe) -> PathBuf {
    layout.content(
        ContentStore::Downloads,
        &entry.hashes[0],
        Some(&FileExtension::new("jar").expect("an extension")),
    )
}

async fn install(
    recipe: &PackRecipe,
    layout: &CacheLayout,
    game_dir: &std::path::Path,
    credentials: &CountingCredentials,
) -> Result<(), PackError> {
    install_files(FileInstall {
        recipe,
        side: PackSide::Client,
        selection: &OptionalSelection::default(),
        game_dir,
        fetch: ArtifactFetch {
            manager: &DownloadManager::new(DownloaderConfig {
                http: HttpConfig::for_loopback_tests(),
                ..DownloaderConfig::default()
            })
            .expect("download manager"),
            cache: layout,
            credentials,
            reporter: &OperationReporter::silent("artifact-credentials"),
            cancel: &CancellationToken::new(),
        },
    })
    .await
    .map(|_| ())
}

/// Serves the artifact only to a request carrying the expected key.
async fn serve_guarded(server: &MockServer, route: &str) {
    Mock::given(method("GET"))
        .and(path_matcher(route))
        .and(header(HEADER, KEY))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(BODY))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path_matcher(route))
        .respond_with(ResponseTemplate::new(403))
        .mount(server)
        .await;
}

#[tokio::test]
async fn a_cache_that_cannot_be_read_fails_locally_without_asking_for_a_credential() {
    // A cache path whose shard is a regular file: reading it is an I/O error
    // rather than "nothing is there". That distinction is the whole test - a
    // local storage fault says nothing about whether a fetch is needed, so
    // recovering from it by fetching would ask for a credential, and a launcher
    // with none configured would be told its API key is missing when no request
    // was ever going to be made.
    let server = MockServer::start().await;
    serve_guarded(&server, "/artifact.jar").await;
    let temp = tempfile::tempdir().expect("tempdir");
    let layout = CacheLayout::new(temp.path().join("cache"));
    let recipe = recipe(
        &format!("{}/artifact.jar", server.uri()),
        Some(ModProvider::CurseForge),
    );

    let shard = cache_path(&layout, &recipe.files[0])
        .parent()
        .expect("a shard directory")
        .to_path_buf();
    std::fs::create_dir_all(shard.parent().expect("a store")).expect("store directory");
    std::fs::write(&shard, b"not a directory").expect("occupy the shard");

    let credentials = CountingCredentials::for_provider(ModProvider::CurseForge);
    let error = install(
        &recipe,
        &layout,
        &temp.path().join("instance/minecraft"),
        &credentials,
    )
    .await
    .expect_err("an unreadable cache is a failure, not a miss");

    assert!(
        matches!(error, PackError::DownloadFailed { .. }),
        "a local cache fault must be reported as itself: {error:?}"
    );
    assert_eq!(
        credentials.asked(),
        0,
        "a credential was requested for an artifact no fetch was proven necessary for"
    );
    assert!(
        server
            .received_requests()
            .await
            .expect("recording enabled")
            .is_empty(),
        "a local cache fault must not produce a request"
    );
}

#[tokio::test]
async fn a_genuine_cache_miss_asks_for_the_credential_exactly_once_and_installs() {
    // The control for the test above: the same recipe, the same supplier, and a
    // cache that is merely empty.
    let server = MockServer::start().await;
    serve_guarded(&server, "/artifact.jar").await;
    let temp = tempfile::tempdir().expect("tempdir");
    let layout = CacheLayout::new(temp.path().join("cache"));
    let game_dir = temp.path().join("instance/minecraft");
    let recipe = recipe(
        &format!("{}/artifact.jar", server.uri()),
        Some(ModProvider::CurseForge),
    );

    let credentials = CountingCredentials::for_provider(ModProvider::CurseForge);
    install(&recipe, &layout, &game_dir, &credentials)
        .await
        .expect("the artifact is fetched with the credential");

    assert_eq!(credentials.asked(), 1);
    assert_eq!(
        credentials.consulted_about_refusals(),
        0,
        "nothing was refused"
    );
    assert_eq!(
        std::fs::read(game_dir.join("mods/wanted.jar")).expect("read"),
        BODY
    );
    assert!(cache_path(&layout, &recipe.files[0]).is_file());
}

#[tokio::test]
async fn a_verified_cache_entry_installs_without_the_supplier_being_asked_anything() {
    let server = MockServer::start().await;
    serve_guarded(&server, "/artifact.jar").await;
    let temp = tempfile::tempdir().expect("tempdir");
    let layout = CacheLayout::new(temp.path().join("cache"));
    let game_dir = temp.path().join("instance/minecraft");
    let recipe = recipe(
        &format!("{}/artifact.jar", server.uri()),
        Some(ModProvider::CurseForge),
    );

    let cached = cache_path(&layout, &recipe.files[0]);
    std::fs::create_dir_all(cached.parent().expect("shard")).expect("shard directory");
    std::fs::write(&cached, BODY).expect("seed the cache");

    let credentials = CountingCredentials::for_provider(ModProvider::CurseForge);
    install(&recipe, &layout, &game_dir, &credentials)
        .await
        .expect("a verified cache entry needs no provider");

    assert_eq!(credentials.asked(), 0);
    assert!(
        server
            .received_requests()
            .await
            .expect("recording enabled")
            .is_empty()
    );
    assert_eq!(
        std::fs::read(game_dir.join("mods/wanted.jar")).expect("read"),
        BODY
    );
}

#[tokio::test]
async fn a_refusal_of_a_credential_this_request_carried_is_reported_as_the_credential() {
    // The key is syntactically fine and the host refuses it, which is a fact only
    // the layer that issued it can turn into a next step for the user.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_matcher("/artifact.jar"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;

    let temp = tempfile::tempdir().expect("tempdir");
    let layout = CacheLayout::new(temp.path().join("cache"));
    let recipe = recipe(
        &format!("{}/artifact.jar", server.uri()),
        Some(ModProvider::CurseForge),
    );

    let credentials = CountingCredentials::for_provider(ModProvider::CurseForge);
    let error = install(
        &recipe,
        &layout,
        &temp.path().join("instance/minecraft"),
        &credentials,
    )
    .await
    .expect_err("a refused credential is a failure");

    let PackError::ArtifactCredentialUnavailable { path, source } = error else {
        panic!("a refused credential must not be reported as a download failure");
    };
    assert_eq!(path, "mods/wanted.jar");
    let refused = source
        .into_inner()
        .downcast::<KeyRefused>()
        .expect("the supplier's own account of the refusal is what travels back");
    assert_eq!(refused.status, 403);
    assert!(credentials.consulted_about_refusals() > 0);
}

#[tokio::test]
async fn the_same_status_from_a_provider_that_was_offered_nothing_stays_a_download_failure() {
    // The identical response, for a file whose identity names another catalog.
    // Nothing was sent to authenticate it, so nothing about it was refused.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_matcher("/artifact.jar"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;

    let temp = tempfile::tempdir().expect("tempdir");
    let layout = CacheLayout::new(temp.path().join("cache"));
    let recipe = recipe(
        &format!("{}/artifact.jar", server.uri()),
        Some(ModProvider::Modrinth),
    );

    let credentials = CountingCredentials::for_provider(ModProvider::CurseForge);
    let error = install(
        &recipe,
        &layout,
        &temp.path().join("instance/minecraft"),
        &credentials,
    )
    .await
    .expect_err("the download failed");

    assert!(
        matches!(error, PackError::DownloadFailed { .. }),
        "another provider's refusal was blamed on a credential: {error:?}"
    );
    assert_eq!(
        credentials.consulted_about_refusals(),
        0,
        "the supplier was asked about a request that carried no credential"
    );
}

#[tokio::test]
async fn a_refusal_from_an_address_the_credential_was_withheld_from_stays_a_download_failure() {
    // `localhost` and `127.0.0.1` are the same machine and different hosts. The
    // source declines to attach itself to the address the recipe names, so the
    // refusal that comes back is about the address rather than about the key.
    let server = MockServer::start().await;
    serve_guarded(&server, "/artifact.jar").await;
    let temp = tempfile::tempdir().expect("tempdir");
    let layout = CacheLayout::new(temp.path().join("cache"));
    let recipe = recipe(
        &format!("http://localhost:{}/artifact.jar", server.address().port()),
        Some(ModProvider::CurseForge),
    );

    let credentials = CountingCredentials::for_provider(ModProvider::CurseForge);
    let error = install(
        &recipe,
        &layout,
        &temp.path().join("instance/minecraft"),
        &credentials,
    )
    .await
    .expect_err("the guarded route refuses an unauthenticated request");

    assert!(
        matches!(error, PackError::DownloadFailed { .. }),
        "a refusal from a host the key was withheld from was blamed on the key: {error:?}"
    );
    assert_eq!(credentials.consulted_about_refusals(), 0);
}
