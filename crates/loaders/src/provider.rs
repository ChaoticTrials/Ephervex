use std::path::Path;

use async_trait::async_trait;
use ephervex_download::{CacheLayout, DownloadManager, Freshness, OperationReporter};
use ephervex_java::{JavaVersion, LEGACY_FORGE_MAX_JAVA_8_UPDATE, VersionBound};
use ephervex_minecraft::{RuleContext, VersionMetadata};
use ephervex_model::{LoaderKind, LoaderVersionMode};
use tokio_util::sync::CancellationToken;

use crate::discovery::{LoaderCatalog, LoaderVersionListing};
use crate::install::InstallerEngine;
use crate::{InstalledClient, InstalledServer, LoaderError, LoaderResult};

#[derive(Debug)]
pub struct ClientInstallRequest<'a> {
    pub minecraft: &'a str,
    pub exact_loader: Option<&'a str>,
    pub parent: &'a VersionMetadata,
    pub java: &'a Path,
    pub context: &'a RuleContext,
    pub reporter: &'a OperationReporter,
    pub cancel: &'a CancellationToken,
}

#[derive(Debug)]
pub struct ServerInstallRequest<'a> {
    pub minecraft: &'a str,
    pub exact_loader: Option<&'a str>,
    pub parent: &'a VersionMetadata,
    pub java: &'a Path,
    pub target: &'a Path,
    pub reporter: &'a OperationReporter,
    pub cancel: &'a CancellationToken,
}

#[async_trait]
pub trait LoaderProvider: std::fmt::Debug + Send + Sync {
    fn kind(&self) -> LoaderKind;

