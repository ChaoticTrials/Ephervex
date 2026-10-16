//! Which Java a client or server would run with, what it expects, and whether
//! the two fit, as the per-host status and Diagnostics both report it.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use ephervex_core::{
    App, DiagnosisRequest, DiagnosticFix, FindingClass, FindingKind, FixOutcome, HostJavaStatus,
    InstalledHost, JavaCheck, JavaLayer, JavaMismatch, JavaSelectionMode, JavaStatusTone, Paths,
    ProposedChange, ResolvedJava,
};
use ephervex_download::{
    CacheLayout, ContentStore, FileExtension, MetadataCache, MetadataKey, OperationReporter,
};
use ephervex_java::{JavaOrigin, JavaRuntime, JavaSourceKind, JavaVersion};
use ephervex_minecraft::{VERSION_MANIFEST_CACHE_KEY, VERSION_MANIFEST_URL, VersionManifest};
use ephervex_model::{JavaOverride, JvmSettings, LoaderKind, LoaderSelection};
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

/// A launch, start or another process can install a managed runtime after
/// the inventory was scanned; the status and the picker follow the store.
#[tokio::test]
async fn a_managed_runtime_installed_after_the_scan_is_what_the_host_now_runs() {
    let mut fixture = Fixture::vanilla().await;
    ephervex_core::test_support::use_java_inventory(&fixture.app, Vec::new());
    fixture.detect(true, true);
    for host in fixture.hosts() {
        assert!(matches!(
            fixture.status(host).await.check,
            JavaCheck::DownloadsAtLaunch { required_major: 21 }
        ));
    }

    let installed = fixture.managed_java("21.0.8");

    for host in fixture.hosts() {
        let status = fixture.status(host).await;
        let runtime = status.runtime().expect("the installed runtime");
        assert_eq!(runtime.path, installed, "{host:?}");
        assert!(runtime.managed);
        assert_eq!(status.tone(), JavaStatusTone::Healthy);
        let choices = fixture.app.java_choices(host).await.unwrap();
        assert!(
            choices
                .runtimes
                .iter()
                .any(|choice| choice.runtime.path == installed && choice.is_compatible())
        );
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

/// Runtimes of several versions, origins and architectures, with real
/// executables so each can also be pinned.
fn mixed_inventory(fixture: &Fixture) -> Vec<JavaRuntime> {
    let mut foreign = scanned(
        &fake_java(
            &fixture.root().join("mixed-foreign"),
            "21.0.9",
            foreign_arch(),
        ),
        "21.0.9",
        "Eclipse Adoptium",
        JavaSourceKind::System,
    );
    foreign_arch().clone_into(&mut foreign.arch);
    vec![
        scanned(
            &fixture.java("mixed-old-21", "21.0.3"),
            "21.0.3",
            "Ubuntu",
            JavaSourceKind::System,
        ),
        scanned(
            &fixture.java("mixed-17", "17.0.12"),
            "17.0.12",
            "Ubuntu",
            JavaSourceKind::System,
        ),
        foreign,
        scanned(
            &fixture.java("mixed-system-21", "21.0.8"),
            "21.0.8",
            "Ubuntu",
            JavaSourceKind::System,
        ),
        scanned(
            &fixture.managed_java("21.0.8"),
            "21.0.8",
            "Eclipse Adoptium",
            JavaSourceKind::Managed,
        ),
    ]
}

#[tokio::test]
async fn the_picker_lists_compatible_runtimes_in_selection_order_then_the_refused_ones() {
    let mut fixture = Fixture::vanilla().await;
    ephervex_core::test_support::use_java_inventory(&fixture.app, mixed_inventory(&fixture));
    fixture.detect(true, false);

    for host in fixture.hosts() {
        let choices = fixture.app.java_choices(host).await.unwrap();
        let listed: Vec<(String, bool)> = choices
            .runtimes
            .iter()
            .map(|choice| (choice.runtime.version.clone(), choice.runtime.managed))
            .collect();
        assert_eq!(
            listed,
            [
                ("21.0.8".to_owned(), true),
                ("21.0.8".to_owned(), false),
                ("21.0.3".to_owned(), false),
                ("21.0.9".to_owned(), false),
                ("17.0.12".to_owned(), false),
            ],
            "{host:?}"
        );
        assert!(matches!(
            choices.runtimes[3].rejection,
            Some(JavaMismatch::Architecture { .. })
        ));
        assert!(matches!(
            choices.runtimes[4].rejection,
            Some(JavaMismatch::Major {
                required: 21,
                found: 17
            })
        ));
        let ephervex_core::AutomaticJava::Runtime(automatic) = &choices.automatic else {
            panic!("{:?}", choices.automatic);
        };
        assert_eq!(automatic, &choices.runtimes[0].runtime);
        assert_eq!(choices.mode, JavaSelectionMode::Automatic);
        assert!(choices.launcher.is_none());
        assert!(choices.runtimes.iter().all(|choice| !choice.current));
    }
}

#[tokio::test]
async fn every_picker_verdict_is_the_verdict_a_pin_of_that_runtime_gets() {
    let mut fixture = Fixture::vanilla().await;
    ephervex_core::test_support::use_java_inventory(&fixture.app, mixed_inventory(&fixture));
    fixture.detect(true, false);

    for host in fixture.hosts() {
        let choices = fixture.app.java_choices(host).await.unwrap();
        for choice in &choices.runtimes {
            fixture.pin(host, &choice.runtime.path);
            let status = fixture.status(host).await;
            let expected = if choice.is_compatible() {
                JavaStatusTone::Customized
            } else {
                JavaStatusTone::Problem
            };
            assert_eq!(
                status.tone(),
                expected,
                "{host:?} {:?}",
                choice.runtime.path
            );
            if let Some(rejection) = &choice.rejection {
                assert!(
                    matches!(
                        problem(&status),
                        FindingKind::JavaIncompatible { mismatch, .. } if mismatch == rejection
                    ),
                    "{:?}",
                    status.findings
                );
            }
            let marked = fixture.app.java_choices(host).await.unwrap();
            let current: Vec<&PathBuf> = marked
                .runtimes
                .iter()
                .filter(|entry| entry.current)
                .map(|entry| &entry.runtime.path)
                .collect();
            assert_eq!(current, [&choice.runtime.path]);
            assert_eq!(marked.mode, JavaSelectionMode::PinnedByHost);
        }
    }
}

#[tokio::test]
async fn a_pinned_runtime_the_scan_never_found_is_listed_as_the_current_choice() {
    let fixture = Fixture::vanilla().await;
    ephervex_core::test_support::use_java_inventory(&fixture.app, Vec::new());
    let custom = fixture.java("custom-jdk", "21.0.8");

    for host in fixture.hosts() {
        fixture.pin(host, &custom);
        let choices = fixture.app.java_choices(host).await.unwrap();
        let [choice] = choices.runtimes.as_slice() else {
            panic!("{:?}", choices.runtimes);
        };
        assert_eq!(choice.runtime.path, custom);
        assert!(choice.current);
        assert!(choice.is_compatible());
        assert_eq!(
            choices.automatic,
            ephervex_core::AutomaticJava::Unavailable(
                ephervex_core::JavaUnavailable::DetectionDisabled
            )
        );
    }
}

#[tokio::test]
async fn the_launcher_preference_is_its_own_entry_with_its_verdict() {
    let mut fixture = Fixture::vanilla().await;
    ephervex_core::test_support::use_java_inventory(&fixture.app, Vec::new());
    let preferred = fixture.java("preferred-17", "17.0.12");
    fixture.prefer(&preferred);

    for host in fixture.hosts() {
        let choices = fixture.app.java_choices(host).await.unwrap();
        assert_eq!(choices.mode, JavaSelectionMode::PinnedByLauncher);
        let launcher = choices.launcher.expect("the launcher entry");
        assert_eq!(launcher.path, preferred);
        assert_eq!(
            launcher.runtime.map(|runtime| runtime.version),
            Some("17.0.12".to_owned())
        );
        assert!(matches!(
            launcher.rejection,
            Some(JavaMismatch::Major { found: 17, .. })
        ));
    }
}

#[tokio::test]
async fn nothing_installed_with_downloads_on_resolves_automatic_to_a_download() {
    let mut fixture = Fixture::vanilla().await;
    ephervex_core::test_support::use_java_inventory(&fixture.app, Vec::new());
    fixture.detect(true, true);

    let choices = fixture.app.java_choices(fixture.client).await.unwrap();
    assert_eq!(
        choices.automatic,
        ephervex_core::AutomaticJava::DownloadsAtLaunch { major: 21 }
    );
}

#[tokio::test]
async fn a_custom_path_is_probed_and_judged_by_the_host_s_rules() {
    let fixture = Fixture::vanilla().await;
    let old = fixture.java("custom-17", "17.0.12");
    let fitting = fixture.java("custom-21", "21.0.8");
    let not_java = fixture.root().join("custom-text");
    std::fs::write(&not_java, b"text").unwrap();

    for host in fixture.hosts() {
        let refused = fixture.app.evaluate_java_path(host, &old).await.unwrap();
        assert!(matches!(
            refused.rejection,
            Some(JavaMismatch::Major { found: 17, .. })
        ));
        assert_eq!(refused.runtime.vendor, "Eclipse Adoptium");
        let accepted = fixture
            .app
            .evaluate_java_path(host, &fitting)
            .await
            .unwrap();
        assert!(accepted.is_compatible());
        assert!(!accepted.current);
        assert!(
            fixture
                .app
                .evaluate_java_path(host, &not_java)
                .await
                .is_err(),
            "a path that is not a runtime cannot be chosen"
        );
    }
}

#[tokio::test]
async fn the_pin_index_names_every_host_and_the_launcher_preference_pinning_a_runtime() {
    let mut fixture = Fixture::vanilla().await;
    let java = fixture.managed_java("21.0.8");
    let other = fixture.java("unpinned-jdk", "21.0.8");
    let link = fixture.root().join("java-link");
    std::os::unix::fs::symlink(&java, &link).unwrap();
    fixture.pin(fixture.client, &link);
    fixture.pin(fixture.server, &java);
    fixture.prefer(&java);

    let index = fixture.app.java_pin_index().unwrap();
    let pins = index.pins_for(&scanned(
        &java,
        "21.0.8",
        "Eclipse Adoptium",
        JavaSourceKind::Managed,
    ));
    let hosts: Vec<(InstalledHost, &str)> = pins
        .hosts
        .iter()
        .map(|pin| (pin.host, pin.name.as_str()))
        .collect();
    assert_eq!(
        hosts,
        [(fixture.client, "Java"), (fixture.server, "Java server")]
    );
    assert!(pins.launcher);
    assert!(
        index
            .pins_for(&scanned(&other, "21.0.8", "Ubuntu", JavaSourceKind::System))
            .is_empty()
    );
}

/// A managed Java 8 home whose `bin/java` and `jre/bin/java` are separate real
/// files, as a scan records it: one runtime with both paths.
fn split_java_8_home(fixture: &Fixture) -> (JavaRuntime, PathBuf) {
    let home = fixture
        .app
        .paths()
        .java_dir()
        .join("8/eclipse-jdk8u312-b07");
    let jdk = fake_java(&home, "1.8.0_312", jvm_arch());
    let jre = fake_java(&home.join("jre"), "1.8.0_312", jvm_arch());
    let mut runtime = scanned(&jdk, "1.8.0_312", "Temurin", JavaSourceKind::Managed);
    runtime.origins.push(JavaOrigin {
        kind: JavaSourceKind::Managed,
        path: jre.clone(),
    });
    (runtime, jre)
}

#[tokio::test]
async fn a_pin_through_the_jre_of_a_split_java_8_home_names_that_runtime() {
    let mut fixture = Fixture::new(
        "1.12.2",
        LoaderSelection {
            kind: LoaderKind::Forge,
            version: Some("14.23.5.2860".to_owned()),
        },
    )
    .await;
    let (runtime, jre) = split_java_8_home(&fixture);
    ephervex_core::test_support::use_java_inventory(&fixture.app, vec![runtime.clone()]);
    fixture.pin(fixture.client, &jre);
    fixture.prefer(&jre);

    let pins = fixture.app.java_pin_index().unwrap().pins_for(&runtime);
    assert_eq!(
        pins.hosts.iter().map(|pin| pin.host).collect::<Vec<_>>(),
        [fixture.client]
    );
    assert!(pins.launcher);

    let choices = fixture.app.java_choices(fixture.client).await.unwrap();
    let [choice] = choices.runtimes.as_slice() else {
        panic!("one runtime, listed once: {:?}", choices.runtimes);
    };
    assert!(choice.current);
    assert!(choice.is_compatible());
}

#[tokio::test]
async fn the_pin_index_reads_a_client_pin_its_index_row_has_not_caught_up_with() {
    let fixture = Fixture::vanilla().await;
    let java = fixture.java("committed-jdk", "21.0.8");
    let InstalledHost::Instance(id) = fixture.client else {
        unreachable!()
    };
    ephervex_core::test_support::commit_client_without_index(&fixture.app, id, |record| {
        record.java_override = Some(JavaOverride::new(&java));
    })
    .unwrap();

    let pins = fixture.app.java_pin_index().unwrap().pins_for(&scanned(
        &java,
        "21.0.8",
        "Ubuntu",
        JavaSourceKind::System,
    ));
    assert_eq!(
        pins.hosts.iter().map(|pin| pin.host).collect::<Vec<_>>(),
        [fixture.client]
    );
}

#[tokio::test]
async fn the_pin_index_reads_a_server_pin_its_index_row_has_not_caught_up_with() {
    let fixture = Fixture::vanilla().await;
    let java = fixture.java("committed-server-jdk", "21.0.8");
    let InstalledHost::Server(id) = fixture.server else {
        unreachable!()
    };
    let mut record = fixture.app.server(id).unwrap();
    record.java_override = Some(JavaOverride::new(&java));
    ephervex_storage::metadata::write_server_metadata(&record.root, &record).unwrap();

    let pins = fixture.app.java_pin_index().unwrap().pins_for(&scanned(
        &java,
        "21.0.8",
        "Ubuntu",
        JavaSourceKind::System,
    ));
    assert_eq!(
        pins.hosts.iter().map(|pin| pin.host).collect::<Vec<_>>(),
        [fixture.server]
    );
}

#[tokio::test]
async fn a_launcher_preference_that_is_not_a_runtime_is_never_compatible() {
    let mut fixture = Fixture::vanilla().await;
    ephervex_core::test_support::use_java_inventory(&fixture.app, Vec::new());
    let missing = fixture.root().join("gone/bin/java");
    fixture.prefer(&missing);

    let choices = fixture.app.java_choices(fixture.client).await.unwrap();
    let launcher = choices.launcher.expect("the launcher entry");
    assert_eq!(launcher.path, missing);
    assert!(launcher.runtime.is_none());
    assert!(!launcher.is_compatible());
    assert_eq!(
        fixture.status(fixture.client).await.tone(),
        JavaStatusTone::Problem,
        "the status calls the same preference a problem"
    );
}

#[tokio::test]
async fn changing_only_the_java_pin_keeps_the_jvm_settings_each_host_holds() {
    let fixture = Fixture::vanilla().await;
    let before = fixture.java("before-jdk", "21.0.3");
    let after = fixture.java("after-jdk", "21.0.8");
    let jvm = JvmSettings {
        jvm_args: vec!["-Dkept=1".to_owned()],
        ..JvmSettings::default()
    };

    for host in fixture.hosts() {
        pin_with_jvm(&fixture, host, &before, jvm.clone());
        match host {
            InstalledHost::Instance(id) => {
                fixture
                    .app
                    .set_client_java_override(id, Some(JavaOverride::new(&after)))
                    .unwrap();
            }
            InstalledHost::Server(id) => {
                fixture
                    .app
                    .set_server_java_override(id, Some(JavaOverride::new(&after)))
                    .unwrap();
            }
        }
        assert_eq!(
            runtime_settings(&fixture, host),
            (Some(after.clone()), Some(jvm.clone())),
            "{host:?}"
        );
    }
}

fn runtime_settings(
    fixture: &Fixture,
    host: InstalledHost,
) -> (Option<PathBuf>, Option<JvmSettings>) {
    match host {
        InstalledHost::Instance(id) => {
            let record = fixture.app.current_client(id).unwrap();
            (
                record.java_override.map(|java| java.path),
                record.jvm_override,
            )
        }
        InstalledHost::Server(id) => {
            let record = fixture.app.server(id).unwrap();
            (
                record.java_override.map(|java| java.path),
                record.jvm_override,
            )
        }
    }
}

fn pin_with_jvm(fixture: &Fixture, host: InstalledHost, path: &Path, jvm: JvmSettings) {
    let java = Some(JavaOverride::new(path));
    match host {
        InstalledHost::Instance(id) => {
            fixture
                .app
                .set_client_runtime_settings(id, java, Some(jvm))
                .unwrap();
        }
        InstalledHost::Server(id) => {
            fixture
                .app
                .set_server_runtime_settings(id, java, Some(jvm))
                .unwrap();
        }
    }
}

async fn apply(fixture: &Fixture, host: InstalledHost, fix: &DiagnosticFix) -> FixOutcome {
    fixture
        .app
        .apply_fix(
            host,
            fix,
            &OperationReporter::silent("fixing"),
            &CancellationToken::new(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn pinning_a_runtime_previews_both_sides_and_changes_only_the_java_path_once_applied() {
    let fixture = Fixture::vanilla().await;
    let old = fixture.java("before-17", "17.0.12");
    let new = fixture.java("after-21", "21.0.8");
    let jvm = JvmSettings {
        max_memory_mib: Some(3072),
        jvm_args: vec!["-Dkept=1".to_owned()],
        ..JvmSettings::default()
    };

    for host in fixture.hosts() {
        pin_with_jvm(&fixture, host, &old, jvm.clone());
        let fix = DiagnosticFix::PinJava(new.clone());

        let preview = fixture.app.preview_fix(host, &fix).await.unwrap();
        assert!(preview.blocked.is_none());
        let [ProposedChange::UseJava(change)] = preview.changes.as_slice() else {
            panic!("{:?}", preview.changes);
        };
        assert_eq!(change.expected.major(), 21);
        assert_eq!(change.current.mode, JavaSelectionMode::PinnedByHost);
        assert!(matches!(
            &change.current.resolved,
            ResolvedJava::Runtime { runtime, rejection: Some(JavaMismatch::Major { found: 17, .. }) }
                if runtime.path == old
        ));
        assert_eq!(change.proposed.mode, JavaSelectionMode::PinnedByHost);
        assert!(matches!(
            &change.proposed.resolved,
            ResolvedJava::Runtime { runtime, rejection: None }
                if runtime.path == new && runtime.vendor == "Eclipse Adoptium"
        ));
        assert_eq!(
            runtime_settings(&fixture, host),
            (Some(old.clone()), Some(jvm.clone())),
            "a preview changes nothing"
        );

        let outcome = apply(&fixture, host, &fix).await;
        assert!(outcome.is_complete(), "{host:?}: {outcome:?}");
        assert_eq!(
            runtime_settings(&fixture, host),
            (Some(new.clone()), Some(jvm.clone())),
            "only the Java path changes"
        );
        assert!(
            fixture
                .app
                .preview_fix(host, &fix)
                .await
                .unwrap()
                .is_empty(),
            "pinning the runtime already pinned changes nothing"
        );
    }
}

#[tokio::test]
async fn switching_to_automatic_previews_what_automatic_selection_resolves_to() {
    let mut fixture = Fixture::vanilla().await;
    let installed = fixture.java("installed-21", "21.0.8");
    ephervex_core::test_support::use_java_inventory(
        &fixture.app,
        vec![scanned(
            &installed,
            "21.0.8",
            "Ubuntu",
            JavaSourceKind::System,
        )],
    );
    fixture.detect(true, false);
    let pinned = fixture.java("pinned-21", "21.0.3");
    let jvm = JvmSettings {
        min_memory_mib: Some(1024),
        ..JvmSettings::default()
    };

    for host in fixture.hosts() {
        pin_with_jvm(&fixture, host, &pinned, jvm.clone());
        let status = fixture.status(host).await;
        assert_eq!(status.tone(), JavaStatusTone::Customized);
        assert_eq!(status.fix(), Some(&DiagnosticFix::UseAutomaticJava));

        let preview = fixture
            .app
            .preview_fix(host, &DiagnosticFix::UseAutomaticJava)
            .await
            .unwrap();
        let [ProposedChange::UseJava(change)] = preview.changes.as_slice() else {
            panic!("{:?}", preview.changes);
        };
        assert_eq!(change.proposed.mode, JavaSelectionMode::Automatic);
        assert!(matches!(
            &change.proposed.resolved,
            ResolvedJava::Runtime { runtime, rejection: None } if runtime.path == installed
        ));

        assert!(
            apply(&fixture, host, &DiagnosticFix::UseAutomaticJava)
                .await
                .is_complete()
        );
        assert_eq!(runtime_settings(&fixture, host), (None, Some(jvm.clone())));
        assert_eq!(fixture.status(host).await.tone(), JavaStatusTone::Healthy);
    }
}

#[tokio::test]
async fn a_java_change_waits_for_a_running_server_to_stop() {
    use rustix::fs::{FlockOperation, flock};

    let fixture = Fixture::vanilla().await;
    let before = fixture.java("server-before", "21.0.3");
    let after = fixture.java("server-after", "21.0.8");
    fixture.pin(fixture.server, &before);
    let InstalledHost::Server(id) = fixture.server else {
        unreachable!()
    };
    let lock_path = fixture
        .app
        .server(id)
        .unwrap()
        .root
        .join(".launcher")
        .join(ephervex_server::RUNTIME_LOCK_FILE);
    std::fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
    let lock = std::fs::File::create(&lock_path).unwrap();
    flock(&lock, FlockOperation::LockExclusive).unwrap();

    let fix = DiagnosticFix::PinJava(after);
    let preview = fixture.app.preview_fix(fixture.server, &fix).await.unwrap();
    assert!(preview.blocked.is_some(), "{preview:?}");
    let refused = fixture
        .app
        .apply_fix(
            fixture.server,
            &fix,
            &OperationReporter::silent("fixing"),
            &CancellationToken::new(),
        )
        .await;
    assert!(refused.is_err());
    assert_eq!(runtime_settings(&fixture, fixture.server).0, Some(before));
}

#[tokio::test]
async fn a_server_s_status_reads_the_pin_its_index_row_has_not_caught_up_with() {
    let fixture = Fixture::vanilla().await;
    let java = fixture.java("committed-status-jdk", "21.0.8");
    let InstalledHost::Server(id) = fixture.server else {
        unreachable!()
    };
    let mut record = fixture.app.server(id).unwrap();
    record.java_override = Some(JavaOverride::new(&java));
    ephervex_storage::metadata::write_server_metadata(&record.root, &record).unwrap();

    let status = fixture.status(fixture.server).await;

    assert_eq!(status.mode, JavaSelectionMode::PinnedByHost);
    assert_eq!(status.pinned_path.as_deref(), Some(java.as_path()));
    assert_eq!(
        fixture
            .app
            .current_server(id)
            .unwrap()
            .java_override
            .map(|java| java.path),
        Some(java)
    );
}

#[tokio::test]
async fn saving_only_jvm_settings_keeps_the_java_pin_another_change_made() {
    let fixture = Fixture::vanilla().await;
    let pinned = fixture.java("kept-jdk", "21.0.8");
    let InstalledHost::Instance(id) = fixture.client else {
        unreachable!()
    };
    fixture.pin(fixture.client, &pinned);
    let jvm = JvmSettings {
        max_memory_mib: Some(2048),
        ..JvmSettings::default()
    };

    fixture
        .app
        .set_client_jvm_settings(id, Some(jvm.clone()))
        .unwrap();

    let record = fixture.app.current_client(id).unwrap();
    assert_eq!(record.java_override, Some(JavaOverride::new(&pinned)));
    assert_eq!(record.jvm_override, Some(jvm));
}
