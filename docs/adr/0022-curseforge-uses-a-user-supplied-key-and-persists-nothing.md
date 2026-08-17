# ADR 0022 — CurseForge uses a user-supplied API key, and its responses are never persisted

**Status:** Accepted

## Context

CurseForge is reached through its official third-party API. Two of its
properties are unlike every other source Ephervex reads.

First, the API requires a key, and a key is granted to an applicant by
CurseForge/Overwolf rather than obtained programmatically. Alternatives exist —
unofficial proxies, third-party mirrors, an application-owned key embedded in
the binary — and all of them either route a user's traffic through a party
neither the user nor the project controls, or ship a shared credential that
cannot be revoked per user and would be extracted from the first release
tarball.

Second, the published third-party API terms prohibit saving or caching data
obtained through the API. Every other metadata source in this launcher is cached
precisely so that a provider outage degrades the launcher instead of stopping
it, and the `MetadataCache` exists to make that automatic. Applying the house
pattern here would violate the terms Ephervex is using the API under.

## Decision

**The credential is the user's.** Ephervex asks the user for a CurseForge
third-party API key, stores it through the credential store of ADR 0004, and
sends it in the `x-api-key` header of each request. The launcher ships no key,
implements no proxy, and does not read the CurseForge Upload API's separate
token.

The transport obtains the key through a `CurseForgeCredentialSource`, which
yields an opaque secret and nothing else. Replacing the *source* later — with an
approved application-owned credential, say — is a second implementation of that
trait, not a change to the transport.

**Nothing the API returns is written down.** The CurseForge client uses no
`MetadataCache`, defines no cache key, has no time-to-live, and creates no
namespace under the metadata store. Responses are parsed in memory, used by the
operation that asked for them, and dropped.

**The credential is scoped to one origin.** The client will not follow a
redirect that leaves the API origin, so no response can direct the launcher to
hand its key to another host. `x-api-key` is set per request rather than
installed as a default header, so no unrelated request can inherit it.

## Consequences

- CurseForge features are unavailable until a user configures a key, and that is
  reported as an actionable setting rather than as an outage. Every other part
  of the launcher is unaffected.
- A CurseForge outage stops CurseForge work outright: there is no stale document
  to fall back on, because keeping one is what the terms forbid.
- Repeating an operation repeats its requests. The API's rate limits, not a
  local cache, are the budget.
- Verified downloaded artifacts are a separate question. A file whose hash the
  provider published is content Ephervex is entitled to keep under its ordinary
  shared-store rules; that is not API response data and is not governed by this
  decision.
- A 401 or 403 is reported as "the configured key may be invalid or no longer
  authorized" rather than as a network failure, because those are the two
  outcomes a user can act on.
