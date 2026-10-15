//! Which Java a client or server would run with, what it expects, and whether
//! the two fit, as the per-host status and Diagnostics both report it.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use ephervex_core::{
    App, DiagnosisRequest, FindingClass, FindingKind, HostJavaStatus, InstalledHost, JavaCheck,
    JavaLayer, JavaMismatch, JavaSelectionMode, JavaStatusTone, Paths,
};
use ephervex_download::{
    CacheLayout, ContentStore, FileExtension, MetadataCache, MetadataKey, OperationReporter,
};
use ephervex_java::{JavaOrigin, JavaRuntime, JavaSourceKind, JavaVersion};
use ephervex_minecraft::{VERSION_MANIFEST_CACHE_KEY, VERSION_MANIFEST_URL, VersionManifest};
use ephervex_model::{JavaOverride, LoaderKind, LoaderSelection};
use ephervex_storage::{NewClient, NewServer, Registry};
use tokio_util::sync::CancellationToken;

struct Fixture {
    directory: tempfile::TempDir,
    app: App,
    client: InstalledHost,
    server: InstalledHost,
}

impl Fixture {
    /// A client and a server of one release and loader. Nothing on this
    /// machine is scanned: detection starts off, and a test that turns it on
    /// seeds the inventory first.
    async fn new(minecraft: &str, loader: LoaderSelection) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(directory.path());
        let registry = Registry::open(paths.clone()).unwrap();
        let client = registry
            .create_client(NewClient::new("Java", minecraft, loader.clone()))
            .unwrap();
        registry.publish_client(&client).unwrap();
        let server = registry
            .create_server(NewServer::new("Java server", minecraft, loader))
            .unwrap();
        seed_minecraft(paths.cache_dir()).await;
        let mut app = App::open_at(paths).unwrap();
        let mut settings = app.java_settings();
        settings.auto_detect = false;
        settings.auto_download = false;
        app.set_java_settings(&settings).unwrap();
        Self {
            directory,
            app,
            client: InstalledHost::Instance(client.id),
            server: InstalledHost::Server(server.id),
        }
    }

    async fn vanilla() -> Self {
        Self::new("1.21.1", LoaderSelection::vanilla()).await
    }

    fn hosts(&self) -> [InstalledHost; 2] {
        [self.client, self.server]
    }

    fn root(&self) -> &Path {
        self.directory.path()
    }

    fn detect(&mut self, auto_detect: bool, auto_download: bool) {
        let mut settings = self.app.java_settings();
        settings.auto_detect = auto_detect;
        settings.auto_download = auto_download;
        self.app.set_java_settings(&settings).unwrap();
    }

    fn prefer(&mut self, path: &Path) {
        let mut settings = self.app.java_settings();
        settings.preferred_path = Some(path.to_path_buf());
        self.app.set_java_settings(&settings).unwrap();
    }

    fn pin(&self, host: InstalledHost, path: &Path) {
        let java = Some(JavaOverride::new(path));
        match host {
            InstalledHost::Instance(id) => {
                self.app
                    .set_client_runtime_settings(id, java, None)
                    .unwrap();
            }
            InstalledHost::Server(id) => {
                self.app
                    .set_server_runtime_settings(id, java, None)
                    .unwrap();
            }
        }
    }

    /// A fake runtime of `version` under `name` in the fixture directory.
    fn java(&self, name: &str, version: &str) -> PathBuf {
        fake_java(&self.root().join(name), version, jvm_arch())
    }

    /// A fake runtime where Ephervex installs managed ones.
    fn managed_java(&self, version: &str) -> PathBuf {
        let home = self
            .app
            .paths()
            .java_dir()
            .join(format!("21/eclipse-jdk-{version}+7"));
        fake_java(&home, version, jvm_arch())
    }

    async fn status(&self, host: InstalledHost) -> HostJavaStatus {
        self.app.host_java_status(host).await.unwrap()
    }
}

/// An executable that answers the property probe as a JVM would.
fn fake_java(home: &Path, version: &str, arch: &str) -> PathBuf {
    let executable = home.join("bin/java");
    std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
    std::fs::write(
        &executable,
        format!(
            "#!/bin/sh\ncat >&2 <<'EOF'\nProperty settings:\n    java.home = {home}\n    \
             java.runtime.version = {version}+7\n    java.vendor = Eclipse Adoptium\n    \
             java.version = {version}\n    os.arch = {arch}\nEOF\n",
            home = home.display(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::canonicalize(executable).unwrap()
}

/// What a scan would have recorded for `executable`.
fn scanned(executable: &Path, version: &str, vendor: &str, kind: JavaSourceKind) -> JavaRuntime {
    let parsed = JavaVersion::parse(version).unwrap();
    JavaRuntime {
        executable: executable.to_path_buf(),
        home: None,
        version: parsed,
        version_string: version.to_owned(),
        runtime_version: format!("{version}+7"),
        vendor: vendor.to_owned(),
        arch: jvm_arch().to_owned(),
        origins: vec![JavaOrigin {
            kind,
            path: executable.to_path_buf(),
        }],
    }
}

const fn jvm_arch() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "amd64"
    }
}

const fn foreign_arch() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "amd64"
    } else {
        "aarch64"
    }
}

