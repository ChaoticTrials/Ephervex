//! A count ceiling on a manifest's declared artifact list.
//!
//! The list is counted without building a record per element, so a compact
//! manifest declaring millions of files is refused before the typed parse
//! allocates for each of them. The typed parse that follows reads the manifest
//! itself, so its errors keep their position in the manifest.

use std::fmt;

use serde::Deserialize;
use serde::de::{Deserializer, Error as _, IgnoredAny, MapAccess, SeqAccess, Visitor};

use crate::error::{PackError, PackResult};

/// Refuses a manifest whose top-level `files` array holds more than `max`
/// elements.
///
/// Every manifest the typed parse can accept passes through here first, so the
/// pass rejects what it cannot count: a document that is not a JSON object is
/// refused, because a derived `serde` struct would otherwise read it from an
/// array by position and its `files` would escape the ceiling.
pub(crate) fn require_declared_files_within(
    entry: &str,
    manifest: &[u8],
    max: u64,
) -> PackResult<()> {
    let Declarations { files } =
        serde_json::from_slice(manifest).map_err(|source| PackError::ManifestInvalid {
            entry: entry.to_owned(),
            source,
        })?;
    if files > max {
        return Err(PackError::LimitExceeded {
            limit: "declared-files",
            allowed: max,
            reached: files,
        });
    }
    Ok(())
}

/// The size of the `files` array of a manifest, zero when it has none.
struct Declarations {
    files: u64,
}

#[derive(Deserialize)]
#[serde(field_identifier)]
enum Member {
    #[serde(rename = "files")]
    Files,
    #[serde(other)]
    Other,
}

impl<'de> Deserialize<'de> for Declarations {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(DeclarationsVisitor)
    }
}

struct DeclarationsVisitor;

impl<'de> Visitor<'de> for DeclarationsVisitor {
    type Value = Declarations;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a manifest object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Declarations, A::Error> {
        let mut files = None;
        while let Some(member) = map.next_key::<Member>()? {
            match member {
                Member::Files if files.is_some() => {
                    return Err(A::Error::duplicate_field("files"));
                }
                Member::Files => files = Some(map.next_value::<ElementCount>()?.0),
                Member::Other => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(Declarations {
            files: files.unwrap_or(0),
        })
    }
}

struct ElementCount(u64);

impl<'de> Deserialize<'de> for ElementCount {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_seq(ElementCountVisitor)
    }
}

struct ElementCountVisitor;

impl<'de> Visitor<'de> for ElementCountVisitor {
    type Value = ElementCount;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an array of file declarations")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<ElementCount, A::Error> {
        let mut count = 0_u64;
        while seq.next_element::<IgnoredAny>()?.is_some() {
            count = count.saturating_add(1);
        }
        Ok(ElementCount(count))
    }
}
