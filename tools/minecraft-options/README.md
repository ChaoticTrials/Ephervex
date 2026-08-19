# Minecraft options.txt compatibility tooling

Maintainer tooling for two related but deliberately separate questions:

> for this canonical setting and this Minecraft release, what exactly does the
> game store, and was that ever actually proven?

and:

> does this Ephervex build expose that setting as a preference, and how should
> the product understand and present it?

The first answer lives in a generated dataset (`dataset.py`) that ships with
the application and can also be published to a remote feed, so a new Minecraft
release does not require a new Ephervex release just to become configurable.
The second answer lives in a bundled product catalog (`preferences.py`) that
has no remote form at all: growing the set of exposed preferences always
requires an Ephervex release, by design. See
[What each document owns](#what-each-document-owns) below.

Nothing in here runs Minecraft. The analyzer reads client JARs as ZIP/class
data and disassembles only the options root and the helper classes it directly
references.

## The workflow

```bash
python3 tools/minecraft-options/update.py --latest
python3 tools/minecraft-options/update.py --version 26.2
```

That single command reads Mojang's version manifest, picks a **release** (never
a snapshot or release candidate), downloads and SHA-1 verifies the official
client JAR and version metadata into an untracked development cache, analyzes
the JAR statically, refreshes that release's official language catalog, folds
the result into the committed dataset, and writes the exact JSON to publish to
the remote feed. It prints a short summary of what actually changed.

Two supporting modes:

```bash
# Fold an existing analyzer output directory in without re-running the analyzer.
python3 tools/minecraft-options/update.py --from-analysis <dir> --offline

# Verify the committed dataset is exactly what those inputs regenerate.
python3 tools/minecraft-options/update.py --check --from-analysis <dir> --offline
```

Publication is manual and deliberate. `update.py` never edits the remote feed
and never needs GitHub credentials; it writes the file and prints its path.

`update.py` also prints, without writing anything, whenever a current
manageable setting is not yet covered by the bundled product catalog:

```
2 product-catalog candidate(s) need application review: someNewSetting, anotherOne
  the bundled preference catalog does not expose or exclude them yet; review with
  preferences.py before they can appear on a settings screen
```

That notice is read-only. Growing the product catalog is a separate,
deliberate step:

```bash
python3 tools/minecraft-options/preferences.py --from-analysis tools/minecraft-options/.cache/analysis
python3 tools/minecraft-options/preferences.py --from-analysis tools/minecraft-options/.cache/analysis --check
```

The first form regenerates `crates/options/data/minecraft-preferences.json`
from the committed compatibility dataset plus the canonical release's own
analyzer output (for proven defaults, enum labels and keybinding groups); the
second verifies the bundled catalog is exactly what those inputs regenerate,
without writing anything. `--from-analysis` is optional: without it, the
catalog still regenerates completely (every accounting fact comes from the
compatibility dataset alone), just with less presentation enrichment.