/// Mojang's manifest and the release documents the fixtures cover, stored as
/// fresh so resolving a release reaches no network.
async fn seed_minecraft(cache: &Path) {
    let layout = CacheLayout::new(cache);
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/minecraft");
    let manifest_json = std::fs::read_to_string(fixtures.join("version_manifest_v2.json")).unwrap();
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

fn problem(status: &HostJavaStatus) -> &FindingKind {
    let problems: Vec<&FindingKind> = status
        .findings
        .iter()
        .filter(|finding| finding.class() == FindingClass::Problem)
        .map(|finding| &finding.kind)
        .collect();
    let [problem] = problems.as_slice() else {
        panic!("expected one problem: {:?}", status.findings);
    };
    problem
}

#[tokio::test]
async fn automatic_selection_of_a_system_runtime_is_healthy() {
    let mut fixture = Fixture::vanilla().await;
    let java = fixture.java("system-jdk", "21.0.8");
    ephervex_core::test_support::use_java_inventory(
        &fixture.app,
        vec![scanned(&java, "21.0.8", "Ubuntu", JavaSourceKind::System)],
    );
    fixture.detect(true, false);

    for host in fixture.hosts() {
        let status = fixture.status(host).await;
        assert_eq!(status.tone(), JavaStatusTone::Healthy, "{host:?}");
        assert_eq!(status.mode, JavaSelectionMode::Automatic);
        assert_eq!(status.expected.major(), 21);
        assert_eq!(status.expected.bounded_by(), None);
        let runtime = status.runtime().expect("a runtime");
        assert_eq!(runtime.path, java);
        assert_eq!(runtime.vendor, "Ubuntu");
        assert_eq!(runtime.arch, jvm_arch());
        assert_eq!(runtime.source, JavaSourceKind::System);
        assert!(!runtime.managed);
        assert!(status.findings.is_empty());
        assert_eq!(status.install, None);
    }
}

#[tokio::test]
async fn automatic_selection_of_a_managed_runtime_is_healthy_and_marked_managed() {
    let mut fixture = Fixture::vanilla().await;
    let java = fixture.managed_java("21.0.8");
    ephervex_core::test_support::use_java_inventory(
        &fixture.app,
        vec![scanned(
            &java,
            "21.0.8",
            "Eclipse Adoptium",
            JavaSourceKind::Managed,
        )],
    );
    fixture.detect(true, false);

    for host in fixture.hosts() {
        let status = fixture.status(host).await;
        assert_eq!(status.tone(), JavaStatusTone::Healthy, "{host:?}");
        assert_eq!(status.mode, JavaSelectionMode::Automatic);
        let runtime = status.runtime().expect("a runtime");
        assert!(runtime.managed);
        assert_eq!(runtime.source, JavaSourceKind::Managed);
    }
}

#[tokio::test]
async fn nothing_installed_with_downloads_on_is_healthy_and_offers_an_install() {
    let mut fixture = Fixture::vanilla().await;
    ephervex_core::test_support::use_java_inventory(&fixture.app, Vec::new());
    fixture.detect(true, true);

    for host in fixture.hosts() {
        let status = fixture.status(host).await;
        assert!(
            matches!(
                status.check,
                JavaCheck::DownloadsAtLaunch { required_major: 21 }
            ),
            "{:?}",
            status.check
        );
        assert_eq!(status.tone(), JavaStatusTone::Healthy);
        let offer = status.install.expect("an install offer");
        assert_eq!(offer.major, 21);
        assert_eq!(offer.maximum_update, None);
    }
}

#[tokio::test]
async fn a_host_pin_that_satisfies_every_rule_is_customized() {
    let fixture = Fixture::vanilla().await;
    let java = fixture.java("pinned-jdk", "21.0.8");

    for host in fixture.hosts() {
        fixture.pin(host, &java);
        let status = fixture.status(host).await;
        assert_eq!(status.tone(), JavaStatusTone::Customized, "{host:?}");
        assert_eq!(status.mode, JavaSelectionMode::PinnedByHost);
        assert_eq!(status.pinned_path.as_deref(), Some(java.as_path()));
        let runtime = status.runtime().expect("a runtime");
        assert_eq!(runtime.vendor, "Eclipse Adoptium");
        assert_eq!(runtime.arch, jvm_arch());
        assert_eq!(status.install, None);
    }
}

#[tokio::test]
async fn a_launcher_pin_is_customized_and_named_as_the_launcher_s() {
    let mut fixture = Fixture::vanilla().await;
    let java = fixture.java("launcher-jdk", "21.0.8");
    fixture.prefer(&java);

    for host in fixture.hosts() {
        let status = fixture.status(host).await;
        assert_eq!(status.tone(), JavaStatusTone::Customized, "{host:?}");
        assert_eq!(status.mode, JavaSelectionMode::PinnedByLauncher);
        assert!(
            matches!(
                status.findings.as_slice(),
                [ephervex_core::Finding {
                    kind: FindingKind::JavaCustomized {
                        layer: JavaLayer::Launcher,
                        ..
                    },
                    ..
                }]
            ),
            "{:?}",
            status.findings
        );
    }
}

#[tokio::test]
async fn a_pin_of_the_wrong_major_is_a_problem_that_names_the_refused_runtime() {
    let fixture = Fixture::vanilla().await;
    let java = fixture.java("old-jdk", "17.0.12");

    for host in fixture.hosts() {
        fixture.pin(host, &java);
        let status = fixture.status(host).await;
        assert_eq!(status.tone(), JavaStatusTone::Problem, "{host:?}");
        assert!(
            matches!(
                problem(&status),
                FindingKind::JavaIncompatible {
                    layer: JavaLayer::Host,
                    mismatch: JavaMismatch::Major {
                        required: 21,
                        found: 17
                    },
                    ..
                }
            ),
            "{:?}",
            status.findings
        );
        let refused = status.runtime().expect("the refused runtime");
        assert_eq!(refused.version, "17.0.12");
        assert_eq!(refused.vendor, "Eclipse Adoptium");
        assert_eq!(
            status.install.map(|offer| offer.major),
            Some(21),
            "nothing installed satisfies the host"
        );
    }
}

#[tokio::test]
async fn legacy_forge_refuses_a_java_8_above_its_update_ceiling() {
    let fixture = Fixture::new(
        "1.12.2",
        LoaderSelection {
            kind: LoaderKind::Forge,
            version: Some("14.23.5.2860".to_owned()),
        },
    )
    .await;
    let late = fixture.java("late-jdk8", "1.8.0_402");

    for host in fixture.hosts() {
        fixture.pin(host, &late);
        let status = fixture.status(host).await;
        assert_eq!(status.expected.major(), 8);
        assert_eq!(status.expected.bounded_by(), Some(LoaderKind::Forge));
        assert_eq!(status.expected.java.maximum_update(), Some(320));
        assert!(
            matches!(
                problem(&status),
                FindingKind::JavaIncompatible {
                    mismatch: JavaMismatch::TooNew { .. },
                    ..
                }
            ),
            "{:?}",
            status.findings
        );
        assert_eq!(
            status.install.map(|offer| offer.maximum_update),
            Some(Some(320)),
            "an install respects the loader's ceiling"
        );
    }
}

#[tokio::test]
async fn a_pin_built_for_another_architecture_is_a_problem() {
    let fixture = Fixture::vanilla().await;
    let java = fake_java(
        &fixture.root().join("foreign-jdk"),
        "21.0.8",
        foreign_arch(),
    );

    for host in fixture.hosts() {
        fixture.pin(host, &java);
        let status = fixture.status(host).await;
        assert!(
            matches!(
                problem(&status),
                FindingKind::JavaIncompatible {
                    mismatch: JavaMismatch::Architecture { .. },
                    ..
                }
            ),
            "{host:?}: {:?}",
            status.findings
        );
        assert_eq!(
            status.runtime().map(|runtime| runtime.arch.as_str()),
            Some(foreign_arch())
        );
    }
}

#[tokio::test]
async fn a_pin_that_is_not_a_runtime_is_an_unusable_problem() {
    let fixture = Fixture::vanilla().await;
    let not_java = fixture.root().join("not-java");
    std::fs::write(&not_java, b"plain text").unwrap();

    for host in fixture.hosts() {
        fixture.pin(host, &not_java);
        let status = fixture.status(host).await;
        assert!(
            matches!(
                problem(&status),
                FindingKind::JavaUnusable {
                    layer: JavaLayer::Host,
                    ..
                }
            ),
            "{host:?}: {:?}",
            status.findings
        );
        assert!(status.runtime().is_none());
    }
}

#[tokio::test]
async fn a_pin_to_a_removed_managed_runtime_is_an_unusable_problem() {
    let fixture = Fixture::vanilla().await;
    let java = fixture.managed_java("21.0.8");
    for host in fixture.hosts() {
        fixture.pin(host, &java);
        assert_eq!(
            fixture.status(host).await.tone(),
            JavaStatusTone::Customized
        );
    }
    std::fs::remove_dir_all(java.parent().unwrap().parent().unwrap()).unwrap();

    for host in fixture.hosts() {
        let status = fixture.status(host).await;
        assert!(
            matches!(problem(&status), FindingKind::JavaUnusable { path, .. } if path == &java),
            "{host:?}: {:?}",
            status.findings
        );
    }
}

#[tokio::test]
async fn a_pinned_managed_runtime_is_marked_managed() {
    let fixture = Fixture::vanilla().await;
    let java = fixture.managed_java("21.0.8");

    for host in fixture.hosts() {
        fixture.pin(host, &java);
        let status = fixture.status(host).await;
        assert_eq!(status.mode, JavaSelectionMode::PinnedByHost);
        assert!(status.runtime().expect("a runtime").managed, "{host:?}");
    }
}

#[tokio::test]
async fn a_pin_the_inventory_already_knows_is_not_probed_again() {
    let fixture = Fixture::vanilla().await;
    let java = fixture.java("known-jdk", "21.0.8");
    ephervex_core::test_support::use_java_inventory(
        &fixture.app,
        vec![scanned(
            &java,
            "21.0.8",
            "Recorded By The Scan",
            JavaSourceKind::System,
        )],
    );

    for host in fixture.hosts() {
        fixture.pin(host, &java);
        let status = fixture.status(host).await;
        assert_eq!(
            status.runtime().expect("a runtime").vendor,
            "Recorded By The Scan",
            "{host:?}"
        );
    }
}

#[tokio::test]
async fn a_pin_reports_the_same_origin_whether_it_was_probed_or_already_known() {
    let fixture = Fixture::vanilla().await;
    let java = fixture.java("origin-jdk", "21.0.8");
    let host = fixture.client;
    fixture.pin(host, &java);
    let probed = fixture.status(host).await.runtime().cloned().unwrap();

    ephervex_core::test_support::use_java_inventory(
        &fixture.app,
        vec![scanned(
            &java,
            "21.0.8",
            "Eclipse Adoptium",
            JavaSourceKind::System,
        )],
    );
    let known = fixture.status(host).await.runtime().cloned().unwrap();

    assert_eq!(probed, known);
    assert_eq!(known.source, JavaSourceKind::UserSupplied);
}

#[tokio::test]
async fn the_recommendation_behind_a_host_pin_reuses_a_known_launcher_preference() {
    let mut fixture = Fixture::vanilla().await;
    // The preferred executable cannot be probed, so only the inventory entry
    // can make it a working recommendation.
    let preferred = fixture.root().join("preferred/bin/java");
    std::fs::create_dir_all(preferred.parent().unwrap()).unwrap();
    std::fs::write(&preferred, "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(&preferred, std::fs::Permissions::from_mode(0o755)).unwrap();
    let preferred = std::fs::canonicalize(preferred).unwrap();
    ephervex_core::test_support::use_java_inventory(
        &fixture.app,
        vec![scanned(
            &preferred,
            "21.0.8",
            "Eclipse Adoptium",
            JavaSourceKind::System,
        )],
    );
    fixture.prefer(&preferred);
    let pinned = fixture.java("pinned-17", "17.0.12");

    for host in fixture.hosts() {
        fixture.pin(host, &pinned);
        let status = fixture.status(host).await;
        assert_eq!(
            status.fix(),
            Some(&ephervex_core::DiagnosticFix::UseAutomaticJava),
            "{host:?}: {:?}",
            status.findings
        );
    }
}

#[tokio::test]
async fn the_status_and_a_diagnosis_classify_a_host_s_java_the_same_way() {
    let fixture = Fixture::vanilla().await;
    let cases = [
        fixture.java("agree-21", "21.0.8"),
        fixture.java("agree-17", "17.0.12"),
        fake_java(&fixture.root().join("agree-arch"), "21.0.8", foreign_arch()),
        fixture.root().join("agree-missing/bin/java"),
    ];

    for host in fixture.hosts() {
        for java in &cases {
            fixture.pin(host, java);
            let status = fixture.status(host).await;
            let diagnosis = fixture
                .app
                .diagnose_host(
                    host,
                    DiagnosisRequest::default(),
                    &OperationReporter::silent("diagnosing"),
                    &CancellationToken::new(),
                )
                .await
                .unwrap();
            assert_eq!(
                format!("{:?}", status.check),
                format!("{:?}", diagnosis.java),
                "{host:?} {java:?}"
            );
            let java_findings: Vec<_> = diagnosis
                .findings
                .iter()
                .filter(|finding| {
                    matches!(
                        finding.kind,
                        FindingKind::JavaIncompatible { .. }
                            | FindingKind::JavaUnusable { .. }
                            | FindingKind::JavaUnavailable { .. }
                            | FindingKind::JavaCustomized { .. }
                    )
                })
                .cloned()
                .collect();
            assert_eq!(status.findings, java_findings, "{host:?} {java:?}");
        }
    }
}
