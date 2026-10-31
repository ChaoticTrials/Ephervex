# Ephervex

**Spawn. Reproduce. Despawn.**

Ephervex is a Linux-first Minecraft launcher and developer tool for disposable
bug-reproduction environments. It creates temporary or persistent instances,
preserves exact game and loader selections, and keeps downloaded artifacts in
shared verified stores.

User guides, an interface tour with screenshots, and a CLI reference are on the
documentation site: <https://ephervex.chaotictrials.de>.

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
  mods, and managed runtimes are verified before they are published to shared
  stores. The reusable download cache (verified downloads and loader installer
  files) has a configurable byte budget, 10 GiB by default, and clearing it only
  costs a re-download.
* **Untrusted imports stay untrusted.** Archive paths and download addresses
  are validated and refused when unsafe. Executable customizations from
  imported packs, such as pre-launch commands, are never run.
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

### Instances and the desktop interface

* XDG-based configuration, storage, reconciliation, duplication, and folder
  inspection;
* temporary and persistent client instances;
* Vanilla installation and direct launching for release versions from
  Minecraft 1.12.2 onward;
* exact Forge and NeoForge loader resolution and installation, and switching an
  instance to another build of its loader;
* local Java discovery, compatibility checks, launcher-wide, per-instance, and
  per-server overrides, and managed OpenJDK installation;
* a per-host view of the Java runtime it runs with (path, version, vendor,
  architecture, and how it was chosen) next to what its Minecraft version and
  loader expect, a runtime picker that marks compatible choices, and runtime
  changes that are previewed before they are applied;
* a three-column desktop window with a searchable, filterable host list, a Home
  page (running hosts, hosts that need attention, recently used hosts, and an
  Activity card for operations that can be canceled), a quick view that shows
  any host over the current page, and a split view that puts two hosts side by
  side;
* instance search, combinable filters, deterministic ordering, favorites,
  groups and custom icons, kept apart from instance files and available in the
  desktop interface and on the command line;
* a Case record for each instance (issue link, reproduction steps, a note, a
  reproduced counter, and a history), kept apart from the instance files, with
  a copyable plain-text issue summary and the matching `instance issue` and
  `instance case` commands;
* local snapshots of a client instance or server with capture, restore, rename,
  and delete in the desktop interface and with `ephervex snapshot`.

### Packs and mods

* Modrinth `.mrpack` and CurseForge modpack analysis and import, from a file,
  an HTTPS address, or an `ephervex://` link with a per-host trust prompt, with
  optional-file selection, layered overrides, secure extraction, verified
  downloads, and retained source recipes;
* a Packs view and `ephervex packs` listing the resource packs, shader packs,
  and data packs an imported modpack placed, with the world that owns each data
  pack;
* mod browsing in the Modrinth and CurseForge catalogs, compatibility-aware
  dependency resolution, and managed mod installation;
* a mods table that marks where each mod came from (the pack, a dependency,
  your own addition, or a hand-placed JAR) and an Add Mods dialog that offers
  only versions that fit the instance's exact Minecraft version and loader;
* dependency-aware mod enable/disable controls, local safety snapshots, and a
  recoverable mod isolation workflow for narrowing reproducing configurations.

### Dedicated servers

* exact Vanilla, Forge, and NeoForge dedicated-server installation;
* standalone and client-linked dedicated servers with typed properties,
  explicit EULA handling, lifecycle control, console commands, and durable
  runtime state;
* automatic server-port selection with explicit custom-port support;
* configurable default operator setup with authentication-mode-aware Minecraft
  identities;
* direct **Launch Client & Connect** workflows for linked client/server
  environments.

### Diagnostics and logs

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
  reports, and recorded state, copied verbatim and never uploaded.

### Accounts, settings, and themes

* Microsoft and offline accounts with multiple-account selection and secure
  refresh-token storage where available;
* application settings for interface text size and theme, diagnostics and
  console text size, favorite Minecraft versions, provider configuration,
  storage and disk usage, Java behavior, mod-change defaults, and linked-server
  and server defaults;
* Light and Dark themes, user theme files (colors, logo, and window icon), and
  date-based seasonal themes that can be turned off;
* global Minecraft preferences applied to new and imported client instances
  through version-aware `options.txt` compatibility data, without writing
  untouched defaults.

### Command line

* command-line management of instances, servers, accounts, Java runtimes, mods,
  Minecraft preferences, and diagnostics, including `launch --detach`,
  `running`, and `logs` for launcher, client, and server logs and crash
  reports;
