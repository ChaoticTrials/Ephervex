use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use ephervex_download::{
    CacheLayout, ContentStore, DownloadManager, DownloadRequest, FileExtension, HashVerifier,
    MavenCoordinate, MetadataCache, MetadataKey, OperationReporter,
};
use ephervex_minecraft::{Installation, RuleContext, VanillaInstaller, VersionMetadata};
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
        Self::validate_bundle(kind, minecraft, exact, &bundle)?;

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
        Self::validate_bundle(kind, minecraft, exact, &bundle)?;
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
        let key = MetadataKey::new(&format!(
            "{}-{}-{}-installer-sha1",
            kind.to_string().to_ascii_lowercase(),
            minecraft,
            exact
        ))?;
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
            .labeled(format!("{coordinate:?}:installer"));
        self.manager
            .download(&request, reporter, cancel)
            .await
            .map_err(|source| LoaderError::Download {
                artifact: format!("{coordinate:?}:installer"),
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

    fn validate_bundle(
        kind: LoaderKind,
        minecraft: &str,
        exact: &str,
        bundle: &InstallerBundle,
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
        Ok(())
    }

    async fn install_profile_libraries(
        &self,
        bundle: &InstallerBundle,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> LoaderResult<()> {
        for library in &bundle.profile.libraries {
            if cancel.is_cancelled() {
                return Err(LoaderError::Canceled);
            }
            let destination = self.layout.library(&library.coordinate);
            if fs::try_exists(&destination).await.unwrap_or(false) {
                continue;
            }
            if library.artifact.url.is_empty() {
                let installer = bundle.installer.clone();
                let entry = format!("maven/{}", library.coordinate.relative_path().display());
                let destination_copy = destination.clone();
                let expected = library.artifact.sha1.clone();
                let size = library.artifact.size;
                tokio::task::spawn_blocking(move || {
                    extract_verified_entry(&installer, &entry, &destination_copy, &expected, size)
                })
                .await
                .map_err(|error| invalid_installer(&bundle.installer, &error.to_string()))??;
            } else {
                let request = DownloadRequest::parse(&library.artifact.url, &destination)?
                    .verifying(library.artifact.sha1.clone())
                    .sized(library.artifact.size)
                    .labeled(library.name.clone());
                self.manager
                    .download(&request, reporter, cancel)
                    .await
                    .map_err(|source| LoaderError::Download {
                        artifact: library.name.clone(),
                        source: Box::new(source),
                    })?;
            }
        }
        Ok(())
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
