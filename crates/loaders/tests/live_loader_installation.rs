//! Opt-in production smoke test for the supported loader matrix.
//!
//! Ordinary CI uses reduced official fixtures and synthetic processor JARs.
//! Run this explicitly when installer formats change; it downloads production
//! artifacts and therefore must never be part of the default test run.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use ephervex_download::{CacheLayout, DownloadManager, DownloaderConfig, OperationReporter};
use ephervex_loaders::{
    ClientInstallRequest, LoaderProvider, LoaderRegistry, ServerInstallRequest,
};
use ephervex_minecraft::{
    JvmOptions, LaunchFeatures, LaunchSettings, Platform, PlayerIdentity, RuleContext,
    VersionCatalog,
};
use ephervex_model::LoaderKind;
use tokio_util::sync::CancellationToken;

struct WorkflowCase {
    kind: LoaderKind,
    minecraft: &'static str,
    loader: &'static str,
    java_env: &'static str,
}

#[tokio::test]
#[ignore = "downloads and executes verified official Forge/NeoForge processors"]
async fn production_client_and_server_installers_accept_the_supported_matrix() {
    let cache = std::env::var_os("EPHERVEX_LIVE_LOADER_CACHE").map_or_else(
        || PathBuf::from("/tmp/ephervex-live-loader-installation"),
        PathBuf::from,
    );
    let layout = CacheLayout::new(&cache);
    let manager = DownloadManager::new(DownloaderConfig::default()).expect("download manager");
    let catalog = VersionCatalog::mojang(manager.clone(), layout.clone()).expect("Mojang catalog");
    let registry = LoaderRegistry::new(manager, layout).expect("loader registry");
    let reporter = OperationReporter::silent("live-loader-installation");
    let cancel = CancellationToken::new();
    let context = RuleContext::new(Platform::host());
    let servers = tempfile::tempdir().expect("server targets");

    for case in [
        WorkflowCase {
            kind: LoaderKind::Forge,
            minecraft: "1.12.2",
            loader: "14.23.5.2859",
            java_env: "EPHERVEX_LIVE_LOADER_JAVA_8",
        },
        WorkflowCase {
            kind: LoaderKind::Forge,
            minecraft: "1.20.1",
            loader: "47.4.10",
            java_env: "EPHERVEX_LIVE_LOADER_JAVA_17",
        },
        WorkflowCase {
            kind: LoaderKind::NeoForge,
            minecraft: "26.1.2",
            loader: "26.1.2.94",
            java_env: "EPHERVEX_LIVE_LOADER_JAVA_CURRENT",
        },
    ] {
        let parent = catalog
            .resolve(case.minecraft, &reporter, &cancel)
            .await
            .unwrap_or_else(|error| panic!("resolve Minecraft {}: {error}", case.minecraft));
        let java =
            std::env::var_os(case.java_env).map_or_else(|| PathBuf::from("java"), PathBuf::from);
        let provider = registry.provider(case.kind);
        install_client(
            provider, &case, &parent, &java, &context, &reporter, &cancel,
        )
        .await;

        let target = servers
            .path()
            .join(format!("{}-{}", case.kind, case.minecraft));
        let installed = provider
            .install_server(ServerInstallRequest {
                minecraft: case.minecraft,
                exact_loader: Some(case.loader),
                parent: &parent,
                java: &java,
                target: &target,
                reporter: &reporter,
                cancel: &cancel,
            })
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "install {} {} server {}: {error}",
                    case.kind, case.minecraft, case.loader
                )
            });
        assert_eq!(installed.selection.version.as_deref(), Some(case.loader));
        assert!(installed.root.is_dir());
    }
}

async fn install_client(
    provider: &dyn LoaderProvider,
    case: &WorkflowCase,
    parent: &ephervex_minecraft::VersionMetadata,
    java: &Path,
    context: &RuleContext,
    reporter: &OperationReporter,
    cancel: &CancellationToken,
) {
    let installed = provider
        .install_client(ClientInstallRequest {
            minecraft: case.minecraft,
            exact_loader: Some(case.loader),
            parent,
            java,
            context,
            reporter,
            cancel,
        })
        .await
        .unwrap_or_else(|error| {
            panic!(
                "install {} {} client {}: {error}",
                case.kind, case.minecraft, case.loader
            )
        });
    assert_eq!(installed.selection.version.as_deref(), Some(case.loader));
    assert!(installed.metadata.is_complete_vanilla());
    assert!(
        installed
            .installation
            .plan
            .classpath()
            .iter()
            .all(|path| path.is_file())
    );
    let game_directory = tempfile::tempdir().expect("workflow game directory");
    let command = ephervex_minecraft::build_command(
        &installed.metadata,
        &installed.installation.plan,
        &LaunchSettings {
            java: java.to_path_buf(),
            game_directory: game_directory.path().to_path_buf(),
            identity: PlayerIdentity::offline(
                "LoaderSmokeTest",
                "00000000-0000-3000-8000-000000000007",
            ),
            features: LaunchFeatures::none(),
            jvm: JvmOptions::default(),
        },
    )
    .expect("loader command");
    assert!(!command.main_class.is_empty());
    assert!(command.arguments().all(|argument| !argument.contains("${")));
}
