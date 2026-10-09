# ADR 0044 - RPM packages are published to the signed package repository

**Status:** Accepted (amends the "no RPM repository" decision of ADR 0026)

## Context

ADR 0026 published only the Debian package to a repository and left the RPM as a
downloadable release artifact, because "publishing one means a signing key, a
metadata layout, and a hosting commitment that nothing has asked for yet".

All three now exist. The ChaoticTrials package repository is signed with one
key, is hosted by GitHub Pages at a stable address, and lays its packages out by
format: `deb` for apt and `rpm` for dnf. What an RPM user lacks is what ADR 0026
gave a Debian user, upgrades through their package manager instead of a page
visit and a manual download.

## Decision

**The RPM is published to the `rpm` repository, for both architectures.** The
GitHub release workflow publishes both formats from one job and one commit, so
the repository never holds one at the new release and the other at the old one.
The job takes the packages from the release rather than rebuilding them, and
refuses a release that lacks either architecture.

**Packages and metadata are signed.** The package repository's own
`scripts/publish-rpm.sh` signs each package and the repository metadata with the
key the apt repository already uses, and verifies the result against the
published public key before anything is committed. Package signatures are what
`dnf` checks with `gpgcheck`; the metadata signature covers the package list
itself (`repo_gpgcheck`). The `.rpm` built here stays unsigned: `build-rpm.sh`
signs nothing, and the signature is added to the copy in the repository.

**The repository's package is not byte for byte the release's.** The Debian
package in the apt repository is, because apt signs the repository rather than
the file. The RPM in the repository differs from the release artifact by its
signature alone, and the release page's `.sha256` describes the unsigned file.

**Publishing stays one workflow's job.** The Forgejo workflow still builds the
same artifacts for its own release and has no path into either repository and no
credentials for them.

## Consequences

- A Fedora or RHEL-family user can add the repository once and receive
  Ephervex upgrades with `dnf upgrade`. The release page's file remains for
  anyone who does not want a repository.
- The package repository keeps every published `.rpm`: its metadata is built
  from the files present, so removing one removes that version from the
  repository.
- The release job pushes to a git repository shared with other projects. A push
  by another project at the same moment is rejected and the job is re-run.
