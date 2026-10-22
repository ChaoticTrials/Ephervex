# ADR 0055 - Resource packs, shader packs, and data packs are recognized by location and kept out of the mod list

**Status:** Accepted

## Context

A modpack carries more than mods. A Modrinth index names files at any path, a
CurseForge manifest names projects whose class decides the directory, and both
formats ship loose files in override trees. Resource packs, shader packs, and
data packs reach an instance through all of these routes. They must survive
import and reconstruction, and a user must be able to tell what the pack
installed, without those files being treated as mods.

A data pack belongs to one world. Nothing in a CurseForge manifest names a
world, and an override tree can place a data pack in a client world, in a
server's world folder, or at the game root where no world owns it.

## Decision

**Location decides what a file is.** `ephervex_pack::auxiliary::classify`
recognizes `resourcepacks/<entry>`, `shaderpacks/<entry>`,
`saves/<world>/datapacks/<entry>`, `world/datapacks/<entry>` (a dedicated
server's default world folder), and `datapacks/<entry>`. The entry is the file
or the directory a multi-file pack sits in. Nothing opens an archive to decide.
A single loose file in these directories counts as shipped content only when it
is a `.zip`, so a loader's options file beside a shader pack is not reported as
a pack.

**Declared files stay ordinary recipe entries.** A pack file the source format
names keeps its provider identity, digests, size, side requirements, and
download addresses, and is installed, verified, repaired, and reconstructed by
the same code as every other recipe file. A CurseForge project of the Resource
Packs or Shader Packs class is routed to its directory by the confirmed class.

**Shipped content is recorded on its override layer.** Each `OverrideLayer`
lists the pack entries it placed. The list is sorted and defaults to empty, so
the recipe format version is unchanged and an instance imported earlier lists
only its declared files until it is imported again. The layer itself still
places the bytes, and a server built from the recipe receives the retained
common and server layers verbatim.

**A data pack is never moved.** It stays at the path the pack gave it, so a
data pack for `saves/Repro` is not applied to another world, copied into a
server's world folder, or made global. A data pack at the game root is listed
as owned by no world, with the note that Minecraft does not apply it by itself.
A CurseForge project of the Data Packs class is refused by name because the
manifest names no world to put it in, and guessing one would be inventing a
placement.

**They are shown on their own surface.** `App::host_pack_content` reads the
recipe and the game directory, offline, and reports each entry's kind, owning
world, origin, and whether it is present. The desktop workspace shows it on a
Packs tab and the command line as `ephervex packs`. The mod list never includes
these entries.

## Consequences

- Reconstruction and repair need no new code path: declared packs are recipe
  files, and shipped packs travel in the retained layers.
- The classifier is a statement about directory names, so a pack placed by a
  mod's own convention (a mod-specific data pack folder, for example) is
  preserved as a plain override but not listed.
- `world/datapacks` assumes the default world folder name. A server whose
  `level-name` differs keeps its files, and they are not listed as data packs.
