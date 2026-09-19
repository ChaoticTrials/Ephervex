//! Instance kinds and where an instance came from.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, de};

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
    /// Retained for metadata compatibility. Current imports reject this format.
    PrismExport,
    /// A dedicated-server archive published alongside a pack, installed as the
    /// complete content of a server. It names no catalog because the archive is
    /// the authority: whoever published it decided what the server contains.
    ServerPack,
}

impl fmt::Display for ImportSourceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Manual => "Manual",
            Self::ModrinthPack => "Modrinth",
            Self::CurseForgePack => "CurseForge",
            Self::PrismExport => "Prism",
            Self::ServerPack => "Server pack",
        })
    }
}

/// Why an instance needs the user's attention.
///
/// The launcher never pretends an instance is faithfully runnable when it is
/// not. Unsupported imported customizations, missing persistent roots, and
/// best-effort server conversions all surface here rather than being silently
/// approximated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
    /// A managed file had no declared server applicability and was omitted
    /// until the user explicitly confirms it for a server creation.
    UnknownServerApplicability(String),
    /// Launcher-owned staging or rollback state survived an interrupted
    /// operation and must be reviewed before further mutation.
    InterruptedMutation,
    /// An advisory reason written by a newer Ephervex.
    ///
    /// Attention is informational state. An unfamiliar reason must keep the
    /// record readable instead of making the whole instance disappear.
    Unknown,
}

#[derive(Deserialize)]
struct StoredAttentionReason {
    kind: String,
    #[serde(default)]
    detail: Option<StoredAttentionDetail>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum StoredAttentionDetail {
    Text(String),
    Other(de::IgnoredAny),
}

impl<'de> Deserialize<'de> for AttentionReason {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let stored = StoredAttentionReason::deserialize(deserializer)?;
        let text = |detail: Option<StoredAttentionDetail>| match detail {
            Some(StoredAttentionDetail::Text(value)) => Ok(value),
            Some(StoredAttentionDetail::Other(_)) | None => {
                Err(de::Error::custom("attention detail must be text"))
            }
        };
        let unit = |detail: Option<StoredAttentionDetail>, reason| {
            if detail.is_some() {
                Err(de::Error::custom("this attention reason has no detail"))
            } else {
                Ok(reason)
            }
        };

        match stored.kind.as_str() {
            "unsupported_loader" => text(stored.detail).map(Self::UnsupportedLoader),
            "unsupported_launch_customization" => {
                text(stored.detail).map(Self::UnsupportedLaunchCustomization)
            }
            "ignored_unsafe_customization" => {
                text(stored.detail).map(Self::IgnoredUnsafeCustomization)
            }
            "missing_root" => unit(stored.detail, Self::MissingRoot),
            "best_effort_server_conversion" => {
                unit(stored.detail, Self::BestEffortServerConversion)
            }
            "manual_download_required" => text(stored.detail).map(Self::ManualDownloadRequired),
            "unknown_server_applicability" => {
                text(stored.detail).map(Self::UnknownServerApplicability)
            }
            "interrupted_mutation" => unit(stored.detail, Self::InterruptedMutation),
            "unknown" => unit(stored.detail, Self::Unknown),
            _ => Ok(Self::Unknown),
        }
    }
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
    fn an_unknown_attention_reason_keeps_the_record_readable() {
        let reason: AttentionReason = serde_json::from_str(
            r#"{"kind":"future_recovery_reason","detail":{"path":"mods/future.jar"}}"#,
        )
        .expect("unknown advisory state stays readable");

        assert_eq!(reason, AttentionReason::Unknown);
    }

    #[test]
    fn source_kinds_render_as_provider_names() {
        assert_eq!(ImportSourceKind::ModrinthPack.to_string(), "Modrinth");
    }
}
