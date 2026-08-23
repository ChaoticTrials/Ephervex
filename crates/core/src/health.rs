//! A local, read-only picture of one installation's health.
//!
//! This is what `ephervex doctor` reports, and it exists in the application
//! layer rather than in a frontend because the question -- can this machine run
//! Ephervex, and if not, which part is wrong -- is application knowledge that a
//! GUI would otherwise have to reimplement.
//!
//! Three rules shape it:
//!
//! - **Local only.** Nothing here makes a network request. A machine with no
//!   connection must still be able to say what is wrong with it.
//! - **No secret, ever.** The credential backend is reported as reachable or
//!   not, with the platform's own explanation when it is not. Whether a
//!   particular credential exists, let alone what it is, is not part of this
//!   report (ADR 0004).
//! - **Degrades instead of failing.** Every section is independently fallible.
//!   A broken database must not stop the report from saying where the
//!   configuration is, because "the database is broken" is exactly the answer
//!   somebody ran this to get.

use std::path::{Path, PathBuf};

use ephervex_auth::CredentialStorage;
use ephervex_model::ArtifactCacheLimit;
use ephervex_options::{MinecraftPreferenceCatalog, OptionsDataset};
use ephervex_storage::{Config, ConfigError, Paths};

use crate::app::App;
use crate::artifact_cache::ArtifactCacheStatus;

/// One installation, described.
#[derive(Debug, Clone)]
pub struct InstallationHealth {
    pub application: ApplicationIdentity,
    pub platform: PlatformIdentity,
    /// Empty only when the XDG roots themselves could not be resolved, which
    /// is the one failure that leaves nothing else to say.
    pub directories: Vec<DirectoryHealth>,
    pub paths_error: Option<String>,
    /// Absent when the XDG roots could not be resolved; `paths_error` says why.
    ///
    /// There is no configuration file to describe when its location is unknown,
    /// so this is `None` rather than a report about an empty path. "Ephervex
    /// cannot tell where your configuration lives" and "you have not written
    /// one yet" are opposite answers, and a diagnostic that confuses them sends
    /// the reader after the wrong problem.
    pub configuration: Option<ConfigurationHealth>,
    pub credential_storage: CredentialBackendHealth,
    pub bundled_data: BundledDataHealth,
    /// Absent when storage could not be opened; `storage_error` says why.
    pub artifact_cache: Option<ArtifactCacheHealth>,
    /// Absent when storage could not be opened; a discovery failure is a
    /// [`JavaHealth::Unavailable`] rather than a missing section.
    pub java: Option<JavaHealth>,
    pub storage_error: Option<String>,
}

impl InstallationHealth {
    /// Whether anything here would stop Ephervex from working.
    ///
    /// Application knowledge rather than a frontend's: a caller that turns this
    /// report into an exit status, a banner, or a warning icon should not each
    /// decide separately what counts as broken.
    ///
    /// The artifact cache and the Java scan are deliberately not faults. A cache
    /// held by another operation refuses to be measured rather than blocking,
    /// and a machine with no usable Java is one Ephervex offers to fix rather
    /// than one that is broken.
    pub fn has_fault(&self) -> bool {
        if self.paths_error.is_some() || self.storage_error.is_some() {
            return true;
        }
        match &self.configuration {
            // No configuration section means the storage locations did not
            // resolve, which `paths_error` has already reported.
            None | Some(ConfigurationHealth::Unusable { .. }) => return true,
            Some(ConfigurationHealth::Absent { .. } | ConfigurationHealth::Loaded { .. }) => {}
        }
        if matches!(
            self.bundled_data.options_dataset,
            BundledDocumentHealth::Unusable { .. }
        ) || matches!(
            self.bundled_data.preference_catalog,
            BundledDocumentHealth::Unusable { .. }
        ) {
            return true;
        }
        self.directories.iter().any(|directory| {
            matches!(
                directory.state,
                DirectoryState::NotWritable { .. } | DirectoryState::Unusable { .. }
            )
        })
    }
}

/// What this build is.
#[derive(Debug, Clone)]
pub struct ApplicationIdentity {
    pub name: &'static str,
    pub version: &'static str,
    pub app_id: &'static str,
    pub user_agent: String,
    pub language: String,
}

/// What it is running on.
#[derive(Debug, Clone, Copy)]
pub struct PlatformIdentity {
    pub os: &'static str,
    pub arch: &'static str,
    pub family: &'static str,
}

