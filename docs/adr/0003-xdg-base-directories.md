# ADR 0003 — XDG base directories

**Status:** Accepted

## Context

Linux is the only supported platform for the first complete version. Users
expect an application to put configuration, data, cache, and state where the XDG
Base Directory Specification says, and to respect the environment variables that
override those locations.

Separately, the product's defining feature is that instances are temporary by
default, which needs a root the OS is willing to clean up.

## Decision

Resolve all four roots through the `directories` crate, keyed on the single
`APP_ID` constant in `ephervex-model`:

| Purpose | Location |
|---|---|
| Configuration | `$XDG_CONFIG_HOME/ephervex/config.toml` |
| Persistent data | `$XDG_DATA_HOME/ephervex/` (`instances/`, `servers/`, `java/`, `shared/`, `icons/`) |
| Cache | `$XDG_CACHE_HOME/ephervex/` (`downloads/`, `metadata/`, `assets/`, `libraries/`, `installer-cache/`) |
| State | `$XDG_STATE_HOME/ephervex/` (`launcher.sqlite`, `logs/`) |
| Temporary instances | `std::env::temp_dir()/ephervex/instances/<uuid>/` |

The temporary root comes from `std::env::temp_dir()`, never from a hardcoded
`/tmp` — that respects `TMPDIR` and keeps the code honest about what it depends
on.

The persistent instance root is user-overridable in settings; the other roots
are not.

## Consequences

- `APP_ID` is referenced exactly once per root. Renaming the application is a
  one-line change plus a migration decision, not a search-and-replace.
- Cache is disposable by definition: deleting `$XDG_CACHE_HOME/ephervex/` must
  never break an installed instance beyond forcing re-downloads.
- Anything under the cache root is content-addressed and hash-verified, which is
  what makes sharing assets and libraries across instances safe.
- No secret is ever written to any of these locations (see ADR 0004).
