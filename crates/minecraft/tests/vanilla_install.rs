//! The vanilla installation pipeline, end to end against a loopback server
//!
//! Unit tests cover which artifacts a plan names; this covers what happens when
//! they are actually fetched - that every one of them is verified against the
//! digest Mojang published, that the natives jars are unpacked into the shared
//! natives store, that assets are fetched by hash into Mojang's own layout, and
//! that a corrupted artifact fails the launch rather than reaching the game.
//!
//! The version documents are **built here** rather than taken from the era
//! fixtures, because these tests need artifacts whose bytes and digests agree
//! with what the mock server serves - and a fixture cannot carry the SHA-1 of a
//! jar that is generated at test time. The *shapes* they exercise (both natives
//! eras, both argument forms) are the ones the fixtures pinned down; nothing
//! here reaches a live service, and `cargo test --offline` passes with the
//! network unplugged.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Write as _;
use std::path::Path;
use std::sync::Arc;

use ephervex_download::{
    CacheLayout, DownloadManager, DownloaderConfig, HttpConfig, OperationReporter, ProgressSink,
};
use ephervex_minecraft::{
    AssetIndex, LaunchError, OsName, Platform, RuleContext, VanillaInstaller, VersionMetadata,
};
use ephervex_model::Operation;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn sha1(bytes: &[u8]) -> String {
    use sha1::Digest as _;
    let mut hasher = sha1::Sha1::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// A jar containing the given entries, as `Vec<u8>`.
fn jar(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, body) in entries {
            writer.start_file(*name, options).expect("start entry");
            writer.write_all(body).expect("write entry");
        }
        writer.finish().expect("finish");
    }
    buffer.into_inner()
}

/// One thing the mock server serves, and the metadata that describes it.
struct Served {
    route: String,
    body: Vec<u8>,
}

impl Served {
    fn new(route: &str, body: Vec<u8>) -> Self {
        Self {
            route: route.to_owned(),
            body,
        }
    }

    fn sha1(&self) -> String {
        sha1(&self.body)
    }

    fn json(&self, server: &MockServer) -> String {
        format!(
            r#"{{"url":"{}{}","sha1":"{}","size":{}}}"#,
            server.uri(),
            self.route,
            self.sha1(),
            self.body.len()
        )
    }

    async fn mount(&self, server: &MockServer) {
        Mock::given(method("GET"))
            .and(path(self.route.clone()))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(self.body.clone()))
            .mount(server)
            .await;
    }
}

/// Everything one synthetic version serves.
struct Fake {
    client: Served,
    library: Served,
    natives: Served,
    logging: Served,
    asset: Served,
}

impl Fake {
    fn new() -> Self {
        Self {
            client: Served::new(
                "/client.jar",
                jar(&[("net/minecraft/Main.class", b"CAFEBABE")]),
            ),
            library: Served::new("/lib.jar", jar(&[("org/lwjgl/Lib.class", b"CAFEBABE")])),
            natives: Served::new(
                "/natives-linux.jar",
                jar(&[
                    ("liblwjgl.so", b"ELF-payload" as &[u8]),
                    ("META-INF/MANIFEST.MF", b"Manifest-Version: 1.0"),
                ]),
            ),
            logging: Served::new("/client-1.12.xml", b"<Configuration/>".to_vec()),
            asset: Served::new("/assets/object", b"an asset object".to_vec()),
        }
    }

