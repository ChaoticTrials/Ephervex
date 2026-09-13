# `options.txt` fixtures

These are **hand-authored representative files**, not captures. Unlike the metadata
under `fixtures/minecraft/` and `fixtures/loaders/`, an `options.txt` is not
published by any service. Each one is written in the shape the named release
writes: `key:value` per line, one line per setting, in roughly the order the
game emits them.

| File | Era | What only this fixture proves |
|---|---|---|
| `options-1.12.2.txt` | 1.12.x | `overrideWidth`/`overrideHeight` exist and are the only place a window size can be expressed; `renderClouds` is a bare boolean and `chatVisibility` an integer |
| `options-1.16.5.txt` | 1.13–1.17.x | the window-size keys are gone and `simulationDistance` does not exist yet, while enum values are still written bare: this release's save path prints `renderClouds:true` from a literal, so quoting is not an era it belongs to |
| `options-1.20.1.txt` | 1.18+ | `simulationDistance` is split out from `renderDistance`; a mod's key (`jei:configVersion:2`) carries a second colon that a naive split would corrupt |
| `options-26.1.txt` | calendar-versioned releases | the same era one scheme change later, plus `aKeyThisBuildHasNeverSeen` - a key Ephervex must preserve without understanding |

## What the tests use them for

`crates/options/tests/era_fixtures.rs` asserts, for every file:

* a file no preference touched renders back **byte for byte**;
* applying preferences rewrites only the managed keys and leaves every other
  line, including mod keys and unknown keys, exactly where it was;
* the release's own `version:` marker is preserved and never replaced with
  another release's;
* no key from a neighboring era leaks in - no `overrideWidth` in a modern
  file, no `simulationDistance` in a 1.12.2 one.

## About the `version:` values

The first line of each file is the data version the game stamps into its
options file, which tells it how much of its own history to apply to the rest
of the file. Ephervex never parses or compares one, and never copies one
between releases: a file that already has a marker keeps it untouched, and a
file Ephervex creates is marked with the data version the generated dataset
read out of that exact release's client jar. The numbers in these fixtures are
therefore illustrative of the shape rather than the source of any behavior; the
authoritative ones live in `crates/options/data/minecraft-options.json`.

## Where the values come from

These files are hand-authored, so a value in one is only as good as the
evidence behind it. Where a claim here has been checked against a release's own
bytecode or against a file the game itself wrote, it says so:

* the field of view reads `fov:0.0` in every era because every release stores
  `(degrees - 70) / 40`, so the seventy-degree default is zero rather than
  `70.0` - confirmed against real files from 26.1.2 and 26.2, which store
  `fov:0.5` for ninety degrees;
* 1.16.5 writes enum values bare, confirmed from its save path, which prints
  `renderClouds:true` from a string literal;
* releases that keep their options in `OptionInstance` encode each one with its
  codec and print the resulting JSON, so a value whose JSON form is a string
  arrives quoted - `renderClouds:"true"`, `mainHand:"right"` - confirmed
  against real 26.1.2 and 26.2 files.
