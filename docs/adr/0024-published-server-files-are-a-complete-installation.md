# ADR 0024 - Published server files are a complete installation

**Status:** Accepted

## Context

ADR 0012 makes one provider-neutral recipe the pack import boundary, and a
linked server is reconstructed from the client's retained copy of it. That is
the only thing available when a pack format declares nothing about servers,
which is the CurseForge export format's normal state: every declared file is
`unknown` on the server side, so the reconstruction either includes content a
server cannot use or omits content it needs, and the result carries an
attention reason saying so.

Many packs publish server files of their own. Those files are what the author
decided the server contains, assembled from the pack the author built. A
reconstruction cannot improve on them and can only differ from them.

Such an archive is also not a pack manifest. It has no format version, no file
identities, no declared sides, and no optional-file decisions. There is nothing
in it to normalize into a recipe.

## Decision

A server may be given a published server archive instead of a reconstruction.
The archive is installed as the server's complete content: nothing is resolved
against a catalog, no side is planned, and no optional file is decided. This is
available to a linked and a standalone server alike, because the archive
decides the content either way and only the exact Minecraft and loader
selection differs in where it comes from.

Ephervex installs the loader and its libraries for every server, including one
built from an archive that ships them. Server archives commonly carry an
installer jar and a start script that fetches the loader on first run, and
neither is ever executed. The exact selection comes from the same place it
always did: the client record for a linked server, the explicit selection for a
standalone one.

The archive does not decide these destinations:

- `libraries/`, which a loader installation replaces with a link into the
  shared libraries store. Containment during extraction is lexical, so an entry
  here would be written through that link into storage every instance and
  server shares.
- The launch descriptor the loader installation produced.
- `eula.txt`, because consent is launcher-global and explicit (ADR 0014).
- `.launcher/`, which no archive may ever name (ADR 0002).

When a linked server is reconstructed from retained override layers, `libraries/`
is left out and `.launcher/` is refused as well. In every flow, a pack entry whose
destination is or sits beneath an existing link is refused rather than written
through it.

Everything else is installed, including start scripts and `user_jvm_args.txt`.
Nothing runs them, and leaving them out would hide what the author shipped.

A single wrapping directory is stripped only when the archive's shape says so
without ambiguity: nothing at the root, exactly one top-level directory, that
directory contains a directory of its own, and its name is not one a Minecraft
server keeps at its own top level. Guessing wrong puts a server's mods one
level from where they belong, and the archive carries no evidence that would
correct it later.

The server keeps its own recipe, at `ImportSourceKind::ServerPack`, listing
every jar the archive left in `mods/` as `required` on the server with an
explicit selection and a digest the launcher computed. No download address is
recorded: the bytes came from an archive rather than a catalog, and an entry
naming no address is the honest statement that this content cannot be
re-fetched.

Those jars are then identified by digest, cheapest and most certain answer
first: what the launcher already recorded, then CurseForge, then Modrinth, then
nothing.

The first step reads the linked client's retained recipe, which pairs a digest
with a catalog identity for every file it installed. That is a lookup table and
not a source of content -- what the server contains was decided by the archive
and is already on disk before this runs -- so it does not weaken the rule above.
It is also the only step that cannot be wrong, because the digest it matches on
is one this launcher computed and wrote itself.

CurseForge is asked by its own file fingerprint, which is what its catalog is
indexed by; a fingerprint is a lookup key with no collision guarantee, so a
match is believed only when the release it names publishes the same SHA-1 the
file on disk has. Modrinth is asked by SHA-1 directly, in one batch, with no
credential.

None of this can fail the installation. A catalog that cannot be reached, a key
that is not configured, and a file nothing recognizes all leave an entry with a
digest and no provider, which is what `ModIdentity::Unknown` already means. An
identity is never overwritten by a later step.

## Consequences

- A CurseForge-derived server can be exactly what its author published, rather
  than a best-effort conversion of a client pack.
- The mod views, `ephervex server mods`, and linked mod sync work against such
  a server unchanged, because it keeps the same recipe every other server does.
- The recipe format version is 3. `ImportSourceKind::ServerPack` is not a value
  an older build can read, so the version gate refuses the file cleanly instead
  of failing to deserialize it.
- The archive is not retained. Installing the same server again means naming
  the file again, which is the same bargain import already makes with a pack.
- A jar the archive installed carries whatever identity could be established
  for it, and none where nothing recognized it. What a server's mod list can
  say about a file therefore depends on which catalogs were reachable when it
  was built, and a server built offline is not wrong -- only less complete.
- Identification reads the bytes of every still-unknown jar a second time to
  compute a CurseForge fingerprint. The files a client recipe already answered
  for are not read again, which is what keeps that cost off the common case.
