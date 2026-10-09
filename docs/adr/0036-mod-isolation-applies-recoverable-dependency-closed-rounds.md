# ADR 0036 - Mod isolation applies recoverable dependency-closed rounds

**Status:** Accepted

## Context

A binary search over jar filenames can produce a configuration that never had a
chance to start. A selected mod may need a library in the other half, several
roots may share one library, and a pinned mod may itself have transitive
requirements. Counting those dependencies as independent suspects also makes
the result misleading.

Isolation changes the live host repeatedly while the user launches and tests
it. The application can close between rounds or during a batch of renames, and
the user can answer a round incorrectly. The workflow therefore needs durable
state and an exact way back to the state that existed before it began.

## Decision

The unit of isolation is a candidate root together with its complete transitive
required-dependency closure from the same graph used by ordinary mod toggles.
Every enabled mod outside the pinned closure is a candidate, including one that
another candidate requires. A required edge says which configurations are
valid, not that the required mod cannot be the cause, and Ephervex does not
guess which mods are libraries. Only a required cycle, whose members cannot be
separated by any valid configuration, is merged under one deterministic
representative. Pinned mods and their complete required closure are enabled in
every round.

Every test configuration is recomputed from scratch as the union of the pinned
closure and the closures of the selected candidate roots. It is never formed by
splitting the flat jar list. A dependency shared by several roots therefore
stays enabled whenever at least one selected root needs it. The resulting
configuration is applied through the existing dependency-aware batch mutation
service.

The search is deterministic ddmin over dependency-closed candidate sets. The
suspect set is always closed: every candidate a suspect requires is itself a
suspect. A subset round enables its group and every suspect the group requires.
A complement round removes its group and every suspect that requires a removed
candidate. Both are therefore closed, and a reproducing round's complete
candidate set, not only its selected group, becomes the new suspect set. This
is what keeps overlapping closures honest: when a mod and its dependency were
both enabled, the search keeps both as suspects and can still test the
dependency without the mod.

The search starts at granularity two, tests each subset, and then tests each
complement. A reproducing subset resets granularity to two. A reproducing
complement reduces granularity by one, with a floor of two. If neither kind
narrows the set, granularity doubles up to the suspect count. A configuration
is skipped as not reproducing when it would enable no candidate, every suspect,
or only files that an earlier non-reproducing round also enabled. Exhausting
the count establishes a one-minimal set: removing any one remaining candidate,
together with the candidates that require it, stopped reproduction. Reaching
zero or one candidate uses a confirmation round and reports only that the
smallest testable candidate set was reached.

One session belongs to one client or one server. A client session lists linked
servers but never changes them. Missing required dependencies and refreshable
but unrecorded dependency knowledge each require an explicit
reduced-confidence acknowledgement. Unidentifiable jars cannot be refreshed,
so they remain independent candidates and make the result reduced-confidence.

The session is a compact version-1 document at
`<host>/.launcher/isolation.json`. It records the safety snapshot, plan, search
state, exact intended round, complete answered-round history, contradiction,
and outcome. The document is atomically replaced before its intended
configuration is applied. Resume idempotently reapplies that configuration, so
an interruption during a batch does not make the partially changed directory
authoritative. Reconciling compares the host's mods with that configuration
first and resumes only when they differ, so an already correct host is not
rewritten. An answer is refused while the host does not have the round's mods,
because it would describe a configuration that was not tested. The desktop
reconciles whenever it reads an active session; a running host or a failed
reapplication leaves the session untouched and blocks answers, undo, and finish
until a retry succeeds. The version stays at 1 under ADR 0033's pre-1.0 policy, and a
newer document is rejected.

Each history entry retains the exact configuration, answer, timestamp, and
pre-answer search state. Undo restores that state and reapplies that exact
round. A non-reproducing answer for a superset of an earlier reproducing
configuration records a contradiction and stops progression; it is not
silently resolved.

Starting and every operation that changes round state refuse while the host is
running. Merely waiting for an answer holds no launch lock, because launching
the current round is the purpose of the workflow.

Starting creates a local snapshot with origin `before_isolation` before any mod
mutation. Cancel restores that snapshot through the snapshot service and then
removes the session document. Finish reapplies the outcome, removes the session
document, leaves the smallest tested configuration enabled, and retains the
safety snapshot under a dated label. Its report names the remaining candidates
that no other remaining candidate requires, and separates them from the files
they require, from pinned roots, and from the files pinned roots require. The
last two were never tested for removal.

Any change to the host's mod set is refused while a session is active:
installing, updating, changing the version of, removing, adding, restoring,
deleting, enabling, or disabling a mod, changing the loader build, repairing
the host, restoring a snapshot, and linked mod operations. A mod added or
replaced during a session would either break every later round or escape being
a suspect. Rounds, resume, finish, and cancel are the session's own changes and
are not refused; cancel restores the session's safety snapshot itself.

A session starts under the host's recipe gate, and every change that holds the
gate checks for a session again after taking it. A change that was already past
its first check therefore either finishes before the session's safety snapshot
is taken or is refused. The gate is process-wide, so this does not reach a
second Ephervex process. The linked mod plan marks a server with a session as
blocked, so a confirmation never offers a change that applying would refuse.

Deleting the safety snapshot is refused while its session is active. A session
whose snapshot is already missing still ends: finish skips the dated label, and
cancel re-enables the mods the plan recorded as initially enabled.

## Consequences

- A round may enable more than half of the jar files because required
  dependencies are validity constraints on every configuration.
- A mod required by a remaining candidate is reported as required by it rather
  than as a cause, because it was tested without that candidate wherever a
  valid configuration allowed.
- A mod required by a pinned mod cannot be tested away. It is reported as
  constrained by the pin, not as ruled out.
- A one-minimal result need not be the globally smallest reproducing set. The UI
  and CLI distinguish it from a smallest testable zero- or one-candidate result.
- A remaining candidate is not a unique culprit when pinned roots stayed in
  every round. Result presentation names those untested pinned roots separately.
- Results based on missing, unrecorded, or unidentifiable dependency
  information are explicitly unproven.
- The session document makes an interrupted workflow visible after restart and
  gives resume one authoritative intended configuration.
- Cancel and finish have deliberately different recovery semantics. Cancel
  restores immediately; finish keeps both the narrowed host and a retained way
  back.