/// One of the launcher's directories.
#[derive(Debug, Clone)]
pub struct DirectoryHealth {
    /// A stable identifier, matching the key `ephervex paths` reports.
    pub purpose: &'static str,
    pub path: PathBuf,
    pub state: DirectoryState,
}

/// What is actually at one of the launcher's directories.
///
/// One state rather than an existence flag beside a writability flag, so a
/// directory that does not exist cannot also claim to be writable.
#[derive(Debug, Clone)]
pub enum DirectoryState {
    /// Nothing is there. A first run, not a fault: the directory is created on
    /// first use. Reported only for a genuine `NotFound`, never for a
    /// directory that merely could not be inspected.
    Absent,
    /// There, and a file could actually be created in it.
    ///
    /// Proven rather than inferred from a permission bit, because a read-only
    /// mount, a full disk, and an immutable attribute all present as writable
    /// permissions.
    Writable,
    /// There, and a probe file could not be created. Carries the reason.
    NotWritable { detail: String },
    /// There is something in the way, or the path could not be inspected at
    /// all: a regular file where a directory belongs, a symbolic link with no
    /// target, or a permission error on the way down. Distinct from [`Absent`]
    /// because "Ephervex may not look at this" is a different problem from
    /// "this does not exist yet", and only the second one fixes itself.
    ///
    /// [`Absent`]: DirectoryState::Absent
    Unusable { detail: String },
}

/// Whether the configuration file can be read.
#[derive(Debug, Clone)]
pub enum ConfigurationHealth {
    /// No file yet. A first run, not a fault.
    Absent {
        path: PathBuf,
    },
    Loaded {
        path: PathBuf,
        schema_version: u32,
    },
    /// Present and unreadable. Carries the localized message id so a frontend
    /// can say what is wrong in the user's language.
    Unusable {
        path: PathBuf,
        message_id: &'static str,
        detail: String,
    },
}

/// Whether secrets have somewhere to live.
#[derive(Debug, Clone)]
pub struct CredentialBackendHealth {
    pub available: bool,
    pub backend: &'static str,
    /// The platform's own explanation when the Secret Service is unreachable.
    /// Never a credential: this is the store's status, not its contents.
    pub detail: Option<String>,
}

/// Whether the data compiled into this binary is intact.
#[derive(Debug, Clone)]
pub struct BundledDataHealth {
    pub options_dataset: BundledDocumentHealth,
    pub preference_catalog: BundledDocumentHealth,
}

/// One compiled-in document.
#[derive(Debug, Clone)]
pub enum BundledDocumentHealth {
    Loaded {
        /// Releases mapped, or preferences exposed, depending on the document.
        entries: usize,
        revision: Option<u32>,
    },
    Unusable {
        detail: String,
    },
}

/// The reusable artifact cache, measured.
#[derive(Debug, Clone)]
pub enum ArtifactCacheHealth {
    Measured {
        used_bytes: u64,
        entries: u64,
        /// False when an operation held the cache and part of it was skipped.
        complete: bool,
        limit: ArtifactCacheLimit,
        over_budget: bool,
    },
    /// The measurement could not run. Not a fault in itself: a cache in use
    /// refuses rather than blocks.
    Unavailable { detail: String },
}

/// What Java this machine offers.
#[derive(Debug, Clone)]
pub enum JavaHealth {
    Discovered {
        discovered: usize,
        managed: usize,
        /// Major versions found, ascending, deduplicated.
        major_versions: Vec<u32>,
    },
    /// The scan itself failed, which is not the same answer as finding no Java
    /// and must not be rendered as one.
    Unavailable { detail: String },
}

/// Collects the report.
///
/// Never returns an error: a failure in any one section is part of the answer.
pub async fn installation_health() -> InstallationHealth {
    let application = ApplicationIdentity {
        name: ephervex_model::APP_NAME,
        version: ephervex_model::APP_VERSION,
        app_id: ephervex_model::APP_ID,
        user_agent: ephervex_model::user_agent(),
        language: ephervex_i18n::current_language().to_string(),
    };
    let platform = PlatformIdentity {
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        family: std::env::consts::FAMILY,
    };

    let mut health = InstallationHealth {
        application,
        platform,
        directories: Vec::new(),
        paths_error: None,
        configuration: None,
        credential_storage: probe_credential_storage(),
        bundled_data: probe_bundled_data(),
        artifact_cache: None,
        java: None,
        storage_error: None,
    };

    let paths = match Paths::from_environment() {
        Ok(paths) => paths,
        Err(error) => {
            health.paths_error = Some(error.to_string());
            return health;
        }
    };

    health.configuration = Some(probe_configuration(&paths));
    // The configured instance root moves one of the reported directories, so
    // the configuration is read before the layout is described.
    let paths = match Config::load_from(&paths.config_file()) {
        Ok(config) => config.apply_to(paths),
        Err(_) => paths,
    };
    // Probed before storage is opened: opening creates the roots, and a report
    // that creates what it is asked to inspect cannot answer the question.
    health.directories = probe_directories(&paths);

    match App::open() {
        Ok(app) => {
            health.artifact_cache = Some(probe_artifact_cache(&app).await);
            health.java = Some(probe_java(&app).await);
        }
        Err(error) => health.storage_error = Some(error.to_string()),
    }

    health
}