    async fn mount(&self, server: &MockServer) {
        for served in [
            &self.client,
            &self.library,
            &self.natives,
            &self.logging,
            &self.asset,
        ] {
            served.mount(server).await;
        }
        // Assets are addressed by digest under Mojang's resource endpoint, so
        // the route the launcher will actually request is the sharded one.
        let digest = self.asset.sha1();
        Mock::given(method("GET"))
            .and(path(format!("/{}/{digest}", &digest[..2])))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(self.asset.body.clone()))
            .mount(server)
            .await;
    }

    fn asset_index(&self) -> Vec<u8> {
        format!(
            r#"{{"objects":{{"minecraft/sounds/a.ogg":{{"hash":"{}","size":{}}}}}}}"#,
            self.asset.sha1(),
            self.asset.body.len()
        )
        .into_bytes()
    }

    /// A 1.12.2-shaped document: the legacy argument string and a natives
    /// classifier map that has to be unpacked.
    fn legacy_document(&self, server: &MockServer, index: &Served) -> String {
        format!(
            r#"{{
              "id": "1.12.2", "type": "release",
              "time": "2017-09-18T08:39:46+00:00", "releaseTime": "2017-09-18T08:39:46+00:00",
              "mainClass": "net.minecraft.client.main.Main",
              "javaVersion": {{"component":"jre-legacy","majorVersion":8}},
              "assets": "1.12",
              "assetIndex": {{"id":"1.12","totalSize":1, {index_body}}},
              "downloads": {{"client": {client}}},
              "logging": {{"client": {{"argument":"-Dlog4j.configurationFile=${{path}}","type":"log4j2-xml","file": {{"id":"client-1.12.xml", {logging_body}}}}}}},
              "libraries": [
                {{"name":"org.lwjgl.lwjgl:lwjgl:2.9.4","downloads":{{"artifact":{library}}}}},
                {{"name":"org.lwjgl.lwjgl:lwjgl-platform:2.9.4",
                  "downloads":{{"classifiers":{{"natives-linux":{natives}}}}},
                  "natives":{{"linux":"natives-linux"}},
                  "extract":{{"exclude":["META-INF/"]}}}},
                {{"name":"ca.weblite:java-objc-bridge:1.0.0","downloads":{{"artifact":{library}}},
                  "rules":[{{"action":"allow","os":{{"name":"osx"}}}}]}}
              ],
              "minecraftArguments": "--username ${{auth_player_name}} --version ${{version_name}} --gameDir ${{game_directory}} --assetsDir ${{assets_root}} --assetIndex ${{assets_index_name}} --uuid ${{auth_uuid}} --accessToken ${{auth_access_token}} --userType ${{user_type}} --versionType ${{version_type}}"
            }}"#,
            index_body = strip_braces(&index.json(server)),
            logging_body = strip_braces(&self.logging.json(server)),
            client = self.client.json(server),
            library = self.library.json(server),
            natives = self.natives.json(server),
        )
    }

    /// A 1.21-shaped document: the modern argument object and natives as an
    /// ordinary rule-gated classpath entry, with nothing to unpack.
    fn modern_document(&self, server: &MockServer, index: &Served) -> String {
        format!(
            r#"{{
              "id": "1.21.1", "type": "release",
              "time": "2024-08-08T12:24:45+00:00", "releaseTime": "2024-08-08T12:24:45+00:00",
              "mainClass": "net.minecraft.client.main.Main",
              "javaVersion": {{"component":"java-runtime-delta","majorVersion":21}},
              "assets": "17",
              "assetIndex": {{"id":"17","totalSize":1, {index_body}}},
              "downloads": {{"client": {client}}},
              "libraries": [
                {{"name":"org.lwjgl:lwjgl:3.3.3","downloads":{{"artifact":{library}}}}},
                {{"name":"org.lwjgl:lwjgl:3.3.3:natives-linux","downloads":{{"artifact":{natives}}},
                  "rules":[{{"action":"allow","os":{{"name":"linux"}}}}]}},
                {{"name":"org.lwjgl:lwjgl:3.3.3:natives-windows","downloads":{{"artifact":{natives}}},
                  "rules":[{{"action":"allow","os":{{"name":"windows"}}}}]}}
              ],
              "arguments": {{
                "game": ["--username","${{auth_player_name}}","--assetIndex","${{assets_index_name}}"],
                "jvm": ["-Djava.library.path=${{natives_directory}}","-cp","${{classpath}}"]
              }}
            }}"#,
            index_body = strip_braces(&index.json(server)),
            client = self.client.json(server),
            library = self.library.json(server),
            natives = self.natives.json(server),
        )
    }
}

/// Splices one JSON object's fields into another literal.
fn strip_braces(json: &str) -> String {
    json.trim()
        .trim_start_matches('{')
        .trim_end_matches('}')
        .to_owned()
}

fn manager() -> DownloadManager {
    DownloadManager::new(DownloaderConfig {
        http: HttpConfig::for_loopback_tests(),
        ..DownloaderConfig::default()
    })
    .expect("client builds")
}

fn linux() -> RuleContext {
    RuleContext::new(Platform::new(OsName::Linux, "x86_64", "6.8.0"))
}

