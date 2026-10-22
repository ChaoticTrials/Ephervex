//! Resource packs, shader packs, and data packs in an imported Modrinth pack:
//! where they land, what the retained recipe records about them, and what a
//! server built later from that recipe receives.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Write as _;
use std::path::{Path, PathBuf};

use ephervex_download::{
    CacheLayout, DomainAllowlist, DownloadManager, DownloaderConfig, HttpConfig, OperationReporter,
};
use ephervex_model::ModProvider;
use ephervex_pack::{
    ArtifactFetch, AuxiliaryKind, AuxiliarySource, DataPackOwner, ExtractionLimits,
    MaterializeRequest, OpenArtifacts, OptionalSelection, OverrideScope, PackPolicy, PackSide,
    ServerMaterializeRequest, SideRequirement, UnknownServerFiles, analyze, materialize,
    materialize_server_recipe, retained_recipe,
};
use sha2::Digest as _;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path as path_matcher};
use wiremock::{Mock, MockServer, ResponseTemplate};

const FAITHFUL: &[u8] = b"resource pack bytes";
const COMPLEMENTARY: &[u8] = b"shader pack bytes";
const DECLARED_DATAPACK: &[u8] = b"declared data pack bytes";

struct Declared {
    path: &'static str,
    body: &'static [u8],
    env: &'static str,
}

const DECLARED: &[Declared] = &[
    Declared {
        path: "resourcepacks/Faithful.zip",
        body: FAITHFUL,
        env: r#"{"client": "required", "server": "unsupported"}"#,
    },
    Declared {
        path: "shaderpacks/Complementary.zip",
        body: COMPLEMENTARY,
        env: r#"{"client": "required", "server": "unsupported"}"#,
    },
    Declared {
        path: "datapacks/declared.zip",
        body: DECLARED_DATAPACK,
        env: r#"{"client": "required", "server": "required"}"#,
    },
];

fn index(origin: &str) -> String {
    let files: Vec<String> = DECLARED
        .iter()
        .map(|declared| {
            let sha1 = hex::encode(sha1::Sha1::digest(declared.body));
            let sha512 = hex::encode(sha2::Sha512::digest(declared.body));
            format!(
                r#"{{"path": "{path}", "hashes": {{"sha1": "{sha1}", "sha512": "{sha512}"}}, "env": {env}, "downloads": ["{origin}/data/AbCd1234/versions/WxYz5678/{name}"], "fileSize": {size}}}"#,
                path = declared.path,
                env = declared.env,
                name = Path::new(declared.path).file_name().unwrap().to_str().unwrap(),
                size = declared.body.len(),
            )
        })
        .collect();
    format!(
        r#"{{"formatVersion": 1, "game": "minecraft", "versionId": "1.0.0", "name": "Auxiliary Pack", "files": [{}], "dependencies": {{"minecraft": "1.20.1", "neoforge": "47.1.106"}}}}"#,
        files.join(",")
    )
}

fn write_archive(destination: &Path, index: &str, entries: &[(&str, &[u8])]) {
    let file = std::fs::File::create(destination).expect("create archive");
    let mut writer = zip::ZipWriter::new(file);
    let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
    writer
        .start_file("modrinth.index.json", options)
        .expect("start index");
    writer.write_all(index.as_bytes()).expect("write index");
    for (name, body) in entries {
        writer.start_file(*name, options).expect("start entry");
        writer.write_all(body).expect("write entry");
    }
    writer.finish().expect("finish archive");
}

const OVERRIDES: &[(&str, &[u8])] = &[
    (
        "overrides/resourcepacks/Shipped.zip",
        b"shipped resource pack",
    ),
    ("overrides/resourcepacks/Folder/pack.mcmeta", b"{}"),
    ("overrides/resourcepacks/Folder/assets/a.json", b"{}"),
    ("overrides/shaderpacks/Shipped.zip.txt", b"iris options"),
    (
        "overrides/saves/Test World/datapacks/tweaks.zip",
        b"world data pack",
    ),
    ("overrides/datapacks/loose.zip", b"worldless data pack"),
    (
        "client-overrides/resourcepacks/ClientOnly.zip",
        b"client only",
    ),
    (
        "server-overrides/world/datapacks/server-tweaks/pack.mcmeta",
        b"{}",
    ),
    (
        "server-overrides/world/datapacks/server-tweaks/data/a.json",
        b"{}",
    ),
];

