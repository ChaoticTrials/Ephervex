//! Importing a pack named by a URL rather than by a path.
//!
//! # A second trust category, not a hole in the first
//!
//! `ephervex-download`'s allowlists exist because a `.mrpack`'s
//! `files[].downloads` array is attacker-controlled text out of a bug report.
//! Nobody consented to those addresses, so they are checked against a fixed,
//! audited list and nothing else is followed.
//!
//! An address that arrives here is a different thing. A human clicked a link,
//! and before anything is fetched they are shown the address and asked. That
//! consent is what makes the fetch legitimate, and it is the only thing that
//! does: this module never widens where a pack's own files may point, and a
//! host agreed to here buys exactly one permission, to serve an archive
//! (ADR 0027).
//!
//! # What is bounded, and why it has to be
//!
//! `ExtractionLimits` bounds what an archive expands into. Nothing bounded the
//! archive itself, and an unbounded fetch of an address a web page chose is a
//! disk-exhaustion primitive. [`MAX_PACK_ARCHIVE_BYTES`] is that bound, applied
//! while streaming, so an oversized body is abandoned as it crosses the line
//! rather than after it has filled a disk.

use std::path::{Path, PathBuf};

use ephervex_download::{
    DownloadManager, DownloadRequest, DownloaderConfig, OperationReporter, Url, check_transport,
    redacted,
};
use tokio_util::sync::CancellationToken;
use tracing::{info, instrument, warn};

use crate::app::App;
use crate::error::{CoreError, CoreResult};

/// The largest pack archive Ephervex will download.
///
/// A `.mrpack` is an index plus overrides, so a few hundred megabytes is a very
/// large one and a gigabyte is not a pack. Generous enough that the limit is
/// not a thing users meet, small enough that it is still a limit.
pub const MAX_PACK_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;

/// An address a pack may be fetched from, once somebody agrees to it.
///
/// Constructing one checks the transport rules and nothing else. It is
/// deliberately not enough on its own to make a request: [`App::fetch_pack`]
/// still wants consent, so no caller can accidentally turn a parsed URL into a
/// download by forgetting a step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackSource {
    url: Url,
    host: String,
    display: String,
}

impl PackSource {
    /// The host to ask about and to remember.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The address as it should be shown to a user before they answer.
    ///
    /// Query and fragment removed, because those are where a link carries
    /// identifiers and tokens, and a confirmation prompt is not the place to
    /// print one. What the user is being asked about is which machine Ephervex
    /// will talk to.
    #[must_use]
    pub fn display(&self) -> &str {
        &self.display
    }

    #[must_use]
    pub const fn url(&self) -> &Url {
        &self.url
    }
}

/// What a caller has been told about this host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostConsent {
    /// The user agreed to this host and wants to be asked again next time.
    JustOnce,
    /// The user agreed and asked not to be prompted for this host again.
    Remember,
}

/// A downloaded pack archive, removed when it goes out of scope.
///
/// The archive is an input to the import, not a thing the launcher keeps: what
/// survives an import is the instance and the recipe retained inside it. Tying
/// the file's life to this value is what stops a failed import, a cancelled
/// one, or a frontend that forgot, from leaving downloads in the temporary
/// root.
#[derive(Debug)]
pub struct StagedPack {
    path: PathBuf,
    /// What it was fetched from, so an import can record a truthful source.
    source: PackSource,
}

impl StagedPack {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub const fn source(&self) -> &PackSource {
        &self.source
    }

    /// Keeps the file and returns where it is.
    ///
    /// For a caller that has to outlive this value. It then owns the cleanup.
    #[must_use]
    pub fn keep(mut self) -> PathBuf {
        std::mem::take(&mut self.path)
    }
}

impl Drop for StagedPack {
    fn drop(&mut self) {
        if self.path.as_os_str().is_empty() {
            return;
        }
        if let Err(error) = std::fs::remove_file(&self.path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            warn!(
                path = %self.path.display(),
                %error,
                "could not remove a downloaded pack archive"
            );
        }
    }
}

impl App {
    /// Reads an address a pack could be fetched from.
    ///
    /// Checks the transport rules and stops there: no request is made, nothing
    /// is written, and the answer is what a frontend shows the user before
    /// asking. Plaintext HTTP is refused here exactly as it is everywhere else,
    /// unless this build is a test one pointed at a loopback server.
    pub fn pack_source(&self, url: &str) -> CoreResult<PackSource> {
        let trimmed = url.trim();
        let parsed = Url::parse(trimmed).map_err(|_| CoreError::PackUrlInvalid {
            url: trimmed.to_owned(),
            detail: "not a valid absolute address".to_owned(),
        })?;
        check_transport(&parsed, self.allows_plaintext_http()).map_err(|source| {
            CoreError::PackUrlInvalid {
                url: redacted(&parsed),
                detail: source.to_string(),
            }
        })?;
        // `check_transport` has already refused an address with no host.
        let host = parsed.host_str().unwrap_or_default().to_ascii_lowercase();
        Ok(PackSource {
            display: redacted(&parsed),
            host,
            url: parsed,
        })
    }

