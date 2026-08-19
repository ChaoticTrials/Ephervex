#!/usr/bin/env python3
"""Builds Ephervex's bundled Minecraft product preference catalog.

The compatibility dataset (`dataset.py`) answers "how does this Minecraft
release store this setting". This module answers a different question: "which
of those settings does this Ephervex build expose as a preference, and how
should the product understand and present it". The two are kept apart on
purpose, matching `crates/options/data/minecraft-options.json` and
`crates/options/data/minecraft-preferences.json`:

* the compatibility dataset is remote-updatable data, so a Minecraft hotfix
  never needs a new Ephervex release just to stay readable;
* the preference catalog is bundled application source. It is generated, but
  publishing it requires an Ephervex release, exactly like any other code
  change, because it is the thing that decides what a settings screen offers.

A remote compatibility update can therefore extend *how* a release stores a
setting Ephervex already offers. It can never make a new setting appear on a
settings screen: this module reads the compatibility dataset only to decide
*how to present* an already-current, already-manageable canonical setting, and
its own policy tables (`EXCLUSIONS`, the Window Size composition, the stable
id overrides) are reviewed application source, not generated facts.

Accounting invariant: every setting in the compatibility dataset's current
canonical set is either exposed by exactly one descriptor or explicitly
excluded with a durable reason. `validate` proves both halves and rejects a
document that leaves anything unaccounted for, claims a historical or
non-manageable setting, or lets two descriptors claim the same canonical
field.
"""

from __future__ import annotations

import json
import re
from pathlib import Path
from typing import Any

# Bumped only when the document's own shape changes.
FORMAT_VERSION = 1

# The canonical dataset id that Language / Accessibility settings persist
# language under. Its presentation is the special `language` kind rather than
# generic text: the set of valid codes is Mojang-owned volatile metadata that
# belongs to the existing language-catalog mechanism, never to this file.
LANGUAGE_CANONICAL_ID = "languageCode"

# Maps the analyzer's own rough grouping to Ephervex's stable product
# categories. Closed on purpose: a settings screen needs a fixed set of tabs,
# not one that grows with Minecraft's own menu structure.
CATEGORIES = {
    "Video": "video",
    "Sound & Music": "sound_and_music",
    "Controls": "controls",
    "Gameplay": "gameplay",
    "Language / Accessibility": "language_and_accessibility",
}

# Presentation kind -> the canonical setting types it may legitimately cover.
# A descriptor whose kind does not match its own field's registry type is
# refused: this is what stops a generator regression from quietly turning a
# boolean into an enum select. `integer` and `decimal` both accept either
# registry type: the registry's `type` is the JVM field's own classification,
# which can legitimately disagree with the proven codec `_numeric_kind`
# actually presents through (a JVM `int` field can still persist a decimal
# value, as `entityDistanceScaling` does); the codec, not the field type, is
# what decides whether a value is representable.
EXPECTED_VALUE_TYPES = {
    "toggle": {"boolean"},
    "integer": {"integer", "number"},
    "decimal": {"integer", "number"},
    "enum": {"enum"},
    "language": {"string"},
    "key_binding": {"key_binding"},
    "text": {"string"},
    "window_size": {"integer", "number"},
}

KNOWN_TIERS = {"standard", "advanced"}

# Stable product ids for the preferences that predate this catalog. Kept
# identical to `PreferenceKey::as_str()` in `crates/options/src/preferences.rs`
# so the existing config/CLI vocabulary stays representable. Every other
# preference derives its id mechanically from its canonical id instead of
# being hand-assigned here.
STABLE_ID_OVERRIDES = {
    "guiScale": "gui_scale",
    LANGUAGE_CANONICAL_ID: "language",
    "fullscreen": "fullscreen",
    "fov": "fov",
    "renderDistance": "render_distance",
    "simulationDistance": "simulation_distance",
}

# The one compound descriptor this catalog composes. Modeled as data, not a
# special case, so a future compound preference does not need new machinery.
COMPOUND_DESCRIPTORS = (
    {
        "id": "window_size",
        "category": "video",
        "fields": (("width", "overrideWidth"), ("height", "overrideHeight")),
    },
)