async fn serve(server: &MockServer) {
    for declared in DECLARED {
        let name = Path::new(declared.path)
            .file_name()
            .unwrap()
            .to_str()
            .unwrap();
        Mock::given(method("GET"))
            .and(path_matcher(format!(
                "/data/AbCd1234/versions/WxYz5678/{name}"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(declared.body))
            .mount(server)
            .await;
    }
}

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

struct Imported {
    game: PathBuf,
    source: PathBuf,
    cache: CacheLayout,
}

async fn import_client(workspace: &Path, server: &MockServer) -> Imported {
    serve(server).await;
    let archive = workspace.join("auxiliary.mrpack");
    write_archive(&archive, &index(&server.uri()), OVERRIDES);
    let (opened, analysis) = analyze(&archive, &loopback_policy(server)).expect("analyze");
    let imported = Imported {
        game: workspace.join("instance/minecraft"),
        source: workspace.join("instance/.launcher/source"),
        cache: CacheLayout::new(workspace.join("cache")),
    };
    materialize(MaterializeRequest {
        archive: opened,
        analysis: &analysis,
        side: PackSide::Client,
        selection: &OptionalSelection::none(),
        game_dir: &imported.game,
        source_dir: &imported.source,
        manager: &loopback_manager(),
        cache: &imported.cache,
        credentials: &OpenArtifacts,
        reporter: &OperationReporter::silent("auxiliary-import"),
        cancel: &CancellationToken::new(),
    })
    .await
    .expect("materialize");
    imported
}

#[tokio::test]
async fn declared_and_shipped_packs_land_at_their_logical_paths_in_a_client() {
    let server = MockServer::start().await;
    let workspace = tempfile::tempdir().expect("temp dir");
    let imported = import_client(workspace.path(), &server).await;

    for (path, body) in [
        ("resourcepacks/Faithful.zip", FAITHFUL),
        ("shaderpacks/Complementary.zip", COMPLEMENTARY),
        ("datapacks/declared.zip", DECLARED_DATAPACK),
    ] {
        assert_eq!(
            std::fs::read(imported.game.join(path)).unwrap(),
            body,
            "{path}"
        );
    }
    for path in [
        "resourcepacks/Shipped.zip",
        "resourcepacks/Folder/pack.mcmeta",
        "resourcepacks/ClientOnly.zip",
        "saves/Test World/datapacks/tweaks.zip",
        "datapacks/loose.zip",
    ] {
        assert!(imported.game.join(path).is_file(), "{path}");
    }
    assert!(!imported.game.join("world/datapacks").exists());
}

#[test]
fn declared_packs_keep_their_provider_identity_digests_size_and_sides() {
    let parsed = ephervex_pack::modrinth::parse(
        index("https://cdn.modrinth.com").as_bytes(),
        &DomainAllowlist::modrinth_pack_defaults(),
        None,
        &ExtractionLimits::default(),
    )
    .expect("parse");
    let faithful = parsed
        .recipe
        .files
        .iter()
        .find(|file| file.logical_path == Path::new("resourcepacks/Faithful.zip"))
        .expect("the declared resource pack is recorded");
    let identity = faithful.identity.as_ref().expect("provider identity");
    assert_eq!(identity.provider, Some(ModProvider::Modrinth));
    assert_eq!(identity.project_id.as_deref(), Some("AbCd1234"));
    assert_eq!(identity.version_id.as_deref(), Some("WxYz5678"));
    assert_eq!(faithful.hashes.len(), 2);
    assert_eq!(faithful.size, Some(FAITHFUL.len() as u64));
    assert!(!faithful.downloads.is_empty());
    assert_eq!(faithful.client, SideRequirement::Required);
    assert_eq!(faithful.server, SideRequirement::Unsupported);
    assert!(
        parsed.recipe.files.iter().all(|file| !file.is_mod_file()),
        "no auxiliary pack is recorded as a mod"
    );
}

#[tokio::test]
async fn a_client_lists_each_pack_with_its_source_and_owning_world() {
    let server = MockServer::start().await;
    let workspace = tempfile::tempdir().expect("temp dir");
    let imported = import_client(workspace.path(), &server).await;
    let recipe = retained_recipe(&imported.source).expect("retained recipe");

    let listed: Vec<(String, AuxiliaryKind, Option<DataPackOwner>, bool)> = recipe
        .auxiliary_content(PackSide::Client)
        .into_iter()
        .map(|content| {
            (
                content.placement.entry.to_string_lossy().into_owned(),
                content.placement.kind,
                content.placement.owner,
                matches!(content.source, AuxiliarySource::Declared(_)),
            )
        })
        .collect();
    let world = Some(DataPackOwner::SavedWorld("Test World".to_owned()));
    assert_eq!(
        listed,
        vec![
            (
                "resourcepacks/ClientOnly.zip".into(),
                AuxiliaryKind::ResourcePack,
                None,
                false
            ),
            (
                "resourcepacks/Faithful.zip".into(),
                AuxiliaryKind::ResourcePack,
                None,
                true
            ),
            (
                "resourcepacks/Folder".into(),
                AuxiliaryKind::ResourcePack,
                None,
                false
            ),
            (
                "resourcepacks/Shipped.zip".into(),
                AuxiliaryKind::ResourcePack,
                None,
                false
            ),
            (
                "shaderpacks/Complementary.zip".into(),
                AuxiliaryKind::ShaderPack,
                None,
                true
            ),
            (
                "saves/Test World/datapacks/tweaks.zip".into(),
                AuxiliaryKind::DataPack,
                world,
                false
            ),
            (
                "datapacks/declared.zip".into(),
                AuxiliaryKind::DataPack,
                Some(DataPackOwner::NoWorld),
                true
            ),
            (
                "datapacks/loose.zip".into(),
                AuxiliaryKind::DataPack,
                Some(DataPackOwner::NoWorld),
                false
            ),
        ],
        "the options file beside the shader packs is not a pack, and the server-only data pack is not listed for a client"
    );
}

#[tokio::test]
async fn a_server_built_from_the_retained_recipe_keeps_each_data_pack_in_its_own_world_folder() {
    let server = MockServer::start().await;
    let workspace = tempfile::tempdir().expect("temp dir");
    let imported = import_client(workspace.path(), &server).await;
    let recipe = retained_recipe(&imported.source).expect("retained recipe");

    let shipped_scopes: Vec<(String, OverrideScope)> = recipe
        .auxiliary_content(PackSide::Server)
        .into_iter()
        .filter_map(|content| match content.source {
            AuxiliarySource::Shipped(scope) => Some((
                content.placement.entry.to_string_lossy().into_owned(),
                scope,
            )),
            AuxiliarySource::Declared(_) => None,
        })
        .collect();
    assert!(shipped_scopes.contains(&(
        "world/datapacks/server-tweaks".to_owned(),
        OverrideScope::Server
    )));
    assert!(
        !shipped_scopes
            .iter()
            .any(|(path, _)| path == "resourcepacks/ClientOnly.zip"),
        "a client-only layer's packs are not the server's"
    );

    let game = workspace.path().join("server/game");
    let server_source = workspace.path().join("server/.launcher/source");
    materialize_server_recipe(ServerMaterializeRequest {
        recipe: &recipe,
        unknown_server_files: &UnknownServerFiles::only(OptionalSelection::none()),
        game_dir: &game,
        client_game_dir: &imported.game,
        client_put_aside_dir: &workspace.path().join("put-aside"),
        client_source_dir: &imported.source,
        server_source_dir: &server_source,
        reserved: &[],
        fetch: ArtifactFetch {
            manager: &loopback_manager(),
            cache: &imported.cache,
            credentials: &OpenArtifacts,
            reporter: &OperationReporter::silent("auxiliary-server"),
            cancel: &CancellationToken::new(),
        },
    })
    .await
    .expect("reconstruct");

    assert!(
        game.join("world/datapacks/server-tweaks/pack.mcmeta")
            .is_file()
    );
    assert!(
        game.join("world/datapacks/server-tweaks/data/a.json")
            .is_file()
    );
    assert_eq!(
        std::fs::read(game.join("saves/Test World/datapacks/tweaks.zip")).unwrap(),
        b"world data pack",
        "a common layer's world-scoped data pack keeps the path the pack gave it"
    );
    assert_eq!(
        std::fs::read(game.join("datapacks/declared.zip")).unwrap(),
        DECLARED_DATAPACK
    );
    assert!(!game.join("resourcepacks/Faithful.zip").exists());
    assert!(!game.join("shaderpacks/Complementary.zip").exists());
    assert!(!game.join("resourcepacks/ClientOnly.zip").exists());
}
