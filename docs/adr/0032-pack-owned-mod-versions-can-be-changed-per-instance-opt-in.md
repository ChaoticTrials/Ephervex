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

**A per-instance setting, started from the instance's own source.**
`ClientRecord::allow_pack_mod_version_changes` follows the same shape as the
existing `java_override`/`jvm_override` precedent: a plain field on the
instance record, changed through `App::set_allow_pack_mod_version_changes`,
refused while the instance is running for the same reason the runtime
overrides are. An imported instance starts with it off, which keeps ADR 0020's
refusal exactly as it was; turning it on is a deliberate, per-instance
decision, never a global one, because "the pack's declaration is a historical
record" is not a rule every instance built from the same pack necessarily
wants relaxed at once. An instance created in the launcher starts with it on,
because it declared nothing to preserve: nothing but the person who made it
ever chose its mods, so there is no record for the setting to protect. The
serde default stays `false` so a document written before the setting existed
keeps the conservative reading.

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

**Moving a pack mod and taking one away are the same claim.** The setting
widens `can_change_version`, `can_update` and `can_remove` together, through
`effective_mutability`. An instance that is no longer preserving the pack's
declaration has no reason to keep a mod its user is done with, and a list that
offered to move a mod but never to end it would be preserving half a record.
`can_delete_file` is not part of this, because it is never restricted in the
first place: taking a jar off disk resolves nothing and retires nothing.

Which of the two a frontend's bin performs is the frontend's decision, read
from the same flag: an instance preserving a declaration offers the reversible
put-aside, and one whose mods are the user's offers the removal, so the bin
means one thing per instance rather than one thing per origin. Putting a jar
aside to find which mod crashes stays available either way, as turning the mod
off.

Both the mutation-planning paths and the read-only display paths call
`effective_mutability`, so a row never offers an action the operation behind it
would refuse. `ManagedModSnapshot` carries the instance's answer for them,
which is why no frontend has to read the instance record to decide policy.

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
  `PackDeclared` is "no/no/no" only while an instance is preserving the pack's
  declaration, at which point it reads "yes/yes/yes" for an instance that is
  not. The refusal ADR 0020 documented as permanent for the general case,
  absent this setting, is unchanged.
- A `PackOverride` entry - a jar the pack shipped inside itself - keeps its
  refusal to be removed whatever this setting says. That refusal is mechanical
  rather than a matter of ownership: the pack's own override layer would place
  the file again, and the list would be wrong until it did.
- Extending this to linked-server propagation is explicitly out of scope here
  and would need its own decision about which version a linked server should
  follow when its client has deviated from the pack.
