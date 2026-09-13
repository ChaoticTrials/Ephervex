//! Minecraft version identity and ordering.
//!
//! # Why this is not `semver`
//!
//! Minecraft version strings have never been SemVer and, as of the 26.x
//! releases, are not even the historical `1.major.minor` scheme any more. A
//! SemVer parser rejects `26.1` outright - it has two components, not three -
//! and reading `1.21.11` as SemVer implies precedence rules (pre-release
//! ordering, build metadata, caret compatibility) that Mojang has never
//! promised. The workspace declares `semver` "only where a real semantic
//! version is appropriate"; this is not one of those places.
//!
//! # Why a parser exists at all, given that ordering comes from Mojang
//!
//! Ephervex is explicit that *selection* should follow the manifest's own ordering
//! and release dates rather than arithmetic on version strings. This type is
//! therefore not the authority on "which version is newest". It exists to
//! answer two narrower questions:
//!
//! 1. Is this at or above the 1.12.2 support floor?
//! 2. How should a list of versions be ordered for display when a caller is
//!    not iterating the manifest in its published order?
//!
//! # Minecraft releases only
//!
//! Both questions are about Minecraft releases, so a Minecraft release id is
//! the only thing that belongs here. Loader ids look close enough to invite the
//! mistake and are not the same thing: `neoforge-26.2.0.59` is NeoForge build
//! 59 for Minecraft 26.2, not a Minecraft version with a fourth component, and
//! `1.20.1-forge-47.4.10` is two identifiers joined by a hyphen. A caller
//! holding a loader-derived version-manifest id has to reach the Minecraft
//! release it extends - [`crate::VersionMetadata::minecraft_version`] carries
//! it - rather than hand the loader id to this parser.
//!
//! # Degrading on an id it cannot parse
//!
//! Mojang has changed the version scheme once already, so this parser must
//! assume it will meet a form it does not know. An unparseable id becomes
//! [`MinecraftVersionId::Unrecognized`], which:
//!
//! * is never an error and never a panic - the id is kept verbatim and still
//!   displays and round-trips;
//! * orders **above** every id that did parse, on the reasoning that a scheme
//!   this code has not been taught is far more likely to be a future one than
//!   an ancient one;
//! * is reported by [`Self::is_recognized`] so callers can log it and show it
//!   as unusual, rather than dropping it where nobody would notice.
//!
//! The support filter never decides the floor from an unrecognized id - it
//! falls back to the manifest's `releaseTime`. See
//! [`crate::manifest::VersionManifest::supported_releases`].

use std::cmp::Ordering;
use std::fmt;

use serde::{Deserialize, Serialize};

/// The oldest Minecraft release Ephervex supports.
///
/// The floor is a product decision, not a technical one: 1.12.2 is where the
/// mod-development ecosystem this launcher serves still has active bug reports.
pub const MINIMUM_SUPPORTED_VERSION: &str = "1.12.2";

/// A parsed Minecraft version id.
///
/// Both released schemes are dotted runs of non-negative integers - `1.12.2`,
/// `1.21`, `26.1` - so one representation covers them, and the calendar scheme
/// happens to order correctly against the old one for free (`26` > `1`).
///
/// `PartialEq`, `Hash`, and `Ord` are implemented by hand rather than derived,
/// because the derived versions would compare the *text*: `1.21` and `1.21.0`
/// are the same version but not the same string, and an `Ord` that called them
/// equal while `PartialEq` called them different would break the contract
/// between the two.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum MinecraftVersionId {
    /// A dotted numeric id, kept alongside the text it was parsed from so that
    /// `1.21` and `1.21.0` display as they were published.
    Numbered { components: Vec<u32>, raw: String },
    /// An id in a form this build does not understand. Never dropped, never
    /// fatal.
    Unrecognized(String),
}

impl MinecraftVersionId {
    /// Parses an id. Infallible by design: see the module docs.
    #[must_use]
    pub fn parse(id: &str) -> Self {
        let raw = id.trim();
        let recognized =
            !raw.is_empty() && !raw.starts_with('.') && !raw.ends_with('.') && !raw.contains("..");
        if !recognized {
            return Self::Unrecognized(raw.to_owned());
        }

        let mut components = Vec::new();
        for part in raw.split('.') {
            // Rejecting non-ASCII-digit runs also rejects `1.12.2-pre1`,
            // `1.RV-Pre1`, `b1.7.3`, and `+1` - all real ids Mojang has
            // published, none of which are releases.
            if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
                return Self::Unrecognized(raw.to_owned());
            }
            match part.parse::<u32>() {
                Ok(value) => components.push(value),
                Err(_) => return Self::Unrecognized(raw.to_owned()),
            }
        }

