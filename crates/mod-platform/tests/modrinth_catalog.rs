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
    InstallSelection, InstanceTarget, ModProviderApi, ModSearchQuery, ModrinthProvider,
    ResolveRequest, resolve,
};
use ephervex_model::{Hash, HashAlgorithm, LoaderKind, ModProvider};
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
    assert_eq!(plan.optional[0].project_id, "EXTRAPROJ");
    assert!(
        plan.to_install()
            .iter()
            .all(|planned| planned.version.project_id != "EXTRAPROJ"),
        "an unselected optional installs nothing"
    );
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

#[tokio::test]
async fn an_unreachable_provider_is_reported_as_such_rather_than_as_an_empty_catalog() {
    let catalog = Catalog::start().await;
    let error = catalog
        .provider
        .versions("NOTHING", &target(), &reporter(), &CancellationToken::new())
        .await
        .expect_err("nothing is mocked");
    // Reported, not swallowed: an empty list would look like a mod with no
    // releases rather than a service that could not be reached.
    assert!(!error.is_cancellation());
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
