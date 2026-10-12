# Modrinth pack fixtures

These **hand-authored** documents cover Modrinth client/server environment
values and common, client, and server override layers. They follow the
Modrinth modpack format specification rather than copying a published pack.
Unlike the metadata under
`fixtures/minecraft/` and `fixtures/loaders/`, a `.mrpack` is a redistributable
artifact belonging to whoever built it, and Ephervex's tests need one whose
hashes match files the repository is allowed to ship.

Nothing here is derived from any real pack, and no test in this repository
contacts Modrinth.

## `layered-client-server/`

The archive is assembled from this directory at test time
(`crates/pack/tests/mrpack_import.rs`), so every part of it stays reviewable as
text rather than sitting in a binary blob nobody opens.

| Path | What only this part proves |
|---|---|
| `modrinth.index.json` | all four client/server environment combinations in one pack |
| `overrides/config/shared.toml` | the common layer reaches both sides |
| `overrides/options.txt` | the common layer is *replaced* by a later layer that ships the same file |
| `client-overrides/options.txt` | the client layer wins over the common one, and carries a mod key with a second colon (`jei:configVersion:2`) that must survive the managed-preference merge |
| `server-overrides/server.properties` | a file the client must never receive, retained under `.launcher/source/` for server creation |
| `overrides/mods/shipped-with-the-pack.jar` | a mod the pack carries rather than declares, which the recipe has to learn from the archive because the index never lists it |
| `overrides/mods/notes.txt` | a pack may ship anything into `mods/`, and only a jar is recorded as a mod |
| `client-overrides/mods/client-only-shipped.jar` | a shipped mod the server must not receive |
| `server-overrides/mods/server-only-shipped.jar` | a shipped mod the client must not receive, recorded on a client import so a server built later still lists it |

The four files in `files/` are the pack's downloadable content:

| File | `env.client` | `env.server` | What it proves |
|---|---|---|---|
| `both-sides.jar` | required | required | installed on both sides |
| `client-only.jar` | required | unsupported | installed for the client, excluded from the server plan |
| `server-only.jar` | unsupported | required | **not** installed into the client, but preserved in the recipe |
| `optional.jar` | optional | optional | default unchecked; installed only when explicitly selected |

They are minimal deterministic JARs - one `META-INF/MANIFEST.MF` entry, fixed
timestamp - so the SHA-1 and SHA-512 digests written into
`layered-client-server/modrinth.index.json` are stable on every machine. If a file here is
regenerated, both digests in the index have to be regenerated with it, or the
import fails with a hash mismatch.

### The one transformation the tests apply

The index's `downloads` addresses name `cdn.modrinth.com`, because that is what
a real pack contains and because it is what the download allowlist accepts.
A test cannot reach that host - and must not try - so the
harness rewrites each address's **origin** to its own loopback server and
asserts that the original host was the allowlisted one before doing so. Paths,
hashes, sizes, environments, and everything else are used exactly as written.

## `invalid/` - packs that must be refused

One document per refusal, so a regression names the rule it broke rather than
"the parser accepted something".

| File | Must be refused because |
|---|---|
| `fabric.index.json` | Fabric is not a supported loader and must not be silently downgraded to Vanilla |
| `both-loaders.index.json` | a pack declaring both Forge and NeoForge is ambiguous |
| `format-version-2.index.json` | a newer pack format revision cannot be guessed safely |
| `path-traversal.index.json` | a file path escapes the game directory |
| `launcher-metadata.index.json` | a file targets launcher-owned `.launcher/` metadata (ADR 0002) |
| `unlisted-host.index.json` | a download host is not on the allowlist |
| `plaintext-download.index.json` | a download uses plaintext HTTP |
| `no-hash.index.json` | no supported hash is available for verification |
| `sha1-only.index.json` | a file lists `sha1` without the `sha512` the format requires |
| `format-version-0.index.json` | `formatVersion` 0 is not a format revision |
| `missing-required-fields.index.json` | `formatVersion`, `game`, `versionId` and `name` are required and are never defaulted |
| `no-files.index.json` | `files` is required, so an index without it is not read as a pack with no files |
| `no-file-size.index.json` | a file without the `fileSize` the format requires |
| `old-minecraft.index.json` | the selected Minecraft release is below the supported floor |

The digests in these documents are the SHA-1 and SHA-512 of the empty string. They are never
verified: every one of these packs is refused before anything is downloaded, and
a digest that looked real would suggest otherwise.

## `api/`

**Hand-authored** responses in the shapes Modrinth's public API documentation
describes, transcribed on **2026-08-09** rather than captured from the live
service. Nothing here is copied from a real project, and no test in this
repository contacts Modrinth.

They are written rather than captured for the same reason the `.mrpack`
fixtures are: a real capture would carry a real project's metadata and CDN
addresses, which belong to whoever published them, and would drift the moment
that project published a new version. What the parsers under test read is the
*shape* - field names, nesting, and the enumerated values - and a transcribed
fixture exercises that exactly as well.

| File | What only this file proves |
|---|---|
| `search-mods.json` | three hits with different verdicts, including one whose project-wide `versions` and `categories` arrays both look compatible |
| `versions-fits.json` | a newest compatible version, an older compatible one, and a newer one for the wrong release - so "newest" cannot be read as "last published" |
| `versions-old.json` | a project published only for another Minecraft release |
| `versions-split.json` | the case search facets get wrong: one version supports this release, another supports this loader, and neither supports both |
| `versions-library.json` | a required dependency that resolves cleanly |
| `versions-optional-extra.json` | an optional dependency, which must stay unselected |
| `versions-primary-files.json` | exactly one file marked `primary` alongside a `sources-jar`, and a version with none marked, whose first entry is the documented fallback |
| `project-undeclared-sides.json` | `server_side: "unknown"` - an upstream value an author can leave in place, which Ephervex records rather than resolves |

The digests are well-formed placeholders. Nothing in these files is downloaded:
the fixtures drive catalog parsing and dependency resolution, and the tests that
actually fetch and verify bytes build their own artifacts against a loopback
server (`crates/pack/tests/managed_mod_installation.rs`).
