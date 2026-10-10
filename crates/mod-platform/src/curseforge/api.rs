//! Authenticated requests against the CurseForge third-party REST API.
//!
//! # One header, attached per request
//!
//! The API authenticates with `x-api-key`. It is set on each request rather
//! than installed as a default header on the client, so the credential cannot
//! be inherited by some later caller that happens to share a connection pool.
//! It is also marked sensitive, which keeps it out of the HTTP/2 header index.
//!
//! `x-api-key` is not `X-Api-Token`. The latter belongs to the CurseForge
//! Upload API, which is a different service with a different credential, and
//! is not what this module speaks to.
//!
//! # Nothing is written to disk
//!
//! There is no [`MetadataCache`](ephervex_download::MetadataCache) here and no
//! staging path. A response is read into memory, parsed, and dropped. See the
//! module documentation on [`super`] for why that is a requirement rather than
//! an omission.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use ephervex_download::{HttpConfig, Url, build_single_origin_client, check_transport, redacted};
use ephervex_model::{
    CurseForgeFileId, CurseForgeProjectId, Hash, HashAlgorithm, LoaderKind, ModProvider,
    SecretString,
};
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderName, HeaderValue};
use reqwest::{Client, Method, Response, StatusCode};
use tokio_util::sync::CancellationToken;
use tracing::debug;

use super::credential::{CurseForgeCredentialSource, missing};
use super::dto::{
    ApiEnvelope, CurseForgeCategoryClass, CurseForgeFileDocument, CurseForgeFileHash,
    CurseForgeFingerprintMatches, CurseForgeGame, CurseForgePagination, CurseForgeProject,
    HASH_ALGO_SHA1,
};
use super::mapping::{file_declared_sides, file_relations, published_at, split_declared_support};
use crate::error::{ModPlatformError, ModPlatformResult};
use crate::model::{Distribution, ModDependency, ModFile, RecognizedFile, SideSupport};

/// The production API root.
pub const CURSEFORGE_API_BASE: &str = "https://api.curseforge.com/v1";

/// CurseForge's game id for Minecraft.
///
/// A published, stable identifier: every Minecraft query on this API carries
/// it, and it is what makes a lookup mean "Minecraft" rather than "some game".
pub const MINECRAFT_GAME_ID: u32 = 432;

/// The provider-returned slug this launcher looks for to identify Minecraft's
/// top-level "Mods" project class.
///
/// Neither this slug nor a class's numeric id is published by CurseForge as a
/// guaranteed API constant. What the documented `GET /v1/categories` endpoint
/// does return, for the categories it currently lists, is a `slug` alongside
/// each class's `id` and `name`; this is the Minecraft Mods class's slug as
/// CurseForge currently serves it. [`CurseForgeOperation::project_classes`]
/// matches against it at request time rather than assuming any numeric id,
/// and if a future response no longer names a class this way, resolution
/// fails closed instead of guessing a number.
const MODS_CLASS_SLUG: &str = "mc-mods";

/// The provider-returned slug this launcher looks for to identify Minecraft's
/// top-level "Resource Packs" project class, on the same terms as
/// [`MODS_CLASS_SLUG`]: not a guaranteed API constant, and matched at request
/// time against whatever the categories response currently names.
///
/// Unlike Mods, a response that does not currently name this class is not a
/// hard failure: only a CurseForge modpack import ever asks whether a project
/// belongs to it, and a mod-browsing operation that will never ask must not
/// fail on its account. See [`CurseForgeProjectClasses::classify`].
const RESOURCE_PACKS_CLASS_SLUG: &str = "texture-packs";

/// The provider-returned slug this launcher looks for to identify Minecraft's
/// top-level "Shader Packs" project class, on the same terms as
/// [`RESOURCE_PACKS_CLASS_SLUG`]: not a guaranteed API constant, matched at
/// request time, and optional rather than a hard failure when a response does
/// not currently name it.
const SHADER_PACKS_CLASS_SLUG: &str = "shaders";

/// The header the third-party API authenticates with.
///
/// Public because the same header is what an authenticated *artifact* request
/// carries, and that request is built above this crate. One definition means
/// the two cannot drift into sending different headers for one credential.
pub const CURSEFORGE_API_KEY_HEADER: &str = "x-api-key";

/// How long a single API request may take.
///
/// Short, like authentication's: nothing this client asks for is more than a
/// few kilobytes, and a user waiting on a settings screen deserves a failure
/// rather than a spinner.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The CurseForge third-party API, as much of it as this launcher uses.
#[derive(Debug, Clone)]
pub struct CurseForgeApi {
    client: Client,
    base: Url,
    credential: Arc<dyn CurseForgeCredentialSource>,
}

impl CurseForgeApi {
    /// A client against the production API.
    pub fn new(
        credential: Arc<dyn CurseForgeCredentialSource>,
        http: &HttpConfig,
    ) -> ModPlatformResult<Self> {
        Self::with_base_url(credential, http, CURSEFORGE_API_BASE)
    }

    /// A client against an explicit API root.
    ///
    /// The root is a parameter so integration tests can point the client at a
    /// loopback server, the same way the Modrinth provider does. Production
    /// always uses [`CURSEFORGE_API_BASE`], and the transport check below still
    /// refuses plaintext HTTP unless a test has explicitly opted in.
    ///
    /// The origin of this root is also the boundary the credential may travel
    /// to: redirects that leave it are refused rather than followed.
    pub fn with_base_url(
        credential: Arc<dyn CurseForgeCredentialSource>,
        http: &HttpConfig,
        base: &str,
    ) -> ModPlatformResult<Self> {
        let base = Url::parse(&format!("{}/", base.trim_end_matches('/')))
            .map_err(|error| unusable(&error.to_string()))?;
        check_transport(&base, http.allow_plaintext_http)
            .map_err(|error| unusable(&error.to_string()))?;
        let client = build_single_origin_client(http, &base)
            .map_err(|error| unusable(&error.to_string()))?;
        Ok(Self {
            client,
            base,
            credential,
        })
    }

    /// The API root this client talks to.
    #[must_use]
    pub fn base_url(&self) -> &Url {
        &self.base
    }

    /// Whether an API key is configured at all.
    ///
    /// Answered without a network request, because the settings screen that
    /// asks is not allowed to need one. Reports the *presence* of a credential
    /// and never its value.
    pub fn has_credential(&self) -> ModPlatformResult<bool> {
        Ok(self.credential.api_key()?.is_some())
    }

    /// One game, by id.
    ///
    /// Small and cheap, and the reason it exists is explicit: it is the
    /// smallest authenticated call that proves a configured key is accepted.
    /// Asking for [`MINECRAFT_GAME_ID`] is a credential check, not the start of
    /// a catalog - nothing it returns is kept.
    pub async fn game(
        &self,
        game_id: u32,
        cancel: &CancellationToken,
    ) -> ModPlatformResult<CurseForgeGame> {
        let url = self.endpoint(&["games", &game_id.to_string()])?;
        self.get::<CurseForgeGame>(&url, cancel).await
    }

    /// One project, by its numeric CurseForge id.
    pub async fn project(
        &self,
        project_id: u32,
        cancel: &CancellationToken,
    ) -> ModPlatformResult<CurseForgeProject> {
        let url = self.endpoint(&["mods", &project_id.to_string()])?;
        match self.get::<CurseForgeProject>(&url, cancel).await {
            // A 404 from this endpoint means the id names nothing, which is
            // worth saying rather than reporting as an outage.
            Err(ModPlatformError::HttpStatus { status, .. }) if status == StatusCode::NOT_FOUND => {
                Err(ModPlatformError::ProjectUnknown {
                    provider: ModProvider::CurseForge,
                    reference: project_id.to_string(),
                })
            }
            other => other,
        }
    }

