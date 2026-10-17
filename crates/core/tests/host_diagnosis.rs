//! Diagnostics: what a host is configured with, what is wrong with it, and the
//! fixes it previews before anything changes.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use ephervex_core::{
    App, CoreError, DiagnosisRequest, DiagnosticFix, FindingClass, FindingKind, HostDiagnosis,
    InstalledHost, JavaLayer, JavaMismatch, JavaSelectionMode, Paths, ProposedChange, ResolvedJava,
    UnmetDependency,
};
use ephervex_download::{
    CacheLayout, ContentStore, FileExtension, MetadataCache, MetadataKey, OperationReporter,
};
use ephervex_minecraft::{VERSION_MANIFEST_CACHE_KEY, VERSION_MANIFEST_URL, VersionManifest};
use ephervex_model::{InstanceId, JavaOverride, JvmSettings, LoaderSelection};
use ephervex_storage::{LaunchLock, NewClient, Registry};
use serde_json::json;
use tokio_util::sync::CancellationToken;

struct Fixture {
    _directory: tempfile::TempDir,
    paths: Paths,
    root: PathBuf,
    id: InstanceId,
}

impl Fixture {
    /// A vanilla 1.21.1 client whose recipe declares each jar and the
    /// projects it requires. Java is never looked for on this machine: only a
    /// runtime a test pins is ever probed.
    async fn with_mods(declared: &[(&str, &[&str])]) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(directory.path());
        let registry = Registry::open(paths.clone()).unwrap();
        let client = registry
            .create_client(NewClient::new(
                "Diagnosis",
                "1.21.1",
                LoaderSelection::vanilla(),
            ))
            .unwrap();
        registry.publish_client(&client).unwrap();
        let fixture = Self {
            _directory: directory,
            paths,
            root: client.root,
            id: client.id,
        };
        let files = declared
            .iter()
            .map(|&(name, requires)| {
                fixture.write(&format!("minecraft/mods/{name}.jar"), name.as_bytes());
                json!({
                    "logical_path": format!("mods/{name}.jar"), "hashes": [],
                    "downloads": [], "size": name.len(), "client": "required", "server": "required",
                    "origin": "user_selected",
                    "identity": {"provider": "modrinth", "project_id": name, "version_id": format!("{name}1")},
                    "relations": requires.iter().map(|name| json!({"kind":"required","target":{"project":{"project_id":name}}})).collect::<Vec<_>>(),
                })
            })
            .collect::<Vec<_>>();
        fixture.write_recipe(&files);
        seed_minecraft(fixture.app().paths().cache_dir()).await;
        let mut app = fixture.app();
        let mut settings = app.java_settings();
        settings.auto_detect = false;
        settings.auto_download = false;
        app.set_java_settings(&settings).unwrap();
        fixture
    }

    fn app(&self) -> App {
        App::open_at(self.paths.clone()).unwrap()
    }

    fn host(&self) -> InstalledHost {
        InstalledHost::Instance(self.id)
    }

    fn write(&self, path: &str, bytes: &[u8]) {
        let path = self.root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    fn write_recipe(&self, files: &[serde_json::Value]) {
        self.write(
            ".launcher/source/recipe.json",
            &serde_json::to_vec(&json!({
                "recipe_version":1, "source_kind":"manual", "original_name":"Diagnosis",
                "minecraft_version":"1.21.1","loader":{"kind":"vanilla","version":null},
                "files":files, "override_layers":[], "provider_metadata":{"kind":"manual"},
            }))
            .unwrap(),
        );
    }

    fn disable(&self, name: &str) {
        let mods = self.root.join("minecraft/mods");
        std::fs::rename(
            mods.join(format!("{name}.jar")),
            mods.join(format!("{name}.jar.disabled")),
        )
        .unwrap();
    }

    fn enabled(&self, name: &str) -> bool {
        self.root
            .join(format!("minecraft/mods/{name}.jar"))
            .is_file()
    }

    /// Pins this host to a fake runtime reporting `version`.
    fn pin_java(&self, version: &str, jvm: Option<JvmSettings>) -> PathBuf {
        self.pin_java_built_for(version, jvm_arch(), jvm)
    }

    /// Pins this host to a fake runtime reporting `version` and `arch`.
    fn pin_java_built_for(&self, version: &str, arch: &str, jvm: Option<JvmSettings>) -> PathBuf {
        let java = fake_java_for(&self.root.join(format!("jdk-{version}")), version, arch);
        self.app()
            .set_client_runtime_settings(self.id, Some(JavaOverride::new(&java)), jvm)
            .unwrap();
        java
    }

    async fn diagnose(&self, integrity: bool) -> HostDiagnosis {
        self.app()
            .diagnose_host(
                self.host(),
                DiagnosisRequest {
                    integrity,
                    ask_catalogs: false,
                },
                &OperationReporter::silent("diagnosing"),
                &CancellationToken::new(),
            )
            .await
            .unwrap()
    }
}