# Deliberately unmanaged current settings and why. Reviewed application
# policy, not a generated fact: an id here that is no longer part of the
# current canonical set is simply not emitted, because a setting that does not
# exist anymore needs no exclusion (see `build`).
EXCLUSIONS = {
    "incompatibleResourcePacks": (
        "resource-pack compatibility state belongs to resource/pack management, not a global preference"
    ),
    "joinedFirstServer": "the first-multiplayer-join marker is client-owned session state, not a preference",
    "lastMpIp": "the last-server address is session/history state, not a portable preference",
    "resourcePacks": "resource-pack selection belongs to resource/pack management, not a global preference",
    "soundDevice": "the selected sound device is machine and environment specific, not portable",
    "startedCleanly": "the clean-startup marker is client-owned crash-recovery state",
    "tutorialStep": "tutorial progress is client-owned state, not a portable global preference",
}

# Keybinding categories the research recovered that this catalog treats as
# advanced/developer-facing rather than a typical settings-screen row.
ADVANCED_KEYBINDING_CATEGORIES = frozenset({"debug"})

class PreferenceCatalogError(Exception):
    """The product preference catalog is not internally consistent, or
    disagrees with the compatibility dataset it was built from."""


def snake_case(identifier: str) -> str:
    """Deterministic `camelCase`/`dotted.id` -> `snake_case` conversion.

    Used only for canonical ids that have no explicit product identity in
    `STABLE_ID_OVERRIDES`. The result must stay a pure function of the input:
    generation reproducibility depends on it never consulting anything but the
    string itself. A boundary is inserted before an uppercase letter that
    follows a lowercase letter or digit (`guiScale` -> `gui_Scale`) and before
    the last letter of an acronym run that starts a new word (`invertXMouse`
    -> `invert_X_Mouse`), so an acronym-then-word id splits the same way a
    person reading it would.
    """
    dotless = identifier.replace(".", "_")
    out: list[str] = []
    for i, ch in enumerate(dotless):
        if ch.isupper() and i > 0:
            previous = dotless[i - 1]
            next_char = dotless[i + 1] if i + 1 < len(dotless) else ""
            if previous.islower() or previous.isdigit() or next_char.islower():
                out.append("_")
        out.append(ch.lower())
    return re.sub("_+", "_", "".join(out))


def stable_id(canonical_id: str) -> str:
    return STABLE_ID_OVERRIDES.get(canonical_id, snake_case(canonical_id))


def _canonical_schema(dataset_document: dict[str, Any]) -> dict[str, Any]:
    version = dataset_document["canonical_minecraft_version"]
    schema_id = next(
        release["schema"]
        for release in dataset_document["releases"]
        if release["minecraft_version"] == version
    )
    return next(schema for schema in dataset_document["schemas"] if schema["id"] == schema_id)


def _codec_by_id(dataset_document: dict[str, Any]) -> dict[str, dict[str, Any]]:
    schema = _canonical_schema(dataset_document)
    representations = dataset_document["representations"]
    return {
        setting_id: representations[position]["codec"]
        for setting_id, position in schema["supported"].items()
    }


def rich_settings_by_canonical_id(record: dict[str, Any]) -> dict[str, dict[str, Any]]:
    """Indexes one analyzer release record by canonical (logical) id.

    The record must be the canonical release's own `normalize()` output: an
    older release's ids are obfuscated field names, and would not line up with
    the compatibility dataset's registry.
    """
    return {setting["logical_id"]: setting for setting in record["settings"]}


def _proven_default(rich: dict[str, Any] | None, expected_type: type) -> Any:
    if rich is None or rich.get("default_confidence") != "high":
        return None
    default = rich.get("default")
    if not isinstance(default, expected_type) or isinstance(default, bool) != (expected_type is bool):
        return None
    return default


def _numeric_kind(value_type: str, codec: dict[str, Any] | None) -> str:
    """Whether a numeric field should present as `integer` or `decimal`.

    Driven by the proven compatibility codec's own `kind`, not by the
    analyzer's JVM field-type classification (`entry["type"]`): the two can
    legitimately disagree, because a value can live in an `int` field and
    still be persisted through a decimal codec (`entityDistanceScaling` is
    exactly this shape - a JVM `int` field storing 0.5..5.0). The codec is
    what actually decides whether a fractional value is representable, so it
    is what decides the presentation kind too. Falls back to the field-type
    classification only when the canonical release has no proven codec at
    all to consult.
    """
    codec_kind = (codec or {}).get("kind")
    if codec_kind in ("integer", "decimal", "scaled_decimal"):
        return "integer" if codec_kind == "integer" else "decimal"
    return "integer" if value_type == "integer" else "decimal"


