//! The Mojang version manifest and the release filter.
//!
//! `version_manifest_v2.json` is the index of every version Mojang has ever
//! published — 905 entries at the time of writing, of which 102 are releases.
//! Each entry carries the URL of that version's own JSON and the SHA-1 of that
//! document, which is what lets a per-version fetch be verified without
//! trusting anything but this one file.
//!
//! # The filter, and why it is only two rules
//!
//! ```text
//! type == "release"   AND   version >= 1.12.2
//! ```
//!
//! No filename patterns. Ephervex is explicit about that, and the reason is
//! load-bearing rather than stylistic: a regex written against `1.x.y` would
//! have quietly stopped matching when Mojang moved to calendar versions like
//! `26.1`, and the launcher would have shown an empty version list with no
//! error to explain it.
//!
//! Nor is there an exclusion rule for release candidates or pre-releases. It
//! would be dead code: Mojang types `26.2-rc-1` and `1.20.2-pre1` as
//! `"snapshot"`, not as channels of their own, so the `type` test already
//! removes them. This was re-verified against the live manifest on 2026-08-07
//! and is pinned by `fixtures/minecraft/version_manifest_v2.json`, which keeps
//! one of each specifically so the assumption fails loudly if it ever changes.
//!
//! # Deciding the floor for an id that will not parse
//!
//! [`MinecraftVersionId`] cannot rank an id in a scheme it has never seen, and
//! Ephervex requires selection should follow Mojang's own ordering and release dates
//! rather than arithmetic on version strings. So when the id is unreadable, the
//! floor is decided by comparing `releaseTime` against the `releaseTime` of the
//! 1.12.2 entry **in the same manifest** — no hardcoded date, no guess. A
//! future scheme therefore appears in the list on the strength of being newer
//! than 1.12.2, which is exactly the claim the manifest is making.

use chrono::{DateTime, Utc};
use ephervex_model::{Hash, HashAlgorithm};
use serde::Deserialize;
use tracing::{debug, warn};

use crate::version::ReleaseType;
use crate::version_id::{MINIMUM_SUPPORTED_VERSION, MinecraftVersionId};

/// The canonical manifest URL (re-verified 2026-08-07).
pub const VERSION_MANIFEST_URL: &str =
    "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";

/// The [`ephervex_download::MetadataKey`] the manifest is cached under.
pub const VERSION_MANIFEST_CACHE_KEY: &str = "mojang-version-manifest";

/// What Mojang currently considers the newest of each channel.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LatestVersions {
    pub release: String,
    pub snapshot: String,
}

/// One entry of the manifest's `versions` array.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionRecord {
    /// The id as published.
    pub id: String,
    /// The same id, parsed. May be [`MinecraftVersionId::Unrecognized`].
    pub version: MinecraftVersionId,
    pub release_type: ReleaseType,
    /// Where that version's own JSON lives.
    pub url: String,
    /// SHA-1 of the document at `url`. Verified on download.
    pub sha1: Hash,
    /// When Mojang released it. The ordering authority.
    pub release_time: DateTime<Utc>,
    /// When the document was last touched. Not a release date.
    pub time: DateTime<Utc>,
    /// `1` once Mojang considers the version's log4j configuration patched.
    pub compliance_level: u32,
}

impl VersionRecord {
    /// Whether the id is in a scheme this build can rank.
    ///
    /// A `false` is worth surfacing: the version is still offered, but on the
    /// strength of its release date rather than its number.
    #[must_use]
    pub const fn has_recognized_version_id(&self) -> bool {
        self.version.is_recognized()
    }
}

/// The parsed manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionManifest {
    pub latest: LatestVersions,
    /// Every entry, in Mojang's own order — newest first.
    pub versions: Vec<VersionRecord>,
}

/// Why an entry was or was not offered, for logs and for a caller that wants to
/// explain a list to a user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupportBasis {
    /// The parsed id is at or above the floor.
    VersionId,
    /// The id could not be parsed, so the manifest's own release date decided
    /// it.
    ReleaseTime,
}

/// Failures reading a manifest document.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ManifestError {
    #[error("not a well-formed version manifest")]
    Json(#[from] serde_json::Error),
    /// An entry declares a digest that is not a well-formed SHA-1. Fatal
    /// rather than skipped: the digest is the only thing standing between the
    /// launcher and an unverified version document.
    #[error("manifest entry `{id}` declares a malformed sha1")]
    MalformedDigest { id: String },
}

