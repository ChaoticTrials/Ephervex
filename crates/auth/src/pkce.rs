//! Proof Key for Code Exchange, and the `state` value that goes with it
//! (RFC 7636).
//!
//! # Why a public client needs this
//!
//! Ephervex has no client secret and cannot have one: the binary ships to
//! users, so anything compiled into it is public by definition.
//! That leaves the authorization code as the only thing standing between an
//! attacker and a token, and the code travels back through a browser redirect
//! to a loopback port that any other process on the machine could also be
//! listening on.
//!
//! PKCE closes that: the code is only redeemable together with the `verifier`,
//! which never leaves this process. Whoever intercepts the code gets a value
//! the token endpoint will refuse.
//!
//! `state` solves a different problem — it proves the redirect that arrived is
//! the one this process asked for, rather than a cross-site forgery or a stale
//! tab from an earlier attempt. Both are needed; neither substitutes for the
//! other.
//!
//! # Entropy comes from the kernel
//!
//! `getrandom` reads the OS CSPRNG. A seeded userspace generator would be
//! wrong here for the obvious reason and for a subtle one: these values are
//! generated at process start, which is exactly when a time-seeded generator is
//! most predictable.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

use crate::secret::SecretString;

/// Bytes of entropy behind a verifier or a state value.
///
/// RFC 7636 requires the verifier to be 43–128 characters and recommends at
/// least 256 bits of entropy. 32 bytes base64url-encodes to exactly 43
/// characters, which is the minimum length and the recommended strength at
/// once.
const ENTROPY_BYTES: usize = 32;

/// One authorization request's PKCE material.
///
/// The verifier is a [`SecretString`]: it is the thing that makes an
/// intercepted authorization code useless, so it must never reach a log.
#[derive(Debug)]
pub struct Pkce {
    verifier: SecretString,
    challenge: String,
    state: String,
}

impl Pkce {
    /// Generates fresh material from the operating system's entropy source.
    ///
    /// Returns `None` if the OS refuses to provide randomness. That is not a
    /// situation to paper over with a fallback — a predictable verifier is
    /// worse than no login at all.
    #[must_use]
    pub fn generate() -> Option<Self> {
        let verifier = random_token()?;
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        Some(Self {
            verifier: SecretString::new(verifier),
            challenge,
            state: random_token()?,
        })
    }

    /// The `code_challenge` query parameter: base64url(SHA-256(verifier)).
    #[must_use]
    pub fn challenge(&self) -> &str {
        &self.challenge
    }

    /// Always `S256`. `plain` is permitted by the RFC and pointless here — it
    /// sends the verifier itself through the browser, which is the leak the
    /// mechanism exists to prevent.
    #[must_use]
    pub const fn challenge_method(&self) -> &'static str {
        "S256"
    }

    /// The `state` value this request sent, for checking against the redirect.
    #[must_use]
    pub fn state(&self) -> &str {
        &self.state
    }

    /// The `code_verifier` sent to the token endpoint.
    #[must_use]
    pub fn verifier(&self) -> &SecretString {
        &self.verifier
    }
}

/// A URL-safe random token with [`ENTROPY_BYTES`] of kernel entropy.
fn random_token() -> Option<String> {
    let mut bytes = [0_u8; ENTROPY_BYTES];
    getrandom::fill(&mut bytes).ok()?;
    Some(URL_SAFE_NO_PAD.encode(bytes))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn a_verifier_satisfies_rfc_7636s_length_and_alphabet() {
        let pkce = Pkce::generate().expect("the os provides entropy");
        let verifier = pkce.verifier().expose();
        assert!(
            (43..=128).contains(&verifier.len()),
            "length {}",
            verifier.len()
        );
        assert!(
            verifier
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~')),
            "{verifier}"
        );
    }

    #[test]
    fn the_challenge_is_the_sha256_of_the_verifier_in_url_safe_base64() {
        let pkce = Pkce::generate().expect("entropy");
        let expected = URL_SAFE_NO_PAD.encode(Sha256::digest(pkce.verifier().expose().as_bytes()));
        assert_eq!(pkce.challenge(), expected);
        assert_eq!(pkce.challenge_method(), "S256");
        // Base64url must not contain characters that would need escaping in a
        // query string, or the challenge would arrive altered.
        assert!(!pkce.challenge().contains('+'));
        assert!(!pkce.challenge().contains('/'));
        assert!(!pkce.challenge().contains('='));
    }

    #[test]
    fn the_challenge_matches_rfc_7636s_own_worked_example() {
        // Appendix B of RFC 7636. Checking against a published vector is the
        // only way to catch encoding the digest as hex, or padding the base64,
        // or hashing the wrong string — all of which produce a plausible-looking
        // value that Microsoft rejects at the very end of the flow.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn two_requests_never_share_a_verifier_or_a_state() {
        let first = Pkce::generate().expect("entropy");
        let second = Pkce::generate().expect("entropy");
        assert_ne!(first.verifier().expose(), second.verifier().expose());
        assert_ne!(first.state(), second.state());
        // The state must not be derived from the verifier: they defend against
        // different attacks and one must not leak the other.
        assert_ne!(first.state(), first.verifier().expose());
    }

    #[test]
    fn the_verifier_is_not_renderable() {
        let pkce = Pkce::generate().expect("entropy");
        let rendered = format!("{pkce:?}");
        assert!(!rendered.contains(pkce.verifier().expose()), "{rendered}");
        // The challenge and state are public by design and stay visible, which
        // is what makes a log line about a failed exchange useful at all.
        assert!(rendered.contains(pkce.challenge()));
    }
}
