# Ephervex Architecture

Ephervex is a Linux-first Minecraft launcher and developer tool for disposable
bug-reproduction environments. This document describes the current workspace,
its dependency direction, the main workflows, and the invariants that keep
imports and launches reproducible. Lasting design decisions and rationale live
in [`docs/adr/`](adr/).

## Dependency direction

Frontend crates call application services in `ephervex-core`. They do not call
storage, provider, download, or launcher implementations directly.

```text
gui ─┐
     ├──> core ──> domain/provider crates ──> model
cli ─┘
```

- `ephervex-model` owns shared domain values and performs no I/O.
- Domain and provider crates depend on the model and on lower-level domains
  only when their behavior requires it.
- `ephervex-core` composes storage, authentication, Java selection, loader,
  Minecraft, options, and pack services into frontend-facing operations.
- `ephervex-cli` and `ephervex-gui` depend on `ephervex-core`; neither frontend
  depends on the other.

The frontend manifests intentionally omit provider crates. `App` also keeps its
`Registry` private and exposes only the records, requests, and structured error
details a frontend needs. This makes the boundary visible in manifests and
prevents UI code from acquiring storage or provider policy accidentally.

## Crate responsibilities

| Crate | Responsibility |
|---|---|
| `ephervex-model` | Typed IDs, account and instance values, loader selections, hashes, storage classes, and operation progress |
| `ephervex-storage` | XDG paths, versioned configuration, SQLite migrations and registry, instance metadata, reconciliation, launch locks, duplication, folder layout, and bounded diagnostic-file discovery and reading |
| `ephervex-i18n` | Fluent resources, locale selection, typed arguments, and message lookup |
| `ephervex-download` | HTTP policy, bounded concurrency, retry classification, cancellation, hash verification, atomic publication, metadata caching, and shared cache layout |
| `ephervex-minecraft` | Mojang/Piston metadata, per-release language catalogs, rule evaluation, artifact planning, Vanilla installation, command construction, process control, and log parsing |
| `ephervex-java` | Local runtime discovery, version probing, compatibility rules, runtime selection, and managed Adoptium installation |
| `ephervex-auth` | Microsoft, Xbox, and Minecraft Services authentication; offline identities; account sessions; keyring and memory-only credentials |
| `ephervex-loaders` | Vanilla, Forge, and NeoForge metadata resolution and client/server installation |
| `ephervex-options` | Line-preserving `options.txt` parsing, the generated per-release compatibility dataset and its remote feed, and version-aware application of global preferences |
| `ephervex-pack` | Format detection, hardened archive handling, provider-neutral recipes, Modrinth and CurseForge manifest parsing, optional selection, materialization, and managed-mod installation |
| `ephervex-mod-platform` | Modrinth and CurseForge project/version catalogs behind one provider trait, the authenticated CurseForge third-party API transport, compatibility verdicts, and provider-neutral dependency resolution |
| `ephervex-server` | Dedicated-server properties, EULA enforcement, process identity, foreground supervision, control channel, and bounded capture |
| `ephervex-core` | Application services and error classification shared by frontends |
| `ephervex-cli` | Command-line frontend and human/JSON rendering |
| `ephervex-gui` | Slint desktop frontend for instances, servers, accounts, runtimes, mods, preferences, and diagnostics |

Provider-neutral types stay neutral. A Modrinth project ID or CurseForge file ID
belongs in an explicit provider/source field, not in a generic loader, file, or
instance identity.

## Instance and shared-storage model

Instance directories contain mutable reproduction state: configuration, saves,
mods selected for that instance, logs, crash reports, and `.launcher` metadata.
Temporary instances live under the operating system's temporary directory;
persistent instances live under the configured data root.

Immutable or reproducible artifacts live in shared stores:

- downloads are content-addressed by verified digest;
- Minecraft libraries retain Maven-compatible paths where the game or loader
  metadata requires them;
- assets use Mojang's object layout plus an atomic launch-view copy of each
  verified index;
- client JARs are content-addressed so republished metadata cannot silently
  reuse stale bytes;
- native extractions are keyed by version and platform; and
- managed runtimes are installed in a shared runtime store.

The content-addressed download stores additionally carry a configurable byte
budget with automatic collection; see the reusable artifact cache budget below.

A file appears at a final shared-store path only after it has been completely
written and verified. Incomplete work remains in staging paths.

The registry writes instance metadata before publishing the corresponding
SQLite row. Reconciliation can therefore rebuild an index entry from an intact
instance. A missing temporary root is treated as expected operating-system
cleanup; a missing persistent root remains registered and requires attention.

Creating a client instance and finishing one are separate steps, because the
work between them -- laying down a pack, downloading mods, applying preferences
-- takes as long as it takes and can be interrupted. Creation writes no index
row; publication writes it. Every read, listing, launch, and mutation path
resolves through the index, so an unpublished instance is absent from all of
them rather than being something each caller has to remember to check for.
Reconciliation reads directories instead of the index, so it gets a second
signal: a marker written into `.launcher/` at creation and cleared at
publication. A marked root is reported and left exactly where it is, never
indexed and never deleted. The marker is cleared before the row is inserted, so
the remaining crash window leaves a fully materialized instance that
reconciliation is right to adopt.

