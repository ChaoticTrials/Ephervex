//! The Modrinth provider against a loopback server serving fixtures.
//!
//! Everything here runs against `wiremock` on 127.0.0.1. No test contacts
//! Modrinth, so ordinary CI never depends on a live service.
//!
//! What these cover that the unit tests cannot: the request actually sent, and
//! what the provider does with a response whose search facets promised more
//! than the versions deliver.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use ephervex_download::{
    CacheLayout, DownloadManager, DownloaderConfig, HttpConfig, OperationReporter, RetryPolicy,
};
use ephervex_mod_platform::{
    HostSide, InstallSelection, InstanceTarget, ModDependency, ModPlatformError, ModProviderApi,
    ModSearchQuery, ModrinthProvider, ResolveRequest, SideSupport, resolve,
};
use ephervex_model::{Hash, HashAlgorithm, LoaderKind, ModProvider, ModRelationKind};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn fixture(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/modrinth/api")
        .join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

struct Catalog {
    server: MockServer,
    cache: TempDir,
    provider: ModrinthProvider,
}

impl Catalog {
    async fn start() -> Self {
        let server = MockServer::start().await;
        let cache = tempfile::tempdir().expect("temp dir");

        // A manager that gives up immediately: an unmocked route should fail
        // the test quickly rather than work through a backoff schedule.
        let manager = DownloadManager::new(DownloaderConfig {
            retry: RetryPolicy::none(),
            http: HttpConfig::for_loopback_tests(),
            ..DownloaderConfig::default()
        })
        .expect("client builds");

        let provider = ModrinthProvider::with_base_url(
            manager,
            CacheLayout::new(cache.path()),
            &format!("{}/v2", server.uri()),
        )
        .expect("provider");

        Self {
            server,
            cache,
            provider,
        }
    }

    /// Every file the provider left in the metadata store, relative to it.
    fn metadata_entries(&self) -> Vec<PathBuf> {
        let root = self.cache.path().join("metadata");
        let mut entries = Vec::new();
        let mut pending = vec![root.clone()];
        while let Some(directory) = pending.pop() {
            let Ok(listing) = std::fs::read_dir(&directory) else {
                continue;
            };
            for entry in listing.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    entries.push(
                        path.strip_prefix(&root)
                            .expect("listed beneath the store")
                            .to_path_buf(),
                    );
                }
            }
        }
        entries
    }

    async fn serving(self, route: &str, body: String) -> Self {
        Mock::given(method("GET"))
            .and(path(route))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/json"))
            .mount(&self.server)
            .await;
        self
    }

    /// The full catalog the search fixtures describe.
    async fn full() -> Self {
        Self::start()
            .await
            .serving("/v2/search", fixture("search-mods.json"))
            .await
            .serving(
                "/v2/project/FITSPROJ/version",
                fixture("versions-fits.json"),
            )
            .await
            .serving("/v2/project/OLDPROJ/version", fixture("versions-old.json"))
            .await
            .serving(
                "/v2/project/SPLITPROJ/version",
                fixture("versions-split.json"),
            )
            .await
            .serving(
                "/v2/project/LIBPROJ/version",
                fixture("versions-library.json"),
            )
            .await
            .serving(
                "/v2/project/EXTRAPROJ/version",
                fixture("versions-optional-extra.json"),
            )
            .await
    }

    async fn requests(&self) -> Vec<Request> {
        self.server
            .received_requests()
            .await
            .expect("the server records requests")
    }

    /// The facets one search actually sent.
    async fn sent_facets(&self) -> String {
        self.requests()
            .await
            .iter()
            .find(|request| request.url.path() == "/v2/search")
            .and_then(|request| {
                request
                    .url
                    .query_pairs()
                    .find(|(key, _)| key == "facets")
                    .map(|(_, value)| value.into_owned())
            })
            .expect("a search was sent")
    }
}

fn target() -> InstanceTarget {
    InstanceTarget::new("1.20.1", LoaderKind::NeoForge)
}

fn reporter() -> OperationReporter {
    OperationReporter::silent("browsing-mods")
}

