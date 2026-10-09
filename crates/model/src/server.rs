//! Provider-neutral dedicated-server installation and connection values.

use std::net::{IpAddr, SocketAddr};
use std::num::NonZeroU16;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize};

/// A Minecraft operator permission level.
///
/// The levels Ephervex configures and writes are the bounded integer domain 1
/// through 4. Keeping that range in the type prevents configuration handling
/// from disagreeing about whether an arbitrary integer is valid. Existing
/// `ops.json` entries are not limited to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct OperatorPermissionLevel(u8);

impl OperatorPermissionLevel {
    pub const MINIMUM: Self = Self(1);
    pub const MAXIMUM: Self = Self(4);
    pub const ALL: [Self; 4] = [Self(1), Self(2), Self(3), Self(4)];

    /// Validates a Minecraft operator permission level.
    pub const fn new(value: u8) -> Result<Self, InvalidOperatorPermissionLevel> {
        if value >= Self::MINIMUM.0 && value <= Self::MAXIMUM.0 {
            Ok(Self(value))
        } else {
            Err(InvalidOperatorPermissionLevel { value })
        }
    }

    /// Resolves a raw Minecraft permission value when it is in range.
    #[must_use]
    pub const fn from_value(value: u8) -> Option<Self> {
        if value >= Self::MINIMUM.0 && value <= Self::MAXIMUM.0 {
            Some(Self(value))
        } else {
            None
        }
    }

    #[must_use]
    pub const fn value(self) -> u8 {
        self.0
    }

    #[must_use]
    pub const fn get(self) -> u8 {
        self.value()
    }
}

impl Default for OperatorPermissionLevel {
    fn default() -> Self {
        Self::MAXIMUM
    }
}

impl TryFrom<u8> for OperatorPermissionLevel {
    type Error = InvalidOperatorPermissionLevel;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<OperatorPermissionLevel> for u8 {
    fn from(value: OperatorPermissionLevel) -> Self {
        value.get()
    }
}

impl<'de> Deserialize<'de> for OperatorPermissionLevel {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = u8::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

impl std::fmt::Display for OperatorPermissionLevel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A value outside Minecraft's operator permission level domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("operator permission level {value} is outside the valid range 1 through 4")]
pub struct InvalidOperatorPermissionLevel {
    pub value: u8,
}

/// The validated destination for a direct local-server connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ConnectionTarget {
    pub address: IpAddr,
    pub port: NonZeroU16,
}

impl ConnectionTarget {
    #[must_use]
    pub const fn socket_address(self) -> SocketAddr {
        SocketAddr::new(self.address, self.port.get())
    }
}

impl std::fmt::Display for ConnectionTarget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.socket_address().fmt(formatter)
    }
}

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
    fn ipv6_connection_targets_render_without_host_port_ambiguity() {
        let target = ConnectionTarget {
            address: "::1".parse().expect("address"),
            port: NonZeroU16::new(25_565).expect("port"),
        };
        assert_eq!(target.to_string(), "[::1]:25565");
    }

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

    #[test]
    fn operator_permission_levels_accept_exactly_the_minecraft_domain() {
        assert!(OperatorPermissionLevel::new(0).is_err());
        assert!(OperatorPermissionLevel::from_value(0).is_none());
        assert_eq!(
            OperatorPermissionLevel::ALL.map(OperatorPermissionLevel::value),
            [1, 2, 3, 4]
        );
        assert!(OperatorPermissionLevel::new(5).is_err());
        assert!(OperatorPermissionLevel::from_value(5).is_none());
        assert_eq!(
            OperatorPermissionLevel::from_value(4),
            Some(OperatorPermissionLevel::default())
        );
        assert_eq!(OperatorPermissionLevel::default().value(), 4);
    }

    #[test]
    fn operator_permission_levels_serialize_as_bounded_integers() {
        let level = OperatorPermissionLevel::new(3).expect("level");
        assert_eq!(serde_json::to_string(&level).expect("serialize"), "3");
        assert_eq!(
            serde_json::from_str::<OperatorPermissionLevel>("3").expect("deserialize"),
            level
        );
        assert!(serde_json::from_str::<OperatorPermissionLevel>("0").is_err());
        assert!(serde_json::from_str::<OperatorPermissionLevel>("5").is_err());
    }
}