    /// Holds this operation's credential for its sequence of requests.
    ///
    /// A pack import asks for hundreds of files, and reading the secret store
    /// once per request would mean hundreds of keyring round trips over D-Bus
    /// for one import. The scope reads it once and drops it when the operation
    /// ends - deliberately not longer: the credential's lifetime is the
    /// operation's, and holding one on the client so a later operation could
    /// reuse it is exactly the extension this avoids.
    ///
    /// Resolving it up front also means an import with no key configured fails
    /// before contacting CurseForge at all, both because the requests could
    /// only be refused and because unauthenticated calls still spend somebody's
    /// rate limit.
    pub fn begin_operation(&self) -> ModPlatformResult<CurseForgeOperation<'_>> {
        let key = self.credential.api_key()?.ok_or_else(missing)?;
        Ok(CurseForgeOperation { api: self, key })
    }

    /// Builds an endpoint URL from path segments.
    ///
    /// Every segment goes through `Url`'s own escaping. Nothing a user typed
    /// reaches one today - both endpoints take a number - and routing them
    /// through here is what keeps that true when one takes a slug.
    fn endpoint(&self, segments: &[&str]) -> ModPlatformResult<Url> {
        let mut url = self.base.clone();
        {
            let mut path = url
                .path_segments_mut()
                .map_err(|()| unusable("the api base cannot have path segments"))?;
            // The base is normalized with a trailing slash, which leaves an
            // empty final segment; appending onto it would produce `//`.
            path.pop_if_empty();
            for segment in segments {
                path.push(segment);
            }
        }
        Ok(url)
    }

    /// Performs one authenticated GET and reads the answer.
    ///
    /// The credential is resolved *first*: a launcher with no key configured
    /// must fail without contacting CurseForge at all, both because the request
    /// could only be refused and because an unauthenticated call still spends
    /// somebody's rate limit.
    async fn get<T: serde::de::DeserializeOwned>(
        &self,
        url: &Url,
        cancel: &CancellationToken,
    ) -> ModPlatformResult<T> {
        let key = self.credential.api_key()?.ok_or_else(missing)?;
        self.get_with_key(url, &key, cancel).await
    }

    /// The same request with the credential already in hand.
    async fn get_with_key<T: serde::de::DeserializeOwned>(
        &self,
        url: &Url,
        key: &SecretString,
        cancel: &CancellationToken,
    ) -> ModPlatformResult<T> {
        self.send_with_key::<T>(Method::GET, url, None, key, cancel)
            .await
            .map(|envelope| envelope.0)
    }

    /// A request whose answer also reports how far into a listing it reached.
    async fn page_with_key<T: serde::de::DeserializeOwned>(
        &self,
        url: &Url,
        key: &SecretString,
        cancel: &CancellationToken,
    ) -> ModPlatformResult<(T, Option<CurseForgePagination>)> {
        self.send_with_key::<T>(Method::GET, url, None, key, cancel)
            .await
            .map(|envelope| (envelope.0, envelope.1))
    }

    /// One authenticated request, and the envelope it answered with.
    ///
    /// `body`, when present, is sent as JSON: the batch endpoints are `POST`
    /// with a list of ids precisely so a plan naming many projects costs one
    /// round trip rather than one each.
    async fn send_with_key<T: serde::de::DeserializeOwned>(
        &self,
        method: Method,
        url: &Url,
        body: Option<serde_json::Value>,
        key: &SecretString,
        cancel: &CancellationToken,
    ) -> ModPlatformResult<(T, Option<CurseForgePagination>)> {
        let mut header = HeaderValue::from_str(key.expose()).map_err(|_| {
            // A fixed sentence, never the rejected value.
            ModPlatformError::CredentialUnreadable {
                provider: ModProvider::CurseForge,
                detail: "the configured API key cannot be sent in a request header".to_owned(),
            }
        })?;
        // Keeps the value out of the HTTP/2 header index, where it would
        // otherwise be retained for the life of the connection.
        header.set_sensitive(true);

        let mut request = self
            .client
            .request(method, url.clone())
            .header(HeaderName::from_static(CURSEFORGE_API_KEY_HEADER), header)
            .header(ACCEPT, HeaderValue::from_static("application/json"))
            .timeout(REQUEST_TIMEOUT);
        if let Some(body) = body {
            request = request
                .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
                .body(serde_json::to_vec(&body).map_err(|error| {
                    ModPlatformError::ResponseInvalid {
                        provider: ModProvider::CurseForge,
                        detail: error.to_string(),
                    }
                })?);
        }

        let response: Response = tokio::select! {
            // Biased so a request canceled before it starts wins rather than
            // racing a round trip whose answer is going to be discarded.
            biased;
            () = cancel.cancelled() => return Err(ModPlatformError::Canceled),
            result = request.send() => result.map_err(|error| request_failed(&error))?,
        };

        let status = response.status();
        // The address without its query, the status, and nothing else. Never
        // the request headers: one of them is the credential.
        debug!(
            status = status.as_u16(),
            url = %redacted(url),
            "curseforge api response"
        );
        classify(status)?;

        let body = response
            .text()
            .await
            .map_err(|error| request_failed(&error))?;
        serde_json::from_str::<ApiEnvelope<T>>(&body)
            .map(|envelope| (envelope.data, envelope.pagination))
            .map_err(|error| ModPlatformError::ResponseInvalid {
                provider: ModProvider::CurseForge,
                detail: error.to_string(),
            })
    }
}

/// One operation's sequence of authenticated CurseForge requests.
///
/// Exists for two reasons, both about the credential. It is read once and lives
/// exactly as long as the operation that needs it, and it is available to
/// whatever else that operation authenticates - an artifact download, for
/// instance - without a second trip to the secret store.
///
/// Nothing it fetches is written down. See [`super`] for why that is a
/// requirement rather than an omission.
pub struct CurseForgeOperation<'a> {
    api: &'a CurseForgeApi,
    key: SecretString,
}

impl fmt::Debug for CurseForgeOperation<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Names what this is and never what it holds. A derived implementation
        // would render the secret's own `Debug`, which is redacted - but the
        // guarantee should not depend on that staying true.
        formatter.write_str("CurseForgeOperation")
    }
}

