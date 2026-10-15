//! The CurseForge integration's application boundary.
//!
//! Two things meet here and nowhere else: the credential store that ADR 0004
//! puts every secret in, and the transport in `ephervex-mod-platform` that
//! needs one. The transport knows nothing about keyrings, and the keyring knows
//! nothing about CurseForge; this module is the adapter, which is what lets the
//! source of the credential change later without the transport noticing.
//!
//! # What a frontend may know
//!
//! Whether a key is configured. Never the key. [`App::curseforge_api`] hands
//! back a client that can *use* the credential, and
//! [`crate::ApiSettings::curseforge_key`] reports, as a
//! [`crate::CurseForgeKeyState`], whether one is in effect; there is no path
//! from a frontend to the value, by construction rather than by convention.

use std::fmt;
use std::sync::Arc;

use ephervex_auth::{AuthError, CredentialStore};
use ephervex_mod_platform::{
    CurseForgeApi, CurseForgeCredentialSource, ModPlatformError, ModPlatformResult,
};
use ephervex_model::{ModProvider, SecretString};

use crate::App;
use crate::account::credential_store;
use crate::error::CoreResult;

/// The CurseForge API key, read from wherever secrets are kept.
///
/// The store is opened on each read rather than held, and that is deliberate:
/// a keyring can be locked and unlocked while the launcher runs, so a handle
/// taken at startup would answer for a machine state that has since changed.
pub struct StoredApiKey(Source);

/// Which secret store a [`StoredApiKey`] reads.
enum Source {
    /// The desktop keyring, opened per read.
    Keyring,
    /// An explicit store. Used by tests, which must never touch the developer's
    /// own keyring.
    Explicit(Arc<dyn CredentialStore>),
    /// The keyring a test `App` was given.
    #[cfg(test)]
    Test(crate::account::TestKeyring),
}

impl StoredApiKey {
    /// A source that reads the desktop keyring.
    #[must_use]
    pub const fn in_keyring() -> Self {
        Self(Source::Keyring)
    }

    /// A source that reads an explicit credential store.
    #[must_use]
    pub fn in_store(store: Arc<dyn CredentialStore>) -> Self {
        Self(Source::Explicit(store))
    }
}

impl fmt::Debug for StoredApiKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Names where the key would come from, never what is there. A derived
        // implementation would render the credential store, which is one
        // refactor away from rendering what it holds.
        formatter.write_str(match self.0 {
            Source::Keyring => "StoredApiKey(keyring)",
            Source::Explicit(_) => "StoredApiKey(explicit store)",
            #[cfg(test)]
            Source::Test(_) => "StoredApiKey(test keyring)",
        })
    }
}

impl CurseForgeCredentialSource for StoredApiKey {
    fn api_key(&self) -> ModPlatformResult<Option<SecretString>> {
        resolve_key(match &self.0 {
            Source::Keyring => credential_store(),
            Source::Explicit(store) => Ok(Arc::clone(store)),
            #[cfg(test)]
            Source::Test(keyring) => keyring.open(),
        })
    }
}

/// The key in effect for the outcome of opening a credential store.
///
/// A machine with no secret storage has no user key, so the build's own key
/// applies (ADR 0030). A store that exists but fails is unreadable instead:
/// it may hold a user key that must not be silently replaced.
fn resolve_key(
    store: Result<Arc<dyn CredentialStore>, AuthError>,
) -> ModPlatformResult<Option<SecretString>> {
    match store {
        Ok(store) => {
            ephervex_auth::curseforge_api_key(store.as_ref()).map_err(|error| unreadable(&error))
        }
        Err(AuthError::SecretStorageUnavailable { .. }) => {
            Ok(ephervex_auth::curseforge_api_key_without_store())
        }
        Err(error) => Err(unreadable(&error)),
    }
}

/// A secret store that could not answer.
///
/// Distinct from having no key: a locked keyring is a machine problem, and
/// telling the user to type their key in again would not fix it. `AuthError`'s
/// own text explains the platform failure and never carries a credential.
fn unreadable(error: &AuthError) -> ModPlatformError {
    ModPlatformError::CredentialUnreadable {
        provider: ModProvider::CurseForge,
        detail: error.to_string(),
    }
}

impl App {
    /// The CurseForge third-party API client.
    ///
    /// Built on first use rather than at startup, for the same reason the mod
    /// catalog is: constructing it builds an HTTP client and a TLS stack, and
    /// listing instances has no business paying for one.
    ///
    /// Building it does *not* read the credential, and using it does not
    /// require one to exist - a request without a key fails with
    /// [`ModPlatformError::CredentialMissing`] before anything is sent. So this
    /// is safe to call on a machine that has never configured CurseForge.
    pub fn curseforge_api(&self) -> CoreResult<&CurseForgeApi> {
        if let Some(api) = self.curseforge.get() {
            return Ok(api);
        }
        #[cfg(test)]
        let key = StoredApiKey(Source::Test(self.test_keyring.clone()));
        #[cfg(not(test))]
        let key = StoredApiKey::in_keyring();
        let api = CurseForgeApi::new(Arc::new(key), &self.http_config())?;
        Ok(self.curseforge.get_or_init(|| api))
    }

    /// Points the CurseForge client at a loopback server with an explicit
    /// credential store.
    ///
    /// Compiled only into a test build, so no release binary can reach either
    /// the relaxed transport or a credential store that is not the keyring.
    #[cfg(test)]
    pub(crate) fn use_loopback_curseforge(
        &mut self,
        base: &str,
        store: Arc<dyn CredentialStore>,
    ) -> CoreResult<()> {
        self.loopback_http = true;
        let api = CurseForgeApi::with_base_url(
            Arc::new(StoredApiKey::in_store(store)),
            &ephervex_download::HttpConfig::for_loopback_tests(),
            base,
        )?;
        let _ = self.curseforge.set(api);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use ephervex_auth::MemoryStore;
    use ephervex_storage::Paths;
    use tokio_util::sync::CancellationToken;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    #[test]
    fn an_app_without_a_named_keyring_resolves_the_build_key_instead_of_opening_secret_storage() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = App::open_at(Paths::rooted_at(base.path())).expect("app opens");
        let source = StoredApiKey(Source::Test(app.test_keyring.clone()));

        let key = source
            .api_key()
            .expect("no secret storage is not a failure");

        assert_eq!(
            key.is_some(),
            !ephervex_auth::DEFAULT_CURSEFORGE_API_KEY.is_empty()
        );
    }

    /// Obviously not a credential.
    const FAKE_KEY: &str = "test-curseforge-key-do-not-use";

    fn app_rooted_at(base: &std::path::Path) -> App {
        App::open_at(Paths::rooted_at(base)).expect("app opens")
    }

    fn store_with_key() -> Arc<dyn CredentialStore> {
        let store = MemoryStore::new();
        ephervex_auth::set_curseforge_api_key(&store, &SecretString::new(FAKE_KEY))
            .expect("store the key");
        Arc::new(store)
    }

    #[test]
    fn a_source_reading_an_empty_store_reports_no_key_rather_than_failing() {
        let source = StoredApiKey::in_store(Arc::new(MemoryStore::new()));
        assert!(source.api_key().expect("read").is_none());
    }

