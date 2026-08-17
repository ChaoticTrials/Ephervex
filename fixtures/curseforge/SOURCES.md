# CurseForge export fixtures

These **hand-authored** documents follow the CurseForge modpack export format
rather than copying a published pack. Nothing here is derived from any real
modpack, no file of any real project is included, and no test in this
repository contacts CurseForge.

That matters more here than it does for the Modrinth fixtures. A CurseForge
export's files are named by project id and file id and are fetched from
CurseForge itself, so a fixture built from a published pack would name somebody
else's projects and would need their jars to be meaningful. The manifests below
therefore declare their files inline in whichever test needs them, against a
loopback server serving tiny bodies the repository owns.

No credential appears in any file here, and none is needed to read one: a
CurseForge export manifest is a local document, and parsing it is a local
operation.

## `export/`

The archive is assembled from this directory at test time
(`crates/pack/tests/curseforge_import.rs`), so every part of it stays reviewable
as text rather than sitting in a binary blob nobody opens.

| Path | What only this part proves |
|---|---|
| `manifest.json` | the current export shape: `manifestType`, `manifestVersion`, the `minecraft` object with an exact version and an exact `modLoaders` build, and a named `overrides` directory. It declares no files, which is what lets the archive half of an import run with no provider, no credential, and no network. |
| `overrides/config/repro.toml` | the single override layer reaches the game directory and is retained for a server built later |
| `overrides/options.txt` | the layer carries a file the launcher's own preference merge later touches |

### Format notes

The loader id is spelled `<family>-<build>`, and `neoforge-20.4.190` is
deliberately a NeoForge *build* number that looks like a Minecraft version. The
parser must keep the two apart, and this fixture is what fails if it stops.

Field spelling is the provider's: `manifestType`, `manifestVersion`,
`modLoaders`, and — in the manifests declared inline by the tests — `projectID`
and `fileID` with their upstream capitalization. None of it is normalized for
aesthetics, and the pretty-printing here is deliberate: this is a human-facing
fixture, not machine-owned storage Ephervex writes.