#[tokio::test]
async fn a_search_narrows_upstream_on_the_exact_release_and_loader() {
    let catalog = Catalog::full().await;
    let results = catalog
        .provider
        .search(
            &target(),
            &ModSearchQuery::new("test"),
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect("search");

    let facets = catalog.sent_facets().await;
    assert!(facets.contains(r#"["project_type:mod"]"#));
    assert!(facets.contains(r#"["categories:neoforge"]"#));
    assert!(facets.contains(r#"["versions:1.20.1"]"#));
    assert_eq!(results.provider, ephervex_model::ModProvider::Modrinth);
}

#[tokio::test]
async fn compatible_only_is_the_default_and_drops_what_the_facets_oversold() {
    // SPLITPROJ satisfies `versions:1.20.1` through one version and
    // `categories:neoforge` through another, so the facets return it and no
    // single version of it can actually be installed.
    let catalog = Catalog::full().await;
    let results = catalog
        .provider
        .search(
            &target(),
            &ModSearchQuery::new("test"),
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect("search");

    let ids: Vec<&str> = results
        .hits
        .iter()
        .map(|hit| hit.project.id.as_str())
        .collect();
    assert_eq!(ids, vec!["FITSPROJ"]);
    assert!(
        results
            .hits
            .iter()
            .all(|hit| hit.compatibility.is_compatible())
    );
    assert_eq!(
        results.hits[0].best_version.as_ref().expect("a version").id,
        "FITSNEW1",
        "the newest version that actually fits"
    );
    // The fixture's raw page carries three hits and reports `total_hits: 3`,
    // so the provider has been fully consumed even though only one of those
    // three survived the compatibility check. Deriving the continuation from
    // `hits.len() < total_hits` (1 < 3) would wrongly say there is more.
    assert_eq!(results.next_offset, None);
}

#[tokio::test]
async fn showing_incompatible_results_widens_the_query_and_explains_each_one() {
    let catalog = Catalog::full().await;
    let results = catalog
        .provider
        .search(
            &target(),
            &ModSearchQuery::new("test").including_incompatible(true),
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect("search");

    // A filter cannot both narrow the list and leave the rows that need
    // annotating in it.
    let facets = catalog.sent_facets().await;
    assert_eq!(facets, r#"[["project_type:mod"]]"#);

    let ids: Vec<&str> = results
        .hits
        .iter()
        .map(|hit| hit.project.id.as_str())
        .collect();
    assert_eq!(ids, vec!["FITSPROJ", "OLDPROJ", "SPLITPROJ"]);

    let old = &results.hits[1];
    assert!(!old.compatibility.is_compatible());
    assert!(
        old.best_version.is_none(),
        "an incompatible result is not quietly offered as installable"
    );
    assert!(
        !old.compatibility.reasons().is_empty(),
        "an incompatible result says why"
    );
}

#[tokio::test]
async fn one_search_stays_within_its_request_budget() {
    // Compatibility cannot be read off a search response, so each hit costs a
    // version lookup. That is a rate-limit budget, and it has a ceiling.
    let catalog = Catalog::full().await;
    catalog
        .provider
        .search(
            &target(),
            &ModSearchQuery::new("test").including_incompatible(true),
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect("search");

    let requests = catalog.requests().await;
    assert_eq!(
        requests.len(),
        4,
        "one search plus one version list per hit, and nothing else"
    );
}

#[tokio::test]
async fn a_raw_page_smaller_than_the_total_continues_from_what_it_carried() {
    // `total_hits` (5) is larger than the two raw hits this page actually
    // carries, standing in for a page that is not the last one.
    let catalog = Catalog::start()
        .await
        .serving(
            "/v2/search",
            r#"{"hits":[
                {"project_id":"FITSPROJ","slug":"fits","title":"Fits","description":"",
                 "author":"someone","client_side":"required","server_side":"required"},
                {"project_id":"LIBPROJ","slug":"lib","title":"Library","description":"",
                 "author":"someone","client_side":"required","server_side":"required"}
            ],"offset":0,"limit":10,"total_hits":5}"#
                .to_owned(),
        )
        .await
        .serving(
            "/v2/project/FITSPROJ/version",
            fixture("versions-fits.json"),
        )
        .await
        .serving(
            "/v2/project/LIBPROJ/version",
            fixture("versions-library.json"),
        )
        .await;

    let results = catalog
        .provider
        .search(
            &target(),
            &ModSearchQuery::new("test"),
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect("search");

    assert_eq!(results.hits.len(), 2, "both hits fit this instance");
    assert_eq!(
        results.next_offset,
        Some(2),
        "the raw page carried two entries; the continuation follows that count, not total_hits"
    );
}

#[tokio::test]
async fn a_page_size_is_clamped_before_it_reaches_the_provider() {
    let catalog = Catalog::full().await;
    catalog
        .provider
        .search(
            &target(),
            &ModSearchQuery::new("test").limited(200),
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect("search");

    let sent = catalog
        .requests()
        .await
        .iter()
        .find(|request| request.url.path() == "/v2/search")
        .and_then(|request| {
            request
                .url
                .query_pairs()
                .find(|(key, _)| key == "limit")
                .map(|(_, value)| value.into_owned())
        })
        .expect("a limit was sent");
    assert_eq!(sent, "25");
}

#[tokio::test]
async fn a_version_list_is_fetched_once_and_then_served_from_the_cache() {
    // Two searches, one version list per project: the second read costs no
    // request, which is what makes browsing usable inside a rate limit.
    let catalog = Catalog::full().await;
    for _ in 0..2 {
        catalog
            .provider
            .versions(
                "FITSPROJ",
                &target(),
                &reporter(),
                &CancellationToken::new(),
            )
            .await
            .expect("versions");
    }

    let lookups = catalog
        .requests()
        .await
        .iter()
        .filter(|request| request.url.path() == "/v2/project/FITSPROJ/version")
        .count();
    assert_eq!(lookups, 1);
}

#[tokio::test]
async fn a_cached_version_list_is_named_by_a_digest_rather_than_by_the_project_that_asked_for_it() {
    // A slug is whatever an author published and a reference is whatever a user
    // typed; neither has a length this launcher controls, and a filename does.
    // An encrypted home directory refuses names past 143 bytes, and the cache
    // lives under `$XDG_CACHE_HOME`.
    let enormous = "l".repeat(4_000);
    let catalog = Catalog::start()
        .await
        .serving(
            &format!("/v2/project/{enormous}/version"),
            fixture("versions-fits.json"),
        )
        .await;

    catalog
        .provider
        .versions(&enormous, &target(), &reporter(), &CancellationToken::new())
        .await
        .expect("versions");

    let entries = catalog.metadata_entries();
    assert_eq!(
        entries.len(),
        1,
        "expected one cached document: {entries:?}"
    );
    let components: Vec<String> = entries[0]
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();
    let [namespace, shard, filename] = components.as_slice() else {
        panic!("expected <namespace>/<shard>/<digest>.json, got {components:?}");
    };

    assert_eq!(namespace, "modrinth");
    assert_eq!(shard.len(), 2);
    let digest = filename
        .strip_suffix(".json")
        .expect("a cached document is json");
    assert_eq!(digest.len(), 64);
    assert!(digest.starts_with(shard.as_str()));
    assert!(
        digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "`{digest}` is not lowercase hexadecimal"
    );
    assert!(
        !entries[0].to_string_lossy().contains(&enormous),
        "the reference reached the path: {}",
        entries[0].display()
    );

    // The document is still the one that was fetched, read back through the
    // provider's own path without another request.
    let versions = catalog
        .provider
        .versions(&enormous, &target(), &reporter(), &CancellationToken::new())
        .await
        .expect("versions");
    assert!(!versions.is_empty());
    assert_eq!(
        catalog
            .requests()
            .await
            .iter()
            .filter(|request| request.url.path().ends_with("/version"))
            .count(),
        1,
        "the second read should have been served from the cache"
    );
}

#[tokio::test]
async fn a_project_reference_cannot_smuggle_a_path_into_the_request() {
    let catalog = Catalog::start().await;
    // Unmocked on purpose: what matters is the path that was requested.
    let _ = catalog
        .provider
        .project("../../admin", &reporter(), &CancellationToken::new())
        .await;

    let requested: Vec<String> = catalog
        .requests()
        .await
        .iter()
        .map(|request| request.url.path().to_owned())
        .collect();
    assert!(
        requested
            .iter()
            .all(|path| !path.contains("/admin") || path.contains("%2F")),
        "a reference from a command line must not choose an endpoint: {requested:?}"
    );
}

#[tokio::test]
async fn resolving_from_the_catalog_installs_the_mod_and_its_required_library() {
    let catalog = Catalog::full().await;
    // The resolver reads only ids and side support from the batch project
    // endpoint, which is where a mod's declared server support comes from.
    Mock::given(method("GET"))
        .and(path("/v2/projects"))
        .and(query_param("ids", r#"["FITSPROJ","LIBPROJ"]"#))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"[{"id":"FITSPROJ","title":"Fits","description":"","project_type":"mod",
                 "client_side":"required","server_side":"required"},
                {"id":"LIBPROJ","title":"Library","description":"","project_type":"mod",
                 "client_side":"required","server_side":"unknown"}]"#,
            "application/json",
        ))
        .mount(&catalog.server)
        .await;

    let plan = resolve(ResolveRequest {
        provider: &catalog.provider,
        target: &target(),
        loader_name: "neoforge",
        selection: &InstallSelection::new("FITSPROJ"),
        installed: &[],
        installed_policy: ephervex_mod_platform::InstalledPolicy::Preserve,
        reporter: &reporter(),
        cancel: &CancellationToken::new(),
    })
    .await
    .expect("a plan");

    assert_eq!(plan.root.version.id, "FITSNEW1");
    assert_eq!(plan.root.version.file.file_name, "fits-2.0.0.jar");
    assert_eq!(plan.dependencies.len(), 1);
    assert_eq!(plan.dependencies[0].version.project_id, "LIBPROJ");
    assert_eq!(
        plan.dependencies[0].project.server_side,
        ephervex_mod_platform::SideSupport::Unknown,
        "an undeclared side reaches the plan as undeclared"
    );

    // The optional extra is offered and not taken.
    assert_eq!(plan.optional.len(), 1);
    assert_eq!(plan.optional[0].project_id.as_deref(), Some("EXTRAPROJ"));
    assert!(
        plan.to_install()
            .iter()
            .all(|planned| planned.version.project_id != "EXTRAPROJ"),
        "an unselected optional installs nothing"
    );
}

/// One version document of a single-project catalog, declaring `environment`.
fn version_with_environment(environment: &str) -> String {
    format!(
        r#"[{{"id":"SIDEV1","project_id":"SIDEPROJ","name":"Side 1","version_number":"1.0.0",
            "version_type":"release","game_versions":["1.20.1"],"loaders":["neoforge"],
            "date_published":"2026-01-01T00:00:00Z","environment":{environment},
            "files":[{{"hashes":{{"sha1":"da39a3ee5e6b4b0d3255bfef95601890afd80709"}},
                       "url":"https://cdn.modrinth.com/data/SIDEPROJ/versions/SIDEV1/side.jar",
                       "filename":"side.jar","primary":true,"size":1}}],
            "dependencies":[]}}]"#
    )
}

/// Plans `SIDEPROJ`, whose project says `required` on both sides, from a
/// catalog whose only version declares `environment`.
async fn planned_sides(environment: &str) -> (SideSupport, SideSupport) {
    let catalog = Catalog::start()
        .await
        .serving(
            "/v2/project/SIDEPROJ/version",
            version_with_environment(environment),
        )
        .await;
    Mock::given(method("GET"))
        .and(path("/v2/projects"))
        .and(query_param("ids", r#"["SIDEPROJ"]"#))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"[{"id":"SIDEPROJ","title":"Side","description":"","project_type":"mod",
                 "client_side":"required","server_side":"required"}]"#,
            "application/json",
        ))
        .mount(&catalog.server)
        .await;

    let plan = resolve(ResolveRequest {
        provider: &catalog.provider,
        target: &target(),
        loader_name: "neoforge",
        selection: &InstallSelection::new("SIDEPROJ"),
        installed: &[],
        installed_policy: ephervex_mod_platform::InstalledPolicy::Preserve,
        reporter: &reporter(),
        cancel: &CancellationToken::new(),
    })
    .await
    .expect("a plan");
    (plan.root.client_side(), plan.root.server_side())
}

#[tokio::test]
async fn a_selected_versions_environment_overrides_its_projects_server_side() {
    assert_eq!(
        planned_sides(r#""client_only""#).await,
        (SideSupport::Required, SideSupport::Unsupported),
        "a client-only version is not a server mod because its project is"
    );
}

#[tokio::test]
async fn a_version_that_declares_no_environment_takes_its_projects_sides() {
    for environment in ["null", r#""unknown""#, r#""something-new""#] {
        assert_eq!(
            planned_sides(environment).await,
            (SideSupport::Required, SideSupport::Required),
            "{environment}"
        );
    }
}

#[tokio::test]
async fn the_primary_file_rule_is_applied_to_real_response_shapes() {
    let catalog = Catalog::start()
        .await
        .serving(
            "/v2/project/FILESPROJ/version",
            fixture("versions-primary-files.json"),
        )
        .await;

    let versions = catalog
        .provider
        .versions(
            "FILESPROJ",
            &target(),
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect("versions");

    let one = versions
        .iter()
        .find(|version| version.id == "ONEPRIMARY")
        .expect("present");
    assert_eq!(
        one.file.file_name, "mod.jar",
        "the file marked primary wins over the sources jar listed before it"
    );

    let none = versions
        .iter()
        .find(|version| version.id == "NOPRIMARY")
        .expect("present");
    assert_eq!(
        none.file.file_name, "first.jar",
        "with nothing marked primary the first file is used, as the api specifies"
    );
}

/// How a provider that cannot answer a project lookup is reported: only a
/// not-found response may say the project does not exist.
async fn failing_catalog(status: u16) -> Catalog {
    let catalog = Catalog::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(status))
        .mount(&catalog.server)
        .await;
    catalog
}

#[tokio::test]
async fn a_not_found_answer_makes_a_project_unknown() {
    let catalog = failing_catalog(404).await;
    let cancel = CancellationToken::new();
    let project = catalog
        .provider
        .project("NOTHING", &reporter(), &cancel)
        .await
        .expect_err("the project does not exist");
    assert!(
        matches!(project, ModPlatformError::ProjectUnknown { .. }),
        "got {project:?}"
    );
    let versions = catalog
        .provider
        .versions("NOTHING", &target(), &reporter(), &cancel)
        .await
        .expect_err("the project does not exist");
    assert!(
        matches!(versions, ModPlatformError::ProjectUnknown { .. }),
        "got {versions:?}"
    );
}

#[tokio::test]
async fn a_server_error_is_an_outage_rather_than_an_unknown_project() {
    for status in [500, 503] {
        let catalog = failing_catalog(status).await;
        let cancel = CancellationToken::new();
        let project = catalog
            .provider
            .project("jei", &reporter(), &cancel)
            .await
            .expect_err("the provider is failing");
        assert!(
            project.is_offline() && !matches!(project, ModPlatformError::ProjectUnknown { .. }),
            "{status}: {project:?}"
        );
        let versions = catalog
            .provider
            .versions("jei", &target(), &reporter(), &cancel)
            .await
            .expect_err("the provider is failing");
        assert!(
            versions.is_offline() && !matches!(versions, ModPlatformError::ProjectUnknown { .. }),
            "{status}: {versions:?}"
        );
    }
}

#[tokio::test]
async fn a_refused_connection_is_an_outage_rather_than_an_unknown_project() {
    // Bound but never listening, so no other test's server can take the port
    // while connections to it are refused. Held until the test ends.
    let reserved = tokio::net::TcpSocket::new_v4().expect("socket");
    reserved
        .bind("127.0.0.1:0".parse().expect("loopback address"))
        .expect("reserve a loopback port");
    let closed = reserved.local_addr().expect("bound address");
    let cache = tempfile::tempdir().expect("temp dir");
    let manager = DownloadManager::new(DownloaderConfig {
        retry: RetryPolicy::none(),
        http: HttpConfig::for_loopback_tests(),
        ..DownloaderConfig::default()
    })
    .expect("client builds");
    let provider = ModrinthProvider::with_base_url(
        manager,
        CacheLayout::new(cache.path()),
        &format!("http://{closed}/v2"),
    )
    .expect("provider");
    let cancel = CancellationToken::new();
    let project = provider
        .project("jei", &reporter(), &cancel)
        .await
        .expect_err("nothing is listening");
    let versions = provider
        .versions("jei", &target(), &reporter(), &cancel)
        .await
        .expect_err("nothing is listening");
    for error in [project, versions] {
        assert!(
            error.is_offline() && !matches!(error, ModPlatformError::ProjectUnknown { .. }),
            "{error:?}"
        );
    }
}

#[tokio::test]
async fn a_read_timeout_is_an_outage_rather_than_an_unknown_project() {
    let catalog = Catalog::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_delay(std::time::Duration::from_secs(30)))
        .mount(&catalog.server)
        .await;
    let manager = DownloadManager::new(DownloaderConfig {
        retry: RetryPolicy::none(),
        http: HttpConfig {
            read_timeout: std::time::Duration::from_millis(200),
            ..HttpConfig::for_loopback_tests()
        },
        ..DownloaderConfig::default()
    })
    .expect("client builds");
    let provider = ModrinthProvider::with_base_url(
        manager,
        CacheLayout::new(catalog.cache.path()),
        &format!("{}/v2", catalog.server.uri()),
    )
    .expect("provider");
    let error = provider
        .project("jei", &reporter(), &CancellationToken::new())
        .await
        .expect_err("the provider never answers");
    assert!(
        error.is_offline() && !matches!(error, ModPlatformError::ProjectUnknown { .. }),
        "{error:?}"
    );
}

#[tokio::test]
async fn an_explicit_cancellation_is_reported_as_one_on_every_metadata_path() {
    let catalog = Catalog::full().await;
    let cancel = CancellationToken::new();
    cancel.cancel();
    let ids = vec!["FITSPROJ".to_owned()];

    let project = catalog
        .provider
        .project("FITSPROJ", &reporter(), &cancel)
        .await
        .expect_err("canceled");
    let versions = catalog
        .provider
        .versions("FITSPROJ", &target(), &reporter(), &cancel)
        .await
        .expect_err("canceled");
    let projects = catalog
        .provider
        .projects_by_id(&ids, &reporter(), &cancel)
        .await
        .expect_err("canceled");
    let versions_by_id = catalog
        .provider
        .versions_by_id(&ids, &reporter(), &cancel)
        .await
        .expect_err("canceled");
    for error in [project, versions, projects, versions_by_id] {
        assert!(error.is_cancellation(), "got {error:?}");
    }
}

/// Recognizing installed files is one request for the whole batch, keyed by the
/// digest the launcher already computed. It needs no credential and no
/// fingerprint of its own, which is the whole difference from CurseForge.
#[tokio::test]
async fn installed_files_are_recognized_by_digest_in_one_request() {
    let catalog = Catalog::start().await;
    let known = "a".repeat(40);
    let unknown = "b".repeat(40);
    Mock::given(method("POST"))
        .and(path("/v2/version_files"))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            r#"{{"{known}":{{"id":"vAbCdEfG","project_id":"AANobbMI","date_published":"2024-01-01T00:00:00Z","files":[]}}}}"#
        )))
        .mount(&catalog.server)
        .await;

    let digests = vec![
        Hash::new(HashAlgorithm::Sha1, &known).expect("sha1"),
        Hash::new(HashAlgorithm::Sha1, &unknown).expect("sha1"),
    ];
    let recognized = catalog
        .provider
        .recognize(&digests, &CancellationToken::new())
        .await
        .expect("the lookup answers");

    assert_eq!(recognized.len(), 1);
    let found = recognized
        .get(&known)
        .expect("the known file is recognized");
    assert_eq!(found.provider, ModProvider::Modrinth);
    assert_eq!(found.project_id, "AANobbMI");
    assert_eq!(found.version_id, "vAbCdEfG");
    assert_eq!(
        found.file_id, None,
        "modrinth names a version and has no separate file id"
    );
    assert_eq!(
        catalog
            .server
            .received_requests()
            .await
            .unwrap_or_default()
            .len(),
        1,
        "a batch is one request however many files it asks about"
    );
}

/// The recognized release's own `environment` is carried with the answer, and a
/// release declaring none stays undeclared rather than becoming a yes.
#[tokio::test]
async fn a_recognized_release_carries_the_sides_its_environment_declares() {
    let catalog = Catalog::start().await;
    let server_only = "a".repeat(40);
    let silent = "b".repeat(40);
    Mock::given(method("POST"))
        .and(path("/v2/version_files"))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            r#"{{"{server_only}":{{"id":"v1","project_id":"P1","environment":"server_only","date_published":"2024-01-01T00:00:00Z","files":[]}},"{silent}":{{"id":"v2","project_id":"P2","date_published":"2024-01-01T00:00:00Z","files":[]}}}}"#
        )))
        .mount(&catalog.server)
        .await;

    let digests = vec![
        Hash::new(HashAlgorithm::Sha1, &server_only).expect("sha1"),
        Hash::new(HashAlgorithm::Sha1, &silent).expect("sha1"),
    ];
    let recognized = catalog
        .provider
        .recognize(&digests, &CancellationToken::new())
        .await
        .expect("the lookup answers");

    let declared = &recognized[&server_only];
    assert_eq!(
        (declared.client_side, declared.server_side),
        (SideSupport::Unsupported, SideSupport::Required)
    );
    let undeclared = &recognized[&silent];
    assert_eq!(
        (undeclared.client_side, undeclared.server_side),
        (SideSupport::Unknown, SideSupport::Unknown)
    );
}

