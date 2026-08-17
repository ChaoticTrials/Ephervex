//! Where the CurseForge API key comes from.
//!
//! One method, and deliberately only one. The transport needs an opaque secret
//! and needs to be told when there is none; it has no business knowing about
//! keyrings, settings files, or a GUI. Everything that does know lives above
//! this crate and implements this trait.

use std::fmt;

use ephervex_model::{ModProvider, SecretString};

use crate::error::{ModPlatformError, ModPlatformResult};

/// Somewhere a CurseForge API key can be obtained from.
///
/// Synchronous, because reading a credential is a local operation in every
/// implementation there is reason to have: a keyring round trip over D-Bus, or
/// a value already in hand. An async method here would infect
/// [`CurseForgeApi`](super::CurseForgeApi) for no benefit.
pub trait CurseForgeCredentialSource: fmt::Debug + Send + Sync {
    /// The configured API key, or `None` when the user has not set one.
    ///
    /// `None` and an error are different answers and stay that way: "you have
    /// not filled this in" sends the user to a settings screen, while "your
    /// keyring is locked" sends them somewhere else entirely.
    fn api_key(&self) -> ModPlatformResult<Option<SecretString>>;
}

/// A key that is already in hand.
///
/// Used by tests, and by any caller that has resolved the credential itself.
/// Holding a [`SecretString`] rather than a `String` is what keeps it out of
/// the `Debug` output this type derives.
#[derive(Debug, Clone, Default)]
pub struct ConfiguredApiKey(Option<SecretString>);

impl ConfiguredApiKey {
    /// A source that hands out this key.
    #[must_use]
    pub fn new(key: SecretString) -> Self {
        Self(Some(key))
    }

    /// A source with no key, which is what an unconfigured launcher has.
    #[must_use]
    pub const fn absent() -> Self {
        Self(None)
    }
}

impl CurseForgeCredentialSource for ConfiguredApiKey {
    fn api_key(&self) -> ModPlatformResult<Option<SecretString>> {
        Ok(self.0.clone())
    }
}

/// The refusal a request makes when no key is configured.
///
/// Here rather than at each call site so that "there is no key" is one error
/// with one message wherever it is discovered.
pub(super) const fn missing() -> ModPlatformError {
    ModPlatformError::CredentialMissing {
        provider: ModProvider::CurseForge,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    const FAKE_KEY: &str = "test-curseforge-key-do-not-use";

    #[test]
    fn a_source_with_no_key_says_so_rather_than_failing() {
        assert!(
            ConfiguredApiKey::absent()
                .api_key()
                .expect("an absent key is not an error")
                .is_none()
        );
        assert!(
            ConfiguredApiKey::default()
                .api_key()
                .expect("read")
                .is_none()
        );
    }

    #[test]
    fn a_configured_source_hands_back_the_key_and_never_renders_it() {
        let source = ConfiguredApiKey::new(SecretString::new(FAKE_KEY));
        assert_eq!(
            source.api_key().expect("read").expect("present").expose(),
            FAKE_KEY
        );
        assert!(!format!("{source:?}").contains(FAKE_KEY), "{source:?}");
    }
}
