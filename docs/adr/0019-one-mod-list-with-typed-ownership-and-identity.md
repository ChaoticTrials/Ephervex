# ADR 0019 — An instance has one mod list, with ownership and identity as separate typed facts

**Status:** Accepted

## Context

ADR 0013 made a mod installed through the browser an entry in the same recipe
as an imported pack's files, so one file describes the whole of what an instance
is supposed to contain. It also defined a *managed mod* as an entry naming a
provider, a project, and a version, because those are what make an entry
re-resolvable.

That definition was right for the mod browser and wrong for a user. A Modrinth
pack index identifies its files by path, hashes, and download address and names
no project, so an imported pack's mods had no identity, were therefore not
managed mods, and were absent from the only list the launcher offers of an
instance's mods. An instance built from a two-hundred-mod pack showed none of
them, and any mod added afterwards appeared to be the only one there.

Two bad repairs suggest themselves. Enumerating `mods/` and hashing what is
found would produce records for jars the launcher did not install — the thing
ADR 0012 and ADR 0013 both exist to prevent. Widening "managed mod" to cover
entries with partial identity would let a mod with no provable release be
handled by code that assumes one, and the first such assumption to reach the
recipe would write a version nothing can look up.

There is also a question about what a pack's download addresses prove. Modrinth
serves its artifacts from `cdn.modrinth.com/data/<project>/versions/<version>/`,
and the pack tooling writes those addresses from the catalog entries it
resolved. The ids in them are catalog data, not a naming convention — but the
same URL position held a human version number in packs built by older tooling,
and a file may legitimately be served from one of the other permitted hosts.

## Decision

An instance has one list of its mods, and ownership and identity are separate
typed facts about each of them.

- `RecordedMod` is every recipe entry that lands in the game's `mods/`
  directory and that this client installs. A pack's config files are not mods,
  and an optional mod nobody selected was never installed, so neither appears.
- Ownership stays `ModOrigin`, which already distinguishes a pack-declared file
  from a user-selected mod, a required dependency, and a selected optional one.
- Identity becomes `ModIdentity`, with three states: the exact catalog version
  is known, the project is known but the release is not, or nothing is known.
  A mod is listed in all three; only the first supports being looked up again.
  Keeping this on its own axis is what stops "we know what this is" and "we
  know which build this is" from being one answer.
- `ManagedMod` keeps its narrower meaning and its role: it is what the resolver
  keys on, and it still requires a complete identity.

Provider identity for an imported pack is **reconstructed** from the pack's own
download addresses, never inferred from a file name. Reconstruction accepts only
the catalog's own host, and accepts a version segment only when it is shaped
like a catalog id, so a pack that recorded `1.4.2` there keeps its project and
stays honestly release-less. A file served from a permitted mirror yields no
catalog identity at all. The same reconstruction runs when a stored recipe is
read, so an instance imported before identities were recorded gains them
deterministically without being re-imported and without a destructive migration;
it fills gaps and never overwrites an identity an installation recorded.

Provider display metadata — titles, attribution, declared compatibility,
artwork — is decoration applied over durable state by an explicit enrichment
step. It is served through the provider metadata cache, so opening the list
repeatedly makes no request and an earlier response is still shown when the
provider is unreachable. Nothing in the launcher's behavior reads it, no failure
to fetch it can remove a mod from the list or change what the launcher believes
is installed, and a mod it does not cover is shown under its file name with no
version claimed.

Artwork has a cache store of its own rather than sharing the content-addressed
downloads store. A provider publishes no hash for an icon, so there is nothing
to verify, and an unverifiable file does not belong among files whose identity
is a checked digest. It is still held to the transport and host policy an
untrusted pack's downloads are held to, plus a size ceiling.

## Consequences

- An imported pack's mods appear in the same list, in the same shape, as ones
  installed afterwards, and are visibly the pack's.
- The resolver now knows about a pack's projects, so browsing for a mod the pack
  already contains reports the conflict instead of installing a second jar for
  the same project.
- A frontend consumes one snapshot from the application service. It does not
  read recipes, call a provider, or decide what a file name implies.
- A mod the instance is recorded as having but whose jar is absent is reported
  as such rather than dropped — deleting a jar to bisect a crash is the workflow
  this launcher exists for, and the instance still wants the mod.
- Nothing enumerates `mods/`. A jar with no provenance is still left alone,
  still not adopted, and still not deleted.
- Version changes, updates, and removal remain unimplemented. This makes the
  state trustworthy first; those operations mutate it, and mutating a list that
  was not yet honest about what it contained would have been the worse order.

## Amendment: a hand-placed jar is listed, and still not owned

"Nothing enumerates `mods/`" was the wrong half of the rule to keep. A jar
somebody copies in loads into the game, so a list that leaves it out is a list
that disagrees with the folder about what is running — which is exactly the
invisibility this ADR set out to end, arrived at from the other side.

Listing now includes them, under `ModOrigin::HandPlaced`. Everything else the
rule protected is unchanged and is now enforced by that origin rather than by
absence: `MutationRestriction::HandPlaced` refuses every mutation, so nothing
here replaces or deletes a file the launcher did not install.

Identification is separate from listing, because listing has to remain a
directory read: it hashes and asks catalogs, so it runs only on an explicit
refresh. A jar a catalog recognizes is then recorded, with its identity, as
`client` required and `server` undeclared — the existing way of saying nobody
has decided whether it belongs on a server, which makes creating one ask rather
than assume. Its origin stays `HandPlaced`, so being recorded does not make it
the launcher's to change. A jar nothing recognizes is listed and left
unrecorded, and a later refresh may still identify it.
