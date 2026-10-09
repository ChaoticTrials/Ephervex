# Screenshot fixture sources

The CaveStone fixture is transcribed from the supplied
`CaveStone-1.1.0-modrinth.mrpack` archive. Its SHA-256 digest is
`f4c9a77375840724c0a253223d130ee33bfd9e37142cb9c9c858dcd2cdccbfac`.
The archive's `modrinth.index.json` declares pack version `1.1.0`, Minecraft
`1.20.1`, and Forge `47.4.10`. It lists 53 mod files, of which one is marked
unsupported on clients. Two more mod JARs occur under `overrides/mods/`, so
the client fixture displays 54 mods.

The visible mod rows use exact filenames from that manifest:

- `mods/create-1.20.1-0.5.1.j.jar`
- `mods/Jade-1.20.1-Forge-11.13.2.jar`
- `mods/jei-1.20.1-forge-15.20.0.130.jar`

The archive is not included or read by the generator. Every other row is
synthetic display data: the remaining instances, servers, accounts, mods,
search results, paths, statuses, and log or crash text. Their names are
original text that borrows Minecraft vocabulary and does not name a real
project, person, or account. The loader builds shown (Forge 14.23.5.2860,
36.2.39, and 47.4.10; NeoForge 21.1.249 and 26.2.0.59) are builds that exist
and are used by the loader test fixtures. Minecraft versions are limited to
releases the launcher supports. The fixtures use no Mojang or Microsoft
artwork.

The import dialog states show CaveStone with its documented name, version,
Minecraft version, and loader. The Modrinth format names no author, so none is
shown. The pack's required and optional split (50 and 2) and the two optional
file names are synthetic. The duplicate, manual-download, and resolving states
use a synthetic CurseForge export named Creeper Containment Lab; its author,
counts, and file names are synthetic, as is the archive name in the missing-key
state.
Blocked-file links use the reserved `example.invalid` host. Activity rows,
running processes, toasts, and the sign-in code `ABCD1234` are synthetic; the
sign-in address is the public Microsoft device-login page.
