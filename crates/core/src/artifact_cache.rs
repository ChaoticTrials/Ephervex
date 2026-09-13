//! Artifact-cache budget and maintenance, as the frontends see it.
//!
//! `ephervex-download` owns the collector; this module owns its *lifecycle*. It
//! answers three questions a frontend and every cache-changing service need:
//! what the budget is, when maintenance runs, and what protects an operation
//! that is still using the cache.
//!
//! # The lease is the whole safety argument
//!
//! Every service that fetches or consumes reusable artifacts runs inside
//! [`App::with_artifact_cache`]. That takes a shared lease before the work
//! starts, holds it until the work is completely finished - including the copy
//! into an instance, the unpack into a runtime, the spawn of a JVM whose
//! classpath names cache paths - and only then lets maintenance try to run.
//! Because the lease is a kernel advisory lock on a file under the cache root,
//! it holds against a second Ephervex process as much as against a second task
//! in this one.
//!
//! # Maintenance never fails an installation
//!
//! A budget is a housekeeping policy, not a precondition. If collection cannot
//! run — the cache is busy, a store cannot be listed, an entry cannot be removed
//! — the operation's own result stands and the cache stays over budget until the
//! next opportunity. Leaving files behind is always preferable to deleting ones
//! whose status is uncertain.

use ephervex_download::{
    ArtifactCache, ArtifactCacheError, ArtifactCacheLease, ArtifactCacheMaintenance,
    ArtifactCacheUsage, CacheLayout,
};
use ephervex_model::ArtifactCacheLimit;
use tracing::{debug, info, warn};

use crate::app::App;
use crate::error::CoreResult;

/// The reusable artifact cache as a settings screen shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactCacheStatus {
    /// Bytes held by finalized reusable artifacts, by the same eligibility rules
    /// collection uses: if it is counted here, a cleanup can free it.
    pub used_bytes: u64,
    /// How many such entries there are.
    pub entries: u64,
    /// Whether the whole store could be measured. A `false` makes the figures a
    /// lower bound, and a frontend should not present them as exact.
    pub complete: bool,
    pub limit: ArtifactCacheLimit,
}

impl ArtifactCacheStatus {
    /// Whether the cache is over its configured high-water mark right now.
    #[must_use]
    pub const fn is_over_budget(&self) -> bool {
        self.limit.is_exceeded_by(self.used_bytes)
    }
}

impl App {
    /// The configured artifact-cache budget.
    #[must_use]
    pub fn artifact_cache_limit(&self) -> ArtifactCacheLimit {
        self.config().cache.artifact_limit()
    }

    /// The collector for this installation's cache root.
    #[must_use]
    pub fn artifact_cache(&self) -> ArtifactCache {
        ArtifactCache::new(CacheLayout::new(self.paths().cache_dir()))
    }

    /// Measures the reusable artifact cache and reports it beside its budget.
    pub async fn artifact_cache_status(&self) -> CoreResult<ArtifactCacheStatus> {
        let ArtifactCacheUsage {
            bytes,
            entries,
            complete,
        } = self.artifact_cache().usage().await?;
        Ok(ArtifactCacheStatus {
            used_bytes: bytes,
            entries,
            complete,
            limit: self.artifact_cache_limit(),
        })
    }

    /// Changes the budget, persists it, and enforces the new one.
    ///
    /// Enforcement happens here rather than being left for the next download,
    /// because lowering a limit is a user asking for space back now. It is still
    /// best-effort: a busy cache means the setting is saved and the collection
    /// waits, which is the same outcome as any other maintenance attempt that
    /// cannot get the lock.
    pub async fn set_artifact_cache_limit(
        &mut self,
        limit: ArtifactCacheLimit,
    ) -> CoreResult<ArtifactCacheStatus> {
        self.mutate_config(|config| config.cache.set_artifact_limit(limit))?;
        info!(%limit, "set the artifact cache budget");
        self.maintain_artifact_cache().await;
        self.artifact_cache_status().await
    }

