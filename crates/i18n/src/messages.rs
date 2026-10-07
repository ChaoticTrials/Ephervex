//! Compile-time-checked accessors for the messages storage and registry support needs.
//!
//! `i18n-embed-fl`'s [`fl!`] macro validates the message id *and* its argument
//! names against the fallback resource while the crate is being compiled - a
//! typo is a build failure rather than a placeholder string a user eventually
//! reports. The catch is that the macro reads the `i18n.toml` of whichever
//! crate invokes it, so only this crate can use it.
//!
//! That is why the plural-sensitive reconciliation summaries live here as
//! functions instead of being assembled with [`crate::lookup_args`] in
//! `ephervex-core` or the CLI: the counting is the part most likely to be got
//! wrong, so it is the part that earns the compile-time check.

use i18n_embed_fl::fl;

use crate::LANGUAGE_LOADER;

/// "Removed N temporary instances whose directories no longer exist."
///
/// A vanished temporary root is the expected result of the OS clearing its
/// temporary directory, so this reads as information rather than as a failure
/// (ADR 0006).
#[must_use]
pub fn removed_stale_temporary(count: usize) -> String {
    fl!(
        LANGUAGE_LOADER,
        "reconcile-removed-stale-temporary",
        count = count
    )
}

/// "N persistent instances are missing their directories."
///
/// The opposite case: never silently deleted, always surfaced.
#[must_use]
pub fn missing_persistent(count: usize) -> String {
    fl!(
        LANGUAGE_LOADER,
        "reconcile-missing-persistent",
        count = count
    )
}

/// The next step offered alongside [`missing_persistent`].
#[must_use]
pub fn missing_persistent_action() -> String {
    fl!(LANGUAGE_LOADER, "reconcile-missing-persistent", "action")
}

/// "Recovered N instances found on disk but missing from the index."
///
/// The payoff of ADR 0002: losing the database costs an index rebuild, not the
/// instances themselves.
#[must_use]
pub fn reindexed(count: usize) -> String {
    fl!(LANGUAGE_LOADER, "reconcile-reindexed", count = count)
}

/// "Finished moving N instances into your persistent instance folder."
///
/// A promotion interrupted between moving the files and recording where they
/// went. The record follows the files rather than being dropped, which is what
/// keeps an instance's linked servers attached to it (ADR 0025).
#[must_use]
pub fn relocated(count: usize) -> String {
    fl!(LANGUAGE_LOADER, "reconcile-relocated", count = count)
}

/// "N instance directories were left unfinished ..."
///
/// A creation or import that was interrupted before it published. The
/// directories are never indexed and never deleted automatically.
#[must_use]
pub fn unfinished_instances(count: usize) -> String {
    fl!(
        LANGUAGE_LOADER,
        "reconcile-unfinished-instances",
        count = count
    )
}

/// The next step offered alongside [`unfinished_instances`].
#[must_use]
pub fn unfinished_instances_action() -> String {
    fl!(LANGUAGE_LOADER, "reconcile-unfinished-instances", "action")
}

/// "Corrected N index entries from the metadata stored with their files."
///
/// Canonical metadata beside the files wins over a disagreeing index copy.
#[must_use]
pub fn index_repaired(count: usize) -> String {
    fl!(LANGUAGE_LOADER, "reconcile-index-repaired", count = count)
}

/// "Rolled back N interrupted snapshot restores ..."
#[must_use]
pub fn restores_rolled_back(count: usize) -> String {
    fl!(
        LANGUAGE_LOADER,
        "reconcile-restores-rolled-back",
        count = count
    )
}

/// "N instances or servers have unfinished changes left in their directories."
#[must_use]
pub fn interrupted_hosts(count: usize) -> String {
    fl!(
        LANGUAGE_LOADER,
        "reconcile-interrupted-hosts",
        count = count
    )
}

