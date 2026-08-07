//! Where an instance lives.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Whether an instance lives under the OS temporary root or under the
/// configured persistent root.
///
/// Temporary is the product's defining default. The OS owns cleanup of
/// temporary roots; the launcher never deletes them on game exit and never
/// implements its own expiry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageClass {
    /// Beneath `std::env::temp_dir()/<APP_ID>/`.
    Temporary,
    /// Beneath the configured persistent instance root.
    Persistent,
}

impl StorageClass {
    /// The product default for newly created and imported instances.
    #[must_use]
    pub const fn default_for_new_instance() -> Self {
        Self::Temporary
    }

    /// Whether a vanished root for this class is an expected outcome.
    ///
    /// A missing temporary root means the OS cleaned it up and the index entry
    /// should be dropped. A missing persistent root is unexpected and must be
    /// surfaced as a recoverable warning instead.
    #[must_use]
    pub const fn vanishing_is_expected(self) -> bool {
        matches!(self, Self::Temporary)
    }
}

impl Default for StorageClass {
    fn default() -> Self {
        Self::default_for_new_instance()
    }
}

impl fmt::Display for StorageClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Temporary => "temporary",
            Self::Persistent => "persistent",
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn new_instances_default_to_temporary() {
        assert_eq!(StorageClass::default(), StorageClass::Temporary);
    }

    #[test]
    fn only_temporary_roots_may_vanish_silently() {
        assert!(StorageClass::Temporary.vanishing_is_expected());
        assert!(!StorageClass::Persistent.vanishing_is_expected());
    }
}