    /// Parses and validates a custom budget, then applies it.
    ///
    /// A refused value leaves the stored setting exactly as it was: nothing is
    /// written before the value is known to be representable, so a frontend can
    /// report the error and keep showing the budget that is still in force.
    pub async fn set_custom_artifact_cache_limit(
        &mut self,
        value: &str,
        unit: ephervex_model::ByteUnit,
    ) -> CoreResult<ArtifactCacheStatus> {
        let limit = ArtifactCacheLimit::parse_in_unit(value, unit)?;
        self.set_artifact_cache_limit(limit).await
    }

    /// Removes every reusable artifact the cache safely can.
    ///
    /// Refuses with [`ArtifactCacheError::Busy`] rather than waiting when an
    /// operation still holds a lease, so a frontend reports "the cache is in
    /// use" instead of hanging on another process. The report is truthful about
    /// what is left: entries that could not be removed are counted, and the
    /// remaining usage is not rounded down to zero for tidiness.
    ///
    /// The configured budget is untouched — clearing is not a settings change.
    pub async fn clear_artifact_cache(&self) -> CoreResult<ArtifactCacheMaintenance> {
        let report = self.artifact_cache().clear().await?;
        info!(
            removed = report.removed_entries,
            freed = report.freed_bytes,
            remaining = report.usage_after,
            "cleared the reusable artifact cache"
        );
        Ok(report)
    }

    /// Runs cache-consuming work with the shared lease held, then maintains.
    ///
    /// The lease is taken before `work` is polled and dropped before maintenance
    /// starts, which is the whole ordering the safety argument rests on: while an
    /// operation may still resolve or open a cache path, no collector can unlink
    /// it, and once it cannot, collection is free to run.
    ///
    /// `work`'s result is returned untouched. Maintenance runs whatever that
    /// result was — a failed import leaves cache entries behind exactly like a
    /// successful one — and cannot change it.
    ///
    /// Nesting is safe and expected: creating a server installs a version, and a
    /// launch may install a runtime. Leases are shared, so an inner one is taken
    /// freely; the inner maintenance attempt finds the cache still leased by the
    /// outer operation and defers, and the outer one runs it.
    ///
    /// A service that takes `&mut self` cannot pass a future that borrows it
    /// here. Those take the same three steps in the open — lease, work, drop,
    /// maintain — which is exactly what this does; see [`App::launch`].
    ///
    /// `work` is boxed at the boundary. Installing a pack or reconstructing a
    /// server is a large state machine, and keeping one inline would put its
    /// whole size into every caller's future; a single allocation per operation
    /// is not a cost worth measuring against a download.
    pub(crate) async fn with_artifact_cache<T>(
        &self,
        work: std::pin::Pin<Box<dyn std::future::Future<Output = CoreResult<T>> + '_>>,
    ) -> CoreResult<T> {
        let lease = self.artifact_cache_lease().await?;
        let outcome = work.await;
        drop(lease);
        self.maintain_artifact_cache().await;
        outcome
    }

    /// Takes a shared lease on the reusable artifact cache.
    ///
    /// Held for as long as the caller may still resolve, open, copy, link, or
    /// launch a reusable artifact path.
    pub(crate) async fn artifact_cache_lease(&self) -> CoreResult<ArtifactCacheLease> {
        Ok(self.artifact_cache().lease().await?)
    }

    /// Enforces the budget, reporting trouble instead of propagating it.
    ///
    /// Deliberately returns nothing. There is no caller for whom a maintenance
    /// failure is actionable at the moment it happens, and giving one a `Result`
    /// invites exactly the mistake this is written to prevent: an installation
    /// that finished perfectly well being reported as failed because tidying up
    /// afterwards did not.
    pub(crate) async fn maintain_artifact_cache(&self) {
        let limit = self.artifact_cache_limit();
        if limit.bytes().is_none() {
            return;
        }
        match self.artifact_cache().collect(limit).await {
            Ok(report) if report.removed_entries > 0 => {
                info!(
                    %limit,
                    removed = report.removed_entries,
                    freed = report.freed_bytes,
                    usage = report.usage_after,
                    "collected the reusable artifact cache"
                );
                report_trouble(&report);
            }
            Ok(report) => {
                debug!(%limit, usage = report.usage_after, "artifact cache is within its budget");
                report_trouble(&report);
            }
            Err(ArtifactCacheError::Busy { .. }) => {
                debug!("artifact cache is in use; maintenance deferred");
            }
            Err(error) => {
                warn!(%error, "artifact cache maintenance did not complete");
            }
        }
    }
}