/// What a recognition request asked for, read back from the loopback capture.
async fn recognition_requests(catalog: &Catalog) -> Vec<(String, Vec<String>)> {
    catalog
        .server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|request| request.url.path() == "/v2/version_files")
        .map(|request| {
            let body: serde_json::Value =
                serde_json::from_slice(&request.body).expect("the request body is JSON");
            let algorithm = body["algorithm"].as_str().expect("an algorithm").to_owned();
            let hashes = body["hashes"]
                .as_array()
                .expect("a hash list")
                .iter()
                .map(|hash| hash.as_str().expect("a string").to_owned())
                .collect();
            (algorithm, hashes)
        })
        .collect()
}

/// The request names the algorithm the digest was computed with, so a SHA-512
/// is looked up as one and the answer is keyed by it.
#[tokio::test]
async fn a_sha512_digest_is_looked_up_as_sha512() {
    let catalog = Catalog::start().await;
    let known = "d".repeat(128);
    Mock::given(method("POST"))
        .and(path("/v2/version_files"))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            r#"{{"{known}":{{"id":"vSha512","project_id":"AANobbMI","date_published":"2024-01-01T00:00:00Z","files":[]}}}}"#
        )))
        .mount(&catalog.server)
        .await;

    let digests = vec![Hash::new(HashAlgorithm::Sha512, &known).expect("sha512")];
    let recognized = catalog
        .provider
        .recognize(&digests, &CancellationToken::new())
        .await
        .expect("the lookup answers");

    assert_eq!(
        recognition_requests(&catalog).await,
        [("sha512".to_owned(), vec![known.clone()])]
    );
    assert_eq!(
        recognized.get(&known).expect("recognized").version_id,
        "vSha512"
    );
}

