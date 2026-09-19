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

`RECIPE_VERSION`, `CONFIG_SCHEMA_VERSION`, and `METADATA_VERSION` stay at 1 for
as long as Ephervex has not shipped 1.0.0, no matter how many times the format
each one stamps changes shape in the meantime. A shape change made during the
freeze has to stay additive - a new or changed field defaults when absent - so
that every other pre-1.0.0 build can still read the file. There is no version
bump available during the freeze to signal "refuse this file instead of
misreading it," so a change that cannot be made additively is not made through
a version bump.

Re-import is the recovery path for a change that cannot be additive, or for an
on-disk file stamped above the frozen value by an earlier pre-1.0.0 build.
Ephervex's instances are disposable and re-importing a pack is cheap, so this
is a deliberate, accepted cost rather than an oversight: nothing before 1.0.0
promises to keep reading files written by a different pre-1.0.0 build.

Once Ephervex reaches 1.0.0, this ADR no longer applies. A shape change that
cannot stay additive bumps its constant as it would have before this decision,
and the version becomes a real compatibility boundary between released builds
instead of a development artifact.

This decision does not cover the launcher index's migration files
(`crates/storage/src/migrations/`). Whether the existing numbered migrations
collapse into a single initial schema before 1.0.0 remains open, as
`docs/internal-format-versions.md` describes.

## Consequences

- `RECIPE_VERSION` moves from 5 back to 1. A pre-1.0.0 build now refuses any
  on-disk recipe stamped 2 through 5, including every one written by a tagged
  release through 0.7.0; the recovery path is re-import.
- `CONFIG_SCHEMA_VERSION` and `METADATA_VERSION` were already at 1 and need no
  code change, but are equally frozen: neither may be bumped before 1.0.0.
- A recipe-shape change that cannot default cleanly, such as a new required
  field or an enum variant an older pre-1.0.0 build cannot parse at all, is
  deferred until 1.0.0 rather than shipped behind a version bump.
- `docs/internal-format-versions.md` records the frozen value and links here
  instead of carrying its own open question about resetting the recipe
  version.
