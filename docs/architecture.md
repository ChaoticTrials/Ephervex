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

## Desktop interface markup

The desktop interface is written in standalone Slint modules under
`crates/gui/ui`. `build.rs` compiles `app.slint` with `slint-build` in the
`fluent` style, and `crates/gui/src/ui.rs` includes the generated bindings, so
the markup is ordinary `.slint` source to Slint-aware editors and nothing about
the build depends on one. Debug information is kept for every profile but
release, because the headless tests find elements by it.

| Module | Holds |
|---|---|
| `app.slint` | `AppWindow`: the properties and callbacks Rust reads and invokes, the navigation shell, its pages, and its modals |
| `theme.slint` | The `Theme` global: the color tokens every surface reads, text sizes, and shared measurements |
| `palettes.slint` | `ThemePalette` and the built-in palettes, the one place colors are written |
| `globals.slint` | `GuiI18n` and `PropertyRules`, globals whose answers Rust supplies |
| `models.slint` | The row and view structs Rust fills models with |
| `components/` | Text primitives, controls, and artwork shared by every surface |
| `workspace/` | A host workspace's chrome and its surfaces: mods, logs, isolation, diagnostics, snapshots, and host settings |
| `home/` | The client and server cards, the inspector, and the Running and Activity cards |
| `settings/` | Application settings panels and the Minecraft defaults screen with its window size picker |

Rust sees only what `app.slint` exports: the globals and windows by name and the
row structs through `export * from "models.slint"`. A new type Rust needs is
exported there. Rust owns application and feature state; the markup renders it
and reports what the user asked for.

Tests that read the markup as text go through `test_support::UI_SOURCES`, which
a test keeps in step with the files on disk.

### Theme and palettes

`Theme`'s color tokens are the contract between the markup and whatever decides
the colors (ADR 0041). Each token reads the field of the same name on
`Theme.palette`, a `ThemePalette` that sets every color token and the scheme
the shipped widgets paint in. Components read tokens and never the palette, so
none of them knows which palette is active.

The built-in palettes, Dark and Light, live in the `BuiltInPalettes` global.
The theme preference is a separate value: `crates/gui/src/palette.rs` resolves
it to one complete palette and writes it to `Theme`, and is the only place that
chooses one. Adding a built-in palette is a `BuiltInPalettes` entry, a scheme
that resolves to it, and its label in the theme list.

User themes are TOML files in the `themes` folder under the configuration
directory (ADR 0042, format in `docs/themes.md`). `crates/gui/src/theme_files.rs`
reads them on the backend thread whenever the settings snapshot is built, and
the snapshot carries the usable themes and a localized line for every file
problem. `palette.rs` resolves a user theme by starting from its built-in
parent and replacing the colors the file sets, so a user theme is complete by
construction. A chosen user theme whose file is gone or unusable paints in the
default palette, and the recorded choice stays.

Text sizes, the resolved monospace family, and the shared measurements stay in
`Theme` outside the palette. Several of them are layout invariants (fixed row
heights that virtualized lists and scroll arithmetic depend on, pane and tab
geometry), and a palette never changes them.

A theme file can set colors only. Token names are part of its format, so a
token can be added but never renamed or removed.

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

Clients and servers follow the same rule for an indexed host whose root is
present: canonical metadata replaces a disagreeing index row, which is how a
metadata write that landed before a failed index write is repaired. Two cases
keep the index instead. An edit made while the root was missing could only be
written to the index, so a row that carries the missing-root warning and is
newer than canonical metadata is published to the root when it returns. And a
root whose snapshot restore is unresolved (recovery failed, the host's claim was
held by another process, or the incomplete marker is still present) holds the
snapshot's copy of the metadata, which is never adopted or mirrored into the
index. Canonical metadata whose storage class contradicts where the root stands
is reported as unhealthy instead of written, because the next pass would
otherwise forget the record. A server or persistent client directory the index
has lost gets the same treatment before it is indexed: a root whose claim another
process holds is skipped, its restore is recovered under the host's claim, and a
root that is still unresolved is reported and left unindexed. A client root the
index already points at is never reported as an unfinished directory, because
the pass over the index has reconciled it. Only an unindexed root, or a copy that
carries an indexed client's id, is reported as one.

Reconciliation takes a host's claim before it reads the record, so it never acts
on a record a finishing move has just changed, and a host found at its
persistent root after an interrupted promotion gets the same restore recovery,
metadata check, and interrupted-state report as one that never moved.

## Mutation and interruption recovery

State-changing operations use a publication boundary or leave durable evidence
that startup reconciliation can report. They do not infer success from a
free-space estimate. A write that reaches `ENOSPC` is classified as storage
exhaustion with an actionable error.