        Self::Numbered {
            components,
            raw: raw.to_owned(),
        }
    }

    /// The lowest release Ephervex supports, as a parsed id.
    #[must_use]
    pub fn minimum_supported() -> Self {
        Self::parse(MINIMUM_SUPPORTED_VERSION)
    }

    /// Whether the id parsed into a form this build can reason about.
    ///
    /// A `false` here is a signal to *say something* - log it, mark it in the
    /// UI - not a signal to hide the version.
    #[must_use]
    pub const fn is_recognized(&self) -> bool {
        matches!(self, Self::Numbered { .. })
    }

    /// The id exactly as Mojang published it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Numbered { raw, .. } | Self::Unrecognized(raw) => raw,
        }
    }

    /// The numeric components, or `None` for an id that did not parse.
    #[must_use]
    pub fn components(&self) -> Option<&[u32]> {
        match self {
            Self::Numbered { components, .. } => Some(components),
            Self::Unrecognized(_) => None,
        }
    }

    /// Whether this id is at or above the support floor, when that can be
    /// decided from the id alone.
    ///
    /// `None` means "ask the manifest instead": either id is in a scheme this
    /// build does not parse, and guessing from an unknown scheme is precisely
    /// guessing how an unknown version scheme compares to a known one.
    #[must_use]
    pub fn at_least(&self, floor: &Self) -> Option<bool> {
        let (Self::Numbered { .. }, Self::Numbered { .. }) = (self, floor) else {
            return None;
        };
        Some(self >= floor)
    }

    /// Whether this id is at or above [`MINIMUM_SUPPORTED_VERSION`], when that
    /// can be decided from the id alone.
    #[must_use]
    pub fn is_supported(&self) -> Option<bool> {
        self.at_least(&Self::minimum_supported())
    }
}

/// Compares two component runs, treating an absent component as zero.
///
/// `1.21` and `1.21.0` therefore compare equal - Mojang publishes one or the
/// other, never both - while `1.21` still sorts below `1.21.1`.
fn compare_components(left: &[u32], right: &[u32]) -> Ordering {
    let width = left.len().max(right.len());
    (0..width)
        .map(|index| {
            let a = left.get(index).copied().unwrap_or(0);
            let b = right.get(index).copied().unwrap_or(0);
            a.cmp(&b)
        })
        .find(|ordering| ordering.is_ne())
        .unwrap_or(Ordering::Equal)
}

/// Drops trailing zero components, so that the canonical form of `1.21.0` and
/// of `1.21` is the same slice - which is what keeps `Hash` in step with `Eq`.
fn significant(components: &[u32]) -> &[u32] {
    let end = components
        .iter()
        .rposition(|component| *component != 0)
        .map_or(0, |index| index + 1);
    components.get(..end).unwrap_or(components)
}

impl PartialEq for MinecraftVersionId {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for MinecraftVersionId {}

impl std::hash::Hash for MinecraftVersionId {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match self {
            Self::Numbered { components, .. } => {
                state.write_u8(0);
                significant(components).hash(state);
            }
            Self::Unrecognized(raw) => {
                state.write_u8(1);
                raw.hash(state);
            }
        }
    }
}

impl Ord for MinecraftVersionId {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Numbered { components: a, .. }, Self::Numbered { components: b, .. }) => {
                compare_components(a, b)
            }
            // An unknown scheme is assumed to be a future one, so it sorts
            // above everything this build can read. Documented on the type
            // because it is a guess, and a display-only one: real selection
            // uses the manifest's own order.
            (Self::Unrecognized(_), Self::Numbered { .. }) => Ordering::Greater,
            (Self::Numbered { .. }, Self::Unrecognized(_)) => Ordering::Less,
            // Alphabetical among themselves purely so the ordering is total and
            // stable; it carries no claim about which is newer.
            (Self::Unrecognized(a), Self::Unrecognized(b)) => a.cmp(b),
        }
    }
}

impl PartialOrd for MinecraftVersionId {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for MinecraftVersionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.as_str())
    }
}

impl From<&str> for MinecraftVersionId {
    fn from(value: &str) -> Self {
        Self::parse(value)
    }
}

impl From<String> for MinecraftVersionId {
    fn from(value: String) -> Self {
        Self::parse(&value)
    }
}

