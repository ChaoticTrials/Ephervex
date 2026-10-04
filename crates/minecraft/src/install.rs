//! Turning one version's metadata into files on disk.
//!
//! # Planning and installing are separate on purpose
//!
//! [`InstallationPlan::resolve`] is pure: metadata plus a [`RuleContext`] in,
//! every path and every artifact out, no I/O and no network. [`install`] then
//! fetches what the plan named. Splitting them is what makes the interesting
//! half testable - which libraries survive their rules, in which order the
//! classpath is built, which era's natives handling applies - without a
//! download, a temporary directory, or a JVM.
//!
//! It also gives `--dry-run` and "explain this launch" a real answer rather
//! than a re-derivation that could disagree with what actually ran.
//!
//! # Never "every cached jar"
//!
//! The classpath must never be assembled from every cached JAR: the shared
//! libraries store holds every library of every version the launcher has ever
//! touched.
//! The classpath is built from *this version's* `libraries` array, filtered by
//! its rules, in the order Mojang published - with the client jar last, where
//! Mojang's own launcher puts it.
//!
//! # Where things land
//!
//! | Artifact | Store | Keyed by |
//! |---|---|---|
//! | libraries | `libraries/` | validated Maven coordinate |
//! | client jar, logging config | `downloads/` | SHA-1 |
//! | asset index (canonical) | `metadata/` | SHA-1 |
//! | asset index (launch view) | `assets/indexes/` | validated index id |
//! | asset objects | `assets/objects/` | SHA-1, Mojang's own layout |
//! | unpacked natives | `natives/<version>/<platform>/` | derived, not downloaded |
//!
//! Every one of them is launcher-level. An instance directory holds saves,
//! options, logs, and crash reports and nothing that could be re-downloaded -
//! which is what makes a temporary instance cost kilobytes instead of gigabytes
//!
//! The client jar is content-addressed rather than filed under a synthetic
//! `com.mojang:minecraft:<version>` coordinate, even though the latter would
//! read better in a log. A logical key would mean a path that does not change
//! when the bytes do, and the download manager's "already at the destination"
//! short-circuit would then serve yesterday's jar for a republished version.
//! Every other hash-named artifact in the workspace makes the same trade.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use ephervex_download::{
    CacheLayout, ContentStore, DownloadManager, DownloadRequest, FileExtension, OperationReporter,
    UrlPolicyError,
};
use tokio::task;
use tokio_util::sync::CancellationToken;
use tracing::{debug, instrument};

use crate::assets::AssetIndex;
use crate::error::{LaunchError, LaunchResult};
use crate::library::{Artifact, Library};
use crate::natives;
use crate::platform::Platform;
use crate::rules::{OsName, RuleContext, rules_allow};
use crate::version::VersionMetadata;

/// One file the plan says must exist, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedArtifact {
    /// Technical name for logs and progress: a Maven coordinate, `client.jar`,
    /// an asset index id. Never used to build a path.
    pub label: String,
    pub artifact: Artifact,
    pub path: PathBuf,
}

/// A pre-1.19 natives jar and the entries its metadata excludes from unpacking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedNatives {
    pub artifact: PlannedArtifact,
    /// `extract.exclude`, universally `["META-INF/"]` but read from the
    /// document rather than assumed.
    pub exclude: Vec<String>,
}

/// The log4j configuration a release wants, and the JVM argument that names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedLogging {
    /// The template, e.g. `-Dlog4j.configurationFile=${path}`.
    pub argument: String,
    pub artifact: PlannedArtifact,
}

/// Why a library the metadata listed is not part of this launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExcludedLibrary {
    pub name: String,
    pub reason: ExclusionReason,
}

/// The reasons a library can be left out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExclusionReason {
    /// Its rules do not apply to this platform or this launch.
    RuleNotSatisfied,
    /// It ships only natives classifiers, so there is no jar for the classpath.
    NativesOnly,
    /// Its natives map names no classifier for this platform.
    NoNativesForPlatform,
    /// Its coordinate resolves to a path another library already occupies.
    /// Vanilla 1.12.2 lists `com.mojang:text2speech:1.10.3` twice, once for the
    /// classpath and once for its natives.
    AlreadyOnClasspath,
}

impl ExclusionReason {
    /// A stable, untranslated identifier for logs and `--json`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RuleNotSatisfied => "rule-not-satisfied",
            Self::NativesOnly => "natives-only",
            Self::NoNativesForPlatform => "no-natives-for-platform",
            Self::AlreadyOnClasspath => "already-on-classpath",
        }
    }
}

/// Everything one version needs on disk, and where each piece goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallationPlan {
    pub version: String,
    pub platform: Platform,
    pub client_jar: PlannedArtifact,
    /// Libraries, in Mojang's own order, already filtered by rules and
    /// deduplicated. The client jar is **not** in here; see
    /// [`Self::classpath`].
    pub libraries: Vec<PlannedArtifact>,
    /// The shared Maven root, exposed to loader argument placeholders.
    pub libraries_dir: PathBuf,
    /// Pre-1.19 natives jars to unpack. Empty from 1.19 onward, where natives
    /// are ordinary classpath entries instead.
    pub natives: Vec<PlannedNatives>,
    /// Where `-Djava.library.path` points.
    pub natives_dir: PathBuf,
    pub asset_index: PlannedArtifact,
    /// The id `--assetIndex` is given, e.g. `1.12`, `17`, `30`.
    pub asset_index_id: String,
    /// The named copy Minecraft opens below [`Self::assets_dir`]. The
    /// canonical verified download remains [`Self::asset_index`].
    pub asset_index_path: PathBuf,
    /// The shared assets root `--assetsDir` is given.
    pub assets_dir: PathBuf,
    pub logging: Option<PlannedLogging>,
    /// Libraries the metadata listed that this launch leaves out, with why.
    /// Kept so "why is that jar not on the classpath" has an answer.
    pub excluded: Vec<ExcludedLibrary>,
}

