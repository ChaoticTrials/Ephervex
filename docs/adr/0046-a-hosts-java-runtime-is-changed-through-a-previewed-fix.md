# ADR 0046 - A host's Java runtime is changed through a previewed fix

**Status:** Accepted (builds on ADR 0043)

## Context

A host runs with whatever Java its settings resolve to: its own override, the
launcher-wide preferred path, or the runtime automatic selection picks from
the discovered and managed ones. That answer used to be visible only after a
launch failed. Reproducing a bug often needs a specific runtime, and a tool
that switches it behind the user's back, or calls a working custom runtime
broken, destroys the reproduction.

Diagnostics already judges a host's Java and offers a previewed fix that
removes the host's own override (ADR 0043). A separate picker, status line, or
launch check with rules of its own could disagree with it and with the launch.

## Decision

**One rule evaluation.** `ephervex-java` evaluates a runtime against a host's
rules in one function: the required major, the loader's update bounds such as
the legacy Forge Java 8 ceiling, and the host's processor architecture.
Automatic selection, the check of a pinned runtime, and the verdict on each
picker entry all call it. A host's status reuses the classification
Diagnostics uses, so the runtime section, the header, Diagnostics and the
launch give the same answer for the same inventory.

**Status separates how a runtime was chosen from where it came from.** A host
either selects automatically, is pinned by its own override, or is pinned by
the launcher preference. Separately, the runtime is Ephervex-managed or found
on the system. An automatic pick that resolves is healthy, as is a host whose
runtime is downloaded at launch. A pin that every rule accepts is valid,
customized, even when automatic selection would choose another runtime. Only a
rule violation or a pin that cannot be run is a problem.

**A change is a previewed fix.** Choosing a runtime in the picker, a custom
path the user checked, or automatic selection opens the same review a
Diagnostics fix opens. The review shows the runtime before and after (path,
version, vendor, architecture, origin) and what the host expects, and Apply
changes only the host's Java override. Installing the managed runtime a host
expects never pins it. When the host would not pick the new runtime on its
own, pinning it is offered as another previewed change.

**A pin is an exact path.** Pinning a managed runtime records its executable
path; the instance and server formats do not change. A pin whose runtime has
since been removed is the existing unusable-pin problem, and its fix leads
back to the picker or automatic selection. Removing a managed runtime lists
every client and server whose override names it, and says when the launcher
preference does, comparing canonical paths. The removal remains the user's to
confirm.

**The launcher preference stays a pin.** It applies to every host without an
override of its own and is labeled as the launcher's, never as the host's. A
host leaves it by pinning a runtime of its own. There is no per-host
"automatic despite the launcher preference" setting.

**Failures name the runtime.** A launch or start that is refused before the
JVM runs reports the runtime it tried, how it was chosen, what the host
expects, and the mismatch or probe failure. A JVM that exits because a class
needs a newer Java is reported with the class and the version it needs, read
from a client's early output or the end of a server run's capture. A server's
failure names the runtime given for that start or else the one its settings
select now, and only when that runtime's Java version is consistent with the
message.

## Consequences

- The picker cannot offer a runtime the launch refuses, or refuse one the
  launch accepts, for the same inventory.
- A host's status probes a pinned path that the inventory does not hold, so
  frontends request it off their UI thread. Probe results are not persisted.
- Removing a managed runtime can leave pins that name nothing. They show as
  problems with a fix rather than being cleared silently.
- Hosts that rely on the launcher preference change together when it changes,
  as before.
