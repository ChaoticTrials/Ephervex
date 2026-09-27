# Architecture Decision Records

One file per decision, numbered and immutable once accepted. A decision that
turns out wrong gets a new ADR that supersedes the old one - the original stays
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
| [0020](0020-mod-mutation-is-one-undoable-transaction.md) | Mod mutation is one undoable transaction, and ownership decides what may be mutated | Accepted (its pack-owned refusal amended by ADR 0032) |
| [0021](0021-linked-mod-sync-pins-the-root-and-resolves-per-target.md) | Linked mod synchronization pins the root release and resolves dependencies per target | Accepted |
| [0022](0022-curseforge-uses-a-user-supplied-key-and-persists-nothing.md) | CurseForge uses a user-supplied API key, and its responses are never persisted | Accepted (its no-embedded-key decision amended by ADR 0030, its persists-nothing decision by ADR 0034) |
| [0023](0023-appimage-is-the-linux-release-artifact.md) | AppImage is the Linux release artifact, on a declared glibc baseline | Accepted (its "No `.deb`" decision superseded by ADR 0026) |
| [0024](0024-published-server-files-are-a-complete-installation.md) | Published server files are installed as a complete server, and the loader stays the launcher's | Accepted |
| [0025](0025-storage-class-is-promoted-in-place.md) | An instance becomes persistent by moving, not by being copied | Accepted |
| [0026](0026-a-debian-package-alongside-the-appimage.md) | A Debian package alongside the AppImage | Accepted |
| [0027](0027-a-pack-address-is-a-second-trust-category.md) | A pack address the user activated is a second trust category | Accepted (its scheme-registration decision amended by ADR 0028) |
| [0028](0028-the-settings-screen-does-not-switch-the-link-handler.md) | The settings screen does not switch the link handler | Superseded by ADR 0029 |
| [0029](0029-the-allowed-sites-list-carries-its-own-context.md) | The allowed-sites list carries its own context | Accepted |
| [0030](0030-ephervex-ships-its-own-curseforge-key-as-a-fallback.md) | Ephervex ships its own CurseForge key as a fallback | Accepted |
| [0031](0031-a-project-declared-twice-resolves-to-one-file.md) | A project declared twice resolves to one file | Accepted |
| [0032](0032-pack-owned-mod-versions-can-be-changed-per-instance-opt-in.md) | Pack-owned mod versions can be changed, per instance and opt-in | Accepted |
| [0033](0033-internal-format-versions-stay-at-1-until-1.0.0.md) | Internal format versions stay at 1 until 1.0.0 | Accepted |
| [0034](0034-a-recipe-entry-records-the-dependencies-its-provider-declared.md) | A recipe entry records the relations its provider declared | Accepted |
| [0035](0035-local-snapshots-use-copy-on-write-clones.md) | Local snapshots use copy-on-write clones | Accepted |
| [0036](0036-mod-isolation-applies-recoverable-dependency-closed-rounds.md) | Mod isolation applies recoverable dependency-closed rounds | Accepted |
| [0037](0037-a-declared-relation-is-resolved-against-the-catalog-and-never-recorded.md) | A declared relation is resolved against the catalog and never recorded | Accepted |
| [0038](0038-a-diagnostic-bundle-is-a-chosen-verbatim-local-copy.md) | A diagnostic bundle is a chosen, verbatim, local copy | Accepted |
| [0039](0039-a-split-workspace-composes-host-views-and-owns-no-feature-state.md) | A split workspace composes host views and owns no feature state | Accepted |
| [0040](0040-a-linked-pair-chooses-one-linked-server-and-starts-as-one-operation.md) | A linked pair chooses one linked server and starts as one operation | Accepted |