/// Cancels the token once the named file is reported as the last of its batch.
#[derive(Debug)]
struct CancelOnBatchCompletion {
    file: &'static str,
    cancel: CancellationToken,
}

impl ProgressSink for CancelOnBatchCompletion {
    fn operation_changed(&self, operation: &Operation) {
        let batch_done = operation.items.total == Some(operation.items.current);
        if batch_done && operation.current_step.as_deref() == Some(self.file) {
            self.cancel.cancel();
        }
    }
}

fn reporter() -> OperationReporter {
    OperationReporter::silent("vanilla-install")
}

/// An installer whose asset endpoint points at the mock server.
///
/// The asset URL is the one URL the pipeline constructs rather than reads, so a
/// test has to be able to redirect it exactly as the manifest's URLs are
/// redirected elsewhere.
fn installer(cache: &Path, server: &MockServer) -> VanillaInstaller {
    VanillaInstaller::new(manager(), CacheLayout::new(cache)).with_assets_base(&server.uri())
}

#[tokio::test]
async fn a_pre_1_19_version_installs_its_libraries_assets_and_unpacked_natives() {
    let server = MockServer::start().await;
    let fake = Fake::new();
    let index = Served::new("/1.12.json", fake.asset_index());
    index.mount(&server).await;
    fake.mount(&server).await;

    let cache = tempfile::tempdir().expect("temp dir");
    let installer = installer(cache.path(), &server);
    let metadata = VersionMetadata::parse(&fake.legacy_document(&server, &index))
        .expect("the synthetic document parses");

    let plan = installer.plan(&metadata, &linux()).expect("plans");
    // The osx-gated library is refused, so it is neither downloaded nor on the
    // classpath: "never every cached jar".
    assert_eq!(plan.libraries.len(), 1);
    assert_eq!(plan.natives.len(), 1);

    let installation = installer
        .install(plan, &reporter(), &CancellationToken::new())
        .await
        .expect("installs");

    assert_eq!(installation.assets, 1);
    assert_eq!(
        installation.natives_files, 1,
        "the manifest entry is excluded by extract.exclude"
    );

    let plan = &installation.plan;
    assert!(plan.client_jar.path.is_file());
    assert!(plan.libraries[0].path.is_file());
    assert!(
        plan.libraries[0]
            .path
            .starts_with(cache.path().join("libraries")),
        "libraries live in the shared store, not in the instance"
    );
    assert!(
        plan.natives_dir.join("liblwjgl.so").is_file(),
        "the classifier jar is unpacked where java.library.path points"
    );
    assert!(
        !plan.natives_dir.join("META-INF").exists(),
        "extract.exclude is honored from the metadata"
    );
    assert!(
        plan.natives_dir
            .starts_with(cache.path().join("natives").join("1.12.2")),
        "natives are launcher-level, keyed by version and platform"
    );
    assert!(
        plan.logging
            .as_ref()
            .expect("1.12.2 ships a log4j configuration")
            .artifact
            .path
            .is_file()
    );

    // Mojang's own asset layout, so the game finds the file by hash.
    let digest = sha1(b"an asset object");
    assert!(
        cache
            .path()
            .join("assets/objects")
            .join(&digest[..2])
            .join(&digest)
            .is_file()
    );
    assert_eq!(
        std::fs::read(cache.path().join("assets/indexes/1.12.json"))
            .expect("launch-visible asset index"),
        fake.asset_index(),
        "Minecraft must find the verified index at --assetsDir/indexes/--assetIndex.json"
    );
}

