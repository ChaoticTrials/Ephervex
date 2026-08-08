//! Mojang's asset index, and the objects it names.
//!
//! A version JSON points at an *index* rather than at the assets themselves:
//! one document mapping logical names (`minecraft/sounds/mob/pig/say3.ogg`) to
//! a SHA-1 and a size. The game locates each file by that hash under
//! `<assets>/objects/<first two hex characters>/<hash>`, which is why
//! `CacheLayout::asset_object` mirrors the layout exactly — any other one would
//! need a translation step that could only introduce bugs.
//!
//! Several versions share one index: 1.20.1 and 1.20.2 both use index `5`. That
//! is the whole reason assets are a launcher-level store  — the
//! 1.12 index alone is 1 305 objects and 129 MB, and copying it into every
//! disposable instance would make temporary instances anything but cheap.
//!
//! # The two legacy layouts are refused rather than approximated
//!
//! Indexes before 1.7.3 set `virtual: true`, and 1.6 sets
//! `map_to_resources: true`; both mean the objects have to be materialized
//! under their logical names instead of by hash. Ephervex's floor is 1.12.2, so
//! neither can occur — and rather than carry an unexercised code path, an index
//! that declares either is reported as unsupported. That is the honest failure
//! The test suite covers: silently laying the files out by hash would produce
//! a game with no sounds and no obvious reason why.

use std::collections::BTreeMap;

use ephervex_model::{Hash, HashAlgorithm};
use serde::Deserialize;

/// One file the game loads by hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetObject {
    pub hash: Hash,
    pub size: u64,
}

/// Why an asset index could not be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum AssetIndexError {
    /// Not an asset index document at all.
    #[error("asset index `{id}` is not a well-formed index")]
    Malformed { id: String },
    /// An object's digest is not a well-formed SHA-1.
    #[error("asset index `{id}` declares a malformed sha1 for `{object}`")]
    MalformedDigest { id: String, object: String },
    /// A pre-1.7 index layout, which cannot occur at or above the supported
    /// floor and is refused rather than half-implemented.
    #[error("asset index `{id}` uses the pre-1.7 `{form}` layout, which is not supported")]
    UnsupportedLayout { id: String, form: &'static str },
}

/// One version's asset index, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetIndex {
    /// Mojang's own id for the index, e.g. `1.12`, `5`, `17`, `30`. This is
    /// what `--assetIndex` is given.
    pub id: String,
    /// Logical name to object. Ordered so that a download plan is deterministic
    /// and two runs produce the same log.
    pub objects: BTreeMap<String, AssetObject>,
}

impl AssetIndex {
    /// Parses an index document fetched for the index named `id`.
    pub fn parse(id: &str, json: &str) -> Result<Self, AssetIndexError> {
        let raw: RawAssetIndex = serde_json::from_str(json)
            .map_err(|_| AssetIndexError::Malformed { id: id.to_owned() })?;

        if raw.is_virtual {
            return Err(AssetIndexError::UnsupportedLayout {
                id: id.to_owned(),
                form: "virtual",
            });
        }
        if raw.map_to_resources {
            return Err(AssetIndexError::UnsupportedLayout {
                id: id.to_owned(),
                form: "map_to_resources",
            });
        }

        let mut objects = BTreeMap::new();
        for (name, object) in raw.objects {
            let hash = Hash::new(HashAlgorithm::Sha1, &object.hash).ok_or_else(|| {
                AssetIndexError::MalformedDigest {
                    id: id.to_owned(),
                    object: name.clone(),
                }
            })?;
            objects.insert(
                name,
                AssetObject {
                    hash,
                    size: object.size,
                },
            );
        }

        Ok(Self {
            id: id.to_owned(),
            objects,
        })
    }

    /// How many objects the index names.
    #[must_use]
    pub fn len(&self) -> usize {
        self.objects.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }

    /// Combined size of every object, for progress before anything is fetched.
    #[must_use]
    pub fn total_size(&self) -> u64 {
        self.objects
            .values()
            .map(|object| object.size)
            .fold(0, u64::saturating_add)
    }

