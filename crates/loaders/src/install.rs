use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use ephervex_download::{
    CacheLayout, ContentStore, DownloadManager, DownloadRequest, FileExtension, HashVerifier,
    MavenCoordinate, MetadataCache, MetadataKey, OperationReporter, UrlPolicyError,
};
use ephervex_minecraft::{
    Installation, Platform, RuleContext, VanillaInstaller, VersionMetadata, rules_allow,
};
use ephervex_model::{Hash, HashAlgorithm, LoaderKind, LoaderSelection};
use reqwest::Url;
use tokio::fs;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;
use tracing::{debug, instrument};
use zip::ZipArchive;

use crate::profile::{InstallProfile, InstallSide, Processor};
use crate::{LoaderError, LoaderResult};

const FORGE_MAVEN: &str = "https://maven.minecraftforge.net";
const NEOFORGE_MAVEN: &str = "https://maven.neoforged.net/releases";
const MAX_PROFILE_BYTES: u64 = 16 * 1024 * 1024;
static WORKSPACE_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
pub struct InstalledClient {
    pub selection: LoaderSelection,
    pub metadata: VersionMetadata,
    pub installation: Installation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerLaunch {
    Jar { jar: PathBuf },
    ArgFile { file: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledServer {
    pub selection: LoaderSelection,
    pub minecraft: String,
    pub root: PathBuf,
    pub launch: ServerLaunch,
}

#[derive(Debug)]
struct InstallerBundle {
    installer: PathBuf,
    profile: InstallProfile,
    child: VersionMetadata,
}

#[derive(Debug, Clone)]
pub(crate) struct InstallerEngine {
    manager: DownloadManager,
    layout: CacheLayout,
}

impl InstallerEngine {
    pub(crate) fn new(manager: DownloadManager, layout: CacheLayout) -> Self {
        Self { manager, layout }
    }

    pub(crate) async fn install_vanilla_client(
        &self,
        metadata: &VersionMetadata,
        context: &RuleContext,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> LoaderResult<InstalledClient> {
        let installer = VanillaInstaller::new(self.manager.clone(), self.layout.clone());
        let plan = installer.plan(metadata, context)?;
        let installation = installer.install(plan, reporter, cancel).await?;
        Ok(InstalledClient {
            selection: LoaderSelection::vanilla(),
            metadata: metadata.clone(),
            installation,
        })
    }

    pub(crate) async fn install_vanilla_server(
        &self,
        metadata: &VersionMetadata,
        target: &Path,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> LoaderResult<InstalledServer> {
        ensure_empty_target(target).await?;
        let staging = server_staging(target);
        remove_tree(&staging).await;
        fs::create_dir_all(&staging)
            .await
            .map_err(|source| io_error(&staging, source))?;
        let artifact = metadata.server_jar()?.clone();
        let jar = staging.join(format!("minecraft_server.{}.jar", metadata.id));
        let request = DownloadRequest::parse(&artifact.url, &jar)?
            .verifying(artifact.sha1)
            .sized(artifact.size)
            .labeled(format!("minecraft {} server", metadata.id));
        self.manager
            .download(&request, reporter, cancel)
            .await
            .map_err(|source| LoaderError::Download {
                artifact: format!("minecraft {} server", metadata.id),
                source: Box::new(source),
            })?;
        fs::rename(&staging, target)
            .await
            .map_err(|source| io_error(target, source))?;
        Ok(InstalledServer {
            selection: LoaderSelection::vanilla(),
            minecraft: metadata.id.clone(),
            root: target.to_path_buf(),
            launch: ServerLaunch::Jar {
                jar: target.join(jar.file_name().unwrap_or_default()),
            },
        })
    }

    #[instrument(skip(self, parent, java, context, reporter, cancel))]
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn install_loader_client(
        &self,
        kind: LoaderKind,
        minecraft: &str,
        exact: &str,
        parent: &VersionMetadata,
        java: &Path,
        context: &RuleContext,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> LoaderResult<InstalledClient> {
        let bundle = self
            .fetch_bundle(kind, minecraft, exact, reporter, cancel)
            .await?;
        Self::validate_bundle(kind, minecraft, exact, &bundle, context)?;

        let vanilla = self
            .install_vanilla_client(parent, context, reporter, cancel)
            .await?;
        self.install_profile_libraries(&bundle, reporter, cancel)
            .await?;
        self.install_installer_declared_libraries(&bundle).await?;

        if bundle.profile.is_processor_driven() {
            let workspace = self.workspace("client");
            self.prepare_workspace(&workspace, &bundle.profile).await?;
            self.prepare_mojang_mappings(
                &bundle.profile,
                InstallSide::Client,
                parent,
                &workspace,
                reporter,
                cancel,
            )
            .await?;
            let result = self
                .run_processors(
                    &bundle,
                    InstallSide::Client,
                    &workspace,
                    &vanilla.installation.plan.client_jar.path,
                    java,
                    cancel,
                )
                .await;
            if let Err(error) = result {
                remove_tree(&workspace).await;
                return Err(error);
            }
            self.publish_libraries(&workspace.join("libraries")).await?;
            remove_tree(&workspace).await;
        }

        let mut metadata = VersionMetadata::inherit(parent, &bundle.child)?;
        self.mark_preinstalled_artifacts(&mut metadata).await?;
        let installer = VanillaInstaller::new(self.manager.clone(), self.layout.clone());
        let plan = installer.plan(&metadata, context)?;
        let installation = installer.install(plan, reporter, cancel).await?;
        Ok(InstalledClient {
            selection: LoaderSelection::exact(kind, exact),
            metadata,
            installation,
        })
    }

    #[instrument(skip(self, parent, java, reporter, cancel))]
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn install_loader_server(
        &self,
        kind: LoaderKind,
        minecraft: &str,
        exact: &str,
        parent: &VersionMetadata,
        java: &Path,
        target: &Path,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> LoaderResult<InstalledServer> {
        ensure_empty_target(target).await?;
        let bundle = self
            .fetch_bundle(kind, minecraft, exact, reporter, cancel)
            .await?;
        Self::validate_bundle(
            kind,
            minecraft,
            exact,
            &bundle,
            &RuleContext::new(Platform::host()),
        )?;
        self.install_metadata_libraries(&bundle, reporter, cancel)
            .await?;
        self.install_profile_libraries(&bundle, reporter, cancel)
            .await?;
        self.install_installer_declared_libraries(&bundle).await?;

        let staging = server_staging(target);
        remove_tree(&staging).await;
        fs::create_dir_all(&staging)
            .await
            .map_err(|source| io_error(&staging, source))?;

        let server = parent.server_jar()?.clone();
        let server_path = staging.join(format!("minecraft_server.{minecraft}.jar"));
        let request = DownloadRequest::parse(&server.url, &server_path)?
            .verifying(server.sha1)
            .sized(server.size)
            .labeled(format!("minecraft {minecraft} server"));
        self.manager
            .download(&request, reporter, cancel)
            .await
            .map_err(|source| LoaderError::Download {
                artifact: format!("minecraft {minecraft} server"),
                source: Box::new(source),
            })?;

        let launch = if bundle.profile.is_processor_driven() {
            self.prepare_workspace_in(&staging, &bundle.profile).await?;
            self.prepare_mojang_mappings(
                &bundle.profile,
                InstallSide::Server,
                parent,
                &staging,
                reporter,
                cancel,
            )
            .await?;
            let result = self
                .run_processors(
                    &bundle,
                    InstallSide::Server,
                    &staging,
                    &server_path,
                    java,
                    cancel,
                )
                .await;
            if let Err(error) = result {
                remove_tree(&staging).await;
                return Err(error);
            }
            self.publish_libraries(&staging.join("libraries")).await?;
            // Which of the two processor-driven eras this is shows in what the
            // processors produced. The argument-file era (Forge 1.17 onward and
            // every NeoForge build) writes `unix_args.txt`; the ModLauncher era
            // before it patches the game under `libraries` and still starts
            // from the launcher jar the installer ships.
            let relative = modern_server_argfile(kind, minecraft, exact);
            let launch = if fs::try_exists(&staging.join(&relative))
                .await
                .unwrap_or(false)
            {
                ServerLaunch::ArgFile {
                    file: target.join(relative),
                }
            } else {
                match self.stage_profile_jar(&bundle, &staging).await {
                    Ok(jar) => ServerLaunch::Jar {
                        jar: target.join(jar),
                    },
                    Err(error) => {
                        remove_tree(&staging).await;
                        return Err(error);
                    }
                }
            };
            replace_with_shared_libraries(&self.layout, &staging).await?;
            launch
        } else {
            let jar = self.stage_profile_jar(&bundle, &staging).await?;
            link_shared_libraries(&self.layout, &staging).await?;
            ServerLaunch::Jar {
                jar: target.join(jar),
            }
        };

        fs::rename(&staging, target)
            .await
            .map_err(|source| io_error(target, source))?;
        Ok(InstalledServer {
            selection: LoaderSelection::exact(kind, exact),
            minecraft: minecraft.to_owned(),
            root: target.to_path_buf(),
            launch,
        })
    }

    async fn fetch_bundle(
        &self,
        kind: LoaderKind,
        minecraft: &str,
        exact: &str,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> LoaderResult<InstallerBundle> {
        let (coordinate, jar_url) = installer_location(kind, minecraft, exact)?;
        let sidecar_url =
            Url::parse(&format!("{jar_url}.sha1")).map_err(|_| LoaderError::MetadataInvalid {
                provider: "installer",
                detail: "installer sidecar URL is invalid".to_owned(),
            })?;
        let key = installer_sidecar_key(kind, minecraft, exact)?;
        let sidecar = MetadataCache::new(self.layout.clone())
            .fetch_text(&self.manager, &key, &sidecar_url, reporter, cancel)
            .await
            .map_err(|source| LoaderError::MetadataUnavailable {
                provider: "installer",
                source: Box::new(source),
            })?
            .text()
            .map_err(|error| LoaderError::MetadataInvalid {
                provider: "installer",
                detail: error.to_string(),
            })?;
        let digest = sidecar.split_whitespace().next().unwrap_or_default();
        let sha1 =
            Hash::new(HashAlgorithm::Sha1, digest).ok_or_else(|| LoaderError::MetadataInvalid {
                provider: "installer",
                detail: "installer sidecar has no SHA-1".to_owned(),
            })?;
        let jar = FileExtension::new("jar")?;
        let installer = self
            .layout
            .content(ContentStore::InstallerCache, &sha1, Some(&jar));
        let request = DownloadRequest::parse(&jar_url, &installer)?
            .verifying(sha1)
            .labeled(format!("{coordinate}"));
        self.manager
            .download(&request, reporter, cancel)
            .await
            .map_err(|source| LoaderError::Download {
                artifact: format!("{coordinate}"),
                source: Box::new(source),
            })?;

        let archive = installer.clone();
        let read = tokio::task::spawn_blocking(move || {
            Ok::<_, LoaderError>((
                read_zip_entry(&archive, "install_profile.json")?,
                read_zip_entry(&archive, "version.json")?,
            ))
        })
        .await
        .map_err(|error| invalid_installer(&installer, &error.to_string()))??;
        Ok(InstallerBundle {
            installer,
            profile: InstallProfile::parse(&read.0)?,
            child: VersionMetadata::parse(&read.1)?,
        })
    }

    /// Checks the bundle's identity, and its library digests under `context`'s rules.
    fn validate_bundle(
        kind: LoaderKind,
        minecraft: &str,
        exact: &str,
        bundle: &InstallerBundle,
        context: &RuleContext,
    ) -> LoaderResult<()> {
        if bundle.profile.minecraft != minecraft || bundle.child.inherits_from() != Some(minecraft)
        {
            return Err(LoaderError::IdentityMismatch {
                detail: format!(
                    "requested minecraft {minecraft}, profile says {}, child inherits {:?}",
                    bundle.profile.minecraft,
                    bundle.child.inherits_from()
                ),
            });
        }
        let correct_version = match kind {
            LoaderKind::Forge => {
                bundle.profile.version == format!("{minecraft}-forge-{exact}")
                    && bundle.child.id == bundle.profile.version
            }
            LoaderKind::NeoForge => {
                bundle.profile.version == format!("neoforge-{exact}")
                    && bundle.child.id == bundle.profile.version
            }
            LoaderKind::Vanilla => false,
        };
        if !correct_version {
            return Err(LoaderError::IdentityMismatch {
                detail: format!(
                    "requested {kind} {exact}, profile/child are {}/{}",
                    bundle.profile.version, bundle.child.id
                ),
            });
        }
        ensure_consistent_library_digests(bundle, context)
    }

    async fn install_profile_libraries(
        &self,
        bundle: &InstallerBundle,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> LoaderResult<()> {
        // Later entries for a coordinate share its store path, so the first one wins.
        let mut planned = HashSet::new();
        let mut requests = Vec::new();
        let mut names = Vec::new();
        for library in &bundle.profile.libraries {
            if cancel.is_cancelled() {
                return Err(LoaderError::Canceled);
            }
            if !planned.insert(&library.coordinate) {
                continue;
            }
            let destination = self.layout.library(&library.coordinate);
            if library.artifact.url.is_empty() {
                if fs::try_exists(&destination).await.unwrap_or(false) {
                    continue;
                }
                let installer = bundle.installer.clone();
                let entry = format!("maven/{}", library.coordinate.relative_path().display());
                let expected = library.artifact.sha1.clone();
                let size = library.artifact.size;
                tokio::task::spawn_blocking(move || {
                    extract_verified_entry(&installer, &entry, &destination, &expected, size)
                })
                .await
                .map_err(|error| invalid_installer(&bundle.installer, &error.to_string()))??;
            } else {
                // The download manager revalidates a stored copy and applies the transport policy.
                let url = parse_library_url(&library.artifact.url)?;
                requests.push(
                    DownloadRequest::new(url, destination)
                        .verifying(library.artifact.sha1.clone())
                        .sized(library.artifact.size)
                        .labeled(library.name.clone()),
                );
                names.push(library.name.as_str());
            }
        }
        self.download_libraries(&requests, &names, &bundle.child.id, reporter, cancel)
            .await
    }

    /// Fetches libraries as one bounded-concurrency batch, naming the one that failed.
    async fn download_libraries(
        &self,
        requests: &[DownloadRequest],
        names: &[&str],
        version: &str,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> LoaderResult<()> {
        if requests.is_empty() {
            return Ok(());
        }
        reporter.set_step(Some(format!("{version} \u{b7} libraries")));
        self.manager
            .download_all(requests, reporter, cancel)
            .await
            .map_err(|failure| {
                if failure.is_cancellation() {
                    return LoaderError::Canceled;
                }
                let artifact = names
                    .get(failure.index())
                    .map_or_else(|| format!("{version} libraries"), |name| (*name).to_owned());
                LoaderError::Download {
                    artifact,
                    source: Box::new(failure.into_error()),
                }
            })?;
        Ok(())
    }

    /// Downloads every library the loader's own version metadata declares with
    /// a download address into the shared store.
    ///
    /// Forge's server argument files and launcher jar manifests name libraries
    /// only `version.json` declares, and the Forge installer's own server
    /// install downloads them. A client install reaches them through the
    /// vanilla installer instead, which selects them by the same rules.
    async fn install_metadata_libraries(
        &self,
        bundle: &InstallerBundle,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> LoaderResult<()> {
        let context = RuleContext::new(Platform::host());
        // The first entry for a coordinate wins, as it does when a client plans the same list.
        let mut planned = HashSet::new();
        let mut requests = Vec::new();
        let mut names = Vec::new();
        for library in &bundle.child.libraries {
            let Some(artifact) = library.downloads.artifact.as_ref() else {
                continue;
            };
            if !rules_allow(&library.rules, &context) || !planned.insert(&library.coordinate) {
                continue;
            }
            if artifact.url.is_empty() {
                continue;
            }
            // The download manager applies the transport policy, as it does for
            // the client's copy of these libraries.
            let url = parse_library_url(&artifact.url)?;
            requests.push(
                DownloadRequest::new(url, self.layout.library(&library.coordinate))
                    .verifying(artifact.sha1.clone())
                    .sized(artifact.size)
                    .labeled(library.name.clone()),
            );
            names.push(library.name.as_str());
        }
        self.download_libraries(&requests, &names, &bundle.child.id, reporter, cancel)
            .await
    }

    /// Installs every library the loader's own version metadata declares with
    /// no download address, where the installer archive carries the file.
    ///
    /// The ModLauncher era (Forge 1.13 through 1.16.5) declares its launcher
    /// jar exactly this way: `version.json` names
    /// `net.minecraftforge:forge:<minecraft>-<exact>` with an empty URL and a
    /// real digest, the file ships at `maven/...` inside the installer, and no
    /// processor ever writes it. The install profile's own library list names
    /// the `universal` classifier instead, so nothing else would install it and
    /// the launch would fail on a library that was never missing.
    ///
    /// An entry the installer does not carry is left alone. Later eras declare
    /// processor outputs the same way, and those arrive when the processors
    /// run.
    async fn install_installer_declared_libraries(
        &self,
        bundle: &InstallerBundle,
    ) -> LoaderResult<()> {
        for library in &bundle.child.libraries {
            let Some(artifact) = library.downloads.artifact.as_ref() else {
                continue;
            };
            if !artifact.url.is_empty() {
                continue;
            }
            let destination = self.layout.library(&library.coordinate);
            if fs::try_exists(&destination).await.unwrap_or(false) {
                continue;
            }
            let installer = bundle.installer.clone();
            let entry = format!("maven/{}", library.coordinate.relative_path().display());
            let expected = artifact.sha1.clone();
            let size = artifact.size;
            let destination_copy = destination.clone();
            tokio::task::spawn_blocking(move || {
                extract_declared_entry(&installer, &entry, &destination_copy, &expected, size)
            })
            .await
            .map_err(|error| invalid_installer(&bundle.installer, &error.to_string()))??;
        }
        Ok(())
    }

    /// Stages the launcher jar a profile names as its own `path`.
    ///
    /// Every Forge era before the argument-file one starts a server from a jar
    /// the installer ships rather than from anything a processor produces. It
    /// is resolved out of the shared library store, which is where both the
    /// profile's libraries and the metadata-declared ones have already been
    /// published, and named after its own coordinate so the file on disk is
    /// the one the coordinate says it is.
    async fn stage_profile_jar(
        &self,
        bundle: &InstallerBundle,
        staging: &Path,
    ) -> LoaderResult<PathBuf> {
        let coordinate =
            bundle.profile.path.as_deref().ok_or_else(|| {
                invalid_installer(&bundle.installer, "profile names no launcher jar")
            })?;
        let coordinate = MavenCoordinate::parse(coordinate)?;
        let relative = coordinate.relative_path();
        let name = PathBuf::from(relative.file_name().ok_or_else(|| {
            invalid_installer(
                &bundle.installer,
                "launcher jar coordinate has no file name",
            )
        })?);
        let cached = self.layout.library(&coordinate);
        if !fs::try_exists(&cached).await.unwrap_or(false) {
            return Err(LoaderError::ProcessorOutputMissing { path: cached });
        }
        copy_file(&cached, &staging.join(&name)).await?;
        Ok(name)
    }

    fn workspace(&self, side: &str) -> PathBuf {
        let sequence = WORKSPACE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        self.layout
            .store_root(ContentStore::InstallerCache)
            .join("work")
            .join(format!("{side}-{}-{sequence}", std::process::id()))
    }

    async fn prepare_workspace(
        &self,
        workspace: &Path,
        profile: &InstallProfile,
    ) -> LoaderResult<()> {
        remove_tree(workspace).await;
        fs::create_dir_all(workspace)
            .await
            .map_err(|source| io_error(workspace, source))?;
        self.prepare_workspace_in(workspace, profile).await
    }

    async fn prepare_workspace_in(
        &self,
        workspace: &Path,
        profile: &InstallProfile,
    ) -> LoaderResult<()> {
        let libraries = workspace.join("libraries");
        fs::create_dir_all(&libraries)
            .await
            .map_err(|source| io_error(&libraries, source))?;
        for library in &profile.libraries {
            let source = self.layout.library(&library.coordinate);
            let destination = libraries.join(library.coordinate.relative_path());
            copy_file(&source, &destination).await?;
        }
        Ok(())
    }

    async fn run_processors(
        &self,
        bundle: &InstallerBundle,
        side: InstallSide,
        workspace: &Path,
        minecraft_jar: &Path,
        java: &Path,
        cancel: &CancellationToken,
    ) -> LoaderResult<()> {
        self.extract_profile_data(bundle, workspace).await?;
        let mut values = BTreeMap::from([
            ("SIDE".to_owned(), side.as_str().to_owned()),
            ("ROOT".to_owned(), workspace.to_string_lossy().into_owned()),
            (
                "LIBRARY_DIR".to_owned(),
                workspace.join("libraries").to_string_lossy().into_owned(),
            ),
            (
                "INSTALLER".to_owned(),
                bundle.installer.to_string_lossy().into_owned(),
            ),
            (
                "MINECRAFT_JAR".to_owned(),
                minecraft_jar.to_string_lossy().into_owned(),
            ),
            (
                "MINECRAFT_VERSION".to_owned(),
                bundle.profile.minecraft.clone(),
            ),
        ]);
        for name in bundle.profile.data.keys() {
            let raw = bundle.profile.data_value(name, side).unwrap_or_default();
            let resolved = resolve_profile_value(raw, workspace, &values)?;
            values.insert(name.clone(), resolved);
        }

        for processor in bundle
            .profile
            .processors
            .iter()
            .filter(|processor| processor.applies_to(side))
        {
            if cancel.is_cancelled() {
                return Err(LoaderError::Canceled);
            }
            // Forge describes Mojang mappings as an installer task. Fetching
            // them here through the shared download manager keeps every HTTP
            // transfer under the launcher's verification/cancellation policy.
            if !is_download_mojmaps(processor) {
                self.run_processor(processor, workspace, java, &values, cancel)
                    .await?;
            }
            self.verify_processor_outputs(processor, &values).await?;
        }

        if let Some(patched) = values.get("PATCHED") {
            let path = PathBuf::from(patched);
            if !fs::try_exists(&path).await.unwrap_or(false) {
                return Err(LoaderError::ProcessorOutputMissing { path });
            }
            if let Some(expected) = values.get("PATCHED_SHA") {
                verify_file_hash(&path, expected).await?;
            }
        }
        Ok(())
    }

    async fn prepare_mojang_mappings(
        &self,
        profile: &InstallProfile,
        side: InstallSide,
        parent: &VersionMetadata,
        workspace: &Path,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> LoaderResult<()> {
        let Some(raw) = profile.data_value("MOJMAPS", side) else {
            return Ok(());
        };
        let coordinate = raw
            .strip_prefix('[')
            .and_then(|value| value.strip_suffix(']'))
            .ok_or_else(|| LoaderError::MetadataInvalid {
                provider: "installer",
                detail: "MOJMAPS is not a Maven coordinate".to_owned(),
            })?;
        let destination = workspace
            .join("libraries")
            .join(MavenCoordinate::parse(coordinate)?.relative_path());
        let key = match side {
            InstallSide::Client => "client_mappings",
            InstallSide::Server => "server_mappings",
        };
        let artifact = parent.download(key)?;
        let request = DownloadRequest::parse(&artifact.url, &destination)?
            .verifying(artifact.sha1.clone())
            .sized(artifact.size)
            .labeled(format!("{} {key}", parent.id));
        self.manager
            .download(&request, reporter, cancel)
            .await
            .map_err(|source| LoaderError::Download {
                artifact: format!("{} {key}", parent.id),
                source: Box::new(source),
            })?;
        Ok(())
    }

    async fn extract_profile_data(
        &self,
        bundle: &InstallerBundle,
        workspace: &Path,
    ) -> LoaderResult<()> {
        for value in bundle.profile.data.values() {
            for raw in [&value.client, &value.server] {
                if let Some(entry) = raw.strip_prefix('/') {
                    let destination = safe_archive_destination(workspace, entry)?;
                    if fs::try_exists(&destination).await.unwrap_or(false) {
                        continue;
                    }
                    let installer = bundle.installer.clone();
                    let entry = entry.to_owned();
                    let destination_copy = destination.clone();
                    tokio::task::spawn_blocking(move || {
                        extract_entry(&installer, &entry, &destination_copy)
                    })
                    .await
                    .map_err(|error| invalid_installer(&bundle.installer, &error.to_string()))??;
                }
            }
        }
        Ok(())
    }

    async fn run_processor(
        &self,
        processor: &Processor,
        workspace: &Path,
        java: &Path,
        values: &BTreeMap<String, String>,
        cancel: &CancellationToken,
    ) -> LoaderResult<()> {
        let libraries = workspace.join("libraries");
        let jar = MavenCoordinate::parse(&processor.jar)?;
        let processor_path = libraries.join(jar.relative_path());
        let mut classpath = Vec::with_capacity(processor.classpath.len() + 1);
        classpath.push(processor_path.clone());
        for coordinate in &processor.classpath {
            classpath.push(libraries.join(MavenCoordinate::parse(coordinate)?.relative_path()));
        }
        let main_class = processor_main_class(&processor_path)?;
        let classpath = std::env::join_paths(classpath).map_err(|error| {
            invalid_installer(
                &processor_path,
                &format!("invalid processor classpath: {error}"),
            )
        })?;
        let args = processor
            .args
            .iter()
            .map(|argument| resolve_profile_value(argument, workspace, values))
            .collect::<LoaderResult<Vec<_>>>()?;
        debug!(
            processor = %processor.jar,
            side = %values["SIDE"],
            "running loader processor"
        );
        let mut command = Command::new(java);
        command
            .current_dir(workspace)
            .arg("-cp")
            .arg(classpath)
            .arg(&main_class)
            .args(args)
            .kill_on_drop(true);
        let output = tokio::select! {
            () = cancel.cancelled() => return Err(LoaderError::Canceled),
            output = command.output() => output.map_err(|source| io_error(java, source))?,
        };
        if !output.status.success() {
            return Err(LoaderError::ProcessorFailed {
                processor: processor.jar.clone(),
                code: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr)
                    .chars()
                    .take(4096)
                    .collect(),
            });
        }
        Ok(())
    }

    async fn verify_processor_outputs(
        &self,
        processor: &Processor,
        values: &BTreeMap<String, String>,
    ) -> LoaderResult<()> {
        for (output, expected) in &processor.outputs {
            let path = PathBuf::from(resolve_tokens(output, values)?);
            if !fs::try_exists(&path).await.unwrap_or(false) {
                return Err(LoaderError::ProcessorOutputMissing { path });
            }
            let hash = resolve_tokens(expected, values)?;
            verify_file_hash(&path, hash.trim_matches('\'')).await?;
        }
        Ok(())
    }

    async fn publish_libraries(&self, source: &Path) -> LoaderResult<()> {
        let destination = self.layout.store_root(ContentStore::Libraries);
        publish_tree(source, &destination).await
    }

    async fn mark_preinstalled_artifacts(
        &self,
        metadata: &mut VersionMetadata,
    ) -> LoaderResult<()> {
        for library in &mut metadata.libraries {
            let Some(artifact) = library.downloads.artifact.as_mut() else {
                continue;
            };
            if !artifact.url.is_empty() {
                continue;
            }
            let path = self.layout.library(&library.coordinate);
            if !fs::try_exists(&path).await.unwrap_or(false) {
                return Err(LoaderError::ProcessorOutputMissing { path });
            }
            // VanillaInstaller constructs all requests before it checks the
            // cache, so the URL still has to parse. This host is deliberately
            // non-resolving: the artifact was verified from the installer and
            // a later cache loss must fail, never fetch different bytes.
            "https://invalid.invalid/preinstalled-loader-artifact".clone_into(&mut artifact.url);
        }
        Ok(())
    }
}

fn installer_sidecar_key(
    kind: LoaderKind,
    minecraft: &str,
    exact: &str,
) -> LoaderResult<MetadataKey> {
    Ok(MetadataKey::new(&format!(
        "{}-{}-{}-installer-sha1",
        kind.to_string().to_ascii_lowercase(),
        minecraft,
        exact
    ))?)
}

/// Refuses a bundle whose profile and version metadata would store different files at one coordinate.
/// Each side contributes the entry an install stores; a coordinate only one side stores is no conflict.
fn ensure_consistent_library_digests(
    bundle: &InstallerBundle,
    context: &RuleContext,
) -> LoaderResult<()> {
    let mut profile: HashMap<&MavenCoordinate, &Hash> = HashMap::new();
    for declared in &bundle.profile.libraries {
        profile
            .entry(&declared.coordinate)
            .or_insert(&declared.artifact.sha1);
    }
    let mut stored = HashSet::new();
    for library in &bundle.child.libraries {
        let Some(artifact) = library.downloads.artifact.as_ref() else {
            continue;
        };
        if !rules_allow(&library.rules, context) || !stored.insert(&library.coordinate) {
            continue;
        }
        if let Some(sha1) = profile.get(&library.coordinate).copied()
            && *sha1 != artifact.sha1
        {
            return Err(LoaderError::InstallerLibraryConflict {
                path: bundle.installer.clone(),
                library: library.name.clone(),
                profile: sha1.clone(),
                version: artifact.sha1.clone(),
            });
        }
    }
    Ok(())
}

fn installer_location(
    kind: LoaderKind,
    minecraft: &str,
    exact: &str,
) -> LoaderResult<(MavenCoordinate, String)> {
    match kind {
        LoaderKind::Forge => {
            let coordinate = MavenCoordinate::parse(&format!(
                "net.minecraftforge:forge:{minecraft}-{exact}:installer"
            ))?;
            let relative = coordinate.relative_path();
            Ok((coordinate, format!("{FORGE_MAVEN}/{}", relative.display())))
        }
        LoaderKind::NeoForge => {
            let coordinate =
                MavenCoordinate::parse(&format!("net.neoforged:neoforge:{exact}:installer"))?;
            let relative = coordinate.relative_path();
            Ok((
                coordinate,
                format!("{NEOFORGE_MAVEN}/{}", relative.display()),
            ))
        }
        LoaderKind::Vanilla => Err(LoaderError::ExactVersionRequired { loader: kind }),
    }
}

fn is_download_mojmaps(processor: &Processor) -> bool {
    processor.args.windows(2).any(|pair| {
        pair.first().map(String::as_str) == Some("--task")
            && pair.get(1).map(String::as_str) == Some("DOWNLOAD_MOJMAPS")
    })
}

fn read_zip_entry(path: &Path, name: &str) -> LoaderResult<String> {
    let file = std::fs::File::open(path).map_err(|source| io_error(path, source))?;
    let mut archive =
        ZipArchive::new(file).map_err(|error| invalid_installer(path, &error.to_string()))?;
    let mut entry = archive
        .by_name(name)
        .map_err(|error| invalid_installer(path, &format!("missing {name}: {error}")))?;
    if entry.size() > MAX_PROFILE_BYTES {
        return Err(invalid_installer(path, &format!("{name} is too large")));
    }
    let mut text = String::new();
    entry
        .read_to_string(&mut text)
        .map_err(|source| io_error(path, source))?;
    Ok(text)
}

fn extract_verified_entry(
    installer: &Path,
    entry: &str,
    destination: &Path,
    expected: &Hash,
    size: u64,
) -> LoaderResult<()> {
    let bytes = read_zip_bytes(installer, entry, size.max(1))?;
    if bytes.len() as u64 != size {
        return Err(invalid_installer(
            installer,
            &format!("{entry} has the wrong size"),
        ));
    }
    let mut verifier = HashVerifier::new(std::slice::from_ref(expected));
    verifier.update(&bytes);
    verifier.finish().map_err(|error| {
        invalid_installer(installer, &format!("{entry} hash mismatch: {error:?}"))
    })?;
    write_atomic(destination, &bytes)
}

fn extract_entry(installer: &Path, entry: &str, destination: &Path) -> LoaderResult<()> {
    let bytes = read_zip_bytes(installer, entry, 128 * 1024 * 1024)?;
    write_atomic(destination, &bytes)
}

/// Extracts a library the version metadata declares but the installer may not
/// carry. A file that is not in the archive is not an error; see
/// [`InstallerEngine::install_installer_declared_libraries`].
fn extract_declared_entry(
    installer: &Path,
    entry: &str,
    destination: &Path,
    expected: &Hash,
    size: u64,
) -> LoaderResult<()> {
    if !zip_entry_exists(installer, entry)? {
        return Ok(());
    }
    extract_verified_entry(installer, entry, destination, expected, size)
}

fn zip_entry_exists(installer: &Path, entry: &str) -> LoaderResult<bool> {
    let file = std::fs::File::open(installer).map_err(|source| io_error(installer, source))?;
    let mut archive =
        ZipArchive::new(file).map_err(|error| invalid_installer(installer, &error.to_string()))?;
    match archive.by_name(entry) {
        Ok(entry) => Ok(!entry.is_dir()),
        Err(zip::result::ZipError::FileNotFound) => Ok(false),
        Err(error) => Err(invalid_installer(installer, &error.to_string())),
    }
}

fn read_zip_bytes(installer: &Path, entry: &str, limit: u64) -> LoaderResult<Vec<u8>> {
    let file = std::fs::File::open(installer).map_err(|source| io_error(installer, source))?;
    let mut archive =
        ZipArchive::new(file).map_err(|error| invalid_installer(installer, &error.to_string()))?;
    let mut file = archive
        .by_name(entry)
        .map_err(|error| invalid_installer(installer, &format!("missing {entry}: {error}")))?;
    if file.is_dir() || file.size() > limit {
        return Err(invalid_installer(
            installer,
            &format!("unsafe entry {entry}"),
        ));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(file.size()).unwrap_or(0));
    file.read_to_end(&mut bytes)
        .map_err(|source| io_error(installer, source))?;
    Ok(bytes)
}

fn parse_library_url(raw: &str) -> LoaderResult<Url> {
    Url::parse(raw).map_err(|_| {
        UrlPolicyError::Malformed {
            url: raw.to_owned(),
        }
        .into()
    })
}

fn write_atomic(destination: &Path, bytes: &[u8]) -> LoaderResult<()> {
    if destination.exists() {
        return Ok(());
    }
    let parent = destination
        .parent()
        .ok_or_else(|| invalid_installer(destination, "destination has no parent"))?;
    std::fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;
    let temporary = destination.with_extension("ephervex-part");
    std::fs::write(&temporary, bytes).map_err(|source| io_error(&temporary, source))?;
    std::fs::rename(&temporary, destination).map_err(|source| io_error(destination, source))?;
    Ok(())
}

fn processor_main_class(path: &Path) -> LoaderResult<String> {
    let file = std::fs::File::open(path).map_err(|source| io_error(path, source))?;
    let mut archive =
        ZipArchive::new(file).map_err(|error| invalid_installer(path, &error.to_string()))?;
    let mut manifest = String::new();
    archive
        .by_name("META-INF/MANIFEST.MF")
        .map_err(|_| LoaderError::ProcessorMainClass {
            processor: path.display().to_string(),
        })?
        .read_to_string(&mut manifest)
        .map_err(|source| io_error(path, source))?;
    let unfolded = manifest.replace("\r\n ", "").replace("\n ", "");
    unfolded
        .lines()
        .find_map(|line| line.strip_prefix("Main-Class:").map(str::trim))
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| LoaderError::ProcessorMainClass {
            processor: path.display().to_string(),
        })
}

fn resolve_profile_value(
    raw: &str,
    workspace: &Path,
    values: &BTreeMap<String, String>,
) -> LoaderResult<String> {
    if let Some(coordinate) = raw
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
    {
        return Ok(workspace
            .join("libraries")
            .join(MavenCoordinate::parse(coordinate)?.relative_path())
            .to_string_lossy()
            .into_owned());
    }
    if let Some(entry) = raw.strip_prefix('/') {
        return Ok(safe_archive_destination(workspace, entry)?
            .to_string_lossy()
            .into_owned());
    }
    let resolved = resolve_tokens(raw, values)?;
    Ok(resolved.trim_matches('\'').to_owned())
}

fn resolve_tokens(raw: &str, values: &BTreeMap<String, String>) -> LoaderResult<String> {
    let mut result = raw.to_owned();
    let mut rest = raw;
    while let Some(start) = rest.find('{') {
        let tail = &rest[start + 1..];
        let Some(end) = tail.find('}') else {
            break;
        };
        let name = &tail[..end];
        let value = values
            .get(name)
            .ok_or_else(|| LoaderError::MetadataInvalid {
                provider: "installer",
                detail: format!("unknown installer token {{{name}}}"),
            })?;
        result = result.replace(&format!("{{{name}}}"), value);
        rest = &tail[end + 1..];
    }
    Ok(result)
}

fn safe_archive_destination(root: &Path, entry: &str) -> LoaderResult<PathBuf> {
    let path = Path::new(entry);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(invalid_installer(
            root,
            &format!("unsafe archive path {entry}"),
        ));
    }
    Ok(root.join(path))
}

async fn verify_file_hash(path: &Path, expected: &str) -> LoaderResult<()> {
    let expected = Hash::new(HashAlgorithm::Sha1, expected)
        .ok_or_else(|| invalid_installer(path, "processor output declares a malformed SHA-1"))?;
    let bytes = fs::read(path)
        .await
        .map_err(|source| io_error(path, source))?;
    let mut verifier = HashVerifier::new(&[expected]);
    verifier.update(&bytes);
    verifier.finish().map_err(|error| {
        invalid_installer(path, &format!("processor output hash mismatch: {error:?}"))
    })
}

async fn copy_file(source: &Path, destination: &Path) -> LoaderResult<()> {
    if fs::try_exists(destination).await.unwrap_or(false) {
        return Ok(());
    }
    let parent = destination
        .parent()
        .ok_or_else(|| invalid_installer(destination, "destination has no parent"))?;
    fs::create_dir_all(parent)
        .await
        .map_err(|source| io_error(parent, source))?;
    fs::copy(source, destination)
        .await
        .map_err(|source| io_error(destination, source))?;
    Ok(())
}

async fn publish_tree(source: &Path, destination: &Path) -> LoaderResult<()> {
    let mut pending = vec![(source.to_path_buf(), destination.to_path_buf())];
    while let Some((from, to)) = pending.pop() {
        fs::create_dir_all(&to)
            .await
            .map_err(|source| io_error(&to, source))?;
        let mut entries = fs::read_dir(&from)
            .await
            .map_err(|source| io_error(&from, source))?;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|source| io_error(&from, source))?
        {
            let kind = entry
                .file_type()
                .await
                .map_err(|source| io_error(&entry.path(), source))?;
            let target = to.join(entry.file_name());
            if kind.is_dir() {
                pending.push((entry.path(), target));
            } else if kind.is_file() {
                copy_file(&entry.path(), &target).await?;
            } else {
                return Err(invalid_installer(
                    &entry.path(),
                    "processor produced a symlink",
                ));
            }
        }
    }
    Ok(())
}

async fn ensure_empty_target(target: &Path) -> LoaderResult<()> {
    if !fs::try_exists(target).await.unwrap_or(false) {
        return Ok(());
    }
    let mut entries = fs::read_dir(target)
        .await
        .map_err(|source| io_error(target, source))?;
    if entries
        .next_entry()
        .await
        .map_err(|source| io_error(target, source))?
        .is_some()
    {
        return Err(LoaderError::ServerTargetOccupied {
            path: target.to_path_buf(),
        });
    }
    fs::remove_dir(target)
        .await
        .map_err(|source| io_error(target, source))?;
    Ok(())
}

fn server_staging(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("server");
    target.with_file_name(format!(
        ".{name}.ephervex-installing-{}",
        std::process::id()
    ))
}

fn modern_server_argfile(kind: LoaderKind, minecraft: &str, exact: &str) -> PathBuf {
    let relative = match kind {
        LoaderKind::Forge => {
            format!("net/minecraftforge/forge/{minecraft}-{exact}/unix_args.txt")
        }
        LoaderKind::NeoForge => {
            format!("net/neoforged/neoforge/{exact}/unix_args.txt")
        }
        LoaderKind::Vanilla => String::new(),
    };
    PathBuf::from("libraries").join(relative)
}

async fn link_shared_libraries(layout: &CacheLayout, root: &Path) -> LoaderResult<()> {
    let link = root.join("libraries");
    create_directory_link(&layout.store_root(ContentStore::Libraries), &link).await
}

async fn replace_with_shared_libraries(layout: &CacheLayout, root: &Path) -> LoaderResult<()> {
    let libraries = root.join("libraries");
    fs::remove_dir_all(&libraries)
        .await
        .map_err(|source| io_error(&libraries, source))?;
    create_directory_link(&layout.store_root(ContentStore::Libraries), &libraries).await
}

#[cfg(unix)]
async fn create_directory_link(source: &Path, destination: &Path) -> LoaderResult<()> {
    let source = source.to_path_buf();
    let destination = destination.to_path_buf();
    let error_path = destination.clone();
    tokio::task::spawn_blocking(move || std::os::unix::fs::symlink(&source, &destination))
        .await
        .map_err(|error| io_error(&error_path, std::io::Error::other(error)))?
        .map_err(|source| io_error(&error_path, source))
}

#[cfg(not(unix))]
async fn create_directory_link(source: &Path, destination: &Path) -> LoaderResult<()> {
    let _ = source;
    Err(invalid_installer(
        destination,
        "shared server libraries require a supported directory-link implementation",
    ))
}

async fn remove_tree(path: &Path) {
    if let Err(error) = fs::remove_dir_all(path).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(
            path = %path.display(),
            %error,
            "could not remove loader staging directory"
        );
    }
}

fn invalid_installer(path: &Path, detail: &str) -> LoaderError {
    LoaderError::InstallerInvalid {
        path: path.to_path_buf(),
        detail: detail.to_owned(),
    }
}

fn io_error(path: &Path, source: std::io::Error) -> LoaderError {
    LoaderError::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

    use std::time::Duration;

    use super::*;

    #[cfg(unix)]
    fn fake_processor_workspace(exit_code: i32) -> (tempfile::TempDir, PathBuf, PathBuf) {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt as _;

        let root = tempfile::tempdir().expect("workspace");
        let workspace = root.path().join("work");
        let processor = workspace.join("libraries/g/p/1/p-1.jar");
        std::fs::create_dir_all(processor.parent().expect("processor parent"))
            .expect("processor directory");
        let file = std::fs::File::create(&processor).expect("processor jar");
        let mut archive = zip::ZipWriter::new(file);
        archive
            .start_file(
                "META-INF/MANIFEST.MF",
                zip::write::SimpleFileOptions::default(),
            )
            .expect("manifest entry");
        archive
            .write_all(b"Manifest-Version: 1.0\r\nMain-Class: fixture.Processor\r\n\r\n")
            .expect("manifest");
        archive.finish().expect("processor jar finish");

        let java = root.path().join("fake-java");
        std::fs::write(
            &java,
            format!("#!/bin/sh\necho synthetic-processor >&2\nexit {exit_code}\n"),
        )
        .expect("fake java");
        let mut permissions = std::fs::metadata(&java).expect("metadata").permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&java, permissions).expect("executable fake java");
        (root, workspace, java)
    }

    #[test]
    fn exact_installer_coordinates_are_safe_and_version_preserving() {
        let (forge, url) =
            installer_location(LoaderKind::Forge, "1.20.1", "47.4.10").expect("forge coordinate");
        assert_eq!(forge.version(), "1.20.1-47.4.10");
        assert!(url.ends_with("forge-1.20.1-47.4.10-installer.jar"));

        let (neo, url) = installer_location(LoaderKind::NeoForge, "26.1.2", "26.1.2.94")
            .expect("neoforge coordinate");
        assert_eq!(neo.version(), "26.1.2.94");
        assert!(url.ends_with("neoforge-26.1.2.94-installer.jar"));
    }

    #[test]
    fn hostile_exact_versions_cannot_choose_an_installer_path() {
        assert!(installer_location(LoaderKind::Forge, "1.20.1", "../../evil").is_err());
    }

    #[test]
    fn tokens_and_maven_values_resolve_inside_the_workspace() {
        let values = BTreeMap::from([("ROOT".to_owned(), "/tmp/work".to_owned())]);
        assert_eq!(
            resolve_profile_value("{ROOT}/run.sh", Path::new("/tmp/work"), &values).expect("root"),
            "/tmp/work/run.sh"
        );
        assert!(resolve_profile_value("[../evil:x:1]", Path::new("/tmp/work"), &values).is_err());
    }

    #[test]
    fn modern_server_argfiles_match_the_official_layout() {
        assert_eq!(
            modern_server_argfile(LoaderKind::Forge, "1.20.1", "47.4.10"),
            PathBuf::from("libraries/net/minecraftforge/forge/1.20.1-47.4.10/unix_args.txt")
        );
        assert_eq!(
            modern_server_argfile(LoaderKind::NeoForge, "26.1.2", "26.1.2.94"),
            PathBuf::from("libraries/net/neoforged/neoforge/26.1.2.94/unix_args.txt")
        );
    }

    const ABC_SHA1: &str = "a9993e364706816aba3e25717850c26c9cd0d89d";
    const DEF_SHA1: &str = "589c22335a381f122d129225f5c0ba3056ed5811";
    const XYZ_SHA1: &str = "66b27417d37e024c46526c2f6d358a754fc552f3";

    fn loopback_engine(root: &Path) -> InstallerEngine {
        let manager = DownloadManager::new(ephervex_download::DownloaderConfig {
            http: ephervex_download::HttpConfig::for_loopback_tests(),
            ..ephervex_download::DownloaderConfig::default()
        })
        .expect("manager");
        InstallerEngine::new(manager, CacheLayout::new(root.join("cache")))
    }

    fn library_json(name: &str, url: &str, sha1: &str) -> String {
        format!(
            r#"{{"name":"{name}","downloads":{{"artifact":{{"url":"{url}","sha1":"{sha1}","size":3}}}}}}"#
        )
    }

    /// A bundle whose profile declares `profile_libraries` (a JSON list body) and
    /// whose version metadata declares `libraries`.
    fn bundle_declaring(
        root: &Path,
        profile_libraries: &str,
        libraries: &[String],
    ) -> InstallerBundle {
        InstallerBundle {
            installer: root.join("installer.jar"),
            profile: InstallProfile::parse(&profile_json(profile_libraries)).expect("profile"),
            child: VersionMetadata::parse(&version_json(libraries)).expect("version metadata"),
        }
    }

    fn profile_json(libraries: &str) -> String {
        format!(
            r#"{{"spec":1,"profile":"forge","version":"1.20.1-forge-47.4.10",
            "minecraft":"1.20.1","libraries":[{libraries}]}}"#
        )
    }

    fn version_json(libraries: &[String]) -> String {
        format!(
            r#"{{"id":"1.20.1-forge-47.4.10","inheritsFrom":"1.20.1","type":"release",
            "releaseTime":"2024-01-01T00:00:00+00:00","time":"2024-01-01T00:00:00+00:00",
            "mainClass":"cpw.mods.bootstraplauncher.BootstrapLauncher","libraries":[{}]}}"#,
            libraries.join(",")
        )
    }

    fn vanilla_parent() -> VersionMetadata {
        VersionMetadata::parse(
            r#"{"id":"1.20.1","type":"release","releaseTime":"2023-06-12T00:00:00+00:00",
            "time":"2023-06-12T00:00:00+00:00","mainClass":"net.minecraft.client.main.Main",
            "libraries":[]}"#,
        )
        .expect("parent metadata")
    }

    /// Places a Forge 1.20.1-47.4.10 installer carrying these documents, and its
    /// digest sidecar, in the caches an install reads before reaching the network.
    async fn cache_installer(engine: &InstallerEngine, profile: &str, version: &str) {
        use std::io::Write as _;

        let staged = engine.layout.root().join("staged-installer.jar");
        std::fs::create_dir_all(staged.parent().expect("parent")).expect("cache root");
        let mut archive = zip::ZipWriter::new(std::fs::File::create(&staged).expect("installer"));
        for (name, body) in [("install_profile.json", profile), ("version.json", version)] {
            archive
                .start_file(name, zip::write::SimpleFileOptions::default())
                .expect("entry");
            archive.write_all(body.as_bytes()).expect("entry body");
        }
        archive.finish().expect("installer finish");

        let sha1 = ephervex_download::hash_file(&staged, HashAlgorithm::Sha1)
            .await
            .expect("installer digest");
        let jar = FileExtension::new("jar").expect("extension");
        let cached = engine
            .layout
            .content(ContentStore::InstallerCache, &sha1, Some(&jar));
        std::fs::create_dir_all(cached.parent().expect("parent")).expect("installer cache");
        std::fs::rename(&staged, &cached).expect("cached installer");

        let (_, jar_url) =
            installer_location(LoaderKind::Forge, "1.20.1", "47.4.10").expect("installer");
        MetadataCache::new(engine.layout.clone())
            .store(
                &installer_sidecar_key(LoaderKind::Forge, "1.20.1", "47.4.10").expect("key"),
                &Url::parse(&format!("{jar_url}.sha1")).expect("sidecar url"),
                serde_json::value::to_raw_value(sha1.digest()).expect("sidecar"),
            )
            .await
            .expect("sidecar cached");
    }

    fn library_path(engine: &InstallerEngine, coordinate: &str) -> PathBuf {
        engine
            .layout
            .library(&MavenCoordinate::parse(coordinate).expect("coordinate"))
    }

    fn store_library(engine: &InstallerEngine, coordinate: &str, bytes: &[u8]) -> PathBuf {
        let path = library_path(engine, coordinate);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("store");
        std::fs::write(&path, bytes).expect("library");
        path
    }

    #[tokio::test]
    async fn a_server_install_fetches_the_libraries_only_its_version_metadata_declares() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        for (route, body) in [("/bootstraplauncher.jar", b"abc"), ("/javafml.jar", b"xyz")] {
            Mock::given(method("GET"))
                .and(path(route))
                .respond_with(ResponseTemplate::new(200).set_body_bytes(body.to_vec()))
                .expect(1)
                .mount(&server)
                .await;
        }
        let base = server.uri();
        let root = tempfile::tempdir().expect("root");
        let engine = loopback_engine(root.path());
        let fmlcore = store_library(&engine, "net.minecraftforge:fmlcore:1.20.1-47.4.10", b"def");
        let javafml = store_library(
            &engine,
            "net.minecraftforge:javafmllanguage:1.20.1-47.4.10",
            b"def",
        );
        let bundle = bundle_declaring(
            root.path(),
            "",
            &[
                library_json(
                    "cpw.mods:bootstraplauncher:1.1.2",
                    &format!("{base}/bootstraplauncher.jar"),
                    ABC_SHA1,
                ),
                // A later entry for the same coordinate is not fetched.
                library_json(
                    "cpw.mods:bootstraplauncher:1.1.2",
                    &format!("{base}/other-bootstraplauncher.jar"),
                    XYZ_SHA1,
                ),
                // An entry the installer carries claims its coordinate too.
                library_json("net.minecraftforge:forge:1.20.1-47.4.10", "", DEF_SHA1),
                library_json(
                    "net.minecraftforge:forge:1.20.1-47.4.10",
                    &format!("{base}/forge.jar"),
                    XYZ_SHA1,
                ),
                library_json(
                    "net.minecraftforge:fmlcore:1.20.1-47.4.10",
                    &format!("{base}/fmlcore.jar"),
                    DEF_SHA1,
                ),
                library_json(
                    "net.minecraftforge:javafmllanguage:1.20.1-47.4.10",
                    &format!("{base}/javafml.jar"),
                    XYZ_SHA1,
                ),
            ],
        );

        engine
            .install_metadata_libraries(
                &bundle,
                &OperationReporter::silent("loader-server-libraries-test"),
                &CancellationToken::new(),
            )
            .await
            .expect("server libraries install");

        let launcher = library_path(&engine, "cpw.mods:bootstraplauncher:1.1.2");
        assert_eq!(std::fs::read(launcher).expect("missing library"), b"abc");
        assert_eq!(std::fs::read(fmlcore).expect("verified library"), b"def");
        assert_eq!(std::fs::read(javafml).expect("replaced library"), b"xyz");
    }

