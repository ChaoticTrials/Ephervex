# ADR 0022 - CurseForge uses a user-supplied API key, and its responses are never persisted

**Status:** Accepted (its no-embedded-key decision amended by [ADR 0030](0030-ephervex-ships-its-own-curseforge-key-as-a-fallback.md))

## Context

CurseForge is reached through its official third-party API. Two of its
properties are unlike every other source Ephervex reads.

First, the API requires a key, and a key is granted to an applicant by
CurseForge/Overwolf rather than obtained programmatically. Alternatives exist -
unofficial proxies, third-party mirrors, an application-owned key embedded in
the binary - and all of them either route a user's traffic through a party
neither the user nor the project controls, or ship a shared credential that
cannot be revoked per user and would be extracted from the first release
tarball.

Second, the published third-party API terms prohibit saving or caching data
obtained through the API. Every other metadata source in this launcher is cached
precisely so that a provider outage degrades the launcher instead of stopping
it, and the `MetadataCache` exists to make that automatic. Applying the house
pattern here would violate the terms Ephervex is using the API under.

## Decision

**The credential is the user's.** Ephervex asks the user for a CurseForge
third-party API key, stores it through the credential store of ADR 0004, and
sends it in the `x-api-key` header of each request. The launcher ships no key,
implements no proxy, and does not read the CurseForge Upload API's separate
token.

The transport obtains the key through a `CurseForgeCredentialSource`, which
yields an opaque secret and nothing else. Replacing the *source* later - with an
approved application-owned credential, say - is a second implementation of that
trait, not a change to the transport.

**Nothing the API returns is written down.** The CurseForge client uses no
`MetadataCache`, defines no cache key, has no time-to-live, and creates no
namespace under the metadata store. Responses are parsed in memory, used by the
operation that asked for them, and dropped.

**The credential is scoped to the origins it was issued for.** The API client
will not follow a redirect that leaves the API origin. An artifact download
carries the key only to CurseForge's own explicitly listed delivery hosts, and
is made through a client whose redirects cannot leave that list, so no response
can direct the launcher to hand its key to another host. `x-api-key` is attached
per request rather than installed as a default header, so no unrelated request -
Mojang's, Modrinth's, a managed JDK's - can inherit it by sharing a client.

## Importing a CurseForge pack

A CurseForge export manifest names each of its files by project id and file id,
and nothing else. That produces four decisions the rest of this launcher depends
on.

**Identity is exact, and it comes from the user's own manifest.** An import
resolves `project P, file F` against the endpoint that answers about exactly
that file, and checks the answer against the question before using it. Nothing
lists a project's versions and picks from the list; a declared file that no
longer exists fails the import rather than being replaced by a newer one. The
durable record is the pair the *manifest* declared - CurseForge's release
identifier is its file id, so the provider-neutral `version_id` carries it
rather than a version parsed out of a display name.

**Pack inclusion is not side support.** A manifest's `required` flag says
whether a file is part of the pack as installed. It maps onto the client side -
`required: false` becomes an optional pack choice - while the server side of
every entry stays `unknown`, because the export format states nothing about
servers. Reading `required: true` as "required on the server" would put a
client-only mod into a reconstructed server on the strength of a flag that never
meant that, and the existing reconstruction path already knows how to ask about
`unknown` rather than assume.

**Nothing an API answer says is written down; the normalized recipe is.** What
survives an import is the provider-neutral `PackRecipe` plus the user's own
manifest, retained verbatim under `.launcher/source/` like every other
provider's. The recipe carries the exact ids and the minimum the launcher's own
model needs to install and verify an artifact - a name, a size, a digest, and
the address it was fetched from. The response document itself is parsed, used,
and dropped, exactly as this ADR requires of every other CurseForge request.

**Artifacts use the shared verified store.** A downloaded jar is content the
provider published a hash for, addressed in `downloads/` by that hash, subject
to the ordinary reusable-cache budget and collection. It is not API response
data.

**Retained metadata identifies an artifact; it does not serve one.** Everything
a reconstruction needs to know is in the recipe, so nothing rebuilt from retained
desired state ever asks the API what an instance contains. Whether the *bytes*
still have to be fetched is a separate question with two answers. A verified copy
in the shared store is installed with no credential, no request, and no
configuration, which is what keeps an installed instance and its linked servers
working on a machine that has never configured CurseForge. A copy the cache
budget has since collected has to come back from the address the recipe records,
and CurseForge serves that address only to an authenticated request - so the key
is required again at that point. A missing, unreadable, or refused key is
reported as the same actionable setting a first import reports; a delivery host
answering `401` or `403` to a request that carried the key is read exactly as the
API answering the same way, from one definition of what those statuses mean. The credential is chosen per artifact
from the provider identity the recipe records, so a desired state naming several
catalogs offers each secret only to its own files.

