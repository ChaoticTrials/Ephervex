# ADR 0058 - A host keeps a bounded history under the state root

**Status:** Accepted

## Context

A developer reproducing a bug wants to read back what happened to an instance
or server: when it was created, when it started and how it ended, what its log
complained about, and how often the bug showed up again. None of that is in
the host's record, and the launcher's own log is a poor place to look, because
it covers every host and is rotated.

The history must not roll back with the host (a snapshot restore rewrites the
host's files, ADR 0048), must not travel in diagnostic bundles by accident
(ADR 0038), and must not need the host claim a record write takes, because a
run ends while the host is claimed by that run.

## Decision

**One JSON Lines file per host** under `<state>/host-events/`, named for the
host's kind and id. The state root already holds the output captures that
belong to a host and are removed with it, and the history follows the same
rule: it is removed when the host's files are deleted and when its record is
dropped because they are gone.

**Appended, never rewritten in place.** An event is one compact JSON object on
one line, written with a single append. Appending and cutting back both run
under an exclusive `flock` on a lock file beside the history, so two launcher
processes cannot interleave a line or lose an event to a cut-back. A file past
1 MiB is cut back to its newest 400 events through the atomic publish helper,
and a history that cannot be read is left as it is.

**Typed, and read as untrusted text.** An event has a time, a kind, a short
subject and an optional detail. The kind is a closed set the frontends
localize; a kind this build does not know reads as `other`. Every text is
cleaned of control and layout-changing characters and bounded when it is
written and again when it is read. The file is read as bytes, and a line that
is not UTF-8 or does not parse costs that line only.

**Recording never fails an operation.** A history that cannot be written is
logged and skipped.

## Consequences

The history survives a host's files being reinstalled or restored, and goes
when the host does. It is not part of a diagnostic bundle unless a later
decision adds it. Events are recorded where the fact is known: creation,
launch and exit of a client, start and stop of a server, the crash report a
failed client run left, and each time the bug is reproduced again.

A run that outlives the process that started it records its start and not its
end: a game or server left running by a closed window or by a detached command
has no one left to report how it ended.