/// Logs what a pass could not do, once, with a bounded sample of paths.
///
/// The paths are cache paths by construction — the collector never looks
/// anywhere else — so they carry no user secret and no credential.
fn report_trouble(report: &ArtifactCacheMaintenance) {
    if report.is_clean() {
        return;
    }
    warn!(
        troubled = report.troubled_entries,
        complete = report.complete,
        sample = ?report.troubles,
        "part of the artifact cache could not be maintained"
    );
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use std::path::PathBuf;

    use ephervex_download::{ContentStore, FileExtension, MaintenanceStage};
    use ephervex_model::{ByteUnit, GIB, Hash, HashAlgorithm, MIB, TIB};
    use ephervex_storage::Paths;

    use super::*;

    fn app_rooted_at(base: &std::path::Path) -> App {
        App::open_at(Paths::rooted_at(base)).expect("app opens against a temporary root")
    }

    /// Publishes `bytes` bytes at a recognizable content address.
    fn publish(app: &App, seed: u8, bytes: usize) -> PathBuf {
        let digest = format!("{seed:02x}").repeat(20);
        let hash = Hash::new(HashAlgorithm::Sha1, &digest).expect("sha1");
        let extension = FileExtension::new("jar").expect("jar");
        let path =
            app.artifact_cache()
                .layout()
                .content(ContentStore::Downloads, &hash, Some(&extension));
        std::fs::create_dir_all(path.parent().expect("shard")).expect("shard");
        std::fs::write(&path, vec![b'a'; bytes]).expect("write");
        path
    }

    #[tokio::test]
    async fn a_fresh_installation_gets_the_ten_gibibyte_default() {
        let base = tempfile::tempdir().expect("tempdir");
        let app = app_rooted_at(base.path());
        assert_eq!(
            app.artifact_cache_limit(),
            ArtifactCacheLimit::Limited(10 * GIB)
        );

        let status = app.artifact_cache_status().await.expect("status");
        assert_eq!(status.used_bytes, 0);
        assert_eq!(status.limit, ArtifactCacheLimit::Limited(10 * GIB));
        assert!(!status.is_over_budget());
        assert!(status.complete);
    }

    #[tokio::test]
    async fn a_custom_budget_is_persisted_and_read_back_from_the_settings_file() {
        let base = tempfile::tempdir().expect("tempdir");
        let mut app = app_rooted_at(base.path());
        for (typed, unit, expected) in [
            ("500", ByteUnit::Gibibytes, 500 * GIB),
            ("1", ByteUnit::Tebibytes, TIB),
            ("4096", ByteUnit::Gibibytes, 4096 * GIB),
        ] {
            let status = app
                .set_custom_artifact_cache_limit(typed, unit)
                .await
                .expect("a representable budget is accepted");
            assert_eq!(status.limit, ArtifactCacheLimit::Limited(expected));

            // Reopened from the file the first application wrote, so this proves
            // persistence rather than an in-memory value.
            let reopened = app_rooted_at(base.path());
            assert_eq!(
                reopened.artifact_cache_limit(),
                ArtifactCacheLimit::Limited(expected),
                "{typed} {unit} must survive a restart"
            );
        }
    }

    #[tokio::test]
    async fn a_refused_budget_leaves_the_stored_one_in_force() {
        let base = tempfile::tempdir().expect("tempdir");
        let mut app = app_rooted_at(base.path());
        app.set_custom_artifact_cache_limit("500", ByteUnit::Gibibytes)
            .await
            .expect("500 GiB");

        for hostile in ["", "lots", "-1", "1.5", "99999999999999999999"] {
            let error = app
                .set_custom_artifact_cache_limit(hostile, ByteUnit::Tebibytes)
                .await
                .expect_err("`{hostile}` must be refused");
            assert_eq!(
                error.message_id(),
                "error-artifact-cache-limit-malformed",
                "`{hostile}`"
            );
        }
        let error = app
            .set_custom_artifact_cache_limit("8388608", ByteUnit::Tebibytes)
            .await
            .expect_err("a budget past the representable range must be refused");
        assert_eq!(error.message_id(), "error-artifact-cache-limit-too-large");

        assert_eq!(
            app.artifact_cache_limit(),
            ArtifactCacheLimit::Limited(500 * GIB),
            "a refusal must not overwrite the budget that is still in force"
        );
        assert_eq!(
            app_rooted_at(base.path()).artifact_cache_limit(),
            ArtifactCacheLimit::Limited(500 * GIB)
        );
    }

    #[tokio::test]
    async fn unlimited_round_trips_and_stops_automatic_collection() {
        let base = tempfile::tempdir().expect("tempdir");
        let mut app = app_rooted_at(base.path());
        let path = publish(&app, 0x21, 3 * 1024 * 1024);
        app.set_artifact_cache_limit(ArtifactCacheLimit::Unlimited)
            .await
            .expect("unlimited");

        assert_eq!(
            app_rooted_at(base.path()).artifact_cache_limit(),
            ArtifactCacheLimit::Unlimited
        );
        app.maintain_artifact_cache().await;
        assert!(path.is_file(), "an unlimited budget collects nothing");

        let status = app.artifact_cache_status().await.expect("status");
        assert_eq!(status.used_bytes, 3 * MIB);
        assert!(!status.is_over_budget());

        let report = app.clear_artifact_cache().await.expect("manual clear");
        assert_eq!(report.removed_entries, 1);
        assert!(!path.exists());
        assert_eq!(
            app.artifact_cache_limit(),
            ArtifactCacheLimit::Unlimited,
            "clearing is not a settings change"
        );
    }

    #[tokio::test]
    async fn lowering_the_budget_enforces_it_immediately() {
        let base = tempfile::tempdir().expect("tempdir");
        let mut app = app_rooted_at(base.path());
        let older = publish(&app, 0x22, 3 * 1024 * 1024);
        let newer = publish(&app, 0x23, 1024 * 1024);
        // Backdate the first so the eviction order is defined rather than
        // dependent on how fast the two writes happened.
        std::fs::File::options()
            .write(true)
            .open(&older)
            .expect("open")
            .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3_600))
            .expect("backdate");

        let status = app
            .set_artifact_cache_limit(ArtifactCacheLimit::Limited(2 * MIB))
            .await
            .expect("lowering the budget");
        assert!(!older.exists(), "the oldest entry goes first");
        assert!(newer.is_file());
        assert_eq!(status.used_bytes, MIB);
        assert!(!status.is_over_budget());
    }

    #[tokio::test]
    async fn an_operation_that_holds_the_cache_defers_maintenance_rather_than_failing() {
        let base = tempfile::tempdir().expect("tempdir");
        let mut app = app_rooted_at(base.path());
        let path = publish(&app, 0x24, 4 * 1024 * 1024);
        app.set_artifact_cache_limit(ArtifactCacheLimit::Limited(MIB))
            .await
            .expect("a budget the cache already exceeds");
        // Enforcement on the settings change removed it; put it back so the
        // deferral has something to leave alone.
        let path = {
            assert!(!path.exists());
            publish(&app, 0x24, 4 * 1024 * 1024)
        };

        let lease = app.artifact_cache_lease().await.expect("lease");
        // The whole point: maintenance is best-effort and silent about a busy
        // cache, so this cannot fail and cannot delete anything.
        app.maintain_artifact_cache().await;
        assert!(path.is_file(), "a leased artifact is untouchable");

        let error = app
            .clear_artifact_cache()
            .await
            .expect_err("a manual clear says so instead of hanging");
        assert_eq!(error.message_id(), "error-artifact-cache-busy");
        assert!(path.is_file());

        drop(lease);
        app.maintain_artifact_cache().await;
        assert!(
            !path.exists(),
            "the deferred pass runs once the lease is gone"
        );
    }

    #[tokio::test]
    async fn work_wrapped_in_a_lease_keeps_its_result_and_gets_the_cache_maintained() {
        let base = tempfile::tempdir().expect("tempdir");
        let mut app = app_rooted_at(base.path());
        app.set_artifact_cache_limit(ArtifactCacheLimit::Limited(MIB))
            .await
            .expect("budget");

        // Stands in for an installation: it publishes an over-budget artifact,
        // sees it in place while its own lease is held, and returns a value.
        let published = app
            .with_artifact_cache(Box::pin(async {
                let path = publish(&app, 0x25, 4 * 1024 * 1024);
                assert!(
                    path.is_file(),
                    "the artifact an operation just fetched is still there while it runs"
                );
                Ok(path)
            }))
            .await
            .expect("the operation's own result is returned untouched");
        assert!(
            !published.exists(),
            "maintenance runs once the operation's lease is released"
        );
    }

    #[tokio::test]
    async fn an_artifact_larger_than_the_whole_budget_still_completes_its_operation() {
        let base = tempfile::tempdir().expect("tempdir");
        let mut app = app_rooted_at(base.path());
        app.set_artifact_cache_limit(ArtifactCacheLimit::Limited(2 * MIB))
            .await
            .expect("budget");

        let outcome: u64 = app
            .with_artifact_cache(Box::pin(async {
                let huge = publish(&app, 0x26, 6 * 1024 * 1024);
                // A soft budget: nothing about the size refuses the operation,
                // and the artifact is usable for as long as it is needed.
                let bytes = std::fs::metadata(&huge).expect("metadata").len();
                assert!(bytes > 2 * MIB);
                Ok(bytes)
            }))
            .await
            .expect("an over-budget artifact does not fail its operation");
        assert_eq!(outcome, 6 * MIB);

        // Afterwards there is no cache state that satisfies a 2 MiB budget while
        // keeping a 6 MiB object, so it is not kept.
        let status = app.artifact_cache_status().await.expect("status");
        assert_eq!(status.used_bytes, 0);
    }

    /// Whether making a directory read-only actually stops this process removing
    /// what is inside it.
    ///
    /// The condition under test is "the launcher tried to remove a file and was
    /// refused". A privileged process is not subject to the mode bits at all, and
    /// neither is a process on a filesystem that does not enforce them, so on those
    /// the barrier this test builds is not a barrier and there is nothing to
    /// observe. Probed rather than inferred from the user id, because privilege is
    /// not the only way it fails to hold.
    #[cfg(unix)]
    fn a_read_only_directory_refuses_this_process() -> bool {
        use std::os::unix::fs::PermissionsExt as _;

        let probe = tempfile::tempdir().expect("tempdir");
        let file = probe.path().join("entry");
        std::fs::write(&file, b"probe").expect("probe file");
        std::fs::set_permissions(probe.path(), std::fs::Permissions::from_mode(0o500))
            .expect("read-only probe directory");
        let refused = std::fs::remove_file(&file).is_err();
        // Restored so the temporary directory can still clean itself up.
        std::fs::set_permissions(probe.path(), std::fs::Permissions::from_mode(0o700))
            .expect("restore the probe directory");
        refused
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn maintenance_that_cannot_remove_an_entry_is_reported_and_not_fatal() {
        use std::os::unix::fs::PermissionsExt as _;

        if !a_read_only_directory_refuses_this_process() {
            eprintln!(
                "skipped: this process can remove files from a read-only directory, \
                 so a refusal cannot be staged for it to report"
            );
            return;
        }

        let base = tempfile::tempdir().expect("tempdir");
        let mut app = app_rooted_at(base.path());
        let stuck = publish(&app, 0x27, 4 * 1024 * 1024);
        app.set_artifact_cache_limit(ArtifactCacheLimit::Unlimited)
            .await
            .expect("unlimited while the shard is made read-only");

        let shard = stuck.parent().expect("shard").to_path_buf();
        std::fs::set_permissions(&shard, std::fs::Permissions::from_mode(0o500))
            .expect("read-only shard");

        let report = app
            .clear_artifact_cache()
            .await
            .expect("a clear that cannot remove everything is not an error");

        // Restored before the assertions, so a failing one still leaves a
        // directory the temporary root can remove.
        std::fs::set_permissions(&shard, std::fs::Permissions::from_mode(0o700))
            .expect("restore the shard");

        assert_eq!(report.removed_entries, 0);
        assert_eq!(
            report.usage_after,
            4 * MIB,
            "the report must not claim the cache is empty"
        );
        assert!(!report.is_clean());
        assert_eq!(report.troubles[0].stage, MaintenanceStage::Removing);
        assert!(stuck.is_file());
    }
}
