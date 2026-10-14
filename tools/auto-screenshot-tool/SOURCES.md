# Screenshot fixture sources

The files in `data/` were recorded from Ephervex 0.10.0 on Linux, run with an
isolated home (`HOME` and every `XDG_*` directory pointed at an empty folder)
and one offline account named `MelanX`. Nothing was typed in by hand except
where this file says so. Machine paths were rewritten to `/demo/...` and the
German month abbreviations Forge printed were rewritten to English; no other
text in a log or crash report was changed.

## Packs

Each pack was imported from the address Modrinth serves it at, with
`ephervex import --persistent`.

| Instance | Version | Modrinth project | License | Minecraft | Loader |
| --- | --- | --- | --- | --- | --- |
| CaveStone | 1.1.0 | `cavestone` (`3HsvIkiC`) | Apache-2.0 | 1.20.1 | Forge 47.4.10 |
| Photon | 1.3.10 | `phtn` (`YvB2dyqO`) | MIT | 1.21.1 | NeoForge 21.1.224 |
| the horrors in the fog | 1.0.5 | `the-horrors-in-the-fog` (`uPkIzDYf`) | GPL-3.0-only | 1.20.1 | Forge 47.2.17 |
| Super tiny | 1.0.0 | `bits-and-chiseled-plus-chiseled-me` (`DnDwQArY`) | MIT | 1.12.2 | Forge 14.23.5.2860 |
| Cobblemon Official Modpack [NeoForge] | 1.8.2 | `cobblemon-neoforge` (`mHMxXbIu`) | MPL-2.0 | 1.21.1 | NeoForge 21.1.259 |

Two instances were built by hand with `ephervex instance create` and
`ephervex mod install`, taking every mod from Modrinth:

- Utility Mods (Minecraft 1.21.1, NeoForge 21.1.224): Simple Backups, UtilitiX,
  Just Enough Advancements, Simply Tools, Data Trader, Excavar, Default World
  Type, and Automatic Tool Swap, with the dependencies the launcher added (LibX,
  Just Enough Items, and the Just Enough Items configuration library).
- Skyblock Tools (Minecraft 1.20.1, Forge 47.4.10): Skyblock Builder, Simply
  Tools, Simple Backups, and Automatic Tool Swap, with LibX.
- Vanilla Test (Minecraft 1.21.1, no loader).

Create+ 6.0.0 Alpha f was also tried. The launcher refused it because a file in
its manifest declares a side as `unknown`, which is the failed import shown in
the Activity page.

## CaveStone after the import

The mod list, mod dialogs, snapshots, and diagnosis results come from CaveStone
after these launcher operations:

- Added from Modrinth: Automatic Tool Swap, AIOT Botania (with Patchouli,
  Botania, and Curios API), and Extra Disks 1.20.1-3.0.1 pinned to that older
  release (with Refined Storage), so Extra Disks has a newer release to update to.
- `ephervex mod disable --only-this` on AppleSkin.
- `ephervex mod delete --only-this` on Jade and Patchouli, which keeps their records.
- The jar of Botanical Machinery downloaded from Modrinth into `mods/` by hand.
- The Simple Backups jar cut to 30,000 bytes, so verification reports it damaged.
- `ephervex snapshot create` twice, then `ephervex snapshot restore --yes` once,
  which makes the launcher's own before-restore snapshot.

The four jars the pack ships inside itself (Placebo, TwerkItMeal, PackMenu, and
Inventory Sorter) are listed as overrides, which is how the launcher classifies
them.

## Servers

`ephervex server create --client` for CaveStone (persistent), CaveStone Staging
(temporary), Photon, the horrors in the fog, Utility Mods, and Cobblemon. Each
was started once with `ephervex server start` after the Minecraft EULA was
accepted in the isolated home, then stopped. CaveStone, Photon, and Cobblemon
reached "Done". The horrors in the fog stopped while loading because the pack
declares Oculus, a client-side mod, as required on servers. Utility Mods stopped
because Just Enough Advancements needs Just Enough Items, which is client-only
and was left off the server.

## Files

| File | What it holds | How it was produced |
| --- | --- | --- |
| `data/hosts.json` | Instances, servers, the CaveStone mod rows, one mod search, and the snapshots | `instances list`, `instance show`, `servers list`, `mod list`, `mod search`, `snapshot list`, plus Modrinth project and release records for titles, versions, compatibility, and logo addresses |
| `data/dialogs.json` | Release lists, relations, impact list, and install plan for the mod dialogs | `mod show`, `mod dependencies`, `mod plan` |
| `data/diagnostics.json` | File verification, mod counts, bundle parts, and crash analyses | `mod verify`, `mod dependencies`, `logs bundle --list`, `server crash` |
| `data/logs/client.log` | 130 records of the CaveStone client | The client's `latest.log`: the first 30 records, 20 spread through the middle without the repeated first-run configuration notices, and the last 80 |
| `data/logs/crash-client.txt` | The report of a debug crash (F3 + C held for ten seconds) | Copied unchanged apart from the rewrites above. It keeps the report's system details, including the processor and graphics card of the recording machine |
| `data/logs/crash-utility-mods-server.txt` | The Utility Mods server crash report | Copied unchanged apart from the rewrites above |
| `data/logs/photon-server.log`, `data/logs/cavestone-server.log` | Server output | Each server's `latest.log`: the first 20 records, 12 from the middle, and the last 40 |

The mod rows list titles and versions as the catalog reports them for the
installed release. The author column is empty, as it is for a mod the launcher
knows only by its project. A jar the launcher cannot identify is shown by its
file name.

## Logos

Each mod row and search result names its project logo by address and by the
SHA-256 of the bytes Modrinth served. The generator fetches them into `artwork/`
and refuses any file that does not match. They are not part of the repository.

## Presentation state that is not recorded

- Which host is shown as running, starting, or needing attention, progress bars,
  toasts, activity timing, process ids and resource use, and the sign-in code.
- The numbers and answers of the isolation rounds, and the one-line "likely
  cause" of a failed run.
- The accounts other than `MelanX`, and every Microsoft account.
- The CurseForge import states and the optional-file list of the import review.
  They use a made-up export named Creeper Containment Lab and made-up optional
  file names, with blocked-file links on the reserved `example.invalid` host.
- The sign-in address, which is the public Microsoft device-login page.
- Snapshot restore and create dialogs reuse the recorded snapshots, and the
  suggested snapshot label is a placeholder in the launcher's own format.
