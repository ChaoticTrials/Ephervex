# Minecraft options.txt compatibility tooling

Maintainer tooling for two related but deliberately separate questions:

> for this canonical setting and this Minecraft release, what exactly does the
> game store, and was that ever actually proven?

and:

> does this Ephervex build expose that setting as a preference, and how should
> the product understand and present it?

The first answer lives in a generated dataset (`dataset.py`) that ships with
the application and is also served as a remote feed once merged to `main`, so a new Minecraft
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
the JAR statically, and folds the result into the committed dataset. It prints a
short summary of what actually changed. Merging the dataset to `main` publishes
it: the Pages workflow serves the committed file from `main` as the remote feed.

A build counts as a release only when its `version.json` `name` and `id` agree
on a release identifier (`1.N`, `1.N.P`, or `YY.N[.P]` from 26.1). A
`release_target` never names a build, and an analysis record whose version is
not a release identifier is skipped by `--from-analysis` instead of being
ordered as if its non-numeric parts were zero.

Exactly one of `--latest`, `--version` and `--from-analysis` selects the input.
`--latest` always re-reads the manifest, and a `--version` the cached manifest
does not list re-reads it once before failing. The version metadata and the
client JAR are each checked against the SHA-1 Mojang published for them; a
mismatch or a missing hash stops the run. Mojang publishes no hash for the
manifest itself, so it is checked for shape only. The version metadata and the client JAR both come
from that one manifest.

Two supporting modes:

```bash
# Fold an existing analyzer output directory in without re-running the analyzer.
python3 tools/minecraft-options/update.py --from-analysis <dir> --offline

# Verify the committed dataset is exactly what those inputs regenerate.
python3 tools/minecraft-options/update.py --check --from-analysis <dir> --offline

# Verify it against a release already analyzed into the cache.
python3 tools/minecraft-options/update.py --check --version 26.2
```

`--check` writes nothing and never uses the network. With a release selection it
reads the cached manifest and the analysis a previous run left under
`.cache/analysis/mc-<version>`.

`update.py --check` takes the directory for one analyzed *release*
(`.cache/analysis/mc-26.2`), not the analysis root: the root's own `normalized/`
holds whatever the last single-release run left there, which is not the same
thing. `preferences.py` takes either: it looks for the canonical release in
`<dir>/mc-<version>/normalized` first and then in `<dir>/normalized`.

`analyze_options.py` run by hand takes `--root` (a directory that must contain every
JAR, including one passed with `--input-jar`) and `--output`. It refuses a root that
is not a directory, an `--input-jar` outside the root, and a root with no JARs, and in
each case leaves the output as it was. Two different JARs that report the same release
are refused too, because the mapping for that release would be undecided; copies of
one JAR count once. A successful run removes the `raw/` and `normalized/` records of
JARs it did not analyze, so the output holds exactly that run's analysis, and
`update.py` and `preferences.py` refuse analyzer output that holds different records for
one release. The analyzer writes `inventory.json`, `raw/` and `normalized/` and nothing
else; `normalized/` is the only part the rest of the tooling reads.

The committed dataset is the feed: the documentation site publishes
`crates/options/data/minecraft-options.json` from `main` at
`/assets/minecraft-options.json`, so there is no second file to publish and no
separate publication step. `update.py` never needs GitHub credentials.

The feed covers the settings the current build offers and nothing else. An
already-released build adopts any feed with a higher revision than its bundled
one, and a setting that build offers but the feed no longer carries becomes
*unknown* for it and is never written. So a revision generated after
`OFFERED_SETTINGS` shrank takes those preferences away from older builds until
their users upgrade; publish it knowing that, or keep the set a superset of what
supported releases offer.

Before anything is written, `dataset.validate` applies the same structural checks
the application applies when it loads a dataset (codec parameters, option keys,
representation positions, schema and release references), and requires the
canonical release to support every setting Ephervex's own preference model needs.
A dataset that fails either check is refused with an error, and the bundled
file is not touched.

`update.py` also prints, without writing anything, whenever a current
manageable setting is not yet covered by the bundled product catalog:

