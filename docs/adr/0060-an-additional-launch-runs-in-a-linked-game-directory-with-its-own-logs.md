# ADR 0060 - An additional launch runs in a linked game directory with its own logs

**Status:** Accepted

## Context

A second client started with `ephervex launch --alongside` shares the
instance's saves, options and configs. Minecraft's logging rolls
`logs/latest.log` away when a client starts, so two clients sharing `logs/`
destroy each other's log, and a crash stack trace cannot be told apart from the
other client's lines.

Mods and tools read `<game directory>/logs/latest.log` (a dump or report
feature, for example), so redirecting only the log files with a launch-specific
log4j configuration would break them. Not every loader takes its logging
configuration from the release's metadata either, and running the game in a
different working directory changes the paths a mod resolves.

## Decision

**An additional launch runs in a launcher-owned directory outside the
instance**, `<state>/logs/clients/<id>/game-<handle>`, which is both the game
directory it is told about and the process's working directory. Each top-level
entry of the instance's game directory appears there as a link to the real
entry, so saves, options, configs and mods are the instance's own.

**Its `logs` entry links to `logs/launch-<handle>/` inside the instance's
`logs/`.** The game and any mod see `<game directory>/logs/latest.log`, while
the files live in the instance, where bundles, storage accounting and the
folder action already look. The first launch is unchanged.

**A log listing names those files `launch-<handle>/<file>`.** Resolving such a
name accepts exactly that shape, a real folder (never a link) inside `logs/`
named for a valid launch handle, and one regular file inside it. Every other
containment rule of ADR 0016 is unchanged.

**The directory holds no state and is rebuilt for each launch.** Folders every
launch uses (`saves`, `screenshots`, `resourcepacks`, `crash-reports`,
`config`) are created in the instance first so the game does not create them in
the launch's own directory. A top-level entry the game creates there anyway
stays with the launch until the instance has an entry of that name; the next
launch then moves the launch's copy into `game-<handle>.set-aside`, replacing an
earlier set-aside entry of the same name, and links the instance's entry. The
instance's entry wins.

## Consequences

- Two launches of one instance each keep a complete game log that survives the
  other's startup.
- A file a game replaces by renaming over a linked top-level file (rather than
  writing it in place), such as `servers.dat`, becomes that launch's own copy
  for the rest of that run.
- Everything that reads a client's current log names the launch it reads:
  crash analysis, the live log view and the running list read
  `launch-<handle>/latest.log` for an additional launch, and the history
  records which launch an event came from.
- Crash reports stay in the instance's one `crash-reports/` folder, and nothing
  inside a report says which launch wrote it. A report is attributed to the
  launch whose captured output names it ("Crash report saved to:"). Output is
  captured only for launches a runner hosts, so a report no launch's output
  names is reviewed with the first launch's log, and a launch whose output
  names no report is given the newest unnamed one written since it started.
- Linking requires a Unix platform.
