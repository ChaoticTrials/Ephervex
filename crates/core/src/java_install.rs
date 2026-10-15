//! Installing a managed runtime for one host, on the user's request.
//!
//! The install applies the same loader update ceiling a launch would, and runs
//! even with automatic download off: the request is the consent. It never
//! changes which runtime a host uses. When the host would not pick the new
//! runtime up on its own, the result offers the previewed change that pins it
//! (ADR 0043).

use tracing::instrument;

use crate::app::App;
use crate::diagnosis::{DiagnosticFix, JavaCheck};
use crate::error::CoreResult;
use crate::java_status::{HostJavaStatus, JavaSelectionMode};
use crate::mods::InstalledHost;
use crate::{CancellationToken, ManagedInstallation, OperationReporter};

/// What installing a runtime for a host did, and what the host does now.
#[derive(Debug)]
pub struct HostJavaInstall {
    pub installation: ManagedInstallation,
    /// The host's Java once the runtime is installed.
    pub status: HostJavaStatus,
    /// The previewable change pinning the new runtime, offered when the host
    /// pins another runtime or still resolves to none.
    pub follow_up: Option<DiagnosticFix>,
}

impl App {
    /// Installs the managed runtime a host expects: its required major, no
    /// later than the update its loader accepts.
    #[instrument(skip(self, reporter, cancel), fields(host = %host.id()))]
    pub async fn install_java_for_host(
        &self,
        host: InstalledHost,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> CoreResult<HostJavaInstall> {
        let facts = self.host_facts(host)?;
        let expected = self
            .host_java_rules(&facts)
            .await?
            .expected(facts.loader.kind);
        let installation = self
            .install_java_with_maximum_update(
                expected.major(),
                expected.java.maximum_update(),
                Some(reporter),
                cancel,
            )
            .await?;
        let status = self.host_java_status(host).await?;
        let resolves = matches!(status.check, JavaCheck::Selected { .. });
        let follow_up = (status.mode != JavaSelectionMode::Automatic || !resolves)
            .then(|| DiagnosticFix::PinJava(installation.runtime.executable.clone()));
        Ok(HostJavaInstall {
            installation,
            status,
            follow_up,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::path::{Path, PathBuf};

    use ephervex_download::{CacheLayout, ContentStore, FileExtension, MetadataCache, MetadataKey};
    use ephervex_minecraft::{VERSION_MANIFEST_CACHE_KEY, VERSION_MANIFEST_URL, VersionManifest};
    use ephervex_model::{JavaOverride, LoaderKind, LoaderSelection};
    use ephervex_storage::{NewClient, NewServer, Paths};
    use sha2::{Digest, Sha256};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::java_status::JavaStatusTone;

    const RELEASE: &str = "jdk8u312-b07";
    const ARCHIVE_ROUTE: &str = "/download/OpenJDK8U-jdk_linux_hotspot_8u312b07.tar.gz";
    /// The ceiling legacy Forge applies, as the version-range request spells
    /// it: Java 8 from 8.0.0 up to, not including, update 321.
    const CAPPED_ROUTE: &str = "/v3/assets/version/%5B8%2C8.0.321%29";

    const fn jvm_arch() -> &'static str {
        if cfg!(target_arch = "aarch64") {
            "aarch64"
        } else {
            "amd64"
        }
    }

    /// A Temurin-shaped tarball whose `bin/java` reports 8u312.
    fn archive() -> Vec<u8> {
        let script = format!(
            "#!/bin/sh\ncat >&2 <<'EOF'\nProperty settings:\n    java.home = /opt/{RELEASE}\n    \
             java.runtime.version = 1.8.0_312-b07\n    java.vendor = Temurin\n    \
             java.version = 1.8.0_312\n    os.arch = {arch}\nEOF\n",
            arch = jvm_arch(),
        );
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        let mut header = tar::Header::new_gnu();
        header.set_size(script.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder
            .append_data(
                &mut header,
                format!("{RELEASE}/bin/java"),
                script.as_bytes(),
            )
            .unwrap();
        builder.into_inner().unwrap().finish().unwrap()
    }

    /// Serves the release only through the capped version-range query, so an
    /// install that ignored the ceiling would find nothing.
    async fn adoptium() -> MockServer {
        let server = MockServer::start().await;
        let archive = archive();
        let checksum = hex::encode(Sha256::digest(&archive));
        let index = serde_json::json!([{
            "binaries": [{"package": {
                "link": format!("{}{ARCHIVE_ROUTE}", server.uri()),
                "name": "OpenJDK8U-jdk_linux_hotspot_8u312b07.tar.gz",
                "checksum": checksum,
                "size": archive.len(),
            }}],
            "release_name": RELEASE,
            "vendor": "eclipse",
        }]);
        Mock::given(method("GET"))
            .and(path(CAPPED_ROUTE))
            .respond_with(ResponseTemplate::new(200).set_body_json(index))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(ARCHIVE_ROUTE))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(archive))
            .mount(&server)
            .await;
        server
    }

    /// Mojang's manifest and the release documents the fixtures cover, stored
    /// as fresh so resolving a release reaches no network.
    async fn seed_minecraft(cache: &Path) {
        let layout = CacheLayout::new(cache);
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/minecraft");
        let manifest_json =
            std::fs::read_to_string(fixtures.join("version_manifest_v2.json")).unwrap();
        let manifest = VersionManifest::parse(&manifest_json).unwrap();
        MetadataCache::new(layout.clone())
            .store(
                &MetadataKey::new(VERSION_MANIFEST_CACHE_KEY).unwrap(),
                &VERSION_MANIFEST_URL.parse().unwrap(),
                serde_json::value::RawValue::from_string(manifest_json).unwrap(),
            )
            .await
            .unwrap();
        let json = FileExtension::new("json").unwrap();
        for record in &manifest.versions {
            let Ok(document) = std::fs::read(fixtures.join(format!("{}.json", record.id))) else {
                continue;
            };
            let path = layout.content(ContentStore::Metadata, &record.sha1, Some(&json));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &document).unwrap();
        }
    }

    struct Fixture {
        _directory: tempfile::TempDir,
        _adoptium: MockServer,
        app: App,
        client: InstalledHost,
        server: InstalledHost,
    }

    /// A legacy Forge client and server, with detection on, downloads off and
    /// nothing installed.
    async fn legacy_forge() -> Fixture {
        let directory = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(directory.path());
        seed_minecraft(paths.cache_dir()).await;
        let adoptium = adoptium().await;
        let mut app = App::open_at(paths).unwrap();
        app.loopback_http = true;
        app.adoptium_base = Some(adoptium.uri());
        let mut settings = app.java_settings();
        settings.auto_detect = true;
        settings.auto_download = false;
        app.set_java_settings(&settings).unwrap();
        app.use_java_inventory_for_test(Vec::new());
        let loader = LoaderSelection {
            kind: LoaderKind::Forge,
            version: Some("14.23.5.2860".to_owned()),
        };
        let client = app
            .registry()
            .create_client(NewClient::new("Forge", "1.12.2", loader.clone()))
            .unwrap();
        app.registry().publish_client(&client).unwrap();
        let server = app
            .registry()
            .create_server(NewServer::new("Forge server", "1.12.2", loader))
            .unwrap();
        Fixture {
            _directory: directory,
            _adoptium: adoptium,
            app,
            client: InstalledHost::Instance(client.id),
            server: InstalledHost::Server(server.id),
        }
    }

    async fn install(fixture: &Fixture, host: InstalledHost) -> HostJavaInstall {
        fixture
            .app
            .install_java_for_host(
                host,
                &OperationReporter::silent("installing"),
                &CancellationToken::new(),
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn an_explicit_install_respects_the_loader_ceiling_with_downloads_off() {
        let fixture = legacy_forge().await;
        let before = fixture.app.host_java_status(fixture.client).await.unwrap();
        assert_eq!(before.tone(), JavaStatusTone::Problem);
        let offer = before.install.expect("an install offer");
        assert_eq!((offer.major, offer.maximum_update), (8, Some(320)));

        let installed = install(&fixture, fixture.client).await;

        assert_eq!(installed.installation.runtime.version.update, 312);
        assert!(installed.installation.runtime.is_managed());
        assert_eq!(installed.status.tone(), JavaStatusTone::Healthy);
        assert_eq!(installed.status.mode, JavaSelectionMode::Automatic);
        assert_eq!(
            installed.follow_up, None,
            "an automatic host picks the new runtime up by itself"
        );
        assert!(installed.status.install.is_none());
        assert_eq!(
            fixture
                .app
                .current_client(match fixture.client {
                    InstalledHost::Instance(id) => id,
                    InstalledHost::Server(_) => unreachable!(),
                })
                .unwrap()
                .java_override,
            None
        );
    }

    #[tokio::test]
    async fn an_install_for_a_pinned_host_offers_the_pin_and_does_not_apply_it() {
        let fixture = legacy_forge().await;
        let InstalledHost::Server(id) = fixture.server else {
            unreachable!()
        };
        let late = PathBuf::from("/nonexistent/late-jdk8/bin/java");
        fixture
            .app
            .set_server_runtime_settings(id, Some(JavaOverride::new(&late)), None)
            .unwrap();

        let installed = install(&fixture, fixture.server).await;

        assert_eq!(
            installed.follow_up,
            Some(DiagnosticFix::PinJava(
                installed.installation.runtime.executable.clone()
            ))
        );
        assert_eq!(installed.status.mode, JavaSelectionMode::PinnedByHost);
        assert_eq!(
            fixture.app.server(id).unwrap().java_override,
            Some(JavaOverride::new(&late)),
            "the host keeps its own pin until the change is applied"
        );
    }

    #[tokio::test]
    async fn an_install_with_detection_off_offers_the_pin_because_nothing_resolves() {
        let mut fixture = legacy_forge().await;
        let mut settings = fixture.app.java_settings();
        settings.auto_detect = false;
        fixture.app.set_java_settings(&settings).unwrap();

        let installed = install(&fixture, fixture.client).await;

        assert_eq!(installed.status.mode, JavaSelectionMode::Automatic);
        assert!(matches!(
            installed.follow_up,
            Some(DiagnosticFix::PinJava(_))
        ));
    }
}