```
2 product-catalog candidate(s) need application review: someNewSetting, anotherOne
  the bundled preference catalog does not expose or exclude them yet; review with
  preferences.py before they can appear on a settings screen
```

That notice is read-only. Ephervex offers only the canonical ids listed in
`OFFERED_SETTINGS` in `preferences.py`, and `update.py` limits the compatibility
dataset to that set plus the few ids the application cannot work without
(`dataset.ALWAYS_RETAINED`). Remote data cannot invent a setting, so a setting
the catalog never offers would only be carried and published for nothing; the
analyzer still reads every setting of a release, so offering one later means
adding its canonical id to `OFFERED_SETTINGS`, analyzing every cached release again
and folding the complete analysis with `--from-analysis` (the committed dataset no
longer holds the older releases' conclusions for it, so a fold of only the newest
release would leave the others unknown), and regenerating the catalog.
Growing the product catalog is a separate, deliberate step:

```bash
python3 tools/minecraft-options/preferences.py --from-analysis tools/minecraft-options/.cache/analysis
python3 tools/minecraft-options/preferences.py --from-analysis tools/minecraft-options/.cache/analysis --check
```

The first form regenerates `crates/options/data/minecraft-preferences.json`
from the committed compatibility dataset plus the canonical release's own
analyzer output (for proven defaults, enum labels and keybinding groups); the
second verifies the bundled catalog is exactly what those inputs regenerate,
without writing anything. A `--from-analysis` directory must hold a complete
analysis of the canonical release whose extracted settings match the committed
compatibility dataset. A directory with no such analysis (a mistyped path, a
different release, an incomplete run, or output left over from an earlier
analyzer version) is an error that names the cause and writes nothing. Without
`--from-analysis` the catalog still regenerates completely (every accounting
fact comes from the compatibility dataset alone), but with no proven defaults,
enum labels or keybinding groups, so the tool says so and `--check` reports
that as the reason for a mismatch.

Generation also validates the catalog against the dataset, and the application
repeats the same checks when it loads the bundled pair: each presentation kind
must match the codec the canonical release stores the setting with, a numeric
range must equal the codec's, an enum must offer every value the codec accepts,
and a default must have the type its presentation kind holds.