/// Digests of different algorithms cannot share a request, because the request
/// carries one algorithm. Each group is sent under its own name.
#[tokio::test]
async fn a_mixed_batch_is_grouped_by_algorithm() {
    let catalog = Catalog::start().await;
    let sha1 = "a".repeat(40);
    let sha512 = "b".repeat(128);
    Mock::given(method("POST"))
        .and(path("/v2/version_files"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .mount(&catalog.server)
        .await;

    let digests = vec![
        Hash::new(HashAlgorithm::Sha512, &sha512).expect("sha512"),
        Hash::new(HashAlgorithm::Sha1, &sha1).expect("sha1"),
    ];
    catalog
        .provider
        .recognize(&digests, &CancellationToken::new())
        .await
        .expect("the lookup answers");

    let mut requests = recognition_requests(&catalog).await;
    requests.sort();
    assert_eq!(
        requests,
        [
            ("sha1".to_owned(), vec![sha1]),
            ("sha512".to_owned(), vec![sha512]),
        ]
    );
}

/// Modrinth cannot look up a SHA-256, and sending one under another
/// algorithm's name would ask about bytes nobody has. It stays unrecognized
/// and the digests Modrinth can look up are still asked about.
#[tokio::test]
async fn a_sha256_digest_is_never_sent_under_another_algorithm() {
    let catalog = Catalog::start().await;
    let sha1 = "a".repeat(40);
    let sha256 = "c".repeat(64);
    Mock::given(method("POST"))
        .and(path("/v2/version_files"))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            r#"{{"{sha1}":{{"id":"vKnown","project_id":"AANobbMI","date_published":"2024-01-01T00:00:00Z","files":[]}}}}"#
        )))
        .mount(&catalog.server)
        .await;

    let digests = vec![
        Hash::new(HashAlgorithm::Sha256, &sha256).expect("sha256"),
        Hash::new(HashAlgorithm::Sha1, &sha1).expect("sha1"),
    ];
    let recognized = catalog
        .provider
        .recognize(&digests, &CancellationToken::new())
        .await
        .expect("the lookup answers");

    assert_eq!(
        recognition_requests(&catalog).await,
        [("sha1".to_owned(), vec![sha1.clone()])]
    );
    assert_eq!(recognized.keys().collect::<Vec<_>>(), [&sha1]);

    let only_sha256 = vec![Hash::new(HashAlgorithm::Sha256, &sha256).expect("sha256")];
    let none = catalog
        .provider
        .recognize(&only_sha256, &CancellationToken::new())
        .await
        .expect("an unsupported algorithm is not a failure");
    assert!(none.is_empty());
    assert_eq!(
        recognition_requests(&catalog).await.len(),
        1,
        "a batch of only unsupported digests contacts nobody"
    );
}

