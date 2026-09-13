# ADR 0026 - A Debian package alongside the AppImage

**Status:** Accepted

Supersedes the "No `.deb`" decision in
[ADR 0023](0023-appimage-is-the-linux-release-artifact.md). Everything else in
that ADR stands: the glibc baseline, the shared-library allowlist, the refusal
to bundle, and the pinning of host tools all apply to this artifact too.

## Context

ADR 0023 chose one artifact and said so plainly: "a package that only ever
installs two binaries and a desktop entry buys nothing an AppImage does not
already give, at the cost of a second distribution surface to maintain and a
per-distribution matrix to test."

The first half of that turned out to be wrong, and for a reason the ADR could
not have known at the time. An AppImage ships one file and dispatches its two
binaries on the name it was invoked under, so the command-line interface only
exists when the file is reached under the name `ephervex`. The obvious way to
put a CLI on `PATH` is `ln -s <file> ~/.local/bin/`, which keeps the source
basename, so the link resolves to the desktop application and silently discards
the whole command line. Every documented workaround is a second manual step
that a user has to know about and get right.

A `.deb` has no such problem. `/usr/bin/ephervex` and `/usr/bin/ephervex-gui`
are two real files at two real paths, on `PATH` already, with a desktop entry
the system menu picks up and an uninstall that removes exactly what was
installed. That is not a second way to do the same thing; it is the thing the
AppImage cannot do.

The second half of the objection was about cost, and it is still right to take
seriously. What makes the cost acceptable now is that the expensive parts are
already built and already shared: the glibc floor, the shared-library
allowlist, the build flags, and the path checks all live in one sourced file
that both packaging scripts use.

## Decision

Ephervex publishes two Linux artifacts: the AppImage, unchanged, and one
Debian package.

**One binary package containing both binaries.** Splitting `ephervex` and
`ephervex-gui` into separate packages would make a user choose before they know
which they want, and the two are a few megabytes that are built together,
versioned together, and released together. A split is a decision to make when
something actually differs between them, not in advance.

**The package is built by hand, not by a converter.** `packaging/build-deb.sh`
lays out the tree and calls `dpkg-deb`, the same way `build-appimage.sh`
assembles an AppDir: an explicit file-by-file allowlist, so nothing walks the
working tree and no glob can sweep `.git`, fixtures, or developer caches into
the payload. A tool that generates packaging from crate metadata would be a
third thing to pin, and would move decisions about what ships out of a reviewed
file.

**Dependencies are computed from the binaries, not written down twice.**
`dpkg-shlibdeps` reads what the built binaries actually need and resolves it to
package relationships for the baseline distribution. The allowlist in
`packaging/linux-shared-libraries` still decides what is *allowed* to be needed;
this decides how to spell it for a package manager. Two hand-maintained lists of
the same fact would disagree eventually, and the one users feel is the one that
makes installation fail.

**Same baseline, same enforcement, same refusal to bundle.** The package is held
to the glibc floor and the shared-library allowlist exactly as the AppImage is,
by the same code. A `.deb` that cleared a lower bar than the AppImage would make
the baseline a property of the format rather than of the release.

**The architecture is `amd64` only**, because `packaging/linux-baseline` names
one supported release architecture. When that changes, this changes with it
rather than ahead of it.

## Consequences

- The command-line interface works after an ordinary install, with no symlink
  and no instructions. That is the problem this ADR exists to solve.
- Desktop integration stops being a manual step: the desktop entry and the icons
  land where the system already looks.
- There is a second artifact to smoke test, and it has its own test rather than
  sharing the AppImage's. The two verify different things: one checks a
  self-contained file that must not read anything outside itself, the other
  checks a tree of installed paths and the control metadata a package manager
  reads.
- Uninstalling is now `apt remove` as well as deleting a file. Neither removes
  the XDG directories the application owns (ADR 0003), which stay the user's.
- The AppImage remains the artifact for everyone not on a Debian derivative, and
  nothing about it changes.
