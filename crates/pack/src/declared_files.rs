//! Count ceilings on a manifest's declared artifact list.
//!
//! The list is counted without building a record per element, so a compact
//! manifest declaring millions of files, or millions of download addresses for
//! one file, is refused before the typed parse allocates for each of them. The
//! typed parse that follows reads the manifest itself, so its errors keep their
//! position in the manifest.

use std::fmt;

use serde::Deserialize;
use serde::de::{Deserializer, Error as _, IgnoredAny, MapAccess, SeqAccess, Unexpected, Visitor};

use crate::archive::ExtractionLimits;
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
    let declarations = count_declarations(entry, manifest)?;
    require_within("declared-files", max, declarations.files)
}

/// Refuses a manifest that declares more files, more download addresses for
/// one file, or more download addresses in total than `limits` allow.
///
/// Addresses are counted as written, before any allowlist decides which are
/// usable, so a list of refused addresses is bounded too.
pub(crate) fn require_declarations_within(
    entry: &str,
    manifest: &[u8],
    limits: &ExtractionLimits,
) -> PackResult<()> {
    let declarations = count_declarations(entry, manifest)?;
    require_within(
        "declared-files",
        limits.max_declared_files,
        declarations.files,
    )?;
    require_within(
        "download-addresses-per-file",
        limits.max_download_addresses_per_file,
        declarations.largest_address_list,
    )?;
    require_within(
        "download-addresses",
        limits.max_download_addresses,
        declarations.addresses,
    )
}

fn require_within(limit: &'static str, allowed: u64, reached: u64) -> PackResult<()> {
    if reached > allowed {
        return Err(PackError::LimitExceeded {
            limit,
            allowed,
            reached,
        });
    }
    Ok(())
}

fn count_declarations(entry: &str, manifest: &[u8]) -> PackResult<Declarations> {
    serde_json::from_slice(manifest).map_err(|source| PackError::ManifestInvalid {
        entry: entry.to_owned(),
        source,
    })
}

/// The size of the `files` array of a manifest, zero when it has none, and how
/// many download addresses its elements list.
struct Declarations {
    files: u64,
    addresses: u64,
    largest_address_list: u64,
}

#[derive(Deserialize)]
#[serde(field_identifier)]
enum Member {
    #[serde(rename = "files")]
    Files,
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(field_identifier)]
enum FileMember {
    #[serde(rename = "downloads")]
    Downloads,
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
                Member::Files => files = Some(map.next_value::<FileList>()?),
                Member::Other => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        let files = files.unwrap_or_default();
        Ok(Declarations {
            files: files.count,
            addresses: files.addresses,
            largest_address_list: files.largest_address_list,
        })
    }
}

/// The `files` array: its length and the addresses its elements list.
#[derive(Default)]
struct FileList {
    count: u64,
    addresses: u64,
    largest_address_list: u64,
}

impl<'de> Deserialize<'de> for FileList {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_seq(FileListVisitor)
    }
}

struct FileListVisitor;

impl<'de> Visitor<'de> for FileListVisitor {
    type Value = FileList;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an array of file declarations")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<FileList, A::Error> {
        let mut list = FileList::default();
        while let Some(DeclaredFile(addresses)) = seq.next_element()? {
            list.count = list.count.saturating_add(1);
            list.addresses = list.addresses.saturating_add(addresses);
            list.largest_address_list = list.largest_address_list.max(addresses);
        }
        Ok(list)
    }
}

/// One element of `files`, reduced to the number of addresses in its
/// `downloads` array.
///
/// A `downloads` member that is not an array counts none and a repeated one
/// counts every occurrence: the pass also runs over manifests that ignore
/// unknown members, and the Modrinth typed parse refuses both shapes with
/// their position. A scalar element lists none and the typed parse refuses it
/// too. An array element is refused here, because a derived `serde` struct
/// would read it by position and its addresses would escape the count.
struct DeclaredFile(u64);

impl<'de> Deserialize<'de> for DeclaredFile {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(DeclaredFileVisitor)
    }
}

struct DeclaredFileVisitor;

impl<'de> Visitor<'de> for DeclaredFileVisitor {
    type Value = DeclaredFile;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a file declaration")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<DeclaredFile, A::Error> {
        let mut addresses = 0_u64;
        while let Some(member) = map.next_key::<FileMember>()? {
            match member {
                FileMember::Downloads => {
                    addresses = addresses.saturating_add(map.next_value::<AddressCount>()?.0);
                }
                FileMember::Other => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(DeclaredFile(addresses))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, _: A) -> Result<DeclaredFile, A::Error> {
        Err(A::Error::invalid_type(Unexpected::Seq, &self))
    }

    fn visit_bool<E>(self, _: bool) -> Result<DeclaredFile, E> {
        Ok(DeclaredFile(0))
    }

    fn visit_i64<E>(self, _: i64) -> Result<DeclaredFile, E> {
        Ok(DeclaredFile(0))
    }

    fn visit_u64<E>(self, _: u64) -> Result<DeclaredFile, E> {
        Ok(DeclaredFile(0))
    }

    fn visit_f64<E>(self, _: f64) -> Result<DeclaredFile, E> {
        Ok(DeclaredFile(0))
    }

    fn visit_str<E>(self, _: &str) -> Result<DeclaredFile, E> {
        Ok(DeclaredFile(0))
    }

    fn visit_unit<E>(self) -> Result<DeclaredFile, E> {
        Ok(DeclaredFile(0))
    }
}

/// The number of elements of a `downloads` member, zero when it is not an
/// array.
struct AddressCount(u64);

impl<'de> Deserialize<'de> for AddressCount {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(AddressCountVisitor)
    }
}

struct AddressCountVisitor;

impl<'de> Visitor<'de> for AddressCountVisitor {
    type Value = AddressCount;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a list of download addresses")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<AddressCount, A::Error> {
        let mut count = 0_u64;
        while seq.next_element::<IgnoredAny>()?.is_some() {
            count = count.saturating_add(1);
        }
        Ok(AddressCount(count))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<AddressCount, A::Error> {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(AddressCount(0))
    }

    fn visit_bool<E>(self, _: bool) -> Result<AddressCount, E> {
        Ok(AddressCount(0))
    }

    fn visit_i64<E>(self, _: i64) -> Result<AddressCount, E> {
        Ok(AddressCount(0))
    }

    fn visit_u64<E>(self, _: u64) -> Result<AddressCount, E> {
        Ok(AddressCount(0))
    }

    fn visit_f64<E>(self, _: f64) -> Result<AddressCount, E> {
        Ok(AddressCount(0))
    }

    fn visit_str<E>(self, _: &str) -> Result<AddressCount, E> {
        Ok(AddressCount(0))
    }

    fn visit_unit<E>(self) -> Result<AddressCount, E> {
        Ok(AddressCount(0))
    }
}