impl CurseForgeOperation<'_> {
    /// The credential this operation may spend, for a request it makes itself.
    ///
    /// Handing out the secret is the point: an artifact download is part of the
    /// same operation and must not read the secret store again. What it may be
    /// sent to is the caller's to constrain.
    #[must_use]
    pub fn credential(&self) -> &SecretString {
        &self.key
    }

    /// The game's top-level project classes, this request's own answer.
    ///
    /// `GET /v1/categories?gameId={game}&classesOnly=true` lists every
    /// top-level class the game currently has. Not cached beyond the caller: a
    /// second operation asks again rather than trusting values this one
    /// happened to see.
    pub async fn project_classes(
        &self,
        game_id: u32,
        cancel: &CancellationToken,
    ) -> ModPlatformResult<CurseForgeProjectClasses> {
        let mut url = self.api.endpoint(&["categories"])?;
        url.query_pairs_mut()
            .append_pair("gameId", &game_id.to_string())
            .append_pair("classesOnly", "true");
        let classes: Vec<CurseForgeCategoryClass> =
            self.api.get_with_key(&url, &self.key, cancel).await?;
        let mods = classes
            .iter()
            .find(|class| class.slug == MODS_CLASS_SLUG)
            .map(|class| class.id)
            .ok_or_else(|| ModPlatformError::ResponseInvalid {
                provider: ModProvider::CurseForge,
                detail: format!(
                    "game {game_id} has no top-level category class named `{MODS_CLASS_SLUG}`"
                ),
            })?;
        // Opportunistic, unlike Mods: see `RESOURCE_PACKS_CLASS_SLUG`.
        let resource_packs = classes
            .iter()
            .find(|class| class.slug == RESOURCE_PACKS_CLASS_SLUG)
            .map(|class| class.id);
        // Opportunistic on the same terms: see `SHADER_PACKS_CLASS_SLUG`.
        let shader_packs = classes
            .iter()
            .find(|class| class.slug == SHADER_PACKS_CLASS_SLUG)
            .map(|class| class.id);
        let names = classes
            .into_iter()
            .map(|class| (class.id, class.name))
            .collect();
        Ok(CurseForgeProjectClasses {
            mods,
            resource_packs,
            shader_packs,
            names,
        })
    }

    /// One project's full metadata, including the class it belongs to.
    ///
    /// `GET /v1/mods/{modId}`. Distinct from [`CurseForgeApi::project`]: that
    /// one exists for a single unauthenticated-feeling credential check and
    /// reads the secret store on every call, which an operation classifying a
    /// pack's many distinct projects must not do.
    pub async fn project(
        &self,
        project: CurseForgeProjectId,
        cancel: &CancellationToken,
    ) -> ModPlatformResult<CurseForgeProject> {
        let url = self.api.endpoint(&["mods", &project.to_string()])?;
        match self.api.get_with_key(&url, &self.key, cancel).await {
            Ok(document) => Ok(document),
            // A 404 here means this id names nothing, which is worth saying
            // rather than reporting as an outage.
            Err(ModPlatformError::HttpStatus { status, .. }) if status == StatusCode::NOT_FOUND => {
                Err(ModPlatformError::ProjectUnknown {
                    provider: ModProvider::CurseForge,
                    reference: project.to_string(),
                })
            }
            Err(other) => Err(other),
        }
    }

    /// One exact file of one exact project.
    ///
    /// `GET /v1/mods/{modId}/files/{fileId}` - the endpoint that answers about
    /// the file that was asked for. Deliberately not a version listing filtered
    /// down afterwards: a pack declares one file id, and anything that picks
    /// from a list can pick a newer one.
    ///
    /// The answer is checked against the question. A response describing
    /// another project or another file is refused rather than used, because a
    /// substituted artifact is precisely the failure an exact import exists to
    /// prevent.
    pub async fn file(
        &self,
        project: CurseForgeProjectId,
        file: CurseForgeFileId,
        cancel: &CancellationToken,
    ) -> ModPlatformResult<CurseForgeFileRelease> {
        let url = self
            .api
            .endpoint(&["mods", &project.to_string(), "files", &file.to_string()])?;
        let document: CurseForgeFileDocument =
            match self.api.get_with_key(&url, &self.key, cancel).await {
                Ok(document) => document,
                // A 404 here means this project has no such file, which is a fact
                // about the pack rather than an outage.
                Err(ModPlatformError::HttpStatus { status, .. })
                    if status == StatusCode::NOT_FOUND =>
                {
                    return Err(ModPlatformError::FileUnknown {
                        provider: ModProvider::CurseForge,
                        project_id: project.to_string(),
                        file_id: file.to_string(),
                    });
                }
                Err(other) => return Err(other),
            };
        normalize_file(project, file, document)
    }

    /// One page of the game's Mods, narrowed to what an instance can run.
    ///
    /// `GET /v1/mods/search`. The game and the provider-discovered Mods class
    /// always travel with it, and so do the exact Minecraft release and the
    /// loader unless the search asks for incompatible results. The class filter
    /// is what keeps resource packs, worlds, and modpacks out of a mod browser;
    /// it is not treated as proof, and each returned project is classified
    /// again by the caller.
    ///
    /// Asking for incompatible results drops the release and loader filters:
    /// a filter would remove the very rows that need annotating.
    ///
    /// A loader this launcher does not support has no filter to send, and the
    /// search is refused rather than widened.
    pub(super) async fn search(
        &self,
        game_id: u32,
        search: &CurseForgeSearch<'_>,
        cancel: &CancellationToken,
    ) -> ModPlatformResult<CurseForgePage<CurseForgeProject>> {
        let loader =
            loader_query_value(search.loader).ok_or(ModPlatformError::LoaderCannotHostMods {
                loader: search.loader,
            })?;
        let mut url = self.api.endpoint(&["mods", "search"])?;
        {
            let mut query = url.query_pairs_mut();
            query
                .append_pair("gameId", &game_id.to_string())
                .append_pair("classId", &search.class_id.to_string());
            if !search.include_incompatible {
                query
                    .append_pair("gameVersion", search.minecraft_version)
                    .append_pair("modLoaderType", &loader.to_string());
            }
            query
                .append_pair("sortField", SORT_FIELD_POPULARITY)
                .append_pair("sortOrder", "desc")
                .append_pair("index", &search.index.to_string())
                .append_pair("pageSize", &search.page_size.min(MAX_PAGE_SIZE).to_string());
            // Sent only when there is something to search for. An empty
            // `searchFilter` is not the same request as no filter at all.
            if !search.text.trim().is_empty() {
                query.append_pair("searchFilter", search.text.trim());
            }
        }
        let (items, pagination): (Vec<CurseForgeProject>, _) =
            self.api.page_with_key(&url, &self.key, cancel).await?;
        Ok(page(items, search.index, pagination))
    }

    /// Every file of one project that fits an exact Minecraft release and
    /// loader.
    ///
    /// `GET /v1/mods/{modId}/files`, filtered server-side. Deliberately not the
    /// project's whole file list: CurseForge pages that list, and a project
    /// with a decade of releases would cost a dozen round trips to enumerate
    /// for an answer that is thrown away by the compatibility check anyway.
    ///
    /// The consequence is stated rather than hidden: a file that does not fit
    /// this instance is not returned at all, so nothing here can explain an
    /// incompatible release the way a full listing could.
    pub(super) async fn project_files(
        &self,
        project: CurseForgeProjectId,
        minecraft_version: &str,
        loader: LoaderKind,
        cancel: &CancellationToken,
    ) -> ModPlatformResult<Vec<CurseForgeFileDocument>> {
        let loader_value =
            loader_query_value(loader).ok_or(ModPlatformError::LoaderCannotHostMods { loader })?;

        let mut collected: Vec<CurseForgeFileDocument> = Vec::new();
        let mut index = 0_u32;
        for _ in 0..MAX_FILE_PAGES {
            let mut url = self
                .api
                .endpoint(&["mods", &project.to_string(), "files"])?;
            url.query_pairs_mut()
                .append_pair("gameVersion", minecraft_version)
                .append_pair("modLoaderType", &loader_value.to_string())
                .append_pair("index", &index.to_string())
                .append_pair("pageSize", &MAX_PAGE_SIZE.to_string());

            let (documents, pagination): (Vec<CurseForgeFileDocument>, _) =
                match self.api.page_with_key(&url, &self.key, cancel).await {
                    Ok(answer) => answer,
                    // A 404 here means the project itself is gone, which is
                    // worth saying rather than reporting as an outage.
                    Err(ModPlatformError::HttpStatus { status, .. })
                        if status == StatusCode::NOT_FOUND =>
                    {
                        return Err(ModPlatformError::ProjectUnknown {
                            provider: ModProvider::CurseForge,
                            reference: project.to_string(),
                        });
                    }
                    Err(other) => return Err(other),
                };

            let received = documents.len();
            collected.extend(documents);
            // The provider's own count decides whether another page exists.
            let more = pagination.is_some_and(|pagination| {
                u64::from(index).saturating_add(received as u64) < pagination.total_count
            });
            if received == 0 || !more {
                break;
            }
            index = index.saturating_add(u32::try_from(received).unwrap_or(MAX_PAGE_SIZE));
        }
        Ok(collected)
    }

    /// The project one slug names, if the game's Mods class has one.
    ///
    /// CurseForge addresses projects by number and publishes no by-slug route,
    /// so the documented search endpoint's own `slug` filter is what answers
    /// this. Deliberately not narrowed by Minecraft release or loader: a slug
    /// names a project, and which of its files fit an instance is a later and
    /// separate question.
    pub(super) async fn project_by_slug(
        &self,
        game_id: u32,
        class_id: u32,
        slug: &str,
        cancel: &CancellationToken,
    ) -> ModPlatformResult<Option<CurseForgeProject>> {
        let mut url = self.api.endpoint(&["mods", "search"])?;
        url.query_pairs_mut()
            .append_pair("gameId", &game_id.to_string())
            .append_pair("classId", &class_id.to_string())
            .append_pair("slug", slug)
            .append_pair("pageSize", "1");
        let (found, _): (Vec<CurseForgeProject>, _) =
            self.api.page_with_key(&url, &self.key, cancel).await?;
        Ok(found.into_iter().next())
    }

    /// Several projects in one request.
    ///
    /// `POST /v1/mods` with the ids in the body, which is the documented way to
    /// ask about many projects at once. A finished plan needs one project
    /// document per resolved mod, and asking one at a time would spend a
    /// rate-limit budget on round trips.
    pub(super) async fn projects(
        &self,
        projects: &[CurseForgeProjectId],
        cancel: &CancellationToken,
    ) -> ModPlatformResult<Vec<CurseForgeProject>> {
        if projects.is_empty() {
            return Ok(Vec::new());
        }
        let url = self.api.endpoint(&["mods"])?;
        let ids: Vec<u32> = projects.iter().map(|project| project.get()).collect();
        let (documents, _) = self
            .api
            .send_with_key::<Vec<CurseForgeProject>>(
                Method::POST,
                &url,
                Some(serde_json::json!({ "modIds": ids })),
                &self.key,
                cancel,
            )
            .await?;
        Ok(documents)
    }

    /// Several exact files in one request.
    ///
    /// `POST /v1/mods/files` with the file ids in the body. Each document names
    /// the project it belongs to, which is what lets an exactly pinned file be
    /// resolved without the caller having to know its project up front.
    pub(super) async fn files(
        &self,
        files: &[CurseForgeFileId],
        cancel: &CancellationToken,
    ) -> ModPlatformResult<Vec<CurseForgeFileDocument>> {
        if files.is_empty() {
            return Ok(Vec::new());
        }
        let url = self.api.endpoint(&["mods", "files"])?;
        let ids: Vec<u32> = files.iter().map(|file| file.get()).collect();
        let (documents, _) = self
            .api
            .send_with_key::<Vec<CurseForgeFileDocument>>(
                Method::POST,
                &url,
                Some(serde_json::json!({ "fileIds": ids })),
                &self.key,
                cancel,
            )
            .await?;
        Ok(documents)
    }

    /// Which catalog releases these files are, by fingerprint.
    ///
    /// `POST /v1/fingerprints`. The one lookup that works from bytes alone,
    /// which is what a jar out of a published server archive is: it arrives
    /// with a name and no identity, and CurseForge indexes exactly this hash
    /// for exactly this question.
    ///
    /// The answer is keyed back by SHA-1 rather than by the fingerprint that
    /// was sent. A fingerprint is a lookup key with no collision guarantee, so
    /// a match is only believed when the release CurseForge named publishes the
    /// same digest the file on disk has. A match that fails that check is
    /// dropped rather than recorded, leaving the file unidentified -- which is
    /// the honest answer and the safe one.
    pub async fn fingerprints(
        &self,
        wanted: &[(u32, Hash)],
        cancel: &CancellationToken,
    ) -> ModPlatformResult<BTreeMap<String, RecognizedFile>> {
        if wanted.is_empty() {
            return Ok(BTreeMap::new());
        }
        let url = self.api.endpoint(&["fingerprints"])?;
        let sent: Vec<u32> = wanted.iter().map(|(fingerprint, _)| *fingerprint).collect();
        let (matches, _) = self
            .api
            .send_with_key::<CurseForgeFingerprintMatches>(
                Method::POST,
                &url,
                Some(serde_json::json!({ "fingerprints": sent })),
                &self.key,
                cancel,
            )
            .await?;

        let expected: BTreeSet<&str> = wanted.iter().map(|(_, hash)| hash.digest()).collect();
        let mut recognized = BTreeMap::new();
        for matched in matches.exact_matches {
            let dependencies = file_relations(&matched.file.dependencies);
            for digest in supported_hashes(&matched.file.hashes) {
                if !expected.contains(digest.digest()) {
                    continue;
                }
                recognized.insert(
                    digest.digest().to_owned(),
                    RecognizedFile {
                        provider: ModProvider::CurseForge,
                        project_id: matched.id.to_string(),
                        version_id: matched.file.id.to_string(),
                        file_id: Some(matched.file.id.to_string()),
                        dependencies: dependencies.clone(),
                    },
                );
            }
        }
        debug!(
            asked = wanted.len(),
            recognized = recognized.len(),
            "matched installed files against the curseforge catalog"
        );
        Ok(recognized)
    }
}