For servers, `.launcher/server.json` is the canonical entity and SQLite is a
query index plus recovery copy. New servers are fully built in a hidden sibling
root and published with one directory rename. Existing metadata wins during
reconciliation; corrupt or missing metadata leaves the SQLite recovery copy
visible but unusable for launch or mutation. Multiple roots claiming one server
ID are a blocking conflict rather than a selection opportunity (ADR 0014).

## Reusable artifact cache budget

The cache under the XDG cache root is a disposable optimization, never a source
of truth. Removing an entry costs a re-download and nothing else, because every
installed workspace owns its own files: pack files and mods are copied into the
instance or server, a managed runtime is unpacked into the data root, and a
loader installer has already run by the time its jar is cacheable.

That is what makes a byte budget expressible, and it also bounds what the budget
governs. Only the two stores addressed purely by verified content hash are in
scope: `downloads/` and `installer-cache/`. Every other store under the cache
root is excluded, for reasons of ownership rather than size - `libraries/` is
what a retained server's own `libraries` link resolves through, `assets/` is the
directory a running game is handed, `natives/` is unpacked rather than
downloaded, `metadata/` holds provider documents and their freshness envelopes,
and `artwork/` holds entries no provider publishes a hash for. The provider
metadata cache has its own addressing and its own freshness policy and is not
governed by this limit.

The budget is a `u64` byte count or Unlimited, defaulting to 10 GiB. A settings
screen offers presets up to 100 GiB, a custom size in mebibytes, gibibytes, or
tebibytes, and Unlimited; the stored value is always bytes, so no unit choice
changes what the file means. A size that cannot be represented is refused with a
typed error and the previous budget stays in force. Existing configuration files
that predate the setting load with the default.

Collection is high/low-water rather than a single threshold: it starts only when
measured usage is strictly above the configured limit and then removes entries
until usage is at or below approximately four fifths of it, so a cache near its
budget does not evict after every download. Unlimited runs no collection at all,
while still supporting measurement and a manual clear.

Eviction order is oldest publication first, with the path as a deterministic
tie-break. This is not LRU and is not described as one: the cache is
content-addressed, so a hit opens a file and changes nothing about it, and
`atime` is unusable on desktops that mount with `relatime` or `noatime`. An
entry's modification time is when its bytes arrived, because publication is a
rename and a rename does not touch it.

The budget is soft in both directions that matter. A required artifact is
fetched, verified, and used whatever its size, even when it is larger than the
whole budget; and an operation holds a shared lease on the cache for as long as
it may still resolve, open, copy, or launch a reusable artifact path, so
collection cannot remove one out from under it. The lease is a kernel advisory
lock on a fixed launcher-owned file at the cache root, which makes it hold
between two Ephervex processes sharing one cache as well as between two tasks in
one. Collection takes the same lock exclusively and never waits for it, so a
manual clear reports a busy cache instead of hanging.

Maintenance is a housekeeping policy, not an installation precondition. It runs
after a cache-changing operation has released its lease and when the budget is
lowered. If it cannot run, or cannot remove a particular entry, the operation's
own result stands, the trouble is reported through the launcher's warning path,
and the cache stays over budget - leaving files behind is always preferable to
removing ones whose status is uncertain. A pass reports what it measured, what
it freed, and what it could not, so a partial clear is never presented as an
empty cache.

## Dedicated-server data flow

```text
typed standalone selection or linked client
   │
   ├── exact Minecraft and loader metadata
   ├── Java requirement + loader compatibility rules
   ├── shared verified downloads and libraries
   ▼
hidden sibling root
   ├── server/ base installation
   ├── linked recipe files from shared verified downloads
   ├── retained common overrides, then server overrides
   ├── managed network properties
   ├── retained provider-neutral recipe state
   └── .launcher/server.json
              │ atomic directory rename
              ▼
        published server ──▶ SQLite query index
```

A server may instead be given a published server archive (ADR 0024), and then
none of the reconstruction below runs. The archive is extracted over the
installed loader and the server's content is complete: nothing is resolved, no
side is planned, and no optional file is decided. Available to a standalone
server as well as a linked one. `libraries/`, the launch descriptor, `eula.txt`,
and `.launcher/` never come from the archive -- `libraries` is a link into the
shared store, and extraction containment is lexical, so an entry there would be
written into storage every instance shares. A single wrapping directory is
stripped only when the archive's shape is unambiguous. The server keeps an
ordinary recipe listing every installed jar with a digest the launcher
computed and no download address. Those jars are then identified by digest --
the linked client's retained recipe first, which costs no request, then
CurseForge by its own file fingerprint, then Modrinth by SHA-1 -- and a file
nothing recognizes keeps its digest and no provider. No catalog failure can
fail the installation.

A linked server reads the client's retained recipe and retained override
layers. It never scans or copies the mutable client game directory, so manually
added jars and local debugging changes remain unmanaged. Required content is
included, unsupported content is excluded, and optional content follows the
recorded choice. Entries with undeclared server applicability require explicit
confirmation for the current creation and otherwise become actionable server
attention reasons. The original pack archive is not consulted.

