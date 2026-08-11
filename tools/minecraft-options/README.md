# Minecraft options.txt compatibility tooling

Maintainer tooling for the one question Ephervex has to answer before it may
touch an instance's `options.txt`:

> for this canonical setting and this Minecraft release, what exactly does the
> game store, and was that ever actually proven?

The answer lives in a generated dataset that ships with the application and can
also be published to a remote feed, so a new Minecraft release does not require
a new Ephervex release just to become configurable.

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

## Requirements

Python 3, a JDK providing `javap`, and network access for the fetching steps.
None of this is needed to build, test, or run Ephervex.

## Layout

| Path | Role |
|---|---|
| `analyze_options.py` | Static options.txt analyzer, imported unchanged from the completed research |
| `fetch_language_catalogs.py` | Fetches a release's language catalog from its own `pack.mcmeta` asset object |
| `dataset.py` | Turns analyzer output into the compatibility dataset, and validates it |
| `update.py` | The maintainer workflow |
| `test_dataset.py` | Offline tests: `python3 -m unittest discover -s tools/minecraft-options` |
| `.cache/` | Untracked development cache: JARs, Mojang metadata, analysis output, generated feed |

Analyzer output is the input to every fingerprint in the committed dataset, so
changes to extraction must be evidence-driven and followed by regeneration and
validation.

## What the dataset says, and what it deliberately does not

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

## Adding a release, and when an application release is needed

For a typical hotfix the meaningful output is that the release maps to a family
that already exists. Publishing the regenerated feed is then enough - no
Ephervex build is involved.

A new application release is needed only when:

- the release's representation cannot be expressed by the codec variants the
  shipped binary implements, or
- a newly recovered Minecraft setting should become a user-facing Ephervex
  preference. New settings are recorded as data, never promoted automatically;
  the application ignores canonical ids its own preference model does not know.

## Regenerating from scratch

The dataset is cumulative. Committed families and release mappings are never
rewritten by a later run, so the historical client JARs are only needed to
establish a release the first time. A key introduced after a release was
analyzed stays *unknown* for that older release rather than being retroactively
declared unsupported, which is the honest state and is never written.
