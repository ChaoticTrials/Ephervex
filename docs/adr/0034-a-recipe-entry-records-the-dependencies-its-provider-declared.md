# ADR 0034 - A recipe entry records the relations its provider declared

**Status:** Accepted (amends ADR 0022 for relation data)

## Context

An instance's recipe is its desired state. It records, per file, the exact
addresses, digests, size, sides, origin and catalog identity of every mod the
instance is supposed to contain (ADR 0012, ADR 0013, ADR 0019). One thing it
does not record is what those mods need from each other.

The consequence shows up the moment a user turns a mod off. Disabling LibX in an
instance that also has Skyblock Builder, Sky GUIs and UtilitiX breaks all three,
and the launcher has nothing to say about it: `ModOrigin::RequiredDependency`
records that a mod arrived as somebody's dependency and never whose. ADR 0020
already named this gap - "a mod dragged in as a dependency two operations ago
carries no record of *who* wanted it" - and worked around it by re-resolving the
whole instance against a live catalog whenever the question came up.

Re-resolution is the wrong instrument here. It needs the network for a question
about files already on disk, it cannot answer at all for a mod the resolver never
touched, and it is far too slow to sit behind a toggle. It also cannot answer the
question a bisect asks a hundred times: which mods may be turned off together
without producing a configuration that fails to load for the wrong reason.

The data itself is never missing. Modrinth states a version's dependencies and
CurseForge states a file's relations, and Ephervex already receives both - when
it installs a mod, when it resolves a CurseForge pack's files, and on every
enriched mod listing. It reads them, uses them once to build an install plan, and
throws them away.

## Decision

A recipe entry records the relations its catalog declared about that file.

**Forward, on the entry the catalog described.** `PackFileRecipe::relations`
holds what *this* file says it needs. The reverse direction - which installed
files require a given project - is derived by `PackRecipe::required_by`.

Storing the reverse instead was considered and rejected. A reverse edge lives on
a different entry from the one the catalog described, so installing one mod would
have to rewrite other mods' records and removing one would have to scrub them.
That turns ADR 0020's single-entry transaction into a whole-recipe rewrite with a
second invariant to preserve, and it makes absence dishonest: a library would
look independent whenever some unrelated mod's install failed to write back to
it. Forward edges keep each entry self-describing, which is also what lets
`ephervex-core` copy an entry into a linked server's own recipe without carrying
dependents that do not exist there.

**Unasked and empty are different.** The field is `Option<Vec<RecipeRelation>>`.
Absent means nothing has asked the catalog; `Some([])` means it answered that
this file needs nothing. A bare `Vec` would collapse them, and every warning
built on the collapsed value would be confidently wrong rather than merely
incomplete - the failure ADR 0020 exists to avoid. A mod whose relations were
never recorded is reported as unknown, with a refresh offered, and is never
treated as independent.

**The catalog is the entry's own.** A relation carries no provider field: it
belongs to the entry's `ProviderFileIdentity::provider`. A dependency graph never
crosses catalogs, so a second place to record a provider would only be a second
place for it to be wrong.

**Nothing the catalog stated is thrown away.** What a relation names is a typed
choice rather than a bag of optional ids: `RelationTarget::Project`, carrying the
exact release where the catalog pinned one, or `RelationTarget::File`, for a
relation that names only a jar. CurseForge always produces the first, because it
states relations between projects. Modrinth produces either. A relation naming a
jar is matched against an installed file by name, case-insensitively, rather than
discarded for lacking a project; one that named a project is matched by project
alone, because falling back to a file name there would match a file the catalog
never mentioned. A relation that names neither cannot be matched against anything
and is not representable.

**A pinned release is recorded with its project.** Modrinth lets a relation give
a release and no project. The release itself states which project owns it, so
the catalog layer reads that and the relation is recorded as
`RelationTarget::Project` carrying the owner and the pin. A new target variant
was rejected because an older build could not parse it (ADR 0033). A release the
catalog does not answer for has no project to record: the relation keeps only a
jar name where it gave one, and is otherwise left out.

**The field is additive.** `RECIPE_VERSION` stays 1 (ADR 0033): an absent field
defaults, so a recipe written by a build that predates this reads correctly and a
build that predates it reads one written now.

## Why this does not breach ADR 0022

ADR 0022 records that CurseForge's third-party API terms prohibit saving what the
API returns, and made CurseForge the one provider deliberately outside the
metadata cache: no cache key, no time-to-live, no namespace under the metadata
store, responses held in memory for the operation that needed them and then
dropped. Recording a CurseForge file's relations in `recipe.json` stores more
CurseForge-derived data than that ADR contemplated, so it is decided here rather
than assumed.

The distinction is what the data is, not how much of it there is.

- **A recipe entry is desired state, not a response.** It already records a
  CurseForge project id, a file id, a download address, a size and a digest,
  because those are the answer to "what is this instance supposed to contain".
  A relation is the same category of fact: it is why this file is here and what
  breaks without it.
- **It is never served in place of an API call.** Nothing resolves a version from
  it, builds an install plan from it, lists a project's files from it, or falls
  back to it when CurseForge is unreachable. It has no key, no time-to-live, no
  namespace and no freshness envelope. It has none of the properties that make
  something a cache, and the CurseForge client's cache-free behavior is unchanged.
- **It answers a question the API cannot.** "Which files already in this
  directory stop working if this one stops loading" is about the contents of one
  instance's `mods/`. CurseForge does not know what is in that directory.

The cost is stated rather than hidden: a recorded relation can go stale if an
author edits it later, and Ephervex will not notice until the user refreshes.
A stale relation produces an over-cautious warning. It never produces a wrong
install, because nothing installs from it.

## Consequences

- Dependency questions are answered offline, from the instance's own state, for
  every mod whose relations have been recorded.
- Behavior is identical for Modrinth and CurseForge. A Modrinth `.mrpack` index
  is the one source that records nothing, because the format states a path,
  hashes and download addresses and declares no relations at all; those entries
  stay unasked until a refresh.
- Refreshing a host's mod list records the relations of every exact release it
  looks up, through the same path as the explicit dependency refresh. It asks
  nothing it did not already ask, and a release no answer covers keeps what it
  had.
- A mod with no catalog identity has nothing to ask about, and stays unasked
  permanently. Any feature built on this graph has to say so rather than treat
  the jar as independent.
- ADR 0020's orphan collection is unaffected. It still proves what is still
  needed by re-resolving, and still collects nothing when a catalog cannot
  answer.
