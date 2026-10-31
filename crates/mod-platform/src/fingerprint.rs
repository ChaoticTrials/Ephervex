//! The file fingerprint CurseForge indexes its catalog by.
//!
//! CurseForge can answer "which release is this jar?" for a file nobody has an
//! id for, but only against its own fingerprint: a 32-bit MurmurHash2 of the
//! file with ASCII whitespace removed first. It is not a digest and nothing
//! here treats it as one -- it is a lookup key, and a match is only ever
//! believed after the answer's own SHA-1 is checked against the file's.
//!
//! Implemented here rather than pulled in because no crate in this workspace
//! provides MurmurHash2, and because the whitespace rule is CurseForge's own
//! rather than part of the hash. The algorithm is fixed by what the service
//! already has indexed, so this is a compatibility constraint and not a choice:
//! it cannot be improved, only matched.

/// Bytes CurseForge drops before hashing: tab, line feed, carriage return, and
/// space.
const STRIPPED: [u8; 4] = [9, 10, 13, 32];

/// MurmurHash2's mixing constants, as the algorithm defines them.
const M: u32 = 0x5bd1_e995;
const R: u32 = 24;

/// The seed CurseForge fingerprints with.
const SEED: u32 = 1;

/// The fingerprint CurseForge would have for these bytes.
#[must_use]
pub fn curseforge_fingerprint(bytes: &[u8]) -> u32 {
    let normalized: Vec<u8> = bytes
        .iter()
        .copied()
        .filter(|byte| !STRIPPED.contains(byte))
        .collect();
    murmur2(&normalized, SEED)
}

/// MurmurHash2, 32-bit, little-endian.
///
/// Every operation wraps: the algorithm is defined on 32-bit overflow, so a
/// checked or saturating variant would produce a different number and match
/// nothing.
// The length is mixed in as a 32-bit value, so a file larger than 4 GiB wraps.
// That is the algorithm as defined and as CurseForge's index was built, not an
// oversight: widening it here would produce a different number for such a file
// and match nothing.
#[allow(clippy::cast_possible_truncation)]
fn murmur2(data: &[u8], seed: u32) -> u32 {
    let mut hash = seed ^ (data.len() as u32);
    let (chunks, tail) = data.as_chunks::<4>();
    for chunk in chunks {
        let mut block = u32::from_le_bytes(*chunk);
        block = block.wrapping_mul(M);
        block ^= block >> R;
        block = block.wrapping_mul(M);
        hash = hash.wrapping_mul(M);
        hash ^= block;
    }

    if !tail.is_empty() {
        if tail.len() >= 3 {
            hash ^= u32::from(tail[2]) << 16;
        }
        if tail.len() >= 2 {
            hash ^= u32::from(tail[1]) << 8;
        }
        hash ^= u32::from(tail[0]);
        hash = hash.wrapping_mul(M);
    }

    hash ^= hash >> 13;
    hash = hash.wrapping_mul(M);
    hash ^= hash >> 15;
    hash
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    /// Values cross-checked against a separate implementation written from
    /// Appleby's published MurmurHash2, so a slip in the block loop, the tail
    /// branches, or the wrapping arithmetic fails here rather than silently
    /// matching nothing in the catalog.
    ///
    /// These pin this implementation, not CurseForge's index. Confirming the
    /// two agree needs an API key and a real file; see the fingerprint tests in
    /// `curseforge_api.rs` for what is checked without one.
    #[test]
    fn murmur2_matches_an_independent_implementation() {
        assert_eq!(murmur2(b"", 0), 0);
        assert_eq!(murmur2(b"abcd", 0), 646_393_889);
        assert_eq!(murmur2(b"hello", 0), 3_848_350_155);

        // Seed 1 is what a fingerprint uses, and every tail length with it.
        assert_eq!(murmur2(b"", 1), 1_540_447_798);
        assert_eq!(murmur2(b"a", 1), 626_045_324);
        assert_eq!(murmur2(b"ab", 1), 1_692_487_918);
        assert_eq!(murmur2(b"abc", 1), 1_621_425_345);
        assert_eq!(murmur2(b"abcd", 1), 3_376_380_438);
        assert_eq!(murmur2(b"abcde", 1), 3_469_237_630);
        assert_eq!(murmur2(b"hello", 1), 2_788_266_382);
        assert_eq!(murmur2(b"jei.jar", 1), 2_038_518_775);
    }

    /// The whitespace rule is CurseForge's, not the hash's: a jar and the same
    /// jar with spaces in it fingerprint alike, which is the entire reason the
    /// rule exists.
    #[test]
    fn whitespace_is_removed_before_hashing() {
        assert_eq!(
            curseforge_fingerprint(b"a b\tc\r\nd"),
            curseforge_fingerprint(b"abcd")
        );
        assert_eq!(curseforge_fingerprint(b"abcd"), murmur2(b"abcd", SEED));
        assert_eq!(curseforge_fingerprint(b"a b\tc\r\nd"), 3_376_380_438);
        assert_eq!(curseforge_fingerprint(b"jei.jar"), 2_038_518_775);
    }

    /// Every tail length has its own branch, and getting one wrong would
    /// silently mismatch only files of that length modulo four.
    #[test]
    fn every_tail_length_is_mixed_in() {
        let fingerprints: Vec<u32> = (0..8)
            .map(|len| curseforge_fingerprint(&vec![b'x'; len]))
            .collect();
        let mut unique = fingerprints.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), fingerprints.len(), "{fingerprints:?}");
    }

    /// Different content fingerprints differently, and the same content always
    /// the same way.
    #[test]
    fn the_fingerprint_is_stable_and_content_dependent() {
        assert_eq!(
            curseforge_fingerprint(b"jei.jar"),
            curseforge_fingerprint(b"jei.jar")
        );
        assert_ne!(
            curseforge_fingerprint(b"jei.jar"),
            curseforge_fingerprint(b"jei2.jar")
        );
    }
}
