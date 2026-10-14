# ADR 0002 - SQLite index with per-instance metadata

**Status:** Accepted

## Context

The launcher needs to list instances and servers quickly, filter them, and
record launch history. A database is the natural fit. But a database is also a
single file that can be lost, corrupted, or left behind when a user copies an
instance directory to another machine - and instances are the thing users
actually care about.

## Decision

Use SQLite (via `rusqlite`, `bundled` feature) as the **index**, and treat it as
a cache of authority rather than the authority itself.

Every instance additionally owns a launcher-written metadata file:

```
instance-root/           server-root/
├──.launcher/           ├──.launcher/
│   ├── instance.json    │   ├── server.json
│   ├── source/          │   ├── source/
│   └── warnings.json    │   └── warnings.json
└── minecraft/           └── server/
```

The database lives at `$XDG_STATE_HOME/ephervex/launcher.sqlite`.

Schema changes go through an ordered migration runner keyed on SQLite's
`PRAGMA user_version`. `CREATE TABLE IF NOT EXISTS` is not a migration strategy
.

## Consequences

- Losing the database is recoverable: scanning the persistent instance root for
  valid `.launcher/` metadata rebuilds the index.
- Two sources of truth must be kept consistent. Writes go to the metadata file
  first, then the index, so a crash between them leaves a re-indexable instance
  rather than an index entry pointing at nothing.
- `instance.json` and `server.json` are the commit points. If only the
  directory sync after their rename fails, the new file is already visible, so
  the update counts as committed: the index is still updated and the failure is
  reported as a warning on the committed result instead of an error. The same
  holds when the index write itself fails after the file was published; the
  next reconciliation adopts the canonical record. The CLI (including
  `--json`) and the GUI report these warnings for the client changes a user
  asks for.
- The index row can lag the metadata file after an index write failed behind a
  committed update. A change to an existing record therefore starts from the
  metadata file when the root exists, never from the index row, so it cannot
  write older fields back over a committed one. Every reader whose result
  depends on a client's Minecraft version, loader, overrides, or pack-mod
  opt-in reads the same way (launch and Java planning, duplication, loader and
  mod planning, diagnosis, snapshots, and the checks tied to a linked server),
  so none of them plans from a selection the user already replaced. The one row
  that outranks the file is a newer one flagged as having a missing root:
  changes made while the root was absent reached only the index, so reads and
  reconciliation both treat that row as current when the root returns. Such a
  change to a persistent client or server flags its row itself; it does not
  wait for a reconciliation pass to notice the missing root first.
- An indexed root whose metadata names a different record, as in a copied
  directory, refuses changes and moves with an error that names the root. The
  record can still be forgotten with its files kept, which does not use that
  metadata. Deleting its files is refused, because the directory is not
  verifiably the host's.
- `bundled` is used because the target machine has no system SQLite development
  package, and pinning the library version removes a class of "works on my
  distro" failures.
- Pack contents must never be able to overwrite `.launcher/` during extraction
. That is enforced in `ephervex-pack`.
