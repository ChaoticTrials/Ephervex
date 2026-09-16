# ADR 0032 - Pack-owned mod versions can be changed, per instance and opt-in

**Status:** Accepted

## Context

ADR 0020 refused every mutation of a `PackDeclared` mod, because a recipe
entry recorded exactly one version and overwriting it would make the launcher
claim the pack shipped a release it never shipped. That ADR named the missing
piece explicitly: representing a deviation honestly needs a second version per
entry and an answer to which one a reconstructed server gets, and it named
itself as what such a feature would supersede.

Users bisecting a crash routinely want to try a different build of a mod the
pack declares - that is exactly the kind of reproduction work this launcher
exists for - without losing the ability to say afterward "put this back to
what the pack shipped."

## Decision

**A per-instance opt-in, off by default.** `ClientRecord::allow_pack_mod_version_changes`
follows the same shape as the existing `java_override`/`jvm_override`
precedent: a plain field on the instance record, changed through
`App::set_allow_pack_mod_version_changes`, refused while the instance is
running for the same reason the runtime overrides are. Leaving it off keeps
ADR 0020's refusal exactly as it was; turning it on is a deliberate,
per-instance decision, never a global one, because "the pack's declaration is
a historical record" is not a rule every instance built from the same pack
necessarily wants relaxed at once.

**The second version ADR 0020 asked for.** `PackFileRecipe::original_identity`
(bumping `RECIPE_VERSION` to 4) records the pack's originally-declared release
the first time a `PackDeclared` entry's identity is about to change, and is
never overwritten after that. A recipe that has never had a pack-owned mod
mutated has an absent `original_identity`, which is exactly correct: the
entry's own `identity` still *is* the pack's version. This answers ADR 0020's
open question about which version a reconstructed server gets: the server is
still built from whatever `identity` currently holds, and `original_identity`
exists purely so "Reset to Pack's Version" can undo a deviation, not so a
server build has to choose between two versions.

**Update and change-version only; removal stays refused.** The setting widens
exactly the two permissions ADR 0020's table already names as the interesting
ones for a pack-owned mod (`can_change_version`, `can_update`), through a
small `effective_mutability` wrapper used only in the mutation-planning
codepaths. Every read-only display path continues to call `ModMutability::of`
directly and is unaffected. `can_remove` is never loosened by this setting:
taking a pack mod out of the instance's desired state is a different decision
from moving its version.

**Client-only for now.** Linked-server propagation of a mod-version change
already refuses a target whose entry is pack-owned
(`LinkedTargetBlock::PackOwned` in `crates/core/src/linked_mods.rs`). This ADR
leaves that guard exactly as it is. A pack-mod version change made on the
client under this setting does not currently propagate to a linked server;
the server keeps whatever the recipe's `identity` says, which is the client's
current (possibly deviated) version the next time a server is built or
reconstructed from it, unless the client mod is reset first.

## Consequences

- A server materialized from the recipe after this setting was used gets the
  *current* file, which may no longer match what the original pack shipped,
  unless "Reset to Pack's Version" is used first. This is the same trade-off
  ADR 0020 already accepted for a `UserSelected` mod; it now also applies to a
  `PackDeclared` one that has opted in.
- A pack export or re-share carries `original_identity` along unchanged: it is
  purely informational, never consulted by install or reconstruction logic on
  its own, so there is no format hazard in keeping it.
- ADR 0020's mutability table gains a caveat rather than a rewritten row:
  `PackDeclared` is "no/no/no" only until an instance opts in, at which point
  it reads "yes/yes/no" for that instance. The refusal ADR 0020 documented as
  permanent for the general case, absent this setting, is unchanged.
- Extending this to linked-server propagation is explicitly out of scope here
  and would need its own decision about which version a linked server should
  follow when its client has deviated from the pack.
