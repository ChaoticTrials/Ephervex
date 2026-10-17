# ADR 0045 - A server installs catalog mods into its own recipe

**Status:** Accepted

## Context

A server can be linked to a client, and ADR 0021 synchronizes a mod the client
chose onto its linked servers. A server that is wanted for its own sake, with no
linked client or one the developer does not want to touch, had no way to take a
mod from a catalog. The only route was to install on a client first, which
changes a client that was meant to stay as it is.

Catalogs describe a release's support for each side, and the values are not all
answers. `unsupported` says the author cannot run the mod there, `required` and
`optional` say it can, and `unknown` is the author saying nothing. A search that
treats `unknown` as `required` puts jars on a server on the strength of
silence, and one that treats it as `unsupported` hides mods that work.

A server's mod set also has a second kind of fact. A required dependency can be
declared client-only while the mod that needs it is not: the mod cannot load
without it, so the server needs the jar whatever its author wrote. A recipe
entry whose server side is `unsupported` does not count as installed on the
server, so recording the jar as declared would make the server's mod list,
verification, repair, and dependency graph ignore a file that is in `mods/`.

## Decision

**A server is its own install target.** Searching, planning, and installing take
a host that is either a client or a server. For a server the target is the
server's own Minecraft version and loader, the installed set is what the
server's own recipe installs on the server, and both are read from that server
alone. A linked server's recipe starts as a copy of its client's, so an entry
it records only for the client has no jar on the server: a plan installs it
there rather than counting it as present, and a confirmation the server never
gave is still asked for. A linked client's recipe,
`mods/`, and records are never read or written. A server with no recipe gets one
describing what it already is, the same as a hand-made client.

**Exact selections are never substituted.** A release that does not fit the
server's Minecraft version or loader, or that is declared client-only, is
refused with its reason. With no version named, the newest compatible release
for the server is chosen and pinned. A Vanilla server has no loader to install
into and is refused.

**Search leaves client-only mods out, upstream and locally.** A server search
asks Modrinth to exclude projects declaring no server support, then both
catalogs drop any hit whose newest fitting release is declared unsupported on a
server, using the release's own declaration before the project's. CurseForge
has no such filter, so a page can come back short, and the next offset follows
the page the provider returned. Showing incompatible results widens the query
instead of filtering: client-only mods are returned and marked, and cannot be
installed on a server.

**Undeclared server support is confirmed, not assumed.** A hit whose effective
server side is `unknown` is kept and labeled as not stated. Installing it, or
installing a mod that needs it, requires an explicit confirmation naming each
such mod, and the confirmation is recorded as the explicit choice for that file.
`optional` and `required` are shown as the catalog declared them and nothing
stronger.

**A client-only required dependency is installed, with a warning.** The plan
carries a typed warning naming the dependency and the mod that requires it.
The entry keeps the catalog's `unsupported` declaration and also records that it
was installed on the server anyway, so every reader that asks whether a file is
installed on the server agrees. Linked synchronization keeps dropping such
dependencies.

**What a server installed, it can change and remove.** Changing a mod's
version, updating it, and removing it take the same host as installing, and
resolve against the server's own target and recipe. A server made here rather
than reconstructed from a pack has no declaration to preserve, so its mods are
the user's in the sense of ADR 0032: removing one takes it out of the recipe,
with the dependencies nothing else needs, instead of putting the jar aside to be
restored. A server built from a pack keeps its declaration until the user turns
on the same setting ADR 0032 gives a client,
`ServerRecord::allow_pack_mod_version_changes`, from the server's Settings tab;
from then on its mods are the user's in the same sense. The setting lives in the
server's metadata only, because the launcher index takes no further migration
before 1.0.0 (ADR 0033), so reading or reconciling the index never overrides it.
A version change that would add a mod whose server support the catalog never
stated is refused rather than confirmed in passing: installing that mod from the
catalog is where the confirmation is asked.

**Downloads are unchanged.** Artifacts come from the shared verified cache, with
the same hash verification, destination safety, and rollback as a client
install. A release its author does not allow to be downloaded stays an
actionable manual acquisition, with the sides the catalog declared. A mod
operation never starts, stops, or restarts a server; a running server receives
the change for its next start (ADR 0021).

## Consequences

The linked client and a server can hold different mods without either
overwriting the other's record, and a server created from a client later keeps
what it installed itself.

A short CurseForge page is a visible cost of not filtering client-only files
upstream. Fetching more pages to fill it would multiply requests for a
provider that is already rate limited.

The extra recorded fact is additive and optional, so recipes written before it
read as they always did (ADR 0033).