impl InstallationPlan {
    /// Works out every path and artifact for one version, without any I/O.
    ///
    /// Fails rather than guessing when the document does not describe a vanilla
    /// launch: a loader child with no `downloads` is a real and expected input
    /// (loader support merges it), and treating it as launchable would produce a
    /// command line with nothing to run.
    pub fn resolve(
        metadata: &VersionMetadata,
        context: &RuleContext,
        layout: &CacheLayout,
    ) -> LaunchResult<Self> {
        let version = metadata.id.clone();
        let invalid = |source| LaunchError::InvalidVersionMetadata {
            version: version.clone(),
            source,
        };

        let jar = FileExtension::new("jar")?;
        let xml = FileExtension::new("xml")?;
        let json = FileExtension::new("json")?;

        let client = metadata.client_jar().map_err(invalid)?;
        let client_jar = PlannedArtifact {
            label: format!("{version}/client.jar"),
            path: layout.content(ContentStore::Downloads, &client.sha1, Some(&jar)),
            artifact: client.clone(),
        };

        let index = metadata.asset_index().map_err(invalid)?;
        let asset_index = PlannedArtifact {
            label: format!("assets/{}", index.id),
            path: layout.content(ContentStore::Metadata, &index.artifact.sha1, Some(&json)),
            artifact: index.artifact.clone(),
        };
        let asset_index_path = layout.asset_index(&index.id)?;

        let logging = metadata.logging.as_ref().map(|logging| PlannedLogging {
            argument: logging.argument.clone(),
            artifact: PlannedArtifact {
                label: logging.id.clone(),
                path: layout.content(ContentStore::Downloads, &logging.file.sha1, Some(&xml)),
                artifact: logging.file.clone(),
            },
        });

        let mut libraries = Vec::new();
        let mut natives = Vec::new();
        let mut excluded = Vec::new();
        let mut placed: BTreeSet<PathBuf> = BTreeSet::new();

        for library in &metadata.libraries {
            if !rules_allow(&library.rules, context) {
                excluded.push(ExcludedLibrary {
                    name: library.name.clone(),
                    reason: ExclusionReason::RuleNotSatisfied,
                });
                continue;
            }

            // A pre-1.19 entry may contribute to both halves: `text2speech`
            // ships a classpath jar *and* a natives classifier. The two are
            // therefore considered independently rather than as an either/or.
            if library.is_legacy_natives() {
                match plan_legacy_natives(library, &context.platform, layout)? {
                    Some(planned) => natives.push(planned),
                    None => excluded.push(ExcludedLibrary {
                        name: library.name.clone(),
                        reason: ExclusionReason::NoNativesForPlatform,
                    }),
                }
            }

            let Some(artifact) = library.downloads.artifact.as_ref() else {
                if !library.is_legacy_natives() {
                    excluded.push(ExcludedLibrary {
                        name: library.name.clone(),
                        reason: ExclusionReason::NativesOnly,
                    });
                }
                continue;
            };

            if let Some(classifier) = library.coordinate.classifier() {
                validate_linux_lwjgl_natives(library, classifier, &context.platform)?;
            }

            let path = layout.library(&library.coordinate);
            if !placed.insert(path.clone()) {
                excluded.push(ExcludedLibrary {
                    name: library.name.clone(),
                    reason: ExclusionReason::AlreadyOnClasspath,
                });
                continue;
            }
            libraries.push(PlannedArtifact {
                label: library.name.clone(),
                artifact: artifact.clone(),
                path,
            });
        }

        let natives_dir = layout.natives(&version, &context.platform.cache_key())?;

        debug!(
            version = %version,
            platform = %context.platform,
            classpath = libraries.len() + 1,
            natives = natives.len(),
            excluded = excluded.len(),
            "resolved an installation plan"
        );

        Ok(Self {
            version,
            platform: context.platform.clone(),
            client_jar,
            libraries,
            libraries_dir: layout.store_root(ContentStore::Libraries),
            natives,
            natives_dir,
            asset_index_id: index.id.clone(),
            asset_index,
            asset_index_path,
            assets_dir: layout.store_root(ContentStore::Assets),
            logging,
            excluded,
        })
    }

    /// The classpath, in launch order: every library, then the client jar.
    ///
    /// The client jar goes last because Mojang's own launcher puts it last, and
    /// because a library that shadows a game class has to win for a
    /// reproduction to be faithful.
    #[must_use]
    pub fn classpath(&self) -> Vec<PathBuf> {
        let mut entries: Vec<PathBuf> = self
            .libraries
            .iter()
            .map(|library| library.path.clone())
            .collect();
        entries.push(self.client_jar.path.clone());
        entries
    }

