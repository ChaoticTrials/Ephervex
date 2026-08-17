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

    /// The policy for a CurseForge export.
    ///
    /// A CurseForge manifest names no download address at all — the addresses
    /// come back from the API — so this list is what the *API's* answers are
    /// checked against. It is deliberately not Modrinth's: an address is
    /// permitted here because CurseForge delivers its own files from it, not
    /// because some other provider vouches for the host.
    #[must_use]
    pub fn curseforge() -> Self {
        Self {
            limits: ExtractionLimits::default(),
            allowlist: DomainAllowlist::curseforge_artifact_defaults(),
        }
    }

    /// The policy for one recognized format.
    ///
    /// Prism resolves to an empty allowlist: an unsupported format permits
    /// nothing rather than inheriting another provider's policy.
    #[must_use]
    pub fn for_format(format: PackFormat) -> Self {
        match format {
            PackFormat::ModrinthPack => Self::modrinth(),
            PackFormat::CurseForgePack => Self::curseforge(),
            PackFormat::PrismExport => Self {
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
    fn an_unsupported_format_permits_no_download_hosts() {
        let policy = PackPolicy::for_format(PackFormat::PrismExport);
        assert_eq!(policy.allowlist.hosts().len(), 0);
    }

    #[test]
    fn a_curseforge_pack_permits_curseforges_own_delivery_hosts_and_not_modrinths() {
        let policy = PackPolicy::for_format(PackFormat::CurseForgePack);
        assert!(policy.allowlist.permits_host("mediafilez.forgecdn.net"));
        assert!(!policy.allowlist.permits_host("cdn.modrinth.com"));
        assert!(!policy.allowlist.permits_host("evil.example"));
    }
}
