# `options.txt` fixtures

These are **hand-authored representative files**, not captures. Unlike the metadata
under `fixtures/minecraft/` and `fixtures/loaders/`, an `options.txt` is not
published by any service. Each one is written in the shape the named release
writes: `key:value` per line, one line per setting, in roughly the order the
game emits them.

| File | Era | What only this fixture proves |
|---|---|---|
| `options-1.12.2.txt` | 1.12.x | `overrideWidth`/`overrideHeight` exist and are the only place a window size can be expressed; `renderClouds` is a bare boolean and `chatVisibility` an integer |
| `options-1.16.5.txt` | 1.13–1.17.x | the window-size keys are gone, `simulationDistance` does not exist yet, and quoted values (`renderClouds:"true"`, `mainHand:"right"`) have appeared |
| `options-1.20.1.txt` | 1.18+ | `simulationDistance` is split out from `renderDistance`; a mod's key (`jei:configVersion:2`) carries a second colon that a naive split would corrupt |
| `options-26.1.txt` | calendar-versioned releases | the same era one scheme change later, plus `aKeyThisBuildHasNeverSeen` — a key Ephervex must preserve without understanding |

## What the tests use them for

`crates/options/tests/era_fixtures.rs` asserts, for every file:

* a file no preference touched renders back **byte for byte**;
* applying preferences rewrites only the managed keys and leaves every other
  line, including mod keys and unknown keys, exactly where it was;
* the release's own `version:` marker is preserved and never replaced with
  another release's;
* no key from a neighboring era leaks in — no `overrideWidth` in a modern
  file, no `simulationDistance` in a 1.12.2 one.

## About the `version:` values

The first line of each file is the data version the game stamps into its
options file. Ephervex **never parses, compares, or copies** this value — the
only behavior under test is that it survives untouched — so the numbers here
are illustrative rather than authoritative. Nothing in the codebase branches on
them, and a fixture refreshed from a real installation may carry a different
one without any test changing.