    /// The distinct objects, deduplicated by digest.
    ///
    /// Several logical names legitimately share one file — the same texture
    /// under two paths — and they share one cache entry too, because the store
    /// is content-addressed. Downloading it once instead of once per name is
    /// the difference between 4 750 requests and rather fewer.
    #[must_use]
    pub fn distinct_objects(&self) -> Vec<(&str, &AssetObject)> {
        let mut seen = std::collections::BTreeSet::new();
        self.objects
            .iter()
            .filter(|(_, object)| seen.insert(object.hash.digest().to_owned()))
            .map(|(name, object)| (name.as_str(), object))
            .collect()
    }
}

// ------------------------------------------------------------------ the wire

#[derive(Debug, Deserialize)]
struct RawAssetIndex {
    objects: BTreeMap<String, RawAssetObject>,
    /// Pre-1.7.3. Named `virtual` in the document, which is a Rust keyword.
    #[serde(default, rename = "virtual")]
    is_virtual: bool,
    /// 1.6 only.
    #[serde(default)]
    map_to_resources: bool,
}

#[derive(Debug, Deserialize)]
struct RawAssetObject {
    hash: String,
    size: u64,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    const INDEX: &str = r#"{"objects":{
        "icons/icon_16x16.png": {"hash":"5ff04807c356f1beed0b86ccf659b44b9983e3fa","size":781},
        "minecraft/lang/en_us.json": {"hash":"af96f55a90eaf11b327f1b5f8834a051027dc506","size":2063},
        "realms/lang/en_us.json": {"hash":"af96f55a90eaf11b327f1b5f8834a051027dc506","size":2063}
    }}"#;

    #[test]
    fn an_index_parses_into_ordered_objects_with_validated_digests() {
        let index = AssetIndex::parse("1.12", INDEX).expect("index parses");
        assert_eq!(index.id, "1.12");
        assert_eq!(index.len(), 3);
        assert!(!index.is_empty());
        assert_eq!(index.total_size(), 781 + 2063 + 2063);

        let object = index
            .objects
            .get("icons/icon_16x16.png")
            .expect("named object");
        assert_eq!(object.hash.algorithm, HashAlgorithm::Sha1);
        assert_eq!(object.size, 781);
    }

    #[test]
    fn two_names_sharing_one_file_are_fetched_once() {
        // The store is content-addressed, so the second name would resolve to
        // the identical path; requesting it again is pure waste.
        let index = AssetIndex::parse("1.12", INDEX).expect("index parses");
        let distinct = index.distinct_objects();
        assert_eq!(distinct.len(), 2);
        assert_eq!(
            distinct.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
            ["icons/icon_16x16.png", "minecraft/lang/en_us.json"]
        );
    }

    #[test]
    fn a_malformed_digest_is_refused_at_the_parsing_boundary() {
        let error = AssetIndex::parse("1.12", r#"{"objects":{"a":{"hash":"nope","size":1}}}"#)
            .expect_err("a bad sha1 must not reach a cache path");
        assert!(matches!(
            error,
            AssetIndexError::MalformedDigest { object,.. } if object == "a"
        ));
    }

    #[test]
    fn a_document_that_is_not_an_index_fails_rather_than_yielding_no_assets() {
        // Silently succeeding with zero objects would produce a game with no
        // sounds and no textures, and no reason on screen for either.
        let error = AssetIndex::parse("1.12", r#"{"totalSize":1}"#).expect_err("not an index");
        assert!(matches!(error, AssetIndexError::Malformed { .. }));
    }

    #[test]
    fn the_pre_1_7_layouts_are_refused_rather_than_laid_out_by_hash_anyway() {
        for (form, json) in [
            ("virtual", r#"{"virtual":true,"objects":{}}"#),
            (
                "map_to_resources",
                r#"{"map_to_resources":true,"objects":{}}"#,
            ),
        ] {
            let error = AssetIndex::parse("legacy", json)
                .expect_err("a layout the launcher does not implement must fail loudly");
            assert!(
                matches!(error, AssetIndexError::UnsupportedLayout { form: reported,.. } if reported == form),
                "{form}: {error:?}"
            );
        }
    }
}
