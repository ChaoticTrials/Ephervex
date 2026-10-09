# ADR 0019 - An instance has one mod list, with ownership and identity as separate typed facts

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
found would produce records for jars the launcher did not install - the thing
ADR 0012 and ADR 0013 both exist to prevent. Widening "managed mod" to cover
entries with partial identity would let a mod with no provable release be
handled by code that assumes one, and the first such assumption to reach the
recipe would write a version nothing can look up.

There is also a question about what a pack's download addresses prove. Modrinth
serves its artifacts from `cdn.modrinth.com/data/<project>/versions/<version>/`,
and the pack tooling writes those addresses from the catalog entries it
resolved. The ids in them are catalog data, not a naming convention - but the
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

Provider display metadata - titles, attribution, declared compatibility,
artwork - is decoration applied over durable state by an explicit enrichment
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

## Amendment: a jar the user supplies over a recorded file

A recipe entry says which bytes belong at a path. When the user adds a jar whose
name an entry already claims, leaving the entry in place makes it false:
verification calls the user's file corrupt, repair and the restore-files fix
overwrite it with the recorded release, and a server built from the recipe ships
that release instead.

The entry therefore goes with the file it described. This covers the launcher's
own records (a mod chosen in the browser, a dependency, an optional dependency)
and the adopted record of an identified hand-placed jar, whose old identity and
hashes describe bytes that are no longer there. The user's jar is hand-placed
like any other, and the next refresh identifies it again. The same holds when
the recorded jar had been put aside, because restoring it would overwrite the
file the user just supplied. The question put to the user says the record is
dropped.

A pack's declaration is the exception. It is the pack's and stays exactly as
imported, so a jar that would sit over a declaration this host installs is
refused rather than leaving the declaration describing a different file. A
declaration the host never installs, such as a server-only mod seen from a
client, puts nothing at the path there and does not block. The refusal points to
the instance's independent mod changes (ADR 0032), which move a pack's mod to
another release through the catalog.

## Consequences

- An imported pack's mods appear in the same list, in the same shape, as ones
  installed afterwards, and are visibly the pack's.
- The resolver now knows about a pack's projects, so browsing for a mod the pack
  already contains reports the conflict instead of installing a second jar for
  the same project.
- A frontend consumes one snapshot from the application service. It does not
  read recipes, call a provider, or decide what a file name implies.
- A mod the instance is recorded as having but whose jar is absent is reported
  as such rather than dropped - deleting a jar to bisect a crash is the workflow
  this launcher exists for, and the instance still wants the mod.
- Nothing enumerates `mods/`. A jar with no provenance is still left alone,
  still not adopted, and still not deleted.
- Version changes, updates, and removal remain unimplemented. This makes the
  state trustworthy first; those operations mutate it, and mutating a list that
  was not yet honest about what it contained would have been the worse order.

## Amendment: a hand-placed jar is listed, and still not owned

"Nothing enumerates `mods/`" was the wrong half of the rule to keep. A jar
somebody copies in loads into the game, so a list that leaves it out is a list
that disagrees with the folder about what is running - which is exactly the
invisibility this ADR set out to end, arrived at from the other side.

Listing now includes them, under `ModOrigin::HandPlaced`. Everything else the
rule protected is unchanged and is now enforced by that origin rather than by
absence: `MutationRestriction::HandPlaced` refuses every mutation, so nothing
here replaces or deletes a file the launcher did not install.

Identification is separate from listing, because listing has to remain a
directory read: it hashes and asks catalogs, so it runs only on an explicit
refresh. A jar a catalog recognizes is then recorded, with its identity, as
`client` required and `server` undeclared - the existing way of saying nobody
has decided whether it belongs on a server, which makes creating one ask rather
than assume. Its origin stays `HandPlaced`, so being recorded does not make it
the launcher's to change. A jar nothing recognizes is listed and left
unrecorded, and a later refresh may still identify it.

## Amendment: the list shows the folder, and a hand-placed jar is the user's to delete

Two of the decisions above no longer hold, and both changed for the same
reason: the list is what a user acts on, so a row they cannot act on is worse
than no row at all.

