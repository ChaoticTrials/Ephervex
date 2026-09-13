//! Content hashes attached to downloadable artifacts.
//!
//! Mojang metadata supplies SHA-1; Modrinth packs supply SHA-1 and SHA-512.
//! The download manager verifies whichever hashes it is given, and a mismatch
//! is a hard failure.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Hash algorithms the launcher can verify.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HashAlgorithm {
    Sha1,
    Sha256,
    Sha512,
}

impl HashAlgorithm {
    /// Every algorithm this build can verify.
    ///
    /// Exists so code that has to recognize an algorithm from its own on-disk
    /// spelling can iterate the set instead of hand-listing it and going stale
    /// the next time one is added.
    pub const ALL: [Self; 3] = [Self::Sha1, Self::Sha256, Self::Sha512];

    /// Length of the hex-encoded digest.
    #[must_use]
    pub const fn hex_len(self) -> usize {
        match self {
            Self::Sha1 => 40,
            Self::Sha256 => 64,
            Self::Sha512 => 128,
        }
    }
}

impl fmt::Display for HashAlgorithm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Sha1 => "sha1",
            Self::Sha256 => "sha256",
            Self::Sha512 => "sha512",
        })
    }
}

/// A hex-encoded digest together with the algorithm that produced it.
///
/// The stored digest is always lowercase, so comparison is a plain string
/// equality check and case differences between providers cannot cause a
/// spurious mismatch.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Hash {
    pub algorithm: HashAlgorithm,
    digest: String,
}

impl Hash {
    /// Creates a hash, normalizing the digest to lowercase.
    ///
    /// Returns `None` if the digest is not valid hex of the right length for
    /// the algorithm - malformed provider metadata should be rejected at the
    /// parsing boundary rather than failing later during verification.
    #[must_use]
    pub fn new(algorithm: HashAlgorithm, digest: &str) -> Option<Self> {
        let digest = digest.trim().to_ascii_lowercase();
        let well_formed =
            digest.len() == algorithm.hex_len() && digest.bytes().all(|b| b.is_ascii_hexdigit());
        well_formed.then_some(Self { algorithm, digest })
    }

    /// The lowercase hex digest.
    #[must_use]
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// Compares against a computed digest, ignoring case.
    #[must_use]
    pub fn matches(&self, computed_hex: &str) -> bool {
        self.digest.eq_ignore_ascii_case(computed_hex.trim())
    }
}

impl fmt::Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.algorithm, self.digest)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    const SHA1_OF_EMPTY: &str = "da39a3ee5e6b4b0d3255bfef95601890afd80709";

    #[test]
    fn digests_are_normalized_to_lowercase() {
        let hash = Hash::new(HashAlgorithm::Sha1, &SHA1_OF_EMPTY.to_ascii_uppercase())
            .expect("valid sha1");
        assert_eq!(hash.digest(), SHA1_OF_EMPTY);
    }

    #[test]
    fn comparison_ignores_case_from_either_side() {
        let hash = Hash::new(HashAlgorithm::Sha1, SHA1_OF_EMPTY).expect("valid sha1");
        assert!(hash.matches(&SHA1_OF_EMPTY.to_ascii_uppercase()));
        assert!(!hash.matches("0000000000000000000000000000000000000000"));
    }

    #[test]
    fn wrong_length_is_rejected() {
        assert!(Hash::new(HashAlgorithm::Sha512, SHA1_OF_EMPTY).is_none());
    }

    #[test]
    fn non_hex_is_rejected() {
        let not_hex = "z".repeat(40);
        assert!(Hash::new(HashAlgorithm::Sha1, &not_hex).is_none());
    }
}
