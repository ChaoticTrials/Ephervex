# ADR 0009 - Session-only login is a second credential store, not a fallback

**Status:** Accepted

## Context

ADR 0004 requires three things when Secret Service is unavailable: say so
clearly, offer session-only login, and never silently fall back to plaintext.
The first and third are easy to state. The second one needs a decision, because
"the user is logged in but nothing is saved" has to be represented somewhere,
and the obvious implementations are all bad:

- **A boolean on the provider** (`persist: bool`) puts an `if` around every
  store, load, and delete. The one that eventually gets written wrong is the
  delete, and the failure mode is a credential left in the keyring after logout.
- **An `Option<Keyring>`** turns every credential operation into a
  three-way match at the call site, and pushes "what do we do when there is no
  store" into six places instead of one.
- **A file-backed store used "only when the keyring is missing"** is precisely
  what ADR 0004 forbids, and it would be one `else` branch away from being the
  default on any machine with a locked keyring.

There is also a second consumer of the same question. The test suite must not
require a working Secret Service - CI has none - so something has to stand in
for the keyring there too.

## Decision

`CredentialStore` is a trait with exactly **two** implementations:

| Implementation | Where the secret goes | `CredentialStorage` |
|---|---|---|
| `KeyringStore` | Secret Service, under the `ephervex` service name | `Keyring` |
| `MemoryStore` | a map that dies with the process | `SessionOnly` |

Session-only login is `MemoryStore`. It is not a degraded mode of the keyring
store, not a flag, and not an absence - it is the same interface backed by
somewhere else, so every operation above it is written once.

Three consequences follow deliberately:

1. **There is no third implementation, and adding one is a visible act.** A
   file-backed store would be a new type in `store.rs` that somebody has to
   write and defend in review, rather than an `else` branch nobody notices.
2. **Degrading requires consent.** `LoginOptions::allow_session_only` defaults
   to `false`. A machine with no Secret Service gets
   `AuthError::SecretStorageUnavailable`, whose `session_only_would_help()`
   is what tells a frontend to offer the alternative. The user is told before
   it happens, not after.
3. **A session-only login writes nothing to `config.toml`.** `App::adopt`
   records account metadata only when `session.storage.is_persistent()`.
   Otherwise the next run would read an active account whose credential does
   not exist and report a login that is not there.

The test suite uses `MemoryStore` throughout. That is not a shortcut: it means
the degraded path is exercised by every test run, rather than only by the users
who happen to have no keyring.

## Consequences

- `Session::storage` crosses the whole stack up to `ephervex account status`,
  so "this sign-in will be forgotten when Ephervex closes" is something the
  interface can actually say.
- `MemoryStore` also makes a second account trivial to test, since
  `CredentialKey::MicrosoftRefreshToken(Uuid)` is keyed per account.
- The cost is one extra indirection on every credential operation. That is the
  price of the guarantee being structural rather than remembered.
