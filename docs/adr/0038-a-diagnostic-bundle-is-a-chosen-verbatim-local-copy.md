# ADR 0038 - A diagnostic bundle is a chosen, verbatim, local copy

**Status:** Accepted (builds on ADR 0016)

## Context

A bug report usually needs files from several places: the game's newest log, a
crash report, the recipe that says what the host is supposed to contain, and
the host's own record of which Minecraft, loader, and Java it uses. Collecting
them by hand means finding launcher-owned directories and knowing which files
matter, so Ephervex offers to collect them into one archive.

Two easy designs are wrong for a tool built around reproducing bugs.

A bundle that "sanitizes" its contents would rewrite logs and crash reports
with patterns meant to catch secrets. Patterns cannot know what is sensitive in
text a game and its mods wrote, so they miss things while giving the impression
that nothing is left, and they remove or mangle the lines somebody needed.
ADR 0016 already shows diagnostic files verbatim for the same reason.

A bundle that takes a fixed set of files would be too much for one report and
too little for another, and would carry files the user never looked at into a
place they may share publicly.

The files also have different owners. Logs and crash reports are written by the
game, its mods, or a server, and can contain anything they chose to print:
server addresses, player names, chat, file paths, or a token a mod logged.
Ephervex does not own that text. What Ephervex does own is its own
configuration, its credential storage, and its own log.

## Decision

A diagnostic bundle is a chosen, verbatim, local copy.

**Chosen.** The core planner lists what one scope (a client, a server, or the
launcher itself) could contribute, with each part's kind and size. A bundle
contains only parts named in the selection passed to the export, and every
frontend builds its choice from that list rather than from paths of its own.
The desktop starts from the newest game log, the newest crash report, and the
host's recorded contents and record, and refuses an empty choice. The command
line takes everything on offer unless parts are named with `--include`, and
`--list` shows what that is before anything is written.

**Verbatim.** A chosen file is copied byte for byte. Nothing is redacted,
reformatted, or truncated, so the archive shows what the user's disk showed.

**Local.** The archive is written to a path the user named. Ephervex never
uploads it, never integrates a paste service or sharing API, and contacts no
network while planning or exporting a bundle. Where it goes afterwards is the
user's decision.

**What Ephervex does not contribute.** The planner offers only files from the
scope's diagnostic directories (ADR 0016) and, for a client, its retained
`recipe.json` and `instance.json`. It never offers `config.toml`, account or
credential files, the system keyring, CurseForge API keys, access or refresh
tokens, or cookies, and a selection naming anything else is reported as not
readable rather than resolved to a path. `config.toml` is excluded as a whole
instead of being filtered: it is the one file whose job is to hold settings,
credential-shaped keys collect there, and a partial copy would be a second,
weaker secret filter. Ephervex's own log is written under the rule that events
never carry tokens, keys, or authorization headers, and launch commands are
redacted before they reach any log, so the files Ephervex wrote contain no
secret of its own.

**Manifest.** Every archive carries a plain-text `bundle.txt` naming the
Ephervex version, the scope, the parts included, the parts offered and left
out by choice, and the parts chosen that could not be read, such as a log that
rotated away between choosing and writing. A bundle never looks more complete
than it is.

**Limits the user is told about.** Game logs, crash reports, and host records
can contain whatever the user, the game, or a mod put in them, including JVM
arguments the user typed. The desktop says so beside the checklist, and the
user decides what to include.

## Consequences

- A bundle is exactly as useful as the files it copies, and a crash report in a
  bundle is the same crash report the Logs surface shows.
- Ephervex's guarantee is narrow and checkable: it adds no secret of its own.
  It does not promise that a game log is safe to publish.
- Launcher logs are a separate scope. A host's bundle carries that host's files;
  the launcher's log is bundled on its own.
- A frontend cannot widen a bundle past what the planner offers, so a new file
  kind is added in the planner, where this decision applies to it.
