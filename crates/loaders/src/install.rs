use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use ephervex_download::{
    CacheLayout, ContentStore, DownloadError, DownloadManager, DownloadRequest, FileExtension,
    HashVerifier, MavenCoordinate, MetadataCache, MetadataCacheError, MetadataKey,
    OperationReporter, UrlPolicyError,
};
use ephervex_minecraft::{
    Artifact, Installation, Library, Platform, RuleContext, VanillaInstaller, VersionMetadata,
    rules_allow,
};
use ephervex_model::{Hash, HashAlgorithm, LoaderKind, LoaderSelection};
use reqwest::Url;
use tokio::fs;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;
use tracing::{debug, instrument};
use zip::ZipArchive;

use crate::profile::{InstallProfile, InstallSide, Processor, ProfileArtifact};
use crate::{LoaderError, LoaderResult};

const FORGE_MAVEN: &str = "https://maven.minecraftforge.net";
const NEOFORGE_MAVEN: &str = "https://maven.neoforged.net/releases";

/// Digests an installer may publish as `<installer>.jar.<algorithm>`, strongest first.
const INSTALLER_DIGESTS: [HashAlgorithm; 3] = [
    HashAlgorithm::Sha512,
    HashAlgorithm::Sha256,
    HashAlgorithm::Sha1,
];
const MAX_PROFILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_PROFILE_DATA_BYTES: u64 = 128 * 1024 * 1024;
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

