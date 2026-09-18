//! The Modrinth pack import required behavior, end to end against a loopback server.
//!
//! > A fixture containing a client-only mod, a server-only mod, an optional
//! > mod, a common override, a client override, and a server override produces
//! > the correct client, and later produces the correct server recipe without
//! > re-reading the original archive.
//!
//! Both halves are here. The first imports the fixture pack under
//! `fixtures/modrinth/layered-client-server/` and checks what landed in the game
//! directory; the second **deletes the archive** and rebuilds the server plan
//! from the retained recipe alone, which is the durability property under test
//! and the one a mutated client directory could not provide.
//!
//! Nothing here reaches a live service. The pack's download addresses name
//! `cdn.modrinth.com` - the host the allowlist accepts, and what a real pack
//! contains - and the harness rewrites only their origin to a `wiremock`
//! server, asserting the original host first. `fixtures/modrinth/SOURCES.md`
//! records that transformation.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Write as _;
use std::path::{Path, PathBuf};

use ephervex_download::{
    CacheLayout, DomainAllowlist, DownloadManager, DownloaderConfig, HttpConfig, OperationReporter,
};
use ephervex_model::{LoaderKind, ModOrigin};
use ephervex_pack::{
    ExtractionLimits, MaterializeRequest, OpenArtifacts, OptionalSelection, PackArchive, PackError,
    PackFormat, PackPolicy, PackSide, SideRequirement, analyze, materialize, retained_recipe,
};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path as path_matcher};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// `<repo>/fixtures/modrinth`.
fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/modrinth")
        .canonicalize()
        .expect("the fixtures directory exists")
}

fn read_fixture(relative: &str) -> Vec<u8> {
    std::fs::read(fixtures().join(relative))
        .unwrap_or_else(|error| panic!("fixture {relative} is readable: {error}"))
}

/// Builds a `.mrpack` from a directory of fixture files.
///
/// Directory entries are written as well as files, because a real pack has
/// them and because the extractor has to cope with both.
fn build_archive(destination: &Path, index: &[u8], layers: &[&str]) {
    let file = std::fs::File::create(destination).expect("create archive");
    let mut writer = zip::ZipWriter::new(file);
    let options: zip::write::FileOptions<'_, ()> =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    writer
        .start_file("modrinth.index.json", options)
        .expect("start index");
    writer.write_all(index).expect("write index");

    let root = fixtures().join("layered-client-server");
    for layer in layers {
        let source = root.join(layer);
        if !source.is_dir() {
            continue;
        }
        writer
            .add_directory(format!("{layer}/"), options)
            .expect("layer directory");
        for entry in walk(&source) {
            let relative = entry.strip_prefix(&root).expect("inside the fixture");
            let name = relative.to_string_lossy().replace('\\', "/");
            writer.start_file(name, options).expect("start entry");
            writer
                .write_all(&std::fs::read(&entry).expect("read fixture file"))
                .expect("write entry");
        }
    }
    writer.finish().expect("finish archive");
}