Base installation, recipe files, overrides, managed network properties, source
retention, and canonical metadata all land in the same hidden sibling root.
The final rename remains the sole publication point; failure at any earlier
step removes the staged root and cannot leave an indexed complete server.

Runtime Java precedence is operation-specific path, server override, global
preferred path, then compatible discovered or managed runtime. Minimum memory,
maximum memory, and extra JVM arguments resolve independently from per-server
overrides over global defaults. Compatibility is checked during installation
and every start, including the legacy Forge Java 8 update ceiling.

A start first merges canonical typed network settings and validates global EULA
consent. Only then may it atomically merge `eula=true`. The foreground
supervisor owns the Java child's stdin and both output pipes, the runtime lock,
and a mode-0600 Unix socket in a short, mode-0700 per-user runtime directory.
The runtime record's generation selects the socket without embedding the
potentially long server root in its path. Stop is graceful and never
escalates; kill is explicit and available only through the owner channel.
Linux boot ID, PID start ticks, canonical executable, and executable identity
distinguish a controlled process, an orphan, an unverifiable process, and
proven-stale state. An orphan is reported but never adopted or signaled.

Launcher capture is separate from Minecraft's own `server/logs/` and
`server/crash-reports/`. A bounded memory ring feeds frontends while rotated
JSON Lines segments provide durable combined output. Retention and write
failures are observable events; pipe draining continues when durable capture
is unavailable.

## Linked client direct-connect flow

```text
client ID + server ID
   │ exact canonical relationship and target validation
   ├── stopped server ──▶ existing start service ──▶ foreground supervisor
   └── controlled server ─────────────────────────▶ reuse
                                      │
                         bounded TCP readiness
                                      │
                                      ▼
                         existing client launch service
                                      │
                    Quick Play or legacy game arguments
```

Core derives a client destination from typed server network settings rather
than from frontend assumptions or by rewriting `server.properties`. Localhost
and IPv4 wildcard binds use `127.0.0.1`; an IPv6 wildcard uses `::1`; a
specific custom bind remains exact. The configured nonzero port is preserved.

Only a controlled server is reusable. A stopped or proven-stale server goes
through the ordinary start service and its EULA, Java, persistence, capture,
and process-ownership rules. Readiness is a successful TCP connection within a
bounded deadline while core also observes supervisor exit or loss of a
controlled runtime. Once ready, the ordinary client launch pipeline supplies
account identity, Java, exact loader metadata, artifacts, locking, and process
capture. Client failure or exit sends no server stop action (ADR 0015).

## Minecraft settings compatibility

Global Minecraft preferences are normalized values, never a copied
`options.txt`. Turning one into a line of a particular release's file needs
proof of what that release stores, which is generated rather than written
(ADR 0017).

Two generated documents answer two different questions. The bundled product
catalog (`crates/options/data/minecraft-preferences.json`) says which settings
this build exposes and what product shape each has; it is application-owned and
has no remote form. The compatibility dataset says how one exact release
persists a canonical setting; it is bundled and may also arrive from the remote
feed. A compatibility update can extend an already-shipped preference to a new
Minecraft release, and can never make a new preference exist.

A configured preference is a stable id and one typed value. Configuration is
sparse: an absent id means Ephervex has no opinion about that setting, which is
different from managing it to a value that happens to match. Ids written by a
newer build are carried through a load and save cycle untouched and are never
presented or applied. Global preferences are defaults for instances created or
imported from here on; changing one never revisits an instance that exists, and
duplication keeps the copied instance's own file.

```text
official client JAR
   │ tools/minecraft-options/update.py, static analysis, never executed
   ▼
stable setting registry + current canonical ids + schema families + release mapping
   ├── committed at crates/options/data/minecraft-options.json, compiled in
   └── published to the remote options feed
              │ validated whole, then cached atomically
              ▼
   valid newer cached feed ──▶ else bundled baseline
              │
              ▼
   canonical preference + exact release
      ├── authoritative representation ──▶ written
      ├── unsupported ─────────────────────▶ skipped, reported
      └── unknown ─────────────────────────▶ skipped, reported
```

Releases share a schema family only when their extracted representations are
identical; a version number never establishes equivalence. A release the dataset
does not name is unknown for every setting, which keeps it fully usable while
receiving no managed preference writes. Values outside a release's proven domain
are skipped and reported rather than clamped, and unknown and mod-added lines
always survive an edit.

The set of languages a release offers is Mojang's data, resolved from that
release's own `pack.mcmeta` asset object and cached per release alongside the
version manifest. Without a catalog the managed language preference is not
applied rather than applied on an assumption. Instance creation and pack import
consult only an already-cached catalog; release/catalog browsing performs the
network refresh.

## Launch data flow

```text
CLI or GUI
   │
   ▼
ephervex-core::App
   ├── storage: instance, preferences, launch lock
   ├── auth: account session and player identity
   ├── loaders + minecraft: exact metadata and artifact plan
   ├── java: compatible local or managed runtime
   └── download: verified shared artifacts
               │
               ▼
        redacted launch command
               │
               ▼
       child process and log stream
```

The requested Minecraft and loader versions remain exact throughout this flow.
Missing metadata, unsupported loader combinations, unresolved placeholders, and
incompatible Java runtimes are failures; none causes a fallback to a nearby or
"latest" version.

