# ADR 0039 - A split workspace composes host views and owns no feature state

**Status:** Accepted

## Context

Reproducing a bug often needs two things on screen at once: a client's
isolation round and its log, a server's console and its log, or a client and
its linked server side by side. The host workspace showed one host and one
surface at a time, and every surface kept its state in one set of window-level
properties: one log viewer, one mod list, one isolation setup, one Diagnostics
result. Showing a second host, or the same host twice, would either duplicate
that state per pane or let one pane overwrite what another shows.

A general docking system would solve the layout problem, but it brings
recursive split trees, drag targets, floating panes, and saved presets, and
each of those needs its own rules for what happens to a surface's state when a
pane moves. None of that is needed to show two to four views together.

## Decision

A pane chooses where a host's feature is displayed. It never becomes the
owner of that feature or its state.

**Constrained shapes.** The layout is a typed model whose only representable
values are a single pane, two full-height panes side by side (1:1), a stacked
left column beside a full-height pane (2:1), a full-height pane beside a
stacked right column (1:2), and two stacked columns (2:2). Splitting beside is
offered only from a single pane, and splitting below only to a full-height pane
of a split. Closing a pane settles immediately on another of these shapes, in
reading order, so the workspace never holds a gap. There is no recursive
splitting, dragging, floating, detaching, or resizing, and the columns and rows
are equal.

**Host-keyed state.** The controller builds one view per shown host, however
many panes show it, and every feature reports by host. Two panes on one host
read the same row, workflow, isolation setup, mod list, and Diagnostics result.
A feature forgets a host once no pane shows it, and asks afresh when a pane
shows it again. Durable work is not tied to a pane: closing or retargeting a
pane never stops a process, cancels an isolation session, or abandons a
verification that is already running.

**Pane-keyed presentation.** What only one pane is looking at stays with that
pane: each Logs or Console pane has its own log viewer with its own file,
filters, search, and selection.

**Guards stay global.** The one-at-a-time rules that protect a host (a single
isolation change, a single verification or repair, one restart per host, the
host-change claim that makes launching wait) are held by the controller, not by
panes, so a host shown in several panes cannot be asked to do the same thing
twice.

**No persistence.** The workspace opens as a single pane on the host chosen
from a list. Opening a host from a list replaces the layout, and a split is not
restored on the next start.

## Consequences

A surface does not know whether it is shown alone or beside others, so a new
host surface, such as Diagnostics, joins the split without another container.
Later work that pairs a client with a server can arrange panes through the same
model instead of a second side-by-side layout.

Every feature that shows host state has to be keyed by host and has to follow
the set of shown hosts. A feature added as window-level state would show one
host's answer in another host's pane.

Surfaces laid out for the full-width workspace can be narrower than they would
like at the smallest window size. Panes never shrink below a minimum size and
never overlap; below that size the pane area scrolls, and a surface that needs
more room scrolls or clips within its pane.
