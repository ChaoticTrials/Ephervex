# ADR 0012 - Provider-neutral recipes are the pack import boundary

**Status:** Accepted

## Context

Modrinth and CurseForge describe the same eventual instance through different
manifests. Their names for loaders, file identities, optional files,
client/server applicability, and physical override directories do not line up.
Letting those shapes flow past parsing would require separate installers and
separate versions of "Create Server" that could drift in selection and layering
behavior.

A client import also has to preserve enough information to build its server
after the original archive has been deleted. Copying the
client's current `mods/` directory cannot do that: it has neither server-only
files nor the original environment requirements, and the user may have changed
it since import.

## Decision

Every supported provider translates its validated input into one versioned
`PackRecipe`. The recipe contains only concepts the materializer needs:

- exact Minecraft and loader selections;
- logical instance paths, verified hashes, download mirrors, and optional size;
- independent client and server requirements for each file;
- the recorded human selection for optional files, distinct from no decision;
- ordered common, client, and server override layers, including whether a layer
  was retained;
- explicitly nested provider file identities and provider recipe metadata for
  facts that have no neutral meaning.

Provider field names and archive conventions do not appear at the recipe's top
level. Analysis validates and normalizes without writing. A single
provider-neutral materializer then downloads through the shared verified cache,
copies selected files into the instance, and applies common overrides before
the selected side's overrides. `ephervex-core` applies global managed client
preferences after that materializer completes.

Each imported instance stores the normalized recipe at
`.launcher/source/recipe.json`. It also retains the provider's original
manifest byte for byte under `.launcher/source/<provider>/`, plus every
override layer a later server requires. The recipe, not the original archive or
the mutable client directory, is the source for subsequent side planning.

## Consequences

- CurseForge import adds a provider translation without adding another
  installer, selection engine, or server planner.
- The server plan survives process restart and deletion of the original pack.
- Optional files default off unless a human selection was recorded; `None` and
  an explicit `false` remain distinguishable.
- Keeping the original manifest preserves evidence that normalization may omit
  and gives a future migration something authoritative to re-read.
- Retaining common and server overrides costs instance space. Client overrides
  are not duplicated because Modrinth import materializes them in the client;
  their recipe entry records that they were not retained.
- A newer incompatible recipe version is refused rather than partially read.

## Amendment: the recipe describes an instance, not only an archive

ADR 0013 extends this boundary. A mod installed through the mod browser is part
of what an instance is supposed to contain and has to reach a server built from
the recipe, so managed mods are entries in the same `PackRecipe`, told apart
from an archive's own files by a per-file origin and an explicit provider on the
file identity. An instance created by hand acquires a recipe the first time
something is installed into it.

Nothing above changes. Analysis still validates without writing, one
materializer still installs, and provider vocabulary still stays nested. What
changes is the reading of "source": the recipe records what this instance is
meant to be, of which an imported pack is the first and largest contributor.