def _numeric_default(rich: dict[str, Any] | None, kind: str) -> int | float | None:
    if rich is None or rich.get("default_confidence") != "high":
        return None
    default = rich.get("default")
    if isinstance(default, bool) or not isinstance(default, (int, float)):
        return None
    if kind == "integer":
        # A genuinely fractional default has no honest integer presentation;
        # dropping it is safer than rounding a value nobody proved.
        return default if isinstance(default, int) else None
    return float(default)


def _numeric_presentation(kind: str, codec: dict[str, Any] | None, rich: dict[str, Any] | None) -> dict[str, Any]:
    codec = codec or {}
    minimum, maximum = codec.get("min"), codec.get("max")
    if kind == "decimal":
        minimum = None if minimum is None else float(minimum)
        maximum = None if maximum is None else float(maximum)
    default = _numeric_default(rich, kind)
    step = None
    if rich is not None and minimum is not None and maximum is not None:
        numeric = rich.get("numeric") or {}
        proven_min, proven_max = numeric.get("min"), numeric.get("max")
        # Only carried when the analyzer's own domain agrees with the proven
        # compatibility codec: a step attached to a range the dataset could
        # not corroborate would be presentation invented from an unresolved
        # extraction, exactly what this catalog must never do.
        agrees = (
            proven_min is not None
            and proven_max is not None
            and float(proven_min) == minimum
            and float(proven_max) == maximum
        )
        if agrees and numeric.get("step") is not None:
            step = numeric["step"] if kind == "integer" else float(numeric["step"])
    return {"kind": kind, "default": default, "min": minimum, "max": maximum, "step": step}


def _enum_values(codec: dict[str, Any] | None, rich: dict[str, Any] | None) -> list[dict[str, Any]]:
    codec = codec or {}
    if codec.get("kind") == "enum_id":
        proven = {str(value) for value in codec.get("values", [])}
    elif codec.get("kind") == "enum_token":
        proven = set(codec.get("tokens", []))
    else:
        proven = None

    out: list[dict[str, Any]] = []
    seen: set[str] = set()
    for entry in ((rich or {}).get("allowed_values") or {}).get("values") or []:
        persisted = entry.get("persisted")
        if persisted is None:
            continue
        technical = str(persisted)
        if proven is not None and technical not in proven:
            continue  # Not corroborated by the proven codec; do not label it.
        if technical in seen:
            continue
        seen.add(technical)
        out.append(
            {
                "technical_value": technical,
                "enum_constant": entry.get("enum_constant"),
                "translation_key": entry.get("translation_key"),
                "ordinal": entry.get("ordinal"),
            }
        )
    if not out and proven:
        # No enrichment available (or none survived corroboration): the bare
        # proven values are still real product information, just unlabeled.
        out = [
            {"technical_value": value, "enum_constant": None, "translation_key": None, "ordinal": None}
            for value in sorted(proven)
        ]
    out.sort(key=lambda value: (value["ordinal"] if value["ordinal"] is not None else 1 << 30, value["technical_value"]))
    return out


def _enum_default(rich: dict[str, Any] | None) -> str | None:
    if rich is None or rich.get("default_confidence") != "high":
        return None
    default = rich.get("default")
    return None if default is None else str(default)


def _key_binding_default(rich: dict[str, Any] | None) -> dict[str, Any] | None:
    """The proven default binding, or `None` when the analyzer proved none.

    Gated on `default_confidence` exactly like every other presentation kind:
    the analyzer only reaches `confidence == "high"` for a keybinding once it
    has independently resolved a serialized name (a proven bound default) or
    proven the constructor's default is the unbound marker (a proven unbound
    default). A lower confidence means neither was established, and the raw
    `default` object's `serialized: None, unbound: False` in that case is
    only the shape's own placeholder, not a proven fact - it must not be
    carried forward as if `unbound` were proven `False`.
    """
    if rich is None or rich.get("default_confidence") != "high":
        return None
    default = rich.get("default") or {}
    return {"serialized": default.get("serialized"), "unbound": bool(default.get("unbound", False))}