    async fn list_versions(
        &self,
        minecraft: &str,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> LoaderResult<LoaderVersionListing>;

    async fn resolve_version(
        &self,
        minecraft: &str,
        mode: LoaderVersionMode,
        exact: Option<&str>,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> LoaderResult<String> {
        if mode == LoaderVersionMode::Exact {
            return crate::provider::exact(self.kind(), exact).map(str::to_owned);
        }
        self.list_versions(minecraft, reporter, cancel)
            .await?
            .resolve(mode, exact)
    }

    fn java_rules(&self, minecraft: &str, exact: Option<&str>) -> Vec<VersionBound>;

    async fn install_client(
        &self,
        request: ClientInstallRequest<'_>,
    ) -> LoaderResult<InstalledClient>;

    async fn install_server(
        &self,
        request: ServerInstallRequest<'_>,
    ) -> LoaderResult<InstalledServer>;
}

#[derive(Debug, Clone)]
pub struct VanillaProvider {
    engine: InstallerEngine,
}

impl VanillaProvider {
    pub fn new(manager: DownloadManager, layout: CacheLayout) -> Self {
        Self {
            engine: InstallerEngine::new(manager, layout),
        }
    }
}

#[async_trait]
impl LoaderProvider for VanillaProvider {
    fn kind(&self) -> LoaderKind {
        LoaderKind::Vanilla
    }

    async fn list_versions(
        &self,
        minecraft: &str,
        _reporter: &OperationReporter,
        _cancel: &CancellationToken,
    ) -> LoaderResult<LoaderVersionListing> {
        Ok(LoaderVersionListing {
            loader: LoaderKind::Vanilla,
            minecraft: minecraft.to_owned(),
            builds: Vec::new(),
            latest: None,
            stable: None,
            freshness: Freshness::Cached,
        })
    }

    async fn resolve_version(
        &self,
        _minecraft: &str,
        _mode: LoaderVersionMode,
        _exact: Option<&str>,
        _reporter: &OperationReporter,
        _cancel: &CancellationToken,
    ) -> LoaderResult<String> {
        Ok(String::new())
    }

    fn java_rules(&self, _minecraft: &str, _exact: Option<&str>) -> Vec<VersionBound> {
        Vec::new()
    }

    async fn install_client(
        &self,
        request: ClientInstallRequest<'_>,
    ) -> LoaderResult<InstalledClient> {
        self.engine
            .install_vanilla_client(
                request.parent,
                request.context,
                request.reporter,
                request.cancel,
            )
            .await
    }

    async fn install_server(
        &self,
        request: ServerInstallRequest<'_>,
    ) -> LoaderResult<InstalledServer> {
        self.engine
            .install_vanilla_server(
                request.parent,
                request.target,
                request.reporter,
                request.cancel,
            )
            .await
    }
}

#[derive(Debug, Clone)]
pub struct ForgeProvider {
    catalog: LoaderCatalog,
    engine: InstallerEngine,
}

impl ForgeProvider {
    pub fn new(manager: DownloadManager, layout: CacheLayout) -> LoaderResult<Self> {
        Ok(Self {
            catalog: LoaderCatalog::new(manager.clone(), layout.clone())?,
            engine: InstallerEngine::new(manager, layout),
        })
    }
}

#[async_trait]
impl LoaderProvider for ForgeProvider {
    fn kind(&self) -> LoaderKind {
        LoaderKind::Forge
    }

    async fn list_versions(
        &self,
        minecraft: &str,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> LoaderResult<LoaderVersionListing> {
        self.catalog.forge(minecraft, reporter, cancel).await
    }

    fn java_rules(&self, minecraft: &str, _exact: Option<&str>) -> Vec<VersionBound> {
        if minecraft == "1.12.2" {
            vec![
                VersionBound::new("forge-legacy-java8")
                    .major(8)
                    .at_most(JavaVersion::new(8, 0, LEGACY_FORGE_MAX_JAVA_8_UPDATE)),
            ]
        } else {
            Vec::new()
        }
    }

    async fn install_client(
        &self,
        request: ClientInstallRequest<'_>,
    ) -> LoaderResult<InstalledClient> {
        let exact = exact(LoaderKind::Forge, request.exact_loader)?;
        self.engine
            .install_loader_client(
                LoaderKind::Forge,
                request.minecraft,
                exact,
                request.parent,
                request.java,
                request.context,
                request.reporter,
                request.cancel,
            )
            .await
    }

    async fn install_server(
        &self,
        request: ServerInstallRequest<'_>,
    ) -> LoaderResult<InstalledServer> {
        let exact = exact(LoaderKind::Forge, request.exact_loader)?;
        self.engine
            .install_loader_server(
                LoaderKind::Forge,
                request.minecraft,
                exact,
                request.parent,
                request.java,
                request.target,
                request.reporter,
                request.cancel,
            )
            .await
    }
}

#[derive(Debug, Clone)]
pub struct NeoForgeProvider {
    catalog: LoaderCatalog,
    engine: InstallerEngine,
}

impl NeoForgeProvider {
    pub fn new(manager: DownloadManager, layout: CacheLayout) -> LoaderResult<Self> {
        Ok(Self {
            catalog: LoaderCatalog::new(manager.clone(), layout.clone())?,
            engine: InstallerEngine::new(manager, layout),
        })
    }
}

#[async_trait]
impl LoaderProvider for NeoForgeProvider {
    fn kind(&self) -> LoaderKind {
        LoaderKind::NeoForge
    }

    async fn list_versions(
        &self,
        minecraft: &str,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> LoaderResult<LoaderVersionListing> {
        self.catalog.neoforge(minecraft, reporter, cancel).await
    }

    fn java_rules(&self, _minecraft: &str, _exact: Option<&str>) -> Vec<VersionBound> {
        Vec::new()
    }

    async fn install_client(
        &self,
        request: ClientInstallRequest<'_>,
    ) -> LoaderResult<InstalledClient> {
        let exact = exact(LoaderKind::NeoForge, request.exact_loader)?;
        self.engine
            .install_loader_client(
                LoaderKind::NeoForge,
                request.minecraft,
                exact,
                request.parent,
                request.java,
                request.context,
                request.reporter,
                request.cancel,
            )
            .await
    }

    async fn install_server(
        &self,
        request: ServerInstallRequest<'_>,
    ) -> LoaderResult<InstalledServer> {
        let exact = exact(LoaderKind::NeoForge, request.exact_loader)?;
        self.engine
            .install_loader_server(
                LoaderKind::NeoForge,
                request.minecraft,
                exact,
                request.parent,
                request.java,
                request.target,
                request.reporter,
                request.cancel,
            )
            .await
    }
}

#[derive(Debug)]
pub struct LoaderRegistry {
    vanilla: VanillaProvider,
    forge: ForgeProvider,
    neoforge: NeoForgeProvider,
}

impl LoaderRegistry {
    pub fn new(manager: DownloadManager, layout: CacheLayout) -> LoaderResult<Self> {
        Ok(Self {
            vanilla: VanillaProvider::new(manager.clone(), layout.clone()),
            forge: ForgeProvider::new(manager.clone(), layout.clone())?,
            neoforge: NeoForgeProvider::new(manager, layout)?,
        })
    }

    #[must_use]
    pub fn provider(&self, kind: LoaderKind) -> &dyn LoaderProvider {
        match kind {
            LoaderKind::Vanilla => &self.vanilla,
            LoaderKind::Forge => &self.forge,
            LoaderKind::NeoForge => &self.neoforge,
        }
    }
}

fn exact(kind: LoaderKind, value: Option<&str>) -> LoaderResult<&str> {
    value
        .filter(|value| !value.trim().is_empty())
        .ok_or(LoaderError::ExactVersionRequired { loader: kind })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

    use super::*;
    use ephervex_java::{
        JavaCompatibilityRule, JavaOrigin, JavaRuntime, JavaSourceKind, RuleVerdict,
    };
    use std::path::PathBuf;

    #[test]
    fn legacy_forge_registers_the_java8_update_ceiling() {
        let manager =
            DownloadManager::new(ephervex_download::DownloaderConfig::default()).expect("manager");
        let provider = ForgeProvider::new(manager, CacheLayout::new("/tmp/ephervex-loader-test"))
            .expect("provider");
        let rules = provider.java_rules("1.12.2", Some("14.23.5.2859"));
        assert_eq!(rules.len(), 1);
        let runtime = JavaRuntime {
            executable: PathBuf::from("/java"),
            home: None,
            version: JavaVersion::new(8, 0, 321),
            version_string: "1.8.0_321".to_owned(),
            runtime_version: "1.8.0_321".to_owned(),
            vendor: "test".to_owned(),
            arch: "amd64".to_owned(),
            origins: vec![JavaOrigin {
                kind: JavaSourceKind::System,
                path: PathBuf::from("/java"),
            }],
        };
        assert!(matches!(
            rules[0].evaluate(&runtime),
            RuleVerdict::Reject(_)
        ));
        assert!(provider.java_rules("1.16.5", None).is_empty());
    }

    #[test]
    fn modded_installation_requires_an_exact_version() {
        assert!(matches!(
            exact(LoaderKind::Forge, None),
            Err(LoaderError::ExactVersionRequired {
                loader: LoaderKind::Forge
            })
        ));
    }

    #[tokio::test]
    async fn exact_resolution_does_not_depend_on_discovery() {
        let manager =
            DownloadManager::new(ephervex_download::DownloaderConfig::default()).expect("manager");
        let provider = ForgeProvider::new(manager, CacheLayout::new("/tmp/ephervex-loader-test"))
            .expect("provider");
        let resolved = provider
            .resolve_version(
                "1.20.1",
                LoaderVersionMode::Exact,
                Some("47.4.10"),
                &OperationReporter::silent("exact"),
                &CancellationToken::new(),
            )
            .await
            .expect("exact selection must be local");
        assert_eq!(resolved, "47.4.10");
    }
}