/// One page of results, with a total that is never smaller than what this page
/// already proves exists.
fn page<T>(
    items: Vec<T>,
    index: u32,
    pagination: Option<CurseForgePagination>,
) -> CurseForgePage<T> {
    let proven = u64::from(index).saturating_add(items.len() as u64);
    CurseForgePage {
        total: pagination.map_or(proven, |pagination| pagination.total_count.max(proven)),
        next_offset: pagination.and_then(|pagination| {
            crate::model::next_search_offset(
                pagination.index,
                pagination.result_count,
                pagination.total_count,
            )
        }),
        items,
    }
}

/// How CurseForge names the loaders Ephervex supports, and the numbers its
/// query parameters spell them with.
///
/// Both halves of the pair matter and they are different things. The number is
/// the API's `modLoaderType` enum and never leaves this module. The name is
/// what a file document *lists itself* as supporting, and is what compatibility
/// is decided against.
///
/// Only Forge and NeoForge appear, because only Forge and NeoForge are loaders
/// this launcher installs. A loader Ephervex does not support has no entry, and
/// [`loader_query_value`] answers `None` for it rather than falling back to
/// some other loader's filter: a search that quietly became a Fabric search
/// would offer mods that cannot run in the instance being browsed.
const LOADERS: &[(LoaderKind, u32, &str)] = &[
    (LoaderKind::Forge, 1, "Forge"),
    (LoaderKind::NeoForge, 6, "NeoForge"),
];

/// The `modLoaderType` value a loader is filtered by, when this launcher
/// supports it.
pub(super) fn loader_query_value(loader: LoaderKind) -> Option<u32> {
    LOADERS
        .iter()
        .find(|(kind, _, _)| *kind == loader)
        .map(|(_, value, _)| *value)
}

/// CurseForge's own name for a loader, when this launcher supports it.
pub(super) fn loader_name(loader: LoaderKind) -> Option<&'static str> {
    LOADERS
        .iter()
        .find(|(kind, _, _)| *kind == loader)
        .map(|(_, _, name)| *name)
}

/// Every loader name the API's `modLoaderType` enum documents.
///
/// Used to split a file's single `gameVersions` array (which mixes Minecraft
/// releases and loader names) into the two things Ephervex reasons about.
/// The list covers loaders this launcher does *not* support on purpose: a
/// `Fabric` entry has to be recognized as a loader so that it is not mistaken
/// for a Minecraft release, even though no Ephervex instance can use it.
pub(super) const DOCUMENTED_LOADER_NAMES: &[&str] = &[
    "Forge",
    "Cauldron",
    "LiteLoader",
    "Fabric",
    "Quilt",
    "NeoForge",
];

/// The `gameVersions` entries in the API's "Environment" version type, which
/// states which side a file runs on.
///
/// This is a second pseudo-version type mixed into the same array as
/// [`DOCUMENTED_LOADER_NAMES`] and actual Minecraft releases. A file names one
/// side, both, or neither: naming one side and not the other is the file's own
/// claim to be exclusive to it, and naming neither leaves the question
/// undeclared rather than answered.
pub(super) const CURSEFORGE_CLIENT_ENVIRONMENT: &str = "Client";
pub(super) const CURSEFORGE_SERVER_ENVIRONMENT: &str = "Server";
pub(super) const CURSEFORGE_ENVIRONMENT_NAMES: &[&str] =
    &[CURSEFORGE_CLIENT_ENVIRONMENT, CURSEFORGE_SERVER_ENVIRONMENT];

