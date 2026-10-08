# ADR 0030 - Ephervex ships its own CurseForge key as a fallback

**Status:** Accepted

Amends [ADR 0022](0022-curseforge-uses-a-user-supplied-key-and-persists-nothing.md).
Everything else there stands: nothing the API returns is persisted, the
credential is scoped to the origins it was issued for, and a user's own key
always takes precedence. What changes is only which key is used when the user
has not configured one.

## Context

ADR 0022 rejected "an application-owned key embedded in the binary" as an
alternative to a user-supplied key, for two reasons: CurseForge revokes a key,
not a user, so a shared key compromised by one bad actor breaks CurseForge
features for everyone at once; and a key baked into a public release is
trivially extracted from the binary. At the time, no CurseForge API key
existed that was issued to Ephervex specifically - the only key available
would have been one issued for a different project, and shipping that would
have put this project's traffic under another project's grant and rate
limits, which is a separate problem this launcher does not create for itself
(see the equivalent rule for the Microsoft client ID, `DEFAULT_MICROSOFT_CLIENT_ID`
in `crates/auth/src/lib.rs`).

CurseForge has since issued a key to Ephervex specifically. The extraction and
shared-revocation risks ADR 0022 named have not gone away, but they are now a
choice about Ephervex's own key rather than a reason to borrow someone else's,
and the project accepts them for the convenience of CurseForge features
working without a first-run detour through settings.

## Decision

**A build-time default key is used only when the user has not configured
one.** `ephervex_auth::curseforge_api_key` falls back to
`DEFAULT_CURSEFORGE_API_KEY` (`crates/auth/src/chain.rs`) exactly when the
credential store holds nothing, which includes a machine with no secret
storage at all; a key saved through settings always wins, unchanged from
ADR 0022. A store that exists but cannot be read is still reported as
unreadable, since it may hold a user key.

**The default is never a literal in source.** Unlike a client ID, an API key
is a real bearer secret, so it is picked up only from the
`EPHERVEX_CURSEFORGE_API_KEY` build-time environment variable
(`option_env!`), never typed into a tracked file. A build that leaves the
variable unset - every local development build - compiles an empty default,
which means exactly what ADR 0022 already meant: no built-in key, CurseForge
features wait on the user's own.

**The release workflows set the variable from a repository secret.** Both
`.github/workflows/release.yml` and `.forgejo/workflows/release.yml` export
`EPHERVEX_CURSEFORGE_API_KEY` from a secret before the step that builds the
release binaries, so only the published AppImage and Debian package carry a
key - never a developer's own unrelated build, and never the repository or
its history.

## Consequences

- CurseForge features work out of the box on an official release, without a
  first-run trip through settings, at the cost of everyone on that release
  sharing one key until they configure their own.
- If the built-in key is abused or revoked, every user of the affected
  release loses CurseForge features until a new release ships or they
  configure their own key - the exact failure mode ADR 0022 declined to
  accept for a borrowed key, now accepted for Ephervex's own.
- A private or forked build that does not set the CI secret ships with no
  built-in key, identical to ADR 0022's behavior.
- Nothing about persistence, scoping, or per-artifact credential selection
  changes; ADR 0022's rules there still govern every CurseForge request.