Regenerating the catalog is a reviewed source change, not something
`update.py` ever does on its own - see
[What each document owns](#what-each-document-owns).

## Requirements

Python 3, a JDK providing `javap`, and network access for the fetching steps.
None of this is needed to build, test, or run Ephervex.

That is also why regenerating and verifying the committed datasets is a
maintainer step rather than part of the release gate: both checks need the
untracked development cache, which no release build has. The datasets ship
compiled into the binaries and are never fetched at build time. See
[`docs/release.md`](../../docs/release.md).

## Layout

| Path | Role |
|---|---|
| `analyze_options.py` | Static options.txt analyzer: reads a client JAR's bytecode and reports how each option is stored |
| `official_sources.py` | Downloads from Mojang's published sources: https-only, SHA-1 verified, with validated identifiers and the shared version manifest cache |
| `file_io.py` | Every file the scripts generate or cache is written through a temporary file in the same directory and renamed into place, so an interrupted run leaves the previous file. Damaged inputs are reported with their path |
| `release_identity.py` | What counts as a release identifier, and how releases are ordered |
| `token_lambdas.py` | Runs the reader and writer lambdas of an enum's text codec to prove which token each constant is stored as |
| `dataset.py` | Turns analyzer output into the compatibility dataset, and validates it |
| `preferences.py` | Turns the compatibility dataset (plus optional analyzer enrichment) into the bundled product preference catalog, and validates it |
| `update.py` | The maintainer workflow: fetches, analyzes, folds into the compatibility dataset, and reports whether the product catalog needs review |
| `tests/` | Offline tests for all of the above, one `test_*.py` per subject: `python3 -m unittest discover -s tools/minecraft-options`, or one module with `python3 -m unittest tests.test_dataset` from this directory |
| `.cache/` | Untracked development cache: JARs, Mojang metadata, analysis output |

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
| A new canonical setting | recorded only if the build already offers it; any other setting is not carried | requires a reviewed regeneration and an Ephervex release before it is user-facing |

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
bounded integer, bounded decimal, an affine `scaled_decimal` (the field-of-view
transform, `(degrees - 70) / 40`, written as a decimal), enum id, enum token,
LWJGL2 key code, GLFW key name, free text, and language code. A storage
transform the generator cannot recognize as one of those, or one on a setting
the game writes as an integer, becomes *unknown* here, at generation time. No
expression is ever written into the dataset, so nothing downstream has to
evaluate one.

An enum stored as text has the tokens its codec writes. A release that gives the
codec as an inline reader (text to constant) and writer (constant to text) hides
them in string and ordinal switches, so `token_lambdas.py` runs both lambdas for
every constant and accepts the tokens only when the reader returns the constant
the writer was given. Text carried as `Codec.STRING` or as the right side of
`Codec.either(Codec.BOOL, Codec.STRING)` is understood; a codec the evaluator
cannot run to the end leaves the setting *unknown*.

A family may also carry a `startup_preset`. From 1.21.11 the client applies a
graphics preset right after loading `options.txt`, using the preset the file
names or its default when it names none, and that replaces a fixed group of
settings (render distance, clouds, particles and others) on every launch. The
analyzer finds the group from the preset enum's own switch over its ordinal and
the options accessors it calls, and takes the one preset value the switch has no
case for as the value that replaces nothing. The family records the preset
setting, that neutral token and the controlled settings in canonical ids; an
application that writes any of them also writes the neutral token, unless the
user manages the preset. A release whose group cannot be resolved is reported
as an extraction gap instead of being recorded partly. Older builds ignore the
field, so the format version is unchanged.

Free text is the value written raw after the first colon. A string setting a
release prints as a JSON string (`soundDevice:"Foo"` from 1.19) or stores as a
Gson array line (`resourcePacks` before 1.17) is not that, so it stays
*unknown* for that release instead of sharing the raw `text` representation.

A key binding codec also needs the numeric table its release's default codes
belong to. The analyzer assumes LWJGL2 before 1.13 and GLFW from 1.13, then
checks the assumption against the release's own bytecode: well-known defaults
(movement, jump, inventory, chat, the mouse buttons) must decode to the keys
they are known to be. A modern release that contradicts the GLFW table is tried
against USB HID usage ids (`hid`, 1-based mouse buttons), which 26.3 uses; it
is adopted only when its own anchors verify. Both write the same key names, so
they share one codec. A JAR whose release version cannot be identified assumes
neither: each of LWJGL2 and GLFW is tried, and the one whose anchors verify is
adopted. A release that contradicts every table, or exposes too few
of those defaults to prove it, records the code space as `unverified`, with
the evidence in `keybinding_code_space_check`. Its key bindings keep their raw
codes but are medium confidence with no serialized key, and they get no codec.

The codec does not depend on the default. A binding whose default the analyzer
cannot read keeps the codec of its release, with an unknown default. A default
built from a constant `Key` is the unbound key only when that constant's own
static initializer creates it from code -1 and the key type enum registers -1
under `key.keyboard.unknown`.

The dataset carries no language catalogs. Those are Mojang-owned volatile data
that the application fetches and caches per release during explicit metadata
browsing; creation and import use only an existing cache. Duplicating catalogs
here would create a second authority for something Mojang owns.

### The product preference catalog (`minecraft-preferences.json`)

The committed catalog lives at `crates/options/data/minecraft-preferences.json`
and is embedded in the application (`ephervex_options::MinecraftPreferenceCatalog`).
It is generated, but from the *canonical* release's own analysis plus a small,
narrow, reviewed policy in `preferences.py` - the exclusions, the Window Size
composition, and the stable id overrides for the other six preferences that
predate this catalog. Nothing here duplicates the compatibility dataset's own
per-release mappings.

Every current canonical setting the compatibility dataset knows about is
accounted for exactly once, as either:

- a **descriptor**, exposing one or more canonical dataset ids under a stable
  product id, a category, and a closed presentation kind; or
- an **exclusion**, naming the canonical id and a durable reason it is not a
  global preference (session/history state, machine-specific state, or state
  that belongs to resource/pack management rather than to a settings screen),
  or a manageable setting the canonical release has no codec for, which the
  generator excludes by itself because a row for it could never be applied.

Descriptors are grouped into a closed set of categories (`PRODUCT_CATEGORIES` in
`preferences.py`, mirrored by `PreferenceCategory` in `crates/options`): video,
sound and music, controls, gameplay, language and accessibility, and onboarding.
The first five come from the analyzer's own grouping (`CATEGORIES`); onboarding
is assigned by id through `ONBOARDING_SETTINGS`.
Onboarding holds the client-owned first-launch markers a user may set so a
disposable instance opens without first-launch screens or hints in the way:
`tutorialStep` (`none` turns the tutorial hints off), `onboardAccessibility`
(true opens the accessibility setup screen at the next start),
`joinedFirstServer` (false shows the social interactions hint on the first
multiplayer join) and `skipMultiplayerWarning` (true opens Multiplayer without
the safety warning). `startedCleanly` is never offered: it is crash-recovery state,
not something to set. The markers are listed in `ONBOARDING_SETTINGS`, and the
settings screen shows a one-line description for each.

`preferences.py --check` (or the Rust `MinecraftPreferenceCatalog::bundled`
constructor, which every options-crate test runs against) refuses a catalog
that leaves a current manageable setting unaccounted for, that claims a
non-current or non-manageable setting, that double-claims a canonical field, or
that uses a category or presentation kind this build does not implement.

A descriptor's presentation carries only what the analyzer actually proved for
the canonical release: a default is absent, not guessed, when the analyzer
never established one, including when the game picks it by a condition such as
the platform or the JVM word size; a numeric range is absent, not guessed, when the
compatibility dataset's own codec never proved one either. The list of valid
Minecraft languages is deliberately not part of this file, for the same reason
the compatibility dataset above carries no language catalogs: a descriptor may
identify a setting as the special `language` kind, but carries at most a
proven default code, never a catalog.

## Adding a release, and when an application release is needed

For a typical hotfix the meaningful output is that the release maps to a family
that already exists. Merging the regenerated dataset is then enough: the
site republishes it as the feed and no Ephervex build is involved.

A new application release is needed only when:

- the release's representation cannot be expressed by the codec variants the
  shipped binary implements, or
- a newly recovered Minecraft setting should become a user-facing Ephervex
  preference.

The second case is always a maintainer decision, never a side effect of an
ordinary update. A newly recovered setting is not carried by the dataset until
its canonical id is added to `OFFERED_SETTINGS` in `preferences.py`, and
`update.py` reports a setting the catalog has not accounted for as a read-only
notice (see [The workflow](#the-workflow) above) instead of writing anything.
Turning one into an exposed preference means:

1. adding its canonical id to `OFFERED_SETTINGS`, analyzing every cached release
   again, and folding the complete analysis with `update.py --from-analysis`;
2. deciding whether the setting belongs on a settings screen at all, and if not,
   adding it to `EXCLUSIONS` with a durable reason; otherwise a manageable
   setting with a canonical codec is exposed the next time the catalog is
   regenerated;
3. running `preferences.py --from-analysis <analysis dir>` and reviewing the
   diff to `crates/options/data/minecraft-preferences.json` like any other
   source change;
4. shipping that change in an Ephervex release. Until then, the bundled
   catalog - and therefore the settings screen - does not change.

## Regenerating from scratch

The dataset is cumulative. Committed families and release mappings are kept as they are unless a release's
own extracted representations change or the offered set shrinks (families are
content-addressed, so a pruned family gets a new id), so the historical client JARs are only needed to
establish a release the first time. A key introduced after a release was
analyzed stays *unknown* for that older release rather than being retroactively
declared unsupported, which is the honest state and is never written.

Folding never modifies the committed dataset it starts from, and the revision
advances whenever the published content differs, including registry metadata
and release data versions. Setting ids come only from a batch that reaches the
committed canonical release; an older release names a new setting by its
persisted key. Folding a release again keeps its family unless its own
extracted representations changed, so re-folding analyzed releases, alone or
together with a new release, gives the same dataset as folding only the new
release.