def _presentation(cid: str, value_type: str, codec: dict[str, Any] | None, rich: dict[str, Any] | None) -> dict[str, Any]:
    if cid == LANGUAGE_CANONICAL_ID:
        return {"kind": "language", "default": _proven_default(rich, str)}
    if value_type == "boolean":
        return {"kind": "toggle", "default": _proven_default(rich, bool)}
    if value_type in ("integer", "number"):
        return _numeric_presentation(_numeric_kind(value_type, codec), codec, rich)
    if value_type == "enum":
        return {"kind": "enum", "default": _enum_default(rich), "values": _enum_values(codec, rich)}
    if value_type == "key_binding":
        return {"kind": "key_binding", "default": _key_binding_default(rich)}
    if value_type == "string":
        return {"kind": "text", "default": _proven_default(rich, str)}
    raise PreferenceCatalogError(f"`{cid}` has canonical type `{value_type}`, which no presentation kind covers")


def _section(cid: str, value_type: str, rich: dict[str, Any] | None) -> str | None:
    if value_type == "key_binding":
        return (rich or {}).get("binding_category")
    if cid.startswith("sound.volume."):
        return "volume"
    if cid.startswith("modelPart."):
        return "skin_parts"
    return None


def _tier(value_type: str, rich: dict[str, Any] | None) -> str:
    if value_type == "key_binding" and rich and rich.get("binding_category") in ADVANCED_KEYBINDING_CATEGORIES:
        return "advanced"
    return "standard"


def _simple_descriptor(
    cid: str,
    entry: dict[str, Any],
    codec: dict[str, Any] | None,
    rich: dict[str, Any] | None,
) -> dict[str, Any]:
    value_type = entry["type"]
    return {
        "id": stable_id(cid),
        "category": CATEGORIES[entry["category"]],
        "section": _section(cid, value_type, rich),
        "tier": _tier(value_type, rich),
        "translation_key": entry.get("translation_key"),
        "help_translation_key": None,
        "fields": [{"role": "value", "canonical_id": cid}],
        "presentation": _presentation(cid, value_type, codec, rich),
    }


def _compound_descriptor(spec: dict[str, Any]) -> dict[str, Any]:
    return {
        "id": spec["id"],
        "category": spec["category"],
        "section": None,
        "tier": "standard",
        "translation_key": None,
        "help_translation_key": None,
        "fields": [{"role": role, "canonical_id": cid} for role, cid in spec["fields"]],
        "presentation": {"kind": "window_size"},
    }


def build(dataset_document: dict[str, Any], rich_settings: dict[str, dict[str, Any]] | None = None) -> dict[str, Any]:
    """Builds the product preference catalog from the compatibility dataset.

    `rich_settings` is the canonical release's own analyzer record, indexed by
    canonical id (see `rich_settings_by_canonical_id`). It is optional: every
    accounting fact (what is current, what is manageable, what the safe
    numeric/enum domain is) comes from `dataset_document` alone, so an absent
    or partial `rich_settings` only narrows presentation enrichment (proven
    defaults, enum labels, keybinding grouping), never the accounting.
    """
    registry = {entry["id"]: entry for entry in dataset_document["setting_registry"]}
    current_ids = set(dataset_document["current_canonical_setting_ids"])
    codec_by_id = _codec_by_id(dataset_document)
    rich_settings = rich_settings or {}

    claimed: set[str] = set()
    descriptors: list[dict[str, Any]] = []

    for spec in COMPOUND_DESCRIPTORS:
        ids = {cid for _, cid in spec["fields"]}
        if not ids <= current_ids:
            raise PreferenceCatalogError(f"`{spec['id']}` references a non-current canonical setting")
        descriptors.append(_compound_descriptor(spec))
        claimed |= ids

    exclusions = []
    for cid, reason in sorted(EXCLUSIONS.items()):
        if cid not in current_ids:
            # No longer part of the current release: nothing to exclude.
            continue
        exclusions.append({"canonical_id": cid, "reason": reason})
        claimed.add(cid)

    manageable_current = {cid for cid in current_ids if registry[cid]["manageable"]}
    for cid in sorted(manageable_current - claimed):
        entry = registry[cid]
        descriptors.append(_simple_descriptor(cid, entry, codec_by_id.get(cid), rich_settings.get(cid)))
        claimed.add(cid)

    descriptors.sort(key=lambda descriptor: descriptor["id"])
    return {
        "format_version": FORMAT_VERSION,
        "canonical_minecraft_version": dataset_document["canonical_minecraft_version"],
        "descriptors": descriptors,
        "exclusions": exclusions,
    }


