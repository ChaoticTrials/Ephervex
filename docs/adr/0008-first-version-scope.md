# ADR 0008 — Features excluded from the first complete version

**Status:** Accepted

## Context

The specification lists a large required scope and an equally deliberate list of
exclusions. Recording the exclusions as a decision — rather than leaving them as
a section someone skims — is what stops them being quietly re-litigated by
short-term implementation pressure.

## Decision

The following are outside Ephervex's supported scope:

**Loaders:** Fabric, Quilt, LiteLoader, Rift.

**Minecraft versions:** snapshots, experimental snapshots, pre-releases, release
candidates, old beta/alpha, anything before 1.12.2. Release versions only.

**Platforms:** Windows, macOS, mobile.

**Features:** mod update management, modpack export, cloud sync, social
features, a launcher theme marketplace, automatic bug-report submission,
automatic public server hosting.

## Consequences

- Account kinds and multiple-account behavior are governed by ADR 0010.
- Exclusion is not the same as obstruction. The architecture provides extension
  points where they are cheap: additional loaders are new `LoaderProvider`
  implementations (ADR 0005), and the account model is shaped so a second
  account is an addition rather than a redesign.
- Excluded things must fail *clearly*, never approximately. A pack requiring
  Fabric produces an unsupported-loader error; it does not get imported with a
  guessed loader. An imported launch customization is refused or marked "needs
  attention" rather than presented as faithfully runnable.
- The 1.12.2 floor and release-only filter are enforced from Mojang's metadata
  `type` field, not from filename patterns — so the calendar-style versioning
  Minecraft has moved to does not break the filter.
- Expanding this scope requires a new or superseding ADR.
