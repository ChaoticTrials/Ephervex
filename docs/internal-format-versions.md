# Internal format versions

Ephervex owns several private formats. They are implementation state, not an
exchange format or a compatibility promise to another launcher. ADR 0033 keeps
the three host-facing format versions at `1` until the 1.0.0 boundary.

| Format | Version source | Current value | Compatibility rule |
|---|---|---:|---|
| Configuration | `CONFIG_SCHEMA_VERSION` | 1 | Additive fields default when absent. Unknown keys survive a load and save. |
| Instance and server metadata | `METADATA_VERSION` | 1 | Additive fields default when absent. New advisory attention reasons remain readable. |
| Retained pack recipe | `RECIPE_VERSION` | 1 | Additive fields default when absent. Authoritative values that affect reconstruction fail closed when unknown. |
| Download metadata envelope | `METADATA_ENVELOPE_VERSION` | 1 | Cache state may be discarded and fetched again. |
| Minecraft preference catalog | `PREFERENCE_CATALOG_FORMAT_VERSION` | 1 | A document with a newer format is rejected as a whole. |
| Minecraft options dataset | `DATASET_FORMAT_VERSION` | 1 | A document with a newer format is rejected as a whole. |
| SQLite index | ordered migrations | 1 | The pre-1.0 schema is one initial migration. Later migrations are append-only. The index remains a recovery copy, not host authority. |
| Mod display cache | `CACHE_VERSION` | 1 | Cache state may be discarded and rebuilt. |
| Minecraft analysis workspace | `ANALYSIS_SCHEMA_VERSION` | 1 | Generated analysis state may be discarded and rebuilt. |

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
