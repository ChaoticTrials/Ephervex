# ADR 0014 — Dedicated server authority and conservative supervision

**Status:** Accepted

## Context

A disposable dedicated server is both durable state and a live operating-system
process. Treating SQLite as an equal authority to per-server metadata makes a
crash between writes ambiguous. Treating a PID as proof of process identity
makes PID reuse look like a live server. Treating an existing `eula.txt` as
consent lets an imported or previously generated file authorize a new launch.

Server output also differs from Minecraft's own logs. The launcher must drain
both pipes to avoid deadlock, retain enough output for a frontend, and avoid
unbounded memory or state-directory growth.

## Decision

`.launcher/server.json` is the canonical server entity. It records identity,
the exact Minecraft and loader selection, storage and client linkage, typed
network settings, Java and JVM overrides, installation state, source,
attention, and timestamps. An installed launch descriptor contains only a
validated path relative to the server game directory.

SQLite is a query index and last-known recovery copy. Metadata publication is
the commit point. SQLite is updated afterward; an index failure is reported as
`index_out_of_sync` on an otherwise committed result. Reconciliation applies
these rules:

- valid metadata replaces a disagreeing index row;
- a discovered directory location corrects a stale serialized root;
- valid unindexed persistent metadata is indexed;
- multiple valid roots claiming one server ID are a conflict and none is
  selected automatically;
- a missing temporary root drops its stale row;
- a missing persistent root retains its recovery copy; and
- missing, corrupt, or unsupported metadata retains the recovery copy for
  display but blocks launch and mutation.

New servers are installed in a hidden sibling root. The complete root,
including canonical metadata and managed default properties, is renamed into
place before indexing. Existing uninstalled records may be installed only into
an empty game directory. Vanilla, Forge, and NeoForge installations reuse the
loader providers and shared verified stores; exact selections are never
substituted.

Minecraft EULA consent is global and canonical only in `config.toml`. Valid
consent contains an explicit accepted flag, a UTC timestamp, and the canonical
EULA reference. Creation and installation never write `eula.txt`. A start
validates current global consent before atomically merging `eula=true`.
Revocation blocks later starts without rewriting server files.

The foreground supervisor owns the Java child, stdin, output readers, runtime
lock, and a mode-0600 Unix control socket. The socket lives in a short
mode-0700 per-user runtime directory so an arbitrarily long configured server
root cannot exceed Linux's Unix-socket path limit. Its name is keyed by the
server root's filesystem identity and the supervisor generation recorded in
`.launcher/server-runtime.json`. Runtime evidence also records separate
supervisor and Java-child roles, Linux boot ID, PID, `/proc` start ticks,
canonical executable, and device/inode identity where available. PID alone is
never sufficient.

A verified supervisor with a reachable socket is controlled. A verified
supervisor without a reachable socket is unknown-running. A dead supervisor
with the same verified live child is orphaned. Unverifiable identity is
unknown-running. Evidence is stale only after both recorded processes are
proven dead or the boot ID changed. Ephervex never adopts, writes stdin to, or
signals an orphan. Recovery clears evidence only when it is proven stale.

`Stop` writes `stop\n` and waits at most 30 seconds without escalation.
`Restart` respawns only after graceful termination. `Kill` is available only
through the live owner channel. Abrupt supervisor termination can leave an
orphan despite best-effort child cleanup.

Combined launcher capture is timestamped JSON Lines. The in-memory ring is
bounded to 2,000 lines and 2 MiB, individual records to 64 KiB, segments to
8 MiB, and retained segments to 256 MiB per server. Pruning updates an atomic
index and emits an observable event. A capture write failure degrades capture
health but never stops pipe draining or bounded in-memory retention.

## Consequences

Server display can recover from a damaged index without allowing an unhealthy
recovery copy to authorize a launch. Cross-process duplicate starts are
refused conservatively. Users receive an exact PID and executable for manual
orphan cleanup, but Ephervex does not claim ownership it cannot prove. Server
creation may leave reusable verified shared downloads after a failed staging
transaction, while no incomplete server root is published.