    /// Whether this host has already been agreed to.
    #[must_use]
    pub fn pack_host_is_trusted(&self, source: &PackSource) -> bool {
        self.config().import.trusts(source.host())
    }

    /// Every host a pack may currently be fetched from without asking.
    #[must_use]
    pub fn trusted_pack_hosts(&self) -> &[String] {
        &self.config().import.trusted_pack_hosts
    }

    /// Forgets a host, so the next link pointing at it asks again.
    ///
    /// Returns whether anything was recorded for it, so a frontend can say
    /// "removed" rather than reporting success for a host nobody agreed to.
    pub fn withdraw_pack_host(&mut self, host: &str) -> CoreResult<bool> {
        let mut withdrawn = false;
        self.mutate_config(|config| withdrawn = config.import.withdraw(host))?;
        Ok(withdrawn)
    }

    /// Downloads a pack archive into a staging location.
    ///
    /// Refuses unless the host is already trusted or `consent` says the user
    /// has just agreed to it. The check lives here rather than in each frontend,
    /// so there is one place that decides whether a fetch is allowed and no way
    /// to reach the download by forgetting to ask.
    #[instrument(skip(self, reporter, cancel), fields(host = %source.host()))]
    pub async fn fetch_pack(
        &mut self,
        source: &PackSource,
        consent: Option<HostConsent>,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> CoreResult<StagedPack> {
        match consent {
            Some(HostConsent::Remember) => {
                let host = source.host().to_owned();
                self.mutate_config(|config| config.import.trust(&host))?;
            }
            Some(HostConsent::JustOnce) => {}
            None if self.pack_host_is_trusted(source) => {}
            None => {
                return Err(CoreError::PackHostNotTrusted {
                    url: source.display().to_owned(),
                    host: source.host().to_owned(),
                });
            }
        }

        let staging = pack_download_dir(self.paths());
        std::fs::create_dir_all(&staging).map_err(|source| CoreError::PackDownloadFailed {
            url: staging.display().to_string(),
            detail: source.to_string(),
        })?;
        // The name comes from us, never from the address or a
        // `Content-Disposition` header: a remote server does not get to choose
        // where a file lands. The format is detected by content anyway.
        let destination = staging.join(format!("{}.pack", uuid::Uuid::new_v4()));

        let manager = self.pack_download_manager()?;
        let request = DownloadRequest::new(source.url().clone(), &destination)
            .bounded_by(MAX_PACK_ARCHIVE_BYTES)
            .labeled(source.display().to_owned());

        let staged = StagedPack {
            path: destination,
            source: source.clone(),
        };
        // Built before the request runs, so a failure partway through still
        // takes the partial file with it when this returns.
        manager
            .download(&request, reporter, cancel)
            .await
            .map_err(|error| CoreError::PackDownloadFailed {
                url: source.display().to_owned(),
                detail: error.to_string(),
            })?;

        info!(
            host = %source.host(),
            path = %staged.path().display(),
            "downloaded a pack archive"
        );
        Ok(staged)
    }

    /// A manager for the one transfer a pack URL causes.
    fn pack_download_manager(&self) -> CoreResult<DownloadManager> {
        let config = if self.allows_plaintext_http() {
            DownloaderConfig {
                http: ephervex_download::HttpConfig::for_loopback_tests(),
                retry: ephervex_download::RetryPolicy::none(),
                ..DownloaderConfig::default()
            }
        } else {
            DownloaderConfig::default()
        };
        DownloadManager::new(config).map_err(|source| CoreError::PackDownloadFailed {
            url: String::new(),
            detail: source.to_string(),
        })
    }

    /// Whether this build may talk to a loopback HTTP server.
    ///
    /// False in every production build. The test-only escape hatch
    /// `ephervex-download` already documents, reached the same way the mod
    /// catalog reaches it, so production stays HTTPS-only.
    #[cfg(test)]
    const fn allows_plaintext_http(&self) -> bool {
        self.loopback_http
    }

    #[cfg(not(test))]
    #[allow(clippy::unused_self)]
    const fn allows_plaintext_http(&self) -> bool {
        false
    }
}

/// Where downloaded pack archives wait to be imported.
///
/// Under the temporary root rather than the cache, because the file is neither
/// reusable nor content-addressed: it is one input consumed by one import, and
/// the operating system owning whatever a crash leaves behind is the right
/// answer for it (ADR 0006).
fn pack_download_dir(paths: &ephervex_storage::Paths) -> PathBuf {
    paths.temp_dir().join("pack-downloads")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use ephervex_storage::Paths;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// An application pointed at throwaway roots, allowed to reach a loopback
    /// HTTP server. That relaxation is test-only and compiled out of every
    /// production build; the default stays HTTPS-only.
    fn app_rooted_at(base: &std::path::Path) -> App {
        let mut app = App::open_at(Paths::rooted_at(base)).expect("app opens");
        app.loopback_http = true;
        app
    }

    fn silent() -> OperationReporter {
        OperationReporter::silent("operation-downloading-pack")
    }

    async fn serving(body: Vec<u8>) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/pack"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(&server)
            .await;
        server
    }

