# ADR 0023 — AppImage is the Linux release artifact, on a declared glibc baseline

Status: Accepted

## Context

Ephervex is a Linux-only application (ADR 0008) that had no distributable form:
a desktop entry and a set of icons, but nothing a user could download and run.

Its shape narrows the choice more than it might first appear. It is a developer
tool for reproducing bugs, so the people who need it most are the ones already
dealing with a broken environment; asking them to add a repository, trust a
signing key, or wait for a distribution to package a young project is asking
them to solve a second problem first. It launches Minecraft, so it spawns Java
child processes, reads and writes directories the user chooses, opens sockets on
arbitrary ports, and manages a keyring credential. And it ships two binaries,
`ephervex` and `ephervex-gui`, that share one configuration and one data root.

## Decision

### One artifact: an AppImage

A single executable file the user marks executable and runs. No installer, no
package manager, no root, nothing written outside the XDG roots the application
already owns (ADR 0003), and uninstalling is deleting the file.

Exactly one artifact is produced. A second format is not added for coverage;
each one is a thing to build, verify, and be wrong about.

**Flatpak is out.** Its sandbox is the reason. Ephervex exists to launch other
processes against directories the user names, and to link a client to a server
it also launched. Making that work under portals would mean either a set of
permission holes wide enough to make the sandbox decorative, or a product that
cannot do the thing it is for.

**No `.deb`.** The repository has no Debian packaging infrastructure, and a
package that only ever installs two binaries and a desktop entry buys nothing an
AppImage does not already give, at the cost of a second distribution surface to
maintain and a per-distribution matrix to test.

> Superseded by [ADR 0026](0026-a-debian-package-alongside-the-appimage.md).
> The basename dispatch described below is what made this wrong: it means the
> command-line interface exists only when the artifact is reached under the name
> `ephervex`, which an ordinary symlink onto `PATH` does not do. Everything else
> in this ADR still holds, and applies to that package as well.

### Two binaries, dispatched by the name the artifact was invoked under

`AppRun` reads the basename of `$ARGV0`: `ephervex` runs the command-line
interface, anything else runs the desktop application. That reuses the names the
project already gives its binaries, and an existing AppImage convention, rather
than inventing an Ephervex-specific argument that shell completions and the CLI
documentation would then both be wrong about.

> **Amended: the name selects the default, and arguments select the command
> line.** Dispatching on the name alone was right about the default and wrong
> about everything else. The usual way to put something on `PATH` is
> `ln -s <artifact> ~/.local/bin/`, which keeps the artifact's own file name, so
> the overwhelmingly common link opened a launcher window and silently discarded
> the whole command line. Only a link named exactly `ephervex` worked, which is
> not what anybody types.
>
> An invocation under any other name now runs the command-line interface when it
> was given arguments, and the desktop application when it was not. The AppImage
> runtime consumes its own `--appimage-*` options before `AppRun` sees them, and
> a double-click and the desktop entry both pass none, so the default is
> unchanged and nothing that worked before stops working.
>
> The one exception is a launcher link. A scheme handler runs the artifact with
> an `ephervex://` argument, and that argument belongs to the desktop
> application (ADR 0027), so a single argument naming that scheme goes to the
> window rather than to the command line.
>
> The desktop application also refuses arguments it cannot act on instead of
> ignoring them, so this cannot regress into silence again from the other side.

> **Amended: `AppRun` no longer puts the artifact's `usr/bin` on `PATH`.** For a
> mounted artifact that is a `/tmp/.mount_*` path which ceases to exist the
> moment the launcher exits, and it was inherited by every process the launcher
> started, Minecraft included. Nothing needed it: both binaries are executed by
> absolute path, and nothing in the workspace runs either of them by bare name.

### A declared glibc baseline, enforced against the artifact

The supported minimum is **glibc 2.35**, recorded in `packaging/linux-baseline`.
It covers Ubuntu 22.04 LTS and Linux Mint 21, and Debian 12's 2.36 and every
later release clear it.

The important half is that this is a decision rather than an observation.
Building on a newer machine silently raises the floor for everyone, and nobody
finds out until a user on an older distribution reports a loader error nobody
can reproduce. So `build-appimage.sh` reads the highest `GLIBC_` symbol version
the built binaries actually require and refuses to package anything above the
declared baseline, naming the symbols responsible. That check, rather than the
choice of build environment, is what holds the floor: a build machine or runner
image too new to reach it fails here instead of silently regressing. There is
deliberately no flag to skip it.

The check covers every ELF that ships, the supplied type-2 runtime included.
That one is prepended to the artifact and is the first thing the kernel loads,
so leaving it out would allow a package that clears the floor and still cannot
start on the distribution the floor names. A runtime that requires no glibc
symbol versions at all is statically linked and passes; one that requires them
is held to the baseline like anything else.

Changing the baseline is a change to that file, to this ADR, and to the
compatibility Ephervex claims. It is not a side effect of upgrading a laptop.

### No bundled libraries

Nothing beyond the two binaries and their data is packaged. The desktop
application dynamically links fontconfig, freetype, expat, zlib, bzip2, libpng,
and brotli, and dlopens X11, xkbcommon, and EGL.

The baseline is what makes that correct: each of those is part of the baseline
distribution's own desktop stack, so a machine that meets the floor has them.
Bundling the graphics and input libraries specifically would make things worse,
not better. The GPU driver, the input method, and the font configuration all
belong to the host, and a bundled copy that disagrees with the host's driver
fails on exactly the machines that work today.

If a baseline-built binary is ever found incompatible with a much newer host,
fontconfig and freetype are the two to revisit first, because they are the ones
whose behavior is configuration-driven rather than driver-driven.

Because nothing is bundled, the set of libraries the binaries need is a promise
about the user's machine, and it is declared in
`packaging/linux-shared-libraries` for the same reason the glibc floor is
declared. `build-appimage.sh` reads the direct `DT_NEEDED` entries of each
shipped binary and refuses one that is not on the list. A crate change that
brings in a new dynamic dependency is then a build failure with a name, rather
than a startup failure on the machines of users the release notes told to expect
nothing new.

### `appimagetool` and the type-2 runtime are host dependencies

Neither is downloaded, fetched, vendored, or bootstrapped by the packaging
script. A missing one is an error naming what to install.

This keeps packaging free of a network step that decides what ends up in the
artifact. The runtime in particular is an executable prepended to every
AppImage: left to itself `appimagetool` downloads a fresh one, which both
requires the network and lets the launcher change between two builds of the same
source. Supplied through `$APPIMAGE_RUNTIME_FILE`, it is pinned like any other
input.

Provisioning them is the caller's job, and `packaging/host-tools` is what a
caller provisions from: the address each tool comes from and the SHA-256 the
procedure was verified against. A maintainer checks what they installed against
it; the automated release fetches exactly what it names and refuses anything
whose digest differs. Either way the choice of what ends up in the artifact is
made in a reviewed file rather than by whatever a tool happens to fetch while
packaging.

## Consequences

- A user on glibc older than 2.35 is unsupported and gets a clear failure to
  start rather than a subtle one later.
- Releases must be built in the pinned environment. Building on a developer
  machine produces a package that fails its own gate, which is the intent.
- The CLI is reached through a symlink named `ephervex`, which is a step users
  have to be told about; `docs/release.md` tells them.
- Desktop integration is whatever the user's AppImage integration does with the
  embedded desktop entry and icons. Nothing is installed into
  `~/.local/share/applications` by Ephervex itself.
- Two builds of the same source in the same environment produce the same bytes.
  This is a **reproducible release procedure**, not a claim of bit-for-bit
  reproducibility across environments, which has not been verified.