* shell completion generation for Bash, Zsh, Fish, Elvish, and PowerShell;
* a local health report (`ephervex doctor`) covering build and platform,
  storage locations and their writability, the configuration file,
  credential-store availability, bundled data, the artifact cache, and Java,
  with an exit status that signals a fault; it needs no network access and
  prints no credentials.

## Modpacks and mods

Ephervex imports Modrinth `.mrpack` files and CurseForge modpack exports into
one provider-neutral recipe.

Ephervex retains that recipe after import so later workflows, including
linked-server reconstruction, do not depend on the original archive still being
present.

A CurseForge manifest names its files by project and file id, so importing one
needs the official CurseForge third-party API and a key. Official releases
include a fallback key. A key you apply for yourself and configure takes
precedence; Ephervex stores it in the system keyring and never in its
configuration. Reading what an export *says* (its name, its exact Minecraft
version and loader build, how many files it has) needs no key at all. See
[`docs/curseforge-api-key.md`](docs/curseforge-api-key.md).

Managed mod workflows support Modrinth and CurseForge browsing, dependency
resolution, installation, dependency-aware state changes, version selection,
and durable mod isolation. See [`docs/mod-isolation.md`](docs/mod-isolation.md)
for the desktop and command-line isolation workflow. Broader update workflows
are still in development.

## Dedicated servers

Servers may be created independently or linked to a client reproduction
environment.

Linked servers are primarily managed from their client workspace and are hidden
from the global Servers list by default. They can optionally be shown globally
through application settings.

New servers can select a free local port automatically, beginning with the
standard Minecraft server port, or preserve an explicitly configured custom
port.

When enabled in application settings, Ephervex can make the active account an
operator on newly created servers. Online-mode servers use the account's
authenticated Microsoft identity; offline-mode servers use Minecraft's
canonical name-derived offline identity.

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
model used by the CLI itself. `ephervex completions` accepts `bash`, `zsh`,
`fish`, `elvish`, and `powershell`.

For example:

```bash
ephervex completions bash
ephervex completions zsh
ephervex completions fish
```

Completion scripts are written to standard output. Ephervex does not modify
shell configuration files or install completion scripts automatically.

## Current limitations

* CurseForge pack import needs a CurseForge API key. Official releases include
  one, and a build without it needs your own. Some CurseForge files cannot be
  downloaded by Ephervex because their authors turned off third-party
  distribution; the import pauses on them until you download them manually.
* Complete pack version-management workflows are still in development.
* Ephervex writes a Minecraft preference only for releases whose storage it
  knows. The Minecraft options coverage page lists them.
* `x86_64` and `aarch64` are released, on a glibc 2.35 baseline. Distributions
  older than Ubuntu 22.04 or Debian 12 are unsupported.
* Linux ARM client launches require compatible LWJGL natives in the selected
  version's metadata. Versions that select x86 natives, including Vanilla
  1.12.2, 1.20.1, and 1.21.1, are refused before installation. Ephervex does not
  substitute native downloads or change the selected versions. ARM launches
  have not yet been tested.
* The Debian and RPM packages both reach the ChaoticTrials package repository,
  so an installation from it upgrades through the package manager. A package
  installed from a downloaded file has no repository to upgrade from.
* Running the AppImage needs FUSE. Where it is unavailable,
  `APPIMAGE_EXTRACT_AND_RUN=1` starts it without mounting.
* The desktop needs X11 or XWayland. A Wayland-only session without XWayland is
  untested, and dropping a file onto the window also needs one of them.
* The desktop interface is currently English only.
* Microsoft sign-in depends on an Azure application that is permitted to use
  the Minecraft Services API. Offline accounts do not require Microsoft
  authentication. Persistent credential storage needs a working Secret Service
  provider such as GNOME Keyring or KWallet, and Ephervex never falls back to a
  plaintext file.
* Quitting Ephervex stops its running clients and servers.
* Avoid concurrent mod changes on one instance or server, including changes
  during an active mod isolation session.
* Ephervex currently targets Linux, release versions of Minecraft 1.12.2 and
  newer, and Vanilla, Forge, and NeoForge. Forge builds for Minecraft 1.12.2
  older than 14.23.5.2851 use an older installer format and are not supported.
* Fabric, Quilt, snapshots, pre-releases, release candidates, Windows, and
  macOS are outside the current supported scope. Prism and MultiMC exports are
  recognized but refused.

## Installing