| Operation | Publication boundary | Interruption state and recovery |
|---|---|---|
| Configuration and launcher-owned JSON metadata | A sibling file is flushed, renamed over the destination, then the parent directory is flushed. | The previous file remains authoritative until rename. A temporary sibling is never read as state. |
| Client creation and pack import | The incomplete marker is cleared, then canonical metadata and the index make the host visible. | A marked, unpublished root is reported and left untouched for manual review. |
| Server installation | A fully built hidden sibling root is renamed into its final location before it is indexed. | An unpublished or incomplete root is not launched as a server. Reconciliation reports rather than guesses when canonical state is missing or conflicting. |
| Instance duplication | The complete destination is published by rename, followed by the recoverable index update. | Incomplete destinations are reported. |
| Storage promotion | One rename when both roots share a filesystem. Otherwise the tree is copied and flushed into a hidden staging sibling, renamed into place, then indexed, and only then is the source removed. | The destination only ever holds a complete host. A retry replaces the staging directory an interrupted copy left, refuses while another move or a snapshot restore of the same host runs, and refuses a host with an unrecovered snapshot restore. Reconciliation never adopts a staging directory, and reports it unless a running move still holds the host. If a promoted root moved before its index update, reconciliation follows the canonical files instead of deleting the record and then reconciles them like any other present host. |
| Downloads and shared artifacts | Bytes are staged, verified against the strongest supplied hash, then renamed into the shared store. | Partial or mismatching bytes are never published as cache entries. A retry starts from verified state. |
| Managed mod install, update, and removal | Old jars move to `.launcher/retired/`; new jars land; the retained recipe is atomically published. | Pre-publication failure removes new jars, including a jar whose copy failed partway, and restores retired jars. Any failed compensation is returned as a typed recovery issue and persists `InterruptedMutation` attention. Post-publication cleanup failures are also reported. |
| Dependency-aware enable, disable, and delete | One planned batch applies ordered file changes using the same recorded dependency graph. | Failure rolls back the batch. Any launcher-owned staging or retired content left behind is reported on the next reconciliation. |
| Verification repair | Replacement bytes are verified before they replace a managed file. | A failed repair keeps or restores the prior file; leftover transaction state remains visible for reconciliation. |
| Diagnostic fix | Only the changes its preview listed are applied, each on its own: a mod rename, one runtime-settings update, or a verification repair. | Each change is reported as applied or not, so a partial fix is visible. The host is diagnosed again rather than assumed fixed. |
| Snapshot restore | A durable journal names only fixed host parts. Each live part moves into the restore transaction before its staged replacement is renamed into place. | Failure rolls every published part back. The restore holds the host's claim, shared with storage moves, until it ends. On restart, reconciliation recognizes the versioned journal and restores the complete pre-restore state, but only for a host whose claim it can take, so a restore another launcher is still running is never rolled back (a filesystem with no file locking cannot show that across processes). Unknown or ambiguous staging is left untouched and receives host attention. |
| Mod isolation round | The versioned session records the next exact configuration before the dependency-aware batch applies it. | Resume reapplies the intended configuration idempotently. Cancel restores the pre-isolation snapshot; finish retains it and leaves the smallest tested configuration enabled. |
| Diagnostic bundle export | A caller-selected set is written to the requested local archive. | Failure is reported against the output path. Source logs are never changed or scrubbed. |
| Launch and server process control | A durable owner record and kernel-visible process identity establish control. | Stale ownership evidence is distinguished from a live process. A server orphan is reported and never adopted or signaled. A client game that outlived its launcher keeps its instance locked and is reported as orphaned; only the validated force stop signals it. |

Non-empty `.launcher/retired/` or `.launcher/staging/` directories are always
suspicious after restart. Reconciliation adds host attention and leaves every
entry in place. A later operation must not silently erase that evidence.

## Local safety snapshots

Clients and local servers expose the same snapshot service through core. A
snapshot captures the complete game directory, retained source state, and
canonical host metadata under `.launcher/snapshots/`. It prefers reflinks,
falls back to independent byte copies, preserves symlinks, and never uses hard
links for mutable Minecraft data. Snapshot history and transaction state are
not captured. See ADR 0035.

The CLI exposes create, list, restore, rename, and delete as scriptable commands
with structured output. Destructive commands require explicit confirmation.
The desktop exposes the same operations on both client and server workspaces,
including actual apparent size, copy method, origin, and mod counts. Creation
and restoration run through the shared cancellable operation infrastructure.
Retention is always user-managed. The host deletion dialog includes the number
of retained snapshots that deleting its files also removes.

## Host workspaces

The desktop represents an installed client or local server with one reusable
host workspace. The workspace owns the host header, running status, launch or
process actions, data-driven surface navigation, and a small persistent area
for a long-running host workflow. Overview, Mods, Isolation, Logs, Snapshots,
and Diagnostics remain independent surfaces beneath that boundary; server-only
surfaces are entries in the server's surface model rather than a separate
container. Adding another host tool means adding a surface entry and its body,
not another host header or navigation structure. More than one host workspace
can therefore be composed by a parent without teaching its surfaces about that
parent.

