# ADR 0020 - Mod mutation is one undoable transaction, and ownership decides what may be mutated

**Status:** Accepted

## Context

ADR 0013 made an installed mod an entry in the retained recipe, and ADR 0019
made every mod of an instance one list with typed ownership and identity. Both
were about describing state. Changing it raises problems neither had to answer.

Installing only ever adds, so its worst failure is "nothing happened". Changing
a mod's version and removing one both take something away, and the states in
between are worse than that:

- both the old jar and the new one in `mods/`, so the loader sees two builds of
  one mod and the game does not start;
- neither there, so the instance lost a mod nobody asked to remove;
- files changed and the recipe not, so a server rebuilt from the recipe no
  longer matches the client.

The case that catches a naive implementation is a project that keeps one file
name across releases. Writing the new jar first destroys the old one, and there
is then nothing to restore if a later step fails.

A second problem has no filesystem in it at all. A mod dragged in as a
dependency two operations ago carries no record of *who* wanted it: the recipe
says `RequiredDependency` and stops there. So "is this still needed after the
mod that wanted it changes" cannot be looked up.

And a third: not every mod is the user's to change. A pack's own mod is part of
an imported pack's declaration, an automatic dependency's version belongs to
whatever requires it, and a mod with no proven release cannot be told apart from
a mod that is already current.

## Decision

**One declarative transaction.** A caller states which mods the instance must
have and which projects it must not; nothing states an order. The operation runs
`plan → fetch and verify → retire → write → publish recipe`, and outgoing jars
are *moved* into `.launcher/retired/` inside the same instance rather than
deleted. That makes every step before the recipe is published exactly undoable:
new files are removed, retired files are moved back. Retirement precedes writing,
which is what makes a same-file-name replacement safe - nothing is lost by the
earlier move, because by then every replacement artifact is verified in the
shared cache. "Ready" means verified, not written.

Retired files are deleted only after the recipe is on disk, and that deletion is
allowed to fail: a leftover in launcher-owned scratch is invisible to the game,
whereas a leftover in `mods/` is a crash. Each mutation clears what an
interrupted predecessor left, so the directory cannot grow across operations.

A replacement updates an entry **in place**, so one project can never acquire two
recorded versions, and it keeps the ownership already recorded rather than
whatever the caller supplied. An update must not promote a dependency into a
root.

**Orphans are proven, not assumed.** After a change, the instance's roots - mods
a human chose by name, plus optional dependencies they ticked - are re-resolved,
each pinned to the release it is actually at, and the union of what they reach is
what the instance still requires. An automatic dependency outside that union has
no remaining owner and may go. A catalog that cannot answer means nothing is
collected: keeping a dependency nothing needs costs a megabyte, deleting one
something needs costs the instance. A pack's own mods are not resolved, because
an import records every file it declares and never runs the resolver, so no
`RequiredDependency` entry can belong to one.

**The resolver learns one parameter, not a second implementation.** Installing
must refuse to move a mod that is already there; changing a version is a request
to do exactly that. `InstalledPolicy` chooses between them, and the plan reports
replacements separately so a caller knows what to retire.

**Ownership is the policy.**

| Origin | Change version | Update | Remove |
|---|---|---|---|
| `UserSelected` | yes | yes | yes |
| `SelectedOptionalDependency` | no | no | yes |
| `RequiredDependency` | no | no | no |
| `PackDeclared` | no | no | no |

An optional dependency sits where it does because both halves of its name are
true: the resolver picks its version, so moving it independently could break what
asked for it; a human ticked it, so unticking it is theirs and nothing else may
collect it. Anything whose exact installed release is unproven is refused too -
without it there is no way to tell an update from a no-op, or to know what is
being replaced.

**Pack-owned mutation is refused for now, and says so.** A recipe entry records
one version. Overwriting a pack file's version would make the launcher claim the
pack shipped a release it never shipped; keeping both needs a second version per
entry plus an answer to which one a reconstructed server gets. That is a change
to how packs are recorded rather than a mutation feature, so this build refuses
with a typed `PackOwned` reason and still shows the mod's available releases.
Refusing honestly is preferred to a local-deviation model built in a hurry.

**Update is a choice of version.** "Newest compatible for this exact instance",
never "newest published". Already current is a typed result that rewrites
nothing; already current *with the jar missing* is a repair, because a recorded
mod whose file was deleted while bisecting is not a no-op.

**Loader builds move within one family only.** Forge to Forge, NeoForge to
NeoForge, from the loader catalog's own list for the instance's exact Minecraft
release. Crossing a family changes what the instance is and invalidates every mod
in it. Nothing is recorded until the build installs, and installed mods are
re-checked but never changed: a loader change and a mod update are two decisions,
and resolving one with the other moves two variables in an instance that exists
to isolate one.

## Consequences

- A failed download, a failed write, or a failed recipe publication all leave the
  instance exactly as it was, with its old jars present and its recipe describing
  them.
- Upgrades, downgrades, and repairs are one operation with one set of failure
  paths, rather than three that can each be wrong differently.
- A frontend asks what a mod allows and renders that. It does not re-derive
  policy from an origin and an identity, and it cannot offer an action the core
  would refuse.
- Removing a mod can be shown honestly before it happens, including the case
  where the launcher cannot prove which dependencies are still needed.
- Pack customization stays an unimplemented feature with a named reason rather
  than a half-built one. When it arrives it will need a way to record a local
  deviation from a pack's declaration, and this ADR is what it supersedes.
- Linked server synchronization is untouched. A loader-build change updates both
  the instance record and the recipe's target so the two cannot disagree; the
  pack's name, version, and every file it declared are left exactly as imported.
