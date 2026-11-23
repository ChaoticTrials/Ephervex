# ADR 0053 - Storage cleanup removes only unreferenced, regenerable entries after a confirmed preview

**Status:** Accepted (builds on ADR 0011 and ADR 0043)

## Context

The shared stores under the cache root hold everything a launch can download or
derive (ADR 0011). They grow without bound, and a user needs to reclaim space
from them without risking an instance, a server, or anything a host owns.

## Decision

**Only the shared stores are in scope.** Downloaded files, installers, asset
objects and indexes, libraries, and unpacked natives are the only things a plan
can name. Instances, servers, mods, worlds, configuration, snapshots, and
recovery state are never listed, and nothing outside the stores can be removed.

**Unused means no installed host names it.** Every client and server, persistent
or temporary, is read from its canonical record. A client's game version
resolves, from the version metadata already on disk, to the jar, libraries,
natives, logging configuration, asset index, and the asset objects that index
lists. Age is never evidence. Where what a host needs is not recorded, the
entry is kept and the reason is reported: libraries and installers while a
Forge or NeoForge host exists, everything while a host's version metadata or
asset index is present and unreadable, and a cached file that is also hard
linked inside a host.

**A cleanup is a plan, then a confirmed application.** The plan lists kinds,
counts, sizes, the largest entries, and why each kind is disposable. Applying
accepts only a plan, takes the cache lock exclusively (so it refuses while a
launch, an installation, or a running game holds the cache), decides again from
the references at that moment, and removes only entries still unused and
unchanged since the plan. A partial result names what was removed, what was
not and why, and counts freed bytes only for entries that are gone. Repeating
a cleanup is always safe.

**The user chooses among the kinds a plan lists.** A frontend may narrow the
plan it showed to some of its kinds. Narrowing only drops entries, so the
narrowed plan is still one core made, names nothing the review did not show,
and is applied and re-decided exactly as the whole plan would be.

**Files the user supplied by hand are removed only on request.** A file adopted
because its provider refuses to serve it costs a manual download to get back,
so it is never counted as reclaimable and never part of a default cleanup. The
plan lists the ones no host names as their own kind, which a frontend leaves
unselected until the user chooses it. Removing one also removes its provenance
marker. The same withholding reasons still apply first, so such a file is
offered only when nothing else keeps it. The byte budget of the download stores
still never collects them.

**Integrity and recovery use evidence only.** Entries named by their digest are
read and compared with it. Files an installed version's metadata lists are
compared with the digest it states. Natives and loader libraries are reported
as not covered. Recovery fetches only the damaged or missing files through the
ordinary download path, which sets aside a mismatching file and publishes the
verified replacement atomically, so a failed fetch leaves the file absent and
the entry is never presented as healthy.

**The unconditional clear is gone.** The Storage page offers a review of this
cleanup and nothing that removes without one. The byte budget's collection of
the download stores stays a separate soft policy.

## Consequences

- With a loader host installed, libraries and installers are kept even if
  unused, until a per-host record of them exists.
- After a cleanup or recovery the Disk Usage totals are marked out of date.
