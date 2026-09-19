# ADR 0035 - Local snapshots use copy-on-write clones

**Status:** Accepted

## Context

An Ephervex instance or local server contains more than a mod list. Its world,
configuration, logs, crash reports, disabled jars, exact loader selection, and
retained pack recipe can all be necessary to reproduce a development state.
Copying only selected files produces a backup that looks useful but cannot put
the host back into that state.

Mutable Minecraft files cannot use hard links. The game and mods rewrite many
of them in place, which would also rewrite a hard-linked snapshot. Server
library directories are symlinks into the shared library store, so following a
link would copy shared storage and omitting it would restore an unusable server.

## Decision

A snapshot is a whole local safety copy stored at
`<host>/.launcher/snapshots/<snapshot-id>/`. It captures the game directory,
retained source recipe, and canonical host metadata. Symlinks are recreated
with their original target text and never followed.

Regular files use the Linux `FICLONE` operation when the filesystem supports
it. A failed reflink attempt falls back to an ordinary byte copy. Hard links
are never used. Snapshot metadata records the least efficient method used and
the apparent byte size so a frontend can explain likely disk use.

Launcher transaction state, launch locks, incomplete markers, isolation state,
and snapshot history are excluded. A duplicated instance also excludes the
snapshot directory, so duplication cannot recursively copy its history.

Creation and restoration are refused while the client or server may be
running. Retention is unbounded and user-managed. No cache budget or automatic
collector may remove snapshots.

A restore replaces captured parts rather than merging them. By default it
first creates a safety snapshot with origin `before_restore`. Restoration uses
a versioned journal under launcher-owned staging. The journal contains only
typed, fixed host parts rather than arbitrary paths. Each original part moves
inside the transaction before its replacement is published. Reconciliation
rolls a recognized interrupted restore back in full; unfamiliar or ambiguous
state remains untouched for manual review.

## Consequences

The restore journal records a durable commit decision before cleanup. Restart
discards unpublished staging, rolls back interrupted publication, and completes
cleanup of committed restores without replacing their live state again.

Cloning checks cancellation between directory entries and byte-copy chunks.
Special files and symlinked capture roots are refused; links inside captured
directories are preserved without following their targets.

- A filesystem without reflink support uses real space for every snapshot.
- Deleting a host deletes the snapshots inside it.
- Broken symlinks remain broken symlinks, which preserves the captured shape.
- Snapshots are local recovery state, not a pack export format.