### Split workspace

The workspace can be split into two to four panes, each showing one host and
one of its surfaces. The layout is a typed model in the GUI crate whose only
values are a single pane and the 1:1, 2:1, 1:2, and 2:2 shapes: two equal
columns, each holding one pane or two equal stacked panes. A single pane offers
Split beside; a full-height pane of a split offers a pane below it; every pane
of a split can be closed, and closing settles on the next smaller shape in
reading order, down to the single pane. A new pane shows the same host's
Overview. Each pane of a split carries a compact header: the host, its state,
its Restart and primary action on one row, and the surface tabs and layout
controls on the next. The host opens a picker of cards like the home screen:
every client, with its linked servers on its card, and the servers the Servers
page lists, running hosts first. Clients rank by what runs on their card: the
client and a linked server, the client alone, a linked server alone, nothing. Surface tabs that do not fit wait under More,
and the selected surface always keeps a tab. A workflow's banner is left to the
pane showing the workflow's own surface, so a round shown in Isolation is not
repeated above the host's other panes. A pane's accessible label names its
position, host, and surface, so two panes on one host are never announced
alike. The split has one Back, above the panes, which leaves for the page the
split was opened from. The workspace is not persisted: opening a host from a
list replaces the layout with a single pane.

The layout owns composition only (see ADR 0039). The controller renders one
host view per shown host and one row per pane pointing at it, and every feature
(snapshots, runtime settings, server properties, mod lists and catalog
searches, isolation, Diagnostics and bundles, workflow status) writes by host,
so every pane on a host reads the same state. What a single pane is looking at
stays with the pane: each Logs or Console pane has its own log viewer, and the
worker follows each client's log for as long as a pane reads it. The
single-flight guards (the isolation change, the diagnosis or fix, a host's
restart, the host-change claim) stay in the controller and the worker,
so two panes on one host cannot start the same work twice. A pane whose host is
deleted stays in place, says so, and can be pointed at another host or closed.

Panes are framed and clipped. Below `Theme.workspace-pane-min-width` and
`Theme.workspace-pane-min-height` the pane area scrolls instead of letting panes
shrink or overlap. Surfaces laid out for the full-width workspace can be wider
than a pane at the smallest window size; they scroll or clip inside their pane.

The workspace does not interpret isolation state. The controller supplies a
workflow identifier, status copy, and action rows, and routes those actions to
the owning feature. Isolation currently uses that extension point for its
current round, common answers, undo, finish, and detailed-view navigation. The
same boundary can carry another durable developer workflow without moving its
state into the visual container.

One host-change state says which host a change is rewriting and what it is
doing. An isolation change and a diagnostic fix both claim it, so the workspace
header, the Isolation answers, and Diagnostics read one answer, and each refuses
to start while the other holds it. The worker keeps the matching guard: while an
isolation start or cancel, a diagnostic fix, or a snapshot capture or restore
rewrites a host, launching or starting it and starting another of those changes on it are
refused with a message naming the change in progress.

### Local server tooling

The server Overview opens the actual game directory containing
`server.properties`. Its folder shortcuts use core's existing folder resolver
for mod configuration, Minecraft logs, crash reports, mods, and the server root
containing Ephervex metadata. Retained Ephervex stdout/stderr capture has its own
core folder action because it lives in the shared state directory. Missing logs
and crash reports are reported as unavailable; opening them does not create
empty directories. Arbitrary file editing stays in the user's desktop tools.
The Properties shortcut opens the existing structured server settings.

The controller keeps pending starts, stops, console submissions, and observed
run failures by server ID, alongside the existing host restart state. All views
of a server share those guards and status. Snapshot values remain the baseline
for runtime ownership, and completion releases controls even when setup or a
control request fails. Failed status describes a run observed by this desktop
session; retained output and Minecraft crash reports remain available after
reopening Ephervex. Starting and Running describe process preparation and a
spawned Java child, respectively, not whether Minecraft accepts connections.

Console uses the existing supervisor control channel (ADR 0014). Only a
confirmed write of a single nonempty command reports delivery. Stopping and
restarting reject input, and a failed or timed-out stdin write disables further
input for that child without abandoning process supervision or output capture.
The same classified failure feeds the existing quick Diagnostics path. Control
completion and final run completion can arrive in either order; only a genuine
failure triggers diagnosis. A run the user did not stop, kill, or restart fails
when its last child exits with a nonzero code or a signal, or exits cleanly
before logging vanilla's `Done (...)! For help, type "help"` line: Minecraft's
server catches a startup exception, logs it, and exits with code 0.
Results for an earlier failed run are ignored after
a new start. The server observer reports final completion separately from each
child exit, so a linked run releases its server guard independently of the
client and normal restarts do not produce failure notices.