Minecraft metadata shapes are preserved until the layer that understands them.
Legacy argument strings and modern argument arrays remain distinct. Rule-gated
libraries retain their rules. Forge and NeoForge child metadata must be merged
with the exact `inheritsFrom` parent before it is installable or launchable.

`ephervex-storage` holds an instance launch lock for the full operation. The
lock records the launcher process and kernel start time to distinguish a live
owner from PID reuse, then records the game PID after process creation.

Authentication and Java failures are classified in `ephervex-core`, so a
frontend can offer sign-in, session-only login, or managed Java without
matching on provider-specific error trees.

## Pack-import data flow

An archive reaches this flow from the frontend's file dialog or from a drop onto
the desktop window; both hand `ephervex-core` a path and nothing else. Detection
reads the archive's contents rather than its name, so how the path was named
never changes how the pack is read.

```text
untrusted archive
   │
   ├── format detection and bounded archive inspection
   ├── provider parser and exact-version validation
   ├── provider file resolution, where a manifest names files by id alone
   ▼
provider-neutral PackRecipe
   │
   ├── optional-file selection
   ├── verified shared downloads
   ├── common overrides
   ├── client or server overrides
   └── managed client preferences
   ▼
instance plus retained source metadata
```

Parsing and analysis do not write an instance. Materialization begins only
after the recipe and optional selections are valid. The normalized recipe is
stored under `.launcher/source/recipe.json`; provider metadata and the override
layers needed to reproduce the other side are retained explicitly.

The recipe is the operational boundary shared by import formats. Provider
field names, archive conventions, and IDs remain nested provider metadata.
Common/client/server applicability and optional selection have provider-neutral
representations, preventing each importer from inventing a separate installer.

Two formats reach that boundary by different routes. A Modrinth index carries
each file's path, hashes, and download addresses, so parsing produces a complete
recipe. A CurseForge export names each file by project id and file id only, so
parsing produces a recipe plus a list of entries the provider still has to
describe; `ephervex-core` resolves each against the endpoint that answers about
that exact file and turns the result into ordinary recipe entries. Recognizing
and describing the archive stays local either way - only the resolution step
needs a credential, and analysis reports that as a blocker rather than failing
to identify the file (ADR 0022).

A recipe entry records the exact addresses, digests, and size of its artifact,
which is everything needed to identify and verify it: nothing built from retained
desired state asks a provider what an instance contains. Serving the bytes is a
separate question. Materialization probes the shared artifact cache first, and a
verified entry is installed with no credential and no request, which is why an
installed instance and its linked servers keep working with no provider
configured. Only an artifact the cache no longer holds is fetched, and only then
is a credential asked for - per file, from the provider identity that file's own
recipe entry records, so a desired state naming several catalogs offers each
secret only to its own artifacts. A refusal is attributed to the credential only
when the request actually carried one and the provider uses that status to refuse
credentials, so an ordinary `403` from a host that was never offered a secret
stays an ordinary download failure. A cache that cannot be *read* is neither: it
is a local storage failure, reported as itself rather than recovered from by
fetching.

## Mod-installation data flow

```text
instance record
   │
   ├── exact Minecraft version + loader          (inputs, never written)
   ▼
provider catalog
   ├── search, narrowed by facets
   ├── compatibility decided per concrete version
   ▼
dependency resolution to a fixpoint             (no writes)
   ├── constraints merged per project
   ├── conflicts, unresolvable requirements, and
   │   already-installed version changes refused here
   ▼
destination pre-flight                          (no writes)
   ├── intra-plan filename collisions refused
   └── occupied destinations refused
   ▼
verified shared downloads ──▶ instance mods/ ──▶ recipe.json
```

Search facets narrow the candidate set; they do not decide compatibility. A
project satisfies a version facet and a loader facet through two different
versions, so the verdict always comes from a concrete version's own
`game_versions` and `loaders`. Showing incompatible results widens the upstream
query rather than filtering locally, because a filter would remove the rows that
need annotating. Per-search version lookups are bounded, and the page size is
clamped.

Exact selections are preserved. A pinned version that does not fit is refused
rather than moved to a neighboring release, two mods pinning one project to
different versions is a conflict rather than an arbitration, and a required
dependency naming only a file fails rather than being skipped or given an
invented project identity. The instance's Minecraft version and loader are
inputs throughout; no graph is satisfied by changing them.

Mods installed through a provider are entries in the same recipe (ADR 0013), so
a server built later contains them. No automatic operation overwrites or deletes
a jar the launcher did not install. Installation writes the recipe last; any failure -
including a failure to write the recipe - removes the files that operation
created and leaves everything else untouched.

