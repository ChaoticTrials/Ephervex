# ADR 0050 - Server properties saved while running wait for its restart

**Status:** Accepted

## Context

A developer reproducing a bug changes a server setting, such as the difficulty
or the message of the day, and restarts the server to see the effect. Requiring
the server to be stopped before a save turns that into stop, edit, save, start,
and the Restart action in the header goes unused for the most common reason to
restart.

Minecraft reads `server.properties` once, when the process starts. It also
writes the file back from the copy it read, at startup and whenever a command
such as `/whitelist on` changes a setting it persists. A value written into the
file while the server runs is therefore not read by that process, and can be
overwritten by it before any process reads it.

The typed network settings (online mode, bind address, port) are also the
server's connection target. A linked client connects to what the record says,
so the record cannot describe a port the running process is not listening on.

## Decision

**Gameplay properties can be saved while a server runs under Ephervex.** A save
is accepted while the server is stopped, or while its process runs under an
Ephervex supervisor with a live child. It is refused while the server is
starting, during a restart's handover, and while a process runs that Ephervex
does not control.

**The value is written now and kept until the next process starts.** Each value
that differs from what the next start would read is written to
`server.properties` and kept in `.launcher/server-properties-pending.json`.
Before the supervisor spawns any process, for a start or a restart, it writes
the kept values into `server.properties` again and forgets them. A value the
running server wrote over in the meantime is restored at that point. A save made
while the server is stopped writes the kept values first, then its own, so the
newer save wins where both set the same key.

**A save carries only what the user changed.** The Properties form sends the
values that differ from what it was loaded with. A setting the running server
changed itself since, such as the allowlist after `/whitelist on`, is not put
back by a save that did not touch it.

**A snapshot restore forgets kept values.** They were saved into the files the
restore replaces, and applying them at the next start would undo part of it.

**Saving never restarts.** A save while running reports that the change takes
effect when the server restarts, and the server shows Restart required until a
new process has read the values. Restart remains the user's explicit action.

**Network settings change only while stopped.** A save while running must carry
the stored network settings; a different one is refused, and the form disables
those fields while the server runs.

Reads and writes of the kept values and the file they apply to take an
exclusive lock in `.launcher/server-properties.lock`, so a save and a
supervisor's start never interleave. The host claim still keeps a save out of
start preparation and every other change that rewrites the host.

## Consequences

The Properties form works during a run, and a header pill and the Overview say
when a restart is still needed. The CLI `server properties set` follows the same
rule and `server status` lists properties that wait for a restart.

A value saved while running and then changed by the server itself, for example
by `/whitelist off` after the allowlist toggle was saved, is set back to the
saved value at the next start. The explicit save is treated as the newer intent.

An edit made to `server.properties` outside Ephervex while values are kept is
overwritten for those keys at the next start. Opening the file while the server
runs shows the saved values, because they are written to the file at once.