**A recorded mod whose jar is absent is no longer listed.** The original
reasoning - the instance still wants the mod, so say so - was right about the
recipe and wrong about the list. Deleting a jar to bisect a crash is this
launcher's central workflow, and it left behind a row saying "File missing from
mods folder" for every jar deleted that way: a warning for the thing the user
had just deliberately done. The record still says the instance wants the mod, so
`ModPresence::Missing`, `ManagedModSnapshot::missing()`, repair, and server
reconstruction are all unchanged. Only the list stops drawing it, and the
message is gone from the product.

Nothing is lost silently, because the absent records are still readable:
`App::absent_recorded_mods` names them, and anything built out of the instance
asks. Duplicating a client offers to keep or drop them, defaulting to keep.

**A hand-placed jar may be deleted, by the user, by name.** Everything that
made it "not owned" still holds - no automatic operation installs, updates,
replaces, retires, or collects it - and `MutationRestriction::HandPlaced` still
refuses every mutation. Deleting the file is not one of those: it resolves no
dependencies, retires nothing, and asks no catalog. It is a separate permission,
`ModMutability::can_delete_file`, true for that origin and no other, and it
takes the adopted recipe entry with it so the record cannot outlive the file.

Ephervex also now writes into `mods/` on request, when a user drops jars onto
the list. Such a jar is deliberately **not** recorded: nothing about a file
somebody hands over says what project it is, and recording a guess is what this
ADR exists to prevent. It is hand-placed however it arrived, and the same
digest lookup identifies it on the next refresh.

One consequence taken deliberately: pack-declared entries and identified
hand-placed ones answer a lookup by project identically, because identity is not
ownership. `ephervex-pack`'s mutation planner now checks origin as well, so a
provider removal or replacement cannot reach a jar the user placed. That rule
was stated in its module documentation before and was not enforced.

## Amendment: a pack's mod can be put aside, and put back

Two rules above were drawn tighter than the reasoning behind them justified.

**`can_delete_file` is no longer "that origin and no other".** The permission is
about whether taking the jar away makes a claim the launcher cannot support, and
for a pack's own file it does not. Deleting it resolves no dependencies, retires
nothing, asks no catalog, and leaves the pack's declaration exactly as imported.
`MutationRestriction::PackOwned` still refuses every mutation, so nothing here
lets a provider operation reach a pack file or lets the recipe claim a release
the pack never shipped. What it allows is putting one mod aside to find which
one crashes, which is the workflow this launcher exists for and which the pack's
own mods were excluded from.

The two origins differ in what goes with the file. A hand-placed jar takes its
adopted record with it, because that record only ever remembered what a digest
lookup found and describes nothing once the jar is gone. A pack file keeps its
declaration, because that declaration is what makes the deletion reversible.

**A recorded mod whose jar is absent is listed again when this list is what
deleted it.** The earlier amendment removed those rows because a row saying
"File missing from mods folder" was a warning for something the user had just
deliberately done, by hand, outside the application. A mod deleted *through the
list* is not that: the list offered the action, so it owes the user a way back.
Such a row stays, says it was deleted and that the pack still lists it, and
offers to restore it. Everything else is unchanged: a jar deleted outside
Ephervex still leaves no row, because nothing here can promise to restore what
it did not remove.

Restoring is deliberately not a mutation. It resolves nothing and asks no
catalog; it writes the file the recipe already describes, which is what lets it
work for a pack file served from a mirror no catalog can name.

## Amendment: any jar may be put aside, and taking one takes what needed it

`can_delete_file` has been widened twice already, each time by the same
argument, and each time it stopped one origin short of where the argument
reaches. It is about whether taking the jar off disk makes a claim the launcher
cannot support. It never does.

A dependency was the last origin still refused, on the grounds that its presence
belongs to whatever required it and that removing the root is what collects it.
That is right about *removal* and says nothing about deletion. A library is very
often the one jar a bisect needs gone, and "remove the four mods that wanted it,
then put them back afterwards" is not the same operation.

So deleting the jar is available for every origin. The two permissions now say
genuinely different things rather than one being a weaker form of the other:

- `can_remove` - the instance should stop containing this mod. A provider
  operation, addressed by project, which resolves orphans, retires files and
  rewrites desired state. Still refused for a dependency and for a pack file.
- `can_delete_file` - the jar should not be on disk right now. Addressed by
  path, resolves nothing, asks no catalog.