A new child gets a new input channel. Stop, kill, and console requests run off
the GUI worker loop so another host can still be used while a server responds.
Confirmed delivery clears the submitting pane's command field; a refused or
failed send keeps it for correction. Feedback names the target server. Sending a command does not insert a synthetic log line.

Logs distinguishes launcher-captured output from files written by Minecraft.
Its folder action follows that distinction, and Overview offers direct paths
to Console and crash reports. These shortcuts reuse the existing pane log
viewers and source ownership; they do not add another subscription or change
workspace composition (ADR 0039).

### Restart

A restart stops a running host, waits until that exact host is observed
stopped, and starts it once. It is never a delayed second click, and a stopped
host is started with Start or Launch, not Restart.

A local server restarts inside its own supervisor: the supervisor sends
`stop`, waits for the Java child to exit, and spawns the same command again,
reporting the move from stopping to starting as it happens. A stop or kill that
arrives while a restart waits ends it with the server stopped, a restart cannot
turn a stop already asked for into a start, and a second restart joins the
first. When the replacement cannot be spawned the server is left stopped, not
looking like one that needs recovery, and the restart reports that its start
failed.

A client restart reuses the ordinary launch path. Only an exclusive launch this
run of the desktop started can be restarted, because only for those is the
launch request known: it is kept in memory for as long as the launch runs and
holds an account reference, never a credential. A client started by another
Ephervex process or alongside another launch is not offered a restart. A
client started by Launch & Connect, including a pair start, becomes
restartable once its server is ready, and its launch request also records the
server it joined. The restart stops the game the way its Stop does, waits for
that launch to return and for nothing to hold the instance, then launches it
again as the same account, joining the same server when it did; if the game
is not seen to stop within a bound, nothing is started. Restart starts only
the client, so it never starts or restarts that server. The replacement counts as started only once the instance's
lock records its game, with the start time that proves the process, and that
game is still the one recorded a few seconds later. A JVM that prints an error
and exits, or exits before it is recorded, is a failed start, whatever it
printed. Stopping a client kills its game: Minecraft offers no request to quit
from outside the game.

Both claim the host-change state for their whole duration, so isolation,
repair, snapshots, launching, and a second restart wait for them, and they are
not cancelable once accepted. Their stages are carried on the host's own row,
so every view of the host shows the same restart. A failure after the stop
leaves the host stopped and is reported as a failed start.

## Dependency-aware mod isolation

Isolation planning uses the same required-dependency graph as mod toggles.
Every enabled mod outside the pinned closure is a candidate, carrying its
complete required closure, including mods that another candidate requires; a
required edge constrains which configurations are valid, not which mods can be
suspects. Shared libraries remain enabled whenever any selected candidate needs
them. Pinned mods carry their own transitive closure. Only a required cycle is
merged, under one deterministic representative.
Missing jars and incomplete dependency knowledge are shown before starting.
Either condition requires an explicit acknowledgement before the desktop can
continue, and the acknowledgement is retained in the session. A setup that has
not started carries a fingerprint of the plan it shows. Every refresh of a
host's mods reports the fingerprint of the plan the host would get now, and a
different one retires the setup; an unchanged host keeps it.

An active session is a compact version-1 document at
`<host>/.launcher/isolation.json`. It records the pre-isolation snapshot, the
complete plan and search state, every answered round, and the exact intended
current configuration. The document is published atomically before the mod
batch, so reopening the application can show the persistent workflow state and
resume can repair an interrupted application by applying the same batch again.
The desktop reconciles the host with the session whenever it reads one: a
matching host is left alone, a differing one is reapplied through resume, and a
running host or a failed reapplication blocks round answers until a retry
succeeds. Core refuses an answer while the host lacks the round's mods.

The deterministic search tests subsets and then complements, both closed
under required dependencies: a subset also enables the suspects its group
requires, and a complement also removes every suspect requiring a removed one.
Reproduction narrows the suspects to every candidate the round enabled, not
only the ones its group selected, so testing a mod never counts as having
tested its dependencies. A configuration that would enable no candidate, every
suspect, or only files an earlier non-reproducing round enabled is skipped as
not reproducing. No reduction grows granularity until removing each remaining
candidate, with its dependents, has been tested. Each history entry retains its pre-answer
state so undo restores the precise prior round. A non-reproducing superset of a
known reproducing configuration is recorded as a contradiction instead of
silently advancing.

Start and round mutations refuse while the selected client or server is
running, but a waiting session holds no launch lock. A client session reports
its linked servers without changing them. Missing dependencies and unrecorded
knowledge require separate explicit reduced-confidence acknowledgements.
Unidentifiable jars carry the same caveat because no catalog query can make
their dependency information complete.

