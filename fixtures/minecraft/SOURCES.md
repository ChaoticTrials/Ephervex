# Minecraft metadata fixtures

Captured from Mojang's public Piston metadata service on **2026-08-07** and
then reduced to the fields the tests actually exercise . Tests
must run with no network, so nothing here is fetched at test time.

## Sources

| File | Captured from |
|---|---|
| `version_manifest_v2.json` | `https://piston-meta.mojang.com/mc/game/version_manifest_v2.json` |
| `1.12.2.json` | `https://piston-meta.mojang.com/v1/packages/832d95b9f40699d4961394dcf6cf549e65f15dc5/1.12.2.json` |
| `1.20.1.json` | `https://piston-meta.mojang.com/v1/packages/c2f76c955503c7143424b51e21eea87bbe5c6547/1.20.1.json` |
| `1.21.1.json` | `https://piston-meta.mojang.com/v1/packages/091b2c2728f8ac413b6e7e683a6c12f21a53057c/1.21.1.json` |
| `26.1.json` | `https://piston-meta.mojang.com/v1/packages/d6e4071ad771d30003e17727e8acd41283aef208/26.1.json` |

Each per-version URL is the `url` of that version's entry in the manifest, and
each entry is preserved verbatim in `version_manifest_v2.json`.

## Why these four versions

Together they cover every metadata era supported by Ephervex. Each fixture is
the smallest one that retains at least one distinct parser or launch behavior:

| Fixture | Java | Argument form | What only this fixture proves |
|---|---|---|---|
| `1.12.2` | 8 | legacy `minecraftArguments` string | the `natives` classifier map, `downloads.classifiers`, `extract.exclude`, a library with classifiers but **no** `artifact`, and an `allow`-then-`disallow` rule pair |
| `1.20.1` | 17 | modern `arguments` object | natives shipped as ordinary rule-gated library artifacts, `os.arch` rules, `client_mappings`/`server_mappings` download keys |
| `1.21.1` | 21 | modern `arguments` object | the same shapes one Java generation on — a regression canary for "modern" drifting |
| `26.1` | 25 | modern `arguments` object | the `default-user-jvm` argument group and `os.versionRange { min, max }` used by calendar-versioned releases |

The manifest fixture keeps the 14 entries that matter for filtering: releases
above and below the 1.12.2 floor, an `old_beta` and an `old_alpha`, and a
release candidate plus a pre-release — both of which Mojang types as
`"snapshot"` rather than as types of their own. That is why filtering on
`type == "release"` alone is sufficient and no extra exclusion rules exist.

## What was removed

* **Manifest** — 891 of the 905 version entries. The 14 that remain are
  verbatim, in Mojang's own newest-first order.
* **Version JSONs** — most `libraries` entries (39–107 down to 4–6, chosen to
  cover every distinct shape) and most unconditional `arguments` entries (the
  first four of each group are kept; **every** conditional entry is kept,
  because those are the ones carrying rules).

Everything else — `javaVersion`, `assetIndex`, `downloads`, `logging`,
`mainClass`, `assets`, `complianceLevel`, `minimumLauncherVersion`, and the
timestamps — is preserved verbatim.

## The `sha1` fields are deliberately not the live ones

`version_manifest_v2.json` records a `sha1` per version entry, and Ephervex
verifies it when it downloads that version's JSON . Reducing a
version JSON changes its digest, so the four fixture entries carry the SHA-1 of
the **reduced** file instead of Mojang's. The verification path is therefore
exercised end to end offline. Every other field of those entries, including the
`url`, is untouched.

Regenerating a fixture means recomputing that digest:

```bash
sha1sum fixtures/minecraft/1.12.2.json
```