What survives the file still differs by origin, and that is the part that
matters. A hand-placed jar takes its adopted record with it, because that record
only ever said what a digest lookup found. Everything else keeps its record, so
the row offers to put the file back, a server built from the recipe is still
complete, and a repair still fetches it.

**Deleting one jar takes the jars that needed it.** The set is the one the
dependency graph proves for disabling (ADR 0034), so deleting and disabling can
never disagree about consequences, and it is applied in the same order: what
everything needs goes last, so an interrupted run never leaves a mod on disk
with its library already gone. A frontend that cannot ask refuses instead and
changes nothing.

## Amendment: a mod the pack ships inside itself is the pack's

A Modrinth or CurseForge pack has two ways to give an instance a mod. It can
declare one in its index, naming a download address and the hashes to verify it
against, and it can carry the jar in its own override tree, where the index
names the tree by its directory prefix and never lists what is inside it.

Only the first was recorded. The second was extracted into `mods/` and then
read back by the directory scan the amendment above introduced, which correctly
found a jar nothing had recorded and reported it as `HandPlaced` - "somebody
copied this in themselves". Every consequence followed from there: the row said
"Added by hand", identification by digest answered from whichever catalog
happened to recognize the file rather than from the pack, and adoption kept the
origin, so the wrong answer was permanent.

A mod the pack ships inside its override tree is `ModOrigin::PackOverride`. The
pack owns it, Ephervex placed it by extracting that tree and may place it again,
and it is part of the instance's desired state, so a reinstall restores it and
the list is complete. Its path is collected while the layer is extracted, which
is the only moment the layer's contents are known, and recorded once every layer
is known.

Layers extract over a declared file at the same path, later layer winning, and
the recipe follows the bytes that win. A path shipped by several layers is one
entry, required on every side a layer applies to. When layers win on every side
the declared file applies to, the entry stops being the declared release: its
hashes, addresses, and identity are dropped, because they describe bytes that
are not installed. When the declared file would still be installed on some side
(a client-only layer over a file declared for both sides, say), one entry would
have to describe different bytes per side, and the import is refused instead of
recording either one.

It is **not** `PackDeclared`, and the difference is not cosmetic. A declared
file is fetched and verified against published hashes; this one has no address
and never will, because the pack shipped the bytes rather than a reference to
them. So it is kept out of every plan that downloads - an entry with no address
reaching one is a failure to report, not a fact to absorb - and it reads under
its own badge, because "the pack ships this" and "the pack points at this" are
different things to know about a jar that misbehaves.

The per-instance version opt-in of ADR 0032 does not apply to it, and not
because it is more restricted but because there is nothing for that opt-in to
protect. The opt-in guards a *declaration*: the pack said which project and
which release, that statement is a historical record, and moving away from it is
the deliberate act the setting asks about. A shipped jar carries no such
statement - no project, no release, nothing the recipe recorded from the pack.
So once a digest lookup has found which project it is, it is managed like any
other identified mod, with no setting in the way.

Removing it from the desired state stays refused. The pack's own override layer
would place the jar again on the next reapply, and a list that had dropped it
would be wrong until then. Deleting the file is still offered, because putting a
jar aside to find which one crashes is what this launcher is for.

The same rule covers a declared file that is not a mod, such as a resource pack,
a shader pack, or a config file. The recipe records no entry for a shipped file
that is not a mod jar, because the layer itself is what places it, so a declared
entry the layers win over on every side is dropped rather than converted. Left
in place it would keep hashes for bytes that are not installed: verification
would report the pack's own file as corrupt and repair would download the
declared file over it. A layer that covers only some of the sides the declared
file applies to is refused for the same reason as above.

## Consequences

- The recipe format is version 5. An entry recording a shipped mod carries no
  hashes, no download address and no provider identity, which is exactly what
  the pack said about it.
- An instance imported by an earlier build records nothing about which of its
  jars came out of an override tree, and cannot be repaired in place: the client
  layer is deliberately not retained, because it is already in the game
  directory, so for those jars the information is not on disk anywhere.
  Recovering it by inspecting `mods/` would be the enumeration this ADR exists
  to refuse. Those instances keep reading their shipped jars as hand-placed
  until the pack is imported again, which is cheap in a launcher built around
  disposable environments.