Finish leaves the smallest tested configuration enabled, removes the session
marker, and retains the dated safety snapshot. Its result names the remaining
candidates that no other remaining candidate requires, and keeps them separate
from the candidates and files they require, from pinned roots, and from the
files pinned roots require, neither of which was ever tested for removal. Cancel restores the snapshot through the
ordinary snapshot service and removes the marker. See
[ADR 0036](adr/0036-mod-isolation-applies-recoverable-dependency-closed-rounds.md)
and the [mod isolation guide](mod-isolation.md).

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

## Disk usage

Core measures what Ephervex's own roots hold: the persistent and temporary
instance and server roots, and the cache, data, state, config, and temporary
roots. It reads the file system and changes nothing; the view built on it
offers no cleanup.

Every figure is the apparent size of regular files, the length each reports.
It is the one number that is exact on every filesystem, and it overstates
physical use where data is shared or sparse: a copy-on-write snapshot shares
its unchanged bytes with its host, so the Snapshots figure is what the
snapshots would take as plain copies, not what they occupy. Attributing shared
extents would need filesystem-specific extent queries and would still depend on
which owner is asked first.

Each file is counted once. It belongs to one category by where it sits, the
most specific rule winning: inside a host, `.launcher/snapshots` is Snapshots,
`.launcher/retired`, `.launcher/staging`, and `restore-*` journals are Recovery,
and the game directory's `mods`, `logs`, and `crash-reports` are Mods and Logs;
the rest of a host is its client or server. The cache root splits into the
download cache and shared game files, the data root into Java runtimes, icons,
and the rest, and the state root into logs and the index database. A file with
several hard links is counted the first time it is met, walking hosts first; a
symbolic link is never followed; and a root that sits inside another root is
measured once, as itself.

Each file also has one retention class: persistent, temporary (a temporary
host, under the operating system's temporary directory), regenerable (caches
and managed runtimes that can be fetched or derived again), or recovery. Recovery
state is never regenerable: it is what makes an interrupted change reversible
or reviewable. Both breakdowns sum to the total. Directories and files that
exist and cannot be read are counted and named, and their size is missing from
every figure; a file that disappears during the walk is simply not there.

A category's folders are the directories it has of its own. One spread through
every host, such as Mods, Snapshots, or Recovery, has none, and a frontend asks
core to open a folder by category and position rather than by path.

The desktop shows it as a Settings page rather than a host surface, because it
measures the launcher's roots, not one host. Opening the page measures nothing;
Measure runs the walk as an ordinary cancelable operation that reports how many
files it has counted. A result is a snapshot with the time it was taken, and
Measure again takes a new one rather than anything tracking later changes. The
page shows the total, a bar and a legend splitting it by retention class, and
every category with its size in words beside its bar, largest first.

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
nothing recognizes keeps its digest and no provider. The answer that identifies
a jar also records what that exact file declares (ADR 0034): a catalog match
records its relations, a client record's relations are copied as they stand,
and a jar nothing answered for stays unasked. No catalog failure can fail the
installation.

A linked server reads the client's retained recipe and retained override
layers. It never scans or copies the mutable client game directory, so manually
added jars and local debugging changes remain unmanaged. Required content is
included, unsupported content is excluded, and optional content follows the
recorded choice. Entries with undeclared server applicability require explicit
confirmation for the current creation and otherwise become actionable server
attention reasons. The original pack archive is not consulted. The server's own
recipe still records the excluded content, and every reading of it keeps to
the server's side. The server's mod list and verification and repair leave out
what the server does not install. Dependency checks treat only content
unsupported on the server as absent on purpose, never a requirement to
satisfy; a deselected optional entry that an installed mod requires is still
reported missing.

Base installation, recipe files, overrides, managed network properties, source
retention, and canonical metadata all land in the same hidden sibling root.
The final rename remains the sole publication point; failure at any earlier
step removes the staged root and cannot leave an indexed complete server.

Runtime Java precedence is operation-specific path, server override, global
preferred path, then compatible discovered or managed runtime. Minimum memory,
maximum memory, and extra JVM arguments resolve independently from per-server
overrides over global defaults. The layered heap range must be positive with
the minimum no greater than the maximum: saving an override that cannot form a
valid range is refused, and planning checks again because the global settings
can change afterward. Compatibility is checked during installation
and every start, including the legacy Forge Java 8 update ceiling. A runtime
built for another processor architecture than the host, or one that did not
report its architecture, is refused for both automatic selection and a pinned
path.

