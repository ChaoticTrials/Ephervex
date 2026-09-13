//! Which provider credential a retained artifact's download may carry.
//!
//! # The rule this exists to express
//!
//! A recipe entry records everything needed to *identify and verify* an
//! artifact: its exact addresses, its published digests, its size, and the
//! provider identity it came from. That is enough to install it from the shared
//! cache forever, and it is why an instance, once installed, never depends on a
//! provider being reachable or configured.
//!
//! It is not always enough to *fetch* it again. One provider serves its
//! artifacts only to a request carrying an API key, so a recipe entry whose
//! verified copy has been collected out of the shared cache needs that key
//! before the bytes can come back. The credential is not part of the desired
//! state and is never written into a recipe; it is asked for, per artifact, at
//! the moment a fetch turns out to be unavoidable.
//!
//! # Per artifact, not per operation
//!
//! An instance's desired state can name files from several providers at once -
//! a CurseForge pack whose owner later installed a Modrinth mod is the ordinary
//! case. So the question this trait answers is asked of one file's
//! [`ProviderFileIdentity`] and not of the operation, which is what keeps one
//! provider's secret from being offered to another provider's CDN.

use std::fmt;
use std::sync::Arc;

use ephervex_download::AuthenticatedSource;

use crate::recipe::ProviderFileIdentity;

/// A provider credential that was needed and could not be obtained.
///
/// Opaque on purpose. Deciding that a key is absent, unreadable, or refused is
/// the provider layer's job and its vocabulary lives above this crate, so the
/// typed failure travels through as its own error rather than being flattened
/// into a download failure that a frontend could not route anywhere useful. The
/// layer that produced it can take it back out with [`Self::into_inner`].
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct CredentialUnavailable(Box<dyn std::error::Error + Send + Sync + 'static>);

impl CredentialUnavailable {
    #[must_use]
    pub fn new(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self(Box::new(source))
    }

    /// Rewraps an error taken back out by [`Self::into_inner`] and not
    /// recognized, so a caller that could not name it can put it back rather
    /// than replacing it with wording of its own.
    #[must_use]
    pub fn from_boxed(source: Box<dyn std::error::Error + Send + Sync + 'static>) -> Self {
        Self(source)
    }

    /// The provider layer's own error, for a caller that can name its type.
    #[must_use]
    pub fn into_inner(self) -> Box<dyn std::error::Error + Send + Sync + 'static> {
        self.0
    }
}

/// Supplies the credential one artifact's download may carry.
///
/// Consulted only for an artifact that is about to be fetched over the network.
/// An artifact already in the verified shared cache is never the subject of a
/// call, which is what makes a cached instance independent of provider
/// configuration: see [`ArtifactFetch`](crate::ArtifactFetch).
///
/// Answering `Ok(None)` means "this file's source publishes its artifacts
/// openly", which is the right answer for most of them and the only answer for
/// a file whose identity names no provider.
///
/// Deliberately synchronous. Every implementation's work is reading a secret
/// store, which is a blocking call the operations that need it already make
/// this way, and an async trait object here would buy nothing but a boxed
/// future per artifact.
pub trait ArtifactCredentials: fmt::Debug {
    /// The credential this file's provider requires, if it requires one.
    ///
    /// The identity is the durable provider fact recorded on the recipe entry.
    /// Nothing else - not the address, not the filename, not the format the
    /// pack was imported from - may be used to decide this.
    fn credential_for(
        &self,
        identity: Option<&ProviderFileIdentity>,
    ) -> Result<Option<Arc<AuthenticatedSource>>, CredentialUnavailable>;

    /// What this provider means by refusing an authenticated request with
    /// `status`, when it means anything at all.
    ///
    /// A provider that serves its artifacts only to a credential also refuses a
    /// wrong one, and it does so with an HTTP status rather than with anything
    /// this crate could recognize. The status alone proves nothing - a `403` is
    /// a perfectly ordinary answer from a CDN that never saw a credential - so
    /// this is asked only when all of the following held for the failed request:
    /// the file's identity named a provider, [`Self::credential_for`] issued a
    /// credential for it, and the address was one that credential was actually
    /// sent to.
    ///
    /// `None` keeps the failure an ordinary download failure, which is the right
    /// answer for a status the provider does not use to refuse credentials.
    fn rejected(
        &self,
        identity: Option<&ProviderFileIdentity>,
        status: u16,
    ) -> Option<CredentialUnavailable>;
}

/// The supplier for an operation whose artifacts all come from sources that
/// publish them openly.
///
/// Says "no credential applies here" rather than "no credential was wired up",
/// so a caller that could encounter an authenticating provider is expected to
/// supply a real implementation instead of this one.
#[derive(Debug, Clone, Copy, Default)]
pub struct OpenArtifacts;

impl ArtifactCredentials for OpenArtifacts {
    fn credential_for(
        &self,
        _identity: Option<&ProviderFileIdentity>,
    ) -> Result<Option<Arc<AuthenticatedSource>>, CredentialUnavailable> {
        Ok(None)
    }

    fn rejected(
        &self,
        _identity: Option<&ProviderFileIdentity>,
        _status: u16,
    ) -> Option<CredentialUnavailable> {
        // Unreachable in practice: nothing here issues a credential, so no
        // request made under this supplier can have carried one.
        None
    }
}