## Browsing CurseForge mods

The mod browser is one browser with a catalog selector, not a second page. A
search, a project, a version list, and an install all travel the existing
provider-neutral path (the same models, the same resolver, the same install
planner, the same retained recipe), and CurseForge is one more implementation of
the mod platform provider trait of ADR 0005. Four decisions are specific to it.

**The catalog is part of the request.** A project id is meaningful only together
with the catalog it was read from, so the provider travels with every search,
review, install, dependency question, and version change rather than being an
application-wide setting. An instance can hold mods from both catalogs, and each
is asked only about its own: a Modrinth install never sees a CurseForge project
as already present, and a dependency graph never crosses catalogs.

**Only Minecraft's Mods class is browsable.** A search carries the Mods class id
discovered from `GET /v1/categories` for this operation, never a hard-coded
number. The filter is not taken as proof: every project document that comes back
is classified again against the classes that same operation asked for, by the
one implementation a pack import also uses. A project whose class cannot be
confirmed as Mods is left out of a result list and refused by name when it is
asked for directly. Resource packs, worlds, shader packs, modpacks, and plugin
projects are therefore not installable through the browser, and nothing is
inferred from a filename or a category.

**Compatibility is what the provider publishes, scoped to the exact target.**
CurseForge pages its file listings, so a project's whole release history cannot
be enumerated to answer one question: a search and a version list are filtered
server-side on the instance's exact Minecraft release and on the loader family,
and neither is ever widened. Forge and NeoForge map to their own documented
`modLoaderType` values and never to each other's or to a loader this launcher
does not support. The cost is stated rather than hidden: a file that does not fit
is not returned at all, so nothing explains an incompatible release the way
Modrinth's full listing can. CurseForge's file metadata expresses loader
*family*, not an exact Forge or NeoForge build, and this launcher does not claim
otherwise.

**A file with no third-party download is refused, never skipped.** Silently
leaving a restricted file out of a version list would offer a different release
than the newest one that fits, which is the substitution this launcher exists not
to make. Such a file fails with the same typed refusal a pack import produces,
naming the exact project and file.

What is installed is recorded the way a CurseForge pack import records it:
provider, project id, and (because CurseForge's release identifier is its file
id) that id in both the `version_id` and `file_id` fields, beside the digest,
size, and address the launcher verifies and reconstructs from. Search results,
project documents, categories responses, file listings, and pagination metadata
are parsed, used, and dropped. Searching twice queries CurseForge twice.

## Consequences

- CurseForge features are unavailable until a user configures a key, and that is
  reported as an actionable setting rather than as an outage. Every other part
  of the launcher is unaffected.
- A CurseForge outage stops CurseForge work outright: there is no stale document
  to fall back on, because keeping one is what the terms forbid.
- Repeating an operation repeats its requests. The API's rate limits, not a
  local cache, are the budget.
- Verified downloaded artifacts are a separate question. A file whose hash the
  provider published is content Ephervex is entitled to keep under its ordinary
  shared-store rules; that is not API response data and is not governed by this
  decision.
- A 401 or 403 is reported as "the configured key may be invalid or no longer
  authorized" rather than as a network failure, because those are the two
  outcomes a user can act on.
- Recognizing a CurseForge export and reading what its manifest says stays
  local, so a launcher with no key configured still identifies the archive,
  names the pack, and reports its exact Minecraft version, loader build, and
  file counts. Only resolving those files needs the credential, and that is
  reported as a blocker on an otherwise complete description.
- A project whose owner has turned off third-party distribution can be neither
  imported nor installed from the browser. That is the author's decision: the API declines to say where the
  file is, there is no second address, and assembling a CDN path out of the ids
  would be routing around it. The import stops and says which project it was.
- The export format states no destination directory for a declared file, and the
  exact-file response states none either, so every declared file is installed
  into `mods/`. A pack shipping a resource pack or a shader as a *declared file*
  rather than in its overrides would land it there. Deciding otherwise would mean
  either a second request per project or a table of undocumented class ids, and
  neither is a schema this launcher can prove.
