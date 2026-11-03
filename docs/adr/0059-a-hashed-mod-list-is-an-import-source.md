# ADR 0059 - A hashed mod list is an import source

**Status:** Accepted

## Context

A bug report often arrives as a crash report or a `latest.log` rather than as
a pack. The stock `Mod List:` table names each jar, its mod id, and its
version, which is not enough to find the exact file: a version string is not a
release, and a file name is not unique. LibX can replace that table with one
that also records each jar's SHA-1 and its CurseForge fingerprint (Murmur2),
and the game prints the same report into `latest.log` when it crashes.

Exact versions are the product rule (AGENTS.md): an import must not pick a
newer or nearby release when the exact one cannot be found.

## Decision

**A crash report or log with a hashed mod list imports like a pack.** It is
told from an archive by its contents. The last `Mod List:` table that has a
hash is used, together with the Minecraft version and the Forge or NeoForge
build from the System Details of the report it belongs to. A file whose table
has no hashes is refused, and so is one that does not name the game exactly.
The file itself is copied into the new instance, as `crash-reports/<name>` or
`logs/latest.log`.

**Jars are found by hash only, in the preferred catalog first.** Modrinth is
asked by SHA-1. CurseForge is asked by fingerprint, and a match counts only
when CurseForge's own SHA-1 for the file is the same. The catalog chosen as the
preferred one is asked about every hashed jar, and the other one only about
what the first did not provide. A jar that neither provides, including one a
catalog will not serve or a CurseForge file the author forbids third-party
downloads of and that no other catalog has, is left out and named on the
instance. Nothing is matched by name or version. A jar the report did not hash
(`-`, usually one nested in another jar) is not looked up.

**A dedicated server's report still builds a client.** Mods the catalog says no
client runs stay on the server side of the recipe, and the desktop offers to
create the linked server from the start.

**A pack carrying a hashed log is imported as published unless asked.** The
client layers of a pack may carry `logs/latest.log` or crash reports. The log is
preferred, then the newest crash report with a usable list, and only a list for
the pack's own Minecraft version, loader, and the client side is offered. The
desktop asks with nothing preselected; the CLI applies it with `--logged-mods`.
When applied, the instance is made to match the list: jars the pack lacks are
added from the catalogs, a pack file of a project the list names at another
version is replaced, and pack mods the list does not name are disabled, not
deleted, so they can be turned back on.

## Consequences

- An instance can be rebuilt from a report alone, and what could not be found is
  visible on it rather than guessed at.
- A rebuilt instance depends on the catalogs still serving each file. A jar
  removed from both, or never published, cannot be reproduced.
- The disabled pack mods are disabled in the client only. A server built from
  the instance's recipe later installs them again.
