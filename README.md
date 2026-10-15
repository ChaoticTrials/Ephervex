# Ephervex

**Spawn. Reproduce. Despawn.**

Ephervex is a Linux-first Minecraft launcher and developer tool for disposable
bug-reproduction environments. It creates temporary or persistent instances,
preserves exact game and loader selections, and keeps downloaded artifacts in
shared verified stores.

## Why Ephervex

Reproducing a modded Minecraft bug means recreating someone else's environment
exactly, using it briefly, and retaining enough context to diagnose what went
wrong. General-purpose launchers optimize for durable installations; Ephervex
optimizes for controlled, disposable reproductions.

* **Temporary by default.** New and imported instances use the operating
  system's temporary directory unless persistent storage is requested.
* **Exact selections.** Minecraft, loader, mod, and pack versions are never
  silently upgraded, downgraded, or approximated.
* **Shared verified artifacts.** Game JARs, libraries, assets, loader files,
  mods, and managed runtimes are cached and verified before publication. The
  reusable download cache has a configurable byte budget, 10 GiB by default, and
  clearing it only costs a re-download.
* **Untrusted imports stay untrusted.** Archive paths, download addresses, and
  executable customizations are validated or refused rather than trusted.
* **Reproduction environments can include servers.** A client instance can have
  a linked dedicated server with matching installation data, direct connection,
  contextual lifecycle controls, and retained diagnostics.

## Name and mascot

The name **Ephervex** combines *ephemeral* with *Vex*.

"Ephemeral" describes the disposable instances at the heart of the launcher:
spawn an environment, reproduce the issue, then discard it.

The mascot and logo are deliberately somewhere between a Vex and an Allay. In
practice, the Allay genes clearly won. It keeps the supernatural Minecraft
connection without making the application look quite as hostile as the thing
it is named after.

## Current capabilities

Ephervex provides both a Slint desktop application and a command-line
interface backed by the same GUI-independent application core.

Current functionality includes:

* XDG-based configuration, storage, reconciliation, duplication, and folder
  inspection;
* temporary and persistent client instances;
* Vanilla installation and direct launching for release versions from
  Minecraft 1.12.2 onward;
* exact Forge and NeoForge loader resolution and installation;
* local Java discovery, compatibility checks, per-instance overrides, and
  managed OpenJDK installation;
* a per-host view of the Java runtime it runs with (path, version, vendor,
  architecture, and how it was chosen) next to what its Minecraft version and
  loader expect, a runtime picker that marks compatible choices, and runtime
  changes that are previewed before they are applied;
* Microsoft and offline accounts with multiple-account selection and secure
  refresh-token storage where available;
* application settings for interface text size, technical diagnostics text,
  provider configuration, storage locations, Java behavior, and server
  defaults;
* Minecraft preferences applied through version-aware `options.txt`
  compatibility data without writing untouched defaults;
* Modrinth `.mrpack` analysis and import with optional-file selection, layered
  overrides, secure extraction, verified downloads, and retained source
  recipes;
* Modrinth mod browsing, compatibility-aware dependency resolution, and managed
  mod installation;
* dependency-aware mod enable/disable controls, local safety snapshots, and a
  recoverable mod isolation workflow for narrowing reproducing configurations;
* exact Vanilla, Forge, and NeoForge dedicated-server installation;
* standalone and client-linked dedicated servers with typed properties,
  explicit EULA handling, lifecycle control, console commands, and durable
  runtime state;
* automatic server-port selection with explicit custom-port support;
* configurable default operator setup with authentication-mode-aware Minecraft
  identities;
* direct **Launch Client & Connect** workflows for linked client/server
  environments;
* contextual live client logs and server console output with severity parsing,
  filtering, search, line wrapping, bounded history, and follow controls;
* crash-report and launcher-diagnostics browsing with bounded local reads and
  folder actions;
* per-host Diagnostics that separates problems from valid customizations:
  incompatible or missing Java, turned-off or missing required mods with the
  chain that needs them, dependencies the loader refused, recent crashes, and
  missing or damaged recorded files; safe fixes are previewed before they are
  applied, a failed run gets a quick likely-cause notice, and a redacted
  technical report can be copied for bug reports;
