//! The CurseForge mod catalog against loopback servers.
//!
//! Every response here is a fixture served by `wiremock` on 127.0.0.1 with an
//! obviously fake key. Nothing contacts CurseForge, nothing reads a credential
//! from the environment or the system keyring, and nothing needs a real API key
//! to run.
//!
//! What these cover that a unit test cannot: the request that is actually sent
//! (which game, which project class, which Minecraft release, which loader)
//! and what the provider does with an answer it cannot vouch for.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use ephervex_download::{DomainAllowlist, HttpConfig, OperationReporter};
use ephervex_mod_platform::{
    Compatibility, ConfiguredApiKey, CurseForgeApi, CurseForgeProvider, IncompatibilityReason,
    InstanceTarget, ModPlatformError, ModProviderApi, ModSearchQuery, ReleaseChannel, SideSupport,
};
use ephervex_model::{LoaderKind, ModProvider, ModRelationKind, SecretString};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{body_json, method, path, path_regex, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

/// Obviously not a credential, and the only one this file ever uses.
const FAKE_KEY: &str = "test-curseforge-key-do-not-use";

/// The Mods class id these fixtures report.
///
/// Deliberately not CurseForge's current production value: a build that
/// hard-coded a number instead of reading the categories response would send
/// the wrong one and fail these tests.
const MODS_CLASS: u32 = 9911;

/// A class that is not Mods, so a project carrying it must not be offered.
const RESOURCE_PACK_CLASS: u32 = 9912;

/// Another class that is not Mods, on the same terms as [`RESOURCE_PACK_CLASS`].
const SHADER_PACK_CLASS: u32 = 9913;

const JEI_PROJECT: u32 = 238_222;
const JEI_FILE: u32 = 4_593_548;

fn categories() -> String {
    format!(
        r#"{{"data":[
            {{"id":{MODS_CLASS},"gameId":432,"name":"Mods","slug":"mc-mods","isClass":true}},
            {{"id":{RESOURCE_PACK_CLASS},"gameId":432,"name":"Resource Packs","slug":"texture-packs","isClass":true}},
            {{"id":{SHADER_PACK_CLASS},"gameId":432,"name":"Shaders","slug":"shaders","isClass":true}}
        ]}}"#
    )
}

/// One project document, with whatever class the caller wants to test.
fn project_document(id: u32, name: &str, slug: &str, class: &str) -> String {
    format!(
        r#"{{"id":{id},"gameId":432,"name":"{name}","slug":"{slug}","summary":"A test mod",
            "classId":{class},"authors":[{{"id":1,"name":"mezz"}}],
            "logo":{{"id":1,"url":"http://127.0.0.1/logo.png"}}}}"#
    )
}

/// One file document, published from the loopback server itself.
fn file_document(
    server: &MockServer,
    id: u32,
    project: u32,
    display: &str,
    release: u32,
) -> String {
    format!(
        r#"{{"id":{id},"gameId":432,"modId":{project},"isAvailable":true,
            "displayName":"{display}","fileName":"jei-{id}.jar","releaseType":{release},
            "fileDate":"2023-07-01T10:00:00Z","fileLength":1234567,
            "downloadUrl":"{uri}/files/{id}/jei-{id}.jar",
            "hashes":[{{"value":"da39a3ee5e6b4b0d3255bfef95601890afd80709","algo":1}}],
            "gameVersions":["1.20.1","Forge","NeoForge","Client","Server"],
            "dependencies":[]}}"#,
        uri = server.uri()
    )
}

fn provider_against(server: &MockServer, key: Option<&str>) -> CurseForgeProvider {
    let credential = match key {
        Some(key) => ConfiguredApiKey::new(SecretString::new(key)),
        None => ConfiguredApiKey::absent(),
    };
    let api = CurseForgeApi::with_base_url(
        Arc::new(credential),
        &HttpConfig::for_loopback_tests(),
        &format!("{}/v1", server.uri()),
    )
    .expect("a client builds");
    CurseForgeProvider::with_artifact_hosts(api, DomainAllowlist::for_loopback_tests("127.0.0.1"))
}

async fn serve_categories(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/v1/categories"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(categories(), "application/json"))
        .mount(server)
        .await;
}

