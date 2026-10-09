# ADR 0013 - Mods installed through a provider are recipe entries

**Status:** Accepted

## Context

ADR 0012 established the provider-neutral recipe as the record of what an
imported pack said, precisely so that a server built later is built from the
pack rather than from a client directory the user has since edited.

Installing a mod through the mod browser breaks that framing. Such a mod was
never in any archive, but it is unquestionably part of what the instance is
supposed to contain: a user who adds a library to reproduce a crash and then
creates a server expects the server to have it. Deriving that from the client's
current `mods/` directory is exactly what ADR 0012 rejected - the directory also
contains jars the user dropped in by hand, and no longer contains the ones they
deleted while bisecting.

The obvious alternative is a second file beside the recipe holding installed-mod
records. That produces two descriptions of one `mods/` directory which can
disagree about the same jar, and forces every consumer - side planning, server
creation, the instance UI - to read and merge both.

There is also a question the launcher cannot answer for itself. A Modrinth
project declares `client_side` and `server_side`, and one of the values those
fields can take is `unknown`: the author declared nothing. Reading that as
"required" would assert something they declined to assert.

A release can declare its own sides, and a project's declaration only summarizes
its releases. The selected release's declaration decides what is recorded, and
the project's is used only for a side the release leaves undeclared.

## Decision

A mod installed through a provider is an entry in the same `PackRecipe`, and the
recipe describes the *instance's desired state* rather than only an archive's
contents.

- `PackFileRecipe` carries `origin`, distinguishing a pack-declared file from a
  user-selected mod, a required dependency, and a selected optional dependency.
- `ProviderFileIdentity` carries the `provider` alongside the project and
  version ids, which is what makes an entry re-resolvable.
- An instance created by hand acquires a recipe the first time something is
  installed into it, describing the instance it already is.
- `SideRequirement` gains `Unknown`, recording an undeclared side as undeclared.
  It never installs on its own. The `.mrpack` format cannot produce it: that
  format has three environment values, and an omitted `env` object is defined to
  mean required on both sides.
- The recipe format version is 2. A version 1 build would drop these entries
  silently and build a server missing them, so it refuses to read the file
  instead. Version 1 recipes still read; the new fields default.

Two things are deliberately excluded.

**Jars the launcher did not install.** Nothing enumerates `mods/`. A file with
no provenance is left alone rather than hashed and given an invented record: a
fabricated entry would claim a project nothing can look up, and would put that
jar into a reconstructed server on the strength of a guess. `ModProvider` has no
`Unknown` variant for the same reason - absence of an identity already says
"nothing trustworthy is known", and saying nothing is the honest answer.

**Changing an installed mod's version.** `PackRecipe::record_managed` is
insert-only and refuses a project already recorded at a different version.
Replacing it is a mod update, which this build does not implement, and an upsert
would have to decide the fate of the old, differently-named jar. Making the
invariant structural means no code path can leave two jars for one project.

When a linked server is reconstructed, side applicability follows a stricter
operation-specific policy:

- server-required entries are included;
- server-unsupported entries are excluded;
- optional entries are included only when the retained recipe records an
  affirmative selection; and
- `Unknown` entries are omitted unless the current server-creation request
  explicitly confirms their logical paths.

An old affirmative selection is not sufficient for `Unknown`. The recipe has
one recorded selection per file, and that selection may have been made while
materializing the client. Treating it as server confirmation would turn an
undeclared server side into an accidental requirement. Omitted unknown entries
remain visible as server attention reasons, so conservative reconstruction is
diagnosable rather than silent.

## Consequences

- Side planning and "Create Server" work unchanged: they read the recipe, and a
  browser-installed mod is in it.
- There is one source of truth for the contents of `mods/` that the launcher
  claims responsibility for, and one file to keep consistent.
- Installation writes the recipe last, after every jar is in place, and a
  failure to write it fails the installation and removes the jars that
  installation created. A recipe naming absent mods would make server creation
  build something that never existed.
- Adding a provider adds a `ModProvider` variant and a translation; it does not
  change the recipe, the installer, or the retained-source layout.
- Standalone server creation installs only the exact selected base. Linked
  server creation installs that same base and then materializes this recipe in
  the same staging transaction, using the conservative applicability policy
  above.
