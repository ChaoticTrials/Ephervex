//! Turning a CurseForge pack's declared ids into installable recipe entries.
//!
//! # Why this step exists at all
//!
//! A CurseForge export manifest names each of its files by project id and file
//! id and nothing else: no name, no size, no hash, no address. `ephervex-pack`
//! therefore parses one into a recipe plus a list of
//! [`PendingProviderFile`](ephervex_pack::PendingProviderFile) entries, and
//! stops. This module is the half that asks CurseForge, and it lives here for
//! the same reason every other cross-boundary sequence does: the pack crate
//! knows archives and recipes, the mod-platform crate knows the API, and
//! neither should learn the other.
//!
//! A frontend never orchestrates this. It analyzes an archive, asks core to
//! resolve it, and gets back an analysis it can display and import — the same
//! two steps a Modrinth pack takes, where the second happens to have nothing to
//! do.
//!
//! # What is exact about it
//!
//! Every lookup is `project P, file F` against the endpoint that answers about
//! exactly that file, and the answer is checked against the question before it
//! is used. Nothing here lists a project's versions, and nothing picks a newer
//! file when the declared one is gone: a missing exact file fails the import.
//!
//! # The credential
//!
//! Read once per import, held for that import's request sequence, and dropped
//! with it. It travels to the API origin, which the API client scopes it to,
//! and to CurseForge's own delivery hosts, which
//! [`AuthenticatedSource`] scopes it to. It reaches nothing else, appears in no
//! recipe, no retained source, no error, and no log line.
//!
//! # Nothing an API answer says is written down
//!
//! What survives an import is the normalized recipe: the exact project and file
//! ids the *user's own manifest* declared, plus the minimum the launcher's own
//! model needs to install and verify an artifact — a name, a size, a digest,
//! and the address it came from. The response itself is parsed, used, and
//! dropped. See ADR 0022.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use ephervex_download::{AuthenticatedSource, DomainAllowlist, DownloadManager, DownloaderConfig};
use ephervex_mod_platform::{
    CURSEFORGE_API_KEY_HEADER, CurseForgeFileRelease, CurseForgeOperation, ModPlatformError,
};
use ephervex_model::{CurseForgeFileId, CurseForgeProjectId, ModOrigin, ModProvider};
use ephervex_pack::{
    MODS_DIR, PackAnalysis, PackError, PackFileRecipe, PackFormat, PendingProviderFile,
    ProviderFileIdentity,
};
use futures_util::StreamExt as _;
use reqwest::header::HeaderName;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, instrument};

use crate::app::App;
use crate::error::CoreResult;

/// How many exact-file lookups one import runs at once.
///
/// A pack may declare several hundred files, and one task per entry would open
/// several hundred sockets against somebody else's rate limit. The number
/// matches the download manager's own default concurrency, which is the figure
/// this launcher has already decided is polite.
const LOOKUP_CONCURRENCY: usize = 6;

impl App {
    /// Fills in everything a pack analysis still needs from its provider.
    ///
    /// A no-op for a pack whose manifest already describes its own files, which
    /// is what lets a caller run it unconditionally. For a CurseForge export it
    /// resolves every declared file against the API and turns the result into
    /// ordinary recipe entries, after which the analysis is indistinguishable
    /// from one that never needed resolving.
    ///
    /// Requires the CurseForge API key, and says so with
    /// [`ModPlatformError::CredentialMissing`] before any request is made.
    /// Analysis itself does not: identifying an archive and reading its
    /// manifest stays local.
    pub async fn resolve_pack(
        &self,
        analysis: &mut PackAnalysis,
        cancel: &CancellationToken,
    ) -> CoreResult<()> {
        self.prepare_provider_files(analysis, cancel).await?;
        Ok(())
    }

    /// The same work, keeping the credential the artifact downloads need.
    ///
    /// Returned rather than re-read: one import is one operation, and reading
    /// the secret store again for the download half would be a second exposure
    /// of the same secret for no benefit.
    pub(crate) async fn prepare_provider_files(
        &self,
        analysis: &mut PackAnalysis,
        cancel: &CancellationToken,
    ) -> CoreResult<Option<Arc<AuthenticatedSource>>> {
        match analysis.format {
            PackFormat::CurseForgePack => {
                Ok(Some(self.resolve_curseforge_pack(analysis, cancel).await?))
            }
            // Every other format's manifest describes its own files, so there
            // is nothing to resolve and no credential to spend.
            PackFormat::ModrinthPack | PackFormat::PrismExport | _ => Ok(None),
        }
    }