    #[test]
    fn a_plaintext_address_is_refused_in_a_production_build() {
        let base = tempfile::tempdir().expect("temp dir");
        // Deliberately not the loopback-enabled constructor: this is the
        // production policy, and it is the one users get.
        let app = App::open_at(Paths::rooted_at(base.path())).expect("app opens");

        let error = app
            .pack_source("http://example.com/pack.mrpack")
            .expect_err("plaintext http is not a transport a pack arrives over");

        assert!(
            matches!(error, CoreError::PackUrlInvalid { .. }),
            "expected a refused address, got {error}"
        );
    }

    #[test]
    fn an_address_that_is_not_one_is_refused_before_anything_is_fetched() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());

        for candidate in [
            "not a url",
            "file:///etc/passwd",
            "https://user:secret@example.com/pack.mrpack",
            "ephervex://import?url=https%3A%2F%2Fexample.com%2Fpack.mrpack",
        ] {
            let error = app
                .pack_source(candidate)
                .expect_err("an address like this is not one a pack arrives over");
            assert!(
                matches!(error, CoreError::PackUrlInvalid { .. }),
                "expected {candidate} to be refused, got {error}"
            );
        }
    }

    /// What a confirmation prompt shows. A link carries identifiers and tokens
    /// in its query, and the question being asked is which machine Ephervex
    /// will talk to.
    #[test]
    fn the_address_a_user_is_shown_carries_no_query_or_credentials() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());

        let source = app
            .pack_source(
                "https://dumps.example.com/api/dump/abc/modpack?platform=modrinth&token=s3cret",
            )
            .expect("a well-formed address");

        assert_eq!(source.host(), "dumps.example.com");
        assert_eq!(
            source.display(),
            "https://dumps.example.com/api/dump/abc/modpack"
        );
        assert!(!source.display().contains("s3cret"));
        // The request itself still carries everything the server needs.
        assert_eq!(source.url().query(), Some("platform=modrinth&token=s3cret"));
    }

    #[tokio::test]
    async fn a_host_nobody_agreed_to_is_refused_rather_than_fetched() {
        let base = tempfile::tempdir().expect("temp dir");
        let server = serving(b"never read".to_vec()).await;
        let mut app = app_rooted_at(base.path());
        let source = app
            .pack_source(&format!("{}/pack", server.uri()))
            .expect("a loopback address");

        let error = app
            .fetch_pack(&source, None, &silent(), &CancellationToken::new())
            .await
            .expect_err("nobody has agreed to this host");

        assert!(
            matches!(error, CoreError::PackHostNotTrusted { .. }),
            "expected a consent refusal, got {error}"
        );
        assert_eq!(
            server.received_requests().await.expect("requests").len(),
            0,
            "a refusal must happen before anything reaches the network"
        );
    }

    #[tokio::test]
    async fn agreeing_once_downloads_the_pack_and_remembers_nothing() {
        let base = tempfile::tempdir().expect("temp dir");
        let server = serving(b"a pack archive".to_vec()).await;
        let mut app = app_rooted_at(base.path());
        let source = app
            .pack_source(&format!("{}/pack", server.uri()))
            .expect("a loopback address");

        let staged = app
            .fetch_pack(
                &source,
                Some(HostConsent::JustOnce),
                &silent(),
                &CancellationToken::new(),
            )
            .await
            .expect("download");

        assert_eq!(
            std::fs::read(staged.path()).expect("read"),
            b"a pack archive"
        );
        assert!(
            !app.pack_host_is_trusted(&source),
            "answering once must not record an answer for next time"
        );
    }

    #[tokio::test]
    async fn a_remembered_host_is_not_asked_about_again_after_a_restart() {
        let base = tempfile::tempdir().expect("temp dir");
        let server = serving(b"a pack archive".to_vec()).await;
        let mut app = app_rooted_at(base.path());
        let source = app
            .pack_source(&format!("{}/pack", server.uri()))
            .expect("a loopback address");

        app.fetch_pack(
            &source,
            Some(HostConsent::Remember),
            &silent(),
            &CancellationToken::new(),
        )
        .await
        .expect("download");

        // A new application over the same roots, which is what the next launch
        // reads rather than what this process remembers.
        drop(app);
        let mut reopened = app_rooted_at(base.path());
        assert!(reopened.pack_host_is_trusted(&source));

        let staged = reopened
            .fetch_pack(&source, None, &silent(), &CancellationToken::new())
            .await
            .expect("an agreed host needs no second answer");
        assert_eq!(
            std::fs::read(staged.path()).expect("read"),
            b"a pack archive"
        );

        assert!(
            reopened
                .withdraw_pack_host(source.host())
                .expect("withdraw")
        );
        assert!(
            !reopened.pack_host_is_trusted(&source),
            "an answer has to be revocable"
        );
    }

    /// The archive is an input to an import, not something the launcher keeps.
    #[tokio::test]
    async fn a_downloaded_archive_is_removed_when_nothing_holds_it() {
        let base = tempfile::tempdir().expect("temp dir");
        let server = serving(b"a pack archive".to_vec()).await;
        let mut app = app_rooted_at(base.path());
        let source = app
            .pack_source(&format!("{}/pack", server.uri()))
            .expect("a loopback address");

        let staged = app
            .fetch_pack(
                &source,
                Some(HostConsent::JustOnce),
                &silent(),
                &CancellationToken::new(),
            )
            .await
            .expect("download");
        let path = staged.path().to_path_buf();
        assert!(path.is_file());

        drop(staged);
        assert!(
            !path.exists(),
            "a failed or finished import leaves no downloaded archive behind"
        );
    }

    /// The whole point of the capability: an address a web page offered turns
    /// into a pack the existing import pipeline can read, with no file picker
    /// and no manual download in between.
    #[tokio::test]
    async fn a_pack_named_by_an_address_becomes_one_the_importer_can_read() {
        use std::io::Write as _;

        let base = tempfile::tempdir().expect("temp dir");
        // A minimal Modrinth pack: an index and one override. Shaped like what
        // DumpViewer serves, which is an index plus `overrides/`.
        let index = br#"{"formatVersion":1,"game":"minecraft","versionId":"1.0.0",
            "name":"SkyBlock","files":[],
            "dependencies":{"minecraft":"1.21.1","neoforge":"21.1.115"}}"#;
        let mut archive = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut archive));
            let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            writer
                .start_file("modrinth.index.json", options)
                .expect("index");
            writer.write_all(index).expect("write index");
            writer
                .start_file(
                    "overrides/config/skyblockbuilder/customization.json5",
                    options,
                )
                .expect("override");
            writer.write_all(b"{}").expect("write override");
            writer.finish().expect("finish");
        }

        let server = serving(archive).await;
        let mut app = app_rooted_at(base.path());
        let source = app
            .pack_source(&format!("{}/pack", server.uri()))
            .expect("a loopback address");

        let staged = app
            .fetch_pack(
                &source,
                Some(HostConsent::Remember),
                &silent(),
                &CancellationToken::new(),
            )
            .await
            .expect("download");

        // The format is decided by content, so nothing depended on the address
        // ending in `.mrpack`. It does not.
        assert!(!source.url().path().ends_with(".mrpack"));
        let analysis = app
            .analyze_pack(staged.path())
            .expect("the download is a pack");
        assert_eq!(analysis.name(), "SkyBlock");
        assert_eq!(analysis.minecraft_version(), "1.21.1");
    }

    #[tokio::test]
    async fn an_archive_larger_than_the_ceiling_is_abandoned_rather_than_written_out() {
        let base = tempfile::tempdir().expect("temp dir");
        // Served without a Content-Length the manager could refuse up front, so
        // the bound has to hold while the body streams.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/pack"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0u8; 2 * 1024 * 1024]))
            .mount(&server)
            .await;
        let app = app_rooted_at(base.path());
        let source = app
            .pack_source(&format!("{}/pack", server.uri()))
            .expect("a loopback address");

        let request_ceiling = 1024;
        let staging = pack_download_dir(app.paths());
        std::fs::create_dir_all(&staging).expect("staging");
        let destination = staging.join("bounded.pack");
        let manager = app.pack_download_manager().expect("manager");
        let request =
            DownloadRequest::new(source.url().clone(), &destination).bounded_by(request_ceiling);

        let error = manager
            .download(&request, &silent(), &CancellationToken::new())
            .await
            .expect_err("a body past the ceiling is refused");

        assert!(
            !destination.exists(),
            "an abandoned download leaves no partial file: {error}"
        );
    }
}