/// The next step offered alongside [`interrupted_hosts`].
#[must_use]
pub fn interrupted_hosts_action() -> String {
    fl!(LANGUAGE_LOADER, "reconcile-interrupted-hosts", "action")
}

/// "N instances or servers have metadata that cannot be read."
#[must_use]
pub fn persistence_unhealthy(count: usize) -> String {
    fl!(
        LANGUAGE_LOADER,
        "reconcile-persistence-unhealthy",
        count = count
    )
}

/// The next step offered alongside [`persistence_unhealthy`].
#[must_use]
pub fn persistence_unhealthy_action() -> String {
    fl!(LANGUAGE_LOADER, "reconcile-persistence-unhealthy", "action")
}

/// "N instances are claimed by more than one directory."
///
/// No directory is chosen automatically.
#[must_use]
pub fn client_conflicts(count: usize) -> String {
    fl!(LANGUAGE_LOADER, "reconcile-client-conflicts", count = count)
}

/// The next step offered alongside [`client_conflicts`].
#[must_use]
pub fn client_conflicts_action() -> String {
    fl!(LANGUAGE_LOADER, "reconcile-client-conflicts", "action")
}

/// "N servers are claimed by more than one directory."
///
/// No directory is chosen automatically.
#[must_use]
pub fn server_conflicts(count: usize) -> String {
    fl!(LANGUAGE_LOADER, "reconcile-server-conflicts", count = count)
}

/// The next step offered alongside [`server_conflicts`].
#[must_use]
pub fn server_conflicts_action() -> String {
    fl!(LANGUAGE_LOADER, "reconcile-server-conflicts", "action")
}

/// Shown when the index already agreed with the filesystem.
#[must_use]
pub fn nothing_to_reconcile() -> String {
    fl!(LANGUAGE_LOADER, "reconcile-nothing-to-do")
}

/// The label for a storage class, e.g. in an instance list.
#[must_use]
pub fn storage_class(class: &str) -> String {
    match class {
        "temporary" => fl!(LANGUAGE_LOADER, "storage-class-temporary"),
        _ => fl!(LANGUAGE_LOADER, "storage-class-persistent"),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn reconciliation_summaries_use_the_right_plural_form() {
        assert_eq!(
            removed_stale_temporary(1),
            "Removed 1 temporary instance whose directory no longer exists."
        );
        assert_eq!(
            removed_stale_temporary(3),
            "Removed 3 temporary instances whose directories no longer exist."
        );
        assert_eq!(
            missing_persistent(1),
            "1 persistent instance is missing its directory."
        );
        assert_eq!(
            missing_persistent(2),
            "2 persistent instances are missing their directories."
        );
        assert_eq!(
            reindexed(1),
            "Recovered 1 instance found on disk but missing from the index."
        );
    }

    #[test]
    fn a_missing_persistent_root_offers_a_recovery_step_rather_than_a_deletion() {
        let action = missing_persistent_action();
        assert!(action.contains("kept the record"));
        assert!(!nothing_to_reconcile().is_empty());
    }

    #[test]
    fn every_attention_state_has_a_summary_and_a_next_step() {
        assert_eq!(
            client_conflicts(1),
            "1 instance is claimed by more than one directory."
        );
        assert_eq!(
            server_conflicts(2),
            "2 servers are claimed by more than one directory."
        );
        assert_eq!(
            interrupted_hosts(1),
            "1 instance or server has unfinished changes left in its directory."
        );
        assert_eq!(
            persistence_unhealthy(3),
            "3 instances or servers have metadata that cannot be read."
        );
        for action in [
            client_conflicts_action(),
            server_conflicts_action(),
            interrupted_hosts_action(),
            persistence_unhealthy_action(),
        ] {
            assert!(!action.is_empty());
        }
    }

    #[test]
    fn storage_classes_have_user_facing_labels() {
        assert_eq!(storage_class("temporary"), "Temporary");
        assert_eq!(storage_class("persistent"), "Persistent");
    }
}
