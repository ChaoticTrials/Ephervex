# ADR 0010 — Several accounts, two kinds, one default

**Status:** Accepted

## Context

The account model needs both Microsoft and offline identities, and an
installation may contain several accounts. Microsoft authentication depends on
external application approval for the Minecraft Services API; offline accounts
must remain usable without that approval or any network access.

## Decision

### Account kind is an enum, not a flag

`AccountKind` is `Microsoft | Offline`, lives in `ephervex-model` because
`ephervex-storage` and `ephervex-auth` both need it and cannot see each other,
and is `#[non_exhaustive]`.

More importantly, a *session* carries `Credentials`, which is also an enum:
`Microsoft { token, expires_at, entitlements, storage }` or `Offline`. The
rejected alternative was an `Option<SecretString>` token on a flat `Session`.
That fails for a specific reason: it makes "no token" mean two different things
— *offline* and *expired* — and the launch pipeline must never confuse them. One
of them wants a refresh, the other wants a different command line.

Because `Credentials` is an enum, the launch pipeline must handle both arms.
`Session::access_token()` returns `Option<&SecretString>` and
`Session::is_authenticated()` reads the credentials rather than the account's
label, so a session assembled with mismatched halves cannot claim more than it
holds.

### Offline UUIDs are derived, never invented

```text
UUID.nameUUIDFromBytes(("OfflinePlayer:" + name).getBytes(UTF_8))
```

MD5, stamped version 3 / RFC 4122. This is what a server running
`online-mode=false` computes for itself. A linked client and test server must
therefore use the same derivation.

Inventing a UUID would make a client and its own linked server disagree about
who the player is, and the failure would be **silent**: no error, just an
inventory, position, advancement set, and op status belonging to a different
identity than the world already holds. In a tool built for reproducing bugs,
that corrupts the reproduction rather than failing it.

The trap worth naming: this is *not* RFC 4122's namespaced version 3, which
prepends a namespace UUID's bytes. A UUID library's `new_v3` helper produces a
well-formed, deterministic, version-3, and entirely wrong answer.
`offline.rs` pins published name/UUID pairs and asserts inequality against every
standard namespace.

### Offline accounts have no secret — they do not get a laxer store

ADR 0004 is unchanged and unweakened. An offline account has no credential of
any kind, so secret-storage policy has nothing to persist. Concretely:

- no keyring entry is created, read, or deleted for one;
- `account status`, `account list`, and removal all work on a machine with no
  Secret Service at all;
- the interface says "No credential needed", never "not saved", because the two
  are different states and conflating them would read as a broken keyring.

This is a stronger position than a relaxed store, not a weaker one.

### One default, with rules that never leave a launcher unable to launch

- The **first** account added becomes the default; with one account there is
  nothing to choose, and leaving it unset would mean a launcher that has an
  account and refuses to use it.
- Adding another does **not** steal the default. Signing in to a second account
  to check something must not silently change what the next launch uses.
  `account use` is the only thing that changes it, and it is always explicit.
- Removing the default **promotes the most recently used survivor**, tie-broken
  on UUID for determinism. That is the account the user was working with before
  this one. Leaving no default would break the next launch for someone who still
  has a perfectly good account.
- Removing the last account leaves none, which is the honest answer.

### Removal deletes one credential, and cannot orphan one

`CredentialKey::MicrosoftRefreshToken(Uuid)` is per-account, so removing one
account cannot reach another's entry.

The **ordering** is the guarantee: the keyring entry goes first, and a failure
there aborts the whole operation. The reverse order would let a failed deletion
leave a secret in the keyring with nothing in `config.toml` recording that it
exists — an orphan nobody could find to remove later. This way the worst case is
an account still listed, which can simply be removed again.

## Consequences

- `logout` becomes "remove the default account", which is what it means once
  there can be more than one.
- An account row without `kind` deserializes as `Microsoft`, preserving the
  meaning of the original schema without a database migration.
- Two accounts may legitimately share a display name (one Microsoft, one
  offline), so name lookup reports ambiguity rather than guessing; removing the
  wrong account is not recoverable.
- An offline account cannot be renamed. Its identity *is* the hash of its name,
  so a rename is a different player; `add-offline` refuses a duplicate name
  rather than silently returning the existing account.
- Offline launch remains available while Microsoft authentication is blocked
  on approval.