/// An executable that answers the property probe as a JVM of `version` would.
fn fake_java(home: &Path, version: &str) -> PathBuf {
    fake_java_for(home, version, jvm_arch())
}

/// Like [`fake_java`], reporting `arch` as its `os.arch`.
fn fake_java_for(home: &Path, version: &str, arch: &str) -> PathBuf {
    let executable = home.join("bin/java");
    std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
    std::fs::write(
        &executable,
        format!(
            "#!/bin/sh\ncat >&2 <<'EOF'\nProperty settings:\n    java.home = {home}\n    \
             java.runtime.version = {version}+7\n    java.vendor = Eclipse Adoptium\n    \
             java.version = {version}\n    os.arch = {arch}\nEOF\n",
            home = home.display(),
            arch = arch,
        ),
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    executable
}

const fn jvm_arch() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "amd64"
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

fn kinds(diagnosis: &HostDiagnosis, class: FindingClass) -> Vec<&FindingKind> {
    diagnosis
        .findings
        .iter()
        .filter(|finding| finding.class() == class)
        .map(|finding| &finding.kind)
        .collect()
}

#[tokio::test]
async fn a_pinned_java_that_satisfies_the_release_is_a_customization_not_a_problem() {
    let fixture = Fixture::with_mods(&[]).await;
    fixture.pin_java("21.0.8", None);

    let diagnosis = fixture.diagnose(false).await;

    assert!(diagnosis.is_healthy(), "{:?}", diagnosis.findings);
    let customized = kinds(&diagnosis, FindingClass::Customized);
    assert!(
        matches!(
            customized.as_slice(),
            [FindingKind::JavaCustomized {
                layer: JavaLayer::Host,
                required_major: 21,
                ..
            }]
        ),
        "{customized:?}"
    );
    assert_eq!(
        diagnosis.findings[0].fix, None,
        "with detection off there is nothing better to recommend"
    );
}

#[tokio::test]
async fn a_pinned_java_of_the_wrong_major_is_a_problem() {
    let fixture = Fixture::with_mods(&[]).await;
    fixture.pin_java("17.0.12", None);

    let diagnosis = fixture.diagnose(false).await;

    assert!(!diagnosis.is_healthy());
    assert!(
        matches!(
            kinds(&diagnosis, FindingClass::Problem).as_slice(),
            [FindingKind::JavaIncompatible {
                layer: JavaLayer::Host,
                required_major: 21,
                mismatch: JavaMismatch::Major {
                    required: 21,
                    found: 17
                },
                ..
            }]
        ),
        "{:?}",
        diagnosis.findings
    );
    assert!(
        diagnosis
            .technical_report()
            .contains("needs Java 21, found 17")
    );
}

#[tokio::test]
async fn a_pinned_java_built_for_another_architecture_is_a_problem() {
    let fixture = Fixture::with_mods(&[]).await;
    let foreign = if cfg!(target_arch = "aarch64") {
        "amd64"
    } else {
        "aarch64"
    };
    fixture.pin_java_built_for("21.0.8", foreign, None);

    let diagnosis = fixture.diagnose(false).await;

    assert!(!diagnosis.is_healthy());
    assert!(
        matches!(
            kinds(&diagnosis, FindingClass::Problem).as_slice(),
            [FindingKind::JavaIncompatible {
                layer: JavaLayer::Host,
                required_major: 21,
                mismatch: JavaMismatch::Architecture { .. },
                ..
            }]
        ),
        "{:?}",
        diagnosis.findings
    );
}

#[tokio::test]
async fn using_the_recommended_java_previews_both_runtimes_and_changes_only_this_host() {
    let fixture = Fixture::with_mods(&[]).await;
    let mut app = fixture.app();
    let launcher_java = fake_java(&fixture.root.join("launcher-jdk"), "21.0.8");
    let mut settings = app.java_settings();
    settings.preferred_path = Some(launcher_java.clone());
    app.set_java_settings(&settings).unwrap();
    let jvm = JvmSettings {
        max_memory_mib: Some(4096),
        ..JvmSettings::default()
    };
    let pinned = fixture.pin_java("17.0.12", Some(jvm.clone()));

    let diagnosis = fixture.diagnose(false).await;
    let fix = diagnosis.findings[0].fix.clone();
    assert_eq!(fix, Some(DiagnosticFix::UseAutomaticJava));

    let preview = app
        .preview_fix(fixture.host(), fix.as_ref().unwrap())
        .await
        .unwrap();
    let [ProposedChange::UseJava(change)] = preview.changes.as_slice() else {
        panic!("{:?}", preview.changes);
    };
    assert_eq!(change.expected.major(), 21);
    assert_eq!(change.current.mode, JavaSelectionMode::PinnedByHost);
    let ResolvedJava::Runtime {
        runtime: current,
        rejection: Some(JavaMismatch::Major { found: 17, .. }),
    } = &change.current.resolved
    else {
        panic!("{:?}", change.current);
    };
    assert_eq!(current.path, pinned);
    assert_eq!(current.version, "17.0.12");
    assert_eq!(change.proposed.mode, JavaSelectionMode::PinnedByLauncher);
    let ResolvedJava::Runtime {
        runtime: recommended,
        rejection: None,
    } = &change.proposed.resolved
    else {
        panic!("{:?}", change.proposed);
    };
    assert_eq!(recommended.version, "21.0.8");
    assert!(preview.blocked.is_none());
    assert!(
        app.client(fixture.id).unwrap().java_override.is_some(),
        "a preview changes nothing"
    );

    let outcome = app
        .apply_fix(
            fixture.host(),
            fix.as_ref().unwrap(),
            &OperationReporter::silent("fixing"),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(outcome.is_complete());
    let record = app.client(fixture.id).unwrap();
    assert_eq!(record.java_override, None);
    assert_eq!(
        record.jvm_override,
        Some(jvm),
        "only the Java selection changes"
    );
}

#[tokio::test]
async fn using_the_recommended_java_reports_a_metadata_directory_that_could_not_sync() {
    let fixture = Fixture::with_mods(&[]).await;
    let mut app = fixture.app();
    let launcher_java = fake_java(&fixture.root.join("launcher-jdk"), "21.0.8");
    let mut settings = app.java_settings();
    settings.preferred_path = Some(launcher_java);
    app.set_java_settings(&settings).unwrap();
    fixture.pin_java("17.0.12", None);
    let diagnosis = fixture.diagnose(false).await;
    let fix = diagnosis.findings[0].fix.clone().unwrap();
    let metadata = app.client(fixture.id).unwrap().root.join(".launcher");
    std::fs::set_permissions(&metadata, std::fs::Permissions::from_mode(0o300)).unwrap();
    let unsyncable = std::fs::File::open(&metadata).is_err();

    let outcome = if unsyncable {
        Some(
            app.apply_fix(
                fixture.host(),
                &fix,
                &OperationReporter::silent("fixing"),
                &CancellationToken::new(),
            )
            .await,
        )
    } else {
        None
    };
    std::fs::set_permissions(&metadata, std::fs::Permissions::from_mode(0o700)).unwrap();
    let Some(outcome) = outcome else {
        return;
    };

    let outcome = outcome.unwrap();
    assert!(outcome.is_complete());
    assert!(matches!(
        outcome.client_warnings.as_slice(),
        [ephervex_core::ClientCommitWarning::MetadataNotDurable { .. }]
    ));
    assert_eq!(app.client(fixture.id).unwrap().java_override, None);
}

#[tokio::test]
async fn custom_jvm_arguments_are_customized_and_the_report_leaves_out_their_values() {
    let fixture = Fixture::with_mods(&[]).await;
    fixture.pin_java(
        "21.0.8",
        Some(JvmSettings {
            jvm_args: vec!["-Dsecret=hunter2".to_owned(), "-Xss4m".to_owned()],
            ..JvmSettings::default()
        }),
    );

    let diagnosis = fixture.diagnose(false).await;

    assert!(diagnosis.is_healthy());
    assert!(
        kinds(&diagnosis, FindingClass::Customized)
            .iter()
            .any(|kind| matches!(
                kind,
                FindingKind::JvmArgumentsCustomized {
                    layer: JavaLayer::Host,
                    count: 2
                }
            ))
    );
    let report = diagnosis.technical_report();
    assert!(!report.contains("hunter2"), "{report}");
    assert!(report.contains("Result: no problems found"), "{report}");
}

#[tokio::test]
async fn a_turned_off_requirement_is_enabled_only_after_its_preview_is_applied() {
    let fixture = Fixture::with_mods(&[("create", &["flywheel"]), ("flywheel", &[])]).await;
    fixture.pin_java("21.0.8", None);
    fixture.disable("flywheel");
    let app = fixture.app();

    let diagnosis = fixture.diagnose(false).await;
    let problems = kinds(&diagnosis, FindingClass::Problem);
    let [FindingKind::DependencyDisabled(requirement)] = problems.as_slice() else {
        panic!("{problems:?}");
    };
    assert!(matches!(
        requirement.dependency,
        UnmetDependency::Disabled(_)
    ));
    let fix = diagnosis.findings[0].fix.clone().unwrap();
    assert_eq!(
        fix,
        DiagnosticFix::EnableMods(vec![PathBuf::from("mods/flywheel.jar")])
    );

    let preview = app.preview_fix(fixture.host(), &fix).await.unwrap();
    assert_eq!(
        preview.changes,
        [ProposedChange::Enable {
            name: "flywheel.jar".to_owned(),
            logical_path: PathBuf::from("mods/flywheel.jar"),
        }]
    );
    assert!(!fixture.enabled("flywheel"), "a preview changes nothing");

    let outcome = app
        .apply_fix(
            fixture.host(),
            &fix,
            &OperationReporter::silent("fixing"),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(outcome.applied, ["flywheel.jar"]);
    assert!(fixture.enabled("flywheel"));
    assert!(fixture.diagnose(false).await.is_healthy());
}

#[tokio::test]
async fn a_canceled_enable_lists_the_mods_it_never_reached_instead_of_reporting_success() {
    let fixture = Fixture::with_mods(&[("create", &["flywheel"]), ("flywheel", &[])]).await;
    fixture.disable("flywheel");
    fixture.disable("create");
    let app = fixture.app();
    let fix = DiagnosticFix::EnableMods(vec![
        PathBuf::from("mods/flywheel.jar"),
        PathBuf::from("mods/create.jar"),
    ]);
    let cancel = CancellationToken::new();
    cancel.cancel();

    let outcome = app
        .apply_fix(
            fixture.host(),
            &fix,
            &OperationReporter::silent("fixing"),
            &cancel,
        )
        .await
        .unwrap();

    assert!(outcome.was_canceled());
    assert!(!outcome.is_complete(), "{outcome:?}");
    assert!(outcome.applied.is_empty());
    assert_eq!(outcome.skipped, ["flywheel.jar", "create.jar"]);
    assert!(!fixture.enabled("flywheel"));
    assert!(!fixture.enabled("create"));
}

#[tokio::test]
async fn a_fix_is_held_back_while_the_host_runs() {
    let fixture = Fixture::with_mods(&[("create", &["flywheel"]), ("flywheel", &[])]).await;
    fixture.disable("flywheel");
    let app = fixture.app();
    let fix = DiagnosticFix::EnableMods(vec![PathBuf::from("mods/flywheel.jar")]);
    let launch = LaunchLock::acquire(&fixture.root).unwrap();

    let preview = app.preview_fix(fixture.host(), &fix).await.unwrap();
    assert!(preview.blocked.is_some());
    let refused = app
        .apply_fix(
            fixture.host(),
            &fix,
            &OperationReporter::silent("fixing"),
            &CancellationToken::new(),
        )
        .await;
    assert!(matches!(refused, Err(CoreError::ChangeRefused { .. })));
    assert!(!fixture.enabled("flywheel"));
    drop(launch);
}

#[tokio::test]
async fn a_crash_is_a_problem_only_when_it_came_after_the_last_start() {
    let fixture = Fixture::with_mods(&[]).await;
    fixture.pin_java("21.0.8", None);
    fixture.write(
        "minecraft/crash-reports/crash-2026-09-27_12.00.00-client.txt",
        b"---- Minecraft Crash Report ----\nDescription: Ticking\n\njava.lang.IllegalStateException: boom\n",
    );

    let diagnosis = fixture.diagnose(false).await;
    assert!(
        kinds(&diagnosis, FindingClass::Problem)
            .iter()
            .any(|kind| matches!(kind, FindingKind::RecentCrash { .. })),
        "never started from here, so the report is the newest evidence"
    );

    let registry = Registry::open(fixture.paths.clone()).unwrap();
    let mut record = registry.client(fixture.id).unwrap();
    record.last_launched_at = Some(chrono::Utc::now() + chrono::Duration::hours(1));
    registry.update_client(&mut record).unwrap();

    let diagnosis = fixture.diagnose(false).await;
    assert!(diagnosis.recent_crash.is_none());
    assert!(diagnosis.is_healthy(), "{:?}", diagnosis.findings);
}

/// A crash report left from an earlier run does not describe the one since, so
/// nothing that reviews the last run's crash offers it again.
#[tokio::test]
async fn a_crash_report_from_before_the_last_start_is_not_reviewed() {
    let fixture = Fixture::with_mods(&[]).await;
    fixture.write(
        "minecraft/crash-reports/crash-2026-09-27_12.00.00-client.txt",
        b"---- Minecraft Crash Report ----\nDescription: Ticking\n\njava.lang.IllegalStateException: boom\n",
    );
    let app = fixture.app();
    assert!(app.crash_review(fixture.host()).unwrap().is_some());

    let registry = Registry::open(fixture.paths.clone()).unwrap();
    let mut record = registry.client(fixture.id).unwrap();
    record.last_launched_at = Some(chrono::Utc::now() + chrono::Duration::hours(1));
    registry.update_client(&mut record).unwrap();

    assert!(app.crash_review(fixture.host()).unwrap().is_none());
}

/// A module that runs on the loader's class path but is no file in `mods/`
/// (here bundled inside another mod) is evidence, not a mod to disable.
#[tokio::test]
async fn a_crash_suspect_whose_jar_is_not_in_the_hosts_mods_is_not_named() {
    let fixture = Fixture::with_mods(&[("create", &[])]).await;
    fixture.write(
        "minecraft/crash-reports/crash-2026-09-27_12.00.00-client.txt",
        b"---- Minecraft Crash Report ----\nDescription: Ticking\n\n\
java.lang.IllegalStateException: boom\n\
\tat TRANSFORMER/create@6.0/com.simibubi.create.Create.tick(Create.java:1) ~[create.jar:6.0]\n\
\tat TRANSFORMER/flywheel@1.0.0/dev.engine_room.flywheel.Flywheel.tick(Flywheel.java:2) ~[flywheel-1.0.0.jar:1.0.0]\n\
\tat TRANSFORMER/minecraft@1.21.1/net.minecraft.world.level.Level.tick(Level.java:3) ~[client.jar:?]\n\n\
-- System Details --\n\
Details:\n\
\tMod List: \n\
\t\tclient.jar                |Minecraft |minecraft |1.21.1 |COMMON_SET|Manifest: NOSIGNATURE\n\
\t\tcreate.jar                |Create    |create    |6.0    |DONE      |Manifest: NOSIGNATURE\n\
\t\tflywheel-1.0.0.jar        |Flywheel  |flywheel  |1.0.0  |DONE      |Manifest: NOSIGNATURE\n",
    );

    let review = fixture
        .app()
        .crash_review(fixture.host())
        .unwrap()
        .expect("the crash came after the last start");

    let named: Vec<&str> = review
        .analysis
        .suspects
        .iter()
        .map(|suspect| suspect.mod_id.as_str())
        .collect();
    assert_eq!(named, ["create"]);
    assert_eq!(review.mods.len(), 1);
    assert!(review.mods[0].is_actionable());
}

/// An early loading failure writes no `Mod List:` table. The report's own
/// `Mod file:` line is what maps the named mod to its installed row.
#[tokio::test]
async fn a_loading_failure_report_without_a_mod_list_maps_the_named_mod_to_its_installed_jar() {
    let fixture = Fixture::with_mods(&[("jea", &[]), ("create", &[])]).await;
    fixture.write(
        "minecraft/crash-reports/crash-2026-10-14_19.20.01-client.txt",
        b"---- Minecraft Crash Report ----
Description: Mod loading error has occurred

-- Mod loading issue for: jea --
Details:
\tMod file: /srv/pack/server/mods/jea.jar
\tFailure message: Mod jea requires jei 19.25.0.322 or above
",
    );

    let review = fixture
        .app()
        .crash_review(fixture.host())
        .unwrap()
        .expect("the crash came after the last start");

    let [entry] = review.mods.as_slice() else {
        panic!("{:?}", review.mods);
    };
    assert_eq!(entry.suspect.mod_id, "jea");
    assert_eq!(
        entry.logical_path.as_deref(),
        Some(Path::new("mods/jea.jar"))
    );
    assert!(entry.is_actionable());
}

/// Forge 1.20.1 writes `-- MOD <id> --` with a `Mod File:` line.
#[tokio::test]
async fn a_forge_loading_failure_report_without_a_mod_list_maps_the_named_mod_to_its_installed_jar()
{
    let fixture = Fixture::with_mods(&[("oculus", &[])]).await;
    fixture.write(
        "minecraft/crash-reports/crash-2026-10-14_19.22.40-server.txt",
        b"---- Minecraft Crash Report ----
Description: Exception in server tick loop

-- MOD oculus --
Details:
\tMod File: C:\\servers\\pack\\mods\\oculus.jar
\tFailure message: Oculus (oculus) has failed to load correctly
",
    );

    let review = fixture
        .app()
        .crash_review(fixture.host())
        .unwrap()
        .expect("the crash came after the last start");

    let [entry] = review.mods.as_slice() else {
        panic!("{:?}", review.mods);
    };
    assert_eq!(entry.suspect.mod_id, "oculus");
    assert_eq!(
        entry.logical_path.as_deref(),
        Some(Path::new("mods/oculus.jar"))
    );
    assert!(entry.is_actionable());
}

/// The named mod is only missing from the mods list when its jar really is.
#[tokio::test]
async fn a_loading_failure_naming_a_jar_the_host_does_not_have_maps_to_no_row() {
    let fixture = Fixture::with_mods(&[("create", &[])]).await;
    fixture.write(
        "minecraft/crash-reports/crash-2026-10-14_19.20.01-client.txt",
        b"---- Minecraft Crash Report ----
Description: Mod loading error has occurred

-- Mod loading issue for: jea --
Details:
\tMod file: /srv/pack/server/mods/jea.jar
",
    );

    let review = fixture
        .app()
        .crash_review(fixture.host())
        .unwrap()
        .expect("the crash came after the last start");

    let [entry] = review.mods.as_slice() else {
        panic!("{:?}", review.mods);
    };
    assert_eq!(entry.suspect.mod_id, "jea");
    assert_eq!(entry.logical_path, None);
    assert!(!entry.is_actionable());
}

#[tokio::test]
async fn the_loader_s_refusal_in_the_last_log_is_a_problem() {
    let fixture = Fixture::with_mods(&[]).await;
    fixture.pin_java("21.0.8", None);
    fixture.write(
        "minecraft/logs/latest.log",
        b"[12:00:01] [main/ERROR] [net.neoforged.fml.loading.ModSorter/LOADING]: Missing or unsupported mandatory dependencies:\n\
          \tMod ID: 'bar', Requested by: 'foo', Expected range: '[3.0,)', Actual version: '2.4'\n",
    );

    let diagnosis = fixture.diagnose(false).await;

    let problems = kinds(&diagnosis, FindingClass::Problem);
    let [FindingKind::LoaderReportedDependency(reported)] = problems.as_slice() else {
        panic!("{problems:?}");
    };
    assert_eq!(reported.accepted_range.as_deref(), Some("[3.0,)"));
    assert_eq!(reported.requests[0].requested_by, "foo");
    assert_eq!(reported.actual.as_deref(), Some("2.4"));
}

const LIBX_REFUSED: &[u8] = b"[12:00:01] [main/ERROR] [net.minecraftforge.fml.loading.ModSorter/LOADING]: Missing or unsupported mandatory dependencies:\n\
    \tMod ID: 'libx', Requested by: 'datatrader', Expected range: '[1.19.3-4.3.0,)', Actual version: '[MISSING]'\n\
    \tMod ID: 'libx', Requested by: 'utilitix', Expected range: '[1.20.1-5.0.1,)', Actual version: '[MISSING]'\n\
    \tMod ID: 'libx', Requested by: 'packessentials', Expected range: '[1.20.1-5.0.14,)', Actual version: '[MISSING]'\n";

/// The loader's refusal and a recorded relation are two observations. A
/// loader mod id is not a catalog project, so a catalog requirement whose name
/// reads like the mod id stays its own finding, and so does one named nothing
/// like it. The loader's lines for one mod id are still one finding.
#[tokio::test]
async fn a_loader_refusal_is_never_merged_with_a_catalog_requirement_by_name() {
    for required in ["libx", "Library of Things"] {
        let fixture = Fixture::with_mods(&[("datatrader", &[required])]).await;
        fixture.pin_java("21.0.8", None);
        fixture.write("minecraft/logs/latest.log", LIBX_REFUSED);

        let diagnosis = fixture.diagnose(false).await;

        let problems = kinds(&diagnosis, FindingClass::Problem);
        let [
            FindingKind::LoaderReportedDependency(loader),
            FindingKind::DependencyMissing(requirement),
        ] = problems.as_slice()
        else {
            panic!("{required}: {problems:?}");
        };
        let UnmetDependency::Missing { name, .. } = &requirement.dependency else {
            panic!("{requirement:?}");
        };
        assert_eq!(name, required);
        assert_eq!(loader.mod_id, "libx");
        assert_eq!(loader.accepted_range.as_deref(), Some("[1.20.1-5.0.14,)"));
        assert_eq!(
            loader.ranges_asked_for(),
            [(
                "[1.20.1-5.0.14,)".to_owned(),
                vec![
                    "datatrader".to_owned(),
                    "utilitix".to_owned(),
                    "packessentials".to_owned()
                ]
            )]
        );
    }
}

/// A catalog finding may offer its mod back; a loader finding never does,
/// because nothing proves which catalog project its mod id is.
#[tokio::test]
async fn only_the_catalog_finding_offers_its_mod_back() {
    let fixture = Fixture::with_mods(&[("datatrader", &["libx"])]).await;
    fixture.pin_java("21.0.8", None);
    fixture.write_recipe(&[
        json!({
            "logical_path": "mods/datatrader.jar", "hashes": [], "downloads": [], "size": 10,
            "client": "required", "server": "required", "origin": "pack_declared",
            "identity": {"provider": "modrinth", "project_id": "datatrader", "version_id": "datatrader1"},
            "relations": [{"kind":"required","target":{"project":{"project_id":"libx"}}}],
        }),
        json!({
            "logical_path": "mods/LibX-1.20.1-5.0.14.jar",
            "hashes": [{"algorithm": "sha1", "digest": "da39a3ee5e6b4b0d3255bfef95601890afd80709"}],
            "downloads": ["https://cdn.modrinth.com/data/qEH6GYul/versions/YHTMDq85/LibX-1.20.1-5.0.14.jar"],
            "size": 1, "client": "required", "server": "required", "origin": "pack_declared",
            "identity": {"provider": "modrinth", "project_id": "libx", "version_id": "YHTMDq85"},
        }),
    ]);
    fixture.write("minecraft/logs/latest.log", LIBX_REFUSED);

    let diagnosis = fixture.diagnose(false).await;

    let fix_of = |test: fn(&FindingKind) -> bool| {
        diagnosis
            .findings
            .iter()
            .find(|finding| test(&finding.kind))
            .map(|finding| finding.fix.clone())
    };
    assert_eq!(
        fix_of(|kind| matches!(kind, FindingKind::LoaderReportedDependency(_))),
        Some(None)
    );
    assert_eq!(
        fix_of(|kind| matches!(kind, FindingKind::DependencyMissing(_))),
        Some(Some(DiagnosticFix::RestoreFiles(vec![PathBuf::from(
            "mods/LibX-1.20.1-5.0.14.jar"
        )])))
    );
}

/// A required mod the host records, whose jar alone is gone, is restored at
/// its recorded release: the quick pass offers that for this one file.
#[tokio::test]
async fn a_recorded_dependency_whose_jar_is_gone_is_offered_back() {
    let fixture = Fixture::with_mods(&[("datatrader", &["libx"])]).await;
    fixture.pin_java("21.0.8", None);
    fixture.write_recipe(&[
        json!({
            "logical_path": "mods/datatrader.jar", "hashes": [], "downloads": [], "size": 10,
            "client": "required", "server": "required", "origin": "pack_declared",
            "identity": {"provider": "modrinth", "project_id": "datatrader", "version_id": "datatrader1"},
            "relations": [{"kind":"required","target":{"project":{"project_id":"libx"}}}],
        }),
        json!({
            "logical_path": "mods/LibX-1.20.1-5.0.14.jar",
            "hashes": [{"algorithm": "sha1", "digest": "da39a3ee5e6b4b0d3255bfef95601890afd80709"}],
            "downloads": ["https://cdn.modrinth.com/data/qEH6GYul/versions/YHTMDq85/LibX-1.20.1-5.0.14.jar"],
            "size": 1, "client": "required", "server": "required", "origin": "pack_declared",
            "identity": {"provider": "modrinth", "project_id": "libx", "version_id": "YHTMDq85"},
        }),
    ]);

    let diagnosis = fixture.diagnose(false).await;

    let restore = diagnosis
        .findings
        .iter()
        .find_map(|finding| match &finding.kind {
            FindingKind::DependencyMissing { .. } => Some(finding.fix.clone()),
            _ => None,
        });
    assert_eq!(
        restore,
        Some(Some(DiagnosticFix::RestoreFiles(vec![PathBuf::from(
            "mods/LibX-1.20.1-5.0.14.jar"
        )]))),
        "{:?}",
        diagnosis.findings
    );
}

/// A pack mod put aside on purpose is a deliberate change with a way back, not
/// a missing file, even when the recipe records no address for it.
#[tokio::test]
async fn a_pack_mod_put_aside_is_reported_as_deliberate_and_offered_back() {
    let fixture = Fixture::with_mods(&[]).await;
    fixture.pin_java("21.0.8", None);
    fixture.write("minecraft/mods/miner.jar", b"miner");
    fixture.write_recipe(&[json!({
        "logical_path": "mods/miner.jar", "hashes": [], "downloads": [], "size": 5,
        "client": "required", "server": "required", "origin": "pack_declared",
    })]);
    let app = fixture.app();
    app.delete_mod_file(fixture.host(), Path::new("mods/miner.jar"))
        .unwrap();

    let diagnosis = fixture.diagnose(true).await;

    assert!(
        !diagnosis
            .findings
            .iter()
            .any(|finding| matches!(finding.kind, FindingKind::ManagedFilesDamaged { .. })),
        "{:?}",
        diagnosis.findings
    );
    let put_aside = diagnosis
        .findings
        .iter()
        .find(|finding| matches!(finding.kind, FindingKind::ModsPutAside { .. }))
        .expect("the put-aside mod is reported");
    assert_eq!(put_aside.class(), FindingClass::Customized);
    let restore = DiagnosticFix::RestorePutAside(vec![PathBuf::from("mods/miner.jar")]);
    assert_eq!(put_aside.fix.as_ref(), Some(&restore));
    assert!(diagnosis.is_healthy(), "{:?}", diagnosis.findings);

    let outcome = app
        .apply_fix(
            fixture.host(),
            &restore,
            &OperationReporter::silent("fixing"),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(outcome.applied, ["mods/miner.jar"]);
    assert!(fixture.enabled("miner"));
}

/// A put-aside mod another enabled mod requires is still a missing
/// dependency, and its kept copy is what offers it back.
#[tokio::test]
async fn a_put_aside_dependency_with_no_recorded_address_is_offered_back() {
    let fixture = Fixture::with_mods(&[("datatrader", &["libx"]), ("libx", &[])]).await;
    fixture.pin_java("21.0.8", None);
    fixture.write_recipe(&[
        json!({
            "logical_path": "mods/datatrader.jar", "hashes": [], "downloads": [], "size": 10,
            "client": "required", "server": "required", "origin": "pack_declared",
            "identity": {"provider": "modrinth", "project_id": "datatrader", "version_id": "datatrader1"},
            "relations": [{"kind":"required","target":{"project":{"project_id":"libx"}}}],
        }),
        json!({
            "logical_path": "mods/libx.jar", "hashes": [], "downloads": [], "size": 4,
            "client": "required", "server": "required", "origin": "pack_declared",
            "identity": {"provider": "modrinth", "project_id": "libx", "version_id": "libx1"},
        }),
    ]);
    fixture
        .app()
        .delete_mod_file(fixture.host(), Path::new("mods/libx.jar"))
        .unwrap();

    let diagnosis = fixture.diagnose(false).await;

    let restore = diagnosis
        .findings
        .iter()
        .find_map(|finding| match &finding.kind {
            FindingKind::DependencyMissing { .. } => Some(finding.fix.clone()),
            _ => None,
        });
    assert_eq!(
        restore,
        Some(Some(DiagnosticFix::RestorePutAside(vec![PathBuf::from(
            "mods/libx.jar"
        )]))),
        "{:?}",
        diagnosis.findings
    );
}

/// A kept copy that does not hold the recorded bytes cannot bring a required
/// mod back, so only a recorded address is offered, and without one nothing is.
#[tokio::test]
async fn a_put_aside_dependency_whose_kept_copy_is_wrong_is_offered_only_its_address() {
    for (downloads, expected) in [
        (
            json!(["https://cdn.modrinth.com/data/x/libx.jar"]),
            Some(DiagnosticFix::RestoreFiles(vec![PathBuf::from(
                "mods/libx.jar",
            )])),
        ),
        (json!([]), None),
    ] {
        let fixture = Fixture::with_mods(&[("datatrader", &["libx"]), ("libx", &[])]).await;
        fixture.pin_java("21.0.8", None);
        // The recorded digest is the empty file's, which the jar's bytes are not.
        fixture.write_recipe(&[
            json!({
                "logical_path": "mods/datatrader.jar", "hashes": [], "downloads": [], "size": 10,
                "client": "required", "server": "required", "origin": "pack_declared",
                "identity": {"provider": "modrinth", "project_id": "datatrader", "version_id": "datatrader1"},
                "relations": [{"kind":"required","target":{"project":{"project_id":"libx"}}}],
            }),
            json!({
                "logical_path": "mods/libx.jar",
                "hashes": [{"algorithm": "sha1", "digest": "da39a3ee5e6b4b0d3255bfef95601890afd80709"}],
                "downloads": downloads, "size": 4,
                "client": "required", "server": "required", "origin": "pack_declared",
                "identity": {"provider": "modrinth", "project_id": "libx", "version_id": "libx1"},
            }),
        ]);
        fixture
            .app()
            .delete_mod_file(fixture.host(), Path::new("mods/libx.jar"))
            .unwrap();

        let diagnosis = fixture.diagnose(false).await;

        let restore = diagnosis
            .findings
            .iter()
            .find_map(|finding| match &finding.kind {
                FindingKind::DependencyMissing { .. } => Some(finding.fix.clone()),
                _ => None,
            });
        assert_eq!(restore, Some(expected), "{:?}", diagnosis.findings);
    }
}

#[tokio::test]
async fn recorded_files_are_hashed_only_when_asked() {
    let fixture = Fixture::with_mods(&[]).await;
    fixture.pin_java("21.0.8", None);
    fixture.write_recipe(&[json!({
        "logical_path": "mods/gone.jar",
        "hashes": [{"algorithm": "sha1", "digest": "da39a3ee5e6b4b0d3255bfef95601890afd80709"}],
        "downloads": ["https://cdn.modrinth.com/data/x/gone.jar"], "size": 1,
        "client": "required", "server": "required", "origin": "user_selected",
    })]);

    let quick = fixture.diagnose(false).await;
    assert!(quick.integrity.is_none());
    assert!(quick.is_healthy());

    let full = fixture.diagnose(true).await;
    assert!(full.integrity.is_some());
    let problem = full
        .findings
        .iter()
        .find(|finding| matches!(finding.kind, FindingKind::ManagedFilesDamaged { .. }))
        .expect("the missing file is a problem");
    assert_eq!(
        problem.fix,
        Some(DiagnosticFix::RestoreFiles(vec![PathBuf::from(
            "mods/gone.jar"
        )]))
    );
}
