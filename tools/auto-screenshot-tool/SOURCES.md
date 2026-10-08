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

The archive is not included or read by the generator. The server, Vanilla
instance, account labels, paths, and statuses are synthetic display data.