A start first merges canonical typed network settings and validates global EULA
consent. Only then may it atomically merge `eula=true`. The foreground
supervisor owns the Java child's stdin and both output pipes, the runtime lock,
and a mode-0600 Unix socket in a short, mode-0700 per-user runtime directory.
The runtime record's generation selects the socket without embedding the
potentially long server root in its path. Stop is graceful and never
escalates; kill is explicit and available only through the owner channel.
Linux boot ID, PID, and start ticks identify a process; the recorded
executable is informational, so a replaced or exec'd binary never counts as an
exit. They distinguish a controlled process, an orphan, an unverifiable
process, and proven-stale state. An orphan is reported but never adopted or
signaled.

Launcher capture is separate from Minecraft's own `server/logs/` and
`server/crash-reports/`. A bounded memory ring feeds frontends while rotated
JSON Lines segments provide durable combined output. Retention and write
failures are observable events; pipe draining continues when durable capture
is unavailable.

## Linked pairs

A client is paired with one of its linked servers (ADR 0040): its first linked
server by default, however it was created, or another one the user chose. A
choice, or an unpairing, is launcher workflow state in `config.toml`, keyed by
the client. Core resolves pairs against the registry on every read, so a
deleted client or server, or a server that is no longer linked, is never a
pair; a chosen server that disappears falls back to the first linked server.
Unpairing leaves the link, and the mod sync and Launch & Connect built on it,
untouched, and keeps the client unpaired while the server it declined stays
its first.

Either side's Overview names its counterpart and pairs, unpairs, and shows it
beside, and every pane showing a paired host carries a badge beside its state
that names the counterpart's own state, restarts included, whichever surface is
shown. Show beside reveals the counterpart through the layout model rather than by
arranging panes itself:

- a pane that already shows the counterpart is reused and nothing changes;
- otherwise one pane is added in the smallest step the shapes allow: a single
  pane becomes 1:1, 1:1 gains a pane below whichever column the asking pane is
  not in, 2:1 fills its right column, and 1:2 fills its left one;
- every existing pane keeps its host, surface, column, and row; and
- a 2:2 layout has no room and is left as it is, with a message instead.

Launch & Connect shows both sides the same way as it starts them: the
workspace opens on the client when it is closed, and whichever side is missing
is added beside the other where there is room, a server on its Console.

The pair belongs to the hosts, not to panes, so closing or retargeting a pane,
leaving the workspace, and coming back never change it.

A pair can start together. Launch or Start on either side then asks core what
a fresh start means: with the client stopped it is one Launch & Connect run
(the server is started or reused, and the client launches once the server
accepts connections), and with the client already running only the server
starts. Either way both sides are put on screen as Launch & Connect puts
them. Nothing listens for a host having started in order to start another,
so neither side's start can lead back to the other's. The controller holds one
pair start per pair, keyed by an operation number, refuses a second Launch or
Start of either side and a Restart until it settles, and stamps "starting the
pair" on both hosts' rows so every pane agrees. The worker settles it exactly
once through a drop guard: when the client is running, or when the run fails,
is refused, or is canceled. A failure names the side that failed, from whether
the server became ready, and leaves a server that started running. Restart
never consults the pair: restarting a client that came up with its pair
relaunches only the client, joining the same server.

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
bounded deadline, counted only while the server's recorded Java process itself
holds every listening socket the connection can reach. Core also observes
supervisor exit or loss of a controlled runtime while it waits. Once ready,
the ordinary client launch pipeline supplies account identity, Java, exact
loader metadata, artifacts, locking, and process capture. Client failure or
exit sends no server stop action (ADR 0015).

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
Installer profile and child metadata declarations that resolve to the same
library cache path must agree on both SHA-1 and size, even when their Maven
coordinates differ. Client installs also check the vanilla parent
libraries they store against the profile. Conflicts are refused before library
downloads and identify the library and the differing declarations.

`ephervex-storage` holds an instance launch lock for the full operation. The
lock records the launcher process and kernel start time to distinguish a live
owner from PID reuse, then records the game PID and start time after process
creation.

A launcher run can end while its game keeps running. The lock still holds the
instance while the recorded game is alive and still has its recorded start
time, so no second game can share its directory; the instance is shown as
orphaned rather than stopped. Nothing supervises such a game: its output and
process handle went with that run, so it is not restartable and its console is
not recovered. The force stop signals it only after proving it is that process,
and its lock stays until it has exited. A force stop never releases the lock of
a launcher run that is still alive, including one it just signaled: that run may
still be preparing a game, so the lock stays until the run has exited. A run is
signaled only when its recorded start time proves it is the process that took the
lock; a lock with no recorded start time, or one read while `/proc` is
unavailable, is kept and nothing is signaled. Recording
the game's id takes the same ownership guard and is refused when the lock file
no longer names that run, in which case the game is not left running. A recorded
id whose start time does not match, or that has no recorded start time, is not
believed, and a lock whose owner and game are both gone is cleared by the next
launch.

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
download addresses - so its entries stay unasked until a refresh. A jar with no
catalog identity has nothing to ask about and stays unasked.

