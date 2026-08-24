//! Mod loader identity and version selection.

use std::fmt;

use serde::{Deserialize, Serialize};

/// The mod loaders supported by the first complete version.
///
/// Fabric, Quilt, LiteLoader, and Rift are deliberately absent. Adding one is a
/// new `LoaderProvider` implementation in `ephervex-loaders` plus a variant
/// here — never a rewrite of the launcher core.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoaderKind {
    Vanilla,
    Forge,
    NeoForge,
}

impl LoaderKind {
    /// Whether this loader needs a loader version alongside the Minecraft
    /// version. Vanilla does not, so the UI hides its version controls
    #[must_use]
    pub const fn requires_version(self) -> bool {
        !matches!(self, Self::Vanilla)
    }

    /// Whether this loader can host mods.
    #[must_use]
    pub const fn supports_mods(self) -> bool {
        !matches!(self, Self::Vanilla)
    }
}

impl fmt::Display for LoaderKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Loader names are product names and stay untranslated.
        f.write_str(match self {
            Self::Vanilla => "Vanilla",
            Self::Forge => "Forge",
            Self::NeoForge => "NeoForge",
        })
    }
}

/// How a loader version was chosen.
///
/// `Stable` and `Latest` are never silently substituted for one another. If a
/// selected Minecraft version has no stable build, that is reported to the user
/// rather than quietly resolved to the latest build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoaderVersionMode {
    /// Newest available build, including beta/prerelease where the provider
    /// publishes them.
    Latest,
    /// Newest build the provider marks recommended/non-prerelease.
    Stable,
    /// A specific build. Always used for imported packs, which must reproduce
    /// the exact declared version.
    Exact,
}

/// A resolved loader choice for an instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoaderSelection {
    pub kind: LoaderKind,
    /// `None` only for [`LoaderKind::Vanilla`].
    pub version: Option<String>,
}

impl LoaderSelection {
    /// Vanilla, which carries no loader version.
    #[must_use]
    pub const fn vanilla() -> Self {
        Self {
            kind: LoaderKind::Vanilla,
            version: None,
        }
    }

    /// A modded selection pinned to an exact build.
    #[must_use]
    pub fn exact(kind: LoaderKind, version: impl Into<String>) -> Self {
        Self {
            kind,
            version: Some(version.into()),
        }
    }

    /// Whether this selection is internally consistent: a loader that requires
    /// a version has one, and Vanilla does not carry a stray version.
    #[must_use]
    pub fn is_coherent(&self) -> bool {
        self.kind.requires_version() == self.version.is_some()
    }
}

impl fmt::Display for LoaderSelection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.version {
            Some(version) => write!(f, "{} {version}", self.kind),
            None => write!(f, "{}", self.kind),
        }
    }
}

/// The default name suggested for a new instance of this target.
///
/// A distinct, dash-joined convention from [`LoaderSelection`]'s
/// space-joined `Display`: this one names a directory-friendly default, not
/// a sentence. Vanilla carries no loader segment. A modded loader whose
/// version has not loaded yet omits the version segment rather than
/// rendering a placeholder, since the caller recomputes this as soon as the
/// version becomes known.
#[must_use]
pub fn default_instance_name(
    minecraft: &str,
    loader: LoaderKind,
    loader_version: Option<&str>,
) -> String {
    if loader == LoaderKind::Vanilla {
        return minecraft.to_owned();
    }
    match loader_version
        .map(str::trim)
        .filter(|version| !version.is_empty())
    {
        Some(version) => format!("{minecraft}-{loader}-{version}"),
        None => format!("{minecraft}-{loader}"),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn vanilla_needs_no_loader_version() {
        assert!(!LoaderKind::Vanilla.requires_version());
        assert!(LoaderSelection::vanilla().is_coherent());
    }

    #[test]
    fn modded_selections_require_a_version() {
        assert!(LoaderSelection::exact(LoaderKind::Forge, "14.23.5.2859").is_coherent());
        assert!(
            !LoaderSelection {
                kind: LoaderKind::Forge,
                version: None
            }
            .is_coherent()
        );
    }

    #[test]
    fn vanilla_carrying_a_version_is_incoherent() {
        let bogus = LoaderSelection {
            kind: LoaderKind::Vanilla,
            version: Some("1.2.3".to_owned()),
        };
        assert!(!bogus.is_coherent());
    }

    #[test]
    fn loader_names_render_as_product_names() {
        assert_eq!(LoaderKind::NeoForge.to_string(), "NeoForge");
        assert_eq!(
            LoaderSelection::exact(LoaderKind::Forge, "47.4.0").to_string(),
            "Forge 47.4.0"
        );
    }

    #[test]
    fn default_instance_name_uses_only_the_release_for_vanilla() {
        assert_eq!(
            default_instance_name("1.20.1", LoaderKind::Vanilla, None),
            "1.20.1"
        );
        assert_eq!(
            default_instance_name("1.20.1", LoaderKind::Vanilla, Some("47.4.0")),
            "1.20.1"
        );
    }

    #[test]
    fn default_instance_name_joins_loader_and_version_with_dashes() {
        assert_eq!(
            default_instance_name("1.20.1", LoaderKind::Forge, Some("47.4.0")),
            "1.20.1-Forge-47.4.0"
        );
        assert_eq!(
            default_instance_name("1.21.1", LoaderKind::NeoForge, Some("21.1.100")),
            "1.21.1-NeoForge-21.1.100"
        );
    }

    #[test]
    fn default_instance_name_omits_a_not_yet_loaded_loader_version() {
        assert_eq!(
            default_instance_name("1.20.1", LoaderKind::Forge, None),
            "1.20.1-Forge"
        );
        assert_eq!(
            default_instance_name("1.20.1", LoaderKind::Forge, Some("")),
            "1.20.1-Forge"
        );
    }
}
