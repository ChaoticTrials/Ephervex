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
  next reconciliation adopts the canonical record.
- `bundled` is used because the target machine has no system SQLite development
  package, and pinning the library version removes a class of "works on my
  distro" failures.
- Pack contents must never be able to overwrite `.launcher/` during extraction
. That is enforced in `ephervex-pack`.
