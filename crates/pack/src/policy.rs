//! The rules an import is run under.
//!
//! Bundled into one value rather than threaded through as three arguments,
//! because they are all answers to the same question — how far does the
//! launcher trust this file — and because a caller that has to assemble them
//! individually is a caller that can quietly assemble a weaker set.
//!
//! Every field has a production default. The reason they are settable at all is
//! that a provider brings its own answers: Modrinth publishes a list of
//! permitted download hosts, CurseForge will resolve addresses through its own
//! API and needs a different list, and an integration test needs to point the
//! downloader at a loopback server without weakening what ships.

use ephervex_download::DomainAllowlist;

use crate::archive::ExtractionLimits;
use crate::detect::PackFormat;

/// What an import is permitted to do.
#[derive(Debug, Clone)]
pub struct PackPolicy {
    /// Bounds on what the archive may expand into.
    pub limits: ExtractionLimits,
    /// The hosts a pack's download addresses may name.
    ///
    /// Never a wildcard and never derived from the pack: an untrusted archive
    /// does not get to widen the set of machines the launcher will talk to.
    pub allowlist: DomainAllowlist,
}

impl PackPolicy {
    /// The policy for a Modrinth `.mrpack`: Modrinth's own published host list
    /// and the default extraction limits.
    #[must_use]
    pub fn modrinth() -> Self {
        Self {
            limits: ExtractionLimits::default(),
            allowlist: DomainAllowlist::modrinth_pack_defaults(),
        }
    }

    /// The policy for one recognized format.
    ///
    /// CurseForge and Prism resolve to an empty allowlist rather than to
    /// Modrinth's: their importers are separate consumers, and a format whose rules
    /// have not been decided yet should permit nothing rather than inherit
    /// somebody else's answer.
    #[must_use]
    pub fn for_format(format: PackFormat) -> Self {
        match format {
            PackFormat::ModrinthPack => Self::modrinth(),
            PackFormat::CurseForgePack | PackFormat::PrismExport => Self {
                limits: ExtractionLimits::default(),
                allowlist: DomainAllowlist::empty(),
            },
        }
    }
}

impl Default for PackPolicy {
    fn default() -> Self {
        Self::modrinth()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn the_modrinth_policy_permits_modrinths_own_hosts_and_nothing_else() {
        let policy = PackPolicy::modrinth();
        assert!(policy.allowlist.permits_host("cdn.modrinth.com"));
        assert!(!policy.allowlist.permits_host("evil.example"));
    }

    #[test]
    fn a_format_with_no_importer_yet_permits_no_host_at_all() {
        let policy = PackPolicy::for_format(PackFormat::CurseForgePack);
        assert_eq!(policy.allowlist.hosts().len(), 0);
    }
}