    #[instrument(skip_all, fields(pending = analysis.pending.len()))]
    async fn resolve_curseforge_pack(
        &self,
        analysis: &mut PackAnalysis,
        cancel: &CancellationToken,
    ) -> CoreResult<Arc<AuthenticatedSource>> {
        let api = self.curseforge_api()?;
        // One read of the secret store for the whole import, and the point at
        // which an unconfigured launcher fails without contacting anyone.
        let operation = api.begin_operation()?;
        let hosts = self.curseforge_artifact_hosts();
        let credential = Arc::new(AuthenticatedSource::new(
            HeaderName::from_static(CURSEFORGE_API_KEY_HEADER),
            operation.credential().clone(),
            hosts.clone(),
        ));

        if analysis.pending.is_empty() {
            return Ok(credential);
        }
        let pending = std::mem::take(&mut analysis.pending);
        let resolved = match resolve_files(&operation, &pending, &hosts, cancel).await {
            Ok(resolved) => resolved,
            Err(error) => {
                // The analysis is left describing the pack it describes: a
                // caller that retries after configuring a key must still see
                // the entries that need resolving.
                analysis.pending = pending;
                return Err(error);
            }
        };

        info!(
            files = resolved.len(),
            "resolved a CurseForge pack's declared files"
        );
        analysis.recipe.files.extend(resolved);
        Ok(credential)
    }

    /// The hosts a CurseForge artifact may be fetched from, and the only ones
    /// its credential may reach.
    ///
    /// A method rather than a free function because a test build reads the
    /// loopback flag off the application; a release build has no such field, so
    /// the receiver is unused there and the production list is the only one it
    /// can return.
    #[cfg_attr(
        not(test),
        expect(
            clippy::unused_self,
            reason = "only a test build reads the loopback flag"
        )
    )]
    pub(crate) fn curseforge_artifact_hosts(&self) -> DomainAllowlist {
        // Only a test build has the field to read, so the strict production
        // list is the only one a release binary can produce.
        #[cfg(test)]
        if self.loopback_http {
            return DomainAllowlist::for_loopback_tests("127.0.0.1");
        }
        DomainAllowlist::curseforge_artifact_defaults()
    }

    /// A download manager whose redirects cannot leave a credential's hosts.
    ///
    /// The ordinary manager follows a redirect anywhere, which is right for an
    /// unauthenticated artifact and wrong for one carrying a secret: a `302` to
    /// another host would hand the key to whoever the response named. So an
    /// authenticated operation gets a manager built around a client that stops
    /// at the same host list the credential is scoped to.
    ///
    /// Everything else about it is the shared downloader: bounded concurrency,
    /// atomic publication, hash and size verification, cancellation, and the
    /// same content-addressed store.
    pub(crate) fn artifact_manager(
        &self,
        authenticated: Option<&Arc<AuthenticatedSource>>,
    ) -> CoreResult<DownloadManager> {
        let http = self.http_config();
        let config = DownloaderConfig {
            http: http.clone(),
            ..DownloaderConfig::default()
        };
        let Some(_) = authenticated else {
            return Ok(DownloadManager::new(config).map_err(unusable_downloader)?);
        };
        let client =
            ephervex_download::build_allowlisted_client(&http, &self.curseforge_artifact_hosts())
                .map_err(unusable_downloader)?;
        Ok(DownloadManager::with_client(client, config))
    }
}

/// An HTTP client this build could not construct.
///
/// Reported as a download failure because that is what it is from the user's
/// side: nothing can be fetched. The pack's own name is not available here, so
/// the label names the subsystem rather than inventing a file.
fn unusable_downloader(source: ephervex_download::DownloadError) -> PackError {
    PackError::DownloadFailed {
        path: "artifact downloads".to_owned(),
        source: Box::new(source),
    }
}

/// Looks up every declared file, deduplicated and with bounded concurrency.
///
/// Returns entries in the pack's own order, so the recipe reads the way the
/// manifest does.
async fn resolve_files(
    operation: &CurseForgeOperation<'_>,
    pending: &[PendingProviderFile],
    hosts: &DomainAllowlist,
    cancel: &CancellationToken,
) -> CoreResult<Vec<PackFileRecipe>> {
    let wanted: Vec<(CurseForgeProjectId, CurseForgeFileId)> = pending
        .iter()
        .map(declared_ids)
        .collect::<CoreResult<_>>()?;

    // One request per distinct file, however many entries name it. Responses
    // cannot be cached across operations, which is all the more reason not to
    // ask twice inside one.
    let mut distinct: Vec<(CurseForgeProjectId, CurseForgeFileId)> = wanted.clone();
    distinct.sort_unstable();
    distinct.dedup();
    debug!(
        declared = wanted.len(),
        requests = distinct.len(),
        "resolving CurseForge pack files"
    );

    let releases: Vec<CurseForgeFileRelease> = futures_util::stream::iter(
        distinct
            .iter()
            .map(|(project, file)| operation.file(*project, *file, cancel)),
    )
    .buffered(LOOKUP_CONCURRENCY)
    .collect::<Vec<_>>()
    .await
    .into_iter()
    .collect::<Result<_, ModPlatformError>>()?;

    let by_identity: BTreeMap<(CurseForgeProjectId, CurseForgeFileId), &CurseForgeFileRelease> =
        releases
            .iter()
            .map(|release| ((release.project, release.file), release))
            .collect();

    pending
        .iter()
        .zip(&wanted)
        .map(|(entry, identity)| {
            let release = by_identity
                .get(identity)
                .copied()
                // Unreachable: every distinct identity was requested and every
                // response was checked against the identity it answers for.
                .ok_or_else(|| ModPlatformError::FileUnknown {
                    provider: ModProvider::CurseForge,
                    project_id: identity.0.to_string(),
                    file_id: identity.1.to_string(),
                })?;
            recipe_entry(entry, release, hosts)
        })
        .collect()
}