impl VersionManifest {
    /// Parses a manifest document.
    pub fn parse(json: &str) -> Result<Self, ManifestError> {
        let raw: RawManifest = serde_json::from_str(json)?;
        let versions = raw
            .versions
            .into_iter()
            .map(RawVersionRecord::into_record)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            latest: raw.latest,
            versions,
        })
    }

    /// One entry by its published id.
    #[must_use]
    pub fn version(&self, id: &str) -> Option<&VersionRecord> {
        self.versions.iter().find(|record| record.id == id)
    }

    /// The releases Ephervex offers, in Mojang's own newest-first order
    ///
    /// Ordering is *not* recomputed from the parsed ids. The manifest's order
    /// is the authority, and preserving it means the list stays right even for
    /// versions whose ids this build cannot rank.
    #[must_use]
    pub fn supported_releases(&self) -> Vec<&VersionRecord> {
        let floor = MinecraftVersionId::minimum_supported();
        let floor_released_at = self
            .version(MINIMUM_SUPPORTED_VERSION)
            .map(|record| record.release_time);

        self.versions
            .iter()
            .filter(|record| record.release_type.is_release())
            .filter(|record| supports(record, &floor, floor_released_at).is_some())
            .collect()
    }

    /// Why an entry is offered, or `None` if it is not.
    ///
    /// Separate from [`Self::supported_releases`] so a caller — or a test — can
    /// ask *how* a version qualified without re-deriving the rule.
    #[must_use]
    pub fn support_basis(&self, record: &VersionRecord) -> Option<SupportBasis> {
        if !record.release_type.is_release() {
            return None;
        }
        let floor = MinecraftVersionId::minimum_supported();
        let floor_released_at = self
            .version(MINIMUM_SUPPORTED_VERSION)
            .map(|entry| entry.release_time);
        supports(record, &floor, floor_released_at)
    }
}

/// Decides whether one release entry is inside the supported range.
///
/// A free function rather than a method: it reads nothing from the manifest
/// beyond the two values already passed in, and computing those once per list
/// is the difference between one lookup and one per entry.
fn supports(
    record: &VersionRecord,
    floor: &MinecraftVersionId,
    floor_released_at: Option<DateTime<Utc>>,
) -> Option<SupportBasis> {
    if let Some(decided) = record.version.at_least(floor) {
        return decided.then_some(SupportBasis::VersionId);
    }

    // The id is in a scheme this build cannot rank. Fall back to what the
    // manifest itself asserts  rather than dropping the entry,
    // which would hide a new release behind an outdated parser.
    match floor_released_at {
        Some(cutoff) if record.release_time >= cutoff => {
            warn!(
                version = %record.id,
                "release id is in an unknown scheme; offering it on the strength of its release date"
            );
            Some(SupportBasis::ReleaseTime)
        }
        Some(_) => {
            debug!(version = %record.id, "release predates the supported floor");
            None
        }
        None => {
            // No 1.12.2 entry to compare against — a manifest shape nobody
            // has seen. Refusing is the honest answer: offering a version
            // whose age is unknown could put a user on an unsupported
            // release.
            warn!(
                version = %record.id,
                "cannot place this release relative to {MINIMUM_SUPPORTED_VERSION}; leaving it out"
            );
            None
        }
    }
}

// ------------------------------------------------------------------ the wire

