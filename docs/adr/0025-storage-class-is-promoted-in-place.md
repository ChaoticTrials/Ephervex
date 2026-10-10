# ADR 0025 - An instance becomes persistent by moving, not by being copied

**Status:** Accepted

## Context

ADR 0006 gives the operating system the lifecycle of a temporary instance and
says the launcher never deletes one on its own. It carves out exactly one
exception: explicit, user-initiated deletion. That carve-out has a twin nobody
wrote down, and the product said so in its own copy for a while -- "converting
an existing instance in place is not available".

The workflow that wants it is the one this launcher exists for. A bug report is
imported into the temp root, the bug is reproduced, and only then does the
instance turn out to be worth keeping: it now has the crash report, the tuned
`options.txt`, the half-bisected `mods/` directory, and a linked server with a
world in it. At that moment the user wants *this* instance to survive a reboot.

Duplication was the only thing on offer and it answers a different question. A
copy is a new instance: new id, new name, no launch history, none of the linked
servers, and two directories where the user wanted one. Everything that made
the original the evidence is left in a directory the operating system is about
to clear.

There are two smaller problems underneath. The temporary root is normally a
different filesystem from the data root, because `std::env::temp_dir()` is
usually tmpfs, so a move is not a rename in the ordinary case. And
`ephervex-storage`'s existing tree copy deliberately does not follow symlinked
directories, which is right for a duplicate and wrong for a move: a dedicated
server's `libraries` *is* a link into the shared library store, so copying a
server that way produces a server with no libraries.

## Decision

An instance's storage class can be promoted in place, from temporary to
persistent, as one explicit user-initiated operation.

**Promotion preserves identity, and that is what distinguishes it from
duplication.** The id, the name, the retained recipe, the launch history and the
linked-server relationships are the same afterwards. Only `root` and `storage`
change. A user who wanted a second instance already has a way to ask for one.

**It is one-way.** Demotion would hand a user's instance back to a cleanup
process they do not control, and nothing in the workflow asks for it. A
persistent instance that has outlived its usefulness is deleted, which is
already explicit and already reversible up to the moment it runs.

**The move is a rename where a rename works, and a symlink-preserving copy
where it does not.** Crossing a filesystem falls back to walking the tree:
files by content, directories recursed, and symlinks recreated as symlinks
rather than followed or dropped. A link's target lies outside the tree being
moved, so recreating it verbatim is what keeps it pointing where it pointed.
The source is removed only after the move is recorded.

**An interrupted promotion is resumable rather than wedged.** A copy is made
into a hidden staging directory beside the destination, flushed, and renamed
into place in one step, so the destination path only ever holds a complete
instance. The staging directory's name is derived from the instance id, so a
retry finds what an interrupted attempt left there and replaces it. A directory
at the destination itself that names the same instance is a copy an earlier
attempt finished but could not record, while the instance still stands at its
source, and is replaced. Anything else there is a conflict and is refused. A
move holds the instance's host claim for as long as it runs, so a second move
or a snapshot restore of the same instance is refused rather than replacing or
reading a copy that is still being made. The claim is taken before the check
that the game is stopped, and a launch takes the same claim while it reads the
instance's record and takes its launch lock. A launch therefore either finishes
taking its lock before the check, and the move is refused, or is refused itself
until the move is over. Storage clears the launch locks of an instance it is
about to move only after finding that no launch holds it, under the same guard
that decides who holds the instance. Reconciliation skips a staging
directory whose claim another process holds, because that move is still
running. A move neither writes nor clears the incomplete marker, and refuses an
instance that carries one: it is an interrupted snapshot restore's, and
recovering it after the move would put the pre-restore metadata back over the
move's.

**Reconciliation adopts a moved root instead of forgetting the record.** A
temporary record whose root has vanished is normally expected cleanup, and
dropping it also unlinks its servers. Before treating it that way,
reconciliation checks whether the instance's files are standing, complete, at
the persistent path for that same id. If they are, they were moved, and the
record is corrected rather than deleted. Without this, an interrupted promotion
would cost the instance its linked servers on the next start. A staging
directory is never adopted; reconciliation reports it and leaves it alone.

**Linked servers are a per-server choice, not a cascade.** A server has its own
storage class and its own world. Every temporary linked server is offered,
selected by default, because a persistent client whose server disappears on the
next reboot is a surprise; but leaving one temporary is a legitimate answer and
the operation does not decide it. A server left unselected is not touched and
not checked.

**The client moves first, and a server that fails to move is reported rather
than rolled back.** Undoing a completed multi-gigabyte move because a later one
failed costs more than saying which one failed, and the client is what the user
asked about.

## Consequences

- ADR 0006 is qualified, not overturned. The operating system still owns every
  temporary instance's lifecycle, and the launcher still deletes nothing on its
  own. Promotion is an explicit exit from that lifecycle, in the same way
  explicit deletion is an explicit exit from it.
- Duplication keeps its meaning, and the two operations now differ visibly:
  a duplicate reports the symlinked folders it could not copy, and a promotion
  has nothing to report because it copies none of them by value.
- The tree relocation is a second primitive beside the tree copy rather than a
  flag on it. They disagree about symlinks for good reasons on both sides, and
  a shared implementation would have to be told which set of reasons applies.
- Every test harness puts all of the roots under one directory, so the rename
  path is the only one integration tests reach. The copy fallback is the path
  real users take and is tested directly.
- Whether a mod is turned off travels with the instance, because the file does.
