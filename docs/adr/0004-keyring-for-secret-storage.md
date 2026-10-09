# ADR 0004 - Keyring for secret storage, with no plaintext fallback

**Status:** Accepted

## Context

The launcher holds a Microsoft refresh token and, optionally, a user-supplied
CurseForge API key. Both are long-lived credentials. Writing either to
`config.toml` would put a token that can authenticate as the user into a
world-readable dotfile, into backups, and into any bug report that includes
their configuration.

## Context-specific risk

This is a *bug reproduction* tool. Its users routinely zip up instance folders
and configuration and send them to other people. That makes plaintext credential
storage more dangerous here than in a typical launcher, not less.

## Decision

Store all secrets through the Linux Secret Service / desktop keyring, via the
`keyring` crate, under a service name derived from `APP_ID`.

Configuration may hold only non-secret metadata: account UUID, display name,
timestamps, and whether a credential exists.

When Secret Service is unavailable:

1. say so clearly,
2. offer session-only login that keeps the token in memory for the run,
3. **never** silently fall back to writing the token to disk.

## Consequences

- A headless or minimal environment without a Secret Service provider gets
  degraded but honest behavior: log in each session, or install a keyring.
- Logout must remove the stored credential, not merely forget it in memory.
- Log redaction is the responsibility of whichever crate produces the event,
  because only it knows which fields are secret. Tokens, API keys, and
  authorization headers never reach `tracing`.
- The keyring never holds a credential that no recorded account and no pending
  cleanup names. Every change to a stored credential, and the decision about
  the account record that names it, happens under one lock per configuration
  directory that holds across processes: a sign-in stores its credential and
  records its account together, a removal deletes the credential and forgets
  the account together, a resume replaces a credential only while it is still
  the one it started from, and each pending cleanup is decided against the
  current list. A sign-in first lists its account for cleanup, which recording
  the account clears, so one that stops between storing and recording leaves a
  credential that a later cleanup deletes.
- Tests must not require a keyring. The `AccountProvider` abstraction (ADR 0005)
  is what makes credential storage mockable.
