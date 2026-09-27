# ADR 0040 - A linked pair chooses one linked server and starts as one operation

**Status:** Accepted

## Context

A server created from a client records that client in its own
`linked_client_id` (ADR 0014). That link drives exact-version checks, linked
mod sync (ADR 0021), and Launch & Connect (ADR 0015), and one client can have
several linked servers. Working on a client together with a server needs a
narrower idea: which one of those servers the developer is using with it right
now, whether starting one side should start the other, and an easy way to show
both.

Server and client records are mirrored column by column into the SQLite index,
whose schema stays at its initial migration until 1.0.0 (ADR 0033). A new
field on either record would need a new column.

Starting both sides raises a feedback risk: if starting a client started its
server, and a server start started its client, each start would trigger the
other again.

## Decision

**A pair names one existing link.** A client has at most one paired server,
and it must be one of the servers linked to that client. Pairing never creates,
changes, or removes a link. Unpairing leaves the server linked, so mod sync and
Launch & Connect keep working. A server has at most one linked client, so it is
in at most one pair.

**The first linked server is the pair by default.** A client's oldest linked
server is its pair without the user doing anything, however that server was
created, whether by hand or by a pack import. Choosing another linked server
replaces it. Unpairing records the first server the user declined, and the
client stays unpaired while that server remains its first; once it is deleted,
the next first linked server is paired again.

**Pairs are launcher workflow state in `config.toml`.** They are stored under
`[linked_pairs.<client id>]` with the chosen server id, a `start_together`
flag, and the server the user unpaired from, and they survive a restart of
Ephervex. Only a choice is written: the default pair needs no entry. An entry
whose server is gone, no longer linked, or malformed falls back to the default,
and entries of clients that are gone or have no linked server left are dropped
the next time any pair is written.

**Pairing does not depend on the workspace.** Which panes show which hosts,
closing a pane, and moving between pages never change a pair. Showing a pair
side by side only adds a pane for the counterpart when the current layout can
grow without changing any existing pane. It never closes, replaces, reorders,
or retargets a pane, and a full layout is left as it is.

**Starting the pair is one operation.** With `start_together` on, the primary
Launch or Start action of either side becomes one Launch & Connect run: the
server is started or reused, and the client launches once it is ready. When the
server side starts it and the client is already running, only the server
starts. Nothing reacts to a host having started by starting another host, so
there is no path from one start back to another. The frontend holds one
in-flight pair start per pair, identified by a generation, and refuses a second
until the first settles. The worker settles it exactly once, including on
failure, cancellation, or refusal. Restart is a different operation and never
consults the pair.

**Failure keeps the pair and the running side.** A failed pair start reports
which side failed. A server that started is left running, and the pair is
unchanged.

## Consequences

Pairs are not shared through the index or with other launchers, and a pair
written by one Ephervex process is seen by another only when it next reads its
configuration. That matches how other workflow preferences are stored.

Later pair-aware work, such as isolating a client and its server together or a
pair restart, can read the pair from core without a new relationship model.
Each side keeps its own dependency graph and lifecycle.