* local diagnostic bundles built from a checklist of the host's logs, crash
  reports, and recorded state, copied verbatim and never uploaded;
* instance search, combinable filters, deterministic ordering, favorites,
  groups and custom icons, kept apart from instance files and available in the
  desktop interface and on the command line;
* desktop management of instances, servers, accounts, Java runtimes, mods,
  Minecraft preferences, diagnostics, and application settings; and
* shell completion generation for Bash, Zsh, Fish, Elvish, and PowerShell; and
* a local health report (`ephervex doctor`) covering storage locations,
  writability, credential-store availability, bundled data, cache, and Java.

## Modpacks and mods

Modrinth is currently the supported modpack import provider.

Ephervex retains a provider-neutral pack recipe after import so later workflows,
including linked-server reconstruction, do not depend on the original archive
still being present.

CurseForge modpack exports import into that same recipe. A CurseForge manifest
names its files by project and file id, so importing one needs the official
third-party API and an API key you apply for yourself; Ephervex stores it in the
system keyring and never in its configuration. Reading what an export *says* -
its name, its exact Minecraft version and loader build, how many files it has -
needs no key at all. CurseForge mod browsing is planned but not yet
implemented. See [`docs/curseforge-api-key.md`](docs/curseforge-api-key.md).

Managed mod workflows support Modrinth browsing, dependency resolution,
installation, dependency-aware state changes, version selection, and durable
mod isolation. See [`docs/mod-isolation.md`](docs/mod-isolation.md) for the
desktop and command-line isolation workflow. Broader update workflows are still
in development.

## Dedicated servers

Servers may be created independently or linked to a client reproduction
environment.

Linked servers are primarily managed from their client workspace and are hidden
from the global Servers list by default. They can optionally be shown globally
through application settings.

New servers can select a free local port automatically, beginning with the
standard Minecraft server port, or preserve an explicitly configured custom
port.

Ephervex can also retain operator setup intent for newly created development
servers. Online-mode servers use authenticated Microsoft identities, while
offline-mode servers use Minecraft's canonical name-derived offline identity.

## Live diagnostics

Client and server diagnostics use a shared typed live-log model.

The desktop viewer supports:

* TRACE, DEBUG, INFO, WARN, ERROR, FATAL, and unclassified output;
* multiline exception and stack-trace grouping;
* severity filtering;
* text search;
* optional line wrapping;
* explicit follow and jump-to-latest behavior;
* bounded retained history with truncation indication; and
* selectable, copyable local output.

Client instances incrementally follow `logs/latest.log`. Server output is
captured through Ephervex's supervised server runtime and presented without
transport-level stdout/stderr prefixes leaking into Minecraft log text.

Logs and crash reports remain local. Ephervex does not upload them or integrate
a paste service.

## CLI completions

Shell completion scripts are generated from the same canonical Clap command
model used by the CLI itself.

For example:

```bash
ephervex completions bash
ephervex completions zsh
ephervex completions fish
```

Completion scripts are written to standard output. Ephervex does not modify
shell configuration files or install completion scripts automatically.

## Current limitations

* CurseForge mod browsing is not yet available. CurseForge pack import needs a
  configured API key, and cannot obtain a file whose author has turned off
  third-party distribution.
* Complete pack and mod version-management workflows are still in development.
* The generated Minecraft preference metadata is not yet exposed through the
  complete desktop preference interface.
* `x86_64` and `aarch64` are released, on a glibc 2.35 baseline. Distributions
  older than Ubuntu 22.04 or Debian 12 are unsupported.
* Linux ARM client launches require compatible LWJGL natives in the selected
  version's metadata. Versions that select x86 natives, including Vanilla
  1.12.2, 1.20.1, and 1.21.1, are refused before installation. Ephervex does not
  substitute native downloads or change the selected versions.
* The Debian and RPM packages both reach the ChaoticTrials package repository,
  so an installation from it upgrades through the package manager. A package
  installed from a downloaded file has no repository to upgrade from.
* Running the AppImage needs FUSE. Where it is unavailable,
  `APPIMAGE_EXTRACT_AND_RUN=1` starts it without mounting.
