# ADR 0005 - Provider abstractions for loaders, mod platforms, and accounts

**Status:** Accepted

## Context

Three areas of this launcher are certain to change underneath us:

- **Mod loaders.** Vanilla, Forge, and NeoForge ship first; Fabric and Quilt are
  explicitly out of scope for v1 but explicitly wanted later.
- **Mod platforms.** Modrinth and CurseForge have different APIs, different
  dependency semantics, and different distribution rules.
- **Authentication.** The Microsoft/Xbox/Minecraft Services token chain has
  changed before and is not documented as one stable public specification.

Hardcoding any of these into the launch pipeline means the next change is a
rewrite instead of an addition.

## Decision

Each gets a trait boundary, and the core depends on the trait rather than on a
concrete implementation:

- `LoaderProvider` - version discovery, client install, server install, Java
  validation.
- Mod platform provider - search, project metadata, versions, dependency
  mapping into one shared dependency graph.
- `AccountProvider` - the whole Minecraft-specific authentication chain behind
  one interface.

Provider-specific metadata stays in provider-specific structures. It does not
leak into the common model.

## Consequences

- Adding Fabric later is a new `LoaderProvider` implementation plus a
  `LoaderKind` variant - not a change to the launch pipeline.
- Each provider is independently testable against fixtures, which is what lets
  CI avoid live services entirely.
- The traits must not be shaped around one provider's quirks. Concretely:
  `Stable` and `Latest` are separate concepts because Forge's "Recommended" and
  NeoForge's "not beta" are different things, and neither may be silently
  substituted for the other.
- Trait methods that hit the network are async and cancellable, so a hung
  provider cannot wedge the UI.