fn probe_credential_storage() -> CredentialBackendHealth {
    match CredentialStorage::probe() {
        Ok(storage) => CredentialBackendHealth {
            available: true,
            backend: storage.as_str(),
            detail: None,
        },
        Err(detail) => CredentialBackendHealth {
            available: false,
            backend: CredentialStorage::SessionOnly.as_str(),
            detail: Some(detail),
        },
    }
}

fn probe_bundled_data() -> BundledDataHealth {
    let dataset = OptionsDataset::bundled_checked().map_or_else(
        |error| BundledDocumentHealth::Unusable {
            detail: error.to_string(),
        },
        |dataset| BundledDocumentHealth::Loaded {
            entries: dataset.mapped_releases().len(),
            revision: Some(dataset.revision),
        },
    );
    let catalog = MinecraftPreferenceCatalog::bundled().map_or_else(
        |error| BundledDocumentHealth::Unusable {
            detail: error.to_string(),
        },
        |catalog| BundledDocumentHealth::Loaded {
            entries: catalog.descriptors.len(),
            revision: None,
        },
    );
    BundledDataHealth {
        options_dataset: dataset,
        preference_catalog: catalog,
    }
}

fn probe_configuration(paths: &Paths) -> ConfigurationHealth {
    let path = paths.config_file();
    // Not `Path::exists`, which reports a permission error as "no". A
    // configuration that cannot be read is the opposite of one that has not
    // been written, and answering the second when the first is true tells the
    // reader their settings are gone.
    match std::fs::metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // `metadata` follows links, so a configuration symlinked onto a
            // volume that is not mounted also lands here. Something is there,
            // and calling it absent is the same wrong answer as calling an
            // unreadable file absent.
            return if std::fs::symlink_metadata(&path).is_ok() {
                ConfigurationHealth::Unusable {
                    message_id: "error-config-unreadable",
                    detail: "a symbolic link whose target does not exist".to_owned(),
                    path,
                }
            } else {
                ConfigurationHealth::Absent { path }
            };
        }
        Err(source) => {
            // Classified by the crate that owns configuration errors, so the
            // message the user sees is the same one every other unreadable
            // configuration produces.
            let error = ConfigError::Unreadable {
                path: path.clone(),
                source,
            };
            return ConfigurationHealth::Unusable {
                message_id: error.message_id(),
                detail: error.to_string(),
                path,
            };
        }
        Ok(_) => {}
    }
    match Config::load_from(&path) {
        Ok(config) => ConfigurationHealth::Loaded {
            path,
            schema_version: config.schema_version,
        },
        Err(error) => ConfigurationHealth::Unusable {
            path,
            message_id: error.message_id(),
            detail: error.to_string(),
        },
    }
}

fn probe_directories(paths: &Paths) -> Vec<DirectoryHealth> {
    // The same keys `ephervex paths` uses, so the two reports line up.
    [
        ("config", paths.config_dir().to_path_buf()),
        ("data", paths.data_dir().to_path_buf()),
        ("cache", paths.cache_dir().to_path_buf()),
        ("state", paths.state_dir().to_path_buf()),
        ("logs", paths.logs_dir()),
        ("persistent_instances", paths.persistent_instances_dir()),
        ("persistent_servers", paths.persistent_servers_dir()),
        ("temp", paths.temp_dir().to_path_buf()),
    ]
    .into_iter()
    .map(|(purpose, path)| probe_directory(purpose, path))
    .collect()
}

fn probe_directory(purpose: &'static str, path: PathBuf) -> DirectoryHealth {
    DirectoryHealth {
        purpose,
        state: probe_directory_state(&path),
        path,
    }
}

