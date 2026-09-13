# ADR 0021 - Linked mod synchronization pins the root release and resolves dependencies per target

**Status:** Accepted

## Context

A client instance can have several linked servers, and the reason to have one is
to reproduce a bug on both sides of the same build. ADR 0012 made a linked server
reconstruct itself from the client's retained recipe, so the two agree at the
moment the server is created. They stop agreeing the moment anyone installs,
changes, updates, or removes a mod on the client afterwards.

Closing that gap looks like four small features and is really one decision with
several ways to get it wrong.

The first is what release a server gets. "Install this mod on the servers too"
sounds like it means running the same operation against each of them. It does
not: running "the newest release that fits" once per target can produce client
v5, server A v4, and server B v6 from a single click, which is three different
builds of the mod the user is bisecting. Nor can the client's answer simply be
assumed to fit - Ephervex already supports changing a linked server's exact
loader build independently, so a linked server may be on another Minecraft
release or another loader build than the client it came from.

The second is provenance. A server's recipe records what its pack declared, and
ADR 0019 made that declaration a historical record rather than a wish list. A mod
synced months after reconstruction was never in that pack, and writing it in as
though it were would make the launcher claim a release the pack never shipped.

The third is atomicity. ADR 0020 made one instance's mutation one undoable
transaction. Several instances cannot be one transaction - there is no
cross-instance filesystem transaction to have - and imitating one by undoing the
targets that succeeded would destroy working state to make a report look tidier.

The fourth is what a server is allowed to be told about a mod at all. A project
declares its client and server support, and `unknown` is one of the four values
an author may leave in place. Reading it as `required` puts a mod onto a server
on the strength of a value that says the opposite of certainty.

## Decision

**The root release is exact and shared; dependencies are not.** The client
operation settles on one exact provider release - the one the user picked, or the
one an update resolved against the *client's* exact Minecraft version and loader
- and that release is what every selected linked server is evaluated for. No
server runs its own newest-compatible search. Dependencies are the opposite: each
target resolves its own graph against its own release, loader, and installed set,
with the root pinned. A server legitimately needs things a client does not, and
only the explicitly synchronized root has to match.

A release that does not fit a server makes **that server** ineligible, with a
typed reason naming which of Minecraft, loader, or the resolution itself refused
it. Nothing looks for a release that would fit better, and the client operation
proceeds without that target.

**Every server is judged on its own current state.** Its Minecraft release, its
loader build, its runtime state, its link, and its own retained desired state.
The client's compatibility result is never taken as proof for a server.

**Synced mods are recipe entries with their own ownership.** A server keeps the
recipe reconstruction left it, and a synced root is added to it as
`UserSelected`, its dependencies as `RequiredDependency` - the same
provider-neutral ownership a client uses (ADR 0019). Pack-declared entries are
never rewritten, so pack history, later synced roots, and their dependencies stay
three distinguishable things in one list. Ownership is read back from that file,
never reconstructed by looking at `mods/`, so a jar Ephervex did not put there is
never adopted and never removed. Mutation of a pack-owned entry stays refused on
a server exactly as it is on a client.

A reconstructed server's copy of the recipe records the *server's* answers to
decisions, not the client's. An entry whose server side is undeclared is
installed only when a creation confirms it, so inheriting the client's answer
would leave the server's own record claiming a jar it never received.

**Each target is one transaction; the operation is not.** Per target, the ADR
0020 ordering runs unchanged - plan, fetch and verify, retire, write, publish,
discard - against that server's own game directory, scratch, and recipe. A failed
target keeps the state it had. Across targets there is no transaction and none is
claimed: results are typed and per target, keyed by stable server id, and a
partial outcome is reported as a partial outcome. Targets that succeeded stay
succeeded.

**Artifacts come from the shared verified cache.** The client's installed jar is
mutable instance state and is never the source for a server. The same verified
artifact in the launcher-level cache is published independently into each target,
so a root both sides need is fetched once.

**Undeclared server support is not required support.** `required` may preselect a
compatible target; `optional` and `unknown` are offered and never preselected,
and the difference between them is said out loud, because "your choice" and "the
author said nothing" are different statements. `unsupported` is never presented
as a target at all. A server that already manages the root is a separate case: a
version change, a repair, or a removal there is the operation the user just asked
for applied where they already applied it, so such a target is selected by
default whatever the declaration says.

**Everything is revalidated at execution time.** A confirmation can be open while
a server is started, unlinked, deleted, or moved to another loader build, so each
target is planned again from scratch before it is written to, with the same exact
pin. A target that no longer holds is skipped with its reason rather than failing
partway through a write.

**A running server is refused, not stopped.** Changing a live server's mods has
no proven safe mechanism here, and starting, stopping, or restarting a server as
a side effect of a mod operation is not something a mod operation may do. The
target reports that the server must be stopped, and the user retries.

## Consequences

One user action can leave the client and a subset of its servers changed, and
that is reported as exactly what it is. The alternative - refusing the whole
operation because one server is running - would make the common case hostage to
the rare one.

A linked server can drift from its client, and this feature does not hide the
drift. A server on another Minecraft release is simply not a target for a release
that does not support it, and it says so rather than silently receiving something
else.

Nothing here creates a second mod-management system for servers. There is one
mutation transaction, one resolver, one ownership model, and one shared verified
cache; linked synchronization is orchestration above them. A frontend selects
targets and renders results, and resolves nothing itself.

Synchronization needs the catalog. A plan that cannot establish compatibility
leaves the target unverifiable and the server unchanged, rather than reusing a
locally present jar as evidence that a release fits.