/// The `sortField` a browse uses.
///
/// Popularity, descending, which is what CurseForge's own catalog opens on.
/// One fixed order rather than a setting: the Ephervex browser exposes no sort
/// control, and an unstated ordering that varies per provider is worse than a
/// documented one.
const SORT_FIELD_POPULARITY: &str = "2";

/// The largest page the API will answer with.
///
/// The documented ceiling on `pageSize`. Requests are clamped to it rather
/// than refused, and the launcher's own page size is smaller anyway.
const MAX_PAGE_SIZE: u32 = 50;

/// How many pages of one project's files a version listing will ask for.
///
/// The listing is already narrowed to one exact Minecraft release and one
/// loader, which for a real project is a few dozen files at most, so the first
/// page normally ends the loop. The ceiling exists so that a project with an
/// implausible number of files cannot turn opening a version picker into an
/// unbounded crawl.
const MAX_FILE_PAGES: u32 = 5;

/// What a search is for.
///
/// Every field is a constraint the browser actually applies. There is no
/// category, author, or featured filter here, because the Ephervex browser has
/// no control that would set one and a query parameter is not a reason to grow
/// a UI.
#[derive(Debug, Clone)]
pub(super) struct CurseForgeSearch<'a> {
    /// Free text, as the user typed it. Empty is a valid search and means
    /// "everything that fits", which is what an empty field asks for.
    pub text: &'a str,
    /// The exact Minecraft release the instance runs. Sent as a filter unless
    /// `include_incompatible` is set; never replaced by a neighboring release.
    pub minecraft_version: &'a str,
    /// The instance's loader. Sent as a filter unless `include_incompatible`
    /// is set, but still required so an unsupported loader is refused.
    pub loader: LoaderKind,
    /// Whether projects the release and loader filters would exclude are
    /// wanted too, so the query leaves both filters off.
    pub include_incompatible: bool,
    /// The provider-discovered Mods class. Never a hard-coded number.
    pub class_id: u32,
    pub index: u32,
    pub page_size: u32,
}

/// One page of a listing, with what the provider said about the rest of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CurseForgePage<T> {
    pub items: Vec<T>,
    /// How many entries the whole listing has, as the provider reported it.
    ///
    /// Falls back to what this page proves (the offset plus what it carried)
    /// when a response omits `pagination`, so a caller is never told there is
    /// more than has actually been seen.
    pub total: u64,
    /// Where the provider says the next page begins, read from its own
    /// `index` and `resultCount` rather than from the offset this request
    /// sent or how many items ended up in `items`. `None` when a response
    /// omits `pagination` at all: without it there is nothing to fabricate a
    /// further page from.
    pub next_offset: Option<u32>,
}

/// One game's top-level project classes, as one [`CurseForgeOperation::project_classes`]
/// request answered.
///
/// The one authoritative reading of what a project's `classId` means: which
/// numeric id is Mods, which is Resource Packs or Shader Packs where the
/// response named one, and what CurseForge itself calls every other class it
/// listed, so a refusal can name the *class*, never the unrelated project's
/// own display name. [`Self::classify`] is where that reading happens; what
/// an operation does with the result is that operation's own policy, not
/// this type's.
#[derive(Debug, Clone)]
pub struct CurseForgeProjectClasses {
    mods: u32,
    /// The Resource Packs class id, when this response named one. See
    /// [`RESOURCE_PACKS_CLASS_SLUG`] for why this is optional where
    /// [`Self::mods`] is not.
    resource_packs: Option<u32>,
    /// The Shader Packs class id, on the same optional terms as
    /// [`Self::resource_packs`]. See [`SHADER_PACKS_CLASS_SLUG`].
    shader_packs: Option<u32>,
    names: HashMap<u32, String>,
}

impl CurseForgeProjectClasses {
    /// Whether this class id is the game's Mods class.
    #[must_use]
    pub fn is_mods(&self, class_id: u32) -> bool {
        class_id == self.mods
    }

    /// Whether this class id is the game's Resource Packs class.
    #[must_use]
    pub fn is_resource_packs(&self, class_id: u32) -> bool {
        Some(class_id) == self.resource_packs
    }

    /// Whether this class id is the game's Shader Packs class.
    #[must_use]
    pub fn is_shader_packs(&self, class_id: u32) -> bool {
        Some(class_id) == self.shader_packs
    }

    /// The Mods class id this response reported.
    ///
    /// For building a query that asks for mods and nothing else. It is this
    /// response's own answer: a later operation asks again rather than reusing
    /// a number this one happened to see.
    #[must_use]
    pub const fn mods_class_id(&self) -> u32 {
        self.mods
    }

    /// The one authoritative reading of a project's provider class.
    ///
    /// Every caller, the mod browser and a CurseForge pack import alike,
    /// classifies a project through this method rather than re-deriving what
    /// a `classId` means, so the two cannot arrive at different answers about
    /// the same project. What differs between them is *policy*: which classes
    /// each operation accepts, and what it does with one it does not. This
    /// method only classifies; see [`Self::rejection_for`] for the
    /// mod-browser policy built on top of it.
    ///
    /// Fails closed rather than returning a class, for two cases neither of
    /// which is guessed at:
    ///
    /// - no reported class at all (`classId` is documented as nullable);
    /// - a class id this response's own categories never listed.
    ///
    /// Never derived from anything but `classId` itself: not a file name, not
    /// an extension, not the project's title or slug.
    pub fn classify(
        &self,
        project: &CurseForgeProject,
    ) -> ModPlatformResult<CurseForgeProjectClass> {
        let class_id = project
            .class_id
            .ok_or_else(|| ModPlatformError::ResponseInvalid {
                provider: ModProvider::CurseForge,
                detail: format!(
                    "project {} reports no project class, so Ephervex cannot confirm its content type",
                    project.id
                ),
            })?;
        if self.is_mods(class_id) {
            return Ok(CurseForgeProjectClass::Mods);
        }
        if self.is_resource_packs(class_id) {
            return Ok(CurseForgeProjectClass::ResourcePacks);
        }
        if self.is_shader_packs(class_id) {
            return Ok(CurseForgeProjectClass::ShaderPacks);
        }
        match self.name_of(class_id) {
            Some(name) => Ok(CurseForgeProjectClass::Other(name.to_owned())),
            None => Err(ModPlatformError::ResponseInvalid {
                provider: ModProvider::CurseForge,
                detail: format!(
                    "project {} reports class {class_id}, which the game's category classes did not list",
                    project.id
                ),
            }),
        }
    }

    /// The typed refusal for one classified project, or `None` when it is a
    /// Mod.
    ///
    /// The mod-browser policy built on [`Self::classify`]: Ephervex v1
    /// browses and manages Mods only, so every other class, Resource Packs
    /// and Shader Packs included, is refused by the provider's own name for
    /// it. A CurseForge modpack import applies its own, wider policy directly
    /// on [`Self::classify`] instead of calling this.
    #[must_use]
    pub fn rejection_for(&self, project: &CurseForgeProject) -> Option<ModPlatformError> {
        match self.classify(project) {
            Ok(CurseForgeProjectClass::Mods) => None,
            Ok(CurseForgeProjectClass::ResourcePacks) => {
                Some(ModPlatformError::ProjectTypeUnsupported {
                    reference: project.id.to_string(),
                    project_type: self
                        .resource_packs
                        .and_then(|id| self.name_of(id))
                        .unwrap_or("Resource Packs")
                        .to_owned(),
                })
            }
            Ok(CurseForgeProjectClass::ShaderPacks) => {
                Some(ModPlatformError::ProjectTypeUnsupported {
                    reference: project.id.to_string(),
                    project_type: self
                        .shader_packs
                        .and_then(|id| self.name_of(id))
                        .unwrap_or("Shader Packs")
                        .to_owned(),
                })
            }
            Ok(CurseForgeProjectClass::Other(project_type)) => {
                Some(ModPlatformError::ProjectTypeUnsupported {
                    reference: project.id.to_string(),
                    project_type,
                })
            }
            Err(error) => Some(error),
        }
    }

