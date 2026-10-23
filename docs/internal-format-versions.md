# Internal format versions

Ephervex owns several private formats. They are implementation state, not an
exchange format or a compatibility promise to another launcher. ADR 0033 keeps
the three host-facing format versions at `1` until the 1.0.0 boundary.

| Format | Version source | Current value | Compatibility rule |
|---|---|---:|---|
| Configuration | `CONFIG_SCHEMA_VERSION` | 1 | Additive fields default when absent. Unknown keys survive a load and save, including the key of a setting a later build removed. |
| Instance and server metadata | `METADATA_VERSION` | 1 | Additive fields default when absent. New advisory attention reasons remain readable. |
| Retained pack recipe | `RECIPE_VERSION` | 1 | Additive fields default when absent. Authoritative values that affect reconstruction fail closed when unknown. |
| Download metadata envelope | `METADATA_ENVELOPE_VERSION` | 1 | Cache state may be discarded and fetched again. |
| Minecraft preference catalog | `PREFERENCE_CATALOG_FORMAT_VERSION` | 1 | A document with a newer format is rejected as a whole. |
| Minecraft options dataset | `DATASET_FORMAT_VERSION` | 1 | A document with a newer format is rejected as a whole. |
| Third-party license inventory | `LICENSE_INVENTORY_FORMAT_VERSION` | 1 | A document with a newer format is rejected as a whole. Generated from `Cargo.lock` and compiled in, so a mismatch is a broken build rather than stale user data. |
| Server properties saved while running | `PENDING_PROPERTIES_FORMAT_VERSION` | 1 | A newer document is refused, and the start that would apply it fails rather than drop it. The next start that applies it removes it. |
| Mod isolation session | `ISOLATION_VERSION` | 1 | A newer document is rejected. Retained state changes remain additive before 1.0.0; finish or cancel removes the session. |
| Mod isolation server marker | `LEADER_VERSION` | 1 | A newer document is rejected. The marker only names the client to ask; a leftover file is inert, and ownership comes from the client session that lists the server. |
| Instance organization | `ORGANIZATION_FORMAT_VERSION` | 1 | Additive fields default when absent. Unknown fields survive a load and save. A file from a newer build, or one that cannot be read, is moved aside and replaced by `organization.old.json`, the copy from before the last change, or by an empty organization when that copy cannot be used either (ADR 0048). |
| SQLite index | ordered migrations | 1 | The pre-1.0 schema is one initial migration. Later migrations are append-only. The index remains a recovery copy, not host authority. |
| Mod display cache | `CACHE_VERSION` | 1 | Cache state may be discarded and rebuilt. |
| Minecraft analysis workspace | `ANALYSIS_SCHEMA_VERSION` | 1 | Generated analysis state may be discarded and rebuilt. |

A field that the index has no column for stays in the host's metadata alone
until migration 2 is available after 1.0.0. A server's
`allow_pack_mod_version_changes` is one: reading the index reports it off, and
every read for showing or changing the server, and reconciliation, take it from
the metadata. A client's optional `issue_link` is another: reading the index
reports it absent, and every read for showing or changing the client, and
reconciliation, take it from the metadata. Both are additive fields of metadata
version 1.

## Persisted enum compatibility

Unknown values are handled according to what the value authorizes:

- `AttentionReason` is advisory. An unknown tagged reason becomes `Unknown`,
  so a newer warning cannot hide an otherwise readable instance or server.
- Configuration preferences that are designed to round-trip future values use
  their explicit unrecognized representation or retain the unknown TOML key.
- Loader, storage, server installation, server launch, pack side, side
  requirement, relation target, ownership, and hash algorithm values affect
  launch, mutation, verification, or reconstruction. An unknown value is not
  safe to approximate and makes that record or recipe unreadable.
- Cache-only enums may reject an unknown value because deleting and rebuilding
  the cache is the recovery path.

This distinction is intentional. A catch-all is safe for an advisory label,
but unsafe when it could turn an unknown loader or content requirement into a
different runnable state.

See [ADR 0033](adr/0033-internal-format-versions-stay-at-1-until-1.0.0.md) for
the pre-1.0 version freeze and its release-boundary rules.