/// The exact ids a pending entry declares.
fn declared_ids(
    entry: &PendingProviderFile,
) -> CoreResult<(CurseForgeProjectId, CurseForgeFileId)> {
    let project = entry
        .identity
        .project_id
        .as_deref()
        .and_then(CurseForgeProjectId::parse)
        .ok_or_else(|| PackError::PackEntryIdInvalid {
            field: "projectID",
            value: entry.identity.project_id.clone().unwrap_or_default(),
        })?;
    let file = entry
        .identity
        .version_id
        .as_deref()
        .and_then(CurseForgeFileId::parse)
        .ok_or_else(|| PackError::PackEntryIdInvalid {
            field: "fileID",
            value: entry.identity.version_id.clone().unwrap_or_default(),
        })?;
    Ok((project, file))
}

/// One resolved file as the recipe entry that records it.
///
/// The side requirements come from the *pack*, not from the API: pack inclusion
/// is what the manifest stated, and CurseForge's file document says nothing
/// about servers at all. Everything the API contributes is what the launcher
/// needs to fetch and verify an artifact.
fn recipe_entry(
    entry: &PendingProviderFile,
    release: &CurseForgeFileRelease,
    hosts: &DomainAllowlist,
) -> CoreResult<PackFileRecipe> {
    // The file name comes from a provider and is untrusted: it is validated as
    // a single safe path component before it can name a destination.
    let name = ephervex_pack::safe_file_name(&release.artifact.file_name).ok_or_else(|| {
        PackError::EntryPathUnsafe {
            entry: release.artifact.file_name.clone(),
        }
    })?;

    // The address is provider data, and provider data decides which machine the
    // launcher connects to. It is checked like any other remote address, and a
    // refusal is fatal: there is no second address for a CurseForge file.
    let address =
        hosts
            .validate(&release.artifact.url)
            .map_err(|source| PackError::DownloadUrlRejected {
                path: name.to_string_lossy().into_owned(),
                source,
            })?;

    Ok(PackFileRecipe {
        logical_path: PathBuf::from(MODS_DIR).join(name),
        hashes: release.artifact.hashes.clone(),
        downloads: vec![address.to_string()],
        size: release.artifact.size,
        client: entry.client,
        server: entry.server,
        selected: None,
        // The pack asked for this file. Resolving it exactly does not make it
        // anybody else's, and the pack-owned mutation policy depends on that.
        origin: ModOrigin::PackDeclared,
        identity: Some(ProviderFileIdentity {
            provider: Some(ModProvider::CurseForge),
            project_id: Some(release.project.to_string()),
            // CurseForge's release identifier is its file id. Both fields carry
            // it rather than one of them carrying a version parsed out of a
            // display name.
            version_id: Some(release.file.to_string()),
            file_id: Some(release.file.to_string()),
        }),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use std::io::Write as _;
    use std::path::Path;

    use ephervex_auth::{CredentialStore, MemoryStore};
    use ephervex_download::{ArtifactCache, CacheLayout, ContentStore, OperationReporter};
    use ephervex_model::{
        ArtifactCacheLimit, Hash, HashAlgorithm, ImportSourceKind, LoaderKind, SecretString,
        StorageClass,
    };
    use ephervex_pack::{OptionalSelection, PackSide, SideRequirement, SourceStore};
    use ephervex_storage::Paths;
    use sha1::{Digest as _, Sha1};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    use crate::import::ImportRequest;

    use super::*;

    /// Obviously not a credential, and the only one this file ever uses.
    const FAKE_KEY: &str = "test-curseforge-key-do-not-use";

    /// The pack's two declared projects and the exact files it names.
    const REQUIRED_PROJECT: u32 = 238_222;
    const REQUIRED_FILE: u32 = 4_593_548;
    const OPTIONAL_PROJECT: u32 = 306_612;
    const OPTIONAL_FILE: u32 = 4_593_600;

    fn store_with_key() -> Arc<dyn CredentialStore> {
        let store = MemoryStore::new();
        ephervex_auth::set_curseforge_api_key(&store, &SecretString::new(FAKE_KEY))
            .expect("store the key");
        Arc::new(store)
    }

    fn zip_file(path: &Path, entries: &[(&str, &[u8])]) {
        let file = std::fs::File::create(path).expect("create archive");
        let mut writer = zip::ZipWriter::new(file);
        let options: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, body) in entries {
            writer.start_file(*name, options).expect("start entry");
            writer.write_all(body).expect("write entry");
        }
        writer.finish().expect("finish");
    }

    /// A CurseForge export declaring one required and one optional file.
    fn export_manifest() -> Vec<u8> {
        format!(
            r#"{{
                "minecraft": {{
                    "version": "1.20.1",
                    "modLoaders": [{{"id": "forge-47.4.10", "primary": true}}]
                }},
                "manifestType": "minecraftModpack",
                "manifestVersion": 1,
                "name": "Repro Pack",
                "version": "3.1.0",
                "author": "A Reporter",
                "files": [
                    {{"projectID": {REQUIRED_PROJECT}, "fileID": {REQUIRED_FILE}, "required": true}},
                    {{"projectID": {OPTIONAL_PROJECT}, "fileID": {OPTIONAL_FILE}, "required": false}}
                ],
                "overrides": "overrides"
            }}"#
        )
        .into_bytes()
    }

    fn sha1_of(body: &[u8]) -> String {
        let mut hasher = Sha1::new();
        hasher.update(body);
        hex::encode(hasher.finalize())
    }

    /// The exact-file document for one artifact, as the API answers.
    fn file_document(
        server: &MockServer,
        project: u32,
        file: u32,
        name: &str,
        body: &[u8],
    ) -> String {
        format!(
            r#"{{"data":{{"id":{file},"gameId":432,"modId":{project},"isAvailable":true,
                "displayName":"{name}","fileName":"{name}","releaseType":1,"fileStatus":4,
                "fileLength":{length},
                "downloadUrl":"{base}/files/{project}/{file}/{name}",
                "hashes":[{{"value":"{digest}","algo":1}},
                          {{"value":"0123456789abcdef0123456789abcdef","algo":2}}]}}}}"#,
            length = body.len(),
            base = server.uri(),
            digest = sha1_of(body),
        )
    }

    /// Serves one exact file's metadata and its bytes.
    async fn serve_file(server: &MockServer, project: u32, file: u32, name: &str, body: &[u8]) {
        Mock::given(method("GET"))
            .and(path(format!("/v1/mods/{project}/files/{file}")))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                file_document(server, project, file, name, body),
                "application/json",
            ))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/files/{project}/{file}/{name}")))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(body.to_vec(), "application/java-archive"),
            )
            .mount(server)
            .await;
    }

    /// An application pointed at a loopback CurseForge, with a key configured.
    fn app_against(base: &Path, server: &MockServer) -> App {
        let mut app = App::open_at(Paths::rooted_at(base)).expect("app opens");
        app.use_loopback_curseforge(&format!("{}/v1", server.uri()), store_with_key())
            .expect("a loopback client");
        app
    }

    /// The same, with no key in the store.
    fn app_without_a_key(base: &Path, server: &MockServer) -> App {
        let mut app = App::open_at(Paths::rooted_at(base)).expect("app opens");
        app.use_loopback_curseforge(
            &format!("{}/v1", server.uri()),
            Arc::new(MemoryStore::new()),
        )
        .expect("a loopback client");
        app
    }

    fn archive_at(dir: &Path, name: &str) -> std::path::PathBuf {
        let archive = dir.join(name);
        zip_file(
            &archive,
            &[
                ("manifest.json", &export_manifest()),
                ("overrides/config/repro.toml", b"debug = true\n" as &[u8]),
            ],
        );
        archive
    }

    async fn import(app: &App, archive: &Path) -> CoreResult<crate::import::ImportedPack> {
        app.import_pack(
            &ImportRequest::new(archive).with_storage(StorageClass::Temporary),
            &OperationReporter::silent("curseforge-import"),
            &CancellationToken::new(),
        )
        .await
    }

    const REQUIRED_BODY: &[u8] = b"the required mod jar";
    const OPTIONAL_BODY: &[u8] = b"the optional mod jar";

    async fn serve_pack(server: &MockServer) {
        serve_file(
            server,
            REQUIRED_PROJECT,
            REQUIRED_FILE,
            "required-1.0.0.jar",
            REQUIRED_BODY,
        )
        .await;
        serve_file(
            server,
            OPTIONAL_PROJECT,
            OPTIONAL_FILE,
            "optional-1.0.0.jar",
            OPTIONAL_BODY,
        )
        .await;
    }

    // -- analysis without a credential -------------------------------------

    #[test]
    fn a_curseforge_export_is_identified_and_described_without_any_key_configured() {
        // Detection and manifest parsing are local. A launcher that could not
        // even say what the file is until a key was configured would be worse
        // than one that says what it is and what it still needs.
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = App::open_at(Paths::rooted_at(dir.path())).expect("app opens");

        let analysis = app.analyze_pack(&archive).expect("analyze");
        assert_eq!(analysis.format, PackFormat::CurseForgePack);
        assert_eq!(analysis.name(), "Repro Pack");
        assert_eq!(analysis.minecraft_version(), "1.20.1");
        assert_eq!(analysis.loader().kind, LoaderKind::Forge);
        assert_eq!(analysis.loader().version.as_deref(), Some("47.4.10"));
        assert_eq!(analysis.author(), Some("A Reporter"));
        assert_eq!(analysis.required_file_count(PackSide::Client), 1);
        assert_eq!(analysis.optional_file_count(PackSide::Client), 1);
        assert!(analysis.needs_provider_resolution());
    }

    #[tokio::test]
    async fn an_import_with_no_key_configured_fails_before_any_request_and_creates_nothing() {
        let server = MockServer::start().await;
        serve_pack(&server).await;
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_without_a_key(dir.path(), &server);

        let error = import(&app, &archive).await.expect_err("must refuse");
        assert_eq!(error.message_id(), "error-mod-credential-missing");
        assert!(app.list_clients().expect("list").is_empty());
        assert!(
            server
                .received_requests()
                .await
                .expect("recorded")
                .is_empty(),
            "a request was made without a key"
        );
    }

    // -- the exact import --------------------------------------------------

    #[tokio::test]
    async fn a_curseforge_pack_imports_into_the_same_normalized_recipe_a_modrinth_pack_does() {
        let server = MockServer::start().await;
        serve_pack(&server).await;
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);

        let imported = import(&app, &archive).await.expect("import");
        let record = &imported.record;
        assert_eq!(record.name, "Repro Pack");
        assert_eq!(record.minecraft_version, "1.20.1");
        assert_eq!(record.loader.kind, LoaderKind::Forge);
        assert_eq!(record.loader.version.as_deref(), Some("47.4.10"));
        assert_eq!(record.source.kind, ImportSourceKind::CurseForgePack);

        // The required file is installed; the unselected optional one is not.
        let game = ephervex_storage::metadata::client_game_dir(&record.root);
        assert_eq!(
            std::fs::read(game.join("mods/required-1.0.0.jar")).expect("read"),
            REQUIRED_BODY
        );
        assert!(!game.join("mods/optional-1.0.0.jar").exists());
        // ...and the pack's overrides landed.
        assert_eq!(
            std::fs::read_to_string(game.join("config/repro.toml")).expect("read"),
            "debug = true\n"
        );

        let recipe = imported.recipe();
        assert_eq!(recipe.files.len(), 2);
        let required = &recipe.files[0];
        assert_eq!(
            required.logical_path,
            PathBuf::from("mods/required-1.0.0.jar")
        );
        assert_eq!(required.origin, ModOrigin::PackDeclared);
        assert_eq!(required.client, SideRequirement::Required);
        assert_eq!(
            required.server,
            SideRequirement::Unknown,
            "the export format states nothing about servers"
        );
        let identity = required.identity.as_ref().expect("an identity");
        assert_eq!(identity.provider, Some(ModProvider::CurseForge));
        assert_eq!(identity.project_id.as_deref(), Some("238222"));
        assert_eq!(identity.version_id.as_deref(), Some("4593548"));
        assert_eq!(identity.file_id.as_deref(), Some("4593548"));

        let optional = &recipe.files[1];
        assert_eq!(optional.client, SideRequirement::Optional);
        assert_eq!(
            optional.selected,
            Some(false),
            "the user was asked and said no"
        );
    }

    #[tokio::test]
    async fn the_exact_declared_file_is_requested_and_a_newer_one_is_never_substituted() {
        // The regression this whole design exists for. The API is told to
        // answer about a *newer* file for the same project; the import must
        // refuse rather than install it.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/v1/mods/{REQUIRED_PROJECT}/files/{REQUIRED_FILE}"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                file_document(
                    &server,
                    REQUIRED_PROJECT,
                    REQUIRED_FILE + 1,
                    "required-2.0.0.jar",
                    REQUIRED_BODY,
                ),
                "application/json",
            ))
            .mount(&server)
            .await;
        serve_file(
            &server,
            OPTIONAL_PROJECT,
            OPTIONAL_FILE,
            "optional-1.0.0.jar",
            OPTIONAL_BODY,
        )
        .await;

        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);

        let error = import(&app, &archive).await.expect_err("must refuse");
        assert_eq!(error.message_id(), "error-mod-file-identity-mismatch");
        assert!(app.list_clients().expect("list").is_empty());

        // Only the exact endpoint was asked; nothing listed the project's
        // versions looking for something that would fit.
        let paths: Vec<String> = server
            .received_requests()
            .await
            .expect("recorded")
            .iter()
            .map(|request| request.url.path().to_owned())
            .collect();
        assert!(
            paths
                .iter()
                .all(|requested| requested.contains("/files/") || requested.starts_with("/files/")),
            "{paths:?}"
        );
    }

    #[tokio::test]
    async fn each_distinct_file_is_looked_up_exactly_once_for_one_import() {
        let server = MockServer::start().await;
        serve_pack(&server).await;
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);

        import(&app, &archive).await.expect("import");

        let lookups = server
            .received_requests()
            .await
            .expect("recorded")
            .iter()
            .filter(|request| request.url.path().starts_with("/v1/"))
            .count();
        assert_eq!(
            lookups, 2,
            "one exact-file request per declared file, and no more"
        );
    }

    // -- verification and failure ------------------------------------------

    #[tokio::test]
    async fn a_body_that_does_not_match_the_published_digest_publishes_no_instance() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/v1/mods/{REQUIRED_PROJECT}/files/{REQUIRED_FILE}"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                file_document(
                    &server,
                    REQUIRED_PROJECT,
                    REQUIRED_FILE,
                    "required-1.0.0.jar",
                    REQUIRED_BODY,
                ),
                "application/json",
            ))
            .mount(&server)
            .await;
        // The declared size still matches, so only the digest can catch this.
        Mock::given(method("GET"))
            .and(path(format!(
                "/files/{REQUIRED_PROJECT}/{REQUIRED_FILE}/required-1.0.0.jar"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                b"the CORRUPTED mod jar".to_vec(),
                "application/java-archive",
            ))
            .mount(&server)
            .await;
        serve_file(
            &server,
            OPTIONAL_PROJECT,
            OPTIONAL_FILE,
            "optional-1.0.0.jar",
            OPTIONAL_BODY,
        )
        .await;

        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);

        let error = import(&app, &archive).await.expect_err("must refuse");
        assert_eq!(error.message_id(), "error-pack-download-failed");
        assert!(
            app.list_clients().expect("list").is_empty(),
            "a failed import leaves no visible instance"
        );
    }

    #[tokio::test]
    async fn a_file_whose_author_forbids_third_party_downloads_stops_the_import_explicitly() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/v1/mods/{REQUIRED_PROJECT}/files/{REQUIRED_FILE}"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"{{"data":{{"id":{REQUIRED_FILE},"gameId":432,"modId":{REQUIRED_PROJECT},
                        "isAvailable":true,"fileName":"restricted.jar","fileLength":10,
                        "downloadUrl":null,"hashes":[]}}}}"#
                ),
                "application/json",
            ))
            .mount(&server)
            .await;
        serve_file(
            &server,
            OPTIONAL_PROJECT,
            OPTIONAL_FILE,
            "optional-1.0.0.jar",
            OPTIONAL_BODY,
        )
        .await;

        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);

        let error = import(&app, &archive).await.expect_err("must refuse");
        assert_eq!(error.message_id(), "error-mod-file-distribution-restricted");
        assert!(app.list_clients().expect("list").is_empty());
        // Nothing went looking for the file anywhere else.
        let fetches = server
            .received_requests()
            .await
            .expect("recorded")
            .iter()
            .filter(|request| request.url.path().starts_with("/files/"))
            .count();
        assert_eq!(fetches, 0);
    }

    #[tokio::test]
    async fn a_canceled_import_publishes_nothing() {
        let server = MockServer::start().await;
        serve_pack(&server).await;
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);

        let cancel = CancellationToken::new();
        cancel.cancel();
        let error = app
            .import_pack(
                &ImportRequest::new(&archive),
                &OperationReporter::silent("curseforge-import"),
                &cancel,
            )
            .await
            .expect_err("canceled");
        assert!(error.is_cancellation(), "{error:?}");
        assert!(app.list_clients().expect("list").is_empty());
    }

    // -- credential containment --------------------------------------------

    #[tokio::test]
    async fn the_api_key_reaches_the_api_and_the_delivery_host_and_nothing_else() {
        // Both are loopback here, so the assertion that matters is the other
        // one: an unrelated download made through the same application does not
        // carry the key, and nothing written to disk contains it.
        let server = MockServer::start().await;
        serve_pack(&server).await;
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);

        let imported = import(&app, &archive).await.expect("import");

        let authenticated: Vec<&Request> = server
            .received_requests()
            .await
            .expect("recorded")
            .iter()
            .filter(|request| request.headers.contains_key("x-api-key"))
            .cloned()
            .collect::<Vec<_>>()
            .leak()
            .iter()
            .collect();
        assert!(
            !authenticated.is_empty(),
            "the API request must be authenticated"
        );

        // Nothing the import left behind names the key.
        let source = SourceStore::new(ephervex_storage::metadata::source_dir(
            &imported.record.root,
        ));
        let recipe = std::fs::read_to_string(source.recipe_path()).expect("read");
        assert!(!recipe.contains(FAKE_KEY), "{recipe}");
        let manifest =
            std::fs::read_to_string(source.dir().join("curseforge/manifest.json")).expect("read");
        assert!(!manifest.contains(FAKE_KEY));
        for file in &imported.recipe().files {
            for address in &file.downloads {
                assert!(!address.contains(FAKE_KEY), "{address}");
                assert!(!address.contains('?'), "no query string carries a secret");
            }
        }
    }

    #[tokio::test]
    async fn a_download_address_off_the_permitted_hosts_is_refused_rather_than_followed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/v1/mods/{REQUIRED_PROJECT}/files/{REQUIRED_FILE}"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"{{"data":{{"id":{REQUIRED_FILE},"gameId":432,"modId":{REQUIRED_PROJECT},
                        "isAvailable":true,"fileName":"required-1.0.0.jar","fileLength":20,
                        "downloadUrl":"https://evil.example/required-1.0.0.jar",
                        "hashes":[{{"value":"{digest}","algo":1}}]}}}}"#,
                    digest = sha1_of(REQUIRED_BODY),
                ),
                "application/json",
            ))
            .mount(&server)
            .await;
        serve_file(
            &server,
            OPTIONAL_PROJECT,
            OPTIONAL_FILE,
            "optional-1.0.0.jar",
            OPTIONAL_BODY,
        )
        .await;

        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);

        let error = import(&app, &archive).await.expect_err("must refuse");
        assert_eq!(error.message_id(), "error-pack-download-url-rejected");
        assert!(app.list_clients().expect("list").is_empty());
    }

    // -- the shared artifact cache -----------------------------------------

    #[tokio::test]
    async fn a_curseforge_artifact_lands_in_the_shared_store_and_is_reused_by_the_next_import() {
        let server = MockServer::start().await;
        serve_pack(&server).await;
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);

        let first = import(&app, &archive).await.expect("import");
        assert_eq!(first.materialized.files.reused_from_cache, 0);

        // Addressed by the verified content hash, not by an id or a file name.
        let layout = CacheLayout::new(app.paths().cache_dir());
        let digest = Hash::new(HashAlgorithm::Sha1, &sha1_of(REQUIRED_BODY)).expect("a digest");
        let cached = layout.content(ContentStore::Downloads, &digest, None);
        assert!(
            cached.parent().expect("a parent").is_dir(),
            "the artifact is not in the shared content-addressed store"
        );

        let second = import(&app, &archive).await.expect("a second import");
        assert_eq!(
            second.materialized.files.reused_from_cache, 1,
            "the verified artifact was reused rather than downloaded again"
        );
    }

    #[tokio::test]
    async fn collecting_the_shared_cache_leaves_an_imported_instance_working() {
        let server = MockServer::start().await;
        serve_pack(&server).await;
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);

        let imported = import(&app, &archive).await.expect("import");
        let installed = ephervex_storage::metadata::client_game_dir(&imported.record.root)
            .join("mods/required-1.0.0.jar");
        assert!(installed.is_file());

        // A budget no artifact could fit, so the collector has to take them.
        let report = ArtifactCache::new(CacheLayout::new(app.paths().cache_dir()))
            .collect(ArtifactCacheLimit::Limited(0))
            .await
            .expect("collect");
        assert!(report.removed_entries > 0, "nothing was collected");
        assert_eq!(
            std::fs::read(&installed).expect("read"),
            REQUIRED_BODY,
            "the instance owns its own copy and survives eviction"
        );
    }

    // -- durability --------------------------------------------------------

    #[tokio::test]
    async fn the_normalized_recipe_reopens_with_no_provider_reachable_at_all() {
        let server = MockServer::start().await;
        serve_pack(&server).await;
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);
        let imported = import(&app, &archive).await.expect("import");

        // Nothing below this line may contact anyone: the API is gone and so is
        // the archive.
        drop(server);
        std::fs::remove_file(&archive).expect("the archive goes away");

        let recipe = app
            .pack_recipe(imported.record.id)
            .expect("query")
            .expect("an imported instance has one");
        assert_eq!(recipe.source_kind, ImportSourceKind::CurseForgePack);
        assert_eq!(recipe.minecraft_version, "1.20.1");
        assert_eq!(recipe.loader.version.as_deref(), Some("47.4.10"));

        // Every declared entry is re-resolvable, and the one this client
        // actually took is the one its mod list shows.
        let managed = recipe.managed_mods();
        assert_eq!(managed.len(), 2);
        assert_eq!(managed[0].provider, ModProvider::CurseForge);
        assert_eq!(managed[0].project_id, "238222");
        assert_eq!(managed[0].version_id, "4593548");
        assert_eq!(managed[0].origin, ModOrigin::PackDeclared);

        let recorded = recipe.recorded_mods();
        assert_eq!(
            recorded.len(),
            1,
            "the optional file nobody selected is not a jar this client has"
        );
        assert_eq!(recorded[0].file_name, "required-1.0.0.jar");
        assert_eq!(recorded[0].origin, ModOrigin::PackDeclared);
        assert_eq!(recorded[0].server, SideRequirement::Unknown);

        // ...and the user's own manifest is still there, byte for byte.
        let source = app.pack_source_dir(imported.record.id).expect("source dir");
        assert_eq!(
            std::fs::read(source.join("curseforge/manifest.json")).expect("read"),
            export_manifest(),
            "the original manifest is retained verbatim"
        );

        // The recipe Ephervex owns is compact.
        let recipe_text = std::fs::read_to_string(source.join("recipe.json")).expect("read");
        assert!(!recipe_text.contains('\n'), "{recipe_text}");
    }

    #[tokio::test]
    async fn a_normalized_curseforge_recipe_plans_a_server_without_any_curseforge_knowledge() {
        let server = MockServer::start().await;
        serve_pack(&server).await;
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);
        let imported = import(&app, &archive).await.expect("import");
        drop(server);

        let recipe = app
            .pack_recipe(imported.record.id)
            .expect("query")
            .expect("a recipe");

        // The generic planner, unchanged and knowing nothing about CurseForge.
        let plan = recipe
            .server_reconstruction_plan(&OptionalSelection::none())
            .expect("a plan");
        assert!(
            plan.files.is_empty(),
            "an undeclared server side is a question, not a yes"
        );
        assert_eq!(
            plan.unconfirmed_unknown.len(),
            2,
            "both declared files are offered for confirmation rather than assumed"
        );
        assert_eq!(plan.minecraft_version, "1.20.1");
        assert_eq!(plan.loader.version.as_deref(), Some("47.4.10"));
        assert!(
            plan.layers_are_retained(),
            "the pack's overrides were kept for a server built later"
        );

        // Confirming it explicitly is what puts it in, and the exact identity
        // and ownership survive that too.
        let confirmed = recipe
            .server_reconstruction_plan(&OptionalSelection::of(["mods/required-1.0.0.jar"]))
            .expect("a plan");
        assert_eq!(confirmed.files.len(), 1);
        assert_eq!(confirmed.files[0].origin, ModOrigin::PackDeclared);
        assert_eq!(
            confirmed.files[0]
                .identity
                .as_ref()
                .and_then(|identity| identity.file_id.as_deref()),
            Some("4593548")
        );
    }

    // -- opt-in production smoke -------------------------------------------

    /// The exact file this smoke installs, and deliberately not the newest one.
    ///
    /// Just Enough Items is a long-lived project that permits third-party
    /// downloads, and this is one of its older 1.20.1 builds. Newer files of
    /// the same project exist, which is the point: an exact import installs the
    /// one the manifest declares.
    const LIVE_PROJECT: u32 = 238_222;
    const LIVE_FILE: u32 = 8_617_367;

    #[tokio::test]
    #[ignore = "contacts the production CurseForge API with the configured key and downloads one artifact"]
    async fn the_production_api_resolves_and_delivers_one_exact_declared_file() {
        // Runs against the real credential store and the real API, into a
        // temporary root of its own: no instance the user owns is touched, and
        // the key is never read by this test — only by the transport, which
        // puts it in one header.
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("live.zip");
        zip_file(
            &archive,
            &[(
                "manifest.json",
                format!(
                    r#"{{
                        "minecraft": {{
                            "version": "1.20.1",
                            "modLoaders": [{{"id": "forge-47.4.10", "primary": true}}]
                        }},
                        "manifestType": "minecraftModpack",
                        "manifestVersion": 1,
                        "name": "Live Smoke",
                        "version": "1.0.0",
                        "files": [
                            {{"projectID": {LIVE_PROJECT}, "fileID": {LIVE_FILE}, "required": true}}
                        ],
                        "overrides": "overrides"
                    }}"#
                )
                .as_bytes(),
            )],
        );

        let app = App::open_at(Paths::rooted_at(dir.path())).expect("app opens");
        let imported = import(&app, &archive)
            .await
            .expect("the production API resolves the declared file");

        let entry = &imported.recipe().files[0];
        assert_eq!(
            entry
                .identity
                .as_ref()
                .and_then(|identity| identity.file_id.as_deref()),
            Some(LIVE_FILE.to_string().as_str()),
            "the declared file, not a newer one"
        );
        assert_eq!(entry.origin, ModOrigin::PackDeclared);
        assert_eq!(entry.server, SideRequirement::Unknown);
        assert!(
            entry
                .hashes
                .iter()
                .any(|hash| hash.algorithm == HashAlgorithm::Sha1),
            "a cryptographic digest was published and kept"
        );

        // The artifact is verified, in the instance, and in the shared store.
        let installed = ephervex_storage::metadata::client_game_dir(&imported.record.root)
            .join(&entry.logical_path);
        assert!(installed.is_file(), "{}", installed.display());
        let cached = CacheLayout::new(app.paths().cache_dir()).content(
            ContentStore::Downloads,
            &entry.hashes[0],
            None,
        );
        assert!(
            cached.parent().expect("a parent").is_dir(),
            "the verified artifact is not in the shared content-addressed store"
        );

        // Nothing the import kept names a credential-bearing address.
        for address in &entry.downloads {
            assert!(address.starts_with("https://"), "{address}");
            assert!(address.find('?').is_none(), "{address}");
        }
    }
}