    /// Answers every request with `abc`, holding the answers until `expected`
    /// requests are waiting at once or a short wait passes. Returns the address
    /// and the most requests that were ever waiting together.
    async fn serve_held_until_concurrent(
        expected: usize,
    ) -> (String, tokio::task::JoinHandle<usize>) {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("listener");
        let address = format!("http://{}", listener.local_addr().expect("address"));
        let server = tokio::spawn(async move {
            let mut answered = 0;
            let mut most_waiting = 0;
            while answered < expected {
                let mut waiting = Vec::new();
                let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
                while waiting.len() < expected - answered {
                    let Ok(accepted) = tokio::time::timeout_at(deadline, listener.accept()).await
                    else {
                        break;
                    };
                    let (mut stream, _) = accepted.expect("accept");
                    let mut request = Vec::new();
                    let mut chunk = [0_u8; 1024];
                    while !request.windows(4).any(|end| end == b"\r\n\r\n") {
                        let read = stream.read(&mut chunk).await.expect("request");
                        assert!(read > 0, "the request ended early");
                        request.extend_from_slice(&chunk[..read]);
                    }
                    waiting.push(stream);
                }
                // A wait with nothing new means the rest are never coming.
                if waiting.is_empty() {
                    break;
                }
                most_waiting = most_waiting.max(waiting.len());
                for mut stream in waiting {
                    stream
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nabc",
                        )
                        .await
                        .expect("response");
                    answered += 1;
                }
            }
            most_waiting
        });
        (address, server)
    }

    #[tokio::test]
    async fn profile_libraries_are_fetched_as_one_concurrent_batch() {
        const LIBRARIES: usize = 4;
        let root = tempfile::tempdir().expect("root");
        let manager = DownloadManager::new(ephervex_download::DownloaderConfig {
            http: ephervex_download::HttpConfig::for_loopback_tests(),
            concurrency: LIBRARIES,
            ..ephervex_download::DownloaderConfig::default()
        })
        .expect("manager");
        let engine = InstallerEngine::new(manager, CacheLayout::new(root.path().join("cache")));
        let (address, server) = serve_held_until_concurrent(LIBRARIES).await;
        let names: Vec<String> = (0..LIBRARIES)
            .map(|index| format!("test.library:part-{index}:1"))
            .collect();
        let profile_libraries = names
            .iter()
            .enumerate()
            .map(|(index, name)| {
                library_json(name, &format!("{address}/part-{index}.jar"), ABC_SHA1)
            })
            .collect::<Vec<_>>()
            .join(",");
        let bundle = bundle_declaring(root.path(), &profile_libraries, &[]);

        engine
            .install_profile_libraries(
                &bundle,
                &OperationReporter::silent("loader-profile-libraries-test"),
                &CancellationToken::new(),
            )
            .await
            .expect("profile libraries install");

        assert_eq!(
            server.await.expect("server"),
            LIBRARIES,
            "every profile library was requested before any was answered"
        );
        for name in &names {
            assert_eq!(
                std::fs::read(library_path(&engine, name)).expect("stored library"),
                b"abc"
            );
        }
    }

    #[tokio::test]
    async fn a_coordinate_the_profile_repeats_is_fetched_once_from_its_first_entry() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/first.jar"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"abc"))
            .expect(1)
            .mount(&server)
            .await;
        // A second fetch would replace the first entry's file with these bytes.
        Mock::given(method("GET"))
            .and(path("/second.jar"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"def"))
            .mount(&server)
            .await;
        let root = tempfile::tempdir().expect("root");
        let engine = loopback_engine(root.path());
        let coordinate = "net.minecraftforge:fmlcore:1.20.1-47.4.10";
        let bundle = bundle_declaring(
            root.path(),
            &[
                library_json(coordinate, &format!("{}/first.jar", server.uri()), ABC_SHA1),
                library_json(
                    coordinate,
                    &format!("{}/second.jar", server.uri()),
                    DEF_SHA1,
                ),
            ]
            .join(","),
            &[],
        );

        engine
            .install_profile_libraries(
                &bundle,
                &OperationReporter::silent("loader-profile-libraries-test"),
                &CancellationToken::new(),
            )
            .await
            .expect("the first entry installs the coordinate");

        assert_eq!(
            std::fs::read(library_path(&engine, coordinate)).expect("library"),
            b"abc"
        );
        let requested: Vec<_> = server
            .received_requests()
            .await
            .expect("recorded requests")
            .into_iter()
            .map(|request| request.url.path().to_owned())
            .collect();
        assert_eq!(requested, ["/first.jar"]);
    }

    #[tokio::test]
    async fn a_profile_library_whose_stored_copy_is_wrong_is_fetched_again() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/fmlcore.jar"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"abc"))
            .expect(1)
            .mount(&server)
            .await;
        let root = tempfile::tempdir().expect("root");
        let engine = loopback_engine(root.path());
        let coordinate = "net.minecraftforge:fmlcore:1.20.1-47.4.10";
        let stored = store_library(&engine, coordinate, b"xyz");
        let bundle = bundle_declaring(
            root.path(),
            &library_json(
                coordinate,
                &format!("{}/fmlcore.jar", server.uri()),
                ABC_SHA1,
            ),
            &[],
        );

        engine
            .install_profile_libraries(
                &bundle,
                &OperationReporter::silent("loader-profile-libraries-test"),
                &CancellationToken::new(),
            )
            .await
            .expect("profile libraries install");

        assert_eq!(std::fs::read(stored).expect("library"), b"abc");
    }

    #[tokio::test]
    async fn a_failed_profile_library_download_names_the_library() {
        let server = wiremock::MockServer::start().await;
        let root = tempfile::tempdir().expect("root");
        let engine = loopback_engine(root.path());
        let bundle = bundle_declaring(
            root.path(),
            &library_json(
                "net.minecraftforge:fmlcore:1.20.1-47.4.10",
                &format!("{}/fmlcore.jar", server.uri()),
                ABC_SHA1,
            ),
            &[],
        );

        let error = engine
            .install_profile_libraries(
                &bundle,
                &OperationReporter::silent("loader-profile-libraries-test"),
                &CancellationToken::new(),
            )
            .await
            .expect_err("a missing library fails the install");

        assert!(matches!(
            error,
            LoaderError::Download { ref artifact, .. }
                if artifact == "net.minecraftforge:fmlcore:1.20.1-47.4.10"
        ));
    }

    #[tokio::test]
    async fn a_failed_server_library_download_names_the_library() {
        let server = wiremock::MockServer::start().await;
        let root = tempfile::tempdir().expect("root");
        let engine = loopback_engine(root.path());
        let bundle = bundle_declaring(
            root.path(),
            "",
            &[library_json(
                "cpw.mods:securejarhandler:2.1.10",
                &format!("{}/securejarhandler.jar", server.uri()),
                ABC_SHA1,
            )],
        );

        let error = engine
            .install_metadata_libraries(
                &bundle,
                &OperationReporter::silent("loader-server-libraries-test"),
                &CancellationToken::new(),
            )
            .await
            .expect_err("a missing library fails the install");

        assert!(matches!(
            error,
            LoaderError::Download { ref artifact, .. } if artifact == "cpw.mods:securejarhandler:2.1.10"
        ));
    }

    #[tokio::test]
    async fn a_server_library_refused_by_the_transport_policy_is_named() {
        let root = tempfile::tempdir().expect("root");
        let engine = loopback_engine(root.path());
        // A policy refusal names no address.
        let bundle = bundle_declaring(
            root.path(),
            "",
            &[library_json(
                "cpw.mods:securejarhandler:2.1.10",
                "file:///securejarhandler.jar",
                ABC_SHA1,
            )],
        );

        let error = engine
            .install_metadata_libraries(
                &bundle,
                &OperationReporter::silent("loader-server-libraries-test"),
                &CancellationToken::new(),
            )
            .await
            .expect_err("a refused library fails the install");

        assert!(matches!(
            error,
            LoaderError::Download { ref artifact, .. } if artifact == "cpw.mods:securejarhandler:2.1.10"
        ));
    }

    #[tokio::test]
    async fn a_failed_server_library_download_is_not_blamed_on_a_library_whose_address_prefixes_it()
    {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/lib.jar"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"abc".to_vec()))
            .mount(&server)
            .await;
        let base = server.uri();
        let root = tempfile::tempdir().expect("root");
        let engine = loopback_engine(root.path());
        let bundle = bundle_declaring(
            root.path(),
            "",
            &[
                library_json(
                    "cpw.mods:bootstraplauncher:1.1.2",
                    &format!("{base}/lib.jar"),
                    ABC_SHA1,
                ),
                library_json(
                    "cpw.mods:securejarhandler:2.1.10",
                    &format!("{base}/lib.jar2"),
                    ABC_SHA1,
                ),
            ],
        );

        let error = engine
            .install_metadata_libraries(
                &bundle,
                &OperationReporter::silent("loader-server-libraries-test"),
                &CancellationToken::new(),
            )
            .await
            .expect_err("a missing library fails the install");

        assert!(matches!(
            error,
            LoaderError::Download { ref artifact, .. } if artifact == "cpw.mods:securejarhandler:2.1.10"
        ));
    }

    #[tokio::test]
    async fn canceling_server_library_downloads_is_reported_as_cancellation() {
        let server = wiremock::MockServer::start().await;
        let root = tempfile::tempdir().expect("root");
        let engine = loopback_engine(root.path());
        let bundle = bundle_declaring(
            root.path(),
            "",
            &[library_json(
                "cpw.mods:securejarhandler:2.1.10",
                &format!("{}/securejarhandler.jar", server.uri()),
                ABC_SHA1,
            )],
        );
        let cancel = CancellationToken::new();
        cancel.cancel();

        let error = engine
            .install_metadata_libraries(
                &bundle,
                &OperationReporter::silent("loader-server-libraries-test"),
                &cancel,
            )
            .await
            .expect_err("a canceled install stops");

        assert!(error.is_cancellation());
    }

    const FMLCORE: &str = "net.minecraftforge:fmlcore:1.20.1-47.4.10";

    /// A profile and version metadata that publish different digests for `FMLCORE`.
    fn conflicting_documents(base: &str) -> (String, String) {
        (
            profile_json(&library_json(
                FMLCORE,
                &format!("{base}/profile-fmlcore.jar"),
                DEF_SHA1,
            )),
            version_json(&[library_json(
                FMLCORE,
                &format!("{base}/fmlcore.jar"),
                XYZ_SHA1,
            )]),
        )
    }

    fn assert_fmlcore_conflict(error: &LoaderError) {
        assert!(
            matches!(
                error,
                LoaderError::InstallerLibraryConflict { library, profile, version, .. }
                    if library == FMLCORE
                        && profile.digest() == DEF_SHA1
                        && version.digest() == XYZ_SHA1
            ),
            "unexpected error: {error:?}"
        );
    }

    fn validate_on_host(bundle: &InstallerBundle) -> LoaderResult<()> {
        InstallerEngine::validate_bundle(
            LoaderKind::Forge,
            "1.20.1",
            "47.4.10",
            bundle,
            &RuleContext::new(Platform::host()),
        )
    }

    #[tokio::test]
    async fn a_client_install_refuses_conflicting_library_digests_before_downloading() {
        let server = wiremock::MockServer::start().await;
        let root = tempfile::tempdir().expect("root");
        let engine = loopback_engine(root.path());
        let fmlcore = store_library(&engine, FMLCORE, b"def");
        let (profile, version) = conflicting_documents(&server.uri());
        cache_installer(&engine, &profile, &version).await;

        let error = engine
            .install_loader_client(
                LoaderKind::Forge,
                "1.20.1",
                "47.4.10",
                &vanilla_parent(),
                &root.path().join("java"),
                &RuleContext::new(Platform::host()),
                &OperationReporter::silent("loader-client-conflict-test"),
                &CancellationToken::new(),
            )
            .await
            .expect_err("conflicting digests fail the install");

        assert_fmlcore_conflict(&error);
        assert_eq!(
            error.message_id(),
            "error-loader-installer-library-conflict"
        );
        assert!(
            server
                .received_requests()
                .await
                .expect("recorded")
                .is_empty()
        );
        assert_eq!(std::fs::read(fmlcore).expect("profile library"), b"def");
    }

    #[tokio::test]
    async fn a_server_install_refuses_conflicting_library_digests_before_downloading() {
        let server = wiremock::MockServer::start().await;
        let root = tempfile::tempdir().expect("root");
        let engine = loopback_engine(root.path());
        let fmlcore = store_library(&engine, FMLCORE, b"def");
        let (profile, version) = conflicting_documents(&server.uri());
        cache_installer(&engine, &profile, &version).await;
        let target = root.path().join("server");

        let error = engine
            .install_loader_server(
                LoaderKind::Forge,
                "1.20.1",
                "47.4.10",
                &vanilla_parent(),
                &root.path().join("java"),
                &target,
                &OperationReporter::silent("loader-server-conflict-test"),
                &CancellationToken::new(),
            )
            .await
            .expect_err("conflicting digests fail the install");

        assert_fmlcore_conflict(&error);
        assert!(
            server
                .received_requests()
                .await
                .expect("recorded")
                .is_empty()
        );
        assert_eq!(std::fs::read(fmlcore).expect("profile library"), b"def");
        assert!(!target.exists());
    }

    #[test]
    fn an_installer_carried_library_the_profile_declares_with_another_digest_is_refused() {
        let root = tempfile::tempdir().expect("root");
        let bundle = bundle_declaring(
            root.path(),
            &library_json(FMLCORE, "https://maven.invalid/fmlcore.jar", DEF_SHA1),
            &[library_json(FMLCORE, "", XYZ_SHA1)],
        );

        assert_fmlcore_conflict(&validate_on_host(&bundle).expect_err("conflict"));
    }

    #[test]
    fn a_library_both_documents_publish_with_one_digest_is_accepted() {
        let root = tempfile::tempdir().expect("root");
        let securejarhandler = "cpw.mods:securejarhandler:2.1.10";
        let bundle = bundle_declaring(
            root.path(),
            &[
                library_json(FMLCORE, "https://maven.invalid/profile.jar", DEF_SHA1),
                library_json(securejarhandler, "https://maven.invalid/sjh.jar", ABC_SHA1),
            ]
            .join(","),
            &[
                library_json(FMLCORE, "https://maven.invalid/fmlcore.jar", DEF_SHA1),
                // Digests compare by value, not by spelling.
                library_json(securejarhandler, "", &ABC_SHA1.to_ascii_uppercase()),
            ],
        );

        validate_on_host(&bundle).expect("matching digests are not a conflict");
    }

    #[test]
    fn only_the_version_metadata_entry_an_install_stores_is_compared() {
        let root = tempfile::tempdir().expect("root");
        let elsewhere = if cfg!(target_os = "macos") {
            "windows"
        } else {
            "osx"
        };
        let bundle = bundle_declaring(
            root.path(),
            &library_json(FMLCORE, "https://maven.invalid/profile.jar", DEF_SHA1),
            &[
                // No artifact, so no install stores it.
                format!(r#"{{"name":"{FMLCORE}","url":"https://maven.invalid/"}}"#),
                format!(
                    r#"{{"name":"{FMLCORE}","rules":[{{"action":"allow","os":{{"name":"{elsewhere}"}}}}],
                    "downloads":{{"artifact":{{"url":"https://maven.invalid/a.jar","sha1":"{XYZ_SHA1}","size":3}}}}}}"#
                ),
                library_json(FMLCORE, "https://maven.invalid/fmlcore.jar", DEF_SHA1),
                // Shadowed by the entry above.
                library_json(FMLCORE, "https://maven.invalid/later.jar", XYZ_SHA1),
            ],
        );

        validate_on_host(&bundle).expect("entries no install stores are not a conflict");
    }

    #[tokio::test]
    async fn a_library_its_rules_exclude_from_this_host_is_not_fetched() {
        let server = wiremock::MockServer::start().await;
        let root = tempfile::tempdir().expect("root");
        let engine = loopback_engine(root.path());
        let elsewhere = if cfg!(target_os = "macos") {
            "windows"
        } else {
            "osx"
        };
        let excluded = format!(
            r#"{{"name":"ca.weblite:java-objc-bridge:1.1","rules":[{{"action":"allow","os":{{"name":"{elsewhere}"}}}}],
            "downloads":{{"artifact":{{"url":"{}/objc.jar","sha1":"{ABC_SHA1}","size":3}}}}}}"#,
            server.uri()
        );
        let bundle = bundle_declaring(root.path(), "", &[excluded]);

        engine
            .install_metadata_libraries(
                &bundle,
                &OperationReporter::silent("loader-server-libraries-test"),
                &CancellationToken::new(),
            )
            .await
            .expect("an excluded library is skipped");

        assert!(!library_path(&engine, "ca.weblite:java-objc-bridge:1.1").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn synthetic_processor_is_invoked_through_its_manifest_main_class() {
        let (_root, workspace, java) = fake_processor_workspace(0);
        let manager =
            DownloadManager::new(ephervex_download::DownloaderConfig::default()).expect("manager");
        let engine = InstallerEngine::new(manager, CacheLayout::new(workspace.join("cache")));
        let processor = Processor {
            sides: Vec::new(),
            jar: "g:p:1".to_owned(),
            classpath: Vec::new(),
            args: vec!["{ROOT}/argument".to_owned()],
            outputs: BTreeMap::new(),
        };
        let values = BTreeMap::from([
            ("SIDE".to_owned(), "client".to_owned()),
            ("ROOT".to_owned(), workspace.to_string_lossy().into_owned()),
        ]);
        engine
            .run_processor(
                &processor,
                &workspace,
                &java,
                &values,
                &CancellationToken::new(),
            )
            .await
            .expect("processor succeeds");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn synthetic_processor_failure_preserves_exit_code_and_stderr() {
        let (_root, workspace, java) = fake_processor_workspace(7);
        let manager =
            DownloadManager::new(ephervex_download::DownloaderConfig::default()).expect("manager");
        let engine = InstallerEngine::new(manager, CacheLayout::new(workspace.join("cache")));
        let processor = Processor {
            sides: Vec::new(),
            jar: "g:p:1".to_owned(),
            classpath: Vec::new(),
            args: Vec::new(),
            outputs: BTreeMap::new(),
        };
        let values = BTreeMap::from([("SIDE".to_owned(), "server".to_owned())]);
        let error = engine
            .run_processor(
                &processor,
                &workspace,
                &java,
                &values,
                &CancellationToken::new(),
            )
            .await
            .expect_err("processor fails");
        assert!(matches!(
                    error,
                    LoaderError::ProcessorFailed {
                        code: Some(7),
                        ref stderr,
        ..
                    } if stderr.contains("synthetic-processor")
                ));
    }
}