/// The version that identified a file also says what it declares, and a
/// version declaring nothing is an answer, not a missing one.
#[tokio::test]
async fn a_recognized_file_carries_what_its_version_declares() {
    let catalog = Catalog::start().await;
    let feature = "a".repeat(40);
    let library = "b".repeat(40);
    Mock::given(method("POST"))
        .and(path("/v2/version_files"))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            r#"{{"{feature}":{{"id":"vFeature","project_id":"FEATURE1","date_published":"2024-01-01T00:00:00Z","files":[],"dependencies":[{{"project_id":"LIBRARY1","version_id":"vLibrary","dependency_type":"required"}},{{"file_name":"extras.jar","dependency_type":"optional"}}]}},"{library}":{{"id":"vLibrary","project_id":"LIBRARY1","date_published":"2024-01-01T00:00:00Z","files":[],"dependencies":[]}}}}"#
        )))
        .mount(&catalog.server)
        .await;

    let digests = vec![
        Hash::new(HashAlgorithm::Sha1, &feature).expect("sha1"),
        Hash::new(HashAlgorithm::Sha1, &library).expect("sha1"),
    ];
    let recognized = catalog
        .provider
        .recognize(&digests, &CancellationToken::new())
        .await
        .expect("the lookup answers");

    assert_eq!(
        recognized.get(&feature).expect("recognized").dependencies,
        [
            ModDependency {
                kind: ModRelationKind::Required,
                project_id: Some("LIBRARY1".to_owned()),
                version_id: Some("vLibrary".to_owned()),
                file_name: None,
            },
            ModDependency {
                kind: ModRelationKind::Optional,
                project_id: None,
                version_id: None,
                file_name: Some("extras.jar".to_owned()),
            },
        ]
    );
    assert_eq!(
        recognized.get(&library).expect("recognized").dependencies,
        []
    );
}

/// A dependency may give a release and no project. What a recipe records has to
/// say which project that is, so the release's owner is read from the release.
#[tokio::test]
async fn a_version_only_dependency_is_given_the_project_its_release_belongs_to() {
    let catalog = Catalog::start().await;
    let feature = "a".repeat(40);
    Mock::given(method("POST"))
        .and(path("/v2/version_files"))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            r#"{{"{feature}":{{"id":"vFeature","project_id":"FEATURE1","date_published":"2024-01-01T00:00:00Z","files":[],"dependencies":[{{"version_id":"vLibrary","dependency_type":"required"}},{{"version_id":"vRival","file_name":"rival.jar","dependency_type":"incompatible"}},{{"version_id":"vGone","dependency_type":"optional"}}]}}}}"#
        )))
        .mount(&catalog.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v2/versions"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"[{"id":"vLibrary","project_id":"LIBRARY1","date_published":"2024-01-01T00:00:00Z","files":[]},{"id":"vRival","project_id":"RIVAL1","date_published":"2024-01-01T00:00:00Z","files":[]}]"#,
        ))
        .mount(&catalog.server)
        .await;

    let digests = vec![Hash::new(HashAlgorithm::Sha1, &feature).expect("sha1")];
    let recognized = catalog
        .provider
        .recognize(&digests, &CancellationToken::new())
        .await
        .expect("the lookup answers");

    assert_eq!(
        recognized.get(&feature).expect("recognized").dependencies,
        [
            ModDependency {
                kind: ModRelationKind::Required,
                project_id: Some("LIBRARY1".to_owned()),
                version_id: Some("vLibrary".to_owned()),
                file_name: None,
            },
            ModDependency {
                kind: ModRelationKind::Incompatible,
                project_id: Some("RIVAL1".to_owned()),
                version_id: Some("vRival".to_owned()),
                file_name: Some("rival.jar".to_owned()),
            },
            ModDependency {
                kind: ModRelationKind::Optional,
                project_id: None,
                version_id: Some("vGone".to_owned()),
                file_name: None,
            },
        ],
        "a release the catalog does not answer for is left as stated"
    );
    let batches: Vec<_> = catalog
        .requests()
        .await
        .into_iter()
        .filter(|request| request.url.path() == "/v2/versions")
        .collect();
    assert_eq!(batches.len(), 1, "every pinned release is one request");
}