Ephervex is distributed for Linux on `x86_64` and `aarch64`, as an AppImage that
installs nothing and as Debian and RPM packages. Every file names the
architecture it is for; take the one matching your `uname -m`. On `aarch64` the
files are `ephervex_<version>_arm64.deb`, `ephervex-<version>-1.aarch64.rpm`,
and `Ephervex-aarch64.AppImage`. The
[install page](https://ephervex.chaotictrials.de/install) lists the same
options with the matching download for your machine.

On Debian, Ubuntu, Linux Mint and derivatives:

```bash
sudo apt install ./ephervex_<version>_amd64.deb
```

On Fedora, RHEL and derivatives:

```bash
sudo dnf install ./ephervex-<version>-1.x86_64.rpm
```

On openSUSE:

```bash
sudo zypper install ./ephervex-<version>-1.x86_64.rpm
```

To receive upgrades through apt or dnf instead, add the ChaoticTrials package
repository (<https://repo.chaotictrials.de>) once. The commands are in
[`docs/release.md`](docs/release.md#installing).

Either puts `ephervex` and `ephervex-gui` on `PATH` and the desktop entry in the
system menu. Everywhere else, the AppImage runs as it is:

```bash
chmod +x Ephervex-x86_64.AppImage
./Ephervex-x86_64.AppImage
```

That starts the desktop application. The same artifact carries the command-line
interface: a link named `ephervex` always runs it, and so does any invocation
with arguments other than a single `ephervex://` link:

```bash
ln -s "$PWD/Ephervex-x86_64.AppImage" ~/.local/bin/ephervex
ephervex --help
ephervex doctor
```

The supported baseline is glibc 2.35, which covers Ubuntu 22.04 LTS, Linux Mint
21, Debian 12, and anything newer, on both architectures. Like every AppImage,
the artifact mounts itself through FUSE, which needs `/dev/fuse` and a
`fusermount` helper; where that is unavailable,
`APPIMAGE_EXTRACT_AND_RUN=1 ./Ephervex-x86_64.AppImage` runs it without
mounting. The AppImage installs nothing and nothing runs as root. To remove it,
delete the file; delete the XDG directories too if you want your instances gone,
and remove the `ephervex` keyring entries if you signed in.

`ephervex doctor` reports whether this machine is set up correctly: the resolved
directories and whether they are writable, whether the credential store can be
reached, whether the bundled Minecraft data is intact, and what Java it found.
It needs no network, and its exit status is the verdict, so a script can gate on
it without parsing anything.

See [`docs/release.md`](docs/release.md) for storage locations, upgrade and
uninstall behavior, offline expectations, and how a release is built.

## Building and testing

Use a stable Rust toolchain, version 1.91.1 or newer:

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

* The [documentation site](https://ephervex.chaotictrials.de) holds the user
  guides, an interface tour, and a generated CLI reference. Its source is in
  [`web/`](web/), and the screenshots it shows are generated from the real
  interface by [`tools/auto-screenshot-tool`](tools/auto-screenshot-tool/).
* The [Road to v1 archive](https://ephervex.chaotictrials.de/road-to-v1/) is a
  read-only copy of the issues and pull requests from before the project moved
  to GitHub. Issue and pull request numbers in older commit messages link
  there.
* The [Minecraft options coverage](https://ephervex.chaotictrials.de/minecraft-options/)
  page lists the releases and settings the compatibility dataset covers. The
  same dataset is the feed the application reads between releases.
* [`THIRD-PARTY-LICENSES.md`](THIRD-PARTY-LICENSES.md) carries the full terms
  of every open-source package and bundled font Ephervex is built on, generated
  from the dependency graph and the font manifest. `ephervex licenses` shows
  the same list, and so does the desktop application's About & Licenses screen.
* [`docs/architecture.md`](docs/architecture.md) describes crate ownership,
  dependency direction, workflows, and security boundaries.
* [`docs/release.md`](docs/release.md) covers supported Linux scope, storage
  locations, upgrade and uninstall behavior, offline expectations, and the
  release build procedure. `./prepare_release.sh <version>` runs the release
  gate, then commits and tags locally, and stops before anything is published.
* [`docs/curseforge-api-key.md`](docs/curseforge-api-key.md) explains why
  CurseForge access uses an API key, how to get your own, and where Ephervex
  keeps it.
* [`docs/themes.md`](docs/themes.md) documents the theme file format and the
  color tokens.
* [`docs/internal-format-versions.md`](docs/internal-format-versions.md) lists
  the versions and compatibility rules of Ephervex's private on-disk formats.
* [`docs/adr/`](docs/adr/) contains architecture decision records.

## Status

Ephervex is under active development.

The launcher core, desktop application, direct launching, Modrinth and
CurseForge pack import, dedicated servers, linked reproduction environments,
local diagnostics, local snapshots, themes, and Linux packages (AppImage,
Debian, and RPM) are functional. Remaining work is focused on richer pack and
mod version management and broader provider support.