#[tokio::test]
async fn a_1_19_or_later_version_puts_its_natives_on_the_classpath_and_unpacks_nothing() {
    let server = MockServer::start().await;
    let fake = Fake::new();
    let index = Served::new("/17.json", fake.asset_index());
    index.mount(&server).await;
    fake.mount(&server).await;

    let cache = tempfile::tempdir().expect("temp dir");
    let installer = installer(cache.path(), &server);
    let metadata = VersionMetadata::parse(&fake.modern_document(&server, &index)).expect("parses");
    let plan = installer.plan(&metadata, &linux()).expect("plans");

    assert!(plan.natives.is_empty(), "there is no extraction step");
    assert_eq!(
        plan.libraries.len(),
        2,
        "the linux native is a classpath entry; the windows one is refused"
    );
    assert!(
        plan.libraries
            .iter()
            .any(|library| library.label.ends_with(":natives-linux"))
    );

    let installation = installer
        .install(plan, &reporter(), &CancellationToken::new())
        .await
        .expect("installs");

    assert_eq!(installation.natives_files, 0);
    assert!(
        installation.plan.natives_dir.is_dir(),
        "the directory still has to exist: LWJGL extracts into it and will not create it"
    );
    for library in &installation.plan.libraries {
        assert!(library.path.is_file(), "{}", library.label);
    }
    assert!(
        cache.path().join("assets/indexes/17.json").is_file(),
        "modern releases use the same named asset-index presentation layout"
    );
}

#[tokio::test]
async fn an_artifact_that_fails_its_digest_stops_the_launch_and_names_what_it_was() {
    let server = MockServer::start().await;
    let fake = Fake::new();
    let index = Served::new("/17.json", fake.asset_index());
    index.mount(&server).await;
    fake.mount(&server).await;
    // The library route now serves something else entirely. Its declared SHA-1
    // is the only thing that can tell the difference.
    server.reset().await;
    index.mount(&server).await;
    fake.client.mount(&server).await;
    fake.natives.mount(&server).await;
    Mock::given(method("GET"))
        .and(path("/lib.jar"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"tampered".to_vec()))
        .mount(&server)
        .await;

    let cache = tempfile::tempdir().expect("temp dir");
    let installer = installer(cache.path(), &server);
    let metadata = VersionMetadata::parse(&fake.modern_document(&server, &index)).expect("parses");
    let plan = installer.plan(&metadata, &linux()).expect("plans");
    let library_path = plan.libraries[0].path.clone();

    let error = installer
        .install(plan, &reporter(), &CancellationToken::new())
        .await
        .expect_err("a tampered artifact must not reach the classpath");

    assert!(matches!(error, LaunchError::DownloadFailed { .. }));
    assert_eq!(
        error.message_id(),
        "error-launch-artifact-mismatched",
        "a file served with the wrong contents is not a network problem, and telling \
         somebody to check their connection would send them the wrong way"
    );
    assert!(
        !library_path.exists(),
        "a bad artifact is deleted rather than left where a retry would trust it"
    );
}

#[tokio::test]
async fn a_library_that_fails_to_download_is_named_even_when_its_address_has_a_query() {
    let server = MockServer::start().await;
    let mut fake = Fake::new();
    // Left unserved. The failure's address is redacted of the query string.
    fake.library.route = "/lib.jar?signature=secret".to_owned();
    let index = Served::new("/17.json", fake.asset_index());
    index.mount(&server).await;
    fake.client.mount(&server).await;
    fake.natives.mount(&server).await;

    let cache = tempfile::tempdir().expect("temp dir");
    let installer = installer(cache.path(), &server);
    let metadata = VersionMetadata::parse(&fake.modern_document(&server, &index)).expect("parses");
    let plan = installer.plan(&metadata, &linux()).expect("plans");

    let error = installer
        .install(plan, &reporter(), &CancellationToken::new())
        .await
        .expect_err("a missing library fails the install");

    assert!(
        matches!(&error, LaunchError::DownloadFailed { artifact, .. } if artifact == "org.lwjgl:lwjgl:3.3.3"),
        "{error:?}"
    );
}

#[tokio::test]
async fn canceling_an_install_is_reported_as_cancellation_rather_than_a_failed_file() {
    let server = MockServer::start().await;
    let fake = Fake::new();
    let index = Served::new("/17.json", fake.asset_index());
    index.mount(&server).await;
    fake.mount(&server).await;

    let cache = tempfile::tempdir().expect("temp dir");
    let installer = installer(cache.path(), &server);
    let metadata = VersionMetadata::parse(&fake.modern_document(&server, &index)).expect("parses");
    let plan = installer.plan(&metadata, &linux()).expect("plans");
    let cancel = CancellationToken::new();
    cancel.cancel();

    let error = installer
        .install(plan, &reporter(), &cancel)
        .await
        .expect_err("a canceled install does not complete");

    assert!(error.is_cancellation(), "{error:?}");
}