    /// Every artifact that has to be fetched before the game can start, except
    /// the assets - those are named by the index, which is one of these.
    fn required_artifacts(&self) -> Vec<&PlannedArtifact> {
        let mut artifacts = vec![&self.client_jar, &self.asset_index];
        artifacts.extend(self.libraries.iter());
        artifacts.extend(self.natives.iter().map(|natives| &natives.artifact));
        if let Some(logging) = self.logging.as_ref() {
            artifacts.push(&logging.artifact);
        }
        artifacts
    }
}

/// Picks the natives classifier for a platform, substituting `${arch}`.
/// A map without this OS is inapplicable; a selected classifier without an
/// artifact is incomplete metadata and must not produce a launch plan.
fn plan_legacy_natives(
    library: &Library,
    platform: &Platform,
    layout: &CacheLayout,
) -> LaunchResult<Option<PlannedNatives>> {
    let Some(classifier) = library.natives_classifier(platform.natives_key()) else {
        return Ok(None);
    };
    let classifier = classifier.replace("${arch}", platform.natives_arch());
    validate_linux_lwjgl_natives(library, &classifier, platform)?;
    let artifact =
        library
            .classifier(&classifier)
            .ok_or_else(|| LaunchError::MissingNativeArtifact {
                library: library.name.clone(),
                classifier: classifier.clone(),
            })?;

    Ok(Some(PlannedNatives {
        artifact: PlannedArtifact {
            label: format!("{}:{classifier}", library.name),
            // Classifier jars are content-addressed rather than filed under the
            // coordinate: the coordinate does not carry the classifier in this
            // era, so two platforms' natives would collide at one path.
            path: layout.content(
                ContentStore::Downloads,
                &artifact.sha1,
                FileExtension::new("jar").ok().as_ref(),
            ),
            artifact: artifact.clone(),
        },
        exclude: library
            .extract
            .as_ref()
            .map(|rules| rules.exclude.clone())
            .unwrap_or_default(),
    }))
}

/// OS-only rules do not establish native architecture compatibility. LWJGL 3
/// publishes separate Linux classifiers (see LWJGL's `build.xml`, `natives-jar`);
/// LWJGL 2's generic Linux jar instead bundles both x86 widths.
fn validate_linux_lwjgl_natives(
    library: &Library,
    classifier: &str,
    platform: &Platform,
) -> LaunchResult<()> {
    if platform.os != OsName::Linux || !classifier.starts_with("natives-") {
        return Ok(());
    }
    let compatible = match library.coordinate.group() {
        "org.lwjgl" => match classifier {
            "natives-linux" => platform.arch == "x86_64",
            "natives-linux-arm64" => platform.arch == "arm64",
            "natives-linux-arm32" => platform.arch == "arm",
            _ => false,
        },
        "org.lwjgl.lwjgl" => {
            classifier == "natives-linux" && matches!(platform.arch.as_str(), "x86" | "x86_64")
        }
        _ => return Ok(()),
    };
    if !compatible {
        return Err(LaunchError::IncompatibleNativeLibrary {
            library: library.name.clone(),
            classifier: classifier.to_owned(),
            platform: Box::new(platform.clone()),
        });
    }
    Ok(())
}

/// What an installation actually did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installation {
    pub plan: InstallationPlan,
    /// Distinct asset objects the index named.
    pub assets: usize,
    /// Files unpacked into the natives directory.
    pub natives_files: usize,
}

/// Mojang's asset resource endpoint, which addresses objects by digest.
///
/// The one URL in the pipeline that is *constructed* rather than read from
/// metadata, because an asset index publishes hashes and not links. It is still
/// safe to build: the digest is validated hex, so nothing provider-controlled
/// can reshape the URL, and the response is verified against that same digest
/// before it is accepted.
pub const ASSET_RESOURCE_BASE: &str = "https://resources.download.minecraft.net";

/// Installs one version's files into the shared stores.
///
/// Mirrors [`crate::VersionCatalog`]: it owns the download manager and the
/// cache layout so that no caller has to remember to pair them, and it exposes
/// an endpoint override that exists for the offline tests rather than as a
/// configuration knob.
#[derive(Debug, Clone)]
pub struct VanillaInstaller {
    manager: DownloadManager,
    layout: CacheLayout,
    assets_base: String,
}

impl VanillaInstaller {
    /// An installer fetching from Mojang.
    #[must_use]
    pub fn new(manager: DownloadManager, layout: CacheLayout) -> Self {
        Self {
            manager,
            layout,
            assets_base: ASSET_RESOURCE_BASE.to_owned(),
        }
    }

    /// Points the asset endpoint somewhere else.
    ///
    /// For the loopback tests, which need the constructed URL to reach a mock
    /// server - the same reason `VersionCatalog::new` takes a manifest URL. Not
    /// a user setting: redirecting it would make every asset digest meaningless.
    #[must_use]
    pub fn with_assets_base(mut self, base: &str) -> Self {
        base.trim_end_matches('/').clone_into(&mut self.assets_base);
        self
    }

    /// The paths and artifacts one version needs, without any I/O.
    pub fn plan(
        &self,
        metadata: &VersionMetadata,
        context: &RuleContext,
    ) -> LaunchResult<InstallationPlan> {
        InstallationPlan::resolve(metadata, context, &self.layout)
    }

