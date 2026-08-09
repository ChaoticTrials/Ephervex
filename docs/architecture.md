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
| `ephervex-storage` | XDG paths, versioned configuration, SQLite migrations and registry, instance metadata, reconciliation, launch locks, duplication, and folder layout |
| `ephervex-i18n` | Fluent resources, locale selection, typed arguments, and message lookup |
| `ephervex-download` | HTTP policy, bounded concurrency, retry classification, cancellation, hash verification, atomic publication, metadata caching, and shared cache layout |
| `ephervex-minecraft` | Mojang/Piston metadata, rule evaluation, artifact planning, Vanilla installation, command construction, process control, and log parsing |
| `ephervex-java` | Local runtime discovery, version probing, compatibility rules, runtime selection, and managed Adoptium installation |
| `ephervex-auth` | Microsoft, Xbox, and Minecraft Services authentication; offline identities; account sessions; keyring and memory-only credentials |
| `ephervex-loaders` | Vanilla, Forge, and NeoForge metadata resolution and client/server installation |
| `ephervex-options` | Line-preserving `options.txt` parsing and version-aware application of global preferences |
| `ephervex-pack` | Format detection, hardened archive handling, provider-neutral recipes, Modrinth parsing, optional selection, materialization, and managed-mod installation |
| `ephervex-mod-platform` | Modrinth project/version catalog, compatibility verdicts, and provider-neutral dependency resolution |
| `ephervex-server` | Boundary reserved for dedicated-server installation and process management |
| `ephervex-core` | Application services and error classification shared by frontends |
| `ephervex-cli` | Command-line frontend and human/JSON rendering |
| `ephervex-gui` | Slint frontend boundary; no functional interface is available |

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

A file appears at a final shared-store path only after it has been completely
written and verified. Incomplete work remains in staging paths.

The registry writes instance metadata before publishing the corresponding
SQLite row. Reconciliation can therefore rebuild an index entry from an intact
instance. A missing temporary root is treated as expected operating-system
cleanup; a missing persistent root remains registered and requires attention.

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

### Secrets and diagnostics

Refresh tokens and API keys use a credential store, never `config.toml`.
Configuration loading strips secret-shaped legacy keys, and saving refuses to
serialize one. When a keyring is unavailable, Microsoft login can use an
explicit memory-only session without weakening the persistent-storage rule.

`SecretString` redacts `Debug` output and has no `Display` implementation.
Launch commands mark token-bearing arguments when values are substituted, and
logs use redacted forms. User-facing errors are localized at the frontend;
lower layers retain technical context without including response bodies,
authorization headers, tokens, or sensitive query parameters.

## Configuration, localization, and errors

The configuration schema is versioned. Unknown keys are preserved so an older
binary does not destroy settings it does not understand; a newer schema version
is refused instead of guessed at. `App::mutate_config` commits an in-memory
change only after the atomic write succeeds.

Errors are typed by domain and composed through `CoreError`. User-visible text
uses Fluent message IDs and typed arguments, while paths, versions, hashes, and
provider IDs remain exact technical values.

CLI JSON output is written to stdout. Logs and human progress are written to
stderr so machine-readable output remains valid.

## Supported scope and known boundaries

The supported scope includes Linux, Minecraft release versions from 1.12.2
onward, Vanilla, Forge, NeoForge, Modrinth pack import, and Modrinth mod
browsing with dependency resolution. The GUI, CurseForge browsing, CurseForge
and Prism import, mod updates and removal, and dedicated-server process
management are unavailable. Fabric, Quilt, snapshots, Windows, and macOS are
outside the supported scope recorded in ADR 0008.

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