def validate(document: dict[str, Any], dataset_document: dict[str, Any]) -> None:
    """Checks a catalog is internally consistent and agrees with its dataset."""
    if document.get("format_version") != FORMAT_VERSION:
        raise PreferenceCatalogError(f"format version {document.get('format_version')} is not {FORMAT_VERSION}")
    if document.get("canonical_minecraft_version") != dataset_document.get("canonical_minecraft_version"):
        raise PreferenceCatalogError("the catalog's canonical release does not match the compatibility dataset")

    registry = {entry["id"]: entry for entry in dataset_document["setting_registry"]}
    current_ids = set(dataset_document["current_canonical_setting_ids"])

    descriptors = document["descriptors"]
    ids = [descriptor["id"] for descriptor in descriptors]
    if len(set(ids)) != len(ids):
        raise PreferenceCatalogError("two descriptors share one product preference id")

    claimed_by: dict[str, str] = {}
    exposed_ids: set[str] = set()
    for descriptor in descriptors:
        did = descriptor["id"]
        if descriptor["category"] not in CATEGORIES.values():
            raise PreferenceCatalogError(f"`{did}` uses an unknown category `{descriptor['category']}`")
        if descriptor["tier"] not in KNOWN_TIERS:
            raise PreferenceCatalogError(f"`{did}` uses an unknown presentation tier `{descriptor['tier']}`")

        kind = descriptor["presentation"]["kind"]
        if kind not in EXPECTED_VALUE_TYPES:
            raise PreferenceCatalogError(f"`{did}` uses an unknown presentation kind `{kind}`")

        roles = sorted(field["role"] for field in descriptor["fields"])
        expected_roles = ["height", "width"] if kind == "window_size" else ["value"]
        if roles != expected_roles:
            raise PreferenceCatalogError(f"`{did}` does not reference the fields its presentation kind requires")

        for field in descriptor["fields"]:
            cid = field["canonical_id"]
            if cid in claimed_by:
                raise PreferenceCatalogError(
                    f"canonical setting `{cid}` is claimed by both `{claimed_by[cid]}` and `{did}`"
                )
            claimed_by[cid] = did
            exposed_ids.add(cid)
            if cid not in registry:
                raise PreferenceCatalogError(f"`{did}` references unknown canonical setting `{cid}`")
            if cid not in current_ids:
                raise PreferenceCatalogError(f"`{did}` references historical (non-current) setting `{cid}`")
            if not registry[cid]["manageable"]:
                raise PreferenceCatalogError(f"`{did}` exposes `{cid}`, which is not manageable")
            if registry[cid]["type"] not in EXPECTED_VALUE_TYPES[kind]:
                raise PreferenceCatalogError(
                    f"`{did}` presentation `{kind}` does not fit canonical type `{registry[cid]['type']}` of `{cid}`"
                )

        if kind == "enum":
            values = descriptor["presentation"]["values"]
            technical_values = [value["technical_value"] for value in values]
            if len(set(technical_values)) != len(technical_values):
                raise PreferenceCatalogError(f"`{did}` has two enum values with the same technical value")
            default = descriptor["presentation"].get("default")
            if default is not None and default not in technical_values:
                raise PreferenceCatalogError(f"`{did}` default is not one of its own enum values")

        if kind in ("integer", "decimal"):
            presentation = descriptor["presentation"]
            minimum, maximum, step = presentation.get("min"), presentation.get("max"), presentation.get("step")
            if minimum is not None and maximum is not None and minimum > maximum:
                raise PreferenceCatalogError(f"`{did}` numeric domain is inverted")
            if step is not None and step <= 0:
                raise PreferenceCatalogError(f"`{did}` numeric step must be positive")
            default = presentation.get("default")
            if default is not None:
                below = minimum is not None and default < minimum
                above = maximum is not None and default > maximum
                if below or above:
                    raise PreferenceCatalogError(f"`{did}` default is outside its own numeric domain")

    exclusion_ids: set[str] = set()
    for exclusion in document["exclusions"]:
        cid = exclusion["canonical_id"]
        if not exclusion.get("reason", "").strip():
            raise PreferenceCatalogError(f"exclusion `{cid}` has no reason")
        if cid not in current_ids:
            raise PreferenceCatalogError(f"exclusion `{cid}` is not a current canonical setting")
        if cid in exclusion_ids:
            raise PreferenceCatalogError(f"`{cid}` is excluded twice")
        exclusion_ids.add(cid)

    overlap = exposed_ids & exclusion_ids
    if overlap:
        raise PreferenceCatalogError(f"settings both exposed and excluded: {', '.join(sorted(overlap))}")

    unaccounted = current_ids - exposed_ids - exclusion_ids
    if unaccounted:
        raise PreferenceCatalogError(f"current canonical settings not accounted for: {', '.join(sorted(unaccounted))}")