/// Decides what is at `path`.
///
/// Not `Path::is_dir`, which answers "no" both for a directory that is not
/// there and for one the process may not look at. Only `NotFound` means
/// absent; everything else is a fault worth naming, because a directory
/// Ephervex cannot inspect will not create itself on first use either.
/// Distinguishes a directory that is merely not there yet from one that cannot
/// be created because something above it is in the way.
///
/// [`DirectoryState::Absent`] promises the directory appears on first use. That
/// promise is false when the nearest existing ancestor is a regular file or a
/// broken link, and reporting it anyway sends the reader past the only thing
/// they have to fix.
fn absent_or_blocked_by_an_ancestor(path: &Path) -> DirectoryState {
    let mut ancestor = path.parent();
    while let Some(candidate) = ancestor {
        match std::fs::metadata(candidate) {
            Ok(metadata) if metadata.is_dir() => return DirectoryState::Absent,
            Ok(_) => {
                return DirectoryState::Unusable {
                    detail: format!("{} is not a directory", candidate.display()),
                };
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if std::fs::symlink_metadata(candidate).is_ok() {
                    return DirectoryState::Unusable {
                        detail: format!(
                            "{} is a symbolic link whose target does not exist",
                            candidate.display()
                        ),
                    };
                }
                ancestor = candidate.parent();
            }
            Err(error) => {
                return DirectoryState::Unusable {
                    detail: format!("{}: {error}", candidate.display()),
                };
            }
        }
    }
    DirectoryState::Absent
}

fn probe_directory_state(path: &Path) -> DirectoryState {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // `metadata` follows links, so a symbolic link whose target is
            // gone also lands here. That one is not absent: something is
            // there, and creating the directory will fail.
            return if std::fs::symlink_metadata(path).is_ok() {
                DirectoryState::Unusable {
                    detail: "a symbolic link whose target does not exist".to_owned(),
                }
            } else {
                absent_or_blocked_by_an_ancestor(path)
            };
        }
        Err(error) => {
            return DirectoryState::Unusable {
                detail: error.to_string(),
            };
        }
    };
    if !metadata.is_dir() {
        return DirectoryState::Unusable {
            detail: "not a directory".to_owned(),
        };
    }
    match write_probe(path) {
        Ok(()) => DirectoryState::Writable,
        Err(error) => DirectoryState::NotWritable {
            detail: error.to_string(),
        },
    }
}

/// Proves a directory is writable by writing in it, then removing what it
/// wrote.
///
/// The name is unique per probe so two Ephervex processes running this at once
/// cannot collide, and removal is attempted on every path out.
///
/// Only the write decides the answer. A sticky directory owned by somebody else
/// accepts the file and refuses the unlink, and reporting that as "not
/// writable" would describe a directory Ephervex can in fact write to. A
/// leftover probe is logged instead.
fn write_probe(directory: &Path) -> std::io::Result<()> {
    let probe = directory.join(format!(
        ".{}-write-probe-{}",
        ephervex_model::APP_ID,
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::File::create(&probe).map(drop)?;
    if let Err(error) = std::fs::remove_file(&probe) {
        if error.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(
                path = %probe.display(),
                %error,
                "could not remove a write probe; the file is left behind"
            );
        }
    }
    Ok(())
}

async fn probe_artifact_cache(app: &App) -> ArtifactCacheHealth {
    match app.artifact_cache_status().await {
        Ok(ArtifactCacheStatus {
            used_bytes,
            entries,
            complete,
            limit,
        }) => ArtifactCacheHealth::Measured {
            used_bytes,
            entries,
            complete,
            limit,
            over_budget: limit.is_exceeded_by(used_bytes),
        },
        Err(error) => ArtifactCacheHealth::Unavailable {
            detail: error.to_string(),
        },
    }
}

async fn probe_java(app: &App) -> JavaHealth {
    let inventory = match app.java_runtimes().await {
        Ok(inventory) => inventory,
        Err(error) => {
            return JavaHealth::Unavailable {
                detail: error.to_string(),
            };
        }
    };
    let mut major_versions: Vec<u32> = inventory
        .runtimes
        .iter()
        .map(|runtime| runtime.version.major)
        .collect();
    major_versions.sort_unstable();
    major_versions.dedup();
    JavaHealth::Discovered {
        discovered: inventory.runtimes.len(),
        managed: inventory
            .runtimes
            .iter()
            .filter(|runtime| runtime.is_managed())
            .count(),
        major_versions,
    }
}
