//! What kind of player identity an account is.
//!
//! # Why this is an enum and not a boolean
//!
//! The two kinds are not "authenticated" and "not authenticated" on a scale.
//! They are different things with different capabilities, and the difference
//! has to survive all the way into the launch pipeline: a Microsoft account
//! carries a session token and may join online-mode servers, an offline account
//! carries nothing and may not. A `bool`, or a token field left empty, would let
//! every consumer forget the distinction by writing `if` and getting the common
//! case right. A non-exhaustive enum makes the compiler ask.
//!
//! # Why it lives in `ephervex-model`
//!
//! It has two consumers that cannot see each other: `ephervex-storage` records
//! it in `config.toml`, and `ephervex-auth` decides what a session made from it
//! can do. Storage must not depend on the authentication crate, so the shared
//! vocabulary belongs here — the same argument that moved `JavaOverride` and
//! `JvmSettings` here in Java runtime support.
//!
//! The *derivation* of an offline account's UUID does not live here. That is
//! `ephervex-auth`'s `offline` module: it needs a hash implementation, and this
//! crate holds types rather than algorithms.

use std::fmt;

use serde::{Deserialize, Serialize};

/// How a player identity was established.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AccountKind {
    /// A real Microsoft account that completed the Xbox/Minecraft token chain.
    /// Owns the game, has a session token, and may join online-mode servers.
    Microsoft,
    /// A local identity with a chosen name and no credential of any kind.
    ///
    /// Useful because local reproduction servers commonly use
    /// `online-mode=false`: reproducing a bug does not require proving who you
    /// are to anybody.
    Offline,
}

impl Default for AccountKind {
    /// Microsoft, because a row written before offline accounts existed is a
    /// Microsoft account, and a missing `kind` in `config.toml` must read as
    /// the thing it was rather than as the newer option.
    fn default() -> Self {
        Self::Microsoft
    }
}

impl AccountKind {
    /// A stable identifier for `config.toml`, logs, and `--json` output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Microsoft => "microsoft",
            Self::Offline => "offline",
        }
    }

    /// The `ephervex-i18n` message id naming this kind to a user.
    #[must_use]
    pub const fn message_id(self) -> &'static str {
        match self {
            Self::Microsoft => "account-kind-microsoft",
            Self::Offline => "account-kind-offline",
        }
    }

    /// Whether this identity was proved to Mojang.
    ///
    /// The single question every "can this do X" check should be phrased in
    /// terms of, rather than each caller re-deriving it from the variant.
    #[must_use]
    pub const fn is_authenticated(self) -> bool {
        matches!(self, Self::Microsoft)
    }

    /// Whether a credential for this kind belongs in the keyring.
    ///
    /// False for offline accounts because there is no secret. An offline account
    /// has nothing to protect,
    /// so it needs no store, and a user with no Secret Service at all can still
    /// create and remove one.
    #[must_use]
    pub const fn has_stored_credential(self) -> bool {
        matches!(self, Self::Microsoft)
    }
}

impl fmt::Display for AccountKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn an_offline_account_is_never_authenticated() {
        // The property the whole type exists to guarantee. If this ever becomes
        // true, an offline identity could be handed to an online-mode server.
        assert!(!AccountKind::Offline.is_authenticated());
        assert!(AccountKind::Microsoft.is_authenticated());
    }

    #[test]
    fn an_offline_account_has_no_credential_to_store() {
        // Offline accounts have no credential material to
        // nothing to store, which is a stronger position than a laxer store.
        assert!(!AccountKind::Offline.has_stored_credential());
        assert!(AccountKind::Microsoft.has_stored_credential());
    }

    #[test]
    fn a_row_with_no_kind_reads_as_the_kind_that_predates_the_field() {
        assert_eq!(AccountKind::default(), AccountKind::Microsoft);
    }

    #[test]
    fn the_serialized_form_is_stable_and_lowercase() {
        // It is written into `config.toml`; renaming it silently would orphan
        // every existing account row.
        assert_eq!(
            serde_json::to_string(&AccountKind::Microsoft).unwrap(),
            "\"microsoft\""
        );
        assert_eq!(
            serde_json::to_string(&AccountKind::Offline).unwrap(),
            "\"offline\""
        );
        assert_eq!(AccountKind::Offline.as_str(), "offline");
    }

    #[test]
    fn the_two_kinds_have_distinct_message_ids() {
        assert_ne!(
            AccountKind::Microsoft.message_id(),
            AccountKind::Offline.message_id()
        );
    }
}