A jar somebody put in `mods/` themselves is listed rather than hidden: it loads
into the game, so a list that omitted it disagreed with the folder about what
runs. It is listed as hand-placed, and that origin refuses every mutation, so
the launcher still never changes a file it did not install. The one thing
offered for such a jar is deleting it, which only an explicit request from the
user does; it removes both the enabled and disabled spellings and drops the
recipe entry identification wrote, so no record outlives the file. Jars dropped
onto the mod list are copied in on the same terms: staged inside the instance
and published by a rename, listed as hand-placed, and deliberately unrecorded,
because nothing about a file handed over says which project it is. Identifying one is
an explicit refresh rather than part of listing, because listing has to stay a
directory read. Modrinth is asked first there, by the SHA-1 the launcher
computes, and CurseForge second by fingerprint - the reverse of a server archive
(ADR 0024), which is a CurseForge problem by construction. A jar a catalog
recognizes is recorded in the recipe with its identity, `client` required and
`server` undeclared, so creating a server asks about it in the same way it asks
about a CurseForge pack's own files rather than assuming. A jar nothing
recognizes stays listed and unrecorded.

An instance's mods are one list, and each entry carries its ownership and the
completeness of its provider identity as separate typed facts (ADR 0019). An
imported pack's mods are reconstructed from the pack's own catalog download
addresses, which is provider data rather than an inference from a file name; an
address that proves no catalog release leaves the release unknown instead of
recording a version number as an identifier. Provider titles, compatibility, and
artwork are enrichment over that state: they are served through the metadata
cache, and failing to fetch them costs a label rather than a mod. A recorded mod
whose jar is absent from `mods/` is marked absent rather than forgotten: the
recipe still wants it, so a repair still fetches it and a server built from the
recipe still contains it, but the mod list leaves it out, because a row for a
file that is not there names nothing a reader can act on. Anything built out of
an instance asks what to do with those records instead of deciding: duplicating
a client offers to keep or drop them, and keeps them unless told otherwise.
The frontends consume one snapshot from the application service and never read a
recipe, call a provider, or interpret a file name themselves.

A pack can also be named by an address rather than by a path. That address is a
second trust category, not an addition to the download allowlists: it arrives
from a link a person activated, is shown to them with its query removed and
agreed to before anything is fetched, and buys one permission, to serve an
archive (ADR 0027). A pack's own `files[].downloads` stay on the existing list,
the archive itself is size-bounded while it streams, and the downloaded file is
a temporary input removed once the import that wanted it is finished. The
application service refuses the download unless the host is agreed to, so a
frontend cannot reach it by forgetting to ask.

Registering the scheme itself is an installation step, not a preference: the
Debian package ships the desktop entry that claims it, the AppImage is
registered from the command line, and the desktop application offers no control
over either (ADR 0028). A desktop scheme handler starts a fresh process for every
link, so the launcher claims one name per user through a Unix socket under
`$XDG_RUNTIME_DIR`. Binding succeeds exactly when nobody else holds it, which
makes claiming the name and finding the running instance one operation that
cannot disagree with itself; a socket nobody answers is a stale name to take
over rather than an error. A second launch hands its link across and exits, and
the running window opens the same import dialog a dropped file would.

Which catalog to browse is a preference, not desired state. The settings file
records a preferred catalog under `[mods]`; the mod browser opens on it, and the
CLI reads it wherever `--provider` is left off. It decides how a future choice is
presented and nothing else: it never rewrites the catalog identity of an
installed mod, is never consulted for a retained recipe, and naming a catalog
explicitly bypasses it entirely. Nothing about reaching a catalog lives there.
A CurseForge API key remains a keyring credential (ADR 0004, ADR 0022), so
preferring CurseForge is answerable offline and with no key stored, and the key
is still checked by the request that needs it.

An explicit mod decision can be carried from a client to its linked servers
(ADR 0021). The exact release the client settles on is the release every
selected server is evaluated for; no server runs its own newest-compatible
search, and a release that does not fit one makes that target ineligible with a
typed reason rather than being substituted. Dependencies resolve per target
against that server's own Minecraft release, loader, and installed set. A synced
root is recorded in the server's own retained recipe as an explicit choice, so it
stays distinguishable from what the pack declared, and ownership is read back
from that record rather than from the directory. Each target's mutation is the
same undoable transaction; several targets are not one transaction, and the
result is per target.

## What a mod requires

A recipe entry records the relations its catalog declared about that file:
required, optional, incompatible, embedded (ADR 0034). What a relation names is
typed rather than optional fields: a project, with the exact release where the
catalog pinned one, or a jar, for a relation that names only that. CurseForge
states relations between projects and never pins a release; Modrinth may do
either. Nothing a catalog stated is discarded, and a relation that names neither
a project nor a jar is not representable. The direction stored is
forward, on the entry the catalog described, so installing or removing one mod
never rewrites another mod's record and an entry copied into a linked server's
recipe stays correct there. The direction a warning needs - which installed files
require a given project - is derived over the recipe rather than stored.

Relations and the fact that nobody has asked for them are different values. An
absent set means no catalog has been asked about that file; an empty set means
one answered that the file needs nothing. Anything built on this distinguishes
them, because a mod nothing has asked about is unknown rather than independent,
and a warning that treats it as independent is confidently wrong rather than
merely incomplete.

Installing a mod through a catalog records them, because the resolver already
has the release's declared dependencies in hand and used to drop them; the same
entry, and the same relations, is what a linked server's own recipe receives.
A relation the catalog states by file name alone carries no project to key on and
is left out rather than given an invented one, which loses no requirement: the
resolver refuses a required dependency shaped that way rather than skipping it.