Two refreshes record them, and both go through
`App::record_declared_relations`, so they leave the host in the same state.
`App::refresh_mod_dependencies` is the explicit one. It asks each catalog once,
in one batched request, about every mod whose recorded identity names an exact
release - a project without one has nothing to look up, since asking about the
project would answer for whichever release the catalog considers current rather
than the one installed here. A refreshed mod listing already fetches those same
exact releases for their names and versions, so it records the relations they
declare as well, and refreshing the Mods list is enough for a dependency graph
built on it. Neither runs when a mod list is merely read, which stays a
directory read. The recipe is read again after the network, so a change made
while a catalog was being asked is kept. A catalog that cannot be reached, or
that does not answer for a release, costs only those mods' answers; they keep
what they had, and a mod nothing had asked about stays unasked rather than being
recorded as needing nothing, which is the one outcome that would turn a missing
answer into a confident wrong one. The explicit refresh names such a catalog in
its report.

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
directions, no closure. It is what a mod's own page shows, and what
`ephervex mod dependencies` prints for one mod or for every mod of a host, and
it is what makes the two warnings checkable - a reader who wants to know why
turning one library off took three mods with it can look at each of them and see
the relation that said so. An entry nothing has asked about says that rather
than showing an empty list, and carries the refresh as its action. Asking for
every mod builds the graph once: asking per mod would re-read the recipe and
re-walk the directory once per jar.

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
cascade and reports it in a toast. Deleting a jar takes the same three paths,
because the loader sees the same absence either way: the confirmation it raises
is about what else goes with the jar, so a jar nothing needs is simply deleted
rather than confirmed twice. The dialog carries the
preference's own switch, so the decision is recorded where it is made, and the
General settings page carries the same one, so it can be taken back: an answer
given inside a dialog that only ever appears while the answer is still yes
would otherwise be a one-way door.

Two global preferences decide what a change does, and they resolve into one
rule rather than being read separately at each call site, because a frontend
that derived the rule for itself is how the desktop application and the command
line would come to disagree about when a change is quiet.

`[mods] warn_on_dependency_impact`, defaulting to on, decides only whether a
frontend asks. Off does not mean the change is made blind and does not mean the
other mods are left alone: the consequence is still worked out, the cascade is
still applied, and what happened is still reported, without blocking. Somebody
who turned the confirmation off has already agreed to it, and applying the
change to the named mod alone would hand them a mod set that does not load - the
outcome the warning exists to prevent.

`[mods] follow_mod_dependencies`, also defaulting to on, decides whether a
change acts on other mods at all. Off, every switch changes exactly the mod it
belongs to: arranging a mod set by hand is one of the things this launcher is
for, and a launcher that keeps putting a library back is in the way of it. What
each mod requires stays recorded and stays on display either way - this decides
whether a change follows those relations, not whether they are known. With it
off there is nothing for a confirmation to be about, so the confirmation is not
asked and its switch says it cannot be answered from there. On the command line
the same rule holds: while the change is confirmed, a command stops whenever the
core's answer is not silent. Turning a mod off or deleting it stops for mods
that need it, mods that use it optionally, and mods whose requirements are
unrecorded; turning one on stops for required mods that are turned off or not
installed and for installed mods it is declared incompatible with. The one
exception is that `--with-dependents` and `--with-dependencies` are
instructions for that run and carry the other mods
whatever is recorded.

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

A repair is refused while the host runs, because it moves jars the game may
have open, and while an isolation session is active, because it would put back
files the current round turned off. A jar that was turned off stays off: its
replacement is renamed back out of the loader's way, and a failed repair puts
the broken copy back under the name it had. A canceled verification is reported
as canceled rather than as a shorter report that reads as complete.

The desktop exposes both on each host workspace's Diagnostics surface.
Verification runs as an ordinary cancellable operation and shows its current
file; the result separates what a repair replaces, what nothing can fetch again,
and what has no checksum to prove it either way. Repair shows those lists before
it runs, claims the host-change state while it runs, and checks the host again
afterwards, so what the surface shows is what is on disk. When a failed rollback
leaves files in launcher scratch, the surface names them and the host keeps its
recorded attention. Shared launch files are checked for clients only, because
core checks them against a client's release metadata; a server's surface says
so.


## Diagnostic bundles

A bundle collects diagnostics into one archive somebody can attach to a bug
report (ADR 0038). It is not an upload: nothing contacts a service, no paste site is
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

The desktop builds its checklist on each host's Diagnostics surface from the
planner's own offer, so nothing can be chosen that the planner did not list.
It starts with the newest game log, the newest crash report, and the host's
recorded contents and record chosen; older logs and reports are offered and left
out until chosen. An empty choice cannot be exported. The destination comes
from the desktop's save dialog, the export runs as an ordinary operation, and
the result names the written file and what could not be read, with a way to
show its folder. A bundle holds one host's files: the launcher's own logs are
a separate scope the command line bundles with `ephervex logs bundle --launcher`.


