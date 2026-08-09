# Ephervex

**Spawn. Reproduce. Despawn.**

Ephervex is a Linux-first Minecraft launcher and developer tool for disposable
bug-reproduction environments. It creates temporary or persistent instances,
preserves exact game and loader selections, and keeps downloaded artifacts in
shared verified stores.

## Why Ephervex

Reproducing a modded Minecraft bug means recreating someone else's environment
exactly, using it briefly, and retaining the logs and crash reports long enough
to diagnose it. General-purpose launchers optimize for durable installations;
Ephervex optimizes for controlled, disposable reproductions.

- **Temporary by default.** New and imported instances use the operating
  system's temporary directory unless a persistent location is requested.
- **Exact selections.** Minecraft, loader, and pack versions are never silently
  upgraded, downgraded, or approximated.
- **Shared verified artifacts.** Game JARs, libraries, assets, loader files, and
  managed runtimes are cached once and verified before publication.
- **Untrusted imports stay untrusted.** Archive paths, download addresses, and
  executable customizations are validated or refused rather than trusted.

## Current capabilities

The CLI currently supports:

- XDG-based configuration, instance storage, reconciliation, duplication, and
  folder inspection;
- Vanilla installation and launching for release versions from Minecraft
  1.12.2 onward;
- Forge and NeoForge loader resolution and installation;
- local Java discovery, compatibility checks, and managed OpenJDK installation;
- Microsoft and offline accounts, including multiple-account selection and
  session-only login when secure credential storage is unavailable;
- global Minecraft preferences applied through version-aware `options.txt`
  adapters; and
- Modrinth `.mrpack` analysis and import with optional-file selection, layered
  overrides, secure extraction, and verified downloads; and
- exact Vanilla, Forge, and NeoForge dedicated-server installation, typed
  properties and network settings, explicit global EULA consent, foreground
  supervision, console control, conservative recovery, and bounded diagnostic
  capture; and
- log and crash-report browsing for client instances, servers, and Ephervex
  itself, with newest-first crash reports, summaries, bounded reading, search,
  copy-friendly paths, and folder actions.

## Current limitations

- The Slint GUI does not provide a functional interface; use the `ephervex` CLI.
- CurseForge and Prism pack import are not available.
- Microsoft sign-in requires a suitable Azure application registration and
  Microsoft approval for the Minecraft Services API. Offline accounts do not
  require either.
- Ephervex targets Linux, release versions of Minecraft 1.12.2 and newer, and
  Vanilla, Forge, and NeoForge. Fabric, Quilt, snapshots, Windows, and macOS are
  outside the current supported scope.
- Logs and crash reports are inspected locally. Ephervex does not upload them
  and integrates no paste service.
- Dedicated servers are supervised in the foreground. Abrupt supervisor
  termination can leave an orphaned Java process; Ephervex reports verified
  identity and requires explicit operating-system termination rather than
  adopting or signaling that process.

## Building and testing

Use a stable Rust toolchain:

```bash
cargo build --workspace
cargo test --workspace
cargo run -p ephervex-cli -- --help
```

The ordinary test suite is offline and uses fixtures or loopback HTTP servers.
Production-service smoke tests are opt-in.

## Documentation

- [`docs/architecture.md`](docs/architecture.md) describes crate ownership,
  dependency direction, workflows, and security boundaries.
- [`docs/adr/`](docs/adr/) contains architecture decision records.
