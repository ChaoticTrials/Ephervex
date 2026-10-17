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
retained source recipe, the kept copies of put-aside mods that recipe promises
back (ADR 0019), and canonical host metadata. Symlinks are recreated
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
inside the transaction before its replacement is published. A restore holds
the host's claim, shared with storage moves (ADR 0025), from before its safety
snapshot until it finishes or is rolled back, so a move and a restore of one
host exclude each other. Reconciliation rolls a recognized interrupted restore
back in full only when it can take that claim, which shows the restore's
process is gone; a host whose claim is held is left entirely alone. On a
filesystem with no file locking the claim only separates threads of one
process, so a restore running in another launcher cannot be told from an
abandoned one there. Unfamiliar
or ambiguous state remains untouched for manual review.

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