#[tokio::test]
async fn an_install_canceled_after_its_downloads_publishes_no_natives_or_asset_index() {
    let server = MockServer::start().await;
    let fake = Fake::new();
    let index = Served::new("/1.12.json", fake.asset_index());
    index.mount(&server).await;
    fake.mount(&server).await;

    let cache = tempfile::tempdir().expect("temp dir");
    let installer = installer(cache.path(), &server);
    let metadata = VersionMetadata::parse(&fake.legacy_document(&server, &index)).expect("parses");
    let plan = installer.plan(&metadata, &linux()).expect("plans");
    let natives_dir = plan.natives_dir.clone();
    let asset_index_path = plan.asset_index_path.clone();

    let cancel = CancellationToken::new();
    let sink = Arc::new(CancelOnBatchCompletion {
        file: "minecraft/sounds/a.ogg",
        cancel: cancel.clone(),
    });
    let reporter = OperationReporter::new("vanilla-install", true, sink);

    let error = installer
        .install(plan, &reporter, &cancel)
        .await
        .expect_err("an install canceled once its downloads finished does not succeed");

    assert!(cancel.is_cancelled(), "the sink canceled the install");
    assert!(error.is_cancellation(), "{error:?}");
    assert!(
        !natives_dir.join("liblwjgl.so").exists(),
        "no native is unpacked after cancellation"
    );
    assert!(
        !asset_index_path.exists(),
        "the launch-visible asset index is not published after cancellation"
    );
    let leftovers = std::fs::read_dir(asset_index_path.parent().expect("indexes directory"))
        .map_or(0, Iterator::count);
    assert_eq!(leftovers, 0, "no temporary file is left behind");
}

#[tokio::test]
async fn an_asset_that_fails_to_download_is_named() {
    let server = MockServer::start().await;
    let fake = Fake::new();
    let index = Served::new("/17.json", fake.asset_index());
    index.mount(&server).await;
    fake.client.mount(&server).await;
    fake.library.mount(&server).await;
    fake.natives.mount(&server).await;

    let cache = tempfile::tempdir().expect("temp dir");
    let installer = installer(cache.path(), &server);
    let metadata = VersionMetadata::parse(&fake.modern_document(&server, &index)).expect("parses");
    let plan = installer.plan(&metadata, &linux()).expect("plans");

    let error = installer
        .install(plan, &reporter(), &CancellationToken::new())
        .await
        .expect_err("a missing asset fails the install");

    assert!(
        matches!(&error, LaunchError::DownloadFailed { artifact, .. } if artifact == "minecraft/sounds/a.ogg"),
        "{error:?}"
    );
}

