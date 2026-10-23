# ADR 0048 - Instance organization is presentation state outside host metadata

**Status:** Accepted

## Context

A collection of instances needs to be searchable, filterable and orderable, and
a developer wants the instances they keep returning to within reach. That takes
favorites, lightweight groups and per-host icons. All of it only changes how
hosts are presented. None of it may change an instance's identity, its storage
path, its linked servers, its snapshots or any other developer state.

The obvious homes for it are the host's own `instance.json` and the SQLite
index, and neither works:

- A snapshot restore and a mod-isolation cancel rewrite every field of
  `instance.json` except `root` and `storage`, so presentation kept there would
  roll back with the files.
- `instance.json` is copied verbatim into diagnostic bundles (ADR 0038), which
  would publish presentation choices the user never meant to share.
- `list_clients()` reads index rows, which mirror the records column by column,
  and the index schema stays at its initial migration until 1.0.0 (ADR 0033).
  A field missing from the index would make reconciliation repair the row on
  every start (ADR 0002).
- A favorite, a group or an icon must work while a host is moving, restoring or
  running, so changing one must not need the host claim a record write takes.

## Decision

**One launcher-level file holds all of it.** `organization.json` lives in the
data directory beside the icon store. It holds the favorite client and server
ids, the group definitions, each client's group, and the icon each client and
server uses. It is its own format at version 1 (ADR 0033): every field is
additive and defaults when absent, fields this build does not know survive a
rewrite, and the file is written compactly through the atomic publish helper.
Reading and changing it is serialized across processes by a file lock, so two
windows or commands never lose each other's changes.

**The records do not change.** `ClientRecord`, `ServerRecord`, the SQL
migrations and the metadata files are untouched. A snapshot restore, a
promotion, a storage move, an isolation cancel, a linked pair or a launch
cannot reach organization data, and promotion keeps the instance id so its
favorite, group and icon follow it. A diagnostic bundle never carries the file
or the icons, because it is built only from host-owned files and logs.

**A damaged file never stops the launcher.** A file that cannot be parsed, that
is too large, or that a newer build wrote is moved aside under a name nothing
else uses and reported. Every change first keeps the file as it was in
`organization.old.json`, and that copy takes the damaged file's place when it is
usable, so a damaged file costs at most its latest change. Only when the copy is
unusable too does the launcher start with an empty organization. Icons the copy
names are kept until it is replaced. A damaged file is never overwritten
silently.

**Stale entries go away on the next write.** Entries naming an instance or
server that no longer exists are dropped by the next change, and lookups are by
id, so they never resolve in between. This covers deleted hosts and temporary
roots reconciliation forgot. Nothing is pruned when the index cannot be read,
because an unreadable index says nothing about which hosts exist. A group with
no members stays until the user deletes it.

**Groups are flat labels.** A client is in at most one group or none. A group
has a stable id, so renaming it never touches an assignment. Names are checked
by the same rules as instance names and are unique without regard to case.
Deleting a group leaves its members ungrouped. A new group takes the color slot
fewest groups use and keeps it, so a rename or another group never repaints it.
Groups apply to clients only and are never nested. The list stays one flat list
of cards, with the group shown as a label and offered as a filter, so the fixed
card height that the scroll arithmetic relies on does not change.

**Icons are untrusted input.** The user picks a local PNG or JPEG file. Core
refuses anything over a size limit before reading it, then decodes within
dimension and pixel bounds of its own, checks the format from the bytes,
scales it down to at most 256 pixels and re-encodes it as PNG. The bounds admit
an ordinary photo or screenshot, wider than artwork's, because only the scaled
copy is kept and the full decode is held only briefly. The result is
stored once by the hash of its bytes in the icon directory. The organization
file stores only that hash, never the original path. An icon neither the file
nor its previous copy names is removed in the locked update that leaves it
unnamed. No icon is
ever fetched from a remote address. A host without an icon, or whose icon file
is missing or will not load, shows the symbol of its loader, so the same host
always looks the same.

**Search, filters and order are one pure implementation.** Core owns what
matches and in what order, so the desktop list, the host picker and the command
line agree. Conditions combine, and an empty search matches everything. The
order is always deterministic: the chosen key, then newest first, then the id.
Minecraft versions rank by the cached version manifest when one is available
offline and by a version-aware text comparison otherwise, never by asking the
network. Only the chosen order is remembered between runs, in the display
settings, and an unrecognized stored value falls back to the default. Filters
and the search start cleared. Whether a client is paired is derived from the
linked pairs (ADR 0040), so each client falls into exactly one of paired,
linked servers without a pair, or no linked servers.

**The workspace is unaffected.** The host list's filters narrow that list
only. A split workspace and its host picker are given every host (ADR 0039),
and the picker ranks favorites first within its existing running-first order.

## Consequences

- Organization survives every operation that rewrites a host, and a host's
  files can be restored, moved or deleted without it.
- Organization is per installation. It is not exported with an instance, and a
  duplicated instance inherits the original's group and icon but not its
  favorite.
- Two small files need care when a data directory is copied by hand: the
  organization file and the icon directory belong together.
- A newer organization file is set aside by an older build instead of being
  misread, at the cost of that build starting from the previous copy, or with
  nothing when that copy is newer too.
- An icon stays on disk for one change longer than anything names it, because
  the previous copy may still name it.
