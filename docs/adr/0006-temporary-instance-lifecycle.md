# ADR 0006 — The OS owns temporary-instance cleanup

**Status:** Accepted

## Context

Ephervex exists to make instances disposable. The tempting implementation is for
the launcher to manage that lifecycle itself: delete on exit, or expire after N
days.

Both are wrong for the actual workflow. A developer reproduces a bug, quits the
game, and *then* wants the crash report, the log, and often another launch with
one config changed. A launcher that deletes on exit destroys the evidence at
exactly the moment it becomes interesting.

## Decision

Temporary instances live under `std::env::temp_dir()/ephervex-<uid>/instances/<uuid>/`
and the operating system owns their cleanup.

Specifically:

- **Do not** delete a temporary instance when Minecraft exits.
- **Do not** implement launcher-managed expiry as default behavior.
- Detect index entries whose temporary root has vanished and remove them during
  startup reconciliation.

If a user's system does not clear its temp directory on reboot, that is
acceptable — the instance simply persists until it does.

## Consequences

- A vanished temporary root is a normal, expected event, not an error. A
  vanished *persistent* root is the opposite: it keeps a recoverable warning and
  is never silently deleted. `StorageClass::vanishing_is_expected` encodes this
  distinction in the model so no caller has to remember it.
- Startup reconciliation is a correctness requirement, not a nicety: without it
  the instance list fills with entries pointing at directories that a reboot
  removed.
- Shared assets and libraries must not live inside instance roots, or a reboot
  would force re-downloading gigabytes. They live under the cache root
  (ADR 0003) and instances reference them.
- Explicit user-initiated deletion is still supported. This ADR is about what
  happens *without* the user asking.
