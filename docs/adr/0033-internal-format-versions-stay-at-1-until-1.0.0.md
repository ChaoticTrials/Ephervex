# ADR 0033 - Internal format versions stay at 1 until 1.0.0

**Status:** Accepted

## Context

`docs/internal-format-versions.md` tracks the private, internal-only formats
Ephervex stamps with a version: the pack recipe, the configuration schema, and
instance metadata. `RECIPE_VERSION` reached 5 through ordinary development
before Ephervex has ever had a 1.0.0 release, and tagged releases up to 0.7.0
have already written recipes stamped anywhere from 1 through 5. None of that
number means anything to a user; it only records how many times the format
changed shape during development. A build for an earlier commit, or a binary
found on the project's Forgejo instance, showing a private format already at
version 5 before the product has ever reached 1.0.0 looks like a project that
skipped its own foundations.

Resetting the constant back to 1 partway through development is not free: a
build with `RECIPE_VERSION` reset to 1 reads every already-written recipe
stamped 2 through 5 as newer than itself and refuses it, the same refusal
`SourceStore::read_recipe` performs for a genuinely future recipe (ADR 0012).

## Decision

Every Ephervex-owned format version stays at 1 for as long as Ephervex has not
shipped 1.0.0, no matter how many times the format changes shape during
development. This includes the SQLite index, disposable caches, and generated
analysis workspaces as well as the configuration, host metadata, and retained
recipe. A shape change made during the freeze has to stay additive where state
is retained. Disposable state may instead be rejected and rebuilt. There is no
pre-1.0 version bump available to signal "refuse this file instead of misreading
it."

Re-import is the recovery path for a change that cannot be additive, or for an
on-disk file stamped above the frozen value by an earlier pre-1.0.0 build.
Ephervex's instances are disposable and re-importing a pack is cheap, so this
is a deliberate, accepted cost rather than an oversight: nothing before 1.0.0
promises to keep reading files written by a different pre-1.0.0 build.

Once Ephervex reaches 1.0.0, this ADR no longer applies. A shape change that
cannot stay additive bumps its constant as it would have before this decision,
and the version becomes a real compatibility boundary between released builds
instead of a development artifact.

The launcher index's pre-1.0 migrations are collapsed into one initial schema.
Migration 2 is reserved for the first compatible schema change after the 1.0.0
boundary.

## Consequences

- `RECIPE_VERSION` moves from 5 back to 1. A pre-1.0.0 build now refuses any
  on-disk recipe stamped 2 through 5, including every one written by a tagged
  release through 0.7.0; the recovery path is re-import.
- `CONFIG_SCHEMA_VERSION` and `METADATA_VERSION` were already at 1 and need no
  code change, but are equally frozen: neither may be bumped before 1.0.0.
- The SQLite index, disposable caches, and generated analysis state also reset
  to 1. Existing pre-1.0 copies with higher values are refused or rebuilt by
  their existing recovery policy.
- A recipe-shape change that cannot default cleanly, such as a new required
  field or an enum variant an older pre-1.0.0 build cannot parse at all, is
  deferred until 1.0.0 rather than shipped behind a version bump.
- `docs/internal-format-versions.md` records the frozen value and links here
  instead of carrying its own open question about resetting the recipe
  version.
