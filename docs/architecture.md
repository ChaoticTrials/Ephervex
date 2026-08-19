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
root is excluded, for reasons of ownership rather than size — `libraries/` is
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
and the cache stays over budget — leaving files behind is always preferable to
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
and describing the archive stays local either way — only the resolution step
needs a credential, and analysis reports that as a blocker rather than failing
to identify the file (ADR 0022).

A recipe entry records the exact addresses, digests, and size of its artifact,
which is everything needed to identify and verify it: nothing built from retained
desired state asks a provider what an instance contains. Serving the bytes is a
separate question. Materialization probes the shared artifact cache first, and a
verified entry is installed with no credential and no request, which is why an
installed instance and its linked servers keep working with no provider
configured. Only an artifact the cache no longer holds is fetched, and only then
is a credential asked for — per file, from the provider identity that file's own
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
a server built later contains them. A jar the launcher did not install is never
overwritten, deleted, or adopted into that record: nothing enumerates `mods/`,
and a file with no provenance stays outside the managed model. Installation
writes the recipe last; any failure — including a failure to write the recipe —
removes the files that operation created and leaves everything else untouched.

An instance's mods are one list, and each entry carries its ownership and the
completeness of its provider identity as separate typed facts (ADR 0019). An
imported pack's mods are reconstructed from the pack's own catalog download
addresses, which is provider data rather than an inference from a file name; an
address that proves no catalog release leaves the release unknown instead of
recording a version number as an identifier. Provider titles, compatibility, and
artwork are enrichment over that state: they are served through the metadata
cache, and failing to fetch them costs a label rather than a mod. A recorded mod
whose jar is absent from `mods/` is reported as missing rather than forgotten.
The frontends consume one snapshot from the application service and never read a
recipe, call a provider, or interpret a file name themselves.

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
through the same surface. Long-running operations — launching, starting a
server, importing a pack, installing mods — carry an operation id in their span
and report it to the user, which is how a run is located in that file. A
dedicated server's captured process output remains separate (ADR 0014): it is a
live console history, not a file the server wrote.

## Configuration, localization, and errors

The configuration schema is versioned. Unknown keys are preserved so an older
binary does not destroy settings it does not understand; a newer schema version
is refused instead of guessed at. `App::mutate_config` commits an in-memory
change only after the atomic write succeeds.

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
its file listings. Mod updates and removal are unavailable. Fabric, Quilt,
snapshots, Windows, and macOS are outside the supported scope recorded in
ADR 0008.

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
