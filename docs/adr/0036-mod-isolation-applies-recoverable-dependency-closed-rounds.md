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
A root required by another candidate is carried as a dependency instead of
being counted independently. A required cycle has one deterministic
representative. Pinned mods and their complete required closure are enabled in
every round.

Every test configuration is recomputed from scratch as the union of the pinned
closure and the closures of the selected candidate roots. It is never formed by
splitting the flat jar list. A dependency shared by several roots therefore
stays enabled whenever at least one selected root needs it. The resulting
configuration is applied through the existing dependency-aware batch mutation
service.

The search is deterministic ddmin. It starts at granularity two, tests each
subset, and then tests each complement. A reproducing subset becomes the new
suspect set and resets granularity to two. A reproducing complement becomes the
new suspect set and reduces granularity by one, with a floor of two. If neither
kind narrows the set, granularity doubles up to the suspect count. Exhausting
that count establishes a one-minimal set: removing any one remaining candidate
stopped reproduction. Reaching zero or one candidate uses a confirmation round
and reports only that the smallest testable candidate set was reached.

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
authoritative. The version stays at 1 under ADR 0033's pre-1.0 policy, and a
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
document, leaves the reproducing configuration enabled, and retains the safety
snapshot under a dated label.

## Consequences

- A round may enable more than half of the jar files because dependencies are
  correctness constraints, not suspects.
- A one-minimal result need not be the globally smallest reproducing set. The UI
  and CLI distinguish it from a smallest testable zero- or one-candidate result.
- Results based on missing, unrecorded, or unidentifiable dependency
  information are explicitly unproven.
- The session document makes an interrupted workflow visible after restart and
  gives resume one authoritative intended configuration.
- Cancel and finish have deliberately different recovery semantics. Cancel
  restores immediately; finish keeps both the narrowed host and a retained way
  back.