Importing a CurseForge pack records them too. The exact-file document the import
already fetches to learn a file's name, address and digest carries its relations
in the same response, so nothing extra is requested and no credential is used
that the import did not already need. A file recovered from a manual download
because the provider refused to describe it has no such document, and stays
unasked.

A Modrinth pack index declares no relations at all - it states paths, hashes and
download addresses - so its entries stay unasked until an explicit refresh. A jar
with no catalog identity has nothing to ask about and stays unasked.

That refresh is `App::refresh_mod_dependencies`, and it is explicit because it
reaches the network: reading a mod list stays a directory read. It asks each
catalog once, in one batched request, about every mod whose recorded identity
names an exact release - a project without one has nothing to look up, since
asking about the project would answer for whichever release the catalog
considers current rather than the one installed here. A catalog that cannot be
reached is named in the report and costs only its own mods' answers; they stay
unasked rather than being recorded as needing nothing, which is the one outcome
that would turn a missing answer into a confident wrong one.

Two questions are asked of that state, both offline and both pure. What else
stops loading if this mod does, which is the same answer for turning a mod off
and for deleting its jar, because the loader sees the same absence either way;
and what this mod needs before it can load, which is what a frontend asks before
turning one back on. Both walk transitively: a dependency's own dependencies are
needed just as much, so turning a mod on while something two steps below it
stays off would produce a set that still does not load. Neither reaches a catalog, so a frontend may ask on every
toggle.

The first keeps proven and unproven apart, in four lists rather than one. A mod
is reported as breaking only because a recorded relation says so. A mod that
declares the target optional is named and never cascaded: it keeps loading and
does less. A mod nothing has asked about is reported as unknown, with a refresh
offered where one would help, rather than being assumed independent - that
assumption is the one that would make the warning confidently wrong. Optional
relations are excluded from the closure structurally, by a relation-kind
predicate the disable path passes `Required` to, so there is no flag that widens
it.

A third question is asked of the same state and answers neither of those: what
one mod's own entry declares, and which entries name it. Direct relations, both
directions, no closure. It is what a mod's own page shows, and it is what makes
the two warnings checkable - a reader who wants to know why turning one library
off took three mods with it can open each of them and see the relation that said
so. An entry nothing has asked about says that rather than showing an empty
list, and carries the refresh as its action.

A frontend asks the question and renders the answer; it re-derives none of it.
A mod row's switch reports a request rather than setting its own position: the
launcher may refuse it, or act on it and several other mods besides, so what the
switch shows has to come from what actually happened. The rows are written into
the model already on screen rather than replacing it, which keeps a half-finished
double click alive, and that is exactly why a switch may not decide its own
position - an element that had assigned to its own state would keep reporting a
click from minutes ago, for whichever mod later occupied that row.
The desktop application turns one mod off by asking what else that affects and
then taking one of three paths: nothing affected applies straight away, an
affected set with the confirmation wanted opens a dialog and changes nothing
yet, and an affected set with the confirmation turned off applies the whole
cascade and reports it in a toast. Deleting a jar asks the same question,
because the loader sees the same absence either way. The dialog carries the
preference's own switch, so the decision is recorded where it is made.

Whether a change that affects other mods stops to be confirmed is one global
preference, `[mods] warn_on_dependency_impact`, defaulting to on. It decides
only whether a frontend asks. Off does not mean the change is made blind and does
not mean the other mods are left alone: the consequence is still worked out, the
cascade is still applied, and what happened is still reported, without blocking.
Somebody who turned the confirmation off has already agreed to it, and applying
the change to the named mod alone would hand them a mod set that does not load -
the outcome the warning exists to prevent.

Deleting a jar is offered whatever owns it, and takes the jars that needed it.
It is not removal: the record survives, so the row offers to put the file back
and anything built from the recipe is still complete. The set it takes is the
one the graph proves for disabling, so the two can never disagree about
consequences.

Turning several mods on or off is one call, ordered so that any prefix of it
leaves a mod set the loader can start: disables go dependents first, enables go
dependencies first. A frontend issuing one rename per row cannot promise that,
and an operation interrupted mid-cascade would otherwise leave a dependency gone
with something that needs it still loading.

## Verifying what an instance holds

Installing decides what to do from whether a destination exists, which is right
for an install and wrong for trusting an instance: a jar truncated by a full
disk, or left half written by a machine that lost power, exists. Verification
answers the question presence cannot, from the digests the recipe already
records, so it reaches no network and writes nothing.

Each recorded file comes back intact, absent, corrupt, unreadable, or
unverifiable. The last is not a fault: a mod a pack shipped inside its own
override tree has no published hash and never will, so nothing about it is
proven either way, and it is reported as unproven rather than counted sound or
broken. A disabled jar is checked under the name it actually has, because
turning a mod off renames it out of the loader's way and leaves it installed.

Repairing replaces what is absent or wrong with what the recipe records. It
verifies first, so nothing sound is rewritten. A file that is present and wrong
moves into `.launcher/retired/` before its replacement is written, because
writing one refuses to overwrite - the rule that keeps an install from touching
a jar it did not put there - and because moving rather than deleting is what
makes the failure path recoverable (ADR 0020): the bytes go back when the
replacement cannot be obtained, and the outgoing copy is deleted only once the
new one is in place. A file that cannot be obtained is therefore left exactly
where it was: losing a file to a failed repair is worse than keeping a
wrong one, because a wrong one can still be looked at. One file's failure does
not abandon the rest, and the report separates what nothing could have fixed
from what a repair tried and failed at - the first says re-import the pack, the
second says try again.