#[tokio::test]
async fn an_asset_index_that_is_not_an_index_fails_rather_than_launching_without_assets() {
    let server = MockServer::start().await;
    let fake = Fake::new();
    // Well-formed JSON, and not an asset index. Launching anyway would produce
    // a game with no sounds and no reason on screen.
    let index = Served::new("/17.json", br#"{"totalSize": 12}"#.to_vec());
    index.mount(&server).await;
    fake.mount(&server).await;

    let cache = tempfile::tempdir().expect("temp dir");
    let installer = installer(cache.path(), &server);
    let metadata = VersionMetadata::parse(&fake.modern_document(&server, &index)).expect("parses");
    let plan = installer.plan(&metadata, &linux()).expect("plans");

    let error = installer
        .install(plan, &reporter(), &CancellationToken::new())
        .await
        .expect_err("an unusable index is a failed launch");
    assert!(matches!(error, LaunchError::AssetIndex(_)));
}

#[tokio::test]
async fn a_second_install_of_the_same_version_downloads_nothing() {
    // Every artifact is content-addressed and finalized atomically, so presence
    // at the destination is proof it was verified.
    let server = MockServer::start().await;
    let fake = Fake::new();
    let index = Served::new("/17.json", fake.asset_index());
    index.mount(&server).await;
    fake.mount(&server).await;

    let cache = tempfile::tempdir().expect("temp dir");
    let installer = installer(cache.path(), &server);
    let metadata = VersionMetadata::parse(&fake.modern_document(&server, &index)).expect("parses");

    for _ in 0..2 {
        let plan = installer.plan(&metadata, &linux()).expect("plans");
        installer
            .install(plan, &reporter(), &CancellationToken::new())
            .await
            .expect("installs");
    }

    let requests = server.received_requests().await.expect("request log").len();
    assert_eq!(
        requests, 5,
        "one request per distinct artifact across both installs"
    );
}

#[tokio::test]
async fn the_command_built_from_a_real_installation_points_at_the_files_that_exist() {
    // The join between installation and launch: everything the command line
    // names has to be a path the installer actually produced.
    let server = MockServer::start().await;
    let fake = Fake::new();
    let index = Served::new("/1.12.json", fake.asset_index());
    index.mount(&server).await;
    fake.mount(&server).await;

    let cache = tempfile::tempdir().expect("temp dir");
    let instance = tempfile::tempdir().expect("temp dir");
    let installer = installer(cache.path(), &server);
    let metadata = VersionMetadata::parse(&fake.legacy_document(&server, &index)).expect("parses");
    let plan = installer.plan(&metadata, &linux()).expect("plans");

    let installation = installer
        .install(plan, &reporter(), &CancellationToken::new())
        .await
        .expect("installs");

    let command = ephervex_minecraft::build_command(
        &metadata,
        &installation.plan,
        &ephervex_minecraft::LaunchSettings {
            java: std::path::PathBuf::from("/usr/bin/java"),
            game_directory: instance.path().join("minecraft"),
            identity: ephervex_minecraft::PlayerIdentity::offline(
                "Notch",
                "b50ad385-829d-3141-a216-7e7d7539ba7f",
            ),
            features: ephervex_minecraft::LaunchFeatures::none(),
            jvm: ephervex_minecraft::JvmOptions::default(),
        },
    )
    .expect("builds");

    let arguments: Vec<String> = command.arguments().map(str::to_owned).collect();
    let classpath = arguments
        .iter()
        .position(|argument| argument == "-cp")
        .map(|index| arguments[index + 1].clone())
        .expect("a classpath");

    for entry in classpath.split(':') {
        assert!(
            Path::new(entry).is_file(),
            "the classpath names a file that was never installed: {entry}"
        );
    }
    assert!(arguments.iter().any(
        |argument| argument.starts_with("-Dlog4j.configurationFile=")
            && Path::new(argument.trim_start_matches("-Dlog4j.configurationFile=")).is_file()
    ));
}

#[tokio::test]
async fn an_installation_names_every_shared_path_it_wrote() {
    let server = MockServer::start().await;
    let fake = Fake::new();
    let index = Served::new("/1.12.json", fake.asset_index());
    index.mount(&server).await;
    fake.mount(&server).await;
    let cache = tempfile::tempdir().expect("temp dir");
    let installer = installer(cache.path(), &server);
    let metadata = VersionMetadata::parse(&fake.legacy_document(&server, &index)).expect("parses");
    let plan = installer.plan(&metadata, &linux()).expect("plans");
    installer
        .install(plan.clone(), &reporter(), &CancellationToken::new())
        .await
        .expect("installs");

    let stored = plan.stored_paths();
    for path in &stored.files {
        assert!(
            path.is_file(),
            "{} is named but was not written",
            path.display()
        );
    }
    assert_eq!(stored.directories, [plan.natives_dir.clone()].into());
    assert!(plan.natives_dir.is_dir());
    assert!(stored.files.contains(&plan.client_jar.path));
    assert!(stored.files.contains(&plan.asset_index_path));

    let layout = CacheLayout::new(cache.path());
    let objects = AssetIndex::parse("1.12", &String::from_utf8(fake.asset_index()).unwrap())
        .expect("index")
        .object_paths(&layout)
        .expect("paths");
    assert_eq!(objects.len(), 1);
    assert!(objects.iter().all(|path| path.is_file()));
}

#[tokio::test]
async fn reacquiring_fetches_only_the_named_artifacts_and_replaces_damaged_bytes() {
    let server = MockServer::start().await;
    let fake = Fake::new();
    let index = Served::new("/1.12.json", fake.asset_index());
    index.mount(&server).await;
    fake.mount(&server).await;
    let cache = tempfile::tempdir().expect("temp dir");
    let installer = installer(cache.path(), &server);
    let metadata = VersionMetadata::parse(&fake.legacy_document(&server, &index)).expect("parses");
    let plan = installer.plan(&metadata, &linux()).expect("plans");
    installer
        .install(plan.clone(), &reporter(), &CancellationToken::new())
        .await
        .expect("installs");

    let library = plan.libraries[0].path.clone();
    let natives_jar = plan.natives[0].artifact.path.clone();
    let digest = sha1(b"an asset object");
    let asset = cache
        .path()
        .join("assets/objects")
        .join(&digest[..2])
        .join(&digest);
    std::fs::write(&library, b"damaged").expect("damage the library");
    std::fs::write(&asset, b"damaged").expect("damage the asset");
    std::fs::remove_file(&natives_jar).expect("remove the natives jar");
    std::fs::remove_file(plan.natives_dir.join("liblwjgl.so")).expect("remove unpacked natives");
    std::fs::remove_file(&plan.client_jar.path).expect("remove the client jar");
    std::fs::write(&plan.asset_index_path, b"stale view").expect("damage the launch view");

    let named = [
        library.clone(),
        asset.clone(),
        natives_jar.clone(),
        plan.asset_index_path.clone(),
    ]
    .into_iter()
    .collect();
    let fetched = installer
        .reacquire(&plan, &named, &reporter(), &CancellationToken::new())
        .await
        .expect("reacquires");

    assert_eq!(
        fetched, 3,
        "the library, the natives jar, and the asset object"
    );
    assert_eq!(std::fs::read(&library).expect("library"), fake.library.body);
    assert_eq!(std::fs::read(&asset).expect("asset"), fake.asset.body);
    assert!(natives_jar.is_file());
    assert!(
        plan.natives_dir.join("liblwjgl.so").is_file(),
        "unpacked natives are derived again from a re-fetched natives jar"
    );
    assert_eq!(
        std::fs::read(&plan.asset_index_path).expect("view"),
        fake.asset_index(),
        "the launch view is copied again from the verified index"
    );
    assert!(
        !plan.client_jar.path.exists(),
        "an artifact that was not named is not fetched"
    );
}

#[tokio::test]
async fn a_failed_reacquisition_leaves_the_entry_absent_rather_than_present_and_wrong() {
    let server = MockServer::start().await;
    let fake = Fake::new();
    let index = Served::new("/1.12.json", fake.asset_index());
    index.mount(&server).await;
    fake.mount(&server).await;
    let cache = tempfile::tempdir().expect("temp dir");
    let installer = installer(cache.path(), &server);
    let metadata = VersionMetadata::parse(&fake.legacy_document(&server, &index)).expect("parses");
    let plan = installer.plan(&metadata, &linux()).expect("plans");
    installer
        .install(plan.clone(), &reporter(), &CancellationToken::new())
        .await
        .expect("installs");
    let library = plan.libraries[0].path.clone();
    std::fs::write(&library, b"damaged").expect("damage the library");
    server.reset().await;
    Mock::given(method("GET"))
        .and(path("/lib.jar"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"tampered".to_vec()))
        .mount(&server)
        .await;

    let error = installer
        .reacquire(
            &plan,
            &[library.clone()].into_iter().collect(),
            &reporter(),
            &CancellationToken::new(),
        )
        .await
        .expect_err("the replacement does not match its digest");

    assert!(
        matches!(error, LaunchError::DownloadFailed { .. }),
        "{error:?}"
    );
    assert!(
        !library.exists(),
        "neither the damaged bytes nor the unverified replacement may stay at the path"
    );
}

#[tokio::test]
async fn a_canceled_reacquisition_is_reported_as_canceled() {
    let server = MockServer::start().await;
    let fake = Fake::new();
    let index = Served::new("/1.12.json", fake.asset_index());
    index.mount(&server).await;
    fake.mount(&server).await;
    let cache = tempfile::tempdir().expect("temp dir");
    let installer = installer(cache.path(), &server);
    let metadata = VersionMetadata::parse(&fake.legacy_document(&server, &index)).expect("parses");
    let plan = installer.plan(&metadata, &linux()).expect("plans");
    let cancel = CancellationToken::new();
    cancel.cancel();

    let error = installer
        .reacquire(
            &plan,
            &[plan.libraries[0].path.clone()].into_iter().collect(),
            &reporter(),
            &cancel,
        )
        .await
        .expect_err("canceled");

    assert!(matches!(error, LaunchError::Canceled), "{error:?}");
}