/// A server that answers a search with one mod, one resource pack, and one
/// project that reports no class at all.
async fn catalog(server: &MockServer) {
    serve_categories(server).await;
    let body = format!(
        r#"{{"data":[{mod_project},{pack},{unclassified}],
            "pagination":{{"index":0,"pageSize":10,"resultCount":3,"totalCount":42}}}}"#,
        mod_project = project_document(
            JEI_PROJECT,
            "Just Enough Items",
            "jei",
            &MODS_CLASS.to_string()
        ),
        pack = project_document(
            1000,
            "Faithful",
            "faithful",
            &RESOURCE_PACK_CLASS.to_string()
        ),
        unclassified = project_document(1001, "Mystery", "mystery", "null"),
    );
    Mock::given(method("GET"))
        .and(path("/v1/mods/search"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/json"))
        .mount(server)
        .await;

    let files = format!(
        r#"{{"data":[{newer},{older}],"pagination":{{"index":0,"pageSize":50,"resultCount":2,"totalCount":2}}}}"#,
        newer = file_document(server, JEI_FILE, JEI_PROJECT, "JEI 15.2.0.27", 1),
        older = file_document(server, JEI_FILE - 1, JEI_PROJECT, "JEI 15.2.0.26", 2),
    );
    Mock::given(method("GET"))
        .and(path_regex(r"^/v1/mods/\d+/files$"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(files, "application/json"))
        .mount(server)
        .await;

    Mock::given(method("GET"))
        .and(path_regex(r"^/v1/mods/\d+$"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            format!(
                r#"{{"data":{}}}"#,
                project_document(
                    JEI_PROJECT,
                    "Just Enough Items",
                    "jei",
                    &MODS_CLASS.to_string()
                )
            ),
            "application/json",
        ))
        .mount(server)
        .await;
}

/// Two distinct pages of three Mods each, matched by the `index` the request
/// carries, so a caller that follows the continuation lands on genuinely new
/// entries rather than the server's one canned response.
async fn two_pages(server: &MockServer) {
    serve_categories(server).await;

    let first = format!(
        r#"{{"data":[{a},{b},{c}],
            "pagination":{{"index":0,"pageSize":3,"resultCount":3,"totalCount":6}}}}"#,
        a = project_document(2001, "Page One A", "page-one-a", &MODS_CLASS.to_string()),
        b = project_document(2002, "Page One B", "page-one-b", &MODS_CLASS.to_string()),
        c = project_document(2003, "Page One C", "page-one-c", &MODS_CLASS.to_string()),
    );
    Mock::given(method("GET"))
        .and(path("/v1/mods/search"))
        .and(query_param("index", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(first, "application/json"))
        .mount(server)
        .await;

    let second = format!(
        r#"{{"data":[{d},{e},{f}],
            "pagination":{{"index":3,"pageSize":3,"resultCount":3,"totalCount":6}}}}"#,
        d = project_document(2004, "Page Two D", "page-two-d", &MODS_CLASS.to_string()),
        e = project_document(2005, "Page Two E", "page-two-e", &MODS_CLASS.to_string()),
        f = project_document(2006, "Page Two F", "page-two-f", &MODS_CLASS.to_string()),
    );
    Mock::given(method("GET"))
        .and(path("/v1/mods/search"))
        .and(query_param("index", "3"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(second, "application/json"))
        .mount(server)
        .await;

    // Each project above lists one compatible file of its own, so every one of
    // them can annotate.
    for project in 2001..=2006_u32 {
        let files = format!(
            r#"{{"data":[{file}],"pagination":{{"index":0,"pageSize":50,"resultCount":1,"totalCount":1}}}}"#,
            file = file_document(server, 9_000_000 + project, project, "Fixture", 1),
        );
        Mock::given(method("GET"))
            .and(path(format!("/v1/mods/{project}/files")))
            .respond_with(ResponseTemplate::new(200).set_body_raw(files, "application/json"))
            .mount(server)
            .await;
    }
}