The shared stores an instance launches from are checked separately, and only on
request. Libraries and the client JAR sit on Maven-style paths rather than under
their own digests, so nothing re-checks them once they are written; their
digests come from the release's metadata, read from the cache and never fetched,
so the check stays local and says "no metadata cached" rather than reaching for
it. An absent shared artifact is not a fault - these stores fill as a launch
needs them, and a freshly created instance is missing most of them - while bytes
that are present and wrong are, because nothing else will ever notice them. A
loader's own libraries come from that loader's documents and are reported as not
covered rather than silently omitted.

Repair is offered only where replacing the file could help, which means a fault
plus a recorded address. Telling somebody to repair an instance that repairing
cannot mend is worse than telling them nothing, so files with no address are
reported separately. Acting on a report is a separate request: "this file is
wrong" and "replace this file" are different decisions.


## Diagnostic bundles

A bundle collects diagnostics into one archive somebody can attach to a bug
report. It is not an upload: nothing contacts a service, no paste site is
integrated, and the archive is written to a path the user named and goes no
further than they take it.

Files go in byte for byte. A crash report that has been tidied is a crash report
missing the line somebody needed, so nothing here tidies one (ADR 0016). What is
guaranteed instead is narrower and checkable: Ephervex contributes no secret of
its own. Its own log is written under the same redaction rules as its stderr,
launch commands are redacted before reaching any log, and `config.toml` is
neither offered nor included - it is the one file whose whole job is to hold
settings, and the walk that strips credential-shaped keys from it exists because
that is where they collect.

The contents are chosen rather than fixed, because two reports are rarely about
the same thing: a crash wants the report and the game log, a failed import wants
the launcher log and the recipe. The archive carries a manifest naming what was
taken, what was declined, and what was asked for and could not be read - a log
that rotated away between choosing and writing is said out loud rather than
quietly missing. One unreadable part never abandons the rest.


## Security boundaries

### Untrusted paths and archives

Archive entry names, symlink targets, and provider paths are untrusted. The
shared path validator rejects absolute paths, prefixes, traversal, empty
normalized paths, and launcher-owned metadata destinations. Extractors also
refuse writes through links created by earlier entries and enforce entry-count
and uncompressed-size limits.

Imported pre-launch commands, post-exit commands, wrapper commands, and custom
Java paths are never executed. Unsupported customizations are reported or
retained as source metadata.

### Downloads

Production downloads use HTTPS, bounded redirects, timeouts, and explicit host
policy. The strongest supplied hashes are checked, and a mismatch is a hard
failure. Atomic publication ensures another operation cannot mistake partial or
unverified bytes for a cached artifact.

Resume is allowed only with a strong ETag and compatible range metadata. Pack
URLs must also pass the pack-download allowlist; loopback exceptions are scoped
to tests.

No cache path is derived from a name a provider or a user chose. Shared
artifacts are addressed by a published content hash. A provider metadata
document either carries a name the launcher chose itself or is addressed by the
SHA-256 digest of a canonical key describing the request, in a provider
namespace and sharded on the first two digest characters. A slug, a search
query, or an id list therefore cannot decide how long a filename is, which
matters because staging and temporary files are siblings of the entry and
filesystem name limits apply to them too.

CurseForge is the one source deliberately outside the metadata cache. Its
published third-party API terms prohibit saving or caching what the API returns,
so its client defines no cache key, no time-to-live, and no namespace under the
metadata store; responses live in memory for the operation that needed them and
are then dropped (ADR 0022).

### Secrets and diagnostics

Refresh tokens and API keys use a credential store, never `config.toml`.
Configuration loading strips secret-shaped legacy keys, and saving refuses to
serialize one. When a keyring is unavailable, Microsoft login can use an
explicit memory-only session without weakening the persistent-storage rule.

The CurseForge third-party API key follows the same rule and adds two of its
own (ADR 0022). It is sent in an `x-api-key` header set per request rather than
installed as a default header, and its client refuses to follow a redirect that
leaves the CurseForge API origin, so no response can direct the launcher to hand
the key to another host. An artifact download applies the same rule to a
different boundary: the header travels only to CurseForge's own explicitly
listed delivery hosts, and the client whose redirects cannot leave that list is
owned by the credential itself rather than by the downloader. That is what lets
one operation fetch artifacts from several providers through one download
manager: the authenticated request is confined, and an unrelated provider's
download keeps the ordinary redirect policy and is never offered the header.
An operation reads the credential once and holds it for that operation's requests
and no longer. Frontends learn whether a key is configured and never what it
is.

`SecretString` redacts `Debug` output and has no `Display` implementation.
Launch commands mark token-bearing arguments when values are substituted, and
logs use redacted forms. User-facing errors are localized at the frontend;
lower layers retain technical context without including response bodies,
authorization headers, tokens, or sensitive query parameters.