    /// The provider-returned name of a top-level class this response listed,
    /// if any of them carried this id.
    ///
    /// `None` for a class id this response never named: a project reporting
    /// one is not resolved to a guessed name, and stays for the caller to
    /// fail closed on.
    #[must_use]
    pub fn name_of(&self, class_id: u32) -> Option<&str> {
        self.names.get(&class_id).map(String::as_str)
    }
}

/// One project's provider-managed content class, as
/// [`CurseForgeProjectClasses::classify`] authoritatively read it.
///
/// Classification is the one fact every caller agrees on; what to do about it
/// is not. The mod browser and a CurseForge pack import both classify a
/// project through the same method and then apply their own, different
/// policy to the result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CurseForgeProjectClass {
    /// Minecraft's top-level Mods class.
    Mods,
    /// Minecraft's top-level Resource Packs class.
    ResourcePacks,
    /// Minecraft's top-level Shader Packs class.
    ShaderPacks,
    /// A top-level class this build does not special-case, named the way
    /// CurseForge itself names it in the same categories response that
    /// answered which class is Mods.
    Other(String),
}

impl CurseForgeProjectClass {
    /// The path segment CurseForge's own site uses for a project of this
    /// class, e.g. `mc-mods` in `curseforge.com/minecraft/mc-mods/<slug>`.
    ///
    /// The same slugs classification itself reads a category response for,
    /// so a browser link built from this and a project's own slug needs no
    /// second request. `None` for [`Self::Other`]: this build never confirms
    /// a project of an unrecognized class in the first place, so nothing
    /// downstream ever needs a link for one.
    #[must_use]
    pub const fn browser_path_segment(&self) -> Option<&'static str> {
        match self {
            Self::Mods => Some(MODS_CLASS_SLUG),
            Self::ResourcePacks => Some(RESOURCE_PACKS_CLASS_SLUG),
            Self::ShaderPacks => Some(SHADER_PACKS_CLASS_SLUG),
            Self::Other(_) => None,
        }
    }
}

/// One exact CurseForge file, normalized and checked against what was asked
/// for.
///
/// The provider's wire shape stops here: everything above works from
/// [`ModFile`], the two ids that name it, and this exact file's own
/// client/server declaration - read from its `gameVersions`, not guessed at
/// from the project it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurseForgeFileRelease {
    pub project: CurseForgeProjectId,
    pub file: CurseForgeFileId,
    /// The name, address, size, and verifiable digests, in the provider-neutral
    /// shape the rest of the launcher installs from.
    pub artifact: ModFile,
    /// What this exact file's own `gameVersions` says about which side(s) it
    /// runs on. See [`file_declared_sides`] for how it is read.
    pub client_side: SideSupport,
    pub server_side: SideSupport,
    /// The Minecraft releases this exact file's own `gameVersions` names, read
    /// the same way [`super::mapping::version`] reads them for the mod
    /// browser. Lets a caller that already knows which file it wants also ask
    /// whether this build of it fits a particular Minecraft release.
    pub game_versions: Vec<String>,
    /// When CurseForge published this file. See [`published_at`] for how an
    /// absent or unparsable date is read.
    pub published_at: DateTime<Utc>,
    /// What this file's document says it needs from other projects, read the
    /// same way the mod browser reads it. Recorded into an imported pack's
    /// recipe so the instance can answer dependency questions later without
    /// asking CurseForge again (ADR 0034).
    pub dependencies: Vec<ModDependency>,
}

/// Checks one file document against the request and normalizes it.
///
/// Every refusal here is a distinct thing a user can be told and, where it is
/// their pack's problem rather than a fault, act on.
fn normalize_file(
    project: CurseForgeProjectId,
    file: CurseForgeFileId,
    document: CurseForgeFileDocument,
) -> ModPlatformResult<CurseForgeFileRelease> {
    if document.mod_id != project.get() || document.id != file.get() {
        return Err(ModPlatformError::FileIdentityMismatch {
            provider: ModProvider::CurseForge,
            requested: format!("{project}/{file}"),
            returned: format!("{}/{}", document.mod_id, document.id),
        });
    }
    if !document.is_available {
        return Err(ModPlatformError::FileUnavailable {
            provider: ModProvider::CurseForge,
            project_id: project.to_string(),
            file_id: file.to_string(),
        });
    }

    let hashes = supported_hashes(&document.hashes);

    // Null rather than absent, and it means the project's owner turned off
    // third-party distribution. Recorded rather than refused: everything else
    // the API said about the file is still true, and a caller that already
    // holds a verified copy - or that can ask the user for one - needs all of
    // it. There is no second address to try either way: assembling a CDN path
    // out of the ids would route around a decision the author made.
    let distribution = document
        .download_url
        .map(|url| url.trim().to_owned())
        .filter(|url| !url.is_empty())
        .map_or(Distribution::Restricted, Distribution::Download);

    if hashes.is_empty() {
        // Verification is mandatory. A file with no digest this build can check
        // is one it will not install, rather than one it trusts because of the
        // address it came from - and for a restricted file the digest is also
        // the only way to recognize a hand-placed copy.
        return Err(ModPlatformError::HashesMissing {
            version_id: file.to_string(),
        });
    }

    let (client_side, server_side) = file_declared_sides(&document.game_versions);
    let (game_versions, _loaders) = split_declared_support(&document.game_versions);
    let published = published_at(document.file_date.as_deref());

    Ok(CurseForgeFileRelease {
        project,
        file,
        artifact: ModFile {
            file_name: document.file_name,
            distribution,
            size: document.file_length,
            hashes,
        },
        client_side,
        server_side,
        game_versions,
        published_at: published,
        dependencies: file_relations(&document.dependencies),
    })
}

/// The digests from a file document this build can verify with.
///
/// SHA-1 only, because it is the one cryptographic hash CurseForge publishes
/// that Ephervex's hash model has. MD5 is published beside it and is not used:
/// it is not stronger, and a weaker digest is not a substitute for one that is
/// present. The fingerprint CurseForge also publishes is a lookup key rather
/// than a cryptographic hash and is not read at all.
///
/// A digest whose tag and shape disagree is dropped rather than trusted:
/// `Hash::new` enforces the hex length the algorithm has.
pub(super) fn supported_hashes(published: &[CurseForgeFileHash]) -> Vec<Hash> {
    published
        .iter()
        .filter(|hash| hash.algo == HASH_ALGO_SHA1)
        .filter_map(|hash| Hash::new(HashAlgorithm::Sha1, hash.value.trim()))
        .collect()
}

/// Whether this status is CurseForge refusing the credential rather than
/// failing the request, and the typed failure it means.
///
/// `401` and `403` are the two the user can act on: the key is wrong, expired,
/// or no longer authorized, and the answer is to set a new one. Everything else
/// is a status about the request.
///
/// Public because the API is not the only thing that sends the key. An artifact
/// download carries it too, and it refuses a bad one with the same statuses -
/// so the decision of what those statuses mean is made once, here, rather than
/// once per caller that has to make it.
#[must_use]
pub fn credential_refusal(status: u16) -> Option<ModPlatformError> {
    (status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN).then_some(
        ModPlatformError::CredentialRejected {
            provider: ModProvider::CurseForge,
            status,
        },
    )
}

/// Turns a non-success status into the failure it means.
///
/// Everything a [`credential_refusal`] does not claim - including the `3xx` that
/// a refused cross-origin redirect leaves behind - is a status this launcher
/// cannot use.
fn classify(status: StatusCode) -> ModPlatformResult<()> {
    if status.is_success() {
        return Ok(());
    }
    let status = status.as_u16();
    Err(
        credential_refusal(status).unwrap_or(ModPlatformError::HttpStatus {
            provider: ModProvider::CurseForge,
            status,
        }),
    )
}