/// A raw page whose every project fails class confirmation, so nothing
/// survives to display even though the provider consumed entries and still
/// has more.
async fn all_filtered_page(server: &MockServer) {
    serve_categories(server).await;
    let body = format!(
        r#"{{"data":[{a},{b}],
            "pagination":{{"index":0,"pageSize":10,"resultCount":2,"totalCount":50}}}}"#,
        a = project_document(
            3001,
            "Not A Mod",
            "not-a-mod",
            &RESOURCE_PACK_CLASS.to_string()
        ),
        b = project_document(3002, "Also Not A Mod", "also-not-a-mod", "null"),
    );
    Mock::given(method("GET"))
        .and(path("/v1/mods/search"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/json"))
        .mount(server)
        .await;
}

fn target(loader: LoaderKind) -> InstanceTarget {
    InstanceTarget::new("1.20.1", loader)
}

fn reporter() -> OperationReporter {
    OperationReporter::silent("browsing-mods")
}

/// The query string of the first search request the server received.
async fn sent_search(server: &MockServer) -> String {
    server
        .received_requests()
        .await
        .expect("recorded requests")
        .into_iter()
        .find(|request: &Request| request.url.path() == "/v1/mods/search")
        .expect("a search was sent")
        .url
        .query()
        .unwrap_or_default()
        .to_owned()
}

/// The query string of the first file-listing request the server received.
async fn sent_file_listing(server: &MockServer) -> String {
    server
        .received_requests()
        .await
        .expect("recorded requests")
        .into_iter()
        .find(|request: &Request| request.url.path().ends_with("/files"))
        .expect("a file listing was sent")
        .url
        .query()
        .unwrap_or_default()
        .to_owned()
}

async fn search(server: &MockServer, loader: LoaderKind, query: ModSearchQuery) {
    let _ = provider_against(server, Some(FAKE_KEY))
        .search(
            &target(loader),
            &query,
            &reporter(),
            &CancellationToken::new(),
        )
        .await;
}

#[tokio::test]
async fn a_search_asks_about_minecraft_by_its_published_game_id() {
    let server = MockServer::start().await;
    catalog(&server).await;
    search(&server, LoaderKind::NeoForge, ModSearchQuery::new("jei")).await;
    assert!(sent_search(&server).await.contains("gameId=432"));
}

#[tokio::test]
async fn a_search_carries_the_mods_class_the_provider_itself_reported() {
    // The fixture's Mods class is not CurseForge's production number, so a
    // build that hard-coded one would send the wrong class here.
    let server = MockServer::start().await;
    catalog(&server).await;
    search(&server, LoaderKind::NeoForge, ModSearchQuery::new("jei")).await;
    let sent = sent_search(&server).await;
    assert!(sent.contains(&format!("classId={MODS_CLASS}")), "{sent}");
}

#[tokio::test]
async fn a_search_narrows_on_the_instances_exact_minecraft_release() {
    let server = MockServer::start().await;
    catalog(&server).await;
    search(&server, LoaderKind::NeoForge, ModSearchQuery::new("jei")).await;
    let sent = sent_search(&server).await;
    assert!(sent.contains("gameVersion=1.20.1"), "{sent}");
}

/// A search that finds one project fitting the instance, one that publishes
/// only for another Minecraft release, and one that publishes only for another
/// loader. Its file listing answers only for the first, as the real filtered
/// listing would.
async fn widened_catalog(server: &MockServer) {
    serve_categories(server).await;
    let project = |id: u32, name: &str, declared: &str| {
        format!(
            r#"{{"id":{id},"gameId":432,"name":"{name}","slug":"{name}","summary":"A test mod",
                "classId":{MODS_CLASS},"authors":[{{"id":1,"name":"mezz"}}],
                "latestFiles":[{{"gameVersions":[{declared}]}}]}}"#
        )
    };
    let body = format!(
        r#"{{"data":[{fits},{old_release},{other_loader}],
            "pagination":{{"index":0,"pageSize":10,"resultCount":3,"totalCount":3}}}}"#,
        fits = project(5001, "fits", r#""1.20.1","NeoForge""#),
        old_release = project(5002, "old-release", r#""1.19.2","NeoForge""#),
        other_loader = project(5003, "other-loader", r#""1.20.1","Fabric""#),
    );
    Mock::given(method("GET"))
        .and(path("/v1/mods/search"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/json"))
        .mount(server)
        .await;
    let fitting = format!(
        r#"{{"data":[{file}],"pagination":{{"index":0,"pageSize":50,"resultCount":1,"totalCount":1}}}}"#,
        file = file_document(server, 7_000_001, 5001, "Fits", 1),
    );
    Mock::given(method("GET"))
        .and(path("/v1/mods/5001/files"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(fitting, "application/json"))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/v1/mods/500[23]/files$"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"{"data":[],"pagination":{"index":0,"pageSize":50,"resultCount":0,"totalCount":0}}"#,
            "application/json",
        ))
        .mount(server)
        .await;
}

#[tokio::test]
async fn asking_for_incompatible_results_leaves_the_release_and_loader_filters_off() {
    let server = MockServer::start().await;
    catalog(&server).await;
    search(
        &server,
        LoaderKind::NeoForge,
        ModSearchQuery::new("jei").including_incompatible(true),
    )
    .await;
    let sent = sent_search(&server).await;
    assert!(!sent.contains("gameVersion"), "{sent}");
    assert!(!sent.contains("modLoaderType"), "{sent}");
    assert!(sent.contains("gameId=432"), "{sent}");
    assert!(sent.contains(&format!("classId={MODS_CLASS}")), "{sent}");
    assert!(sent.contains("searchFilter=jei"), "{sent}");
}

#[tokio::test]
async fn a_compatible_only_search_keeps_the_release_and_loader_filters() {
    let server = MockServer::start().await;
    catalog(&server).await;
    search(
        &server,
        LoaderKind::NeoForge,
        ModSearchQuery::new("jei").including_incompatible(false),
    )
    .await;
    let sent = sent_search(&server).await;
    assert!(sent.contains("gameVersion=1.20.1"), "{sent}");
    assert!(sent.contains("modLoaderType=6"), "{sent}");
}

#[tokio::test]
async fn incompatible_results_still_ask_each_file_listing_about_the_instance() {
    let server = MockServer::start().await;
    widened_catalog(&server).await;
    provider_against(&server, Some(FAKE_KEY))
        .search(
            &target(LoaderKind::NeoForge),
            &ModSearchQuery::new("").including_incompatible(true),
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect("a search");
    let listings: Vec<String> = server
        .received_requests()
        .await
        .expect("recorded requests")
        .into_iter()
        .filter(|request: &Request| request.url.path().ends_with("/files"))
        .map(|request| request.url.query().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(listings.len(), 3, "one listing per hit, no more");
    for listing in listings {
        assert!(listing.contains("gameVersion=1.20.1"), "{listing}");
        assert!(listing.contains("modLoaderType=6"), "{listing}");
    }
}

#[tokio::test]
async fn projects_the_provider_would_have_filtered_out_come_back_marked_incompatible() {
    let server = MockServer::start().await;
    widened_catalog(&server).await;
    let results = provider_against(&server, Some(FAKE_KEY))
        .search(
            &target(LoaderKind::NeoForge),
            &ModSearchQuery::new("").including_incompatible(true),
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect("a search");
    assert_eq!(results.hits.len(), 3);

    let fits = &results.hits[0];
    assert!(fits.best_version.is_some());
    assert_eq!(fits.compatibility, Compatibility::Compatible);

    let old_release = &results.hits[1];
    assert!(old_release.best_version.is_none());
    assert_eq!(
        old_release.compatibility,
        Compatibility::Incompatible(vec![IncompatibilityReason::MinecraftVersion {
            required: "1.20.1".to_owned(),
            supported: vec!["1.19.2".to_owned()],
        }])
    );

    let other_loader = &results.hits[2];
    assert!(other_loader.best_version.is_none());
    assert_eq!(
        other_loader.compatibility,
        Compatibility::Incompatible(vec![IncompatibilityReason::Loader {
            required: LoaderKind::NeoForge,
            supported: vec!["Fabric".to_owned()],
        }])
    );
}

#[tokio::test]
async fn a_compatible_only_search_leaves_incompatible_projects_out() {
    let server = MockServer::start().await;
    widened_catalog(&server).await;
    let results = provider_against(&server, Some(FAKE_KEY))
        .search(
            &target(LoaderKind::NeoForge),
            &ModSearchQuery::new(""),
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect("a search");
    assert_eq!(results.hits.len(), 1);
    assert_eq!(results.hits[0].project.title, "fits");
}

#[tokio::test]
async fn an_unsupported_loader_is_refused_before_a_widened_search_spends_a_request() {
    let server = MockServer::start().await;
    catalog(&server).await;
    let error = provider_against(&server, Some(FAKE_KEY))
        .search(
            &target(LoaderKind::Vanilla),
            &ModSearchQuery::new("jei").including_incompatible(true),
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect_err("vanilla cannot host mods");
    assert!(matches!(
        error,
        ModPlatformError::LoaderCannotHostMods {
            loader: LoaderKind::Vanilla
        }
    ));
    assert!(
        server
            .received_requests()
            .await
            .expect("recorded requests")
            .is_empty()
    );
}

#[tokio::test]
async fn forge_is_filtered_by_curseforges_own_forge_loader_value() {
    let server = MockServer::start().await;
    catalog(&server).await;
    search(&server, LoaderKind::Forge, ModSearchQuery::new("jei")).await;
    let sent = sent_search(&server).await;
    assert!(sent.contains("modLoaderType=1"), "{sent}");
}

#[tokio::test]
async fn neoforge_is_filtered_by_curseforges_own_neoforge_loader_value() {
    let server = MockServer::start().await;
    catalog(&server).await;
    search(&server, LoaderKind::NeoForge, ModSearchQuery::new("jei")).await;
    let sent = sent_search(&server).await;
    assert!(sent.contains("modLoaderType=6"), "{sent}");
    // Never Forge's value, and never some other loader's.
    assert!(!sent.contains("modLoaderType=1&"), "{sent}");
}

#[tokio::test]
async fn a_loader_this_launcher_does_not_support_never_becomes_another_one() {
    // Vanilla has no CurseForge loader, and the refusal happens before a
    // request rather than being widened into "any loader".
    let server = MockServer::start().await;
    catalog(&server).await;
    let error = provider_against(&server, Some(FAKE_KEY))
        .search(
            &target(LoaderKind::Vanilla),
            &ModSearchQuery::new("jei"),
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect_err("vanilla cannot host mods");
    assert!(matches!(
        error,
        ModPlatformError::LoaderCannotHostMods {
            loader: LoaderKind::Vanilla
        }
    ));
    assert!(
        server
            .received_requests()
            .await
            .expect("recorded requests")
            .is_empty(),
        "an unsupported loader must not spend a request"
    );
}

#[tokio::test]
async fn the_typed_search_text_reaches_the_provider_intact() {
    let server = MockServer::start().await;
    catalog(&server).await;
    search(
        &server,
        LoaderKind::NeoForge,
        ModSearchQuery::new("just enough items"),
    )
    .await;
    let sent = sent_search(&server).await;
    // Percent-encoded by the URL builder, not concatenated raw.
    assert!(sent.contains("searchFilter=just+enough+items"), "{sent}");
}

#[tokio::test]
async fn an_empty_search_sends_no_filter_rather_than_an_empty_one() {
    let server = MockServer::start().await;
    catalog(&server).await;
    search(&server, LoaderKind::NeoForge, ModSearchQuery::new("  ")).await;
    let sent = sent_search(&server).await;
    assert!(!sent.contains("searchFilter"), "{sent}");
}

#[tokio::test]
async fn how_many_results_exist_is_read_from_the_provider_rather_than_guessed() {
    let server = MockServer::start().await;
    catalog(&server).await;
    let results = provider_against(&server, Some(FAKE_KEY))
        .search(
            &target(LoaderKind::NeoForge),
            &ModSearchQuery::new("jei").from_offset(0),
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect("a search");
    // The page carried three projects; the provider said there are forty-two.
    assert_eq!(results.total_hits, 42);
    assert_eq!(results.offset, 0);
    let sent = sent_search(&server).await;
    assert!(sent.contains("index=0"), "{sent}");
}

#[tokio::test]
async fn a_requested_page_is_asked_for_at_the_offset_it_starts_at() {
    let server = MockServer::start().await;
    catalog(&server).await;
    let results = provider_against(&server, Some(FAKE_KEY))
        .search(
            &target(LoaderKind::NeoForge),
            &ModSearchQuery::new("jei").from_offset(10),
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect("a search");
    assert_eq!(results.offset, 10);
    let sent = sent_search(&server).await;
    assert!(sent.contains("index=10"), "{sent}");
}

#[tokio::test]
async fn a_page_where_nothing_is_confirmed_as_a_mod_can_still_continue() {
    // Every raw entry is refused by class, so the visible page is empty. The
    // provider still consumed two entries and reports more beyond them, and
    // an empty visible page must not be read as "no more to load".
    let server = MockServer::start().await;
    all_filtered_page(&server).await;
    let results = provider_against(&server, Some(FAKE_KEY))
        .search(
            &target(LoaderKind::NeoForge),
            &ModSearchQuery::new("jei"),
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect("a search");
    assert!(results.hits.is_empty());
    assert_eq!(results.next_offset, Some(2));
}

#[tokio::test]
async fn a_later_page_asked_for_at_the_continuation_does_not_repeat_the_first() {
    let server = MockServer::start().await;
    two_pages(&server).await;

    let first = provider_against(&server, Some(FAKE_KEY))
        .search(
            &target(LoaderKind::NeoForge),
            &ModSearchQuery::new(""),
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect("a first page");
    assert_eq!(first.next_offset, Some(3));
    let first_ids: Vec<String> = first
        .hits
        .iter()
        .map(|hit| hit.project.id.clone())
        .collect();
    assert_eq!(first_ids.len(), 3);

    let second = provider_against(&server, Some(FAKE_KEY))
        .search(
            &target(LoaderKind::NeoForge),
            &ModSearchQuery::new("").from_offset(first.next_offset.expect("a continuation")),
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect("a second page");
    // Six of six have now been seen.
    assert_eq!(second.next_offset, None);
    let second_ids: Vec<String> = second
        .hits
        .iter()
        .map(|hit| hit.project.id.clone())
        .collect();
    assert_eq!(second_ids.len(), 3);

    assert!(
        first_ids.iter().all(|id| !second_ids.contains(id)),
        "first page {first_ids:?} and second page {second_ids:?} must not overlap"
    );
}

#[tokio::test]
async fn a_project_that_is_not_a_mod_is_left_out_of_the_results() {
    // The class filter narrowed the request; it is not taken as proof. The
    // resource pack and the project reporting no class at all are both
    // refused, and only the confirmed mod is offered.
    let server = MockServer::start().await;
    catalog(&server).await;
    let results = provider_against(&server, Some(FAKE_KEY))
        .search(
            &target(LoaderKind::NeoForge),
            &ModSearchQuery::new("jei"),
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect("a search");
    let ids: Vec<&str> = results
        .hits
        .iter()
        .map(|hit| hit.project.id.as_str())
        .collect();
    assert_eq!(ids, vec![JEI_PROJECT.to_string().as_str()]);
    assert_eq!(results.provider, ModProvider::CurseForge);
    // The fixture's raw page carries three projects (`resultCount: 3`); only
    // one survives class filtering. The continuation has to follow what the
    // provider consumed, not the one hit this launcher went on to show, or
    // the next request would re-read the two that were left out.
    assert_eq!(results.next_offset, Some(3));
}

#[tokio::test]
async fn a_project_lookup_keeps_the_exact_provider_and_project_identity() {
    let server = MockServer::start().await;
    catalog(&server).await;
    let project = provider_against(&server, Some(FAKE_KEY))
        .project(
            &JEI_PROJECT.to_string(),
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect("a project");
    assert_eq!(project.provider, ModProvider::CurseForge);
    assert_eq!(project.id, JEI_PROJECT.to_string());
    assert_eq!(project.slug.as_deref(), Some("jei"));
    assert_eq!(project.title, "Just Enough Items");
    assert_eq!(project.author.as_deref(), Some("mezz"));
}

#[tokio::test]
async fn a_project_of_another_class_is_refused_by_name_rather_than_installed() {
    let server = MockServer::start().await;
    serve_categories(&server).await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/v1/mods/\d+$"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            format!(
                r#"{{"data":{}}}"#,
                project_document(
                    1000,
                    "Faithful",
                    "faithful",
                    &RESOURCE_PACK_CLASS.to_string()
                )
            ),
            "application/json",
        ))
        .mount(&server)
        .await;

    let error = provider_against(&server, Some(FAKE_KEY))
        .project("1000", &reporter(), &CancellationToken::new())
        .await
        .expect_err("a resource pack is not a mod");
    assert!(
        matches!(
            error,
            ModPlatformError::ProjectTypeUnsupported { ref project_type, .. }
                if project_type == "Resource Packs"
        ),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_shader_pack_project_is_refused_by_name_rather_than_installed() {
    // The pack importer now accepts Shader Packs; the standalone browser does
    // not, and must keep refusing one by the provider's own class name.
    let server = MockServer::start().await;
    serve_categories(&server).await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/v1/mods/\d+$"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            format!(
                r#"{{"data":{}}}"#,
                project_document(
                    1001,
                    "Complementary",
                    "complementary",
                    &SHADER_PACK_CLASS.to_string()
                )
            ),
            "application/json",
        ))
        .mount(&server)
        .await;

    let error = provider_against(&server, Some(FAKE_KEY))
        .project("1001", &reporter(), &CancellationToken::new())
        .await
        .expect_err("a shader pack is not a mod");
    assert!(
        matches!(
            error,
            ModPlatformError::ProjectTypeUnsupported { ref project_type, .. }
                if project_type == "Shaders"
        ),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_file_listing_narrows_on_the_exact_release_and_the_loader() {
    let server = MockServer::start().await;
    catalog(&server).await;
    let _ = provider_against(&server, Some(FAKE_KEY))
        .versions(
            &JEI_PROJECT.to_string(),
            &target(LoaderKind::Forge),
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect("a file listing");
    let sent = sent_file_listing(&server).await;
    assert!(sent.contains("gameVersion=1.20.1"), "{sent}");
    assert!(sent.contains("modLoaderType=1"), "{sent}");
}

#[tokio::test]
async fn every_listed_file_keeps_its_exact_project_and_file_identity() {
    let server = MockServer::start().await;
    catalog(&server).await;
    let versions = provider_against(&server, Some(FAKE_KEY))
        .versions(
            &JEI_PROJECT.to_string(),
            &target(LoaderKind::NeoForge),
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect("a file listing");

    assert_eq!(versions.len(), 2);
    let newest = versions
        .iter()
        .find(|version| version.id == JEI_FILE.to_string())
        .expect("the exact file");
    assert_eq!(newest.provider, ModProvider::CurseForge);
    assert_eq!(newest.project_id, JEI_PROJECT.to_string());
    assert_eq!(newest.version_number, "JEI 15.2.0.27");
    assert_eq!(newest.file.file_name, format!("jei-{JEI_FILE}.jar"));
    assert_eq!(
        newest.file.hashes[0].digest(),
        "da39a3ee5e6b4b0d3255bfef95601890afd80709"
    );
    // The mixed `gameVersions` array is split into the two things this
    // launcher reasons about.
    assert!(newest.game_versions.contains(&"1.20.1".to_owned()));
    assert!(newest.loaders.contains(&"NeoForge".to_owned()));
    assert!(!newest.game_versions.contains(&"NeoForge".to_owned()));
    // Release types become the neutral channel rather than travelling as
    // provider numbers.
    assert_eq!(
        newest.channel,
        ephervex_mod_platform::ReleaseChannel::Release
    );
    let older = versions
        .iter()
        .find(|version| version.id == (JEI_FILE - 1).to_string())
        .expect("the older file");
    assert_eq!(older.channel, ephervex_mod_platform::ReleaseChannel::Beta);
}

#[tokio::test]
async fn an_exactly_named_file_is_fetched_by_its_own_id() {
    let server = MockServer::start().await;
    serve_categories(&server).await;
    Mock::given(method("POST"))
        .and(path("/v1/mods/files"))
        .and(body_json(serde_json::json!({ "fileIds": [JEI_FILE] })))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            format!(
                r#"{{"data":[{}]}}"#,
                file_document(&server, JEI_FILE, JEI_PROJECT, "JEI 15.2.0.27", 1)
            ),
            "application/json",
        ))
        .expect(1)
        .mount(&server)
        .await;

    let versions = provider_against(&server, Some(FAKE_KEY))
        .versions_by_id(
            &[JEI_FILE.to_string()],
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect("the exact file");
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].id, JEI_FILE.to_string());
    assert_eq!(versions[0].project_id, JEI_PROJECT.to_string());
}

#[tokio::test]
async fn each_listed_file_carries_its_own_side_declaration() {
    // The older file names only Client while a newer one names both, so the
    // project reads as running on both sides and the older release still must
    // not: the selected release's own tags decide.
    let server = MockServer::start().await;
    serve_categories(&server).await;
    let files = |id: u32, tags: &str| {
        format!(
            r#"{{"id":{id},"gameId":432,"modId":{JEI_PROJECT},"isAvailable":true,
                "displayName":"File {id}","fileName":"jei-{id}.jar","releaseType":1,
                "fileDate":"2023-07-01T10:00:00Z","fileLength":10,
                "downloadUrl":"{uri}/files/{id}/jei-{id}.jar",
                "hashes":[{{"value":"da39a3ee5e6b4b0d3255bfef95601890afd80709","algo":1}}],
                "gameVersions":["1.20.1","NeoForge"{tags}],"dependencies":[]}}"#,
            uri = server.uri()
        )
    };
    let body = format!(
        r#"{{"data":[{},{},{}],"pagination":{{"totalCount":3}}}}"#,
        files(1, r#","Client""#),
        files(2, r#","Client","Server""#),
        files(3, ""),
    );
    Mock::given(method("GET"))
        .and(path_regex(r"^/v1/mods/\d+/files$"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/json"))
        .mount(&server)
        .await;

    let versions = provider_against(&server, Some(FAKE_KEY))
        .versions(
            &JEI_PROJECT.to_string(),
            &target(LoaderKind::NeoForge),
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect("a file listing");

    let sides = |id: &str| {
        let release = versions
            .iter()
            .find(|version| version.id == id)
            .expect("file");
        (release.client_side, release.server_side)
    };
    assert_eq!(
        sides("1"),
        (SideSupport::Required, SideSupport::Unsupported)
    );
    assert_eq!(sides("2"), (SideSupport::Required, SideSupport::Required));
    assert_eq!(
        sides("3"),
        (SideSupport::Unknown, SideSupport::Unknown),
        "a file naming neither side declares nothing"
    );
}

#[tokio::test]
async fn a_listed_release_declaring_a_relation_to_no_project_is_refused_rather_than_listed_without_it()
 {
    const PROJECT: u32 = 391_382;

    for relation_type in [3, 5] {
        let server = MockServer::start().await;
        serve_categories(&server).await;
        let body = format!(
            r#"{{"data":[{{"id":7951520,"gameId":432,"modId":{PROJECT},
                "isAvailable":true,"displayName":"Guarded 1.0.0","fileName":"guarded-1.0.0.jar",
                "releaseType":1,"fileDate":"2023-07-01T10:00:00Z","fileLength":10,"downloadUrl":null,
                "hashes":[{{"value":"da39a3ee5e6b4b0d3255bfef95601890afd80709","algo":1}}],
                "gameVersions":["1.20.1","NeoForge"],
                "dependencies":[{{"modId":0,"relationType":{relation_type}}}]}}],
                "pagination":{{"totalCount":1}}}}"#
        );
        Mock::given(method("GET"))
            .and(path_regex(r"^/v1/mods/\d+/files$"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/json"))
            .mount(&server)
            .await;

        let error = provider_against(&server, Some(FAKE_KEY))
            .versions(
                &PROJECT.to_string(),
                &target(LoaderKind::NeoForge),
                &reporter(),
                &CancellationToken::new(),
            )
            .await
            .expect_err("a lost requirement or incompatibility must not read as none declared");

        assert!(
            matches!(error, ModPlatformError::ResponseInvalid { .. }),
            "relation type {relation_type}: {error}"
        );
    }
}

#[tokio::test]
async fn a_project_whose_author_disabled_third_party_downloads_still_lists_its_releases() {
    // A download restriction is about acquisition, not about what CurseForge
    // told us. The release stays in the list, complete, so a version picker
    // shows it and an already-cached copy can still be installed. Shaped like
    // project 391382's restricted file 7951520, the regression this covers.
    const RESTRICTED_PROJECT: u32 = 391_382;
    const RESTRICTED_FILE: u32 = 7_951_520;

    let server = MockServer::start().await;
    serve_categories(&server).await;
    let restricted = format!(
        r#"{{"data":[{{"id":{RESTRICTED_FILE},"gameId":432,"modId":{RESTRICTED_PROJECT},
            "isAvailable":true,"displayName":"Restricted 1.0.0","fileName":"restricted-1.0.0.jar",
            "releaseType":1,"fileDate":"2023-07-01T10:00:00Z","fileLength":10,"downloadUrl":null,
            "hashes":[{{"value":"da39a3ee5e6b4b0d3255bfef95601890afd80709","algo":1}}],
            "gameVersions":["1.20.1","NeoForge"],
            "dependencies":[{{"modId":{JEI_PROJECT},"relationType":3}}]}}],
            "pagination":{{"totalCount":1}}}}"#
    );
    Mock::given(method("GET"))
        .and(path_regex(r"^/v1/mods/\d+/files$"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(restricted, "application/json"))
        .mount(&server)
        .await;

    let versions = provider_against(&server, Some(FAKE_KEY))
        .versions(
            &RESTRICTED_PROJECT.to_string(),
            &target(LoaderKind::NeoForge),
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect("a restricted release is still a release");

    assert_eq!(versions.len(), 1);
    let release = &versions[0];
    assert_eq!(release.id, RESTRICTED_FILE.to_string());
    assert_eq!(release.name, "Restricted 1.0.0");
    assert_eq!(release.channel, ReleaseChannel::Release);
    assert_eq!(release.game_versions, vec!["1.20.1".to_owned()]);
    assert_eq!(release.loaders, vec!["NeoForge".to_owned()]);
    assert_eq!(
        release.published_at.to_rfc3339(),
        "2023-07-01T10:00:00+00:00"
    );
    assert!(
        release.file.distribution.is_restricted(),
        "no address may be invented for it"
    );
    assert_eq!(release.file.distribution.download_url(), None);
    assert_eq!(
        release.file.hashes.len(),
        1,
        "the digest is what recognizes a hand-placed copy"
    );
    assert_eq!(
        release
            .dependencies
            .iter()
            .map(|relation| (relation.kind, relation.project_id.as_deref()))
            .collect::<Vec<_>>(),
        vec![(
            ModRelationKind::Required,
            Some(JEI_PROJECT.to_string().as_str())
        )],
    );
    assert!(
        release
            .compatibility(&target(LoaderKind::NeoForge), "neoforge")
            .is_compatible(),
        "the restriction is not an incompatibility"
    );
}

#[tokio::test]
async fn a_download_address_off_the_permitted_hosts_is_refused() {
    let server = MockServer::start().await;
    serve_categories(&server).await;
    let elsewhere = format!(
        r#"{{"data":[{{"id":{JEI_FILE},"gameId":432,"modId":{JEI_PROJECT},"isAvailable":true,
            "displayName":"JEI","fileName":"jei.jar","releaseType":1,
            "fileDate":"2023-07-01T10:00:00Z","fileLength":10,
            "downloadUrl":"https://mirror.example/jei.jar",
            "hashes":[{{"value":"da39a3ee5e6b4b0d3255bfef95601890afd80709","algo":1}}],
            "gameVersions":["1.20.1","NeoForge"],"dependencies":[]}}],
            "pagination":{{"totalCount":1}}}}"#
    );
    Mock::given(method("GET"))
        .and(path_regex(r"^/v1/mods/\d+/files$"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(elsewhere, "application/json"))
        .mount(&server)
        .await;

    let error = provider_against(&server, Some(FAKE_KEY))
        .versions(
            &JEI_PROJECT.to_string(),
            &target(LoaderKind::NeoForge),
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect_err("an unlisted host is not fetched from");
    assert!(matches!(
        error,
        ModPlatformError::DownloadAddressRejected { .. }
    ));
}

#[tokio::test]
async fn browsing_without_a_configured_key_fails_before_any_request_is_made() {
    let server = MockServer::start().await;
    catalog(&server).await;
    let error = provider_against(&server, None)
        .search(
            &target(LoaderKind::NeoForge),
            &ModSearchQuery::new("jei"),
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect_err("no key is configured");
    assert!(
        matches!(
            error,
            ModPlatformError::CredentialMissing {
                provider: ModProvider::CurseForge
            }
        ),
        "{error:?}"
    );
    assert!(
        server
            .received_requests()
            .await
            .expect("recorded requests")
            .is_empty(),
        "an unauthenticated request still spends somebody's rate limit"
    );
}

#[tokio::test]
async fn a_key_the_provider_refuses_stays_a_credential_problem() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/categories"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;

    let error = provider_against(&server, Some(FAKE_KEY))
        .search(
            &target(LoaderKind::NeoForge),
            &ModSearchQuery::new("jei"),
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect_err("the key was refused");
    assert!(error.is_credential_problem(), "{error:?}");
    assert!(matches!(
        error,
        ModPlatformError::CredentialRejected {
            provider: ModProvider::CurseForge,
            status: 403
        }
    ));
}

#[tokio::test]
async fn a_batch_description_keeps_the_mods_and_names_the_projects_that_are_not() {
    let server = MockServer::start().await;
    serve_categories(&server).await;
    Mock::given(method("POST"))
        .and(path("/v1/mods"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            format!(
                r#"{{"data":[{},{},{}]}}"#,
                project_document(
                    JEI_PROJECT,
                    "Just Enough Items",
                    "jei",
                    &MODS_CLASS.to_string()
                ),
                project_document(
                    1000,
                    "Faithful",
                    "faithful",
                    &RESOURCE_PACK_CLASS.to_string()
                ),
                project_document(1002, "Library", "lib", &MODS_CLASS.to_string()),
            ),
            "application/json",
        ))
        .mount(&server)
        .await;
    let ids = [
        JEI_PROJECT.to_string(),
        "1000".to_owned(),
        "1002".to_owned(),
    ];
    let provider = provider_against(&server, Some(FAKE_KEY));

    let batch = provider
        .project_batch_by_id(&ids, &reporter(), &CancellationToken::new())
        .await
        .expect("one project that is not a mod does not fail the batch");
    let strict = provider
        .projects_by_id(&ids, &reporter(), &CancellationToken::new())
        .await
        .expect_err("the strict lookup still refuses it");

    assert_eq!(batch.mods.len(), 2);
    assert_eq!(batch.not_mods.len(), 1);
    assert_eq!(batch.not_mods[0].id, "1000");
    assert_eq!(batch.not_mods[0].project_type, "Resource Packs");
    assert!(
        matches!(
            strict,
            ModPlatformError::ProjectTypeUnsupported { ref reference, .. } if reference == "1000"
        ),
        "{strict:?}"
    );
}

#[tokio::test]
async fn several_projects_are_asked_about_in_one_request() {
    let server = MockServer::start().await;
    serve_categories(&server).await;
    Mock::given(method("POST"))
        .and(path("/v1/mods"))
        .and(body_json(
            serde_json::json!({ "modIds": [JEI_PROJECT, 1002] }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            format!(
                r#"{{"data":[{},{}]}}"#,
                project_document(
                    JEI_PROJECT,
                    "Just Enough Items",
                    "jei",
                    &MODS_CLASS.to_string()
                ),
                project_document(1002, "Library", "lib", &MODS_CLASS.to_string()),
            ),
            "application/json",
        ))
        .expect(1)
        .mount(&server)
        .await;

    let projects = provider_against(&server, Some(FAKE_KEY))
        .projects_by_id(
            &[JEI_PROJECT.to_string(), "1002".to_owned()],
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect("both projects");
    assert_eq!(projects.len(), 2);
    assert!(
        projects
            .iter()
            .all(|project| project.provider == ModProvider::CurseForge)
    );
}

#[tokio::test]
async fn a_file_listing_leaves_out_files_owned_by_another_project() {
    let server = MockServer::start().await;
    serve_categories(&server).await;
    let foreign_project = JEI_PROJECT + 1;
    let body = format!(
        r#"{{"data":[{foreign},{own}],"pagination":{{"index":0,"pageSize":50,"resultCount":2,"totalCount":2}}}}"#,
        foreign = file_document(&server, JEI_FILE + 1, foreign_project, "Foreign 9.9.9", 1),
        own = file_document(&server, JEI_FILE, JEI_PROJECT, "JEI 15.2.0.27", 1),
    );
    Mock::given(method("GET"))
        .and(path_regex(r"^/v1/mods/\d+/files$"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/json"))
        .mount(&server)
        .await;

    let versions = provider_against(&server, Some(FAKE_KEY))
        .project_versions(
            &JEI_PROJECT.to_string(),
            &target(LoaderKind::NeoForge),
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect("a file listing");

    let ids: Vec<&str> = versions.iter().map(|version| version.id.as_str()).collect();
    assert_eq!(ids, vec![JEI_FILE.to_string()]);
}

/// A file another project owns that cannot be mapped for the reason named.
fn unmappable_foreign_file(server: &MockServer, reason: &str) -> String {
    let document = file_document(server, JEI_FILE + 1, JEI_PROJECT + 1, "Foreign 9.9.9", 1);
    match reason {
        "unavailable" => document.replace(r#""isAvailable":true"#, r#""isAvailable":false"#),
        "no supported hash" => document.replace(
            r#""hashes":[{"value":"da39a3ee5e6b4b0d3255bfef95601890afd80709","algo":1}]"#,
            r#""hashes":[]"#,
        ),
        other => panic!("unknown reason {other}"),
    }
}

#[tokio::test]
async fn a_file_listing_ignores_a_file_owned_by_another_project_that_cannot_be_mapped() {
    for reason in ["unavailable", "no supported hash"] {
        let server = MockServer::start().await;
        serve_categories(&server).await;
        let body = format!(
            r#"{{"data":[{foreign},{own}],"pagination":{{"index":0,"pageSize":50,"resultCount":2,"totalCount":2}}}}"#,
            foreign = unmappable_foreign_file(&server, reason),
            own = file_document(&server, JEI_FILE, JEI_PROJECT, "JEI 15.2.0.27", 1),
        );
        Mock::given(method("GET"))
            .and(path_regex(r"^/v1/mods/\d+/files$"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/json"))
            .mount(&server)
            .await;

        let versions = provider_against(&server, Some(FAKE_KEY))
            .project_versions(
                &JEI_PROJECT.to_string(),
                &target(LoaderKind::NeoForge),
                &reporter(),
                &CancellationToken::new(),
            )
            .await
            .unwrap_or_else(|error| panic!("a {reason} foreign file failed the listing: {error}"));

        let ids: Vec<&str> = versions.iter().map(|version| version.id.as_str()).collect();
        assert_eq!(ids, vec![JEI_FILE.to_string()], "{reason}");
    }
}

#[tokio::test]
async fn a_file_listing_still_refuses_an_own_file_that_cannot_be_mapped() {
    let server = MockServer::start().await;
    serve_categories(&server).await;
    let own = file_document(&server, JEI_FILE, JEI_PROJECT, "JEI 15.2.0.27", 1)
        .replace(r#""isAvailable":true"#, r#""isAvailable":false"#);
    let body = format!(
        r#"{{"data":[{own}],"pagination":{{"index":0,"pageSize":50,"resultCount":1,"totalCount":1}}}}"#,
    );
    Mock::given(method("GET"))
        .and(path_regex(r"^/v1/mods/\d+/files$"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/json"))
        .mount(&server)
        .await;

    let result = provider_against(&server, Some(FAKE_KEY))
        .project_versions(
            &JEI_PROJECT.to_string(),
            &target(LoaderKind::NeoForge),
            &reporter(),
            &CancellationToken::new(),
        )
        .await;

    assert!(result.is_err(), "an unavailable own file is not skipped");
}

#[tokio::test]
async fn a_search_hit_is_offered_when_a_file_owned_by_another_project_cannot_be_mapped() {
    let server = MockServer::start().await;
    let body = format!(
        r#"{{"data":[{foreign},{own}],"pagination":{{"index":0,"pageSize":50,"resultCount":2,"totalCount":2}}}}"#,
        foreign = unmappable_foreign_file(&server, "unavailable"),
        own = file_document(&server, JEI_FILE, JEI_PROJECT, "JEI 15.2.0.27", 1),
    );
    Mock::given(method("GET"))
        .and(path_regex(r"^/v1/mods/\d+/files$"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/json"))
        .mount(&server)
        .await;
    catalog(&server).await;

    let results = provider_against(&server, Some(FAKE_KEY))
        .search(
            &target(LoaderKind::NeoForge),
            &ModSearchQuery::new("jei"),
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect("a search");

    let offered: Vec<String> = results
        .hits
        .iter()
        .filter_map(|hit| hit.best_version.as_ref().map(|version| version.id.clone()))
        .collect();
    assert_eq!(offered, vec![JEI_FILE.to_string()]);
}

#[tokio::test]
async fn a_search_hit_never_offers_a_file_owned_by_another_project() {
    let server = MockServer::start().await;
    // Mounted before the shared catalog so this listing answers: the only file
    // it carries belongs to a different project.
    let body = format!(
        r#"{{"data":[{foreign}],"pagination":{{"index":0,"pageSize":50,"resultCount":1,"totalCount":1}}}}"#,
        foreign = file_document(&server, JEI_FILE + 1, JEI_PROJECT + 1, "Foreign 9.9.9", 1),
    );
    Mock::given(method("GET"))
        .and(path_regex(r"^/v1/mods/\d+/files$"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/json"))
        .mount(&server)
        .await;
    catalog(&server).await;

    let results = provider_against(&server, Some(FAKE_KEY))
        .search(
            &target(LoaderKind::NeoForge),
            &ModSearchQuery::new("jei"),
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect("a search");

    assert!(
        results.hits.is_empty(),
        "a project with no file of its own for this instance is not offered: {:?}",
        results
            .hits
            .iter()
            .map(|hit| hit.best_version.as_ref().map(|version| version.id.clone()))
            .collect::<Vec<_>>()
    );
}
