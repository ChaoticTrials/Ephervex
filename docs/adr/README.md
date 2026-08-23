# Architecture Decision Records

One file per decision, numbered and immutable once accepted. A decision that
turns out wrong gets a new ADR that supersedes the old one — the original stays
so the reasoning trail survives.

| ADR | Decision | Status |
|---|---|---|
| [0001](0001-slint-for-the-gui.md) | Slint for the GUI | Accepted |
| [0002](0002-sqlite-index-with-per-instance-metadata.md) | SQLite index with per-instance metadata | Accepted |
| [0003](0003-xdg-base-directories.md) | XDG base directories | Accepted |
| [0004](0004-keyring-for-secret-storage.md) | Keyring for secret storage, no plaintext fallback | Accepted |
| [0005](0005-provider-abstractions.md) | Provider abstractions for loaders, mods, and accounts | Accepted |
| [0006](0006-temporary-instance-lifecycle.md) | The OS owns temporary-instance cleanup | Accepted |
| [0007](0007-licensing-and-prism-reference.md) | Licensing deferred; Prism is behavioral reference only | Superseded by ADR 0018 |
| [0008](0008-first-version-scope.md) | Features excluded from the first complete version | Accepted |
| [0009](0009-session-only-login.md) | Session-only login is a second credential store, not a fallback | Accepted |
| [0010](0010-account-model.md) | Several accounts, two kinds, one default | Accepted |
| [0011](0011-shared-launch-stores.md) | Everything a launch can re-download is launcher-level, including natives | Accepted |
| [0012](0012-provider-neutral-pack-recipe.md) | Provider-neutral recipes are the pack import boundary | Accepted |
| [0013](0013-managed-mods-are-recipe-entries.md) | Mods installed through a provider are recipe entries | Accepted |
| [0014](0014-dedicated-server-authority-and-supervision.md) | Dedicated server authority and conservative supervision | Accepted |
| [0015](0015-linked-client-direct-connect.md) | Linked client direct connection uses canonical lifecycle services | Accepted |
| [0016](0016-diagnostics-are-bounded-read-only-windows.md) | Diagnostics are bounded, read-only windows over launcher-owned roots | Accepted |
| [0017](0017-options-compatibility-is-generated-data.md) | options.txt compatibility is generated data, distributed separately from the application | Accepted |
| [0018](0018-apache-2.0-project-license.md) | Apache-2.0 project license | Accepted |
| [0019](0019-one-mod-list-with-typed-ownership-and-identity.md) | An instance has one mod list, with ownership and identity as separate typed facts | Accepted |
| [0020](0020-mod-mutation-is-one-undoable-transaction.md) | Mod mutation is one undoable transaction, and ownership decides what may be mutated | Accepted |
| [0021](0021-linked-mod-sync-pins-the-root-and-resolves-per-target.md) | Linked mod synchronization pins the root release and resolves dependencies per target | Accepted |
| [0022](0022-curseforge-uses-a-user-supplied-key-and-persists-nothing.md) | CurseForge uses a user-supplied API key, and its responses are never persisted | Accepted |
| [0023](0023-appimage-is-the-linux-release-artifact.md) | AppImage is the Linux release artifact, on a declared glibc baseline | Accepted |
