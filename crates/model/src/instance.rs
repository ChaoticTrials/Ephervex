//! Instance kinds and where an instance came from.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Client or server. Both are first-class entities with their own directory,
/// process, logs, and JVM settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstanceKind {
    Client,
    Server,
}

/// The import format an instance originated from.
///
/// Provider-specific detail belongs in provider-specific structures, not here.
/// This enum only answers "which importer produced this".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportSourceKind {
    /// Created directly in the launcher rather than imported.
    Manual,
    ModrinthPack,
    CurseForgePack,
    PrismExport,
}

impl fmt::Display for ImportSourceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Manual => "Manual",
            Self::ModrinthPack => "Modrinth",
            Self::CurseForgePack => "CurseForge",
            Self::PrismExport => "Prism",
        })
    }
}

/// Why an instance needs the user's attention.
///
/// The launcher never pretends an instance is faithfully runnable when it is
/// not. Unsupported Prism customizations, missing persistent roots, and
/// best-effort server conversions all surface here rather than being silently
/// approximated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "detail")]
pub enum AttentionReason {
    /// The source archive used a loader this launcher does not support.
    UnsupportedLoader(String),
    /// The source used launch customizations that cannot be reproduced safely.
    UnsupportedLaunchCustomization(String),
    /// An imported hook or command was found and deliberately not executed.
    IgnoredUnsafeCustomization(String),
    /// A persistent instance root is missing but its metadata was kept.
    MissingRoot,
    /// A server was generated best-effort and may contain client-only mods.
    BestEffortServerConversion,
    /// Files could not be fetched automatically and need manual download.
    ManualDownloadRequired(String),
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn attention_reasons_round_trip_through_json() {
        let reason = AttentionReason::IgnoredUnsafeCustomization("pre-launch command".to_owned());
        let json = serde_json::to_string(&reason).expect("serialize");
        let back: AttentionReason = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(reason, back);
    }

    #[test]
    fn source_kinds_render_as_provider_names() {
        assert_eq!(ImportSourceKind::ModrinthPack.to_string(), "Modrinth");
    }
}
