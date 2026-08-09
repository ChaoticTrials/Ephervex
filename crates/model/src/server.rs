//! Provider-neutral dedicated-server installation values.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize};

/// A path beneath a server's game directory.
///
/// Launch descriptors are durable metadata and may be imported from an
/// untrusted directory. Keeping the validation in the type prevents a caller
/// from accidentally launching a JAR outside the server root.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct ServerRelativePath(PathBuf);

impl ServerRelativePath {
    /// Validates and stores a non-empty relative path without traversal.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, InvalidServerRelativePath> {
        let path = path.into();
        if path.as_os_str().is_empty() || path.is_absolute() {
            return Err(InvalidServerRelativePath { path });
        }
        if path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        }) {
            return Err(InvalidServerRelativePath { path });
        }
        Ok(Self(path))
    }

    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    /// Resolves this path beneath `root`.
    #[must_use]
    pub fn resolve(&self, root: &Path) -> PathBuf {
        root.join(&self.0)
    }
}

impl<'de> Deserialize<'de> for ServerRelativePath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let path = PathBuf::deserialize(deserializer)?;
        Self::new(path).map_err(serde::de::Error::custom)
    }
}

impl std::fmt::Display for ServerRelativePath {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.display().fmt(formatter)
    }
}

/// Why a persisted server launch path is unusable.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("server launch path `{path}` must be relative and must not contain parent traversal")]
pub struct InvalidServerRelativePath {
    pub path: PathBuf,
}

/// The installed server entry point.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ServerLaunch {
    /// Vanilla and legacy Forge launch a JAR.
    Jar { path: ServerRelativePath },
    /// Modern Forge and NeoForge launch an argument file.
    ArgFile { path: ServerRelativePath },
}

impl ServerLaunch {
    #[must_use]
    pub const fn path(&self) -> &ServerRelativePath {
        match self {
            Self::Jar { path } | Self::ArgFile { path } => path,
        }
    }
}

/// Whether a server has a complete, launchable base installation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum ServerInstallation {
    #[default]
    NotInstalled,
    Installed {
        launch: ServerLaunch,
    },
}

impl ServerInstallation {
    #[must_use]
    pub const fn is_installed(&self) -> bool {
        matches!(self, Self::Installed { .. })
    }

    #[must_use]
    pub const fn launch(&self) -> Option<&ServerLaunch> {
        match self {
            Self::NotInstalled => None,
            Self::Installed { launch } => Some(launch),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn server_launch_paths_reject_absolute_paths_and_traversal() {
        assert!(ServerRelativePath::new("server.jar").is_ok());
        assert!(ServerRelativePath::new("libraries/start.args").is_ok());
        assert!(ServerRelativePath::new("/tmp/server.jar").is_err());
        assert!(ServerRelativePath::new("../server.jar").is_err());
        assert!(ServerRelativePath::new("").is_err());
    }

    #[test]
    fn unsafe_paths_are_rejected_during_metadata_deserialization() {
        let error =
            serde_json::from_str::<ServerLaunch>(r#"{"kind":"jar","path":"../../outside.jar"}"#)
                .expect_err("traversal must not deserialize");
        assert!(error.to_string().contains("must be relative"));
    }
}
