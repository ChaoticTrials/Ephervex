# ADR 0018 - Apache-2.0 project license

**Status:** Accepted

**Supersedes:** ADR 0007

## Context

Repository licensing must be explicit and consistent with workspace package
metadata. The repository includes the Apache License 2.0 text in `LICENSE`, and
every workspace package inherits `license = "Apache-2.0"`.

Third-party dependencies retain their own terms. In particular, the Slint
distribution terms identified by ADR 0001 still require review before public
distribution.

## Decision

Ephervex-authored code in this repository is licensed under Apache-2.0. The
workspace remains `publish = false`; crate publication and release packaging
are separate decisions.

## Consequences

- The tracked `LICENSE` file and Cargo package metadata must continue to agree.
- Dependency licenses and distribution terms remain part of release review.
- This decision does not publish crates, create packages, or authorize a
  release.
