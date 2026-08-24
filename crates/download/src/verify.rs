//! Streaming hash verification.
//!
//! Hashes are computed while the body streams past, not by re-reading the file
//! afterwards: a 300 MB JDK archive should not be read twice, and the verifier
//! must never be tempted to trust a file it did not itself observe being
//! written.
//!
//! `ephervex_model::Hash` owns digest normalization and comparison, so this
//! module only produces hex and asks that type whether it matches.

use std::path::Path;

use ephervex_model::{Hash, HashAlgorithm};
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha512};
use tokio::fs::File;
use tokio::io::AsyncReadExt as _;

use crate::error::{ArtifactStep, DownloadError};

/// A verified expectation that was not met.
///
/// Carries the digests rather than a sentence so the frontend can render them
/// in an expandable technical-details pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HashMismatch {
    pub algorithm: HashAlgorithm,
    pub expected: String,
    pub actual: String,
}

/// Computes every algorithm a set of expected hashes asks for, once.
///
/// Providers disagree about which digest they publish — Mojang gives SHA-1,
/// Modrinth gives SHA-1 and SHA-512 — so the verifier is driven by whatever
/// metadata supplied instead of by a fixed algorithm. If metadata supplies
/// nothing, verification is a no-op and the download is accepted on transport
/// integrity alone; hashes are required only when the source supplies them.
pub struct HashVerifier {
    expected: Vec<Hash>,
    sha1: Option<Sha1>,
    sha256: Option<Sha256>,
    sha512: Option<Sha512>,
}

impl std::fmt::Debug for HashVerifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The digest states are opaque and enormous; the useful debug
        // information is which expectations are in force.
        f.debug_struct("HashVerifier")
            .field("expected", &self.expected)
            .finish_non_exhaustive()
    }
}

impl HashVerifier {
    /// Prepares a verifier for the given expectations.
    #[must_use]
    pub fn new(expected: &[Hash]) -> Self {
        let mut verifier = Self {
            expected: expected.to_vec(),
            sha1: None,
            sha256: None,
            sha512: None,
        };
        verifier.reset();
        verifier
    }

    /// Discards all accumulated state and starts over.
    ///
    /// Used when an attempt cannot resume and has to restart from byte zero:
    /// the hasher state must track the bytes actually on disk exactly, so a
    /// truncated `.part` file and a stale hasher are never allowed to coexist.
    pub fn reset(&mut self) {
        self.sha1 = None;
        self.sha256 = None;
        self.sha512 = None;
        for hash in &self.expected {
            match hash.algorithm {
                HashAlgorithm::Sha1 => self.sha1 = Some(Sha1::new()),
                HashAlgorithm::Sha256 => self.sha256 = Some(Sha256::new()),
                HashAlgorithm::Sha512 => self.sha512 = Some(Sha512::new()),
            }
        }
    }

    /// Whether any expectation was supplied at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.expected.is_empty()
    }

    /// Feeds the next chunk of the body.
    pub fn update(&mut self, chunk: &[u8]) {
        if let Some(hasher) = self.sha1.as_mut() {
            hasher.update(chunk);
        }
        if let Some(hasher) = self.sha256.as_mut() {
            hasher.update(chunk);
        }
        if let Some(hasher) = self.sha512.as_mut() {
            hasher.update(chunk);
        }
    }

    /// Finalizes every digest and compares it against the expectations.
    ///
    /// Returns the first mismatch found. Expectations are checked in the order
    /// they were supplied, so provider metadata order determines which digest a
    /// user sees first.
    pub fn finish(self) -> Result<(), HashMismatch> {
        let sha1 = self.sha1.map(|hasher| hex::encode(hasher.finalize()));
        let sha256 = self.sha256.map(|hasher| hex::encode(hasher.finalize()));
        let sha512 = self.sha512.map(|hasher| hex::encode(hasher.finalize()));

        for expected in &self.expected {
            let actual = match expected.algorithm {
                HashAlgorithm::Sha1 => sha1.as_deref(),
                HashAlgorithm::Sha256 => sha256.as_deref(),
                HashAlgorithm::Sha512 => sha512.as_deref(),
            };
            // `None` is unreachable because `reset` created a hasher for every
            // expectation, but treating it as a mismatch keeps the failure mode
            // "reject the artifact" rather than "silently accept it".
            let actual = actual.unwrap_or_default();
            if !expected.matches(actual) {
                return Err(HashMismatch {
                    algorithm: expected.algorithm,
                    expected: expected.digest().to_owned(),
                    actual: actual.to_owned(),
                });
            }
        }
        Ok(())
    }
}