    /// Downloads and unpacks everything a plan names.
    ///
    /// Every fetch goes through the download manager, so every one of them
    /// inherits hash verification, atomic finalization, retry, and the redirect
    /// and transport policy. There is deliberately no second
    /// path for "just this one small file".
    #[instrument(skip_all, fields(version = %plan.version))]
    pub async fn install(
        &self,
        plan: InstallationPlan,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> LaunchResult<Installation> {
        let required = plan.required_artifacts();
        let requests = required
            .iter()
            .map(|planned| request_for(planned))
            .collect::<LaunchResult<Vec<_>>>()?;

        reporter.set_step(Some(format!("{} · libraries", plan.version)));
        self.manager
            .download_all(&requests, reporter, cancel)
            .await
            .map_err(|failure| {
                if failure.is_cancellation() {
                    return LaunchError::Canceled;
                }
                let name = required.get(failure.index()).map_or_else(
                    || "a required file".to_owned(),
                    |planned| planned.label.clone(),
                );
                LaunchError::download(name, failure.into_error())
            })?;

        // The index is only readable once it is on disk, and its contents
        // decide the second, much larger batch.
        let index_json = tokio::fs::read_to_string(&plan.asset_index.path)
            .await
            .map_err(|source| LaunchError::GameDirectory {
                path: plan.asset_index.path.clone(),
                source,
            })?;
        let index = AssetIndex::parse(&plan.asset_index_id, &index_json)?;

        reporter.set_step(Some(format!("{} · assets", plan.version)));
        let assets = self.download_assets(&index, reporter, cancel).await?;
        let natives_files = unpack_natives(&plan).await?;
        materialize_verified(&plan.asset_index.path, &plan.asset_index_path).await?;

        debug!(
            version = %plan.version,
            assets,
            natives_files,
            classpath = plan.libraries.len() + 1,
            "installation complete"
        );
        Ok(Installation {
            plan,
            assets,
            natives_files,
        })
    }

    /// Fetches every distinct object the index names.
    async fn download_assets(
        &self,
        index: &AssetIndex,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> LaunchResult<usize> {
        let objects = index.distinct_objects();
        let mut requests = Vec::with_capacity(objects.len());
        for (name, object) in &objects {
            let destination = self.layout.asset_object(&object.hash)?;
            requests.push(
                DownloadRequest::new(url(&self.asset_url(&object.hash))?, destination)
                    .verifying(object.hash.clone())
                    .sized(object.size)
                    .labeled((*name).to_owned()),
            );
        }

        self.manager
            .download_all(&requests, reporter, cancel)
            .await
            .map_err(|failure| {
                if failure.is_cancellation() {
                    return LaunchError::Canceled;
                }
                let name = objects
                    .get(failure.index())
                    .map_or_else(|| "assets".to_owned(), |(name, _)| (*name).to_owned());
                LaunchError::download(name, failure.into_error())
            })?;
        Ok(objects.len())
    }

    fn asset_url(&self, hash: &ephervex_model::Hash) -> String {
        let digest = hash.digest();
        let prefix = digest.get(..2).unwrap_or(digest);
        format!("{}/{prefix}/{digest}", self.assets_base)
    }
}

static PRESENTATION_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Atomically publishes a verified cache artifact at the logical path a game
/// opens directly.
///
/// This is intentionally a copy rather than a hard link: the launch-visible
/// file is user-browsable, and modifying it must never mutate the canonical
/// hash-addressed artifact whose presence means "verified".
async fn materialize_verified(source: &Path, destination: &Path) -> LaunchResult<()> {
    let parent = destination
        .parent()
        .ok_or_else(|| LaunchError::GameDirectory {
            path: destination.to_path_buf(),
            source: std::io::Error::other("presentation path has no parent"),
        })?;
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|source| LaunchError::GameDirectory {
            path: parent.to_path_buf(),
            source,
        })?;

    let counter = PRESENTATION_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut temporary_name = destination.file_name().unwrap_or_default().to_os_string();
    temporary_name.push(format!(".{}.{}.part", std::process::id(), counter));
    let temporary = parent.join(temporary_name);

    if let Err(source) = tokio::fs::copy(source, &temporary).await {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(LaunchError::GameDirectory {
            path: destination.to_path_buf(),
            source,
        });
    }
    if let Err(source) = tokio::fs::rename(&temporary, destination).await {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(LaunchError::GameDirectory {
            path: destination.to_path_buf(),
            source,
        });
    }
    Ok(())
}

/// Unpacks the pre-1.19 natives jars, on a blocking thread.
///
/// `zip` is a synchronous reader and unpacking is CPU- and disk-bound, so doing
/// it on the async runtime's worker would stall every other task for the
/// duration.
async fn unpack_natives(plan: &InstallationPlan) -> LaunchResult<usize> {
    if plan.natives.is_empty() {
        // 1.19 onward. The directory is still created: `-Djava.library.path`
        // and LWJGL's extract path both point at it, and LWJGL will not create
        // a missing one for itself.
        tokio::fs::create_dir_all(&plan.natives_dir)
            .await
            .map_err(|source| LaunchError::GameDirectory {
                path: plan.natives_dir.clone(),
                source,
            })?;
        return Ok(0);
    }

    let natives = plan.natives.clone();
    let destination = plan.natives_dir.clone();
    task::spawn_blocking(move || {
        let mut written = 0;
        for planned in &natives {
            let report = natives::extract(&planned.artifact.path, &destination, &planned.exclude)?;
            written += report.files.len();
        }
        Ok(written)
    })
    .await
    .map_err(|error| LaunchError::ProcessLost {
        source: std::io::Error::other(error),
    })?
}

fn request_for(planned: &PlannedArtifact) -> LaunchResult<DownloadRequest> {
    Ok(
        DownloadRequest::new(url(&planned.artifact.url)?, planned.path.clone())
            .verifying(planned.artifact.sha1.clone())
            .sized(planned.artifact.size)
            .labeled(planned.label.clone()),
    )
}

/// Parses a URL out of metadata.
///
/// Deliberately `DownloadRequest::new` rather than `::parse`: the transport
/// policy belongs to the download manager, which applies it from its
/// `HttpConfig` on every attempt. Checking it a second time here would only
/// mean the offline tests could not point the pipeline at a loopback server -
/// which is exactly the trade `VersionCatalog` already makes.
fn url(value: &str) -> Result<reqwest::Url, UrlPolicyError> {
    reqwest::Url::parse(value).map_err(|_| UrlPolicyError::Malformed {
        url: value.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::rules::OsName;
    use crate::test_support::fixture;

    fn layout() -> CacheLayout {
        CacheLayout::new("/cache/ephervex")
    }

    fn metadata(name: &str) -> VersionMetadata {
        VersionMetadata::parse(fixture(name))
            .unwrap_or_else(|error| panic!("fixture `{name}` should parse: {error}"))
    }

    fn linux() -> RuleContext {
        RuleContext::new(Platform::new(OsName::Linux, "x86_64", "6.8.0"))
    }

    fn plan(name: &str, context: &RuleContext) -> InstallationPlan {
        InstallationPlan::resolve(&metadata(name), context, &layout())
            .unwrap_or_else(|error| panic!("`{name}` should plan: {error}"))
    }

    fn names(artifacts: &[PlannedArtifact]) -> Vec<&str> {
        artifacts
            .iter()
            .map(|planned| planned.label.as_str())
            .collect()
    }

    #[test]
    fn a_selected_legacy_native_without_an_artifact_refuses_the_plan() {
        for name in [
            "org.lwjgl.lwjgl:lwjgl-platform:2.9.4-nightly-20150209",
            "net.java.jinput:jinput-platform:2.0.5",
        ] {
            let mut metadata = metadata("1.12.2.json");
            let library = metadata
                .libraries
                .iter_mut()
                .find(|library| library.name == name)
                .expect("native library");
            assert!(
                library
                    .downloads
                    .classifiers
                    .remove("natives-linux")
                    .is_some()
            );
            assert_eq!(library.natives_classifier("linux"), Some("natives-linux"));

            let error = InstallationPlan::resolve(&metadata, &linux(), &layout())
                .expect_err("a declared native dependency cannot be omitted");
            assert_eq!(error.message_id(), "error-launch-missing-native-artifact");
            assert!(
                matches!(error, LaunchError::MissingNativeArtifact { library, classifier }
                if library == name && classifier == "natives-linux")
            );
        }
    }

    #[test]
    fn a_missing_native_artifact_names_the_substituted_architecture() {
        let mut metadata = metadata("1.12.2.json");
        metadata.libraries.retain(|library| {
            library.name == "com.mojang:text2speech:1.10.3" && library.is_legacy_natives()
        });
        assert_eq!(metadata.libraries.len(), 1);
        metadata.libraries[0]
            .natives
            .insert("windows".to_owned(), "natives-windows-${arch}".to_owned());
        for (arch, classifier) in [
            ("x86", "natives-windows-32"),
            ("x86_64", "natives-windows-64"),
        ] {
            let context = RuleContext::new(Platform::new(OsName::Windows, arch, ""));
            let error = InstallationPlan::resolve(&metadata, &context, &layout())
                .expect_err("the generic Windows artifact is not a fallback");
            assert!(
                matches!(error, LaunchError::MissingNativeArtifact { library, classifier: selected }
                if library == "com.mojang:text2speech:1.10.3" && selected == classifier)
            );
        }
    }

    #[test]
    fn a_native_map_without_the_target_os_remains_inapplicable() {
        let mut metadata = metadata("1.12.2.json");
        let name = "org.lwjgl.lwjgl:lwjgl-platform:2.9.4-nightly-20150209";
        let library = metadata
            .libraries
            .iter_mut()
            .find(|library| library.name == name)
            .expect("native library");
        library.natives.remove("linux");
        let plan = InstallationPlan::resolve(&metadata, &linux(), &layout())
            .expect("a map for other operating systems is not a broken reference");
        assert!(plan.excluded.iter().any(|library| library.name == name
            && library.reason == ExclusionReason::NoNativesForPlatform));
        assert!(
            plan.libraries.iter().any(|library| library.label == name),
            "the ordinary classpath artifact remains applicable"
        );
    }

    #[test]
    fn missing_native_artifacts_in_rule_excluded_libraries_do_not_block_a_plan() {
        let mut metadata = metadata("1.12.2.json");
        let name = "org.lwjgl.lwjgl:lwjgl-platform:2.9.2-nightly-20140822";
        let library = metadata
            .libraries
            .iter_mut()
            .find(|library| library.name == name)
            .expect("macOS-only native library");
        library.downloads.classifiers.clear();
        let plan = InstallationPlan::resolve(&metadata, &linux(), &layout())
            .expect("inapplicable native artifacts are not required");
        assert!(
            plan.excluded.iter().any(|library| library.name == name
                && library.reason == ExclusionReason::RuleNotSatisfied)
        );
    }

    #[test]
    fn linux_arm_refuses_x86_natives_in_both_metadata_formats() {
        for name in ["1.12.2.json", "1.20.1.json", "1.21.1.json"] {
            for arch in ["arm64", "arm"] {
                let context = RuleContext::new(Platform::new(OsName::Linux, arch, ""));
                let error = InstallationPlan::resolve(&metadata(name), &context, &layout())
                    .expect_err("x86 natives cannot run on ARM");
                assert_eq!(
                    error.message_id(),
                    "error-launch-incompatible-native-library"
                );
                assert!(
                    matches!(
                        error,
                        LaunchError::IncompatibleNativeLibrary { library, classifier, platform }
                            if library.starts_with("org.lwjgl")
                                && classifier == "natives-linux"
                                && *platform == context.platform
                    ),
                    "{name}: {arch}"
                );
            }
        }
    }

    #[test]
    fn compatible_arm_natives_keep_the_selected_version_and_artifact() {
        let mut metadata = metadata("1.21.1.json");
        let mut library = metadata
            .libraries
            .iter()
            .find(|library| library.name.ends_with(":natives-linux"))
            .expect("Linux LWJGL fixture")
            .clone();
        library.name = "org.lwjgl:lwjgl-glfw:3.3.3:natives-linux-arm64".to_owned();
        library.coordinate =
            ephervex_download::MavenCoordinate::parse(&library.name).expect("valid ARM coordinate");
        let artifact = library
            .downloads
            .artifact
            .as_mut()
            .expect("native artifact");
        artifact.url = "https://libraries.minecraft.net/org/lwjgl/lwjgl-glfw/3.3.3/lwjgl-glfw-3.3.3-natives-linux-arm64.jar".to_owned();
        artifact.path = Some(
            library
                .coordinate
                .relative_path()
                .to_string_lossy()
                .into_owned(),
        );
        metadata.libraries = vec![library.clone()];

        let context = RuleContext::new(Platform::new(OsName::Linux, "arm64", ""));
        let plan = InstallationPlan::resolve(&metadata, &context, &layout())
            .expect("explicit ARM metadata is supported");
        assert_eq!(plan.version, metadata.id);
        assert_eq!(plan.libraries.len(), 1);
        assert_eq!(plan.libraries[0].label, library.name);
        assert_eq!(
            plan.libraries[0].artifact,
            library.downloads.artifact.unwrap()
        );
        assert_eq!(
            plan.libraries[0].path,
            layout().library(&library.coordinate)
        );
        assert!(plan.natives.is_empty());
        assert!(
            matches!(
                InstallationPlan::resolve(&metadata, &linux(), &layout()),
                Err(LaunchError::IncompatibleNativeLibrary { .. })
            ),
            "ARM natives must not be accepted on x86-64 either"
        );
    }

    #[test]
    fn legacy_lwjgl3_natives_are_checked_before_extraction() {
        let mut metadata = metadata("1.12.2.json");
        let mut library = metadata
            .libraries
            .iter()
            .find(|library| library.coordinate.group() == "org.lwjgl.lwjgl")
            .expect("legacy LWJGL fixture")
            .clone();
        library.name = "org.lwjgl:lwjgl:3.3.1".to_owned();
        library.coordinate =
            ephervex_download::MavenCoordinate::parse(&library.name).expect("valid coordinate");
        metadata.libraries = vec![library.clone()];
        let context = RuleContext::new(Platform::new(OsName::Linux, "arm64", ""));
        assert!(matches!(
            InstallationPlan::resolve(&metadata, &context, &layout()),
            Err(LaunchError::IncompatibleNativeLibrary { .. })
        ));

        let artifact = library
            .downloads
            .classifiers
            .remove("natives-linux")
            .expect("native artifact");
        library
            .downloads
            .classifiers
            .insert("natives-linux-arm64".to_owned(), artifact.clone());
        library
            .natives
            .insert("linux".to_owned(), "natives-linux-arm64".to_owned());
        metadata.libraries = vec![library];
        let plan = InstallationPlan::resolve(&metadata, &context, &layout())
            .expect("legacy metadata can explicitly select ARM natives");
        assert_eq!(plan.natives.len(), 1);
        assert_eq!(plan.natives[0].artifact.artifact, artifact);
        assert_eq!(
            plan.natives[0].artifact.label,
            "org.lwjgl:lwjgl:3.3.1:natives-linux-arm64"
        );
    }

    #[test]
    fn incompatible_natives_excluded_by_rules_do_not_block_a_plan() {
        let mut metadata = metadata("1.21.1.json");
        metadata
            .libraries
            .retain(|library| library.name.ends_with(":natives-windows"));
        assert!(!metadata.libraries.is_empty());
        let context = RuleContext::new(Platform::new(OsName::Linux, "arm64", ""));
        let plan = InstallationPlan::resolve(&metadata, &context, &layout())
            .expect("excluded libraries cannot prevent a launch");
        assert!(plan.libraries.is_empty());
        assert_eq!(plan.excluded.len(), metadata.libraries.len());
    }

    #[test]
    fn every_era_plans_a_classpath_ending_in_the_client_jar() {
        for name in ["1.12.2.json", "1.20.1.json", "1.21.1.json", "26.1.json"] {
            let plan = plan(name, &linux());
            let classpath = plan.classpath();

            assert_eq!(
                classpath.last(),
                Some(&plan.client_jar.path),
                "{name}: a library that shadows a game class has to win"
            );
            assert_eq!(classpath.len(), plan.libraries.len() + 1, "{name}");
            assert!(
                classpath
                    .iter()
                    .take(plan.libraries.len())
                    .all(|path| path.starts_with("/cache/ephervex/libraries")),
                "{name}: libraries come from the shared store"
            );
        }
    }

    #[test]
    fn the_classpath_is_this_versions_libraries_and_never_whatever_is_cached() {
        // The shared store holds every library of every version ever launched.
        let plan = plan("1.20.1.json", &linux());
        let expected: Vec<PathBuf> = metadata("1.20.1.json")
            .libraries
            .iter()
            .filter(|library| rules_allow(&library.rules, &linux()))
            .filter_map(|library| {
                library
                    .downloads
                    .artifact
                    .as_ref()
                    .map(|_| layout().library(&library.coordinate))
            })
            .collect();
        assert_eq!(
            plan.libraries
                .iter()
                .map(|planned| planned.path.clone())
                .collect::<Vec<_>>(),
            expected
        );
    }

    #[test]
    fn a_1_12_2_launch_unpacks_the_linux_classifier_and_leaves_the_others_alone() {
        let plan = plan("1.12.2.json", &linux());

        assert!(!plan.natives.is_empty(), "the pre-1.19 form has to unpack");
        assert!(
            plan.natives
                .iter()
                .all(|natives| natives.artifact.label.ends_with(":natives-linux")),
            "only this platform's classifier: {:?}",
            names(
                &plan
                    .natives
                    .iter()
                    .map(|natives| natives.artifact.clone())
                    .collect::<Vec<_>>()
            )
        );
        assert!(
            plan.natives
                .iter()
                .all(|natives| natives.exclude == ["META-INF/"]),
            "extract.exclude comes from the metadata"
        );
        assert_eq!(
            plan.natives_dir,
            PathBuf::from("/cache/ephervex/natives/1.12.2/linux-x86_64"),
            "natives are launcher-level, not copied into every instance"
        );
    }

    #[test]
    fn a_library_carrying_both_a_jar_and_natives_contributes_to_both_halves() {
        // 1.12.2 lists `com.mojang:text2speech:1.10.3` twice: once as a plain
        // classpath jar and once carrying the natives map. Both matter, and the
        // duplicate path must appear on the classpath only once.
        let plan = plan("1.12.2.json", &linux());
        let text2speech: Vec<_> = plan
            .libraries
            .iter()
            .filter(|planned| planned.label.starts_with("com.mojang:text2speech"))
            .collect();
        assert_eq!(text2speech.len(), 1, "deduplicated on the classpath");
        assert!(
            plan.natives
                .iter()
                .any(|natives| natives.artifact.label.starts_with("com.mojang:text2speech")),
            "and still unpacked"
        );
        assert!(
            plan.excluded
                .iter()
                .any(|excluded| excluded.reason == ExclusionReason::AlreadyOnClasspath),
            "the duplicate is reported rather than silently dropped"
        );
    }

    #[test]
    fn a_classifier_only_library_contributes_nothing_to_the_classpath() {
        // `net.java.jinput:jinput-platform` ships natives and no jar.
        let plan = plan("1.12.2.json", &linux());
        assert!(
            !names(&plan.libraries)
                .iter()
                .any(|name| name.starts_with("net.java.jinput:jinput-platform")),
            "there is no jar to put on it"
        );
        assert!(plan.natives.iter().any(|natives| {
            natives
                .artifact
                .label
                .starts_with("net.java.jinput:jinput-platform")
        }));
    }

    #[test]
    fn from_1_19_onward_natives_are_classpath_entries_and_nothing_is_unpacked() {
        for name in ["1.20.1.json", "1.21.1.json", "26.1.json"] {
            let plan = plan(name, &linux());
            assert!(
                plan.natives.is_empty(),
                "{name}: the modern form has no extraction step"
            );
            assert!(
                names(&plan.libraries)
                    .iter()
                    .any(|label| label.ends_with(":natives-linux")),
                "{name}: and the natives jar belongs on the classpath instead"
            );
            assert!(
                !names(&plan.libraries)
                    .iter()
                    .any(|label| label.contains(":natives-windows")
                        || label.contains(":natives-macos")),
                "{name}: another platform's natives must never be on it"
            );
        }
    }

    #[test]
    fn the_two_natives_forms_are_never_normalized_into_one_another() {
        // They behave differently at launch: one is unpacked and reached
        // through `java.library.path`, the other is read from the classpath.
        let legacy = plan("1.12.2.json", &linux());
        let modern = plan("1.21.1.json", &linux());
        assert!(!legacy.natives.is_empty() && modern.natives.is_empty());
        assert!(
            !names(&legacy.libraries)
                .iter()
                .any(|label| label.contains(":natives-")),
            "a pre-1.19 coordinate never carries the classifier"
        );
    }

    #[test]
    fn a_launch_on_another_platform_selects_that_platforms_files() {
        let windows = RuleContext::new(Platform::new(OsName::Windows, "x86_64", "10.0.19045"));
        let plan = plan("1.21.1.json", &windows);
        assert!(
            names(&plan.libraries)
                .iter()
                .any(|label| label.ends_with(":natives-windows"))
        );
        assert!(
            !names(&plan.libraries)
                .iter()
                .any(|label| label.ends_with(":natives-linux"))
        );
        assert_eq!(
            plan.natives_dir,
            PathBuf::from("/cache/ephervex/natives/1.21.1/windows-x86_64"),
            "one version's natives are keyed by platform, so two cannot collide"
        );
    }

    #[test]
    fn a_macos_only_library_is_excluded_on_linux_with_the_reason_recorded() {
        let plan = plan("1.12.2.json", &linux());
        let objc = plan
            .excluded
            .iter()
            .find(|excluded| excluded.name.starts_with("ca.weblite:java-objc-bridge"))
            .expect("the osx-gated library is reported");
        assert_eq!(objc.reason, ExclusionReason::RuleNotSatisfied);
        assert_eq!(objc.reason.as_str(), "rule-not-satisfied");
    }

    #[test]
    fn the_logging_configuration_is_planned_with_the_argument_that_names_it() {
        let plan = plan("1.12.2.json", &linux());
        let logging = plan.logging.expect("1.12.2 ships a log4j configuration");
        assert_eq!(logging.argument, "-Dlog4j.configurationFile=${path}");
        assert!(
            logging
                .artifact
                .path
                .starts_with("/cache/ephervex/downloads")
        );
        assert_eq!(logging.artifact.label, "client-1.12.xml");
    }

    #[test]
    fn the_asset_index_is_planned_and_names_the_id_the_game_is_given() {
        for (name, id) in [
            ("1.12.2.json", "1.12"),
            ("1.20.1.json", "5"),
            ("1.21.1.json", "17"),
            ("26.1.json", "30"),
        ] {
            let plan = plan(name, &linux());
            assert_eq!(plan.asset_index_id, id, "{name}");
            assert_eq!(
                plan.asset_index_path,
                PathBuf::from(format!("/cache/ephervex/assets/indexes/{id}.json")),
                "{name}: Minecraft opens the index below the assets root"
            );
            assert_eq!(
                plan.assets_dir,
                PathBuf::from("/cache/ephervex/assets"),
                "{name}: assets are shared, never copied into an instance"
            );
        }
    }

    #[test]
    fn a_loader_child_document_cannot_be_planned_as_a_vanilla_launch() {
        let child = VersionMetadata::parse(
            r#"{"id":"1.20.1-forge-47.4.0","inheritsFrom":"1.20.1","type":"release",
                "time":"2024-01-01T00:00:00+00:00","releaseTime":"2024-01-01T00:00:00+00:00",
                "mainClass":"cpw.mods.bootstraplauncher.BootstrapLauncher","libraries":[]}"#,
        )
        .expect("a child parses");

        let error = InstallationPlan::resolve(&child, &linux(), &layout())
            .expect_err("a document with no client jar is not launchable");
        assert!(matches!(error, LaunchError::InvalidVersionMetadata { .. }));
        assert_eq!(error.message_id(), "error-launch-invalid-version-metadata");
    }

    #[test]
    fn every_required_artifact_is_addressed_by_its_digest_or_its_coordinate() {
        // nothing lands at a path a URL or a filename chose.
        let plan = plan("1.12.2.json", &linux());
        for planned in plan.required_artifacts() {
            let path = planned.path.to_string_lossy().into_owned();
            let addressed = path.contains(planned.artifact.sha1.digest())
                || path.starts_with("/cache/ephervex/libraries/");
            assert!(addressed, "{} lands at {path}", planned.label);
        }
    }

    #[test]
    fn an_asset_url_is_built_from_the_validated_digest_and_nothing_else() {
        let hash = ephervex_model::Hash::new(
            ephervex_model::HashAlgorithm::Sha1,
            "5ff04807c356f1beed0b86ccf659b44b9983e3fa",
        )
        .expect("valid sha1");
        let installer = VanillaInstaller::new(
            DownloadManager::new(ephervex_download::DownloaderConfig::default())
                .expect("client builds"),
            layout(),
        );
        assert_eq!(
            installer.asset_url(&hash),
            "https://resources.download.minecraft.net/5f/5ff04807c356f1beed0b86ccf659b44b9983e3fa"
        );
        assert_eq!(
            installer
                .with_assets_base("http://127.0.0.1:8080/")
                .asset_url(&hash),
            "http://127.0.0.1:8080/5f/5ff04807c356f1beed0b86ccf659b44b9983e3fa",
            "the offline tests need the constructed url to reach a mock server"
        );
    }
}