/// The same derivation applies to releases asked for by id, which is how a
/// refresh and a mod listing read what a release requires.
#[tokio::test]
async fn releases_asked_for_by_id_state_the_project_of_a_version_only_dependency() {
    let catalog = Catalog::start().await;
    Mock::given(method("GET"))
        .and(path("/v2/versions"))
        .and(query_param("ids", r#"["vFeature"]"#))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"[{"id":"vFeature","project_id":"FEATURE1","name":"Feature","version_number":"1","version_type":"release","game_versions":["1.20.1"],"loaders":["forge"],"date_published":"2024-01-01T00:00:00Z","files":[{"hashes":{"sha1":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},"url":"https://cdn.modrinth.com/data/F/versions/vFeature/feature.jar","filename":"feature.jar","primary":true,"size":1}],"dependencies":[{"version_id":"vLibrary","dependency_type":"required"}]}]"#,
        ))
        .mount(&catalog.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v2/versions"))
        .and(query_param("ids", r#"["vLibrary"]"#))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"[{"id":"vLibrary","project_id":"LIBRARY1","name":"Library","version_number":"1","version_type":"release","game_versions":["1.20.1"],"loaders":["forge"],"date_published":"2024-01-01T00:00:00Z","files":[{"hashes":{"sha1":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"},"url":"https://cdn.modrinth.com/data/L/versions/vLibrary/library.jar","filename":"library.jar","primary":true,"size":1}],"dependencies":[]}]"#,
        ))
        .mount(&catalog.server)
        .await;

    let versions = catalog
        .provider
        .versions_by_id(
            &["vFeature".to_owned()],
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect("the lookup answers");

    assert_eq!(versions.len(), 1);
    assert_eq!(
        versions[0].dependencies,
        [ModDependency {
            kind: ModRelationKind::Required,
            project_id: Some("LIBRARY1".to_owned()),
            version_id: Some("vLibrary".to_owned()),
            file_name: None,
        }]
    );
}

fn release_pinning(pin: &str, kind: &str) -> String {
    format!(
        r#"[{{"id":"vFeature","project_id":"FEATURE1","name":"Feature","version_number":"1","version_type":"release","game_versions":["1.20.1"],"loaders":["forge"],"date_published":"2024-01-01T00:00:00Z","files":[{{"hashes":{{"sha1":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}},"url":"https://cdn.modrinth.com/data/F/versions/vFeature/feature.jar","filename":"feature.jar","primary":true,"size":1}}],"dependencies":[{{"version_id":"{pin}","dependency_type":"{kind}"}}]}}]"#
    )
}

/// An optional extra that cannot be judged must not fail the release that
/// declares it, while a required relation still has to be named.
#[tokio::test]
async fn a_release_is_read_when_the_owner_of_its_optional_version_only_pin_cannot_be_looked_up() {
    for (kind, readable) in [("optional", true), ("required", false)] {
        let catalog = Catalog::start().await;
        Mock::given(method("GET"))
            .and(path("/v2/versions"))
            .and(query_param("ids", r#"["vFeature"]"#))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(release_pinning("vPinned", kind)),
            )
            .mount(&catalog.server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v2/versions"))
            .and(query_param("ids", r#"["vPinned"]"#))
            .respond_with(ResponseTemplate::new(500))
            .mount(&catalog.server)
            .await;

        let read = catalog
            .provider
            .versions_by_id(
                &["vFeature".to_owned()],
                &reporter(),
                &CancellationToken::new(),
            )
            .await;

        match (readable, read) {
            (true, Ok(versions)) => assert_eq!(
                versions[0].dependencies,
                [ModDependency {
                    kind: ModRelationKind::Optional,
                    project_id: None,
                    version_id: Some("vPinned".to_owned()),
                    file_name: None,
                }]
            ),
            (false, Err(_)) => {}
            (readable, read) => panic!("{kind}: readable={readable}, got {read:?}"),
        }
    }
}

/// The answer is about specific bytes on disk and is asked once per server, so
/// caching it would only risk naming a release for a file that has since gone.
#[tokio::test]
async fn recognizing_files_writes_nothing_to_the_metadata_store() {
    let catalog = Catalog::start().await;
    let digest = "c".repeat(40);
    Mock::given(method("POST"))
        .and(path("/v2/version_files"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .mount(&catalog.server)
        .await;

    let digests = vec![Hash::new(HashAlgorithm::Sha1, &digest).expect("sha1")];
    let recognized = catalog
        .provider
        .recognize(&digests, &CancellationToken::new())
        .await
        .expect("the lookup answers");

    assert!(
        recognized.is_empty(),
        "a file modrinth does not know is absent"
    );
    assert!(
        catalog.metadata_entries().is_empty(),
        "left behind: {:?}",
        catalog.metadata_entries()
    );
}

/// Nothing to ask about is not a request.
#[tokio::test]
async fn an_empty_recognition_batch_contacts_nobody() {
    let catalog = Catalog::start().await;

    let recognized = catalog
        .provider
        .recognize(&[], &CancellationToken::new())
        .await
        .expect("no request is made");

    assert!(recognized.is_empty());
    assert!(
        catalog
            .server
            .received_requests()
            .await
            .unwrap_or_default()
            .is_empty()
    );
}

fn project_document(id: &str, project_type: &str) -> String {
    format!(
        r#"{{"id":"{id}","title":"{id}","description":"","project_type":"{project_type}",
            "client_side":"required","server_side":"required"}}"#
    )
}

#[tokio::test]
async fn a_batch_lookup_refuses_a_project_that_is_not_a_mod_like_a_single_lookup() {
    for project_type in ["modpack", "resourcepack", "shader"] {
        let catalog = Catalog::start().await;
        let ids = vec!["FITSPROJ".to_owned(), "LIBPROJ".to_owned()];
        Mock::given(method("GET"))
            .and(path("/v2/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!(
                    "[{},{}]",
                    project_document("FITSPROJ", "mod"),
                    project_document("LIBPROJ", project_type)
                ),
                "application/json",
            ))
            .mount(&catalog.server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v2/project/LIBPROJ"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                project_document("LIBPROJ", project_type),
                "application/json",
            ))
            .mount(&catalog.server)
            .await;

        let single = catalog
            .provider
            .project("LIBPROJ", &reporter(), &CancellationToken::new())
            .await
            .expect_err("a single lookup refuses it");
        let batch = catalog
            .provider
            .projects_by_id(&ids, &reporter(), &CancellationToken::new())
            .await
            .expect_err("a batch lookup refuses it too");

        for error in [single, batch] {
            assert!(
                matches!(
                    &error,
                    ModPlatformError::ProjectTypeUnsupported { reference, project_type: found }
                        if reference == "LIBPROJ" && found == project_type
                ),
                "{project_type}: {error:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_batch_description_keeps_the_mods_and_names_the_projects_that_are_not() {
    let catalog = Catalog::start().await;
    Mock::given(method("GET"))
        .and(path("/v2/projects"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            format!(
                "[{},{},{}]",
                project_document("FITSPROJ", "mod"),
                project_document("PACKPROJ", "resourcepack"),
                project_document("LIBPROJ", "mod")
            ),
            "application/json",
        ))
        .mount(&catalog.server)
        .await;

    let batch = catalog
        .provider
        .project_batch_by_id(
            &[
                "FITSPROJ".to_owned(),
                "PACKPROJ".to_owned(),
                "LIBPROJ".to_owned(),
            ],
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect("one project that is not a mod does not fail the batch");

    let mods: Vec<&str> = batch
        .mods
        .iter()
        .map(|project| project.id.as_str())
        .collect();
    assert_eq!(mods, ["FITSPROJ", "LIBPROJ"]);
    assert_eq!(batch.not_mods.len(), 1);
    assert_eq!(batch.not_mods[0].id, "PACKPROJ");
    assert_eq!(batch.not_mods[0].project_type, "resourcepack");
}

#[tokio::test]
async fn a_batch_lookup_returns_every_project_when_all_are_mods() {
    let catalog = Catalog::start().await;
    Mock::given(method("GET"))
        .and(path("/v2/projects"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            format!(
                "[{},{}]",
                project_document("FITSPROJ", "mod"),
                project_document("LIBPROJ", "mod")
            ),
            "application/json",
        ))
        .mount(&catalog.server)
        .await;

    let projects = catalog
        .provider
        .projects_by_id(
            &["FITSPROJ".to_owned(), "LIBPROJ".to_owned()],
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect("both are mods");
    assert_eq!(projects.len(), 2);
}

#[tokio::test]
async fn a_dependency_that_is_not_a_mod_fails_resolution_instead_of_being_planned() {
    let catalog = Catalog::full().await;
    Mock::given(method("GET"))
        .and(path("/v2/projects"))
        .and(query_param("ids", r#"["FITSPROJ","LIBPROJ"]"#))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            format!(
                "[{},{}]",
                project_document("FITSPROJ", "mod"),
                project_document("LIBPROJ", "modpack")
            ),
            "application/json",
        ))
        .mount(&catalog.server)
        .await;

    let error = resolve(ResolveRequest {
        provider: &catalog.provider,
        target: &target(),
        loader_name: "neoforge",
        selection: &InstallSelection::new("FITSPROJ"),
        installed: &[],
        installed_policy: ephervex_mod_platform::InstalledPolicy::Preserve,
        reporter: &reporter(),
        cancel: &CancellationToken::new(),
    })
    .await
    .expect_err("a modpack is not installed into mods as a dependency");
    assert!(
        matches!(
            &error,
            ModPlatformError::ProjectTypeUnsupported { project_type, .. }
                if project_type == "modpack"
        ),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_project_that_does_not_state_its_type_is_refused_on_both_lookup_paths() {
    let catalog = Catalog::start().await;
    let untyped = r#"{"id":"NOTYPE","title":"No type","description":""}"#;
    Mock::given(method("GET"))
        .and(path("/v2/project/NOTYPE"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(untyped, "application/json"))
        .mount(&catalog.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v2/projects"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(format!("[{untyped}]"), "application/json"),
        )
        .mount(&catalog.server)
        .await;

    let single = catalog
        .provider
        .project("NOTYPE", &reporter(), &CancellationToken::new())
        .await
        .expect_err("an untyped project is not shown to be a mod");
    let batch = catalog
        .provider
        .projects_by_id(
            &["NOTYPE".to_owned()],
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect_err("an untyped project is not shown to be a mod");
    for error in [single, batch] {
        assert!(
            matches!(error, ModPlatformError::ResponseInvalid { .. }),
            "{error:?}"
        );
    }
}

/// The fits fixture with its newest release reported under another project, as
/// a malformed or confused upstream answer would.
fn fits_versions_with_a_foreign_newest_release() -> String {
    let mut versions: serde_json::Value =
        serde_json::from_str(&fixture("versions-fits.json")).expect("fixture is JSON");
    let foreign = versions
        .as_array_mut()
        .and_then(|all| all.iter_mut().find(|version| version["id"] == "FITSNEW1"))
        .expect("the newest release is in the fixture");
    foreign["project_id"] = serde_json::json!("OTHERPROJ");
    versions.to_string()
}

async fn catalog_with_a_foreign_newest_release() -> Catalog {
    Catalog::start()
        .await
        .serving("/v2/search", fixture("search-mods.json"))
        .await
        .serving(
            "/v2/project/FITSPROJ/version",
            fits_versions_with_a_foreign_newest_release(),
        )
        .await
        .serving("/v2/project/OLDPROJ/version", fixture("versions-old.json"))
        .await
        .serving(
            "/v2/project/SPLITPROJ/version",
            fixture("versions-split.json"),
        )
        .await
}

#[tokio::test]
async fn a_version_listing_leaves_out_releases_owned_by_another_project() {
    let catalog = catalog_with_a_foreign_newest_release().await;

    let versions = catalog
        .provider
        .project_versions(
            "FITSPROJ",
            &target(),
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect("versions");

    let ids: Vec<&str> = versions.iter().map(|version| version.id.as_str()).collect();
    assert!(
        !ids.contains(&"FITSNEW1"),
        "the release another project owns is not listed: {ids:?}"
    );
    assert!(ids.contains(&"FITSOLD1"));
    assert!(
        versions
            .iter()
            .all(|version| version.project_id == "FITSPROJ")
    );
}

#[tokio::test]
async fn a_search_hit_never_offers_a_release_owned_by_another_project() {
    let catalog = catalog_with_a_foreign_newest_release().await;

    let results = catalog
        .provider
        .search(
            &target(),
            &ModSearchQuery::new("test"),
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect("search");

    let hit = results
        .hits
        .iter()
        .find(|hit| hit.project.id == "FITSPROJ")
        .expect("the project still has an installable release");
    assert_eq!(
        hit.best_version.as_ref().expect("a version").id,
        "FITSOLD1",
        "the foreign newer release is not this project's"
    );
}

/// The fits fixture with one release made unmappable for the reason named and,
/// when `owner` is given, reported under that project.
fn fits_versions_with_an_unmappable_release(
    release: &str,
    owner: Option<&str>,
    reason: &str,
) -> String {
    let mut versions: serde_json::Value =
        serde_json::from_str(&fixture("versions-fits.json")).expect("fixture is JSON");
    let broken = versions
        .as_array_mut()
        .and_then(|all| all.iter_mut().find(|version| version["id"] == release))
        .expect("the release is in the fixture");
    if let Some(owner) = owner {
        broken["project_id"] = serde_json::json!(owner);
    }
    match reason {
        "no supported hash" => broken["files"][0]["hashes"] = serde_json::json!({}),
        "ambiguous primary file" => {
            let second = broken["files"][0].clone();
            broken["files"]
                .as_array_mut()
                .expect("files is an array")
                .push(second);
        }
        other => panic!("unknown reason {other}"),
    }
    versions.to_string()
}

async fn catalog_serving_fits_versions(body: String) -> Catalog {
    Catalog::start()
        .await
        .serving("/v2/search", fixture("search-mods.json"))
        .await
        .serving("/v2/project/FITSPROJ/version", body)
        .await
        .serving("/v2/project/OLDPROJ/version", fixture("versions-old.json"))
        .await
        .serving(
            "/v2/project/SPLITPROJ/version",
            fixture("versions-split.json"),
        )
        .await
}

#[tokio::test]
async fn a_version_listing_ignores_a_release_owned_by_another_project_that_cannot_be_mapped() {
    for reason in ["no supported hash", "ambiguous primary file"] {
        let catalog = catalog_serving_fits_versions(fits_versions_with_an_unmappable_release(
            "FITSNEW1",
            Some("OTHERPROJ"),
            reason,
        ))
        .await;

        let versions = catalog
            .provider
            .project_versions(
                "FITSPROJ",
                &target(),
                &reporter(),
                &CancellationToken::new(),
            )
            .await
            .unwrap_or_else(|error| {
                panic!("a {reason} foreign release failed the listing: {error}")
            });

        let ids: Vec<&str> = versions.iter().map(|version| version.id.as_str()).collect();
        assert!(!ids.contains(&"FITSNEW1"), "{reason}: {ids:?}");
        assert!(ids.contains(&"FITSOLD1"), "{reason}: {ids:?}");
    }
}

#[tokio::test]
async fn a_version_listing_still_refuses_an_own_release_that_cannot_be_mapped() {
    for reason in ["no supported hash", "ambiguous primary file"] {
        let catalog = catalog_serving_fits_versions(fits_versions_with_an_unmappable_release(
            "FITSNEW1", None, reason,
        ))
        .await;

        let result = catalog
            .provider
            .project_versions(
                "FITSPROJ",
                &target(),
                &reporter(),
                &CancellationToken::new(),
            )
            .await;

        assert!(
            result.is_err(),
            "{reason}: an own release that cannot be mapped is not skipped"
        );
    }
}

#[tokio::test]
async fn a_search_hit_is_offered_when_a_release_owned_by_another_project_cannot_be_mapped() {
    let catalog = catalog_serving_fits_versions(fits_versions_with_an_unmappable_release(
        "FITSNEW1",
        Some("OTHERPROJ"),
        "no supported hash",
    ))
    .await;

    let results = catalog
        .provider
        .search(
            &target(),
            &ModSearchQuery::new("test"),
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect("search");

    let hit = results
        .hits
        .iter()
        .find(|hit| hit.project.id == "FITSPROJ")
        .expect("the project still has an installable release");
    assert_eq!(hit.best_version.as_ref().expect("a version").id, "FITSOLD1");
}

// -- searching for a server ----------------------------------------------

fn side_hit(id: &str, client: &str, server: &str) -> String {
    format!(
        r#"{{"project_id":"{id}","slug":"{id}","title":"{id}","description":"","author":"a",
            "client_side":"{client}","server_side":"{server}","project_type":"mod",
            "categories":["neoforge"],"versions":["1.20.1"]}}"#
    )
}

fn side_versions(id: &str, environment: Option<&str>) -> String {
    let environment =
        environment.map_or_else(String::new, |value| format!(r#""environment":"{value}","#));
    format!(
        r#"[{{"id":"{id}V1","project_id":"{id}","name":"v1","version_number":"1.0.0",
            "version_type":"release","game_versions":["1.20.1"],"loaders":["neoforge"],
            "date_published":"2026-01-01T00:00:00Z",{environment}
            "files":[{{"hashes":{{"sha1":"da39a3ee5e6b4b0d3255bfef95601890afd80709"}},
                       "url":"https://cdn.modrinth.com/data/{id}/versions/{id}V1/{id}.jar",
                       "filename":"{id}.jar","primary":true,"size":1}}],
            "dependencies":[]}}]"#
    )
}

/// A page of four hits, each about one way a side can be declared.
async fn side_catalog() -> Catalog {
    let hits = [
        side_hit("BOTHPROJ", "required", "required"),
        side_hit("PROJECTCLIENT", "required", "unsupported"),
        side_hit("RELEASECLIENT", "required", "required"),
        side_hit("UNSTATED", "required", "unknown"),
    ]
    .join(",");
    let page = format!(r#"{{"hits":[{hits}],"offset":0,"limit":10,"total_hits":40}}"#);
    Catalog::start()
        .await
        .serving("/v2/search", page)
        .await
        .serving(
            "/v2/project/BOTHPROJ/version",
            side_versions("BOTHPROJ", None),
        )
        .await
        .serving(
            "/v2/project/PROJECTCLIENT/version",
            side_versions("PROJECTCLIENT", None),
        )
        .await
        .serving(
            "/v2/project/RELEASECLIENT/version",
            side_versions("RELEASECLIENT", Some("client_only")),
        )
        .await
        .serving(
            "/v2/project/UNSTATED/version",
            side_versions("UNSTATED", None),
        )
        .await
}

async fn search_ids(catalog: &Catalog, query: &ModSearchQuery) -> Vec<String> {
    catalog
        .provider
        .search(&target(), query, &reporter(), &CancellationToken::new())
        .await
        .expect("search")
        .hits
        .into_iter()
        .map(|hit| hit.project.id)
        .collect()
}

#[tokio::test]
async fn a_server_search_asks_upstream_to_leave_out_projects_declaring_no_server_support() {
    let catalog = side_catalog().await;

    search_ids(
        &catalog,
        &ModSearchQuery::new("test").for_side(HostSide::Server),
    )
    .await;

    let facets = catalog.sent_facets().await;
    assert!(
        facets.contains(r#"["server_side!=unsupported"]"#),
        "{facets}"
    );
    assert!(facets.contains(r#"["versions:1.20.1"]"#));
}

#[tokio::test]
async fn a_client_search_sends_no_side_facet() {
    let catalog = side_catalog().await;

    search_ids(&catalog, &ModSearchQuery::new("test")).await;

    assert!(!catalog.sent_facets().await.contains("server_side"));
}

#[tokio::test]
async fn a_server_search_hides_client_only_mods_declared_by_the_project_or_by_the_release() {
    let catalog = side_catalog().await;

    let ids = search_ids(
        &catalog,
        &ModSearchQuery::new("test").for_side(HostSide::Server),
    )
    .await;

    assert_eq!(
        ids,
        ["BOTHPROJ", "UNSTATED"],
        "the project's own client-only declaration and a client-only release are both hidden"
    );
}

#[tokio::test]
async fn a_server_search_keeps_a_mod_with_no_stated_server_side_and_says_so() {
    let catalog = side_catalog().await;

    let results = catalog
        .provider
        .search(
            &target(),
            &ModSearchQuery::new("test").for_side(HostSide::Server),
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect("search");

    let sides = |id: &str| {
        results
            .hits
            .iter()
            .find(|hit| hit.project.id == id)
            .expect("a hit")
            .effective_sides()
            .1
    };
    assert_eq!(sides("UNSTATED"), SideSupport::Unknown);
    assert_eq!(sides("BOTHPROJ"), SideSupport::Required);
}

#[tokio::test]
async fn showing_incompatible_results_for_a_server_keeps_client_only_mods_and_sends_no_side_facet()
{
    let catalog = side_catalog().await;

    let results = catalog
        .provider
        .search(
            &target(),
            &ModSearchQuery::new("test")
                .for_side(HostSide::Server)
                .including_incompatible(true),
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect("search");

    assert_eq!(catalog.sent_facets().await, r#"[["project_type:mod"]]"#);
    let client_only: Vec<&str> = results
        .hits
        .iter()
        .filter(|hit| hit.is_client_only())
        .map(|hit| hit.project.id.as_str())
        .collect();
    assert_eq!(client_only, ["PROJECTCLIENT", "RELEASECLIENT"]);
    assert_eq!(results.hits.len(), 4);
}

#[tokio::test]
async fn a_server_search_continues_from_the_raw_page_not_from_what_survived_the_filter() {
    let catalog = side_catalog().await;

    let results = catalog
        .provider
        .search(
            &target(),
            &ModSearchQuery::new("test")
                .for_side(HostSide::Server)
                .limited(10),
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect("search");

    assert_eq!(results.hits.len(), 2);
    assert_eq!(
        results.next_offset,
        Some(4),
        "the provider returned four hits, so the next page starts after four"
    );
}