* Native text selection in the virtualized live diagnostics viewer is currently
  limited to one rendered record or chunk at a time. The complete retained
  output can still be copied through the viewer.
* Microsoft sign-in depends on an Azure application that is permitted to use
  the Minecraft Services API. Offline accounts do not require Microsoft
  authentication.
* Ephervex currently targets Linux, release versions of Minecraft 1.12.2 and
  newer, and Vanilla, Forge, and NeoForge.
* Fabric, Quilt, snapshots, pre-releases, release candidates, Windows, and
  macOS are outside the current supported scope.

## Installing

Ephervex is distributed for Linux on `x86_64` and `aarch64`, as an AppImage that
installs nothing and as Debian and RPM packages. Every file names the
architecture it is for; take the one matching your `uname -m`.

On Debian, Ubuntu, Linux Mint and derivatives:

```bash
sudo apt install ./ephervex_<version>_amd64.deb
```

On Fedora, openSUSE, RHEL and derivatives:

```bash
sudo dnf install ./ephervex-<version>-1.x86_64.rpm
```

To receive upgrades through the package manager instead, add the ChaoticTrials
repository once. See [`docs/release.md`](docs/release.md#installing).

Either puts `ephervex` and `ephervex-gui` on `PATH` and the desktop entry in the
system menu. Everywhere else, the AppImage runs as it is:

```bash
chmod +x Ephervex-x86_64.AppImage
./Ephervex-x86_64.AppImage
```

That starts the desktop application. The same artifact carries the command-line
interface, reached by invoking it under that name:

```bash
ln -s "$PWD/Ephervex-x86_64.AppImage" ~/.local/bin/ephervex
ephervex --help
ephervex doctor
```

The supported baseline is glibc 2.35, which covers Ubuntu 22.04 LTS, Linux Mint
21, Debian 12, and anything newer, on both architectures. Like every AppImage, the artifact mounts
itself through FUSE, which needs `/dev/fuse` and a `fusermount` helper; where
that is unavailable,
`APPIMAGE_EXTRACT_AND_RUN=1 ./Ephervex-x86_64.AppImage` runs it
without mounting. Nothing is installed and nothing runs as root; uninstalling is
deleting the file, plus the XDG directories if you want your instances gone too.

`ephervex doctor` reports whether this machine is set up correctly: the resolved
directories and whether they are writable, whether the credential store can be
reached, whether the bundled Minecraft data is intact, and what Java it found.
It needs no network, and its exit status is the verdict, so a script can gate on
it without parsing anything.

See [`docs/release.md`](docs/release.md) for storage locations, upgrade and
uninstall behavior, offline expectations, and how a release is built.

## Building and testing

Use a stable Rust toolchain:

```bash
cargo build --workspace
cargo test --workspace
cargo run -p ephervex-cli -- --help
cargo run -p ephervex-gui
```

The ordinary test suite is offline and uses fixtures or local loopback services
where external behavior must be exercised. Production-service smoke tests are
opt-in.

Before committing changes, the repository is expected to remain clean under:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Documentation

* [`THIRD-PARTY-LICENSES.md`](THIRD-PARTY-LICENSES.md) carries the full terms
  of every open-source package Ephervex is built on, generated from the
  dependency graph. `ephervex licenses` shows the same list, and so does the
  desktop application's About & Licenses screen.
* [`docs/architecture.md`](docs/architecture.md) describes crate ownership,
  dependency direction, workflows, and security boundaries.
* [`docs/release.md`](docs/release.md) covers supported Linux scope, storage
  locations, upgrade and uninstall behavior, offline expectations, and the
  release build procedure. `./prepare_release.sh <version>` performs that
  preparation and stops before anything is published.
* [`docs/curseforge-api-key.md`](docs/curseforge-api-key.md) explains why
  CurseForge access needs an API key, how to apply for one, and where Ephervex
  keeps it.
* [`docs/adr/`](docs/adr/) contains architecture decision records.

## Status

Ephervex is under active development.

The core launcher, desktop application, direct launching, Modrinth workflows,
dedicated servers, linked reproduction environments, local diagnostics, and
Linux packaging are functional. Remaining work is focused on broader provider
support, richer pack and mod controls, and complete metadata-driven Minecraft
preferences.