/// Workspace files with a published digest, by workspace path.
type KnownDigests = HashMap<PathBuf, Hash>;

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
        let staging = ScratchDirectory::create(server_staging(target)).await?;
        let jar = staging
            .path()
            .join(format!("minecraft_server.{}.jar", metadata.id));
        let downloaded = async {
            let artifact = metadata.server_jar()?.clone();
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
                })
        }
        .await;
        staging.settle(downloaded, Some(target)).await?;
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
        ensure_consistent_library_artifacts(&bundle, parent, context, "vanilla parent metadata")?;

        let vanilla = self
            .install_vanilla_client(parent, context, reporter, cancel)
            .await?;
        self.install_profile_libraries(&bundle, reporter, cancel)
            .await?;
        self.install_installer_declared_libraries(&bundle, context)
            .await?;

        if bundle.profile.is_processor_driven() {
            self.ensure_client_processed(
                &bundle,
                parent,
                &vanilla.installation.plan.client_jar.path,
                java,
                context,
                reporter,
                cancel,
            )
            .await?;
        }

        let mut metadata = VersionMetadata::inherit(parent, &bundle.child)?;
        self.mark_preinstalled_artifacts(&mut metadata, context)
            .await?;
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
        // A server runs on this host, so every step selects libraries by its rules.
        let context = RuleContext::new(Platform::host());
        let bundle = self
            .fetch_bundle(kind, minecraft, exact, reporter, cancel)
            .await?;
        Self::validate_bundle(kind, minecraft, exact, &bundle, &context)?;
        self.install_metadata_libraries(&bundle, &context, reporter, cancel)
            .await?;
        self.install_profile_libraries(&bundle, reporter, cancel)
            .await?;
        self.install_installer_declared_libraries(&bundle, &context)
            .await?;

        let scratch = ScratchDirectory::create(server_staging(target)).await?;
        let staging = scratch.path();
        let staged = async {
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

            Ok(if bundle.profile.is_processor_driven() {
                self.prepare_workspace_in(staging, &bundle.profile).await?;
                self.prepare_mojang_mappings(
                    &bundle.profile,
                    InstallSide::Server,
                    parent,
                    staging,
                    reporter,
                    cancel,
                )
                .await?;
                self.run_and_publish_processors(
                    &bundle,
                    InstallSide::Server,
                    staging,
                    &server_path,
                    java,
                    &context,
                    cancel,
                )
                .await?;
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
                    ServerLaunch::Jar {
                        jar: target.join(self.stage_profile_jar(&bundle, staging).await?),
                    }
                };
                replace_with_shared_libraries(&self.layout, staging).await?;
                launch
            } else {
                let jar = self.stage_profile_jar(&bundle, staging).await?;
                link_shared_libraries(&self.layout, staging).await?;
                ServerLaunch::Jar {
                    jar: target.join(jar),
                }
            })
        }
        .await;
        let launch = scratch.settle(staged, Some(target)).await?;
        Ok(InstalledServer {
            selection: LoaderSelection::exact(kind, exact),
            minecraft: minecraft.to_owned(),
            root: target.to_path_buf(),
            launch,
        })
    }

    /// Resolves the strongest digest the installer's Maven publishes beside the jar.
    ///
    /// Only a sidecar the Maven confirms absent moves the choice to a weaker
    /// digest, because the oldest builds publish SHA-1 alone. When the Maven
    /// cannot be reached, a weaker sidecar is used only if an earlier lookup
    /// already stored it, so a relaunch works offline. Any other failure, or a
    /// sidecar that is not a valid digest, ends the install.
    async fn installer_digest(
        &self,
        kind: LoaderKind,
        minecraft: &str,
        exact: &str,
        jar_url: &str,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> LoaderResult<Hash> {
        let metadata = MetadataCache::new(self.layout.clone());
        let mut unreachable = None;
        for (index, algorithm) in INSTALLER_DIGESTS.into_iter().enumerate() {
            let sidecar_url = Url::parse(&format!("{jar_url}.{algorithm}")).map_err(|_| {
                LoaderError::MetadataInvalid {
                    provider: "installer",
                    detail: "installer sidecar URL is invalid".to_owned(),
                }
            })?;
            let key = installer_sidecar_key(kind, minecraft, exact, algorithm)?;
            let sidecar = if unreachable.is_some() {
                match metadata.peek(&key).await {
                    Some(sidecar) => sidecar,
                    None => continue,
                }
            } else {
                match metadata
                    .fetch_text(&self.manager, &key, &sidecar_url, reporter, cancel)
                    .await
                {
                    Ok(sidecar) => sidecar,
                    Err(error)
                        if index + 1 < INSTALLER_DIGESTS.len() && sidecar_is_absent(&error) =>
                    {
                        debug!(%algorithm, "installer publishes no sidecar for this digest");
                        continue;
                    }
                    Err(error) if sidecar_is_unreachable(&error) => {
                        unreachable = Some(error);
                        continue;
                    }
                    Err(source) => {
                        return Err(LoaderError::MetadataUnavailable {
                            provider: "installer",
                            source: Box::new(source),
                        });
                    }
                }
            };
            let text = sidecar
                .text()
                .map_err(|error| LoaderError::MetadataInvalid {
                    provider: "installer",
                    detail: error.to_string(),
                })?;
            let digest = text.split_whitespace().next().unwrap_or_default();
            return Hash::new(algorithm, digest).ok_or_else(|| LoaderError::MetadataInvalid {
                provider: "installer",
                detail: format!("installer sidecar has no {algorithm} digest"),
            });
        }
        Err(unreachable.map_or_else(
            || LoaderError::MetadataInvalid {
                provider: "installer",
                detail: "installer publishes no digest".to_owned(),
            },
            |source| LoaderError::MetadataUnavailable {
                provider: "installer",
                source: Box::new(source),
            },
        ))
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
        let digest = self
            .installer_digest(kind, minecraft, exact, &jar_url, reporter, cancel)
            .await?;
        let jar = FileExtension::new("jar")?;
        let installer = self
            .layout
            .content(ContentStore::InstallerCache, &digest, Some(&jar));
        let request = DownloadRequest::parse(&jar_url, &installer)?
            .verifying(digest)
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

    /// Checks the bundle's identity and library artifacts under `context`'s rules.
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
        ensure_consistent_library_artifacts(bundle, &bundle.child, context, "version.json")
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
                if stored_copy_matches(
                    &destination,
                    &library.artifact.sha1,
                    Some(library.artifact.size),
                )
                .await?
                {
                    continue;
                }
                let installer = bundle.installer.clone();
                let entry = format!("maven/{}", library.coordinate.relative_path().display());
                let expected = library.artifact.sha1.clone();
                let size = library.artifact.size;
                let bytes = tokio::task::spawn_blocking(move || {
                    read_verified_entry(&installer, &entry, &expected, size)
                })
                .await
                .map_err(|error| invalid_installer(&bundle.installer, &error.to_string()))??;
                publish_bytes(&destination, &bytes).await?;
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
        context: &RuleContext,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> LoaderResult<()> {
        // The first entry for a cache path wins, as it does when a client plans the same list.
        let mut planned = HashSet::new();
        let mut requests = Vec::new();
        let mut names = Vec::new();
        for library in &bundle.child.libraries {
            let Some(artifact) = library.downloads.artifact.as_ref() else {
                continue;
            };
            if !rules_allow(&library.rules, context)
                || !planned.insert(library.coordinate.relative_path())
            {
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
        context: &RuleContext,
    ) -> LoaderResult<()> {
        // The first entry for a coordinate the host's rules allow is the one an install stores.
        let mut planned = HashSet::new();
        for library in &bundle.child.libraries {
            let Some(artifact) = library.downloads.artifact.as_ref() else {
                continue;
            };
            if !rules_allow(&library.rules, context) || !planned.insert(&library.coordinate) {
                continue;
            }
            if !artifact.url.is_empty() {
                continue;
            }
            let installer = bundle.installer.clone();
            let entry = format!("maven/{}", library.coordinate.relative_path().display());
            let carried = {
                let (installer, entry) = (installer.clone(), entry.clone());
                tokio::task::spawn_blocking(move || zip_entry_exists(&installer, &entry))
                    .await
                    .map_err(|error| invalid_installer(&bundle.installer, &error.to_string()))??
            };
            // Later eras declare processor outputs this way; those arrive when the processors run.
            if !carried {
                continue;
            }
            let destination = self.layout.library(&library.coordinate);
            if stored_copy_matches(&destination, &artifact.sha1, Some(artifact.size)).await? {
                continue;
            }
            let expected = artifact.sha1.clone();
            let size = artifact.size;
            let bytes = tokio::task::spawn_blocking(move || {
                read_verified_entry(&installer, &entry, &expected, size)
            })
            .await
            .map_err(|error| invalid_installer(&bundle.installer, &error.to_string()))??;
            publish_bytes(&destination, &bytes).await?;
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

    /// Runs the client processors unless the shared libraries store already
    /// holds what they produce (see `processor_outputs_published`). A relaunch
    /// then needs no processor run and no network access.
    #[allow(clippy::too_many_arguments)]
    async fn ensure_client_processed(
        &self,
        bundle: &InstallerBundle,
        parent: &VersionMetadata,
        minecraft_jar: &Path,
        java: &Path,
        context: &RuleContext,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> LoaderResult<()> {
        if self
            .processor_outputs_published(bundle, InstallSide::Client, minecraft_jar, context)
            .await?
        {
            return Ok(());
        }
        let scratch = ScratchDirectory::create(self.workspace("client")).await?;
        let workspace = scratch.path();
        let processed = async {
            self.prepare_workspace_in(workspace, &bundle.profile)
                .await?;
            self.prepare_mojang_mappings(
                &bundle.profile,
                InstallSide::Client,
                parent,
                workspace,
                reporter,
                cancel,
            )
            .await?;
            self.run_and_publish_processors(
                bundle,
                InstallSide::Client,
                workspace,
                minecraft_jar,
                java,
                context,
                cancel,
            )
            .await
        }
        .await;
        scratch.settle(processed, None).await
    }

    /// Whether a previous install already published everything the processors
    /// produce. A file with a declared digest must verify: each declared
    /// output, each `<NAME>_SHA` data value, and each address-less library the
    /// version metadata declares (digest and size). A referenced data product
    /// with no digest, such as the remapped game jar or NeoForge's patched
    /// game, is accepted when stored, because the store only receives files
    /// published atomically after a completed processor run. Anything missing,
    /// mismatched, or outside the store means the processors run again.
    async fn processor_outputs_published(
        &self,
        bundle: &InstallerBundle,
        side: InstallSide,
        minecraft_jar: &Path,
        context: &RuleContext,
    ) -> LoaderResult<bool> {
        // Resolves outputs as the processors would see them; nothing is written here.
        let root = self.layout.root().join("published-outputs");
        let values = processor_values(bundle, side, &root, minecraft_jar)?;
        let libraries = root.join("libraries");
        let store = self.layout.store_root(ContentStore::Libraries);
        let stored = |path: &Path| -> Option<PathBuf> {
            let relative = path.strip_prefix(&libraries).ok()?;
            relative
                .components()
                .all(|part| matches!(part, std::path::Component::Normal(_)))
                .then(|| store.join(relative))
        };

        let mut checked = 0_usize;
        for processor in bundle
            .profile
            .processors
            .iter()
            .filter(|processor| processor.applies_to(side))
        {
            for (output, expected) in &processor.outputs {
                let path = PathBuf::from(resolve_tokens(output, &values)?);
                let digest = resolve_tokens(expected, &values)?;
                let (Some(path), Some(digest)) = (
                    stored(&path),
                    Hash::new(HashAlgorithm::Sha1, digest.trim_matches('\'')),
                ) else {
                    return Ok(false);
                };
                if !stored_copy_matches(&path, &digest, None).await? {
                    return Ok(false);
                }
                checked += 1;
            }
        }
        // The processors also write libraries nothing declares an output for, such
        // as the remapped game jar; the game needs them, so each must be stored.
        // A value only the other side's processors name is never produced here.
        // The patched game is always required; a profile that publishes no
        // digest for it, as NeoForge's does not, is accepted on presence.
        let referenced = referenced_tokens(&bundle.profile, side);
        for name in bundle
            .profile
            .data
            .keys()
            .filter(|name| *name == "PATCHED" || referenced.contains(name.as_str()))
        {
            let raw = bundle.profile.data_value(name, side).unwrap_or_default();
            if !(raw.starts_with('[') && raw.ends_with(']')) {
                if name == "PATCHED" {
                    return Ok(false);
                }
                continue;
            }
            let Some(path) = values
                .get(name)
                .and_then(|resolved| stored(Path::new(resolved)))
            else {
                return Ok(false);
            };
            match values.get(&format!("{name}_SHA")) {
                Some(digest) => {
                    let Some(digest) = Hash::new(HashAlgorithm::Sha1, digest) else {
                        return Ok(false);
                    };
                    if !stored_copy_matches(&path, &digest, None).await? {
                        return Ok(false);
                    }
                }
                None => {
                    if !fs::metadata(&path)
                        .await
                        .is_ok_and(|metadata| metadata.is_file())
                    {
                        return Ok(false);
                    }
                }
            }
            checked += 1;
        }
        for (library, artifact) in addressless_artifacts(&bundle.child.libraries, context) {
            let path = self.layout.library(&library.coordinate);
            if !stored_copy_matches(&path, &artifact.sha1, Some(artifact.size)).await? {
                return Ok(false);
            }
            checked += 1;
        }
        Ok(checked > 0)
    }

    fn workspace(&self, side: &str) -> PathBuf {
        let sequence = WORKSPACE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        self.layout
            .store_root(ContentStore::InstallerCache)
            .join("work")
            .join(format!("{side}-{}-{sequence}", std::process::id()))
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

    /// Runs the processors in `workspace` and publishes the libraries they
    /// produced.
    #[allow(clippy::too_many_arguments)]
    async fn run_and_publish_processors(
        &self,
        bundle: &InstallerBundle,
        side: InstallSide,
        workspace: &Path,
        minecraft_jar: &Path,
        java: &Path,
        context: &RuleContext,
        cancel: &CancellationToken,
    ) -> LoaderResult<()> {
        let verified = self
            .run_processors(bundle, side, workspace, minecraft_jar, java, cancel)
            .await?;
        self.publish_libraries(bundle, context, &workspace.join("libraries"), verified)
            .await
    }

    async fn run_processors(
        &self,
        bundle: &InstallerBundle,
        side: InstallSide,
        workspace: &Path,
        minecraft_jar: &Path,
        java: &Path,
        cancel: &CancellationToken,
    ) -> LoaderResult<KnownDigests> {
        self.extract_profile_data(bundle, workspace).await?;
        let mut verified = KnownDigests::new();
        let values = processor_values(bundle, side, workspace, minecraft_jar)?;

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
            self.verify_processor_outputs(processor, &values, &mut verified)
                .await?;
        }

        if let Some(patched) = values.get("PATCHED") {
            let path = PathBuf::from(patched);
            if !fs::try_exists(&path).await.unwrap_or(false) {
                return Err(LoaderError::ProcessorOutputMissing { path });
            }
            if let Some(expected) = values.get("PATCHED_SHA") {
                let hash = verify_file_hash(&path, expected).await?;
                verified.insert(path, hash);
            }
        }
        Ok(verified)
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
                    let bytes = tokio::task::spawn_blocking(move || {
                        read_zip_bytes(&installer, &entry, MAX_PROFILE_DATA_BYTES)
                    })
                    .await
                    .map_err(|error| invalid_installer(&bundle.installer, &error.to_string()))??;
                    write_private(&destination, &bytes).await?;
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
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|source| io_error(java, source))?;
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut stderr = AbortOnDrop(tokio::spawn(capture_stderr(
            child.stderr.take(),
            captured.clone(),
        )));
        let status = tokio::select! {
            () = cancel.cancelled() => None,
            status = child.wait() => Some(status.map_err(|source| io_error(java, source))?),
        };
        let Some(status) = status else {
            // Reaped, so it has stopped writing before its workspace is removed.
            let _ = child.kill().await;
            return Err(LoaderError::Canceled);
        };
        if status.success() {
            return Ok(());
        }
        // A child the processor left behind may hold the pipe open past its exit.
        tokio::select! {
            () = cancel.cancelled() => return Err(LoaderError::Canceled),
            _ = tokio::time::timeout(std::time::Duration::from_secs(5), &mut stderr.0) => {}
        }
        drop(stderr);
        let stderr = captured
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        Err(LoaderError::ProcessorFailed {
            processor: processor.jar.clone(),
            code: status.code(),
            stderr: String::from_utf8_lossy(&stderr)
                .chars()
                .take(4096)
                .collect(),
        })
    }

    async fn verify_processor_outputs(
        &self,
        processor: &Processor,
        values: &BTreeMap<String, String>,
        verified: &mut KnownDigests,
    ) -> LoaderResult<()> {
        for (output, expected) in &processor.outputs {
            let path = PathBuf::from(resolve_tokens(output, values)?);
            if !fs::try_exists(&path).await.unwrap_or(false) {
                return Err(LoaderError::ProcessorOutputMissing { path });
            }
            let hash = resolve_tokens(expected, values)?;
            let hash = verify_file_hash(&path, hash.trim_matches('\'')).await?;
            verified.insert(path, hash);
        }
        Ok(())
    }

    async fn publish_libraries(
        &self,
        bundle: &InstallerBundle,
        context: &RuleContext,
        source: &Path,
        mut known: KnownDigests,
    ) -> LoaderResult<()> {
        // version.json publishes digests for the files with no address, and the
        // entry an install stores is the first one the host's rules allow.
        for (library, artifact) in addressless_artifacts(&bundle.child.libraries, context) {
            // A processor digest from install_profile.json must agree with version.json.
            match known.entry(source.join(library.coordinate.relative_path())) {
                std::collections::hash_map::Entry::Occupied(processed)
                    if *processed.get() != artifact.sha1 =>
                {
                    return Err(LoaderError::InstallerLibraryConflict {
                        path: bundle.installer.clone(),
                        library: library.name.clone(),
                        profile: Box::new(processed.get().clone()),
                        version: Box::new(artifact.sha1.clone()),
                        // Processor outputs declare a digest but no size; use the verified file.
                        profile_size: fs::metadata(processed.key())
                            .await
                            .map_err(|source| io_error(processed.key(), source))?
                            .len(),
                        version_size: artifact.size,
                        version_document: "version.json",
                    });
                }
                std::collections::hash_map::Entry::Occupied(_) => {}
                std::collections::hash_map::Entry::Vacant(missing) => {
                    missing.insert(artifact.sha1.clone());
                }
            }
        }
        let destination = self.layout.store_root(ContentStore::Libraries);
        publish_tree(source, &destination, &known).await
    }

    async fn mark_preinstalled_artifacts(
        &self,
        metadata: &mut VersionMetadata,
        context: &RuleContext,
    ) -> LoaderResult<()> {
        // Only the first entry for a coordinate the host's rules allow is stored.
        let stored: Vec<bool> = {
            let mut planned = HashSet::new();
            metadata
                .libraries
                .iter()
                .map(|library| {
                    library.downloads.artifact.is_some()
                        && rules_allow(&library.rules, context)
                        && planned.insert(&library.coordinate)
                })
                .collect()
        };
        for (library, stored) in metadata.libraries.iter_mut().zip(stored) {
            let Some(artifact) = library.downloads.artifact.as_mut() else {
                continue;
            };
            if !artifact.url.is_empty() {
                continue;
            }
            // Every producer of these files verifies and publishes atomically.
            let path = self.layout.library(&library.coordinate);
            if stored && !fs::try_exists(&path).await.unwrap_or(false) {
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
    algorithm: HashAlgorithm,
) -> LoaderResult<MetadataKey> {
    Ok(MetadataKey::new(&format!(
        "{}-{}-{}-installer-{algorithm}",
        kind.to_string().to_ascii_lowercase(),
        minecraft,
        exact
    ))?)
}

/// Whether the Maven could not be reached, as opposed to answering with a refusal.
fn sidecar_is_unreachable(error: &MetadataCacheError) -> bool {
    matches!(
        error,
        MetadataCacheError::Fetch { source, .. } | MetadataCacheError::Unavailable { source, .. }
            if matches!(
                **source,
                DownloadError::Transport { .. } | DownloadError::RetriesExhausted { .. }
            )
    )
}

/// Whether the Maven answered that the sidecar does not exist, as opposed to failing to answer.
fn sidecar_is_absent(error: &MetadataCacheError) -> bool {
    matches!(
        error,
        MetadataCacheError::Fetch { source, .. } | MetadataCacheError::Unavailable { source, .. }
            if matches!(**source, DownloadError::HttpStatus { status: 404, .. })
    )
}

/// Refuses profile and version metadata that would store different files at one cache path.
/// Each side contributes the entry an install stores; a path only one side stores is no conflict.
fn ensure_consistent_library_artifacts(
    bundle: &InstallerBundle,
    metadata: &VersionMetadata,
    context: &RuleContext,
    version_document: &'static str,
) -> LoaderResult<()> {
    // A dotted classifier and a compound extension can resolve to the same file.
    // Compare storage paths so aliases cannot hide a conflicting declaration.
    let mut profile: HashMap<PathBuf, &ProfileArtifact> = HashMap::new();
    for declared in &bundle.profile.libraries {
        profile
            .entry(declared.coordinate.relative_path())
            .or_insert(&declared.artifact);
    }
    let mut stored = HashSet::new();
    for library in &metadata.libraries {
        let Some(artifact) = library.downloads.artifact.as_ref() else {
            continue;
        };
        if !rules_allow(&library.rules, context)
            || !stored.insert(library.coordinate.relative_path())
        {
            continue;
        }
        if let Some(declared) = profile.get(&library.coordinate.relative_path()).copied()
            && (declared.sha1 != artifact.sha1 || declared.size != artifact.size)
        {
            return Err(LoaderError::InstallerLibraryConflict {
                path: bundle.installer.clone(),
                library: library.name.clone(),
                profile: Box::new(declared.sha1.clone()),
                version: Box::new(artifact.sha1.clone()),
                profile_size: declared.size,
                version_size: artifact.size,
                version_document,
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

fn read_verified_entry(
    installer: &Path,
    entry: &str,
    expected: &Hash,
    size: u64,
) -> LoaderResult<Vec<u8>> {
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
    Ok(bytes)
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

/// The artifacts an install stores for `libraries` that carry no address, which
/// a loader's processors produce rather than download. Only the first entry per
/// coordinate that the host's rules allow is stored.
fn addressless_artifacts<'a>(
    libraries: &'a [Library],
    context: &RuleContext,
) -> Vec<(&'a Library, &'a Artifact)> {
    let mut planned = HashSet::new();
    libraries
        .iter()
        .filter_map(|library| {
            let artifact = library.downloads.artifact.as_ref()?;
            (rules_allow(&library.rules, context) && planned.insert(&library.coordinate))
                .then_some((library, artifact))
        })
        .filter(|(_, artifact)| artifact.url.is_empty())
        .collect()
}

/// The tokens a processor argument or output may name, with Maven-coordinate
/// and installer-entry values resolved inside `workspace`.
fn processor_values(
    bundle: &InstallerBundle,
    side: InstallSide,
    workspace: &Path,
    minecraft_jar: &Path,
) -> LoaderResult<BTreeMap<String, String>> {
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
    Ok(values)
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

/// The names of the tokens the processors that run on `side` take as arguments
/// or declare as outputs.
fn referenced_tokens(profile: &InstallProfile, side: InstallSide) -> HashSet<&str> {
    profile
        .processors
        .iter()
        .filter(|processor| processor.applies_to(side))
        .flat_map(|processor| {
            let outputs = processor
                .outputs
                .iter()
                .flat_map(|(key, value)| [key, value]);
            processor.args.iter().chain(outputs)
        })
        .flat_map(|text| token_names(text))
        .collect()
}

/// The names of the `{NAME}` tokens in `raw`.
fn token_names(raw: &str) -> impl Iterator<Item = &str> {
    raw.split('{')
        .skip(1)
        .filter_map(|tail| tail.split_once('}').map(|(name, _)| name))
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

async fn verify_file_hash(path: &Path, expected: &str) -> LoaderResult<Hash> {
    let expected = Hash::new(HashAlgorithm::Sha1, expected)
        .ok_or_else(|| invalid_installer(path, "processor output declares a malformed SHA-1"))?;
    let matches = ephervex_download::file_matches_hashes(path, std::slice::from_ref(&expected))
        .await
        .map_err(|error| local_read_error(path, error))?;
    if !matches {
        return Err(digest_mismatch(path, path, &expected).await);
    }
    Ok(expected)
}

/// Whether a file is the artifact with this digest, and size when known.
/// Presence alone is not proof where the digest is known.
async fn stored_copy_matches(path: &Path, sha1: &Hash, size: Option<u64>) -> LoaderResult<bool> {
    ephervex_download::stored_file_matches(path, size, std::slice::from_ref(sha1))
        .await
        .map_err(|error| local_read_error(path, error))
}

/// A failure reading a local file while checking its digest, which is file I/O
/// rather than a download.
fn local_read_error(path: &Path, error: ephervex_download::DownloadError) -> LoaderError {
    match error {
        ephervex_download::DownloadError::Io { path, source, .. } => io_error(&path, source),
        other => io_error(path, std::io::Error::other(other)),
    }
}

/// Publishes `bytes` at `destination` by an atomic rename, replacing whatever
/// is there.
async fn publish_bytes(destination: &Path, bytes: &[u8]) -> LoaderResult<()> {
    stage_and_publish(destination, destination, None, |staged| async move {
        fs::write(staged, bytes).await
    })
    .await
}

/// Copies `source` to `destination` by an atomic rename, replacing whatever is
/// there.
async fn copy_atomic(
    source: &Path,
    destination: &Path,
    expected: Option<&Hash>,
) -> LoaderResult<()> {
    stage_and_publish(destination, source, expected, |staged| async move {
        fs::copy(source, staged).await.map(drop)
    })
    .await
}

/// Writes a staging sibling, checks it against `expected` when given, and
/// renames it over `destination`, so only checked, complete bytes ever appear
/// in a shared store. A failed write or mismatch is reported against `origin`.
async fn stage_and_publish<F, Fut>(
    destination: &Path,
    origin: &Path,
    expected: Option<&Hash>,
    write: F,
) -> LoaderResult<()>
where
    F: FnOnce(PathBuf) -> Fut,
    Fut: std::future::Future<Output = std::io::Result<()>>,
{
    let parent = destination
        .parent()
        .ok_or_else(|| invalid_installer(destination, "destination has no parent"))?;
    fs::create_dir_all(parent)
        .await
        .map_err(|source| io_error(parent, source))?;
    let staged = ephervex_download::staging_path(destination);
    let published = async {
        write(staged.clone())
            .await
            .map_err(|source| io_error(origin, source))?;
        if let Some(expected) = expected
            && !stored_copy_matches(&staged, expected, None).await?
        {
            return Err(digest_mismatch(origin, &staged, expected).await);
        }
        ephervex_download::publish_staged(&staged, destination)
            .await
            .map_err(|source| io_error(destination, source))
    }
    .await;
    if published.is_err() {
        let _ = fs::remove_file(&staged).await;
    }
    published
}

/// A refusal naming the published digest and the one `actual` has.
async fn digest_mismatch(path: &Path, actual: &Path, expected: &Hash) -> LoaderError {
    let found = ephervex_download::hash_file(actual, HashAlgorithm::Sha1)
        .await
        .map_or_else(
            |_| "an unreadable file".to_owned(),
            |hash| hash.digest().to_owned(),
        );
    invalid_installer(
        path,
        &format!(
            "processor output does not match its published digest: expected {}, found {found}",
            expected.digest()
        ),
    )
}

/// Writes into a private workspace, which nothing else reads until the install
/// is done with it.
async fn write_private(destination: &Path, bytes: &[u8]) -> LoaderResult<()> {
    let parent = destination
        .parent()
        .ok_or_else(|| invalid_installer(destination, "destination has no parent"))?;
    fs::create_dir_all(parent)
        .await
        .map_err(|source| io_error(parent, source))?;
    fs::write(destination, bytes)
        .await
        .map_err(|source| io_error(destination, source))
}

/// Copies into a private workspace or staging tree, which nothing else reads
/// until the install is done with it.
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

/// Whether `target` is a regular file of `source`'s size.
async fn same_size(source: &Path, target: &Path) -> LoaderResult<bool> {
    let source_size = fs::metadata(source)
        .await
        .map_err(|error| io_error(source, error))?
        .len();
    Ok(fs::symlink_metadata(target)
        .await
        .is_ok_and(|target| target.is_file() && target.len() == source_size))
}

/// Publishes a processor workspace's libraries into the shared store.
///
/// A store file whose digest is known is checked and replaced when it does not
/// match, and the replacement is checked before it is renamed into place. One
/// with no known digest is kept while its size matches the fresh output.
async fn publish_tree(source: &Path, destination: &Path, known: &KnownDigests) -> LoaderResult<()> {
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
            let path = entry.path();
            let target = to.join(entry.file_name());
            if kind.is_dir() {
                pending.push((path, target));
            } else if kind.is_file() {
                let expected = known.get(&path);
                let current = match expected {
                    Some(hash) => stored_copy_matches(&target, hash, None).await?,
                    // A file an earlier, non-atomic publication truncated differs in size.
                    None => same_size(&path, &target).await?,
                };
                if !current {
                    copy_atomic(&path, &target, expected).await?;
                }
            } else {
                return Err(invalid_installer(&path, "processor produced a symlink"));
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

/// An installer-owned scratch directory. The install settles it, publishing or
/// removing it; if the install is dropped first, it is removed off the async
/// runtime.
#[derive(Debug)]
struct ScratchDirectory {
    /// `None` once the directory has been published or removed.
    path: Option<PathBuf>,
}

impl ScratchDirectory {
    /// Creates an empty directory at `path`, clearing anything an earlier
    /// process with the same id left there.
    async fn create(path: PathBuf) -> LoaderResult<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|source| io_error(parent, source))?;
        }
        remove_tree(&path).await;
        fs::create_dir(&path)
            .await
            .map_err(|source| io_error(&path, source))?;
        Ok(Self { path: Some(path) })
    }

    fn path(&self) -> &Path {
        self.path.as_deref().unwrap_or_else(|| Path::new(""))
    }

    /// Renames the directory to `target` when the work succeeded and one is
    /// given, so `target` owns its contents, and removes it otherwise.
    async fn settle<T>(
        mut self,
        result: LoaderResult<T>,
        target: Option<&Path>,
    ) -> LoaderResult<T> {
        let publish_to = target.filter(|_| result.is_ok());
        let (Some(target), Some(path)) = (publish_to, self.path.as_deref()) else {
            self.remove().await;
            return result;
        };
        // One synchronous call, so a dropped install cannot interrupt it halfway.
        if let Err(source) = std::fs::rename(path, target) {
            self.remove().await;
            return Err(io_error(target, source));
        }
        self.path = None;
        result
    }

    async fn remove(mut self) {
        let Some(path) = self.path.take() else {
            return;
        };
        let retried = path.clone();
        // The blocking pool refuses work while the runtime shuts down.
        if tokio::task::spawn_blocking(move || remove_scratch(&retried))
            .await
            .is_err()
        {
            remove_scratch(&path);
        }
    }
}

impl Drop for ScratchDirectory {
    fn drop(&mut self) {
        let Some(path) = self.path.take() else {
            return;
        };
        if tokio::runtime::Handle::try_current().is_err() {
            remove_scratch(&path);
            return;
        }
        // Its own thread, not the runtime's blocking pool, which refuses work
        // while the runtime shuts down.
        let retried = path.clone();
        if std::thread::Builder::new()
            .spawn(move || remove_scratch(&retried))
            .is_err()
        {
            remove_scratch(&path);
        }
    }
}

/// Removes a scratch directory. A dropped or failed install's detached writes
/// and killed processor may still be finishing, so a removal that races them
/// is tried again shortly.
fn remove_scratch(path: &Path) {
    let mut attempts = 0;
    loop {
        match std::fs::remove_dir_all(path) {
            Ok(()) => return,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(_) if attempts < 3 => {
                attempts += 1;
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            Err(error) => {
                tracing::warn!(
                    path = %path.display(),
                    %error,
                    "could not remove loader staging directory"
                );
                return;
            }
        }
    }
}

fn server_staging(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("server");
    target.with_file_name(format!(
        ".{name}.ephervex-installing-{}-{}",
        std::process::id(),
        WORKSPACE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
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

/// Aborts a background task on every exit from the scope that owns it.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// How much of a processor's stderr is kept for its failure report.
const MAX_PROCESSOR_STDERR_BYTES: usize = 16 * 1024;

/// Reads a processor's stderr to its end, keeping the start in `captured`.
async fn capture_stderr(
    pipe: Option<tokio::process::ChildStderr>,
    captured: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
) {
    let Some(mut pipe) = pipe else {
        return;
    };
    let mut chunk = [0_u8; 8192];
    loop {
        let read = match tokio::io::AsyncReadExt::read(&mut pipe, &mut chunk).await {
            Ok(0) => return,
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return,
        };
        let mut kept = captured
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let room = MAX_PROCESSOR_STDERR_BYTES.saturating_sub(kept.len());
        kept.extend_from_slice(&chunk[..read.min(room)]);
    }
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

    fn parent_declaring(libraries: &[String]) -> VersionMetadata {
        let mut parent = vanilla_parent();
        parent.libraries = VersionMetadata::parse(&version_json(libraries))
            .expect("parent libraries")
            .libraries;
        parent
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

        let digest = ephervex_download::hash_file(&staged, HashAlgorithm::Sha512)
            .await
            .expect("installer digest");
        let jar = FileExtension::new("jar").expect("extension");
        let cached = engine
            .layout
            .content(ContentStore::InstallerCache, &digest, Some(&jar));
        std::fs::create_dir_all(cached.parent().expect("parent")).expect("installer cache");
        std::fs::rename(&staged, &cached).expect("cached installer");

        let (_, jar_url) =
            installer_location(LoaderKind::Forge, "1.20.1", "47.4.10").expect("installer");
        MetadataCache::new(engine.layout.clone())
            .store(
                &installer_sidecar_key(
                    LoaderKind::Forge,
                    "1.20.1",
                    "47.4.10",
                    HashAlgorithm::Sha512,
                )
                .expect("key"),
                &Url::parse(&format!("{jar_url}.sha512")).expect("sidecar url"),
                serde_json::value::to_raw_value(digest.digest()).expect("sidecar"),
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

    /// Serves `installer.jar.<algorithm>` sidecars: a digest for the listed
    /// algorithms, the given status for the others. Returns the jar URL.
    async fn serve_installer_sidecars(
        server: &wiremock::MockServer,
        published: &[(HashAlgorithm, &str)],
        status: u16,
    ) -> String {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, ResponseTemplate};

        for algorithm in HashAlgorithm::ALL {
            let body = published
                .iter()
                .find(|(published, _)| *published == algorithm)
                .map(|(_, digest)| format!("{digest}\n"));
            let response = match body {
                Some(body) => ResponseTemplate::new(200).set_body_string(body),
                None => ResponseTemplate::new(status),
            };
            Mock::given(method("GET"))
                .and(path(format!("/installer.jar.{algorithm}")))
                .respond_with(response)
                .mount(server)
                .await;
        }
        format!("{}/installer.jar", server.uri())
    }

    async fn resolve_installer_digest(
        engine: &InstallerEngine,
        jar_url: &str,
    ) -> LoaderResult<Hash> {
        engine
            .installer_digest(
                LoaderKind::Forge,
                "1.20.1",
                "47.4.10",
                jar_url,
                &OperationReporter::silent("installer-digest-test"),
                &CancellationToken::new(),
            )
            .await
    }

    #[tokio::test]
    async fn an_installer_is_verified_with_the_strongest_digest_its_maven_publishes() {
        let server = wiremock::MockServer::start().await;
        let sha512 = "5".repeat(128);
        let jar_url = serve_installer_sidecars(
            &server,
            &[
                (HashAlgorithm::Sha512, &sha512),
                (HashAlgorithm::Sha256, &"2".repeat(64)),
                (HashAlgorithm::Sha1, ABC_SHA1),
            ],
            404,
        )
        .await;
        let root = tempfile::tempdir().expect("root");
        let engine = loopback_engine(root.path());

        let digest = resolve_installer_digest(&engine, &jar_url)
            .await
            .expect("digest");

        assert_eq!(digest.algorithm, HashAlgorithm::Sha512);
        assert_eq!(digest.digest(), sha512);
    }

    #[tokio::test]
    async fn an_installer_with_no_sha512_sidecar_is_verified_with_sha256() {
        let server = wiremock::MockServer::start().await;
        let sha256 = "2".repeat(64);
        let jar_url = serve_installer_sidecars(
            &server,
            &[
                (HashAlgorithm::Sha256, &sha256),
                (HashAlgorithm::Sha1, ABC_SHA1),
            ],
            404,
        )
        .await;
        let root = tempfile::tempdir().expect("root");
        let engine = loopback_engine(root.path());

        let digest = resolve_installer_digest(&engine, &jar_url)
            .await
            .expect("digest");

        assert_eq!(digest.algorithm, HashAlgorithm::Sha256);
    }

    #[tokio::test]
    async fn an_installer_publishing_only_sha1_is_verified_with_sha1() {
        let server = wiremock::MockServer::start().await;
        let jar_url =
            serve_installer_sidecars(&server, &[(HashAlgorithm::Sha1, ABC_SHA1)], 404).await;
        let root = tempfile::tempdir().expect("root");
        let engine = loopback_engine(root.path());

        let digest = resolve_installer_digest(&engine, &jar_url)
            .await
            .expect("digest");

        assert_eq!(digest.algorithm, HashAlgorithm::Sha1);
        assert_eq!(digest.digest(), ABC_SHA1);
    }

    #[tokio::test]
    async fn a_stronger_sidecar_that_fails_for_another_reason_does_not_fall_back_to_sha1() {
        let server = wiremock::MockServer::start().await;
        let jar_url =
            serve_installer_sidecars(&server, &[(HashAlgorithm::Sha1, ABC_SHA1)], 403).await;
        let root = tempfile::tempdir().expect("root");
        let engine = loopback_engine(root.path());

        let error = resolve_installer_digest(&engine, &jar_url)
            .await
            .expect_err("a forbidden sidecar is not an absent one");

        assert!(matches!(error, LoaderError::MetadataUnavailable { .. }));
    }

    async fn store_installer_sidecar(
        engine: &InstallerEngine,
        jar_url: &str,
        algorithm: HashAlgorithm,
        digest: &str,
    ) {
        MetadataCache::new(engine.layout.clone())
            .store(
                &installer_sidecar_key(LoaderKind::Forge, "1.20.1", "47.4.10", algorithm)
                    .expect("key"),
                &Url::parse(&format!("{jar_url}.{algorithm}")).expect("sidecar url"),
                serde_json::value::to_raw_value(digest).expect("sidecar"),
            )
            .await
            .expect("sidecar cached");
    }

    /// A jar URL on a loopback port that is reserved but never listening, so
    /// connections are refused and no other test's server can take the port.
    /// The socket holds the port until it is dropped.
    fn unreachable_jar_url() -> (tokio::net::TcpSocket, String) {
        let reserved = tokio::net::TcpSocket::new_v4().expect("socket");
        reserved
            .bind("127.0.0.1:0".parse().expect("loopback address"))
            .expect("reserve a loopback port");
        let port = reserved.local_addr().expect("bound address").port();
        (reserved, format!("http://127.0.0.1:{port}/installer.jar"))
    }

    #[tokio::test]
    async fn an_unreachable_maven_still_resolves_a_sidecar_stored_by_an_earlier_lookup() {
        let (_reserved, jar_url) = unreachable_jar_url();
        let root = tempfile::tempdir().expect("root");
        let engine = loopback_engine(root.path());
        store_installer_sidecar(&engine, &jar_url, HashAlgorithm::Sha1, ABC_SHA1).await;

        let digest = resolve_installer_digest(&engine, &jar_url)
            .await
            .expect("digest");

        assert_eq!(digest.algorithm, HashAlgorithm::Sha1);
        assert_eq!(digest.digest(), ABC_SHA1);
    }

    #[tokio::test]
    async fn an_unreachable_maven_with_no_stored_sidecar_is_refused() {
        let (_reserved, jar_url) = unreachable_jar_url();
        let root = tempfile::tempdir().expect("root");
        let engine = loopback_engine(root.path());

        let error = resolve_installer_digest(&engine, &jar_url)
            .await
            .expect_err("no digest can be resolved");

        assert!(matches!(error, LoaderError::MetadataUnavailable { .. }));
    }

    #[tokio::test]
    async fn a_refused_stronger_sidecar_is_not_replaced_by_a_stored_sha1_sidecar() {
        let server = wiremock::MockServer::start().await;
        let jar_url =
            serve_installer_sidecars(&server, &[(HashAlgorithm::Sha1, ABC_SHA1)], 403).await;
        let root = tempfile::tempdir().expect("root");
        let engine = loopback_engine(root.path());
        store_installer_sidecar(&engine, &jar_url, HashAlgorithm::Sha1, ABC_SHA1).await;

        let error = resolve_installer_digest(&engine, &jar_url)
            .await
            .expect_err("a refusal is not an absence");

        assert!(matches!(error, LoaderError::MetadataUnavailable { .. }));
    }

    #[tokio::test]
    async fn a_malformed_stronger_sidecar_does_not_fall_back_to_sha1() {
        let server = wiremock::MockServer::start().await;
        let jar_url = serve_installer_sidecars(
            &server,
            &[
                (HashAlgorithm::Sha512, "not-a-digest"),
                (HashAlgorithm::Sha1, ABC_SHA1),
            ],
            404,
        )
        .await;
        let root = tempfile::tempdir().expect("root");
        let engine = loopback_engine(root.path());

        let error = resolve_installer_digest(&engine, &jar_url)
            .await
            .expect_err("a malformed sidecar is refused");

        assert!(matches!(error, LoaderError::MetadataInvalid { .. }));
    }

    #[tokio::test]
    async fn an_installer_with_no_published_digest_is_refused() {
        let server = wiremock::MockServer::start().await;
        let jar_url = serve_installer_sidecars(&server, &[], 404).await;
        let root = tempfile::tempdir().expect("root");
        let engine = loopback_engine(root.path());

        let error = resolve_installer_digest(&engine, &jar_url)
            .await
            .expect_err("an installer without a digest cannot be verified");

        assert!(matches!(error, LoaderError::MetadataUnavailable { .. }));
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
                &RuleContext::new(Platform::host()),
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
    async fn library_path_aliases_use_the_first_stored_metadata_entry() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, ResponseTemplate};

        let server = wiremock::MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/first.jar"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"def".to_vec()))
            .expect(1)
            .mount(&server)
            .await;
        let root = tempfile::tempdir().expect("root");
        let engine = loopback_engine(root.path());
        let first = format!("{FMLCORE}:linux.debug@jar");
        let alias = format!("{FMLCORE}:linux@debug.jar");
        let bundle = bundle_declaring(
            root.path(),
            &library_json(&alias, "https://maven.invalid/profile.jar", DEF_SHA1),
            &[
                library_json(&first, &format!("{}/first.jar", server.uri()), DEF_SHA1),
                library_json(&alias, &format!("{}/alias.jar", server.uri()), XYZ_SHA1),
            ],
        );
        validate_on_host(&bundle).expect("only the first stored entry is compared");
        engine
            .install_metadata_libraries(
                &bundle,
                &RuleContext::new(Platform::host()),
                &OperationReporter::silent("loader-library-path-alias-test"),
                &CancellationToken::new(),
            )
            .await
            .expect("only the first stored entry is downloaded");
        assert_eq!(
            std::fs::read(library_path(&engine, &alias)).expect("library"),
            b"def"
        );
        assert_eq!(server.received_requests().await.expect("requests").len(), 1);
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
                &RuleContext::new(Platform::host()),
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
                &RuleContext::new(Platform::host()),
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
                &RuleContext::new(Platform::host()),
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
                &RuleContext::new(Platform::host()),
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
    async fn a_client_install_refuses_parent_library_conflicts_before_downloading() {
        for (digest, size) in [(XYZ_SHA1, 3), (DEF_SHA1, 4)] {
            let server = wiremock::MockServer::start().await;
            let root = tempfile::tempdir().expect("root");
            let engine = loopback_engine(root.path());
            let cached = store_library(&engine, FMLCORE, b"def");
            let profile = profile_json(&library_json(
                FMLCORE,
                &format!("{}/profile.jar", server.uri()),
                DEF_SHA1,
            ));
            cache_installer(&engine, &profile, &version_json(&[])).await;
            let parent = parent_declaring(&[library_json(
                FMLCORE,
                &format!("{}/parent.jar", server.uri()),
                digest,
            )
            .replace("\"size\":3", &format!("\"size\":{size}"))]);

            let error = engine
                .install_loader_client(
                    LoaderKind::Forge,
                    "1.20.1",
                    "47.4.10",
                    &parent,
                    &root.path().join("java"),
                    &RuleContext::new(Platform::host()),
                    &OperationReporter::silent("loader-parent-conflict-test"),
                    &CancellationToken::new(),
                )
                .await
                .expect_err("a parent conflict fails before the vanilla install");

            assert_eq!(
                error.message_id(),
                "error-loader-installer-library-conflict"
            );
            assert!(error.to_string().contains("vanilla parent metadata"));
            assert!(matches!(
                error,
                LoaderError::InstallerLibraryConflict {
                    library, profile, version, profile_size: 3, version_size,
                    version_document: "vanilla parent metadata", ..
                } if library == FMLCORE && profile.digest() == DEF_SHA1
                    && version.digest() == digest && version_size == size
            ));
            assert!(
                server
                    .received_requests()
                    .await
                    .expect("requests")
                    .is_empty()
            );
            assert_eq!(std::fs::read(cached).expect("cached library"), b"def");
        }
    }

    #[tokio::test]
    async fn a_client_install_refuses_parent_conflicts_at_the_same_cache_path() {
        let server = wiremock::MockServer::start().await;
        let root = tempfile::tempdir().expect("root");
        let engine = loopback_engine(root.path());
        let profile_name = format!("{FMLCORE}:linux.debug@jar");
        let parent_name = format!("{FMLCORE}:linux@debug.jar");
        assert_eq!(
            MavenCoordinate::parse(&profile_name)
                .expect("profile coordinate")
                .relative_path(),
            MavenCoordinate::parse(&parent_name)
                .expect("parent coordinate")
                .relative_path(),
        );
        let cached = store_library(&engine, &profile_name, b"def");
        let profile = profile_json(&library_json(
            &profile_name,
            &format!("{}/profile.jar", server.uri()),
            DEF_SHA1,
        ));
        cache_installer(&engine, &profile, &version_json(&[])).await;
        let parent = parent_declaring(&[library_json(
            &parent_name,
            &format!("{}/parent.jar", server.uri()),
            XYZ_SHA1,
        )]);

        let error = engine
            .install_loader_client(
                LoaderKind::Forge,
                "1.20.1",
                "47.4.10",
                &parent,
                &root.path().join("java"),
                &RuleContext::new(Platform::host()),
                &OperationReporter::silent("loader-parent-path-conflict-test"),
                &CancellationToken::new(),
            )
            .await
            .expect_err("conflicting files at one path fail before the vanilla install");
        assert!(matches!(
            error,
            LoaderError::InstallerLibraryConflict {
                library, version_document: "vanilla parent metadata", ..
            } if library == parent_name
        ));
        assert!(
            server
                .received_requests()
                .await
                .expect("requests")
                .is_empty()
        );
        assert_eq!(std::fs::read(cached).expect("cached library"), b"def");
    }

    #[test]
    fn parent_library_validation_compares_only_the_entries_the_install_stores() {
        let root = tempfile::tempdir().expect("root");
        let bundle = bundle_declaring(
            root.path(),
            &library_json(FMLCORE, "https://maven.invalid/profile.jar", DEF_SHA1),
            &[],
        );
        let context = RuleContext::new(Platform::host());
        let elsewhere = if cfg!(target_os = "macos") {
            "windows"
        } else {
            "osx"
        };
        let parent = parent_declaring(&[
            format!(r#"{{"name":"{FMLCORE}","url":"https://maven.invalid/"}}"#),
            format!(
                r#"{{"name":"{FMLCORE}","rules":[{{"action":"allow","os":{{"name":"{elsewhere}"}}}}],
                "downloads":{{"artifact":{{"url":"https://maven.invalid/a.jar","sha1":"{XYZ_SHA1}","size":4}}}}}}"#
            ),
            library_json(FMLCORE, "https://maven.invalid/parent.jar", DEF_SHA1),
            library_json(FMLCORE, "https://maven.invalid/duplicate.jar", XYZ_SHA1),
        ]);

        ensure_consistent_library_artifacts(&bundle, &parent, &context, "vanilla parent metadata")
            .expect("matching stored artifacts are accepted");

        let distinct = parent_declaring(&[library_json(
            "net.minecraftforge:fmlcore:1.20.1-47.4.11",
            "https://maven.invalid/distinct.jar",
            XYZ_SHA1,
        )]);
        ensure_consistent_library_artifacts(
            &bundle,
            &distinct,
            &context,
            "vanilla parent metadata",
        )
        .expect("different coordinates occupy different paths");
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
    fn matching_library_digests_with_different_sizes_are_refused() {
        let root = tempfile::tempdir().expect("root");
        for url in ["https://maven.invalid/fmlcore.jar", ""] {
            let bundle = bundle_declaring(
                root.path(),
                &library_json(FMLCORE, "https://maven.invalid/profile.jar", DEF_SHA1),
                &[library_json(FMLCORE, url, DEF_SHA1).replace("\"size\":3", "\"size\":4")],
            );

            let error = validate_on_host(&bundle).expect_err("conflicting sizes");
            assert!(matches!(
                error,
                LoaderError::InstallerLibraryConflict {
                    library, profile, version, profile_size: 3, version_size: 4, ..
                } if library == FMLCORE && profile == version
            ));
        }
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
                &RuleContext::new(Platform::host()),
                &OperationReporter::silent("loader-server-libraries-test"),
                &CancellationToken::new(),
            )
            .await
            .expect("an excluded library is skipped");

        assert!(!library_path(&engine, "ca.weblite:java-objc-bridge:1.1").exists());
    }

    fn staging_leftovers(directory: &Path) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let mut pending = vec![directory.to_path_buf()];
        while let Some(directory) = pending.pop() {
            for entry in std::fs::read_dir(&directory).expect("directory") {
                let path = entry.expect("entry").path();
                if path.is_dir() {
                    pending.push(path);
                } else if path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().contains(".part."))
                {
                    found.push(path);
                }
            }
        }
        found
    }

    #[tokio::test]
    async fn processor_outputs_checked_against_a_digest_are_recorded() {
        let root = tempfile::tempdir().expect("root");
        let engine = loopback_engine(root.path());
        let output = root.path().join("work/libraries/g/out/1/out-1.jar");
        std::fs::create_dir_all(output.parent().expect("parent")).expect("output directory");
        std::fs::write(&output, b"abc").expect("output");
        let processor = Processor {
            sides: Vec::new(),
            jar: "g:p:1".to_owned(),
            classpath: Vec::new(),
            args: Vec::new(),
            outputs: BTreeMap::from([(
                "{ROOT}/libraries/g/out/1/out-1.jar".to_owned(),
                format!("'{ABC_SHA1}'"),
            )]),
        };
        let values = BTreeMap::from([(
            "ROOT".to_owned(),
            root.path().join("work").to_string_lossy().into_owned(),
        )]);
        let mut verified = KnownDigests::new();

        engine
            .verify_processor_outputs(&processor, &values, &mut verified)
            .await
            .expect("the output matches");

        assert_eq!(
            verified.get(&output),
            Some(&Hash::new(HashAlgorithm::Sha1, ABC_SHA1).expect("hash"))
        );
    }

    #[tokio::test]
    async fn publishing_replaces_wrong_or_truncated_store_copies_and_keeps_complete_ones() {
        let root = tempfile::tempdir().expect("root");
        let source = root.path().join("work/libraries");
        let store = root.path().join("store");
        for (directory, name, bytes) in [
            (&source, "patched.jar", &b"abc"[..]),
            (&source, "undeclared.jar", &b"new"[..]),
            (&source, "fresh.jar", &b"xyz"[..]),
            (&store, "patched.jar", &b"bad"[..]),
            (&store, "undeclared.jar", &b"old"[..]),
            (&source, "argfile.txt", &b"new"[..]),
            (&store, "argfile.txt", &b"ne"[..]),
        ] {
            std::fs::create_dir_all(directory.join("g")).expect("directory");
            std::fs::write(directory.join("g").join(name), bytes).expect("file");
        }
        let verified = KnownDigests::from([(
            source.join("g/patched.jar"),
            Hash::new(HashAlgorithm::Sha1, ABC_SHA1).expect("hash"),
        )]);

        publish_tree(&source, &store, &verified)
            .await
            .expect("publication");

        assert_eq!(
            std::fs::read(store.join("g/patched.jar")).expect("patched"),
            b"abc"
        );
        assert_eq!(
            std::fs::read(store.join("g/undeclared.jar")).expect("undeclared"),
            b"old",
            "an atomic earlier publication with no known digest is kept"
        );
        assert_eq!(
            std::fs::read(store.join("g/fresh.jar")).expect("fresh"),
            b"xyz"
        );
        assert_eq!(
            std::fs::read(store.join("g/argfile.txt")).expect("argfile"),
            b"new",
            "a truncated earlier copy is replaced"
        );
        assert!(staging_leftovers(&store).is_empty());
    }

    #[tokio::test]
    async fn a_processor_output_rewritten_after_its_check_is_not_published() {
        let root = tempfile::tempdir().expect("root");
        let source = root.path().join("work/libraries");
        let store = root.path().join("store");
        std::fs::create_dir_all(source.join("g")).expect("workspace");
        std::fs::write(source.join("g/patched.jar"), b"xyz").expect("rewritten output");
        let known = KnownDigests::from([(
            source.join("g/patched.jar"),
            Hash::new(HashAlgorithm::Sha1, ABC_SHA1).expect("hash"),
        )]);

        let error = publish_tree(&source, &store, &known)
            .await
            .expect_err("bytes that no longer match their digest are refused");

        assert!(matches!(error, LoaderError::InstallerInvalid { .. }));
        assert!(!store.join("g/patched.jar").exists());
        assert!(staging_leftovers(&store).is_empty());
    }

    #[tokio::test]
    async fn a_wrong_stored_copy_of_an_output_version_metadata_declares_is_replaced() {
        let root = tempfile::tempdir().expect("root");
        let engine = loopback_engine(root.path());
        let coordinate = "net.minecraftforge:forge:1.20.1-47.4.10:client";
        let bundle = bundle_declaring(root.path(), "", &[library_json(coordinate, "", ABC_SHA1)]);
        let source = root.path().join("work/libraries");
        let output = source.join(
            MavenCoordinate::parse(coordinate)
                .expect("coordinate")
                .relative_path(),
        );
        std::fs::create_dir_all(output.parent().expect("parent")).expect("workspace");
        std::fs::write(&output, b"abc").expect("processor output");
        let stored = store_library(&engine, coordinate, b"bad");

        engine
            .publish_libraries(
                &bundle,
                &RuleContext::new(Platform::host()),
                &source,
                KnownDigests::new(),
            )
            .await
            .expect("publication");

        assert_eq!(std::fs::read(stored).expect("library"), b"abc");
    }

    #[tokio::test]
    async fn only_the_first_installer_carried_entry_for_a_coordinate_is_installed() {
        use std::io::Write as _;

        let root = tempfile::tempdir().expect("root");
        let engine = loopback_engine(root.path());
        let coordinate = "net.minecraftforge:forge:1.16.5-36.2.39";
        let bundle = bundle_declaring(
            root.path(),
            "",
            &[
                library_json(coordinate, "", ABC_SHA1),
                library_json(coordinate, "", DEF_SHA1),
            ],
        );
        let relative = MavenCoordinate::parse(coordinate)
            .expect("coordinate")
            .relative_path();
        let mut archive =
            zip::ZipWriter::new(std::fs::File::create(&bundle.installer).expect("installer"));
        archive
            .start_file(
                format!("maven/{}", relative.display()),
                zip::write::SimpleFileOptions::default(),
            )
            .expect("entry");
        archive.write_all(b"abc").expect("entry body");
        archive.finish().expect("installer finish");

        engine
            .install_installer_declared_libraries(&bundle, &RuleContext::new(Platform::host()))
            .await
            .expect("the later entry for the coordinate is not stored");

        assert_eq!(
            std::fs::read(library_path(&engine, coordinate)).expect("library"),
            b"abc"
        );
    }

    #[tokio::test]
    async fn a_preinstalled_artifact_the_host_never_stores_is_not_required() {
        let root = tempfile::tempdir().expect("root");
        let engine = loopback_engine(root.path());
        let excluded = r#"{"name":"net.minecraftforge:forge:1.20.1-47.4.10:mac","rules":[{"action":"allow","os":{"name":"osx"}}],"downloads":{"artifact":{"url":"","sha1":"a9993e364706816aba3e25717850c26c9cd0d89d","size":3}}}"#;
        let mut metadata = VersionMetadata::parse(&version_json(&[excluded.to_owned()]))
            .expect("version metadata");

        engine
            .mark_preinstalled_artifacts(
                &mut metadata,
                &RuleContext::new(Platform::new(
                    ephervex_minecraft::OsName::Linux,
                    "x86_64",
                    "",
                )),
            )
            .await
            .expect("an entry this host never stores is not required");
    }

    #[tokio::test]
    async fn a_processor_digest_that_disagrees_with_version_metadata_is_refused() {
        let root = tempfile::tempdir().expect("root");
        let engine = loopback_engine(root.path());
        let coordinate = "net.minecraftforge:forge:1.20.1-47.4.10:client";
        let bundle = bundle_declaring(root.path(), "", &[library_json(coordinate, "", DEF_SHA1)]);
        let source = root.path().join("work/libraries");
        let output = source.join(
            MavenCoordinate::parse(coordinate)
                .expect("coordinate")
                .relative_path(),
        );
        std::fs::create_dir_all(output.parent().expect("parent")).expect("workspace");
        std::fs::write(&output, b"abc").expect("processor output");
        let processed = KnownDigests::from([(
            output,
            Hash::new(HashAlgorithm::Sha1, ABC_SHA1).expect("hash"),
        )]);

        let error = engine
            .publish_libraries(
                &bundle,
                &RuleContext::new(Platform::host()),
                &source,
                processed,
            )
            .await
            .expect_err("two digests for one output are refused");

        assert!(matches!(
            error,
            LoaderError::InstallerLibraryConflict {
                profile, version, profile_size: 3, version_size: 3, ..
            } if profile.digest() == ABC_SHA1 && version.digest() == DEF_SHA1
        ));
        assert!(!library_path(&engine, coordinate).exists());
    }

    const FORGE_CLIENT: &str = "net.minecraftforge:forge:1.20.1-47.4.10:client";
    /// A value only the server's processors produce, so a client run never stores it.
    const FORGE_UNPACKED: &str = "net.minecraft:client:1.20.1:unpacked";
    const FORGE_MOJMAPS: &str = "net.minecraft:client:1.20.1:mappings@txt";
    /// A processor product the profile declares neither an output nor a digest for.
    const FORGE_SRG: &str = "net.minecraft:client:1.20.1:srg";

    /// A processor-driven bundle whose one processor produces the patched game
    /// and whose profile asks for Mojang mappings, so a run would need the network.
    fn processed_bundle(root: &Path, processor_outputs: &str) -> InstallerBundle {
        let profile = format!(
            r#"{{"spec":1,"profile":"forge","version":"1.20.1-forge-47.4.10","minecraft":"1.20.1",
            "libraries":[],
            "data":{{
                "MOJMAPS":{{"client":"[{FORGE_MOJMAPS}]","server":"[{FORGE_MOJMAPS}]"}},
                "MC_UNPACKED":{{"client":"[{FORGE_UNPACKED}]","server":"[{FORGE_UNPACKED}]"}},
                "MC_SRG":{{"client":"[{FORGE_SRG}]","server":"[{FORGE_SRG}]"}},
                "PATCHED":{{"client":"[{FORGE_CLIENT}]","server":"[{FORGE_CLIENT}]"}},
                "PATCHED_SHA":{{"client":"'{ABC_SHA1}'","server":"'{ABC_SHA1}'"}}
            }},
            "processors":[
                {{"sides":["client"],"jar":"g:p:1","args":["--output","{{MOJMAPS}}"],"outputs":{processor_outputs}}},
                {{"sides":["client"],"jar":"g:p:1","args":["--out-jar","{{MC_SRG}}"]}},
                {{"sides":["server"],"jar":"g:p:1","args":["--output","{{MC_UNPACKED}}"]}}
            ]}}"#
        );
        InstallerBundle {
            installer: root.join("installer.jar"),
            profile: InstallProfile::parse(&profile).expect("profile"),
            child: VersionMetadata::parse(&version_json(&[library_json(
                FORGE_CLIENT,
                "",
                ABC_SHA1,
            )]))
            .expect("version metadata"),
        }
    }

    async fn published(engine: &InstallerEngine, bundle: &InstallerBundle) -> bool {
        engine
            .processor_outputs_published(
                bundle,
                InstallSide::Client,
                Path::new("minecraft.jar"),
                &RuleContext::new(Platform::host()),
            )
            .await
            .expect("check")
    }

    #[tokio::test]
    async fn processor_outputs_are_published_only_when_every_stored_copy_verifies() {
        let root = tempfile::tempdir().expect("root");
        let engine = loopback_engine(root.path());
        let bundle = processed_bundle(root.path(), &format!(r#"{{"{{PATCHED}}":"'{ABC_SHA1}'"}}"#));

        assert!(
            !published(&engine, &bundle).await,
            "nothing is stored before the first install"
        );
        let stored = store_library(&engine, FORGE_CLIENT, b"bad");
        assert!(
            !published(&engine, &bundle).await,
            "a stored copy with the wrong digest is not trusted"
        );
        std::fs::write(&stored, b"abc").expect("verified copy");
        assert!(
            !published(&engine, &bundle).await,
            "a processor product with no declared output is still required"
        );
        let srg = store_library(&engine, FORGE_SRG, b"srg");
        assert!(
            !published(&engine, &bundle).await,
            "the mappings the installer fetched are stored too"
        );
        store_library(&engine, FORGE_MOJMAPS, b"map");
        assert!(published(&engine, &bundle).await);
        std::fs::remove_file(srg).expect("evicted product");
        assert!(!published(&engine, &bundle).await);
    }

    #[tokio::test]
    async fn a_profile_without_digests_is_published_when_its_products_are_stored() {
        let root = tempfile::tempdir().expect("root");
        let engine = loopback_engine(root.path());
        let mut bundle = processed_bundle(root.path(), "{}");
        bundle.profile.data.remove("PATCHED_SHA");
        bundle.child = VersionMetadata::parse(&version_json(&[])).expect("version metadata");
        let patched = store_library(&engine, FORGE_CLIENT, b"patched");
        store_library(&engine, FORGE_SRG, b"srg");
        store_library(&engine, FORGE_MOJMAPS, b"map");

        assert!(published(&engine, &bundle).await);
        std::fs::remove_file(patched).expect("evicted game");
        assert!(
            !published(&engine, &bundle).await,
            "the patched game is required even with no digest to verify"
        );
    }

    #[tokio::test]
    async fn processors_run_when_an_output_lies_outside_the_shared_store() {
        let root = tempfile::tempdir().expect("root");
        let engine = loopback_engine(root.path());
        store_library(&engine, FORGE_CLIENT, b"abc");
        for output in ["{ROOT}/elsewhere.bin", "{LIBRARY_DIR}/../elsewhere.bin"] {
            let bundle =
                processed_bundle(root.path(), &format!(r#"{{"{output}":"'{ABC_SHA1}'"}}"#));

            assert!(!published(&engine, &bundle).await, "{output}");
        }
    }

    #[tokio::test]
    async fn a_relaunch_with_every_output_published_runs_no_processor_and_fetches_no_mappings() {
        let root = tempfile::tempdir().expect("root");
        let engine = loopback_engine(root.path());
        let bundle = processed_bundle(root.path(), &format!(r#"{{"{{PATCHED}}":"'{ABC_SHA1}'"}}"#));
        store_library(&engine, FORGE_CLIENT, b"abc");
        store_library(&engine, FORGE_SRG, b"srg");
        store_library(&engine, FORGE_MOJMAPS, b"map");

        // The parent declares no mappings download and the Java path does not exist,
        // so any attempt to prepare mappings or run a processor fails.
        engine
            .ensure_client_processed(
                &bundle,
                &vanilla_parent(),
                Path::new("minecraft.jar"),
                &root.path().join("no-java"),
                &RuleContext::new(Platform::host()),
                &OperationReporter::silent("loader-relaunch-test"),
                &CancellationToken::new(),
            )
            .await
            .expect("a published install is reused as it is");

        assert!(
            !engine
                .layout
                .store_root(ContentStore::InstallerCache)
                .join("work")
                .exists(),
            "no workspace is created"
        );
    }

    #[tokio::test]
    async fn a_relaunch_whose_stored_output_is_wrong_runs_the_processors_again() {
        let root = tempfile::tempdir().expect("root");
        let engine = loopback_engine(root.path());
        let bundle = processed_bundle(root.path(), &format!(r#"{{"{{PATCHED}}":"'{ABC_SHA1}'"}}"#));
        store_library(&engine, FORGE_CLIENT, b"bad");

        let error = engine
            .ensure_client_processed(
                &bundle,
                &vanilla_parent(),
                Path::new("minecraft.jar"),
                &root.path().join("no-java"),
                &RuleContext::new(Platform::host()),
                &OperationReporter::silent("loader-relaunch-test"),
                &CancellationToken::new(),
            )
            .await
            .expect_err("the installer pipeline starts again");

        assert!(
            format!("{error:?}").contains("client_mappings"),
            "the pipeline began with the mappings: {error:?}"
        );
    }

    fn installing_siblings(target: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(target.parent().expect("parent"))
            .expect("parent directory")
            .map(|entry| entry.expect("entry").path())
            .filter(|path| path.to_string_lossy().contains(".ephervex-installing-"))
            .collect()
    }

    #[tokio::test]
    async fn a_canceled_server_install_leaves_no_staging_directory() {
        let root = tempfile::tempdir().expect("root");
        let engine = loopback_engine(root.path());
        let metadata = VersionMetadata::parse(&format!(
            r#"{{"id":"1.20.1","type":"release","releaseTime":"2023-06-12T00:00:00+00:00",
            "time":"2023-06-12T00:00:00+00:00","mainClass":"net.minecraft.client.main.Main",
            "libraries":[],"downloads":{{"server":{{"url":"https://piston-data.mojang.com/server.jar",
            "sha1":"{ABC_SHA1}","size":3}}}}}}"#
        ))
        .expect("metadata");
        let target = root.path().join("servers/one/game");
        std::fs::create_dir_all(target.parent().expect("parent")).expect("server root");
        let cancel = CancellationToken::new();
        cancel.cancel();

        let error = engine
            .install_vanilla_server(
                &metadata,
                &target,
                &OperationReporter::silent("server-install-test"),
                &cancel,
            )
            .await
            .expect_err("a canceled install does not finish");

        assert!(matches!(
            error,
            LoaderError::Download { ref source, .. } if source.is_cancellation()
        ));
        assert!(installing_siblings(&target).is_empty());
        assert!(!target.exists());
    }

    #[tokio::test]
    async fn a_loader_server_install_that_fails_after_staging_leaves_no_staging_directory() {
        let root = tempfile::tempdir().expect("root");
        let engine = loopback_engine(root.path());
        cache_installer(&engine, &profile_json(""), &version_json(&[])).await;
        // A plain HTTP address the transport policy refuses once staging exists.
        let parent = VersionMetadata::parse(&format!(
            r#"{{"id":"1.20.1","type":"release","releaseTime":"2023-06-12T00:00:00+00:00",
            "time":"2023-06-12T00:00:00+00:00","mainClass":"net.minecraft.client.main.Main",
            "libraries":[],"downloads":{{"server":{{"url":"http://127.0.0.1:1/server.jar",
            "sha1":"{ABC_SHA1}","size":3}}}}}}"#
        ))
        .expect("parent metadata");
        let target = root.path().join("servers/one/game");
        std::fs::create_dir_all(target.parent().expect("parent")).expect("server root");

        let error = engine
            .install_loader_server(
                LoaderKind::Forge,
                "1.20.1",
                "47.4.10",
                &parent,
                &root.path().join("java"),
                &target,
                &OperationReporter::silent("loader-server-staging-test"),
                &CancellationToken::new(),
            )
            .await
            .expect_err("the server jar address is refused");

        assert!(matches!(error, LoaderError::Url(_)), "{error:?}");
        assert!(installing_siblings(&target).is_empty());
        assert!(!target.exists());
    }

    #[tokio::test]
    async fn a_scratch_directory_is_removed_or_handed_over_whole() {
        let root = tempfile::tempdir().expect("root");
        let removed = ScratchDirectory::create(root.path().join("removed"))
            .await
            .expect("scratch");
        std::fs::write(removed.path().join("server.jar"), b"abc").expect("content");
        let path = removed.path().to_path_buf();
        removed.remove().await;
        assert!(!path.exists());

        let staged = ScratchDirectory::create(root.path().join("staged"))
            .await
            .expect("scratch");
        std::fs::write(staged.path().join("server.jar"), b"abc").expect("content");
        let path = staged.path().to_path_buf();
        let target = root.path().join("game");
        staged.settle(Ok(()), Some(&target)).await.expect("publish");
        assert!(!path.exists());
        assert_eq!(
            std::fs::read(target.join("server.jar")).expect("published"),
            b"abc"
        );
    }

    #[tokio::test]
    async fn a_scratch_directory_dropped_inside_the_runtime_is_removed() {
        let root = tempfile::tempdir().expect("root");
        let scratch = ScratchDirectory::create(root.path().join("dropped"))
            .await
            .expect("scratch");
        std::fs::create_dir_all(scratch.path().join("libraries")).expect("content");
        let path = scratch.path().to_path_buf();
        drop(scratch);

        tokio::time::timeout(Duration::from_secs(5), async {
            while path.exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the directory is removed off the runtime");
    }

    #[test]
    fn a_dropped_scratch_directory_is_removed() {
        let root = tempfile::tempdir().expect("root");
        let path = root.path().join("dropped");
        std::fs::create_dir_all(path.join("libraries")).expect("scratch");
        drop(ScratchDirectory {
            path: Some(path.clone()),
        });
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn an_installer_carried_library_whose_stored_copy_is_wrong_is_extracted_again() {
        use std::io::Write as _;

        let root = tempfile::tempdir().expect("root");
        let engine = loopback_engine(root.path());
        let coordinate = "net.minecraftforge:forge:1.16.5-36.2.39";
        let bundle = bundle_declaring(root.path(), "", &[library_json(coordinate, "", ABC_SHA1)]);
        let relative = MavenCoordinate::parse(coordinate)
            .expect("coordinate")
            .relative_path();
        let mut archive =
            zip::ZipWriter::new(std::fs::File::create(&bundle.installer).expect("installer"));
        archive
            .start_file(
                format!("maven/{}", relative.display()),
                zip::write::SimpleFileOptions::default(),
            )
            .expect("entry");
        archive.write_all(b"abc").expect("entry body");
        archive.finish().expect("installer finish");
        let stored = store_library(&engine, coordinate, b"bad");

        engine
            .install_installer_declared_libraries(&bundle, &RuleContext::new(Platform::host()))
            .await
            .expect("installer-carried libraries install");

        assert_eq!(std::fs::read(&stored).expect("library"), b"abc");
        assert!(staging_leftovers(stored.parent().expect("parent")).is_empty());
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