The launcher's own log file records the same events stderr shows and is subject
to the same rule. Browsing a game log does not copy its contents anywhere:
diagnostic reads return text to the caller and are not themselves logged.
Diagnostic files are never uploaded, and no paste service is integrated.

## Logs, crash reports, and launcher diagnostics

Client instances, servers, and the launcher each expose their logs and, for the
first two, their crash reports through one application service. Callers name a
scope and an entry name; they never build a path (ADR 0016).

```text
scope (instance id | server id | launcher)
   │
   ├── owned directory        <instance>/minecraft/logs, <server>/server/logs,
   │                          the matching crash-reports directories, and
   │                          <state>/logs/launcher
   ├── listing                newest first, name descending as the tie-break;
   │                          crash reports carry a best-effort summary
   ▼
entry name
   ├── open / refresh         bounded window, tail for logs, head for reports
   ├── search                 over the window's text; never writes
   ├── copy path              resolved, contained, never joined by a frontend
   └── open folder            the same folder actions instance management uses
```

Reads are bounded by a byte budget and a hard ceiling. A window reports the
file's size, how much it covered, whether it was truncated, and whether bytes
had to be replaced to make text of them. A file that rotated, was truncated, or
was replaced between two reads yields a new window or a typed "no longer there",
never a stale or partial value presented as current.

Entry resolution refuses anything that is not one regular file inside the owned
directory: a name with a separator or `..`, a symbolic link, a non-file entry,
and a directory that resolves outside the root that owns it.

The launcher appends its own `tracing` events to a plain-text file per day under
the state root, bounded by segment size and retained-file count, and browsable
through the same surface. Long-running operations - launching, starting a
server, importing a pack, installing mods - carry an operation id in their span
and report it to the user, which is how a run is located in that file. A
dedicated server's captured process output remains separate (ADR 0014): it is a
live console history, not a file the server wrote.

## Configuration, localization, and errors

The configuration schema is versioned. Unknown keys are preserved so an older
binary does not destroy settings it does not understand; a newer schema version
is refused instead of guessed at. `App::mutate_config` commits an in-memory
change only after the atomic write succeeds.

Preservation covers every open table, including the per-account rows and the
sparse Minecraft preference map, and every one of them is walked for
credential-shaped keys in both directions: dropped on load, refused on save. The
walk descends through tables and through arrays of tables, so `[[section]]`, the
natural shape for a per-provider list, is not a place to hide one.
What preservation does not cover is formatting. A rewrite keeps values and loses
comments, blank lines, quoting style, and key order, and re-emits unknown keys
after the ones this build knows. Reading a configuration never rewrites it, so a
newer build's file survives an older build starting up; the first setting change
is what rewrites the whole file.

Configuration holds settings and never cache contents: what the launcher happens
to have downloaded is a fact about the filesystem, not a preference.

Where a setting names something an enumeration could hold, it is stored as text
and resolved on read, so three states stay distinguishable: nothing recorded,
which is Ephervex's own default and free to change between releases; a value the
user chose, even one that happens to match that default; and a value this build
cannot resolve, which a newer Ephervex may have written. The third is reported
as itself and written back unchanged rather than read as one of the values this
build does know, and it costs the user that one setting instead of the whole
file. The preferred mod catalog under `[mods]` is stored this way.

Errors are typed by domain and composed through `CoreError`. User-visible text
uses Fluent message IDs and typed arguments, while paths, versions, hashes, and
provider IDs remain exact technical values.

CLI JSON output is written to stdout. Logs and human progress are written to
stderr so machine-readable output remains valid.

## Supported scope and known boundaries

The supported scope includes Linux, Minecraft release versions from 1.12.2
onward, Vanilla, Forge, NeoForge, Modrinth and CurseForge pack import, Modrinth
and CurseForge mod browsing with dependency resolution, and linked-server
reconstruction from retained recipes, plus launching a linked client directly
into a local managed server. Logs, crash reports, and launcher diagnostics are
inspectable through `ephervex-core`, the CLI, and the GUI. CurseForge pack
import and CurseForge browsing need a user-supplied API key and cannot obtain a
file whose author has turned off third-party distribution. CurseForge browsing
offers Minecraft's Mods class only, and its version lists are scoped to the
instance's exact Minecraft release and loader family because the provider pages
its file listings. Mod updates and removal are unavailable. Dropping a modpack
archive onto the desktop window works on X11 and XWayland only: the windowing
layer implements file drops there and not on Wayland, and Slint's own
drag-and-drop carries text and images between elements of one application rather
than a path from the desktop. The file dialog remains available everywhere, and
the same X11-only boundary already applies to the Minecraft window-size picker's
aspect hint. Fabric, Quilt, snapshots, Windows, and macOS are outside the
supported scope recorded in ADR 0008.

## Verification

Ordinary tests use deterministic fixtures and loopback servers; they do not
contact production services. Fixture directories include `SOURCES.md` files
with capture dates, upstream locations, and reduction notes where applicable.

The workspace baseline is:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The release gate adds the maintainer tooling's own tests, a release build, and
the packaged artifact's smoke test. Packaging, the declared Linux baseline and
how it is enforced, and the full gate are in
[`docs/release.md`](release.md) and [ADR 0023](adr/0023-appimage-is-the-linux-release-artifact.md).
