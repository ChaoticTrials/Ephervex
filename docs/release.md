# Running and releasing Ephervex on Linux

This document covers what a user needs to install, run, upgrade, and remove
Ephervex, and what a maintainer needs to build a release artifact. The design
reasoning behind the packaging choices is in
[ADR 0023](adr/0023-appimage-is-the-linux-release-artifact.md).

## Supported scope

Ephervex targets Linux only. Windows and macOS are outside the supported scope
recorded in [ADR 0008](adr/0008-first-version-scope.md).

**The baseline is glibc 2.35 on `x86_64`.** That is Ubuntu 22.04 LTS and Linux
Mint 21; Debian 12 ships 2.36 and every later release clears it. Older
distributions are not supported, and the artifact will not start on them.

The baseline is a support commitment declared in
[`packaging/linux-baseline`](../packaging/linux-baseline), not a description of
whichever machine built the release. The packaging script enforces it against
the built binaries, so a build on a newer host fails rather than quietly raising
the floor.

**FUSE is required to run the artifact.** An AppImage mounts itself, which needs
the `/dev/fuse` device and a `fusermount` helper on `PATH`. The runtime Ephervex
ships links libfuse statically, so no `libfuse2` package is involved; what it
needs is the device and the helper. Desktop installations normally have both.
Containers, hardened sandboxes, and some minimal systems do not.
[Installing](#installing) below covers what to do when it is missing.

A desktop environment providing X11 or XWayland is required for the desktop
application, along with the fontconfig that every baseline-era desktop already
installs; it brings freetype, libpng, expat, zlib, bzip2, and brotli with it.
The libraries the binaries link directly are listed in
[`packaging/linux-shared-libraries`](../packaging/linux-shared-libraries), and
the release build refuses to package anything that needs one not on that list. A
Secret Service provider (GNOME Keyring, KWallet, or equivalent) is optional; see
[Credentials](#credentials) below.

## Installing

The release artifact is a single AppImage:

```bash
chmod +x Ephervex-x86_64.AppImage
./Ephervex-x86_64.AppImage
```

That starts the desktop application. Nothing is installed, nothing runs as root,
and nothing is written outside the directories listed under
[Where Ephervex keeps things](#where-ephervex-keeps-things).

The name is the same on every release, so a download address, a symlink, and a
desktop integration all stay valid across upgrades. Which release you have is
the tag it was published under, and what `ephervex --version` reports.

### When FUSE is unavailable

If the artifact exits complaining that it cannot mount itself or cannot find a
`fusermount` binary, install your distribution's FUSE package (`fuse3` on Debian
and Ubuntu, `fuse` on Fedora) if the machine should have one, or run it without
mounting at all:

```bash
APPIMAGE_EXTRACT_AND_RUN=1 ./Ephervex-x86_64.AppImage
```

That unpacks the artifact to a temporary directory for each run. It is slower to
start and it works everywhere, including in a container.

Prefer it over extracting by hand. A manual `--appimage-extract` produces a
`squashfs-root` whose `AppRun` always starts the desktop application, because
the name the artifact was invoked under is what selects between the two
binaries and nothing supplies it there. `APPIMAGE_EXTRACT_AND_RUN=1` keeps that
name, so the command-line interface below still works.

### The command-line interface

The same artifact carries `ephervex`, the command-line interface. It is reached
by invoking the artifact under that name:

```bash
ln -s Ephervex-x86_64.AppImage ephervex
./ephervex --help
./ephervex doctor
./ephervex import some-pack.mrpack
```

Put the symlink somewhere on `PATH` to use it as an ordinary command. Shell
completions come from the CLI itself:

```bash
./ephervex completions bash
```

Ephervex does not modify shell configuration files or install completion scripts
for you.

### Desktop integration

The artifact carries a desktop entry, an AppStream component, and icons at every
size from 16x16 to 512x512. If you use an AppImage integration tool, it will
pick those up and add Ephervex to your application menu. Ephervex does not write
into `~/.local/share/applications` itself.

The desktop entry deliberately declares no MIME associations. The desktop
application takes no arguments, so a file association would open an empty
launcher on a double-clicked pack. Imports are started from inside the
application, or with `ephervex import`.

## Where Ephervex keeps things

Every location follows the XDG Base Directory Specification and honors the
environment variables that override it
([ADR 0003](adr/0003-xdg-base-directories.md)). `ephervex paths` prints the
resolved locations on your machine; `ephervex doctor` also reports whether each
one is writable.

| Purpose | Location | Holds |
|---|---|---|
| Configuration | `$XDG_CONFIG_HOME/ephervex/config.toml` | Settings only. Never a credential. |
| Data | `$XDG_DATA_HOME/ephervex/` | `instances/`, `servers/`, `java/`, `shared/`, `icons/` |
| Cache | `$XDG_CACHE_HOME/ephervex/` | `downloads/`, `metadata/`, `assets/`, `libraries/`, `natives/`, `installer-cache/`, `artwork/` |
| State | `$XDG_STATE_HOME/ephervex/` | `launcher.sqlite`, `logs/` |
| Temporary instances | `$TMPDIR/ephervex-<uid>/` | Instances the operating system is free to clean up |

Defaults are `~/.config`, `~/.local/share`, `~/.cache`, and `~/.local/state`.
The persistent instance root is the one relocatable location, changed in
settings or with `ephervex config instance-root <path>`.

The cache is disposable by definition. Deleting it costs re-downloads and
nothing else: every installed instance owns its own files.

Logs are under `$XDG_STATE_HOME/ephervex/logs/launcher/`, one file per day,
bounded by size and count. They are never uploaded and no paste service is
integrated. `EPHERVEX_LOG` raises or lowers what is recorded, for example
`EPHERVEX_LOG=ephervex_download=debug`.

## Credentials

Microsoft refresh tokens and the CurseForge API key are stored in the system
keyring through the Secret Service, never in `config.toml`
([ADR 0004](adr/0004-keyring-for-secret-storage.md)). This is the one thing
Ephervex writes outside the directories above, and it is written through the
desktop's own credential service rather than to a file Ephervex owns.

Without a Secret Service provider, Ephervex says so and offers a session-only
sign-in that keeps the token in memory for that run. It never falls back to
writing a credential to disk. `ephervex doctor` reports whether the backend is
reachable; it never reports what is stored.

## Upgrading

Replace the AppImage. Configuration, instances, servers, and cached artifacts
are untouched.

Configuration is forward-compatible in both directions:

- keys, tables, Minecraft preference ids, and account fields that a newer
  Ephervex wrote are carried through an older one's load and save cycle
  unchanged, so running an older build once does not cost you a setting;
- a catalog name this build cannot resolve is kept exactly as written rather
  than read as one this build does know;
- a configuration whose `schema_version` is newer than the running build
  understands is refused with a clear message instead of being guessed at, and
  the file is left exactly as it was;
- a value a closed setting cannot represent fails the file rather than being
  coerced to a neighboring value.

Two limits worth knowing:

- **Rewriting does not preserve formatting.** Values survive; comments, blank
  lines, quoting style, and key order do not, and keys the running build does
  not know are re-emitted after the ones it does. Any setting change rewrites
  the whole file.
- **Instance and server metadata has no unknown-field preservation.** A newer
  build's extra fields in `.launcher/instance.json` or `.launcher/server.json`
  are dropped if an older build rewrites that file. The version is checked, so a
  format an older build cannot read is refused rather than misread.

## Uninstalling

Delete the AppImage. That removes the application.

It does not remove your data. To remove that as well:

```bash
rm -rf ~/.config/ephervex ~/.local/share/ephervex ~/.cache/ephervex ~/.local/state/ephervex
```

Adjust for `XDG_*` overrides, and for a relocated persistent instance root
(`ephervex paths` prints where it is). That deletes every instance, every
server, and every world inside them.

Credentials are separate, because they are not in any of those directories. Sign
out of each account in the application before removing it, or remove the
`ephervex` entries from your keyring afterwards with your desktop's credential
manager (`seahorse` on GNOME, `kwalletmanager` on KDE) or with
`secret-tool clear service ephervex`.

Temporary instances under `$TMPDIR` are the operating system's to clean up and
disappear on reboot.

## Working offline

Ephervex is usable without a network. What still works:

- browsing, launching, and duplicating instances that are already installed;
- browsing and starting servers that are already installed;
- everything an installed instance needs, because a retained recipe carries the
  addresses, digests, and sizes of its artifacts, and the shared cache is
  checked before anything is fetched;
- editing every global setting, including Minecraft preferences;
- Minecraft preference compatibility, which falls back to the dataset compiled
  into the build when the published feed has never been fetched;
- measuring, configuring, and clearing the artifact cache;
- reading logs and crash reports;
- `ephervex doctor`.

What needs a network, and reports its absence rather than failing oddly:
signing in to Microsoft, browsing Minecraft releases and loader versions,
searching mod catalogs, resolving a CurseForge pack's files, fetching an
artifact the cache no longer holds, and refreshing a release's language
catalog. A managed language preference is not applied when the catalog is
unavailable, rather than applied on an assumption.

## Checking an installation

```bash
ephervex doctor          # human-readable
ephervex --json doctor   # machine-readable
```

It reports the build and platform, each resolved directory and whether it is
genuinely writable, whether the configuration loads, whether the credential
backend can be reached, whether the compiled-in Minecraft datasets are intact,
what the artifact cache measures, and what Java runtimes were discovered.

It makes no network request. The directory and configuration sections are
produced before any storage is opened, so a broken database or an unreadable
configuration is something it describes rather than something that stops it.
Opening storage afterwards is not read-only: it creates the XDG roots if they
are missing and may migrate the index, which is why a second run reports
directories as writable that the first run reported absent.

Writability is proven by writing a uniquely named probe file and removing it
again, because a permission bit does not know about a read-only mount or a full
disk. Credential *contents* are never part of the report in any form.

The exit status is the verdict, so a script can gate on it without parsing
anything:

```bash
ephervex doctor >/dev/null || echo "this installation needs attention"
```

It is non-zero when the storage locations do not resolve, the database cannot be
opened, the configuration cannot be read, a compiled-in dataset is damaged, or a
directory is unwritable or has something in the way. A busy artifact cache and a
failed Java scan are reported but are not faults: neither stops Ephervex from
running, and a machine with no usable Java is one Ephervex offers to fix.

## Building a release

### Host dependencies

Two tools must already be installed. `build-appimage.sh` downloads, fetches, and
bootstraps neither of them: a missing one is an error naming what to install.

Both are pinned in [`packaging/host-tools`](../packaging/host-tools), which
names the address each comes from and the SHA-256 the release procedure was
verified against: `appimagetool` from the AppImage project's
`AppImage/appimagetool` repository, the runtime (`runtime-x86_64`) from
`AppImage/type2-runtime`. Check what you install against that file before
trusting it; the automated release checks it for you and refuses a mismatch.

Upstream publishes both under rolling `continuous` tags, so a replaced asset
makes the pinned digest stop matching and stops the release. That is the intent.
Check the new binary, then update `packaging/host-tools` deliberately.

`appimagetool` ships as an AppImage itself. Where FUSE is unavailable, extract
it once with `--appimage-extract` and point `$APPIMAGETOOL` at the resulting
`squashfs-root/AppRun`.

Both are required. `build-appimage.sh` fails and names what is missing rather
than letting `appimagetool` fetch a runtime over the network, because that would
make packaging a network step and let the artifact's launcher change between two
builds of the same source. The baseline check covers the runtime as a file and
again on the finished artifact, whose own ELF header is that runtime.

Also required: a Rust toolchain, `readelf` and `strings` (binutils),
`sha256sum`, and `python3`. `desktop-file-validate` and `appstreamcli` are used
when present, and validate the desktop entry and the AppStream component;
packaging itself runs with `--no-appstream`, so `appstreamcli` is the only thing
that reads the component.

### The baseline build environment

The release build runs inside a container pinned to the baseline distribution,
which is what makes the glibc floor reachable. The packaging script never
creates, pulls, or manages a container; it is run *inside* one, exactly as it is
run with `appimagetool` already on `PATH`.

```dockerfile
FROM ubuntu:22.04
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
      build-essential curl ca-certificates pkg-config git file \
      python3 desktop-file-utils binutils zsync appstream \
      libfontconfig-dev libfreetype-dev libssl-dev \
    && rm -rf /var/lib/apt/lists/*
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
      | sh -s -- -y --default-toolchain stable --profile minimal
ENV PATH=/root/.cargo/bin:$PATH
```

### Building

```bash
docker build -f Dockerfile.baseline -t ephervex-baseline:22.04 .

# Populate a persistent registry once, so the build itself needs no network.
docker run --rm -v "$PWD:/src:ro" -v "$CARGO_CACHE:/cargo" \
  -e CARGO_HOME=/cargo ephervex-baseline:22.04 \
  bash -c 'cd /src && cargo fetch --locked'

docker run --rm --network none \
  -v "$PWD:/src:ro" \
  -v "$OUTPUT:/out" \
  -v "$TARGET:/target" \
  -v "$CARGO_CACHE:/cargo" \
  -v "$HOSTTOOLS:/hosttools:ro" \
  -e CARGO_HOME=/cargo \
  -e CARGO_TARGET_DIR=/target \
  -e CARGO_NET_OFFLINE=true \
  -e APPIMAGETOOL=/hosttools/appimagetool/AppRun \
  -e APPIMAGE_RUNTIME_FILE=/hosttools/runtime-x86_64 \
  -e SOURCE_DATE_EPOCH=0 \
  ephervex-baseline:22.04 \
  bash -c 'cd /src && packaging/build-appimage.sh /out'
```

The source tree is mounted read-only and the build output goes elsewhere, so a
release build cannot modify the repository it is built from.

`build-appimage.sh` builds the release binaries with `--locked`, so the artifact
carries the dependency versions in the committed `Cargo.lock` and not whatever a
warm registry happens to hold. It then enforces the declared glibc baseline
against every ELF that ships, including the supplied runtime; checks the direct
`DT_NEEDED` entries against
[`packaging/linux-shared-libraries`](../packaging/linux-shared-libraries);
assembles the AppDir from an explicit allowlist of files; packages it; runs the
smoke test; and prints the artifact path, its SHA-256, the glibc floor it
actually requires, and the versions of the two host tools that produced it.

### The package layout

```text
Ephervex.AppDir/
  AppRun                                            dispatches on $ARGV0
  ephervex.desktop
  ephervex.png                                      256x256
  usr/bin/ephervex                                  command-line interface
  usr/bin/ephervex-gui                              desktop application
  usr/share/applications/ephervex.desktop
  usr/share/metainfo/ephervex.metainfo.xml
  usr/share/icons/hicolor/<size>/apps/ephervex.png  8 sizes, 16x16 to 512x512
```

Everything the application reads at runtime is compiled into the binaries: the
Minecraft options compatibility dataset, the preference catalog, the Fluent
localization resources, and the interface icons. Nothing resolves back into the
source tree, which the smoke test checks by running the packaged CLI from `/`.

### Smoke testing an artifact

Run by `build-appimage.sh`, and repeatable by hand against any artifact:

```bash
packaging/smoke-test-appimage.sh Ephervex-x86_64.AppImage
```

Most of it works on an extracted copy, so it needs no FUSE and runs in the build
container. It checks the file layout; that no repository, fixture, tooling, or
configuration content shipped; that no build-machine home path is embedded; that
the glibc requirements hold; that the CLI answers `--help`, `--version`,
`info --json`, and `doctor --json` against throwaway XDG roots; that the bundled
datasets load out of the artifact; and that nothing needs the source tree.

`doctor` is run twice. The first run has to describe a clean first
installation: no configuration written, and no root reported unwritable or
unusable. Opening the application creates the roots, so the second run has to
find all of them and find every one writable. One run alone would accept a
directory the probe never reached.

Two steps need more than an extracted copy, and each says so when it skips:

- where `/dev/fuse` and a `fusermount` helper are usable, the artifact is run
  the way a user runs it, mounted and through an `ephervex` symlink, which is
  the only way to exercise the runtime and the name-based dispatch together;
- where a display is available, the desktop application is started and has to
  stay up.

A build container normally has neither, so **run the smoke test once on a
desktop host before publishing a release**, not only inside the container. The
container run alone leaves the mount path and the desktop application uncovered.

### Reproducible release procedure

Two builds of the same source in the same pinned environment, with the same
pinned runtime and `SOURCE_DATE_EPOCH`, produce a byte-identical artifact. This
has been verified for a repeat build and for a build from an empty target
directory.

That is a **reproducible release procedure**. It is not a claim of bit-for-bit
reproducibility across different environments, which has not been verified and
should not be assumed.

The generated Minecraft datasets that ship in the binaries are committed to the
repository and are never fetched during a build. Verifying that they are exactly
what the tooling regenerates needs the maintainer's untracked development cache
and is therefore an opt-in maintainer check rather than part of the release
gate:

```bash
python3 tools/minecraft-options/preferences.py --check \
  --from-analysis tools/minecraft-options/.cache/analysis
python3 tools/minecraft-options/update.py --check --offline \
  --from-analysis tools/minecraft-options/.cache/analysis/mc-<release>
```

Note that `update.py --check` takes the directory for one analyzed *release*,
not the analysis root. See
[`tools/minecraft-options/README.md`](../tools/minecraft-options/README.md).

### The release gate

```bash
python3 -m py_compile tools/minecraft-options/*.py
python3 -m unittest discover -s tools/minecraft-options

cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --no-fail-fast
cargo build --release -p ephervex-gui
cargo run -p ephervex-cli -- --help
```

then the baseline build and its smoke test, above, followed by one more run of
the smoke test on a desktop host so the mounted artifact and the desktop
application are covered too. The Rust test suite is offline: it uses fixtures
and loopback servers and contacts no production service.

### Publishing from a tag

[`.forgejo/workflows/release.yml`](../.forgejo/workflows/release.yml) runs the
procedure above on a pushed `v*` tag and publishes the result as a Forgejo
release. Announcing a release is therefore:

```bash
# with the workspace version already bumped and committed
git tag v0.1.0
git push origin v0.1.0
```

The job runs on the runner's own image, refuses to continue when the tag and the
`[workspace.package]` version disagree, runs the gate above, fetches the two
host tools and checks them against `packaging/host-tools`, then builds, packages,
and uploads `Ephervex-x86_64.AppImage` with a `.sha256` beside it. Nothing pins
that image to the baseline distribution, so the glibc floor is held by the check
`build-appimage.sh` runs against the binaries it produced: a runner too new to
reach the floor fails the release instead of publishing a package that will not
start. Publishing
needs a `RELEASE_TOKEN` repository secret: a personal access token with
repository write access, so the release is attributed to an account rather than
to Forgejo's Ghost user.

**It does not replace the desktop smoke test.** A container has neither
`/dev/fuse` nor a display, so the two steps that exercise the mounted artifact
and start the desktop application skip and say so. Download the published
artifact and run `packaging/smoke-test-appimage.sh` against it on a desktop host
before telling anyone the release is there.

## Known limitations

- Only `x86_64` is released. There is no `aarch64` artifact.
- Running the artifact needs FUSE. Where it is unavailable,
  `APPIMAGE_EXTRACT_AND_RUN=1` works at the cost of unpacking on every start.
- The glibc 2.35 baseline excludes distributions older than Ubuntu 22.04 and
  Debian 12.
- A Wayland-only session without XWayland is untested; the desktop application
  reaches X11 through the toolkit's own backend.
- Configuration rewrites do not preserve comments or key order, and instance and
  server metadata drops unknown fields on rewrite. See
  [Upgrading](#upgrading).
- The desktop application always uses the `en-US` locale. It is the only locale
  that exists, so nothing is currently unreachable, but the application does not
  yet follow the configured language the way the CLI does.
- Uninstalling does not remove keyring entries. They have to be removed through
  the desktop's own credential manager.
