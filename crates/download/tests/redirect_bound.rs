//! Every client honors the same `HttpConfig::max_redirects` bound: a chain of
//! exactly that many redirects is followed and one more is refused.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ephervex_download::{
    DomainAllowlist, HttpConfig, build_allowlisted_client, build_client,
    build_origin_contained_client, build_single_origin_client,
};
use reqwest::{Client, Url};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const BODY: &str = "end of the chain";

/// Serves `/hop/0` ... `/hop/{hops - 1}`, each redirecting to the next, with
/// `/hop/{hops}` answering 200. Requesting `/hop/0` therefore takes `hops`
/// redirects to reach the body.
async fn server_with_chain(hops: usize) -> MockServer {
    let server = MockServer::start().await;
    for hop in 0..hops {
        Mock::given(method("GET"))
            .and(path(format!("/hop/{hop}")))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", format!("{}/hop/{}", server.uri(), hop + 1)),
            )
            .mount(&server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path(format!("/hop/{hops}")))
        .respond_with(ResponseTemplate::new(200).set_body_string(BODY))
        .mount(&server)
        .await;
    server
}

fn config(max_redirects: usize) -> HttpConfig {
    HttpConfig {
        max_redirects,
        ..HttpConfig::for_loopback_tests()
    }
}

async fn fetch(client: &Client, server: &MockServer) -> Result<String, reqwest::Error> {
    let url = Url::parse(&format!("{}/hop/0", server.uri())).expect("loopback url");
    client.get(url).send().await?.text().await
}

/// Asserts the client follows `max_redirects` redirects and refuses the next.
async fn assert_bound(max_redirects: usize, build: impl Fn(&HttpConfig, &MockServer) -> Client) {
    let exact = server_with_chain(max_redirects).await;
    let client = build(&config(max_redirects), &exact);
    assert_eq!(
        fetch(&client, &exact)
            .await
            .expect("a chain of exactly max_redirects redirects is followed"),
        BODY
    );

    let one_more = server_with_chain(max_redirects + 1).await;
    let client = build(&config(max_redirects), &one_more);
    let error = fetch(&client, &one_more)
        .await
        .expect_err("one redirect past max_redirects is refused");
    assert!(
        error.is_redirect(),
        "expected a redirect error, got {error:?}"
    );
}

#[tokio::test]
async fn the_ordinary_client_follows_exactly_the_configured_redirects() {
    assert_bound(3, |config, _| build_client(config).expect("client builds")).await;
}

#[tokio::test]
async fn a_single_origin_client_follows_exactly_the_configured_redirects() {
    assert_bound(3, |config, server| {
        let origin = Url::parse(&server.uri()).expect("loopback url");
        build_single_origin_client(config, &origin).expect("client builds")
    })
    .await;
}

#[tokio::test]
async fn an_origin_contained_client_follows_exactly_the_configured_redirects() {
    assert_bound(3, |config, _| {
        build_origin_contained_client(config).expect("client builds")
    })
    .await;
}

#[tokio::test]
async fn an_allowlisted_client_follows_exactly_the_configured_redirects() {
    assert_bound(3, |config, _| {
        build_allowlisted_client(config, &DomainAllowlist::for_loopback_tests("127.0.0.1"))
            .expect("client builds")
    })
    .await;
}

#[tokio::test]
async fn a_zero_redirect_bound_refuses_the_first_redirect() {
    let server = server_with_chain(1).await;
    let client = build_single_origin_client(
        &config(0),
        &Url::parse(&server.uri()).expect("loopback url"),
    )
    .expect("client builds");
    assert!(fetch(&client, &server).await.unwrap_err().is_redirect());
}
