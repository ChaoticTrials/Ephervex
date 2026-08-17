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
//! [`crate::ApiSettings::curseforge_api_key_present`] reports its presence;
//! there is no path from a frontend to the value, by construction rather than
//! by convention.

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
        })
    }
}

impl CurseForgeCredentialSource for StoredApiKey {
    fn api_key(&self) -> ModPlatformResult<Option<SecretString>> {
        let store: Arc<dyn CredentialStore> = match &self.0 {
            Source::Keyring => credential_store().map_err(|error| unreadable(&error))?,
            Source::Explicit(store) => Arc::clone(store),
        };
        ephervex_auth::curseforge_api_key(store.as_ref()).map_err(|error| unreadable(&error))
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
    /// require one to exist — a request without a key fails with
    /// [`ModPlatformError::CredentialMissing`] before anything is sent. So this
    /// is safe to call on a machine that has never configured CurseForge.
    pub fn curseforge_api(&self) -> CoreResult<&CurseForgeApi> {
        if let Some(api) = self.curseforge.get() {
            return Ok(api);
        }
        let api = CurseForgeApi::new(Arc::new(StoredApiKey::in_keyring()), &self.http_config())?;
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
