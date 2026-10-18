# ADR 0054 - A runner process hosts a client or server past the launcher's exit

**Status:** Accepted (amends ADR 0014 and ADR 0015)

## Context

A server's supervisor owns its Java child's stdin, its output pipes, its
runtime lock, and its control socket. When that supervisor lived inside the
desktop application, quitting the application meant stopping every server it
had started, because a server left running without its supervisor can only be
reported as an orphan (ADR 0014). A user who wants a server to stay up while the
application restarts, or while they close its window, had no way to say so.

A client game had the same problem in a different form. Its JVM was a child of
the Ephervex process with piped output and a launch lock owned by that process,
so ending Ephervex killed the game, or when it did not, left a game that nothing
supervised and that could only be signaled (`ephervex` reports it as orphaned).

## Decision

**A runner is a small background process that owns one host for as long as it
runs.** It is the same executable that started it, run again with an internal
argument that no command line accepts, so both frontends can host and no third
binary is packaged. The starting process does everything that needs the
application, such as the EULA, network settings, Java selection, and downloads,
and hands the finished command to the runner on its standard input. The runner
puts itself in a session of its own and supervises the server with the same
supervisor an attached run uses. Nothing about the runtime lock, the control
socket, the evidence, or the capture changes, so a server under a runner
classifies as controlled to every Ephervex process, exactly like a server
started by `ephervex server start`.

**The runner reports to its starter on a pipe, and does not depend on it.** The
runner writes the events its supervisor emits as one JSON message per line, and
the starting process turns them into the events an attached run would have
produced, so a frontend has one code path. The starter's claim on the server is
released when the runner says it owns the runtime lock. A starter that dies in
the milliseconds between starting the runner and that point drops the claim with
it, so another change could see the server as stopped just before the runner
takes the runtime lock. Such a change is not prevented: it has already checked,
and the runner can start the server in files it is changing. The runtime lock
covers every later change. When the starter exits,
the pipe closes, the runner stops writing to it, and the server keeps running.
Output continues into the capture, and a later process reads it from there.

**Hosting is a choice of the caller.** `Hosting::Attached` keeps the supervisor
in the calling process, which is what a foreground command such as
`ephervex server start` means. `Hosting::Detached` starts a runner. The desktop
application starts every server detached, so quitting the window or the
application leaves running servers running, and starting the application again
finds them as controlled servers, with their consoles and output. A termination
request still ends the event loop and the application, and nothing is stopped.
`ephervex server start --detach` does the same from the command line and
returns once the server runs.

**A runner stops its server on a termination request.** SIGTERM, SIGINT, or
SIGHUP delivered to the runner asks the server to stop through the ordinary
control channel, and a second request kills it. A runner leads a session of its own, so
a terminal's signals do not reach it. In the packaged-image case below it leads
a process group of its own instead, which keeps it out of the terminal's
foreground group.

**A packaged image is started again, not run from its mount.** The files of an
AppImage exist only while its runtime process lives, and that process ends with
the application. When the program lives inside such a mount, the runner is
started from the image file instead, in a process group of its own, which gives
it a runtime process and mount that last as long as the runner does. Anywhere
else the runner is started from the running image itself rather than from its
path, so it is the build that started it even after an upgrade or rebuild has
replaced the file.

**A client game gets the same runner, and the runner takes over the lock.** The
starting process prepares the launch as before: the account, Java, the install,
and the lock it takes before anything else is written. It then hands the
finished command, access token included, to the runner on standard input, which
is the only place the token travels. The runner opens a mode-0600 control socket
(the server control protocol and the same private per-user directory), takes
over the launch lock in place, starts the game in a process group of its own
with a run id in its environment, records the game in the lock, and holds the
shared artifact-cache lease for as long as the game runs. Taking the lock over
is refused unless the file still names the starter, so a lock released or
replaced meanwhile is never overwritten. Releasing the starter's handle
afterwards leaves the runner's lock alone, because the file no longer names the
starter.

The lock records the runner: its run id and control socket. Process identity
stays what ADR 0014 says it is: a process id with its start time. Two more
facts are required before anything is believed or signaled. The game must carry
the lock's run id in its environment, so an unrelated process that merely has
the recorded id and start time is not the game. A control socket must be served
by the recorded runner's process id, which is checked on the connection itself.
Evidence that fails either test is never adopted, and a launch whose runner is
gone is the orphan it was before.

The game's stdout and stderr go to a durable capture beside the server
captures, under `logs/clients/<instance>/`, while the starter, if it is still
there, receives the same lines as events. Minecraft's own `latest.log` is
unchanged and remains what the desktop application's live log follows.

A client has no console, so the socket answers `Status`, `Stop`, and `Kill`.
`Kill` ends the game and everything it started at once. `Stop` terminates the
game's process group and never escalates. The desktop application and
`ephervex instance stop` end a launch it did not start through this socket.

`ephervex launch --detach` starts a launch this way and returns once the game
runs. The desktop application starts every client detached, so quitting it
leaves games running, and the next start lists them as running and controllable.

## Consequences

A runner that dies abruptly leaves its server or game with a dead supervisor and a live
child, which is the orphan state ADR 0014 defines. Ephervex still never adopts,
writes to, or signals an orphan; the guarantee is that the common case, the
starter ending, no longer produces one.

Quitting the application no longer frees the memory of its servers and games.
The running view lists them, and stopping one is an explicit act.
