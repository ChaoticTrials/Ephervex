# ADR 0043 - Diagnostics explains, and changes nothing until a previewed fix is applied

**Status:** Accepted (builds on ADR 0016 and ADR 0038)

## Context

A developer reproducing a bug needs to know whether the host is configured
correctly for what it runs, and why a launch or crash failed. Verifying
recorded files answers only part of that. Most failures come from
configuration: a Java runtime that cannot run the release, a required mod that
is missing or turned off, or a dependency the loader refused.

Reproducing bugs also means running deliberately unusual setups. A tool that
warns about every custom Java path or JVM argument teaches the user to ignore
it, and one that "fixes" configuration on its own destroys the reproduction.

## Decision

**Two classes of finding.** A problem is objectively incompatible, missing, or
damaged. A customization differs from what Ephervex would choose and still
works. A valid custom runtime is a customization; only a runtime that cannot
run the release is a problem. A host with customizations and no problems is
healthy.

**Only stated evidence.** Findings come from what Ephervex already records or
reads: the Java plan and its override layers, the relations a catalog declared
(ADR 0034), the loader's own refusal in the last start's log, the newest crash
report written after the last start, and, when asked, the recorded files'
digests. Nothing is inferred from file names, and no conflict is guessed at
without metadata that states it.

The loader names one line per asking mod, so its refusals are grouped by the
exact mod id they lack. A loader mod id is not a catalog project, a display
name, or a jar name, so a loader refusal, a recorded relation, and a crash
report stay separate findings however alike their names are, and only a
finding backed by a recorded or proven catalog identity offers to restore or
install a mod. The versions shown are the one range every asking mod accepts
when that follows with certainty from Maven's range rules and version order;
otherwise each asking mod's range is shown as the loader wrote it.

A relation names a project in its requirer's own catalog, so it only proves an
absence when every jar here is identified in that catalog. Otherwise Diagnose
asks that catalog to identify the other jars by their bytes (SHA-1 on
Modrinth, fingerprint on CurseForge), and the quick pass after a failed run,
which asks nothing, counts the requirement as unconfirmed rather than missing.
A missing project known only by id is named from the catalog, and neither
answer is recorded (ADR 0037).

**Manual by default.** Opening Diagnostics runs nothing. Diagnose checks
configuration and hashes recorded files, with progress and cancellation. When
a run fails, Ephervex runs a quick pass that hashes nothing, shows a notice
with the likely cause above whichever of the host's surfaces is open, and never
navigates away from what the user is looking at. When the failure is a stored
file that is damaged or unreadable, core names that as a typed remedy and the
notice also offers to verify the host's files. Any action on the notice
dismisses it.

**A fix is previewed, and the preview is the confirmation.** A finding may
carry a fix only when Ephervex knows exactly what to change: enabling
turned-off required mods, removing this host's own Java override when
automatic selection leads somewhere that works, or restoring recorded files
from their recorded sources. A required mod the host records, whose jar alone
is gone, is such a file: it comes back at the recorded release. The preview
lists every change, and Apply performs exactly those changes with no further
dialog. Each change is reported on its own, so a partial result says what did
and did not change, and the host is diagnosed again afterwards. Where the
change is ambiguous, such as which version of a missing mod to install,
Diagnostics explains the problem and routes to the surface that owns it
instead. A missing mod whose catalog project is known and that the host does
not record is offered, on a client or a server, as an install of the newest
compatible release through the install confirmation, which names the exact
release before anything changes.

**One diagnosis or fix at a time.** Results and previews are keyed by host and
shared by every pane on it (ADR 0039). A fix claims the host like any other
host change, and is refused while the host runs or, for a change to its mods,
while an isolation session owns them.

**A redacted report and a verbatim bundle.** The technical report summarizes
conclusions for pasting into an issue or a chat. It writes paths under the
home directory from `~` and never includes JVM argument values. The diagnostic
bundle keeps its own rules (ADR 0038) and remains the second, deeper step.

## Consequences

- Diagnostics can say "healthy" about a heavily customized host, and a problem
  finding always means something is actually wrong.
- Fabric and Quilt refusals are not read from the log, because their wording
  is not a shared, stable format; recorded relations still cover those hosts.
- A version constraint that only a jar's own metadata states is not checked
  until the loader refuses it at a start.
- A failed-run notice lives with the host's view and is forgotten when the
  host leaves the workspace, like every other host-keyed state.