/// Turns a transport failure into this crate's own.
///
/// `reqwest`'s own message carries the address it was working on, which is
/// wanted, and never a request header, which is why it is safe to keep.
fn request_failed(source: &reqwest::Error) -> ModPlatformError {
    ModPlatformError::RequestFailed {
        provider: ModProvider::CurseForge,
        detail: source.to_string(),
    }
}

/// A base URL or client this build cannot make requests with.
fn unusable(detail: &str) -> ModPlatformError {
    ModPlatformError::ResponseInvalid {
        provider: ModProvider::CurseForge,
        detail: detail.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use ephervex_model::SecretString;

    use super::super::credential::ConfiguredApiKey;
    use super::*;
    use ephervex_model::ModRelationKind;

    const FAKE_KEY: &str = "test-curseforge-key-do-not-use";

    fn api(base: &str) -> CurseForgeApi {
        CurseForgeApi::with_base_url(
            Arc::new(ConfiguredApiKey::new(SecretString::new(FAKE_KEY))),
            &HttpConfig::for_loopback_tests(),
            base,
        )
        .expect("a client builds")
    }

    #[test]
    fn the_production_root_is_the_documented_third_party_api() {
        assert_eq!(CURSEFORGE_API_BASE, "https://api.curseforge.com/v1");
        assert_eq!(MINECRAFT_GAME_ID, 432);
    }

    #[test]
    fn the_credential_header_is_the_third_party_api_key_and_not_the_upload_token() {
        // Two different credential systems for two different services. Sending
        // one where the other belongs fails in a way nobody can diagnose.
        assert_eq!(CURSEFORGE_API_KEY_HEADER, "x-api-key");
        assert_ne!(
            CURSEFORGE_API_KEY_HEADER.to_ascii_lowercase(),
            "x-api-token"
        );
    }

    #[test]
    fn endpoints_are_built_under_the_configured_root() {
        let client = api("http://127.0.0.1:9/v1");
        assert_eq!(
            client.endpoint(&["games", "432"]).expect("a url").as_str(),
            "http://127.0.0.1:9/v1/games/432"
        );
        assert_eq!(
            client
                .endpoint(&["mods", "238222"])
                .expect("a url")
                .as_str(),
            "http://127.0.0.1:9/v1/mods/238222"
        );
        // A trailing slash on the root must not double up in the path.
        assert_eq!(
            api("http://127.0.0.1:9/v1/")
                .endpoint(&["games", "432"])
                .expect("a url")
                .as_str(),
            "http://127.0.0.1:9/v1/games/432"
        );
    }

    #[test]
    fn no_endpoint_puts_the_key_in_the_address() {
        // The key is a header. A query parameter would end up in server logs,
        // in browser history if the address were ever opened, and in any
        // referrer the API happened to send.
        let url = api("http://127.0.0.1:9/v1")
            .endpoint(&["games", "432"])
            .expect("a url");
        assert!(url.query().is_none());
        assert!(!url.as_str().contains(FAKE_KEY));
    }

    #[test]
    fn a_plaintext_root_is_refused_unless_a_test_has_opted_in() {
        let error = CurseForgeApi::with_base_url(
            Arc::new(ConfiguredApiKey::absent()),
            &HttpConfig::default(),
            "http://api.curseforge.com/v1",
        )
        .expect_err("production refuses plaintext");
        assert!(matches!(error, ModPlatformError::ResponseInvalid { .. }));
    }

    #[test]
    fn the_presence_of_a_key_is_answered_without_a_request() {
        // The settings screen asks this, and saving a credential must never
        // depend on being online.
        assert!(
            api("https://api.curseforge.com/v1")
                .has_credential()
                .expect("read")
        );
        let unconfigured =
            CurseForgeApi::new(Arc::new(ConfiguredApiKey::absent()), &HttpConfig::default())
                .expect("a client builds");
        assert!(!unconfigured.has_credential().expect("read"));
    }

    #[test]
    fn statuses_are_classified_into_what_the_user_can_do_about_them() {
        assert!(classify(StatusCode::OK).is_ok());
        for status in [StatusCode::UNAUTHORIZED, StatusCode::FORBIDDEN] {
            let error = classify(status).expect_err("a refusal");
            assert!(error.is_credential_problem(), "{error:?}");
        }
        for status in [
            StatusCode::NOT_FOUND,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::INTERNAL_SERVER_ERROR,
            // What a refused cross-origin redirect leaves behind.
            StatusCode::FOUND,
        ] {
            let error = classify(status).expect_err("a refusal");
            assert!(!error.is_credential_problem(), "{status}: {error:?}");
            assert!(matches!(error, ModPlatformError::HttpStatus { .. }));
        }
    }

    #[tokio::test]
    async fn a_request_canceled_before_it_starts_never_reaches_the_network() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        // Port 9 discards everything, so anything that did reach the network
        // would fail with a transport error rather than a cancellation.
        let error = api("http://127.0.0.1:9/v1")
            .game(MINECRAFT_GAME_ID, &cancel)
            .await
            .expect_err("canceled");
        assert!(error.is_cancellation(), "{error:?}");
    }

    // -- the exact-file endpoint -------------------------------------------

    fn project() -> CurseForgeProjectId {
        CurseForgeProjectId::new(238_222).expect("a real id")
    }

    fn file_id() -> CurseForgeFileId {
        CurseForgeFileId::new(4_593_548).expect("a real id")
    }

    fn document() -> CurseForgeFileDocument {
        serde_json::from_str(
            r#"{"id":4593548,"gameId":432,"modId":238222,"isAvailable":true,
                "fileName":"jei-1.20.1-15.2.0.27.jar","fileLength":1234567,
                "downloadUrl":"https://mediafilez.forgecdn.net/files/4593/548/jei.jar",
                "hashes":[{"value":"da39a3ee5e6b4b0d3255bfef95601890afd80709","algo":1},
                          {"value":"0123456789abcdef0123456789abcdef","algo":2}]}"#,
        )
        .expect("a documented file response")
    }

    #[test]
    fn the_exact_file_endpoint_addresses_the_project_and_the_file_it_was_given() {
        assert_eq!(
            api("http://127.0.0.1:9/v1")
                .endpoint(&["mods", "238222", "files", "4593548"])
                .expect("a url")
                .as_str(),
            "http://127.0.0.1:9/v1/mods/238222/files/4593548"
        );
    }

    #[test]
    fn a_file_document_normalizes_to_the_address_size_and_digest_a_download_needs() {
        let release = normalize_file(project(), file_id(), document()).expect("a usable file");
        assert_eq!(release.project, project());
        assert_eq!(release.file, file_id());
        assert_eq!(release.artifact.file_name, "jei-1.20.1-15.2.0.27.jar");
        assert_eq!(
            release.artifact.distribution.download_url(),
            Some("https://mediafilez.forgecdn.net/files/4593/548/jei.jar")
        );
        assert_eq!(release.artifact.size, Some(1_234_567));
    }

    #[test]
    fn only_the_cryptographic_digest_this_build_can_verify_is_kept() {
        // MD5 is published beside SHA-1 and is not stronger. Nothing is
        // recorded from it, and the fingerprint is not read at all.
        let release = normalize_file(project(), file_id(), document()).expect("a usable file");
        let algorithms: Vec<HashAlgorithm> = release
            .artifact
            .hashes
            .iter()
            .map(|hash| hash.algorithm)
            .collect();
        assert_eq!(algorithms, vec![HashAlgorithm::Sha1]);
        assert_eq!(
            release.artifact.hashes[0].digest(),
            "da39a3ee5e6b4b0d3255bfef95601890afd80709"
        );
    }

    #[test]
    fn a_digest_whose_tag_and_shape_disagree_is_dropped_rather_than_trusted() {
        let mut document = document();
        // Tagged SHA-1, sized like an MD5.
        document.hashes = vec![CurseForgeFileHash {
            value: "0123456789abcdef0123456789abcdef".to_owned(),
            algo: HASH_ALGO_SHA1,
        }];
        let error = normalize_file(project(), file_id(), document).expect_err("must refuse");
        assert!(matches!(error, ModPlatformError::HashesMissing { .. }));
    }

    #[test]
    fn a_file_with_no_verifiable_digest_is_refused_rather_than_trusted_by_address() {
        let mut document = document();
        document.hashes.retain(|hash| hash.algo != HASH_ALGO_SHA1);
        let error = normalize_file(project(), file_id(), document).expect_err("must refuse");
        assert!(
            matches!(error, ModPlatformError::HashesMissing { ref version_id } if version_id == "4593548"),
            "{error:?}"
        );
    }

    #[test]
    fn a_response_about_another_file_of_the_same_project_is_never_substituted() {
        // The whole point of an exact import: project 238222 file 4593548 means
        // that file, not the newest one the project happens to have.
        let mut newer = document();
        newer.id = 4_593_549;
        let error = normalize_file(project(), file_id(), newer).expect_err("must refuse");
        assert!(
            matches!(
                error,
                ModPlatformError::FileIdentityMismatch { ref requested, ref returned, .. }
                    if requested == "238222/4593548" && returned == "238222/4593549"
            ),
            "{error:?}"
        );
    }

    #[test]
    fn a_response_about_another_project_is_refused_too() {
        let mut elsewhere = document();
        elsewhere.mod_id = 999;
        let error = normalize_file(project(), file_id(), elsewhere).expect_err("must refuse");
        assert!(matches!(
            error,
            ModPlatformError::FileIdentityMismatch { .. }
        ));
    }

    #[test]
    fn a_file_the_provider_will_not_serve_is_still_a_file_it_described() {
        let mut unavailable = document();
        unavailable.is_available = false;
        assert!(
            matches!(
                normalize_file(project(), file_id(), unavailable).expect_err("must refuse"),
                ModPlatformError::FileUnavailable { .. }
            ),
            "a withdrawn file is a refusal; a restricted one is not"
        );

        for (label, address) in [("null", None), ("blank", Some("   ".to_owned()))] {
            let mut restricted = document();
            restricted.download_url = address;
            let release =
                normalize_file(project(), file_id(), restricted).unwrap_or_else(|error| {
                    panic!("a {label} address must not lose the file: {error}")
                });
            assert!(
                release.artifact.distribution.is_restricted(),
                "{label}: the author's decision is recorded rather than an address invented"
            );
            assert_eq!(release.artifact.distribution.download_url(), None);
            // Everything CurseForge did say survives the restriction.
            assert_eq!(release.artifact.file_name, "jei-1.20.1-15.2.0.27.jar");
            assert_eq!(release.artifact.size, Some(1_234_567));
            assert_eq!(release.artifact.hashes.len(), 1);
        }
    }

    #[test]
    fn a_restricted_file_keeps_the_relations_its_document_declared() {
        // The regression case: project 391382's files forbid third-party
        // downloads, and the dependencies they declare must survive that.
        let mut restricted: CurseForgeFileDocument = serde_json::from_str(
            r#"{"id":7951520,"gameId":432,"modId":391382,"isAvailable":true,
                "fileName":"restricted-1.0.0.jar","fileLength":42,
                "fileDate":"2025-03-04T05:06:07Z","releaseType":1,
                "gameVersions":["1.20.1","NeoForge","Client"],
                "hashes":[{"value":"da39a3ee5e6b4b0d3255bfef95601890afd80709","algo":1}],
                "dependencies":[{"modId":238222,"relationType":3},
                                {"modId":306612,"relationType":2}]}"#,
        )
        .expect("a documented file response");
        restricted.download_url = None;

        let release = normalize_file(
            CurseForgeProjectId::new(391_382).expect("a project id"),
            CurseForgeFileId::new(7_951_520).expect("a file id"),
            restricted,
        )
        .expect("a described file");

        assert!(release.artifact.distribution.is_restricted());
        assert_eq!(release.game_versions, vec!["1.20.1".to_owned()]);
        assert_eq!(release.client_side, SideSupport::Required);
        assert_eq!(
            release.published_at.to_rfc3339(),
            "2025-03-04T05:06:07+00:00"
        );
        let kinds: Vec<_> = release
            .dependencies
            .iter()
            .map(|relation| (relation.kind, relation.project_id.clone()))
            .collect();
        assert_eq!(
            kinds,
            vec![
                (ModRelationKind::Required, Some("238222".to_owned())),
                (ModRelationKind::Optional, Some("306612".to_owned())),
            ],
            "a download restriction must not turn declared relations into unknown"
        );
    }

    #[tokio::test]
    async fn an_operation_reads_the_credential_once_and_never_renders_it() {
        let client = api("http://127.0.0.1:9/v1");
        let operation = client.begin_operation().expect("a configured key");
        assert_eq!(operation.credential().expose(), FAKE_KEY);
        assert!(
            !format!("{operation:?}").contains(FAKE_KEY),
            "{operation:?}"
        );
    }

    #[tokio::test]
    async fn an_operation_cannot_begin_without_a_key_so_no_request_is_ever_made() {
        let api = CurseForgeApi::with_base_url(
            Arc::new(ConfiguredApiKey::absent()),
            &HttpConfig::for_loopback_tests(),
            "http://127.0.0.1:9/v1",
        )
        .expect("a client builds");
        assert!(matches!(
            api.begin_operation().expect_err("no key is configured"),
            ModPlatformError::CredentialMissing { .. }
        ));
    }

    #[tokio::test]
    async fn a_missing_key_fails_before_any_request_is_attempted() {
        let api = CurseForgeApi::with_base_url(
            Arc::new(ConfiguredApiKey::absent()),
            &HttpConfig::for_loopback_tests(),
            "http://127.0.0.1:9/v1",
        )
        .expect("a client builds");

        let error = api
            .game(MINECRAFT_GAME_ID, &CancellationToken::new())
            .await
            .expect_err("no key is configured");
        // A transport failure here would mean the request was made anyway.
        assert!(
            matches!(error, ModPlatformError::CredentialMissing { .. }),
            "{error:?}"
        );
    }

    // -- pagination continuation --------------------------------------------

    fn pagination(index: u32, result_count: u64, total_count: u64) -> CurseForgePagination {
        CurseForgePagination {
            index,
            result_count,
            total_count,
        }
    }

    #[test]
    fn the_next_offset_is_read_from_the_providers_own_index_and_result_count() {
        // Requested at 0, but the provider's own numbers are what decide the
        // continuation, not the request or how many items came back.
        let page = page(vec!["a", "b"], 0, Some(pagination(0, 2, 10)));
        assert_eq!(page.next_offset, Some(2));
    }

    #[test]
    fn a_page_reaching_the_providers_own_total_has_no_continuation() {
        let page = page(vec!["a", "b"], 8, Some(pagination(8, 2, 10)));
        assert_eq!(page.next_offset, None);
    }

    #[test]
    fn no_pagination_member_never_fabricates_a_further_page() {
        // A response that omits `pagination` entirely gives nothing to
        // continue from, so this page is treated as the last one rather than
        // guessed at from how many items it happened to carry.
        let page = page(vec!["a", "b", "c"], 0, None);
        assert_eq!(page.next_offset, None);
        assert_eq!(page.total, 3);
    }

    #[test]
    fn a_result_count_smaller_than_the_items_returned_is_still_trusted() {
        // The provider's own count of what it consumed is authoritative even
        // when it disagrees with how many items this page happened to carry.
        let page = page(vec!["a", "b"], 0, Some(pagination(0, 1, 10)));
        assert_eq!(page.next_offset, Some(1));
    }
}
