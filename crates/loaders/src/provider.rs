use std::path::Path;

use async_trait::async_trait;
use ephervex_download::{CacheLayout, DownloadManager, Freshness, OperationReporter};
use ephervex_java::{JavaVersion, LEGACY_FORGE_MAX_JAVA_8_UPDATE, VersionBound};
use ephervex_minecraft::{RuleContext, VersionMetadata};
use ephervex_model::{LoaderKind, LoaderVersionMode};
use tokio_util::sync::CancellationToken;

use crate::discovery::{LoaderCatalog, LoaderVersionListing, require_supported_installer_format};
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
// `async_trait` marks each boxed future it returns `#[must_use]`.
#[allow(clippy::double_must_use)]
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
            let exact = crate::provider::exact(self.kind(), exact)?;
            require_supported_installer_format(self.kind(), minecraft, exact)?;
            return Ok(exact.to_owned());
        }
        self.list_versions(minecraft, reporter, cancel)
            .await?
            .resolve(mode, exact)
    }

    /// The runtime constraints this loader places on top of the Minecraft
    /// version's own requirement.
    ///
    /// `required_major` is the feature release Minecraft itself asks for,
    /// resolved from its metadata. A loader caveat is often a fact about the
    /// *era* rather than about one release id, and the era is what that number
    /// names; deriving it here from the version string instead would be a
    /// second, worse answer to a question already settled.
    fn java_rules(
        &self,
        minecraft: &str,
        exact: Option<&str>,
        required_major: u32,
    ) -> Vec<VersionBound>;

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

    fn java_rules(
        &self,
        _minecraft: &str,
        _exact: Option<&str>,
        _required_major: u32,
    ) -> Vec<VersionBound> {
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

    /// Every Forge that runs on Java 8 needs a Java 8 that is not too late.
    ///
    /// Keyed on the required feature release rather than on a list of
    /// Minecraft versions, because that is the actual shape of the caveat:
    /// Forge stopped needing Java 8 at 1.17, and every release before that is
    /// affected. Two different launch stacks are involved -- LaunchWrapper up
    /// to 1.12.2, ModLauncher from 1.13 -- and late Java 8 updates broke both,
    /// so there is no line to draw between them.
    fn java_rules(
        &self,
        _minecraft: &str,
        _exact: Option<&str>,
        required_major: u32,
    ) -> Vec<VersionBound> {
        if required_major == 8 {
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

    fn java_rules(
        &self,
        _minecraft: &str,
        _exact: Option<&str>,
        _required_major: u32,
    ) -> Vec<VersionBound> {
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

    fn forge() -> ForgeProvider {
        let manager =
            DownloadManager::new(ephervex_download::DownloaderConfig::default()).expect("manager");
        ForgeProvider::new(manager, CacheLayout::new("/tmp/ephervex-loader-test"))
            .expect("provider")
    }

    fn java(version: JavaVersion) -> JavaRuntime {
        JavaRuntime {
            executable: PathBuf::from("/java"),
            home: None,
            version,
            version_string: version.to_string(),
            runtime_version: version.to_string(),
            vendor: "test".to_owned(),
            arch: "amd64".to_owned(),
            origins: vec![JavaOrigin {
                kind: JavaSourceKind::System,
                path: PathBuf::from("/java"),
            }],
        }
    }

    /// The ceiling belongs to the era, not to one release id. Every Forge that
    /// runs on Java 8 is affected, and 1.12.2 is only the oldest of them.
    #[test]
    fn every_java8_forge_registers_the_update_ceiling() {
        let provider = forge();
        // A 1.13-and-later release, which used to be left unconstrained and so
        // was handed whatever late 8u the machine had.
        for (minecraft, exact) in [
            ("1.12.2", Some("14.23.5.2859")),
            ("1.16.3", Some("34.1.42")),
            ("1.16.5", None),
        ] {
            let rules = provider.java_rules(minecraft, exact, 8);
            assert_eq!(rules.len(), 1, "{minecraft} is a Java 8 Forge");
            assert!(
                matches!(
                    rules[0].evaluate(&java(JavaVersion::new(
                        8,
                        0,
                        LEGACY_FORGE_MAX_JAVA_8_UPDATE + 1
                    ))),
                    RuleVerdict::Reject(_)
                ),
                "{minecraft} must refuse an update past the ceiling"
            );
            assert!(
                rules[0]
                    .evaluate(&java(JavaVersion::new(
                        8,
                        0,
                        LEGACY_FORGE_MAX_JAVA_8_UPDATE
                    )))
                    .is_accepted(),
                "{minecraft} must accept the ceiling itself"
            );
        }
    }

    /// A Forge that does not run on Java 8 has none of this to worry about, and
    /// a rule demanding major 8 would refuse the runtime it actually needs.
    #[test]
    fn a_forge_past_java8_is_left_unconstrained() {
        assert!(forge().java_rules("1.20.1", Some("47.4.10"), 17).is_empty());
        assert!(forge().java_rules("1.18.2", None, 17).is_empty());
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

    #[tokio::test]
    async fn an_exact_pin_with_the_older_installer_layout_is_refused_locally() {
        let manager =
            DownloadManager::new(ephervex_download::DownloaderConfig::default()).expect("manager");
        let provider = ForgeProvider::new(manager, CacheLayout::new("/tmp/ephervex-loader-test"))
            .expect("provider");
        let error = provider
            .resolve_version(
                "1.12.2",
                LoaderVersionMode::Exact,
                Some("14.23.5.2847"),
                &OperationReporter::silent("exact"),
                &CancellationToken::new(),
            )
            .await
            .expect_err("the older layout is not installed");
        assert!(matches!(
            error,
            LoaderError::UnsupportedInstallerFormat { .. }
        ));
    }
}