fn walk(directory: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![directory.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in std::fs::read_dir(&current).expect("read dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                stack.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// Serves the pack's files, and returns the index with its download addresses
/// pointed at the mock server.
///
/// The rewrite is deliberately narrow: the *origin* only, and only after the
/// address has been checked against the host a real pack would name.
async fn serve_pack_files(server: &MockServer) -> String {
    let index = String::from_utf8(read_fixture("layered-client-server/modrinth.index.json"))
        .expect("the index is UTF-8");
    let parsed: serde_json::Value = serde_json::from_str(&index).expect("the index is JSON");

    for file in parsed["files"].as_array().expect("files") {
        let address = file["downloads"][0].as_str().expect("an address");
        let url = reqwest::Url::parse(address).expect("a URL");
        assert_eq!(
            url.host_str(),
            Some("cdn.modrinth.com"),
            "the fixture names the host the allowlist accepts"
        );

        let name = Path::new(file["path"].as_str().expect("a path"))
            .file_name()
            .expect("a file name")
            .to_string_lossy()
            .into_owned();
        let body = read_fixture(&format!("files/{name}"));
        Mock::given(method("GET"))
            .and(path_matcher(url.path().to_owned()))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(server)
            .await;
    }

    index.replace("https://cdn.modrinth.com", &server.uri())
}

/// A policy that trusts the loopback server and nothing else.
fn loopback_policy(server: &MockServer) -> PackPolicy {
    let host = reqwest::Url::parse(&server.uri()).expect("a URL");
    PackPolicy {
        limits: ExtractionLimits::default(),
        allowlist: DomainAllowlist::for_loopback_tests(host.host_str().expect("a host")),
    }
}

fn loopback_manager() -> DownloadManager {
    DownloadManager::new(DownloaderConfig {
        http: HttpConfig::for_loopback_tests(),
        ..DownloaderConfig::default()
    })
    .expect("download manager")
}

/// One imported instance's directories.
struct Imported {
    game: PathBuf,
    source: PathBuf,
}

async fn import(
    workspace: &Path,
    archive: &Path,
    policy: &PackPolicy,
    selection: &OptionalSelection,
) -> Imported {
    let (mut opened, analysis) = analyze(archive, policy).expect("analyze");
    assert_eq!(analysis.format, PackFormat::ModrinthPack);

    let imported = Imported {
        game: workspace.join("instance/minecraft"),
        source: workspace.join("instance/.launcher/source"),
    };
    materialize(MaterializeRequest {
        archive: &mut opened,
        analysis: &analysis,
        side: PackSide::Client,
        selection,
        game_dir: &imported.game,
        source_dir: &imported.source,
        manager: &loopback_manager(),
        cache: &CacheLayout::new(workspace.join("cache")),
        credentials: &OpenArtifacts,
        reporter: &OperationReporter::silent("modrinth-pack-import"),
        cancel: &CancellationToken::new(),
    })
    .await
    .expect("materialize");
    imported
}

#[tokio::test]
async fn the_workflow_fixture_produces_the_correct_client_and_later_the_correct_server_recipe() {
    let server = MockServer::start().await;
    let index = serve_pack_files(&server).await;
    let workspace = tempfile::tempdir().expect("temp dir");
    let archive = workspace.path().join("layered-client-server.mrpack");
    build_archive(
        &archive,
        index.as_bytes(),
        &["overrides", "client-overrides", "server-overrides"],
    );

    let imported = import(
        workspace.path(),
        &archive,
        &loopback_policy(&server),
        &OptionalSelection::none(),
    )
    .await;

    // -- the correct client ------------------------------------------------

    // Required-everywhere and client-required files are installed...
    assert!(imported.game.join("mods/both-sides.jar").is_file());
    assert!(imported.game.join("mods/client-only.jar").is_file());
    // ...the server-only file is not, and its recipe still survives.
    assert!(
        !imported.game.join("mods/server-only.jar").exists(),
        "a server-only mod is never installed into a client"
    );
    // ...and an optional file nobody selected stays off.
    assert!(!imported.game.join("mods/optional.jar").exists());

    // The installed jars are the fixture's bytes, verified against the hashes
    // the index published.
    assert_eq!(
        std::fs::read(imported.game.join("mods/both-sides.jar")).expect("read"),
        read_fixture("files/both-sides.jar")
    );

    // Overrides, then client-overrides; the later layer wins.
    assert_eq!(
        std::fs::read(imported.game.join("config/shared.toml")).expect("read"),
        read_fixture("layered-client-server/overrides/config/shared.toml"),
        "the common layer reached the client"
    );
    assert_eq!(
        std::fs::read(imported.game.join("options.txt")).expect("read"),
        read_fixture("layered-client-server/client-overrides/options.txt"),
        "the client layer replaced the common layer's copy of the same file"
    );
    // The server layer never touches the client.
    assert!(!imported.game.join("server.properties").exists());

    // -- the correct server recipe, without the archive ---------------------

    std::fs::remove_file(&archive).expect("the archive goes away");
    drop(server);

    let recipe = retained_recipe(&imported.source).expect("the recipe was retained");
    assert_eq!(recipe.minecraft_version, "1.20.1");
    assert_eq!(recipe.loader.kind, LoaderKind::NeoForge);
    assert_eq!(recipe.loader.version.as_deref(), Some("47.1.106"));

    let plan = recipe.plan(PackSide::Server, &OptionalSelection::none());
    let files: Vec<String> = plan.files.iter().map(|file| file.display_name()).collect();
    assert_eq!(
        files,
        vec!["mods/both-sides.jar", "mods/server-only.jar"],
        "the server takes what the pack said it takes, not what the client has"
    );

    // Server layering is common overrides followed by server overrides, and both are on
    // disk, so a server can be built with the archive long gone.
    let layers: Vec<&str> = plan
        .layers
        .iter()
        .map(|layer| layer.scope.as_str())
        .collect();
    assert_eq!(layers, vec!["common", "server"]);
    assert!(plan.layers_are_retained());
    assert_eq!(
        std::fs::read(
            imported
                .source
                .join("modrinth/server-overrides/server.properties")
        )
        .expect("read"),
        read_fixture("layered-client-server/server-overrides/server.properties")
    );
    assert_eq!(
        std::fs::read(
            imported
                .source
                .join("modrinth/overrides/config/shared.toml")
        )
        .expect("read"),
        read_fixture("layered-client-server/overrides/config/shared.toml")
    );
    // The original manifest is kept byte for byte, so a later build can re-read
    // what the pack actually said rather than this build's summary of it.
    assert_eq!(
        std::fs::read(imported.source.join("modrinth/modrinth.index.json")).expect("read"),
        index.as_bytes()
    );

    // The optional file is still in the recipe, still optional, and recorded as
    // refused rather than as never asked.
    let optional = recipe
        .files
        .iter()
        .find(|file| file.display_name() == "mods/optional.jar")
        .expect("preserved");
    assert_eq!(optional.client, SideRequirement::Optional);
    assert_eq!(optional.server, SideRequirement::Optional);
    assert_eq!(optional.selected, Some(false));
}

/// The index lists what the pack points at. What it carries in its own
/// override tree is named nowhere, so a jar shipped that way was read back off
/// disk as one somebody had copied in by hand and reported as theirs.
#[tokio::test]
async fn a_mod_the_pack_ships_in_its_override_tree_is_recorded_as_pack_content() {
    let server = MockServer::start().await;
    let index = serve_pack_files(&server).await;
    let workspace = tempfile::tempdir().expect("temp dir");
    let archive = workspace.path().join("layered-client-server.mrpack");
    build_archive(
        &archive,
        index.as_bytes(),
        &["overrides", "client-overrides", "server-overrides"],
    );

    let imported = import(
        workspace.path(),
        &archive,
        &loopback_policy(&server),
        &OptionalSelection::none(),
    )
    .await;
    let recipe = retained_recipe(&imported.source).expect("the recipe was retained");

    let shipped = |name: &str| {
        recipe
            .files
            .iter()
            .find(|file| file.display_name() == name)
            .unwrap_or_else(|| panic!("{name} is recorded"))
    };

    for name in [
        "mods/shipped-with-the-pack.jar",
        "mods/client-only-shipped.jar",
        "mods/server-only-shipped.jar",
    ] {
        assert_eq!(
            shipped(name).origin,
            ModOrigin::PackOverride,
            "{name} came out of the pack, so the list has to say so"
        );
        assert!(
            shipped(name).downloads.is_empty(),
            "{name} was shipped rather than pointed at"
        );
    }

    // The layer says which sides a shipped mod belongs to, because it is the
    // only thing the pack said about it.
    assert_eq!(
        shipped("mods/client-only-shipped.jar").server,
        SideRequirement::Unsupported
    );
    assert_eq!(
        shipped("mods/server-only-shipped.jar").client,
        SideRequirement::Unsupported
    );
    assert_eq!(
        shipped("mods/shipped-with-the-pack.jar").client,
        SideRequirement::Required
    );

    assert!(
        recipe
            .files
            .iter()
            .all(|file| file.display_name() != "mods/notes.txt"),
        "a pack may ship anything into mods/, and only a jar is a mod"
    );

    // Recorded, and still never fetched: the plans that download are the ones
    // an entry with no address would fail in.
    assert!(
        recipe
            .files_for(PackSide::Client, &OptionalSelection::none())
            .iter()
            .all(|file| !file.is_layer_supplied()),
        "a shipped jar is extracted, never downloaded"
    );
    let plan = recipe.plan(PackSide::Server, &OptionalSelection::none());
    assert!(
        plan.files.iter().all(|file| !file.is_layer_supplied()),
        "and a server is built from the retained layer rather than by fetching it"
    );
}

#[tokio::test]
async fn selecting_an_optional_file_installs_it_and_records_the_decision_for_the_server() {
    let server = MockServer::start().await;
    let index = serve_pack_files(&server).await;
    let workspace = tempfile::tempdir().expect("temp dir");
    let archive = workspace.path().join("layered-client-server.mrpack");
    build_archive(&archive, index.as_bytes(), &["overrides"]);

    let imported = import(
        workspace.path(),
        &archive,
        &loopback_policy(&server),
        &OptionalSelection::of(["mods/optional.jar"]),
    )
    .await;

    assert!(imported.game.join("mods/optional.jar").is_file());

    // A server created later follows the recorded import selection rather
    // than asking again from nothing.
    let recipe = retained_recipe(&imported.source).expect("recipe");
    let files: Vec<String> = recipe
        .plan(PackSide::Server, &OptionalSelection::none())
        .files
        .iter()
        .map(|file| file.display_name())
        .collect();
    assert!(files.contains(&"mods/optional.jar".to_owned()));
}

#[tokio::test]
async fn importing_the_same_pack_twice_costs_no_second_download() {
    // What makes disposable instances cheap: the pack's
    // files are content-addressed in the shared cache, so a second import of
    // the same pack is a copy rather than a fetch.
    let server = MockServer::start().await;
    let index = serve_pack_files(&server).await;
    let workspace = tempfile::tempdir().expect("temp dir");
    let archive = workspace.path().join("layered-client-server.mrpack");
    build_archive(&archive, index.as_bytes(), &["overrides"]);

    let policy = loopback_policy(&server);
    let cache = CacheLayout::new(workspace.path().join("cache"));
    let mut reused = 0;
    for run in 0..2 {
        let (mut opened, analysis) = analyze(&archive, &policy).expect("analyze");
        let outcome = materialize(MaterializeRequest {
            archive: &mut opened,
            analysis: &analysis,
            side: PackSide::Client,
            selection: &OptionalSelection::none(),
            game_dir: &workspace.path().join(format!("instance-{run}/minecraft")),
            source_dir: &workspace
                .path()
                .join(format!("instance-{run}/.launcher/source")),
            manager: &loopback_manager(),
            cache: &cache,
            credentials: &OpenArtifacts,
            reporter: &OperationReporter::silent("modrinth-pack-import"),
            cancel: &CancellationToken::new(),
        })
        .await
        .expect("materialize");
        reused = outcome.files.reused_from_cache;
    }
    assert_eq!(
        reused, 2,
        "the second import reused both client-side files from the shared cache"
    );
}

#[tokio::test]
async fn a_hash_mismatch_fails_the_import_rather_than_installing_the_file() {
    // A hash mismatch is a hard failure. The bad artifact is
    // deleted rather than left at a cache path where it would look verified.
    let server = MockServer::start().await;
    let index = String::from_utf8(read_fixture("layered-client-server/modrinth.index.json"))
        .expect("the index is UTF-8");
    let parsed: serde_json::Value = serde_json::from_str(&index).expect("json");
    for file in parsed["files"].as_array().expect("files") {
        let url =
            reqwest::Url::parse(file["downloads"][0].as_str().expect("address")).expect("url");
        Mock::given(method("GET"))
            .and(path_matcher(url.path().to_owned()))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"not the file".to_vec()))
            .mount(&server)
            .await;
    }
    let index = index.replace("https://cdn.modrinth.com", &server.uri());

    let workspace = tempfile::tempdir().expect("temp dir");
    let archive = workspace.path().join("tampered.mrpack");
    build_archive(&archive, index.as_bytes(), &["overrides"]);

    let (mut opened, analysis) = analyze(&archive, &loopback_policy(&server)).expect("analyze");
    let game = workspace.path().join("instance/minecraft");
    let error = materialize(MaterializeRequest {
        archive: &mut opened,
        analysis: &analysis,
        side: PackSide::Client,
        selection: &OptionalSelection::none(),
        game_dir: &game,
        source_dir: &workspace.path().join("instance/.launcher/source"),
        manager: &loopback_manager(),
        cache: &CacheLayout::new(workspace.path().join("cache")),
        credentials: &OpenArtifacts,
        reporter: &OperationReporter::silent("modrinth-pack-import"),
        cancel: &CancellationToken::new(),
    })
    .await
    .expect_err("a tampered file must not be installed");

    assert_eq!(error.message_id(), "error-pack-download-failed");
    assert!(!game.join("mods/both-sides.jar").exists());
}

#[tokio::test]
async fn every_refusal_fixture_is_refused_by_the_rule_it_was_written_for() {
    // One case per document under `fixtures/modrinth/invalid/`, so a
    // regression names the rule it broke.
    type RefusalCase = (&'static str, fn(&PackError) -> bool);

    let workspace = tempfile::tempdir().expect("temp dir");
    let cases: [RefusalCase; 8] = [
        ("fabric.index.json", |error| {
            error.unsupported_loader() == Some("fabric-loader")
        }),
        ("both-loaders.index.json", |error| {
            matches!(error, PackError::LoaderAmbiguous { .. })
        }),
        ("format-version-2.index.json", |error| {
            matches!(
                error,
                PackError::ManifestUnsupportedVersion { found: 2, .. }
            )
        }),
        ("path-traversal.index.json", |error| {
            matches!(error, PackError::EntryPathUnsafe { .. })
        }),
        ("launcher-metadata.index.json", |error| {
            matches!(error, PackError::EntryReserved { .. })
        }),
        ("unlisted-host.index.json", |error| {
            matches!(error, PackError::DownloadUrlRejected { .. })
        }),
        ("plaintext-download.index.json", |error| {
            matches!(error, PackError::DownloadUrlRejected { .. })
        }),
        ("no-hash.index.json", |error| {
            matches!(error, PackError::FileHashesMissing { .. })
        }),
    ];

    for (fixture, expected) in cases {
        let archive = workspace.path().join(format!("{fixture}.mrpack"));
        build_archive(&archive, &read_fixture(&format!("invalid/{fixture}")), &[]);

        let error = analyze(&archive, &PackPolicy::modrinth())
            .map(|_| ())
            .expect_err(&format!("{fixture} must be refused"));
        assert!(
            expected(&error),
            "{fixture} was refused for the wrong reason: {error:?}"
        );
        assert!(
            ephervex_i18n::has(error.message_id()),
            "{fixture}: no message for {}",
            error.message_id()
        );
    }
}

#[tokio::test]
async fn a_pack_whose_archive_carries_a_traversing_override_is_refused_before_it_writes() {
    // The index is clean and the *archive* is hostile, which is the other half
    // of the archive trust boundary and the one an index-only check would miss.
    let workspace = tempfile::tempdir().expect("temp dir");
    let archive = workspace.path().join("hostile.mrpack");
    let file = std::fs::File::create(&archive).expect("create");
    let mut writer = zip::ZipWriter::new(file);
    let options: zip::write::FileOptions<'_, ()> =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
    writer
        .start_file("modrinth.index.json", options)
        .expect("start");
    writer
        .write_all(&read_fixture("invalid/fabric.index.json"))
        .expect("write");
    writer
        .start_file("overrides/../../../../tmp/ephervex-escape", options)
        .expect("start");
    writer.write_all(b"pwned").expect("write");
    writer.finish().expect("finish");

    // The loader refusal comes first, which is the right order: nothing is
    // extracted from a pack that cannot be reproduced anyway.
    let error = analyze(&archive, &PackPolicy::modrinth())
        .map(|_| ())
        .expect_err("refused");
    assert_eq!(error.message_id(), "error-pack-loader-unsupported");

    // ...and extracting that layer directly is refused on its own merits.
    let mut opened =
        PackArchive::open(&archive, ExtractionLimits::default()).expect("open the archive");
    let error = opened
        .extract_prefix("overrides", &workspace.path().join("out"))
        .expect_err("must refuse");
    assert!(matches!(error, PackError::EntryPathUnsafe { .. }));
    assert!(!Path::new("/tmp/ephervex-escape").exists());
}