    #[test]
    fn a_source_never_renders_the_key_or_the_store_holding_it() {
        let source = StoredApiKey::in_store(store_with_key());
        assert_eq!(
            source.api_key().expect("read").expect("present").expose(),
            FAKE_KEY
        );
        assert!(!format!("{source:?}").contains(FAKE_KEY), "{source:?}");
    }

    #[test]
    fn an_unusable_secret_store_is_a_credential_problem_and_not_an_absent_key() {
        let error = unreadable(&AuthError::SecretStorageUnavailable {
            detail: "no secret service".to_owned(),
        });
        assert!(error.is_credential_problem());
        assert!(matches!(
            error,
            ModPlatformError::CredentialUnreadable { .. }
        ));
    }

    #[test]
    fn a_machine_without_secret_storage_uses_the_build_key_when_there_is_one() {
        let key = resolve_key(Err(AuthError::SecretStorageUnavailable {
            detail: "no secret service".to_owned(),
        }))
        .expect("no storage is not a credential failure");
        let expected = ephervex_auth::DEFAULT_CURSEFORGE_API_KEY;
        match key {
            Some(key) => assert_eq!(key.expose(), expected),
            None => assert!(expected.is_empty(), "the build key was dropped"),
        }
    }

    #[test]
    fn a_store_that_fails_to_read_stays_unreadable_instead_of_using_the_build_key() {
        #[derive(Debug)]
        struct UnreadableStore;
        impl CredentialStore for UnreadableStore {
            fn store(
                &self,
                _: &ephervex_auth::CredentialKey,
                _: &SecretString,
            ) -> ephervex_auth::AuthResult<()> {
                Ok(())
            }
            fn load(
                &self,
                _: &ephervex_auth::CredentialKey,
            ) -> ephervex_auth::AuthResult<Option<SecretString>> {
                Err(AuthError::SecretStorageUnavailable {
                    detail: "locked".to_owned(),
                })
            }
            fn delete(
                &self,
                _: &ephervex_auth::CredentialKey,
            ) -> ephervex_auth::AuthResult<ephervex_auth::CredentialRemoval> {
                Ok(ephervex_auth::CredentialRemoval::Absent)
            }
            fn storage(&self) -> ephervex_auth::CredentialStorage {
                ephervex_auth::CredentialStorage::Keyring
            }
        }

        let error = resolve_key(Ok(Arc::new(UnreadableStore))).expect_err("a locked store");
        assert!(matches!(
            error,
            ModPlatformError::CredentialUnreadable { .. }
        ));
    }

    #[tokio::test]
    async fn a_curseforge_request_authenticates_and_leaves_nothing_in_the_cache_directory() {
        // The published third-party API terms forbid saving what the API
        // returns, so this asserts against the launcher's real cache root: a
        // metadata cache wired in later would create its namespace under
        // exactly this directory.
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        let cache_root = app.paths().cache_dir().to_path_buf();
        let before = cache_entries(&cache_root);

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/games/432"))
            .and(header("x-api-key", FAKE_KEY))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"{"data":{"id":432,"name":"Minecraft","slug":"minecraft"}}"#,
                "application/json",
            ))
            .expect(1)
            .mount(&server)
            .await;

        app.use_loopback_curseforge(&format!("{}/v1", server.uri()), store_with_key())
            .expect("a loopback client");

        let game = app
            .curseforge_api()
            .expect("a client")
            .game(
                ephervex_mod_platform::MINECRAFT_GAME_ID,
                &CancellationToken::new(),
            )
            .await
            .expect("a documented response");
        assert_eq!(game.slug, "minecraft");

        assert_eq!(
            cache_entries(&cache_root),
            before,
            "a CurseForge response was persisted"
        );
    }

    /// Every path under the cache root, so a new namespace anywhere in it
    /// shows up rather than only a new top-level entry.
    fn cache_entries(root: &std::path::Path) -> Vec<std::path::PathBuf> {
        fn walk(directory: &std::path::Path, found: &mut Vec<std::path::PathBuf>) {
            let Ok(entries) = std::fs::read_dir(directory) else {
                return;
            };
            for entry in entries.filter_map(Result::ok) {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, found);
                }
                found.push(path);
            }
        }
        let mut found = Vec::new();
        walk(root, &mut found);
        found.sort();
        found
    }
}

/// Browsing and installing CurseForge mods through the application.
///
/// Everything here runs against `wiremock` on 127.0.0.1 with an obviously fake
/// key in an in-memory credential store. Nothing contacts CurseForge, and
/// nothing reads the developer's own keyring.
#[cfg(test)]
mod browsing {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use std::path::Path;
    use std::sync::Arc;