#[derive(Debug, Deserialize)]
struct RawManifest {
    latest: LatestVersions,
    versions: Vec<RawVersionRecord>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawVersionRecord {
    id: String,
    #[serde(rename = "type")]
    release_type: ReleaseType,
    url: String,
    sha1: String,
    release_time: DateTime<Utc>,
    time: DateTime<Utc>,
    #[serde(default)]
    compliance_level: u32,
}

impl RawVersionRecord {
    fn into_record(self) -> Result<VersionRecord, ManifestError> {
        let sha1 = Hash::new(HashAlgorithm::Sha1, &self.sha1).ok_or_else(|| {
            ManifestError::MalformedDigest {
                id: self.id.clone(),
            }
        })?;
        Ok(VersionRecord {
            version: MinecraftVersionId::parse(&self.id),
            id: self.id,
            release_type: self.release_type,
            url: self.url,
            sha1,
            release_time: self.release_time,
            time: self.time,
            compliance_level: self.compliance_level,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::test_support::fixture;

    fn manifest() -> VersionManifest {
        VersionManifest::parse(fixture("version_manifest_v2.json")).expect("fixture parses")
    }

    fn supported_ids(manifest: &VersionManifest) -> Vec<&str> {
        manifest
            .supported_releases()
            .iter()
            .map(|record| record.id.as_str())
            .collect()
    }

    #[test]
    fn only_releases_at_or_above_the_floor_are_offered() {
        assert_eq!(
            supported_ids(&manifest()),
            ["26.2", "26.1", "1.21.1", "1.20.1", "1.12.2"]
        );
    }

    #[test]
    fn mojangs_own_newest_first_order_is_preserved_rather_than_recomputed() {
        let manifest = manifest();
        let offered = supported_ids(&manifest);
        let published: Vec<&str> = manifest
            .versions
            .iter()
            .filter(|record| offered.contains(&record.id.as_str()))
            .map(|record| record.id.as_str())
            .collect();
        assert_eq!(offered, published);
    }

    #[test]
    fn release_candidates_and_pre_releases_need_no_rule_of_their_own() {
        let manifest = manifest();
        for id in ["26.2-rc-1", "1.20.2-pre1", "26.3-snapshot-7"] {
            let record = manifest.version(id).expect("fixture entry");
            assert_eq!(
                record.release_type,
                ReleaseType::Snapshot,
                "`{id}` must be typed snapshot; the single type filter depends on it"
            );
            assert!(manifest.support_basis(record).is_none());
        }
    }

    #[test]
    fn everything_below_the_floor_is_excluded_including_the_old_channels() {
        let manifest = manifest();
        for id in ["1.12.1", "1.12", "1.11.2", "1.7.10", "b1.7.3", "rd-132211"] {
            let record = manifest.version(id).expect("fixture entry");
            assert!(
                manifest.support_basis(record).is_none(),
                "`{id}` should not be offered"
            );
        }
    }

    #[test]
    fn the_floor_itself_is_included() {
        let manifest = manifest();
        let record = manifest.version("1.12.2").expect("fixture entry");
        assert_eq!(
            manifest.support_basis(record),
            Some(SupportBasis::VersionId),
            "1.12.2 is the minimum, inclusive"
        );
    }

    #[test]
    fn a_release_in_a_scheme_this_build_cannot_parse_is_offered_on_its_release_date() {
        // Not captured from Mojang: this is the case the parser has not met
        // yet, constructed so the fallback is exercised before it is needed.
        let manifest = VersionManifest::parse(
            r#"{
              "latest": {"release": "2030w01", "snapshot": "2030w01"},
              "versions": [
                {"id":"2030w01","type":"release","url":"https://piston-meta.mojang.com/v1/packages/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/x.json","sha1":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","time":"2030-01-02T00:00:00+00:00","releaseTime":"2030-01-01T00:00:00+00:00","complianceLevel":1},
                {"id":"1.12.2","type":"release","url":"https://piston-meta.mojang.com/v1/packages/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb/1.12.2.json","sha1":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","time":"2017-09-18T08:39:46+00:00","releaseTime":"2017-09-18T08:39:46+00:00","complianceLevel":0},
                {"id":"ancient-scheme","type":"release","url":"https://piston-meta.mojang.com/v1/packages/cccccccccccccccccccccccccccccccccccccccc/y.json","sha1":"cccccccccccccccccccccccccccccccccccccccc","time":"2010-01-02T00:00:00+00:00","releaseTime":"2010-01-01T00:00:00+00:00","complianceLevel":0}
              ]
            }"#,
        )
.expect("parses");

        let future = manifest.version("2030w01").expect("entry");
        assert!(!future.has_recognized_version_id());
        assert_eq!(
            manifest.support_basis(future),
            Some(SupportBasis::ReleaseTime),
            "an unrankable id must not hide a release that the manifest says is new"
        );

        let ancient = manifest.version("ancient-scheme").expect("entry");
        assert_eq!(manifest.support_basis(ancient), None);

        assert_eq!(supported_ids(&manifest), ["2030w01", "1.12.2"]);
    }

    #[test]
    fn a_malformed_digest_fails_the_manifest_rather_than_being_skipped() {
        let error = VersionManifest::parse(
            r#"{"latest":{"release":"1.20.1","snapshot":"1.20.1"},"versions":[
              {"id":"1.20.1","type":"release","url":"https://x.invalid/a.json","sha1":"not-a-digest","time":"2023-06-12T13:25:51+00:00","releaseTime":"2023-06-12T13:25:51+00:00","complianceLevel":1}
            ]}"#,
        )
.expect_err("a bad digest must not be silently dropped");
        assert!(matches!(error, ManifestError::MalformedDigest { id } if id == "1.20.1"));
    }

    #[test]
    fn the_manifest_records_which_version_mojang_calls_latest() {
        let manifest = manifest();
        assert_eq!(manifest.latest.release, "26.2");
        assert_eq!(manifest.latest.snapshot, "26.3-snapshot-7");
        assert!(
            manifest.version(&manifest.latest.release).is_some(),
            "the latest release must be an entry, not a dangling name"
        );
    }
}