impl From<MinecraftVersionId> for String {
    fn from(value: MinecraftVersionId) -> Self {
        match value {
            MinecraftVersionId::Numbered { raw, .. } | MinecraftVersionId::Unrecognized(raw) => raw,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn id(text: &str) -> MinecraftVersionId {
        MinecraftVersionId::parse(text)
    }

    #[test]
    fn both_published_schemes_parse() {
        assert_eq!(id("1.12.2").components(), Some([1, 12, 2].as_slice()));
        assert_eq!(id("1.21").components(), Some([1, 21].as_slice()));
        assert_eq!(id("26.1").components(), Some([26, 1].as_slice()));
        assert_eq!(id("26.1.2").components(), Some([26, 1, 2].as_slice()));
    }

    #[test]
    fn an_id_displays_exactly_as_mojang_published_it() {
        assert_eq!(id("1.21").to_string(), "1.21");
        assert_eq!(id(" 26.1 ").to_string(), "26.1");
        assert_eq!(id("1.RV-Pre1").to_string(), "1.RV-Pre1");
    }

    #[test]
    fn calendar_versions_order_above_the_old_scheme_without_special_casing() {
        assert!(id("26.1") > id("1.21.11"));
        assert!(id("26.2") > id("26.1.2"));
        assert!(id("1.21.11") > id("1.21.9"), "not a lexicographic compare");
    }

    #[test]
    fn a_missing_trailing_component_reads_as_zero() {
        assert_eq!(id("1.21"), id("1.21.0"));
        assert!(id("1.21") < id("1.21.1"));
        assert!(id("1.13") > id("1.12.2"));
    }

    #[test]
    fn equality_and_hashing_agree_with_ordering() {
        use std::collections::HashSet;

        // Two spellings of one version must not become two map entries; that
        // is the failure mode a derived `Hash` alongside a hand-written `Ord`
        // would have introduced.
        let set: HashSet<_> = [id("1.21"), id("1.21.0"), id("1.21.1")]
            .into_iter()
            .collect();
        assert_eq!(set.len(), 2);
        assert_eq!(id("1.21").cmp(&id("1.21.0")), std::cmp::Ordering::Equal);
    }

    #[test]
    fn ordering_a_whole_release_set_matches_mojangs_own_order() {
        let mut released = [
            id("1.12.2"),
            id("26.1"),
            id("1.20.1"),
            id("1.21.11"),
            id("26.1.2"),
            id("1.21"),
        ];
        released.sort();
        let ordered: Vec<_> = released.iter().map(ToString::to_string).collect();
        assert_eq!(
            ordered,
            ["1.12.2", "1.20.1", "1.21", "1.21.11", "26.1", "26.1.2"]
        );
    }

    #[test]
    fn a_snapshot_or_pre_release_style_id_is_kept_rather_than_dropped() {
        for text in [
            "1.12.2-pre1",
            "26.2-rc-1",
            "b1.7.3",
            "rd-132211",
            "1.RV-Pre1",
        ] {
            let parsed = id(text);
            assert!(!parsed.is_recognized(), "`{text}` should not parse");
            assert_eq!(parsed.as_str(), text, "`{text}` must survive verbatim");
        }
    }

    #[test]
    fn a_loader_version_manifest_id_is_not_a_minecraft_version() {
        for text in [
            "neoforge-26.2.0.59",
            "neoforge-21.1.248",
            "1.20.1-forge-47.4.10",
            "1.12.2-forge-14.23.5.2859",
        ] {
            let parsed = id(text);
            assert!(
                !parsed.is_recognized(),
                "`{text}` is a loader build id and must not read as a Minecraft release"
            );
            assert_eq!(parsed.as_str(), text, "`{text}` must survive verbatim");
        }
    }

    #[test]
    fn a_loader_build_number_is_not_the_minecraft_version_it_was_built_for() {
        // Why a loader id must reach its Minecraft release through the
        // document's `inheritsFrom` rather than by removing the loader's name
        // from the front: NeoForge 26.2.0.59 targets Minecraft 26.2, but the
        // remainder of its id parses cleanly as a four-component version that
        // is neither equal to 26.2 nor ordered anywhere near it.
        assert_ne!(id("26.2.0.59"), id("26.2"));
        assert!(id("26.2.0.59") > id("26.2"));
        assert!(id("26.2.0.59") < id("26.2.1"));
    }

    #[test]
    fn a_malformed_id_is_never_a_panic() {
        for text in ["", "   ", ".", "..", "1..2", "1.", ".1", "99999999999999"] {
            assert!(!id(text).is_recognized(), "`{text}` should not parse");
        }
    }

    #[test]
    fn an_unrecognized_id_is_assumed_to_be_a_future_scheme() {
        let future = id("27w03a-something");
        assert!(future > id("26.1"));
        assert!(future > id("1.12.2"));
    }

    #[test]
    fn the_support_floor_is_decided_from_the_id_when_both_sides_parse() {
        assert_eq!(id("1.12.2").is_supported(), Some(true));
        assert_eq!(id("1.20.1").is_supported(), Some(true));
        assert_eq!(id("26.1").is_supported(), Some(true));
        assert_eq!(id("1.12.1").is_supported(), Some(false));
        assert_eq!(id("1.11.2").is_supported(), Some(false));
        assert_eq!(id("1.7.10").is_supported(), Some(false));
    }

    #[test]
    fn the_support_floor_is_undecidable_rather_than_guessed_for_an_unknown_scheme() {
        assert_eq!(id("b1.7.3").is_supported(), None);
        assert_eq!(id("some-future-scheme").is_supported(), None);
    }

    #[test]
    fn an_id_round_trips_through_serde_as_the_published_string() {
        let encoded = serde_json::to_string(&id("26.1")).expect("serialize");
        assert_eq!(encoded, r#""26.1""#);
        let decoded: MinecraftVersionId = serde_json::from_str(&encoded).expect("deserialize");
        assert_eq!(decoded, id("26.1"));

        let odd: MinecraftVersionId = serde_json::from_str(r#""b1.7.3""#).expect("deserialize");
        assert_eq!(odd, MinecraftVersionId::Unrecognized("b1.7.3".to_owned()));
    }
}