Regenerating the catalog is a reviewed source change, not something
`update.py` ever does on its own - see
[What each document owns](#what-each-document-owns).

## Requirements

Python 3, a JDK providing `javap`, and network access for the fetching steps.
None of this is needed to build, test, or run Ephervex.

## Layout

| Path | Role |
|---|---|
| `analyze_options.py` | Static options.txt analyzer, imported unchanged from the completed research |
| `fetch_language_catalogs.py` | Fetches a release's language catalog from its own `pack.mcmeta` asset object |
| `dataset.py` | Turns analyzer output into the compatibility dataset, and validates it |
| `preferences.py` | Turns the compatibility dataset (plus optional analyzer enrichment) into the bundled product preference catalog, and validates it |
| `update.py` | The maintainer workflow: fetches, analyzes, folds into the compatibility dataset, and reports whether the product catalog needs review |
| `test_dataset.py`, `test_preferences.py`, `test_update.py` | Offline tests: `python3 -m unittest discover -s tools/minecraft-options` |
| `.cache/` | Untracked development cache: JARs, Mojang metadata, analysis output, generated feed |

Analyzer output is the input to every fingerprint in the committed dataset, so
changes to extraction must be evidence-driven and followed by regeneration and
validation.

## What each document owns

Two generated documents ship with the application, and neither can do the
other's job:

| | `minecraft-options.json` | `minecraft-preferences.json` |
|---|---|---|
| Answers | how a Minecraft release stores a setting | whether this build exposes a setting, and how to present it |
| Owner | `dataset.py` | `preferences.py` |
| Remote-updatable | yes, via the options feed | no - bundled only |
| A new Minecraft release | may extend it without a new Ephervex release | never changes it by itself |
| A new canonical setting | recorded as data automatically | requires a reviewed regeneration and an Ephervex release before it is user-facing |

That asymmetry is the load-bearing part: the compatibility feed may say "this
release stores this setting this way", but it can never decide "this Ephervex
build should now expose that setting". Only a source change to
`minecraft-preferences.json`, reviewed and shipped like any other code change,
can do that.

### The compatibility dataset (`minecraft-options.json`)

The committed dataset lives at `crates/options/data/minecraft-options.json` and
is embedded in the application. Its four parts are kept apart on purpose:

- **setting registry** - cumulative stable ids retained for every current and
  historical schema;
- **current canonical setting ids** - only settings present in the newest
  supported release;
- **schema families** - one deduplicated per-release representation of every
  canonical setting, identified by a content-derived fingerprint;
- **release mapping** - which release uses which family.

A family id is a digest of the family's own expanded content. Two releases
share a family only when their extracted representations are byte-for-byte the
same conclusion. A version number never makes two releases equivalent: a hotfix
whose extraction differs gets its own family, and a release the analyzer cannot
fully resolve is left unknown rather than assumed to match its predecessor.

Three per-release states, and the difference between the last two matters:

| State | Meaning | Effect |
|---|---|---|
| supported | The representation was proven for that release | Ephervex may write it |
| unsupported | The key provably did not exist in that release | Ephervex must not write it |
| unknown (absent from both) | Nothing was established | Ephervex must not write it |

Codecs are a closed set of typed variants with validated parameters - boolean,
bounded integer, bounded decimal, an affine `scaled_decimal` (the pre-1.19
field-of-view transform), enum id, enum token, LWJGL2 key code, GLFW key name,
free text, and language code. A storage transform the generator cannot recognise
as one of those becomes *unknown* here, at generation time. No expression is
ever written into the dataset, so nothing downstream has to evaluate one.

The dataset carries no language catalogs. Those are Mojang-owned volatile data
that the application fetches and caches per release during explicit metadata
browsing; creation and import use only an existing cache. Duplicating catalogs
here would create a second authority for something Mojang owns.

### The product preference catalog (`minecraft-preferences.json`)

The committed catalog lives at `crates/options/data/minecraft-preferences.json`
and is embedded in the application (`ephervex_options::MinecraftPreferenceCatalog`).
It is generated, but from the *canonical* release's own analysis plus a small,
narrow, reviewed policy in `preferences.py` - the exclusions, the Window Size
composition, and the stable id overrides for the seven preferences that predate
this catalog. Nothing here duplicates the compatibility dataset's own
per-release mappings.

Every current canonical setting the compatibility dataset knows about is
accounted for exactly once, as either:

- a **descriptor**, exposing one or more canonical dataset ids under a stable
  product id, a category, and a closed presentation kind; or
- an **exclusion**, naming the canonical id and a durable reason it is not a
  global preference (session/history state, machine-specific state, or state
  that belongs to resource/pack management rather than to a settings screen).

`preferences.py --check` (or the Rust `MinecraftPreferenceCatalog::bundled`
constructor, which every options-crate test runs against) refuses a catalog
that leaves a current manageable setting unaccounted for, that claims a
non-current or non-manageable setting, that double-claims a canonical field, or
that uses a category or presentation kind this build does not implement.

A descriptor's presentation carries only what the analyzer actually proved for
the canonical release: a default is absent, not guessed, when the analyzer
never established one; a numeric range is absent, not guessed, when the
compatibility dataset's own codec never proved one either. The list of valid
Minecraft languages is deliberately not part of this file, for the same reason
the compatibility dataset above carries no language catalogs: a descriptor may
identify a setting as the special `language` kind, but carries at most a
proven default code, never a catalog.

## Adding a release, and when an application release is needed

For a typical hotfix the meaningful output is that the release maps to a family
that already exists. Publishing the regenerated feed is then enough - no
Ephervex build is involved.

A new application release is needed only when:

- the release's representation cannot be expressed by the codec variants the
  shipped binary implements, or
- a newly recovered Minecraft setting should become a user-facing Ephervex
  preference.

The second case is always a maintainer decision, never a side effect of an
ordinary update. `update.py` reports it as a read-only notice (see
[The workflow](#the-workflow) above) instead of writing anything. Turning the
notice into an exposed preference means:

1. deciding whether the setting belongs on a settings screen at all, and if
   not, adding it to `preferences.py`'s `EXCLUSIONS` with a durable reason;
2. otherwise doing nothing beyond that - `preferences.py`'s accounting is
   automatic, so a manageable setting that is not explicitly excluded is
   exposed the next time the catalog is regenerated;
3. running `preferences.py --from-analysis <analysis dir>` and reviewing the
   diff to `crates/options/data/minecraft-preferences.json` like any other
   source change;
4. shipping that change in an Ephervex release. Until then, the bundled
   catalog - and therefore the settings screen - does not change, however far
   ahead the compatibility dataset's own `current_canonical_setting_ids` gets.

## Regenerating from scratch

The dataset is cumulative. Committed families and release mappings are never
rewritten by a later run, so the historical client JARs are only needed to
establish a release the first time. A key introduced after a release was
analyzed stays *unknown* for that older release rather than being retroactively
declared unsupported, which is the honest state and is never written.
