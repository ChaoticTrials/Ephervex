# ADR 0031 - A project declared twice resolves to one file

**Status:** Accepted

Amends [`CurseForgeFileId`](../../crates/model/src/mods.rs)'s and
`ephervex-pack`'s CurseForge manifest reader's prior rule that a manifest
naming one project under two different file ids is refused outright. ADR 0022's
"exact identity" rules are otherwise unchanged: a lookup is still `project P,
file F` against the endpoint that answers about exactly that file, nothing
lists a project's versions, and a file the API cannot find still fails the
import.

## Context

A real CurseForge export can declare the same project twice with two
different file ids - `Client.Garden.of.Glass.-.Questbook.Edition-v4.3.4.zip`'s
manifest lists Just Enough Items as both `fileID 3157864` and
`fileID 3327383`. This is not a hand-edited manifest; CurseApp produces it
when a pack was updated to a newer Minecraft release without the export
trimming a mod entry the update carried forward under a new file. Refusing
the whole import over one leftover entry in an otherwise well-formed,
CurseForge-authored manifest is a worse outcome than picking between the two.

The two ids are indistinguishable to a human reading the manifest - both are
just numbers - so "reject and ask the user to hand-edit `manifest.json`" is
not a workable answer either. Only CurseForge's own file metadata says
anything a person, or this launcher, can judge them by.

## Decision

**Both ids parse.** `pending_files` (`crates/pack/src/curseforge.rs`) no
longer refuses two different files declared for one project; each becomes its
own `PendingProviderFile`, exactly like two different projects would.
Everything else it already refused - a malformed id, one file declared both
required and optional - still is.

**The resolver picks one, and shows it before the user commits.** Once both
candidates are resolved against the API (`resolve_duplicate_projects` in
`crates/core/src/curseforge_pack.rs`), the file whose own `gameVersions`
names the pack's declared Minecraft version wins; when none, more than one,
or exactly one candidate matches equally, the more recently published file
wins. The loser is not installed, and not silently dropped either: a
`DuplicateProviderFile` (`crates/pack/src/recipe.rs`) names both files - by
their real, resolved names, never the raw ids a manifest entry is until the
API answers - and why the winner won. This is preview information, not
instance metadata: `PackAnalysis::duplicate_files` reaches the import modal
through `ImportPresentation` the same way `blocked` files already do, so the
choice is visible *before* the user presses Import, and it is discarded once
the import completes rather than following the instance around afterward.

A file whose author forbids third-party downloads takes part in the choice like
any other. Only the winner follows the restricted-file path (resolved from the
shared cache, or listed as a manual download); a restricted loser is never
installed and never requested by hand.

**A pinned single file id is never second-guessed.** This is narrowly about a
manifest declaring the same project twice. A recipe entry, an ordinary
managed-mod update, or any other single `project P, file F` reference still
resolves to exactly that file or fails; nothing here starts treating a file
id as a hint rather than an exact reference.

## Consequences

- A manifest that reproduces the field's most common real-world defect
  imports cleanly instead of failing outright.
- The kept-versus-discarded choice is shown in the import modal before the
  user commits, and nowhere after: it is not an `AttentionReason`, and does
  not appear on the instance once it exists. A decision the launcher made
  automatically is disclosed at the moment it is made, not filed as an
  open issue on something the user never asked about.
- `PackError::PackEntryConflict` and its message id are removed: nothing
  constructs it anymore.
- If neither candidate's `gameVersions` names the pack's Minecraft version,
  the choice falls back to publish date, which is a heuristic rather than a
  declared fact - the same trade-off ADR 0030 already accepted for shipping a
  build-time default credential: a reasonable default over a hard failure,
  visibly reported rather than silently assumed.
