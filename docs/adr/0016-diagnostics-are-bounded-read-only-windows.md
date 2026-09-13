# 0016 - Diagnostics are bounded, read-only windows over launcher-owned roots

Status: Accepted

## Context

Logs and crash reports are the reason this product exists: a user reproducing a
bug needs the game's `latest.log`, its `debug.log`, the newest crash report, the
server's equivalents, and the launcher's own record of what it did - without
navigating the filesystem to find any of them.

Every one of those files is untrusted and hostile to naive handling:

- A crash loop produces a `debug.log` of arbitrary size. Reading one whole is an
  out-of-memory kill on the machine the user is trying to debug with.
- The files are written while they are read. Minecraft truncates `latest.log`
  when it starts, rotates it on a schedule, and appends to it continuously.
- Mods write whatever they like into them, including bytes that are not UTF-8.
- The crash-report header is a convention, not a specification. Old versions,
  forks, and mods' own handlers all deviate from it.
- The directories are ordinary directories. A file inside an instance can be a
  symbolic link to anywhere on the machine.

A frontend also needs to open these files, and the frontend must not construct
filesystem paths - the layout belongs to `ephervex-storage` (ADR 0002, ADR 0003)
and a GUI that joined `"minecraft/logs"` itself would have to be found and
changed the day that layout moves.

## Decision

Diagnostic files are exposed as **bounded, read-only windows addressed by name
within a launcher-owned directory**.

1. **Bounded reads.** Every read takes a byte budget and is clamped to a hard
   ceiling. A window is a window; there is no call that loads a file whole. A
   tail window starts at the first line boundary inside it so it never begins
   mid-line, and an incomplete character at a window's far edge is dropped
   rather than reported as invalid data.

2. **Name-addressed, not path-addressed.** A listing yields entry *names*. Every
   subsequent operation - open, refresh, search, copy path, open containing
   folder - takes a scope (client instance, server, or the launcher) plus a
   name. The name must be a single path component; the entry must be a regular
   file; a symbolic link is refused rather than followed; and the directory
   itself must still resolve inside the root that owns it. Containment is
   re-checked on every call, because the file may have been replaced between
   two of them.

3. **Nothing is interpreted or rewritten.** Text is returned as text. No line is
   executed, no path found inside a log is opened, and content is never
   normalized - a crash report that has been "cleaned up" has lost the detail
   somebody needed. Search operates on the text already read and never touches
   the file.

4. **Summaries are best-effort and never gate access.** A crash report's
   recognized headers are extracted where present, and an excerpt of its first
   useful lines is always available. A report whose format is unfamiliar is
   listed, opened, and searched exactly like any other.

5. **Deterministic ordering.** Entries are newest first by modification time,
   with the file name descending as the tie-break. Whole-second filesystem
   timestamps make ties ordinary rather than exotic, and Minecraft's own names
   lead with a timestamp, so the tie-break agrees with the primary key.

6. **The launcher keeps its own log, and it is one of these files.** Tracing
   events are appended to a plain-text file per day under the state root,
   bounded by size and by a retained-file count, and browsed through the same
   surface as everything else. Long-running operations carry an operation id in
   their span, so the id shown to a user locates their own run in that file.

7. **Uploading is out of scope.** Nothing here sends a log anywhere. Copying a
   path or text is the user's action, in their own tools.

## Consequences

A frontend can build a complete log browser - list, open, refresh, search, copy
text, copy path, open folder - without naming a directory, and the same code
serves clients, servers, and the launcher itself.

Reading a rotating file is inherently a sample: a window taken at one moment
does not become wrong when the file changes, it becomes old, and refreshing is
the same call again. Callers are told what a window covered (`file_bytes`,
`returned_bytes`, `truncated`, `lossy`) rather than being given a value that
silently means several different things.

Compressed rotated logs (`*.log.gz`) are not listed. They are not inspectable as
text, and decompressing untrusted archive data to browse a log would open a
security surface this feature has no reason to open.

The launcher's log file is a second place events land, and it inherits the
redaction rule that already governs `tracing` (ADR 0004): a crate that must not
log a credential still must not, and a log file browsed through this surface
must never gain contents that stderr would not have shown.

The dedicated server's captured process output stays where ADR 0014 put it. It
is a live console history keyed to a supervised process, not a file the server
wrote, and merging the two would make one of them worse.