## Security boundaries

### Untrusted paths and archives

Archive entry names, symlink targets, and provider paths are untrusted. The
shared path validator rejects absolute paths, prefixes, traversal, empty
normalized paths, and launcher-owned metadata destinations. Extractors also
refuse writes through links created by earlier entries and enforce entry-count
and uncompressed-size limits. The pack archive reader stops parsing the central
directory at the first header past the entry-count limit, across every
end-of-central-directory candidate the ZIP library tries, so rejecting an
oversized archive does not parse or build a record per declared entry. A ZIP64
end record that declares more entries than the limit is refused before the ZIP
library reserves room for that count. The reader enforces sizes while an entry
is inflated, not only from its declared size, and gives a manifest a much
smaller ceiling than other entries because it is held in memory to be parsed.

Imported pre-launch commands, post-exit commands, wrapper commands, and custom
Java paths are never executed. Unsupported customizations are reported or
retained as source metadata.

### Downloads

Production downloads use HTTPS, bounded redirects, timeouts, and explicit host
policy. The strongest supplied hashes are checked, and a mismatch is a hard
failure. Atomic publication ensures another operation cannot mistake partial or
unverified bytes for a cached artifact.

A download whose address came from remote input, such as a pack file, a managed
mod's artifact, a project icon, or a pack archive behind a link, carries a host
scope. The address is checked against the scope's hosts again when it is sent,
and a redirect may reach only those hosts and the short list of delivery hosts they
are known to redirect to (the storage hosts behind GitHub release assets for
Modrinth packs). Any other redirect is not followed and fails naming the host,
so a host the launcher was allowed to contact cannot send it to one it was not
(ADR 0027).

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

Authentication requests carry credentials in their bodies (authorization codes,
refresh tokens, identity tokens), so the authentication client refuses to follow
a redirect that leaves the origin a request was sent to. A `307` or `308` would
otherwise replay the body to the other origin. The refusal surfaces as a failed
sign-in or refresh.

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
download is confined to its own provider's hosts and is never offered the
header.
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
file. The preferred mod catalog under `[mods]` is stored this way. The
interface theme under `[display]` (`color_scheme`, holding `dark`, `light`, or
`user:<id>` for a theme file) is stored as text for the same reason, so a theme
added later does not make an older build refuse the file. It
always records a name older builds read, so nothing recorded and the default
scheme are the same value there. A name this build cannot resolve paints in the
default palette and is written back unchanged until the user picks another
theme.

Errors are typed by domain and composed through `CoreError`. User-visible text
uses Fluent message IDs and typed arguments, while paths, versions, hashes, and
provider IDs remain exact technical values.

CLI JSON output is written to stdout. Logs and human progress are written to
stderr so machine-readable output remains valid.

## Third-party licenses

What Ephervex depends on, and under what terms, is generated from the real
dependency graph and committed. `cargo-about` reads `Cargo.lock` and each
crate's own license file; `tools/licenses/inventory.py` turns that into a
compact inventory at `crates/core/data/third-party-licenses.json` and the
readable `THIRD-PARTY-LICENSES.md`. Nothing in the application decides what a
dependency is licensed under, and there is no code path that could: the only
source is the compiled-in constant.

`App::third_party_licenses` parses it once per process and both frontends read
that one service, so `ephervex licenses` and the desktop application's "About &
Licenses" screen cannot disagree about what a release contains. It is an
associated function rather than a method, because compiled-in build data needs
no storage, no network, and no open application, and an about screen should
answer on a machine whose storage is broken.

Texts are deduplicated by content rather than by identifier. Forty crates under
MIT ship forty MIT files differing in their copyright line, and collapsing them
would drop the attribution the license exists to require, so a crate names the
texts that cover it and the same text is stored once.

Attribution is satisfied by the documents the artifacts carry, not by the
screen. Every artifact ships `LICENSE` and `THIRD-PARTY-LICENSES.md`; the
screen and the command are how a user finds them. Where a license has a further
condition, it is met where the condition says: Slint's royalty-free license
asks for its `AboutSlint` widget on an about screen, and that widget is on the
"About & Licenses" tab (ADR 0001).

A dependency change that is not regenerated fails CI, and `cargo deny check
licenses` refuses a license `deny.toml` does not allow, so an unexpected license
is a review question rather than something already released.

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
aspect hint. Fabric, Quilt, Minecraft snapshot releases, Windows, and macOS are
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

The release gate adds the maintainer tooling's own tests, a release build, and
the packaged artifact's smoke test. Packaging, the declared Linux baseline and
how it is enforced, and the full gate are in
[`docs/release.md`](release.md) and [ADR 0023](adr/0023-appimage-is-the-linux-release-artifact.md).
