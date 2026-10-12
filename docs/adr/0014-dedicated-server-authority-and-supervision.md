# ADR 0014 - Dedicated server authority and conservative supervision

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
`index_out_of_sync` on an otherwise committed result. A directory sync failure
after the metadata rename leaves the new file published, so it is reported as
`metadata_not_durable` on a committed result and the index is still updated.
Reconciliation applies these rules:

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
mode-0700 per-user directory so an arbitrarily long configured server
root cannot exceed Linux's Unix-socket path limit. The directory is
`$XDG_RUNTIME_DIR` when that is a private directory of the current user, then
`/tmp/evx-<uid>`. When another account has occupied that predictable name, the
run creates `/tmp/evx-<uid>-<random>` for itself and removes it afterward, so
no other account can block a start. A directory is used only when it is an
absolute real directory owned by the current user with mode 0700, and is never
reached through a symlink. The chosen directory is recorded in the runtime
evidence, which is how `stop`, `console`, and classification find the socket
whatever their own environment; evidence without it names `/tmp/evx-<uid>`.
The socket's name is keyed by the server root's filesystem identity and the
supervisor generation recorded in
`.launcher/server-runtime.json`. Runtime evidence also records separate
supervisor and Java-child roles, Linux boot ID, PID, `/proc` start ticks,
canonical executable, and the device/inode of the process image where
available. Boot ID, PID, and start ticks identify a process. The executable is
informational: a file replaced by an upgrade, or a process that executed
another program, is still the same live process. PID alone is never
sufficient.

A verified supervisor with a reachable socket is controlled. A verified
supervisor without a reachable socket is unknown-running. A dead supervisor
with the same verified live child is orphaned. Unverifiable identity is
unknown-running. Evidence is stale only after both recorded processes are
proven dead or the boot ID changed. The supervisor records that a child is
being started before it spawns it, and clears that mark when the child is
recorded. Every server child carries the run's generation in its environment,
and the record names the supervisor's session. When a supervisor dies in
between, the processes of the same user that started after it and carry that
generation stand in for the missing child record: one alive is an orphan, and
a machine restart proves the evidence stale. So does finding none on two looks
a short time apart, which assumes that a start reaches the Java image within
that interval and that the child keeps the variable. A process whose
environment this user cannot read is skipped unless it leads its own process
group in the supervisor's session, as a child does; that one, like any scan
error, leaves the state unknown-running. The check assumes the classifying
process runs as the supervisor's user. Ephervex never adopts, writes stdin to,
or signals an orphan.
Recovery clears evidence only when it is proven stale.

The runtime lock is an exclusive `flock` on `.launcher/server-runtime.lock`,
held for the whole run and dropped by the kernel with its holder. A lock file
that no process holds never blocks a start, whatever it contains. A start and
recovery both take the `flock` before they check or clear anything, so only one
of several racing starts can take over a leftover lock. A lock file written
before locks were flocked is respected while the supervisor it names is alive.

The `server stop`, `server restart`, `server kill`, and `server console`
commands print the supervisor's response and exit with a non-zero status when
it refused the request or a restart left the server stopped.

The foreground `server start` command reports the supervised run's result in
its exit status. A stop asked for through the control channel, Ctrl-C, a
terminal hangup, or a termination request is success. Otherwise the status is
success only for a server that finished starting and exited with status 0;
exiting before startup finished, a non-zero status, or a signal is a failure.
Such a request while the server is still being prepared cancels the start
before anything runs, and a second one kills a server that is slow to stop. A
hangup that the process was started ignoring, as under `nohup`, is not a stop
request. The server runs in a process group of its own, so a terminal signal
reaches it only through the supervisor's graceful stop. A standard output that
stops accepting writes ends the command the same way as the first request, so
the command never exits while its server runs. That is not itself a failure
unless the write error was something other than a closed pipe or a terminal
that hung up. `ephervex launch` applies the same rule to the game: a dry run, a
clean exit, or a game ended with Ctrl-C or a closed output is success. A launch
canceled before the game started is a failure.

`Stop` writes `stop\n` and waits at most 30 seconds without escalation.
`Restart` respawns only after graceful termination. `Kill` is available only
through the live owner channel and ends every process in the server's process
group, so a wrapper's own children go with it. Abrupt supervisor termination
can leave an orphan despite best-effort child cleanup. The desktop application
supervises the servers it starts on threads of its own process, so quitting
stops each of them first (`Stop`, then `Kill` when it does not exit) and waits
for the supervisors to finish before the process exits. A termination request
(SIGTERM, SIGINT, or SIGHUP unless it started ignoring hangups) counts as
quitting: it ends the event loop, and repeated requests during the stop are
absorbed.

A run ends when the server process exits. Output that a descendant still holds
open is read for a short while longer and then dropped, so a lingering
descendant never holds back the exit, a restart, or control answers. A console
line is written without pausing supervision, so a server that does not read its
input cannot delay a Kill or the detection of its exit.

However supervision returns, including a failed start or a supervision error, the
supervisor ends any child it started, removes its socket, withdraws its runtime
evidence, and releases the lock, so the server classifies as stopped and can be
started again. Evidence is kept only when a child it started may still be
alive. Every control request accepted before the lock is released gets
an answer: the final exit, or a refusal saying the server has stopped or could
not be supervised.

A supervision that is canceled before it finishes, as when the launcher gives up
waiting for it, withdraws its evidence the same way once its killed child has
left the process table. If the child is still listed after a short bounded wait,
the evidence stays.

Combined launcher capture is timestamped JSON Lines. The in-memory ring is
bounded to 2,000 lines and 2 MiB, individual records to 64 KiB, segments to
8 MiB, and retained segments to 256 MiB per server. Each supervised run, and
each restart within it, begins a segment of its own; a segment opened only
because the previous one filled carries a `.continued.jsonl` suffix. A reader
that wants the latest run starts at the newest segment without that suffix,
while the retained history across runs stays readable as a whole. Pruning
updates an atomic index and emits an observable event. A capture write
failure degrades capture health but never stops pipe draining or bounded
in-memory retention.

A frontend shows a server's live output by following its capture directory
through `ephervex-core`, so the output is the same whichever process supervises
the server. The follower reads only the bytes each segment gained, treats a
new segment without the `.continued.jsonl` suffix as a new run that replaces
what was shown, and reports a gap when retention removes a segment of the run
it is following. A frontend that also receives the supervisor's events in its
own process shows those lines and lets the follower stay silent meanwhile.
What the follower sends and the supervisor's events share one ordering point,
so a line the follower already sent is not shown again when its event arrives,
and a line whose event arrived first is in the next snapshot because the
supervisor writes a line to the capture before emitting it.

## Consequences

Server display can recover from a damaged index without allowing an unhealthy
recovery copy to authorize a launch. Cross-process duplicate starts are
refused conservatively. Users receive an exact PID and executable for manual
orphan cleanup, but Ephervex does not claim ownership it cannot prove. Server
creation may leave reusable verified shared downloads after a failed staging
transaction, while no incomplete server root is published.