/// Whether a file already on disk matches every expected hash.
///
/// Reads the whole file once, in bounded chunks, computing exactly the
/// algorithms `expected` names — the same work [`HashVerifier`] does while a
/// download streams past, done here for bytes that arrived some other way (a
/// cache entry being revalidated, a file a folder watcher just found). An
/// empty `expected` is vacuously true, the same rule [`HashVerifier`] applies
/// everywhere else: verification is a no-op when nothing was published to
/// check against.
pub async fn file_matches_hashes(path: &Path, expected: &[Hash]) -> Result<bool, DownloadError> {
    if expected.is_empty() {
        return Ok(true);
    }
    let mut file = File::open(path)
        .await
        .map_err(|source| DownloadError::io(ArtifactStep::ReadCached, path, source))?;
    let mut verifier = HashVerifier::new(expected);
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .await
            .map_err(|source| DownloadError::io(ArtifactStep::ReadCached, path, source))?;
        if read == 0 {
            break;
        }
        verifier.update(&buffer[..read]);
    }
    Ok(verifier.finish().is_ok())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    const BODY: &[u8] = b"ephervex";
    const BODY_SHA1: &str = "1a2f2b1a2ff0e1e0ba2b8f22c2ab8b6f5e6c3ff3";
    const BODY_SHA512: &str = "00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000";

    fn sha1_of(bytes: &[u8]) -> String {
        let mut hasher = Sha1::new();
        hasher.update(bytes);
        hex::encode(hasher.finalize())
    }

    fn sha512_of(bytes: &[u8]) -> String {
        let mut hasher = Sha512::new();
        hasher.update(bytes);
        hex::encode(hasher.finalize())
    }

    #[test]
    fn a_verifier_without_expectations_accepts_anything() {
        let mut verifier = HashVerifier::new(&[]);
        assert!(verifier.is_empty());
        verifier.update(BODY);
        assert!(verifier.finish().is_ok());
    }

    #[test]
    fn matching_digests_across_several_algorithms_pass() {
        let expected = vec![
            Hash::new(HashAlgorithm::Sha1, &sha1_of(BODY)).expect("valid sha1"),
            Hash::new(HashAlgorithm::Sha512, &sha512_of(BODY)).expect("valid sha512"),
        ];
        let mut verifier = HashVerifier::new(&expected);
        // Chunked exactly as a streaming body would arrive.
        verifier.update(&BODY[..3]);
        verifier.update(&BODY[3..]);
        assert!(verifier.finish().is_ok());
    }

    #[test]
    fn a_wrong_digest_is_reported_with_both_sides() {
        let expected = vec![Hash::new(HashAlgorithm::Sha1, BODY_SHA1).expect("valid sha1")];
        let mut verifier = HashVerifier::new(&expected);
        verifier.update(BODY);
        let mismatch = verifier.finish().unwrap_err();
        assert_eq!(mismatch.algorithm, HashAlgorithm::Sha1);
        assert_eq!(mismatch.expected, BODY_SHA1);
        assert_eq!(mismatch.actual, sha1_of(BODY));
    }

    #[test]
    fn resetting_discards_previously_fed_bytes() {
        let expected = vec![Hash::new(HashAlgorithm::Sha1, &sha1_of(BODY)).expect("valid sha1")];
        let mut verifier = HashVerifier::new(&expected);
        verifier.update(b"garbage from an abandoned attempt");
        verifier.reset();
        verifier.update(BODY);
        assert!(verifier.finish().is_ok());
    }

    #[test]
    fn a_sha512_expectation_that_does_not_match_is_rejected() {
        let expected = vec![Hash::new(HashAlgorithm::Sha512, BODY_SHA512).expect("valid sha512")];
        let mut verifier = HashVerifier::new(&expected);
        verifier.update(BODY);
        let mismatch = verifier.finish().unwrap_err();
        assert_eq!(mismatch.algorithm, HashAlgorithm::Sha512);
    }

    #[tokio::test]
    async fn a_file_on_disk_matching_every_expected_hash_passes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("artifact.jar");
        std::fs::write(&path, BODY).expect("write fixture");
        let expected = vec![
            Hash::new(HashAlgorithm::Sha1, &sha1_of(BODY)).expect("valid sha1"),
            Hash::new(HashAlgorithm::Sha512, &sha512_of(BODY)).expect("valid sha512"),
        ];
        assert!(
            file_matches_hashes(&path, &expected)
                .await
                .expect("hashing succeeds")
        );
    }

    #[tokio::test]
    async fn a_file_on_disk_not_matching_an_expected_hash_fails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("artifact.jar");
        std::fs::write(&path, b"not the expected content").expect("write fixture");
        let expected = vec![Hash::new(HashAlgorithm::Sha1, BODY_SHA1).expect("valid sha1")];
        assert!(
            !file_matches_hashes(&path, &expected)
                .await
                .expect("hashing succeeds")
        );
    }

    #[tokio::test]
    async fn no_expected_hashes_is_vacuously_a_match() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("artifact.jar");
        std::fs::write(&path, BODY).expect("write fixture");
        assert!(
            file_matches_hashes(&path, &[])
                .await
                .expect("hashing succeeds")
        );
    }
}
