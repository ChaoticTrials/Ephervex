# ADR 0015 — Linked client direct connection uses canonical lifecycle services

**Status:** Accepted

## Context

A linked client and server are separate durable entities and separate
processes. A frontend that starts the server, guesses an address, waits, and
then launches the client would duplicate application logic and could disagree
with server ownership, authentication, Java selection, or network settings.

A server bind is also not always a client destination. `0.0.0.0`, `::`, and an
empty `server-ip` mean “listen on interfaces”; asking a client to connect to a
wildcard is ambiguous. Starting the client as soon as Java spawns is too early,
while an unbounded or fixed startup sleep is not a readiness condition.

## Decision

Launch & Connect is one `ephervex-core` application service. Its request names
both durable IDs. Core reads canonical client and server records, requires the
server's link to name that exact client, and requires their exact Minecraft and
loader selections to agree. Missing entities, missing or mismatched links,
incomplete server installation, invalid network settings, and uncertain or
orphaned runtimes are typed failures.

The connection target is derived from canonical typed server network settings:

- localhost and all-interface binds use IPv4 loopback;
- an IPv4 wildcard uses IPv4 loopback;
- an IPv6 wildcard uses IPv6 loopback; and
- a specific custom address remains exact.

The validated nonzero port remains exact. IPv6 targets use bracketed host-port
display where a combined address is needed. Deriving this target reads no
properties file and changes no server setting.

A stopped or proven-stale server starts only through the existing lifecycle
service, including canonical persistence checks, EULA enforcement, server Java
selection, runtime ownership, output capture, and network-property merge. A
controlled server is reused. Core never attempts a duplicate start or adopts
an unknown or orphaned process.

Readiness is a successful TCP connection to the derived target within a
bounded 120-second deadline. Cancellation is immediate. During a new start,
the supervisor result is observed so process-start failure or exit wins over
readiness. During reuse, runtime classification is monitored so a stopped or
uncertain server cannot be reported ready. Connection refusal is retried on a
bounded interval; there is no fixed startup sleep.

After readiness, core invokes the normal client launch service. It preserves
the selected client, account and token behavior, client Java choice, JVM
settings, exact loader metadata, launch lock, artifacts, and process capture.
Modern metadata receives Mojang's Quick Play multiplayer argument. Releases
without that declared argument receive the established `--server` and `--port`
game arguments. Direct-connect values appear only in the game-argument layer.

The server supervisor remains the sole process owner on an application task.
Client launch failure or client exit does not request server stop. As with all
foreground-supervised servers, ending the owning Ephervex process ends that
supervision according to ADR 0014.

## Consequences

CLI and GUI surfaces need only choose a valid linked pair, subscribe to
progress, and call core. They never interpret binds, probe ports, build
Minecraft arguments, or own server processes. Offline-mode servers do not
create a new client identity path: the configured account still goes through
ordinary authentication and launch behavior.

TCP readiness proves that the local endpoint accepts connections; it does not
promise that every world or mod has finished optional work after the listener
opens. Server output remains available for diagnosing a later disconnect.