    use ephervex_auth::MemoryStore;
    use ephervex_download::OperationReporter;
    use ephervex_mod_platform::{ModPlatformError, ModSearchQuery};
    use ephervex_model::{LoaderKind, LoaderSelection, ModProvider, SecretString};
    use ephervex_storage::{NewClient, Paths};
    use sha1::{Digest as _, Sha1};
    use tokio_util::sync::CancellationToken;
    use wiremock::matchers::{body_json, method, path, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::App;
    use crate::mods::ModInstallRequest;

    const FAKE_KEY: &str = "test-curseforge-key-do-not-use";

    /// The Mods class this fixture reports. Deliberately not the number the
    /// production API happens to use, so an implementation that assumed one
    /// would fail here.
    const MODS_CLASS: u32 = 987_654;

    const ROOT_PROJECT: u32 = 100;
    const LIBRARY_PROJECT: u32 = 200;
    const OPTIONAL_PROJECT: u32 = 300;
    /// A project whose files never fit this instance.
    const UNFITTING_PROJECT: u32 = 400;
    /// A root that requires [`UNFITTING_PROJECT`].
    const BLOCKED_PROJECT: u32 = 500;
    /// A project whose author turned third-party downloads off. The real ids
    /// of the regression this fixture stands in for.
    const RESTRICTED_PROJECT: u32 = 391_382;

    const ROOT_NEWEST: u32 = 1001;
    const ROOT_OLDER: u32 = 1000;
    const LIBRARY_FILE: u32 = 2001;
    const OPTIONAL_FILE: u32 = 3001;
    const BLOCKED_FILE: u32 = 5001;
    const RESTRICTED_INSTALLED: u32 = 7_951_519;
    const RESTRICTED_NEWER: u32 = 7_951_520;

    /// CurseForge's documented relation types.
    const REQUIRED: u32 = 3;
    const OPTIONAL: u32 = 2;

    fn jar(project: u32, file: u32) -> Vec<u8> {
        format!("PK\u{3}\u{4} {project}/{file}").into_bytes()
    }

    fn sha1_of(body: &[u8]) -> String {
        let mut hasher = Sha1::new();
        hasher.update(body);
        hex::encode(hasher.finalize())
    }

    fn file_name(project: u32, file: u32) -> String {
        format!("mod-{project}-{file}.jar")
    }

    /// One file document, published from the loopback server itself.
    fn file_document(server: &MockServer, project: u32, file: u32, relations: &str) -> String {
        let body = jar(project, file);
        format!(
            r#"{{"id":{file},"gameId":432,"modId":{project},"isAvailable":true,
                "displayName":"Release {file}","fileName":"{name}","releaseType":1,
                "fileDate":"2023-07-0{day}T10:00:00Z","fileLength":{length},
                "downloadUrl":"{base}/files/{project}/{file}/{name}",
                "hashes":[{{"value":"{digest}","algo":1}}],
                "gameVersions":["1.20.1","NeoForge"],
                "dependencies":[{relations}]}}"#,
            name = file_name(project, file),
            day = file % 9 + 1,
            length = body.len(),
            base = server.uri(),
            digest = sha1_of(&body),
        )
    }

    /// The same document, as CurseForge returns it for a file whose author
    /// turned third-party downloads off: everything but the address.
    fn restricted_file_document(project: u32, file: u32, relations: &str) -> String {
        let body = jar(project, file);
        format!(
            r#"{{"id":{file},"gameId":432,"modId":{project},"isAvailable":true,
                "displayName":"Release {file}","fileName":"{name}","releaseType":1,
                "fileDate":"2024-07-0{day}T10:00:00Z","fileLength":{length},
                "downloadUrl":null,
                "hashes":[{{"value":"{digest}","algo":1}}],
                "gameVersions":["1.20.1","NeoForge"],
                "dependencies":[{relations}]}}"#,
            name = file_name(project, file),
            day = file % 9 + 1,
            length = body.len(),
            digest = sha1_of(&body),
        )
    }

    /// One restricted project's file listing and the batch endpoint an
    /// exactly pinned release is fetched through.
    ///
    /// Deliberately mounts no byte endpoint for these files: nothing in the
    /// launcher may fetch them, and a request for one would 404 loudly.
    async fn serve_restricted_files(server: &MockServer, project: u32, files: &[u32]) {
        let documents: Vec<String> = files
            .iter()
            .map(|file| restricted_file_document(project, *file, ""))
            .collect();
        let total = documents.len();
        Mock::given(method("GET"))
            .and(path(format!("/v1/mods/{project}/files")))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"{{"data":[{}],"pagination":{{"totalCount":{total}}}}}"#,
                    documents.join(",")
                ),
                "application/json",
            ))
            .mount(server)
            .await;

        for file in files {
            Mock::given(method("POST"))
                .and(path("/v1/mods/files"))
                .and(body_json(serde_json::json!({ "fileIds": [file] })))
                .respond_with(ResponseTemplate::new(200).set_body_raw(
                    format!(
                        r#"{{"data":[{}]}}"#,
                        restricted_file_document(project, *file, "")
                    ),
                    "application/json",
                ))
                .mount(server)
                .await;
        }
    }

    fn project_document(project: u32, name: &str) -> String {
        format!(
            r#"{{"id":{project},"gameId":432,"name":"{name}","slug":"mod-{project}",
                "summary":"A test mod","classId":{MODS_CLASS},
                "authors":[{{"id":1,"name":"a person"}}]}}"#
        )
    }

    /// What both of the root mod's files declare: the library twice, so
    /// deduplication is exercised, and one optional extra.
    fn root_relations_for() -> String {
        format!(
            "{},{},{}",
            relation(LIBRARY_PROJECT, REQUIRED),
            relation(LIBRARY_PROJECT, REQUIRED),
            relation(OPTIONAL_PROJECT, OPTIONAL),
        )
    }

    fn relation(project: u32, kind: u32) -> String {
        format!(r#"{{"modId":{project},"relationType":{kind}}}"#)
    }

    /// Serves one project's file listing and the bytes of every file in it.
    async fn serve_files(server: &MockServer, project: u32, files: &[(u32, String)]) {
        let documents: Vec<String> = files
            .iter()
            .map(|(file, relations)| file_document(server, project, *file, relations))
            .collect();
        let total = documents.len();
        Mock::given(method("GET"))
            .and(path(format!("/v1/mods/{project}/files")))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"{{"data":[{}],"pagination":{{"totalCount":{total}}}}}"#,
                    documents.join(",")
                ),
                "application/json",
            ))
            .mount(server)
            .await;

        for (file, _) in files {
            let body = jar(project, *file);
            Mock::given(method("GET"))
                .and(path(format!(
                    "/files/{project}/{file}/{}",
                    file_name(project, *file)
                )))
                .respond_with(
                    ResponseTemplate::new(200).set_body_raw(body, "application/java-archive"),
                )
                .mount(server)
                .await;
        }
    }

    /// A loopback CurseForge with one root mod, its library, an optional extra,
    /// and a root whose required dependency has nothing that fits.
    async fn catalog() -> MockServer {
        let server = MockServer::start().await;
        serve_categories(&server).await;
        serve_projects(&server).await;
        serve_every_project_file(&server).await;

        Mock::given(method("GET"))
            .and(path("/v1/mods/search"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"{{"data":[{}],"pagination":{{"totalCount":1}}}}"#,
                    project_document(ROOT_PROJECT, "Root Mod")
                ),
                "application/json",
            ))
            .mount(&server)
            .await;

        // Anything else this fixture did not declare is a 404 rather than a
        // hang, so a wrong request fails loudly.
        Mock::given(method("GET"))
            .and(path_regex(r"^/v1/.*$"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        server
    }

    async fn serve_categories(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path("/v1/categories"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"{{"data":[
                        {{"id":{MODS_CLASS},"gameId":432,"name":"Mods","slug":"mc-mods","isClass":true}},
                        {{"id":12,"gameId":432,"name":"Resource Packs","slug":"texture-packs","isClass":true}}
                    ]}}"#
                ),
                "application/json",
            ))
            .mount(server)
            .await;
    }

    /// Every project's file listing, the bytes behind each file, and the batch
    /// endpoint an exactly pinned release is fetched through.
    async fn serve_every_project_file(server: &MockServer) {
        // Both root files declare the library twice and the optional extra
        // once, so deduplication and optional handling are exercised by the
        // ordinary install path.
        let root_relations = root_relations_for();
        serve_files(
            server,
            ROOT_PROJECT,
            &[
                (ROOT_NEWEST, root_relations.clone()),
                (ROOT_OLDER, root_relations),
            ],
        )
        .await;
        serve_files(server, LIBRARY_PROJECT, &[(LIBRARY_FILE, String::new())]).await;
        serve_files(server, OPTIONAL_PROJECT, &[(OPTIONAL_FILE, String::new())]).await;
        serve_files(
            server,
            BLOCKED_PROJECT,
            &[(BLOCKED_FILE, relation(UNFITTING_PROJECT, REQUIRED))],
        )
        .await;
        // Nothing this instance can run.
        serve_files(server, UNFITTING_PROJECT, &[]).await;
        serve_restricted_files(
            server,
            RESTRICTED_PROJECT,
            &[RESTRICTED_NEWER, RESTRICTED_INSTALLED],
        )
        .await;

        // The batch file endpoint, one mock per exact file, because an exactly
        // pinned release asks for that file and must be answered about it.
        for (project, file, relations) in [
            (ROOT_PROJECT, ROOT_NEWEST, root_relations_for()),
            (ROOT_PROJECT, ROOT_OLDER, root_relations_for()),
            (LIBRARY_PROJECT, LIBRARY_FILE, String::new()),
            (OPTIONAL_PROJECT, OPTIONAL_FILE, String::new()),
        ] {
            Mock::given(method("POST"))
                .and(path("/v1/mods/files"))
                .and(body_json(serde_json::json!({ "fileIds": [file] })))
                .respond_with(ResponseTemplate::new(200).set_body_raw(
                    format!(
                        r#"{{"data":[{}]}}"#,
                        file_document(server, project, file, &relations)
                    ),
                    "application/json",
                ))
                .mount(server)
                .await;
        }
    }

    /// Every project document, one at a time and in the batch a finished plan
    /// asks for.
    async fn serve_projects(server: &MockServer) {
        const PROJECTS: &[(u32, &str)] = &[
            (ROOT_PROJECT, "Root Mod"),
            (LIBRARY_PROJECT, "Library"),
            (OPTIONAL_PROJECT, "Optional Extra"),
            (UNFITTING_PROJECT, "Absent Library"),
            (BLOCKED_PROJECT, "Blocked Mod"),
            (RESTRICTED_PROJECT, "Restricted Mod"),
        ];

        for (project, name) in PROJECTS {
            Mock::given(method("GET"))
                .and(path(format!("/v1/mods/{project}")))
                .respond_with(ResponseTemplate::new(200).set_body_raw(
                    format!(r#"{{"data":{}}}"#, project_document(*project, name)),
                    "application/json",
                ))
                .mount(server)
                .await;
        }

        // The batch endpoint the resolver finishes a plan with.
        let batch: Vec<String> = PROJECTS
            .iter()
            .map(|(project, name)| project_document(*project, name))
            .collect();
        Mock::given(method("POST"))
            .and(path("/v1/mods"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!(r#"{{"data":[{}]}}"#, batch.join(",")),
                "application/json",
            ))
            .mount(server)
            .await;
    }

    fn store_with_key() -> Arc<dyn ephervex_auth::CredentialStore> {
        let store = MemoryStore::new();
        ephervex_auth::set_curseforge_api_key(&store, &SecretString::new(FAKE_KEY))
            .expect("store the key");
        Arc::new(store)
    }

    fn app_against(base: &Path, server: &MockServer) -> App {
        let mut app = App::open_at(Paths::rooted_at(base)).expect("app opens");
        app.use_loopback_curseforge(&format!("{}/v1", server.uri()), store_with_key())
            .expect("a loopback client");
        app
    }

    fn neoforge_instance(app: &App) -> ephervex_model::InstanceId {
        app.create_client_for_test(NewClient::new(
            "CurseForge repro",
            "1.20.1",
            LoaderSelection::exact(LoaderKind::NeoForge, "47.1.106"),
        ))
        .expect("create")
        .id
    }

    fn reporter() -> OperationReporter {
        OperationReporter::silent("curseforge-browser-test")
    }

    fn request(project: u32) -> ModInstallRequest {
        ModInstallRequest::new(project.to_string()).from_provider(ModProvider::CurseForge)
    }

    /// Every path under a directory, so a new namespace anywhere in it shows up.
    fn entries(root: &Path) -> Vec<std::path::PathBuf> {
        fn walk(directory: &Path, found: &mut Vec<std::path::PathBuf>) {
            let Ok(entries) = std::fs::read_dir(directory) else {
                return;
            };
            for entry in entries.filter_map(Result::ok) {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, found);
                }
                found.push(path);
            }
        }
        let mut found = Vec::new();
        walk(root, &mut found);
        found.sort();
        found
    }

    #[tokio::test]
    async fn a_search_offers_the_catalogs_mods_against_this_instances_exact_target() {
        let server = catalog().await;
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_against(base.path(), &server);
        let instance = neoforge_instance(&app);

        let results = app
            .search_mods(
                instance,
                ModProvider::CurseForge,
                &ModSearchQuery::new("root"),
            )
            .await
            .expect("a search");
        assert_eq!(results.provider, ModProvider::CurseForge);
        assert_eq!(results.hits.len(), 1);
        let hit = &results.hits[0];
        assert_eq!(hit.project.id, ROOT_PROJECT.to_string());
        assert_eq!(
            hit.best_version.as_ref().map(|version| version.id.as_str()),
            Some(ROOT_NEWEST.to_string().as_str())
        );
    }

    #[tokio::test]
    async fn browsing_targets_the_committed_selection_while_the_index_row_lags() {
        let server = catalog().await;
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_against(base.path(), &server);
        let instance = app
            .create_client_for_test(NewClient::new(
                "Lagging",
                "1.20.1",
                LoaderSelection::exact(LoaderKind::Forge, "47.1.106"),
            ))
            .expect("create")
            .id;
        app.commit_client_without_index_for_test(instance, |committed| {
            committed.loader = LoaderSelection::exact(LoaderKind::NeoForge, "47.1.106");
        })
        .expect("commit");
        assert_eq!(
            app.client(instance).expect("row").loader.kind,
            LoaderKind::Forge
        );

        let results = app
            .search_mods(
                instance,
                ModProvider::CurseForge,
                &ModSearchQuery::new("root"),
            )
            .await
            .expect("a search");
        assert_eq!(results.hits.len(), 1, "the NeoForge build fits");
        let details = app
            .mod_project(
                instance,
                ModProvider::CurseForge,
                &ROOT_PROJECT.to_string(),
                false,
            )
            .await
            .expect("project details");
        assert_eq!(
            details.default_version.as_deref(),
            Some(ROOT_NEWEST.to_string().as_str())
        );
        let plan = app
            .plan_mod_installation(instance, &request(ROOT_PROJECT))
            .await
            .expect("a plan");
        assert_eq!(plan.root.version.id, ROOT_NEWEST.to_string());
    }

    #[tokio::test]
    async fn reading_the_catalog_writes_nothing_to_the_cache_directory() {
        // The published third-party API terms forbid saving what the API
        // returns. Asserted against the launcher's real cache root, so a
        // metadata cache wired in later would create its namespace inside it.
        let server = catalog().await;
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_against(base.path(), &server);
        let instance = neoforge_instance(&app);
        let cache_root = app.paths().cache_dir().to_path_buf();
        let before = entries(&cache_root);

        app.search_mods(
            instance,
            ModProvider::CurseForge,
            &ModSearchQuery::new("root"),
        )
        .await
        .expect("a search");
        app.mod_project(
            instance,
            ModProvider::CurseForge,
            &ROOT_PROJECT.to_string(),
            false,
        )
        .await
        .expect("project details");
        app.plan_mod_installation(instance, &request(ROOT_PROJECT))
            .await
            .expect("a plan");

        assert_eq!(
            entries(&cache_root),
            before,
            "a CurseForge API response was persisted"
        );
    }

    #[tokio::test]
    async fn an_exactly_chosen_file_is_installed_rather_than_the_newest() {
        let server = catalog().await;
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_against(base.path(), &server);
        let instance = neoforge_instance(&app);

        let installed = app
            .install_mods(
                instance,
                &request(ROOT_PROJECT).at_version(ROOT_OLDER.to_string()),
                &reporter(),
                &CancellationToken::new(),
            )
            .await
            .expect("an install");
        assert_eq!(installed.plan.root.version.id, ROOT_OLDER.to_string());

        let managed = app.managed_mods(instance).expect("managed mods");
        let root = managed
            .iter()
            .find(|entry| entry.project_id == ROOT_PROJECT.to_string())
            .expect("the root mod");
        assert_eq!(root.version_id, ROOT_OLDER.to_string());
    }

    #[tokio::test]
    async fn an_installed_mod_retains_the_exact_curseforge_identity_it_came_from() {
        let server = catalog().await;
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_against(base.path(), &server);
        let instance = neoforge_instance(&app);

        app.install_mods(
            instance,
            &request(ROOT_PROJECT),
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect("an install");

        let recipe = app
            .pack_recipe(instance)
            .expect("a recipe")
            .expect("one was written");
        let entry = recipe
            .managed_project(ModProvider::CurseForge, &ROOT_PROJECT.to_string())
            .expect("the root mod");
        let identity = entry.identity.as_ref().expect("an identity");
        assert_eq!(identity.provider, Some(ModProvider::CurseForge));
        assert_eq!(
            identity.project_id.as_deref(),
            Some(ROOT_PROJECT.to_string().as_str())
        );
        // CurseForge's release identifier is its file id, and both fields carry
        // it so a reconstruction needs no second metadata query.
        assert_eq!(
            identity.version_id.as_deref(),
            Some(ROOT_NEWEST.to_string().as_str())
        );
        assert_eq!(
            identity.file_id.as_deref(),
            Some(ROOT_NEWEST.to_string().as_str())
        );
        assert_eq!(
            entry.hashes[0].digest(),
            sha1_of(&jar(ROOT_PROJECT, ROOT_NEWEST))
        );
        assert_eq!(
            entry.logical_path,
            std::path::PathBuf::from("mods").join(file_name(ROOT_PROJECT, ROOT_NEWEST))
        );
        assert!(
            entry
                .downloads
                .iter()
                .any(|url| url.starts_with(&server.uri()))
        );

        // The jar is actually in the instance, verified against that digest.
        let record = app.client(instance).expect("the instance");
        let game_dir = ephervex_storage::metadata::client_game_dir(&record.root);
        assert!(game_dir.join(&entry.logical_path).is_file());
    }

    #[tokio::test]
    async fn a_required_dependency_resolves_to_an_exact_compatible_file() {
        let server = catalog().await;
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_against(base.path(), &server);
        let instance = neoforge_instance(&app);

        let plan = app
            .plan_mod_installation(instance, &request(ROOT_PROJECT))
            .await
            .expect("a plan");
        assert_eq!(plan.dependencies.len(), 1, "{:?}", plan.dependencies);
        let library = &plan.dependencies[0];
        assert_eq!(library.version.project_id, LIBRARY_PROJECT.to_string());
        assert_eq!(library.version.id, LIBRARY_FILE.to_string());
    }

    #[tokio::test]
    async fn a_dependency_named_twice_is_planned_once() {
        // Both root files declare the library twice; the graph is keyed by
        // project, so it appears once and is installed once.
        let server = catalog().await;
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_against(base.path(), &server);
        let instance = neoforge_instance(&app);

        let installed = app
            .install_mods(
                instance,
                &request(ROOT_PROJECT),
                &reporter(),
                &CancellationToken::new(),
            )
            .await
            .expect("an install");
        let library_entries = installed
            .plan
            .all_mods()
            .into_iter()
            .filter(|planned| planned.version.project_id == LIBRARY_PROJECT.to_string())
            .count();
        assert_eq!(library_entries, 1);

        let managed = app.managed_mods(instance).expect("managed mods");
        assert_eq!(
            managed
                .iter()
                .filter(|entry| entry.project_id == LIBRARY_PROJECT.to_string())
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn an_optional_dependency_is_offered_rather_than_installed() {
        let server = catalog().await;
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_against(base.path(), &server);
        let instance = neoforge_instance(&app);

        let installed = app
            .install_mods(
                instance,
                &request(ROOT_PROJECT),
                &reporter(),
                &CancellationToken::new(),
            )
            .await
            .expect("an install");
        assert!(
            installed
                .plan
                .optional
                .iter()
                .any(|candidate| candidate.project_id.as_deref()
                    == Some(OPTIONAL_PROJECT.to_string().as_str())),
            "the optional extra should be offered"
        );
        let managed = app.managed_mods(instance).expect("managed mods");
        assert!(
            !managed
                .iter()
                .any(|entry| entry.project_id == OPTIONAL_PROJECT.to_string()),
            "an optional dependency must not install itself"
        );
    }

    #[tokio::test]
    async fn a_required_dependency_with_no_compatible_file_fails_the_whole_install() {
        let server = catalog().await;
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_against(base.path(), &server);
        let instance = neoforge_instance(&app);

        let error = app
            .install_mods(
                instance,
                &request(BLOCKED_PROJECT),
                &reporter(),
                &CancellationToken::new(),
            )
            .await
            .expect_err("nothing fits");
        assert!(
            matches!(
                error,
                crate::error::CoreError::ModPlatform(ModPlatformError::NoCompatibleVersion {
                    ref project_id,
                    ..
                }) if project_id == &UNFITTING_PROJECT.to_string()
            ),
            "{error:?}"
        );
        // Atomic: the root the user asked for is not installed on its own.
        assert!(app.managed_mods(instance).expect("managed mods").is_empty());
    }

    #[tokio::test]
    async fn one_catalogs_installed_mods_are_not_offered_to_another_as_already_present() {
        // Project ids belong to the catalog they were read from, so a Modrinth
        // browse must not believe a CurseForge project of the same name is
        // already installed.
        let server = catalog().await;
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_against(base.path(), &server);
        let instance = neoforge_instance(&app);

        app.install_mods(
            instance,
            &request(ROOT_PROJECT),
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect("an install");

        let recipe = app.pack_recipe(instance).expect("a recipe");
        assert!(
            crate::mods::installed_projects_of(recipe.as_ref(), ModProvider::Modrinth).is_empty()
        );
        assert_eq!(
            crate::mods::installed_projects_of(recipe.as_ref(), ModProvider::CurseForge).len(),
            2
        );
    }

    #[tokio::test]
    async fn browsing_without_a_key_reports_the_missing_credential() {
        let server = catalog().await;
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = App::open_at(Paths::rooted_at(base.path())).expect("app opens");
        app.use_loopback_curseforge(
            &format!("{}/v1", server.uri()),
            Arc::new(MemoryStore::new()),
        )
        .expect("a loopback client");
        let instance = neoforge_instance(&app);

        let error = app
            .search_mods(
                instance,
                ModProvider::CurseForge,
                &ModSearchQuery::new("root"),
            )
            .await
            .expect_err("no key is configured");
        assert!(
            matches!(
                error,
                crate::error::CoreError::ModPlatform(ModPlatformError::CredentialMissing {
                    provider: ModProvider::CurseForge
                })
            ),
            "{error:?}"
        );
    }

    // -- releases whose author forbids third-party downloads ---------------

    /// Where the shared artifact cache keeps one file's verified bytes.
    fn cache_path(app: &App, project: u32, file: u32) -> std::path::PathBuf {
        let hash = ephervex_model::Hash::new(
            ephervex_model::HashAlgorithm::Sha1,
            &sha1_of(&jar(project, file)),
        )
        .expect("valid sha1");
        let extension = ephervex_download::FileExtension::new("jar").expect("jar");
        app.artifact_cache().layout().content(
            ephervex_download::ContentStore::Downloads,
            &hash,
            Some(&extension),
        )
    }

    /// Puts one restricted file's bytes in the shared artifact cache, the way
    /// a folder watcher or an earlier import would have.
    fn seed_cache(app: &App, project: u32, file: u32) {
        let path = cache_path(app, project, file);
        std::fs::create_dir_all(path.parent().expect("shard")).expect("shard");
        std::fs::write(&path, jar(project, file)).expect("seed the cache");
    }

    /// An instance with the restricted mod installed at
    /// [`RESTRICTED_INSTALLED`], by way of a copy already in the cache.
    async fn instance_with_the_restricted_mod(app: &App) -> ephervex_model::InstanceId {
        let instance = neoforge_instance(app);
        seed_cache(app, RESTRICTED_PROJECT, RESTRICTED_INSTALLED);
        app.install_mods(
            instance,
            &request(RESTRICTED_PROJECT).at_version(RESTRICTED_INSTALLED.to_string()),
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect("a cached restricted release installs the ordinary way");
        instance
    }

    /// Every artifact path the loopback server was asked for.
    async fn artifact_requests(server: &MockServer) -> Vec<String> {
        server
            .received_requests()
            .await
            .expect("recorded")
            .iter()
            .map(|request| request.url.path().to_owned())
            .filter(|path| path.starts_with("/files/"))
            .collect()
    }

    /// A server created on its own: installed, stopped, linked to nothing.
    fn standalone_server(app: &App) -> ephervex_model::ServerId {
        let mut record = app
            .registry_for_services()
            .prepare_server(ephervex_storage::NewServer::new(
                "CurseForge server",
                "1.20.1",
                LoaderSelection::exact(LoaderKind::NeoForge, "47.1.106"),
            ))
            .expect("prepare");
        record.installation = ephervex_model::ServerInstallation::Installed {
            launch: ephervex_model::ServerLaunch::Jar {
                path: ephervex_model::ServerRelativePath::new("server.jar").expect("launch path"),
            },
        };
        let staging = record.root.with_extension("staging");
        std::fs::create_dir_all(ephervex_storage::metadata::server_game_dir(&staging).join("mods"))
            .expect("mods dir");
        app.registry_for_services()
            .publish_staged_server(&record, &staging)
            .expect("publish");
        record.id
    }

    fn server_jars(app: &App, server: ephervex_model::ServerId) -> Vec<String> {
        let root = app.server(server).expect("server").root;
        let mut names: Vec<String> =
            std::fs::read_dir(ephervex_storage::metadata::server_game_dir(&root).join("mods"))
                .expect("mods dir")
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect();
        names.sort();
        names
    }

    /// The catalog states no server side for these fixtures, so installing
    /// them on a server is the caller's confirmed choice.
    fn confirmed(request: ModInstallRequest) -> ModInstallRequest {
        request.confirming_server_support([
            ROOT_PROJECT.to_string(),
            LIBRARY_PROJECT.to_string(),
            RESTRICTED_PROJECT.to_string(),
        ])
    }

    #[tokio::test]
    async fn a_server_installs_a_curseforge_mod_and_its_dependency_with_no_linked_client() {
        let server = catalog().await;
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_against(base.path(), &server);
        let target = standalone_server(&app);

        app.install_mods(
            crate::mods::InstalledHost::Server(target),
            &confirmed(request(ROOT_PROJECT).at_version(ROOT_NEWEST.to_string())),
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect("an install onto the server");

        assert_eq!(
            server_jars(&app, target),
            [
                file_name(ROOT_PROJECT, ROOT_NEWEST),
                file_name(LIBRARY_PROJECT, LIBRARY_FILE)
            ]
        );
        let root = app.server(target).expect("server").root;
        let recipe = ephervex_pack::SourceStore::new(ephervex_storage::metadata::source_dir(&root))
            .read_recipe()
            .expect("the server's own recipe");
        for project in [ROOT_PROJECT, LIBRARY_PROJECT] {
            let entry = recipe
                .managed_project(ModProvider::CurseForge, &project.to_string())
                .expect("recorded");
            assert!(entry.is_installed_on(
                ephervex_pack::PackSide::Server,
                &ephervex_pack::OptionalSelection::none()
            ));
        }
    }

    #[tokio::test]
    async fn a_restricted_file_on_a_server_asks_for_a_manual_download_with_the_catalogs_sides() {
        let server = catalog().await;
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_against(base.path(), &server);
        let target = standalone_server(&app);

        let error = app
            .install_mods(
                crate::mods::InstalledHost::Server(target),
                &confirmed(request(RESTRICTED_PROJECT).at_version(RESTRICTED_NEWER.to_string())),
                &reporter(),
                &CancellationToken::new(),
            )
            .await
            .expect_err("the bytes are nowhere yet");

        let crate::error::CoreError::ModAcquisitionRestricted { blocked, .. } = &error else {
            panic!("a dead-end provider failure instead of the manual path: {error:?}");
        };
        assert_eq!(blocked.len(), 1);
        assert_eq!(
            blocked[0].client,
            ephervex_pack::SideRequirement::Unknown,
            "a server install does not claim the client requires the mod"
        );
        assert_eq!(blocked[0].server, ephervex_pack::SideRequirement::Unknown);
        assert!(blocked[0].browser_url.is_some());
        assert!(artifact_requests(&server).await.is_empty());
        assert!(server_jars(&app, target).is_empty());
    }

    #[tokio::test]
    async fn a_cached_restricted_release_installs_on_a_server_without_a_browser() {
        let server = catalog().await;
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_against(base.path(), &server);
        let target = standalone_server(&app);
        seed_cache(&app, RESTRICTED_PROJECT, RESTRICTED_NEWER);

        app.install_mods(
            crate::mods::InstalledHost::Server(target),
            &confirmed(request(RESTRICTED_PROJECT).at_version(RESTRICTED_NEWER.to_string())),
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect("a verified cached copy is used");

        assert_eq!(
            server_jars(&app, target),
            [file_name(RESTRICTED_PROJECT, RESTRICTED_NEWER)]
        );
        assert!(artifact_requests(&server).await.is_empty());
    }

    #[tokio::test]
    async fn a_restricted_project_manages_like_any_other_mod() {
        let server = catalog().await;
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_against(base.path(), &server);
        let instance = instance_with_the_restricted_mod(&app).await;

        let choices = app
            .managed_mod_versions(
                instance,
                &std::path::PathBuf::from("mods")
                    .join(file_name(RESTRICTED_PROJECT, RESTRICTED_INSTALLED)),
                &reporter(),
                &CancellationToken::new(),
            )
            .await
            .expect("the version picker still has something to show");

        assert_eq!(choices.project.title, "Restricted Mod");
        assert_eq!(
            choices.project_url().as_deref(),
            Some(
                format!("https://www.curseforge.com/minecraft/mc-mods/mod-{RESTRICTED_PROJECT}")
                    .as_str()
            ),
            "the project browser link stays available"
        );
        let listed: Vec<&str> = choices
            .compatible
            .iter()
            .map(|annotated| annotated.version.id.as_str())
            .collect();
        assert_eq!(
            listed,
            vec![
                RESTRICTED_NEWER.to_string().as_str(),
                RESTRICTED_INSTALLED.to_string().as_str(),
            ],
            "every compatible release is offered, newest first"
        );
        assert_eq!(
            choices.installed_version_id.as_deref(),
            Some(RESTRICTED_INSTALLED.to_string().as_str())
        );
        assert_eq!(
            choices.newest_compatible.as_deref(),
            Some(RESTRICTED_NEWER.to_string().as_str())
        );
        assert!(choices.mutability.can_change_version);
        assert!(choices.mutability.can_update);
        assert!(
            choices.compatible.iter().all(|annotated| annotated
                .version
                .file
                .distribution
                .is_restricted()),
            "the restriction is stated on the release, not by hiding it"
        );
        assert!(
            choices
                .compatible
                .iter()
                .all(|annotated| !annotated.version.file.hashes.is_empty()),
            "the digest a hand-placed copy is recognized by is kept"
        );
    }

    #[tokio::test]
    async fn a_cached_restricted_release_is_installed_without_a_browser() {
        let server = catalog().await;
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_against(base.path(), &server);
        let instance = instance_with_the_restricted_mod(&app).await;
        // The exact release the user is about to pick, already verified on
        // this machine. The restriction is about downloading, not about using
        // bytes Ephervex legitimately holds.
        seed_cache(&app, RESTRICTED_PROJECT, RESTRICTED_NEWER);

        let mutation = app
            .change_managed_mod_version(
                instance,
                &std::path::PathBuf::from("mods")
                    .join(file_name(RESTRICTED_PROJECT, RESTRICTED_INSTALLED)),
                &crate::mutation::ModVersionChoice::Exact(RESTRICTED_NEWER.to_string()),
                &reporter(),
                &CancellationToken::new(),
            )
            .await
            .expect("a cached restricted release needs no browser");

        assert!(matches!(
            mutation.plan.outcome,
            crate::mutation::PlannedOutcome::ChangeVersion { .. }
        ));
        let recipe = app
            .pack_recipe(instance)
            .expect("a recipe")
            .expect("one was written");
        let entry = recipe
            .managed_project(ModProvider::CurseForge, &RESTRICTED_PROJECT.to_string())
            .expect("the restricted mod");
        assert_eq!(
            entry
                .identity
                .as_ref()
                .and_then(|id| id.version_id.as_deref()),
            Some(RESTRICTED_NEWER.to_string().as_str())
        );
        assert!(
            artifact_requests(&server).await.is_empty(),
            "nothing may fetch a file the author forbids third-party downloads of"
        );
    }

    #[tokio::test]
    async fn an_uncached_restricted_release_asks_for_a_browser_download() {
        let server = catalog().await;
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_against(base.path(), &server);
        let instance = instance_with_the_restricted_mod(&app).await;

        let error = app
            .change_managed_mod_version(
                instance,
                &std::path::PathBuf::from("mods")
                    .join(file_name(RESTRICTED_PROJECT, RESTRICTED_INSTALLED)),
                &crate::mutation::ModVersionChoice::Exact(RESTRICTED_NEWER.to_string()),
                &reporter(),
                &CancellationToken::new(),
            )
            .await
            .expect_err("the bytes are nowhere yet");

        let crate::error::CoreError::ModAcquisitionRestricted { blocked, .. } = &error else {
            panic!("a dead-end provider failure instead of the manual path: {error:?}");
        };
        assert_eq!(blocked.len(), 1);
        let blocked = &blocked[0];
        assert_eq!(
            blocked.identity.file_id.as_deref(),
            Some(RESTRICTED_NEWER.to_string().as_str())
        );
        assert_eq!(blocked.project_name.as_deref(), Some("Restricted Mod"));
        assert_eq!(
            blocked.file_name.as_deref(),
            Some(file_name(RESTRICTED_PROJECT, RESTRICTED_NEWER).as_str())
        );
        assert_eq!(
            blocked.browser_url.as_deref(),
            Some(
                format!(
                    "https://www.curseforge.com/minecraft/mc-mods/mod-{RESTRICTED_PROJECT}/download/{RESTRICTED_NEWER}"
                )
                .as_str()
            ),
            "the provider's own page for the file, never an assembled delivery address"
        );
        assert!(
            !blocked.hashes.is_empty(),
            "a hand-placed copy has to be recognizable"
        );
        assert!(
            artifact_requests(&server).await.is_empty(),
            "no download was attempted or synthesized"
        );

        // The instance is untouched: the release it was on is still the one
        // it is on.
        let recipe = app
            .pack_recipe(instance)
            .expect("a recipe")
            .expect("one was written");
        let entry = recipe
            .managed_project(ModProvider::CurseForge, &RESTRICTED_PROJECT.to_string())
            .expect("the restricted mod");
        assert_eq!(
            entry
                .identity
                .as_ref()
                .and_then(|id| id.version_id.as_deref()),
            Some(RESTRICTED_INSTALLED.to_string().as_str())
        );
    }

    #[tokio::test]
    async fn a_manually_acquired_release_is_verified_before_the_update_continues() {
        let server = catalog().await;
        let base = tempfile::tempdir().expect("temp dir");
        let downloads = base.path().join("manual-downloads");
        std::fs::create_dir_all(&downloads).expect("a folder to watch");
        let mut app = app_against(base.path(), &server);
        app.set_watched_download_folders(Some(std::slice::from_ref(&downloads)))
            .expect("watch the folder");
        let instance = instance_with_the_restricted_mod(&app).await;
        let target = std::path::PathBuf::from("mods")
            .join(file_name(RESTRICTED_PROJECT, RESTRICTED_INSTALLED));
        let choice = crate::mutation::ModVersionChoice::Exact(RESTRICTED_NEWER.to_string());

        let error = app
            .change_managed_mod_version(
                instance,
                &target,
                &choice,
                &reporter(),
                &CancellationToken::new(),
            )
            .await
            .expect_err("nothing has been acquired yet");
        let crate::error::CoreError::ModAcquisitionRestricted { blocked, .. } = error else {
            panic!("expected the manual path");
        };
        let mut watcher = app.download_watcher(
            blocked
                .iter()
                .map(crate::download_watch::WatchTarget::from_blocked)
                .collect(),
        );

        // Something with the right name and the wrong bytes is not the file.
        std::fs::write(
            downloads.join(file_name(RESTRICTED_PROJECT, RESTRICTED_NEWER)),
            b"not the release, whatever it is called",
        )
        .expect("write an impostor");
        assert!(
            watcher.scan_once().await.is_none(),
            "identity is proven by digest, never by file name"
        );
        app.change_managed_mod_version(
            instance,
            &target,
            &choice,
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect_err("an unverified file changes nothing");

        // The real thing, placed by hand.
        std::fs::write(
            downloads.join(file_name(RESTRICTED_PROJECT, RESTRICTED_NEWER)),
            jar(RESTRICTED_PROJECT, RESTRICTED_NEWER),
        )
        .expect("write the real release");
        let matched = watcher.scan_once().await.expect("the watcher adopts it");
        assert_eq!(
            matched.file_id.as_deref(),
            Some(RESTRICTED_NEWER.to_string().as_str())
        );

        app.change_managed_mod_version(
            instance,
            &target,
            &choice,
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect("the requested version change goes through the ordinary update path");

        let recipe = app
            .pack_recipe(instance)
            .expect("a recipe")
            .expect("one was written");
        let entry = recipe
            .managed_project(ModProvider::CurseForge, &RESTRICTED_PROJECT.to_string())
            .expect("the restricted mod");
        assert_eq!(
            entry
                .identity
                .as_ref()
                .and_then(|id| id.version_id.as_deref()),
            Some(RESTRICTED_NEWER.to_string().as_str())
        );
        assert!(
            artifact_requests(&server).await.is_empty(),
            "the bytes came from the user, not from a download"
        );
    }

    #[tokio::test]
    async fn an_unrestricted_mod_still_downloads_the_ordinary_way() {
        let server = catalog().await;
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_against(base.path(), &server);
        let instance = neoforge_instance(&app);

        app.install_mods(
            instance,
            &request(ROOT_PROJECT).at_version(ROOT_OLDER.to_string()),
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect("an install");
        app.change_managed_mod_version(
            instance,
            &std::path::PathBuf::from("mods").join(file_name(ROOT_PROJECT, ROOT_OLDER)),
            &crate::mutation::ModVersionChoice::Exact(ROOT_NEWEST.to_string()),
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect("a mod nobody restricted updates without a browser");

        assert!(
            artifact_requests(&server)
                .await
                .iter()
                .any(|path| path.contains(&ROOT_NEWEST.to_string())),
            "an unrestricted release is still fetched over the network"
        );
    }

    /// Putting a deleted jar back needs its bytes just as an install does.
    /// The launcher may not fetch them, so it asks - and once the user has
    /// supplied them, the ordinary restore finishes.
    #[tokio::test]
    async fn restoring_a_restricted_jar_asks_for_it_rather_than_failing() {
        let server = catalog().await;
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_against(base.path(), &server);
        let instance = instance_with_the_restricted_mod(&app).await;
        let host = crate::mods::InstalledHost::Instance(instance);
        let logical = std::path::PathBuf::from("mods")
            .join(file_name(RESTRICTED_PROJECT, RESTRICTED_INSTALLED));

        // The jar is put aside to find out which mod crashes, and the cached
        // copy is gone too - a pruned cache, or another machine.
        app.delete_mod_file(host, &logical).expect("the jar goes");
        std::fs::remove_file(cache_path(&app, RESTRICTED_PROJECT, RESTRICTED_INSTALLED))
            .expect("empty the cache");

        let error = app
            .restore_mod_file(host, &logical, &reporter(), &CancellationToken::new())
            .await
            .expect_err("there are no bytes to put back");
        let crate::error::CoreError::ModAcquisitionRestricted { blocked, .. } = &error else {
            panic!("a bare missing-source failure instead of the manual path: {error:?}");
        };
        assert_eq!(blocked.len(), 1);
        assert_eq!(
            blocked[0].browser_url.as_deref(),
            Some(
                format!(
                    "https://www.curseforge.com/minecraft/mc-mods/mod-{RESTRICTED_PROJECT}/download/{RESTRICTED_INSTALLED}"
                )
                .as_str()
            )
        );
        assert_eq!(blocked[0].project_name.as_deref(), Some("Restricted Mod"));
        assert!(!blocked[0].hashes.is_empty());

        // The user fetched it; a watcher adopted it into the shared cache.
        seed_cache(&app, RESTRICTED_PROJECT, RESTRICTED_INSTALLED);
        app.restore_mod_file(host, &logical, &reporter(), &CancellationToken::new())
            .await
            .expect("the ordinary restore finishes once the bytes are here");

        let record = app.client(instance).expect("the instance");
        let restored = ephervex_storage::metadata::client_game_dir(&record.root).join(&logical);
        assert!(restored.is_file(), "{} is not back", restored.display());
        assert!(
            artifact_requests(&server).await.is_empty(),
            "nothing may fetch a file the author forbids third-party downloads of"
        );
    }

    /// A pack file whose mirror simply went away is not an author's decision,
    /// and pointing the user at a download page for it would be a guess.
    #[tokio::test]
    async fn a_recorded_file_with_no_catalog_identity_still_fails_plainly() {
        let server = catalog().await;
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_against(base.path(), &server);
        let instance = instance_with_the_restricted_mod(&app).await;
        let host = crate::mods::InstalledHost::Instance(instance);
        let logical = std::path::PathBuf::from("mods")
            .join(file_name(RESTRICTED_PROJECT, RESTRICTED_INSTALLED));

        // Strip the entry's identity, leaving a recorded file nothing can name.
        let record = app.client(instance).expect("the instance");
        let store =
            ephervex_pack::SourceStore::new(ephervex_storage::metadata::source_dir(&record.root));
        let mut recipe = store.read_recipe().expect("a recipe");
        for file in &mut recipe.files {
            file.identity = None;
        }
        store.write_recipe(&recipe).expect("rewrite");

        app.delete_mod_file(host, &logical).expect("the jar goes");
        std::fs::remove_file(cache_path(&app, RESTRICTED_PROJECT, RESTRICTED_INSTALLED))
            .expect("empty the cache");

        let error = app
            .restore_mod_file(host, &logical, &reporter(), &CancellationToken::new())
            .await
            .expect_err("there are no bytes to put back");
        assert!(
            matches!(
                error,
                crate::error::CoreError::Pack(
                    ephervex_pack::PackError::FileDownloadsMissing { .. }
                )
            ),
            "a lost source stays the missing source it is, never an author's decision: {error:?}"
        );
    }

    // -- whose mods an instance's are ---------------------------------------

    /// Rewrites every recorded mod as one the pack declared, which is what an
    /// imported instance's recipe looks like.
    fn make_pack_declared(app: &App, instance: ephervex_model::InstanceId) {
        let record = app.client(instance).expect("the instance");
        let store =
            ephervex_pack::SourceStore::new(ephervex_storage::metadata::source_dir(&record.root));
        let mut recipe = store.read_recipe().expect("a recipe");
        for file in &mut recipe.files {
            file.origin = ephervex_model::ModOrigin::PackDeclared;
        }
        store.write_recipe(&recipe).expect("rewrite");
    }

    #[tokio::test]
    async fn a_pack_declared_mod_leaves_only_once_the_instance_says_its_mods_are_the_users() {
        let server = catalog().await;
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_against(base.path(), &server);
        let instance = neoforge_instance(&app);
        app.install_mods(
            instance,
            &request(ROOT_PROJECT).at_version(ROOT_OLDER.to_string()),
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect("an install");
        make_pack_declared(&app, instance);
        let target = std::path::PathBuf::from("mods").join(file_name(ROOT_PROJECT, ROOT_OLDER));

        // An imported instance preserves what the pack declared, so the mod is
        // not the user's to take out of it.
        app.set_allow_pack_mod_version_changes(instance, false)
            .expect("preserve the declaration");
        let error = app
            .plan_mod_removal(instance, &target, &reporter(), &CancellationToken::new())
            .await
            .expect_err("the pack's declaration is a record");
        assert!(
            matches!(
                error,
                crate::error::CoreError::ModMutationRestricted {
                    restriction: crate::mutation::MutationRestriction::PackOwned,
                    ..
                }
            ),
            "{error:?}"
        );

        // Said otherwise, moving it and taking it away are both the user's.
        app.set_allow_pack_mod_version_changes(instance, true)
            .expect("the mods are the user's");
        let plan = app
            .plan_mod_removal(instance, &target, &reporter(), &CancellationToken::new())
            .await
            .expect("the mod may leave");
        assert_eq!(plan.outcome, crate::mutation::PlannedOutcome::Remove);

        app.remove_managed_mod(instance, &target, &reporter(), &CancellationToken::new())
            .await
            .expect("and it does");
        let recipe = app
            .pack_recipe(instance)
            .expect("a recipe")
            .expect("one was written");
        assert!(
            recipe
                .managed_project(ModProvider::CurseForge, &ROOT_PROJECT.to_string())
                .is_none(),
            "nothing is left to put back, which is what makes the bin final"
        );
    }
}