def dumps(document: dict[str, Any]) -> str:
    """The exact on-disk text: compact, sorted, newline terminated.

    Machine-owned JSON stays compact per repository policy; unlike the
    compatibility dataset, this file is reviewed as source through its diff,
    not through pretty-printed on-disk formatting.
    """
    return json.dumps(document, sort_keys=True, separators=(",", ":"), ensure_ascii=True) + "\n"


# --------------------------------------------------------------- maintainer CLI


def _load_normalized_record(analysis_dir: Path, canonical_version: str) -> dict[str, Any] | None:
    directory = analysis_dir / "normalized"
    if not directory.is_dir():
        return None
    for path in sorted(directory.glob("*.json")):
        record = json.loads(path.read_text(encoding="utf-8"))
        if record.get("minecraft_version") == canonical_version:
            return record
    return None


def main() -> int:
    import argparse
    import sys

    tool_dir = Path(__file__).resolve().parent
    repo_root = tool_dir.parent.parent
    default_dataset = repo_root / "crates" / "options" / "data" / "minecraft-options.json"
    default_out = repo_root / "crates" / "options" / "data" / "minecraft-preferences.json"

    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--dataset", type=Path, default=default_dataset, help="the compatibility dataset to build from")
    parser.add_argument(
        "--from-analysis",
        type=Path,
        help="an analyzer output directory (see tools/minecraft-options/update.py) to enrich "
        "presentation with the canonical release's proven defaults, enum labels and keybinding groups",
    )
    parser.add_argument("--out", type=Path, default=default_out, help="where to write the catalog")
    parser.add_argument(
        "--check",
        action="store_true",
        help="verify the existing catalog at --out matches what these inputs regenerate, and write nothing",
    )
    args = parser.parse_args()

    dataset_document = json.loads(args.dataset.read_text(encoding="utf-8"))
    rich_settings: dict[str, dict[str, Any]] = {}
    if args.from_analysis:
        record = _load_normalized_record(args.from_analysis, dataset_document["canonical_minecraft_version"])
        if record is None:
            print(
                f"[preferences] no analysis for {dataset_document['canonical_minecraft_version']} "
                f"under {args.from_analysis}; continuing without enrichment",
                file=sys.stderr,
            )
        else:
            rich_settings = rich_settings_by_canonical_id(record)

    try:
        document = build(dataset_document, rich_settings)
        validate(document, dataset_document)
    except PreferenceCatalogError as error:
        print(f"[preferences] {error}", file=sys.stderr)
        return 1

    text = dumps(document)
    if args.check:
        current = args.out.read_text(encoding="utf-8") if args.out.exists() else ""
        if current != text:
            print("[preferences] the bundled catalog does not match what these inputs regenerate", file=sys.stderr)
            return 1
        print("[preferences] bundled catalog is exactly what these inputs regenerate")
        return 0

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(text, encoding="utf-8")
    exposed = sum(len(descriptor["fields"]) for descriptor in document["descriptors"])
    print(f"{len(document['descriptors'])} product preference(s), {exposed} canonical field(s) exposed")
    print(f"{len(document['exclusions'])} canonical setting(s) explicitly excluded")
    print(f"bundled preference catalog: {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
