#!/usr/bin/env python3
"""Turn analyzer output into Ephervex's options compatibility dataset.

The dataset is the single generated source of truth for "how does this
Minecraft release store this setting in options.txt". It ships bundled with the
application and is also the exact document published to the remote options
feed, so one builder produces both and the two can never disagree in shape.

Three things are kept apart on purpose:

* **canonical settings** - stable ids for the settings the research recovered,
  independent of any release's obfuscated field names;
* **schema families** - one deduplicated per-release representation of every
  canonical setting, identified by a content-derived fingerprint;
* **release mapping** - which release uses which family.

A representation is only ever written when the analyzer proved it. Everything
else stays absent, and absence means *unknown*, which is deliberately different
from the explicit `unsupported` list: unknown means "not established", while
unsupported means "the key provably did not exist in that release".

Codecs are closed, typed variants with validated parameters. A formula the
analyzer recovered as text is either recognised as one of those variants here,
at generation time, or the setting is left unknown. No expression ever reaches
the dataset, and therefore none can reach the application.
"""

from __future__ import annotations

import hashlib
import json
import math
import re
from typing import Any

# Bumped only when the document's own shape changes. An application that reads
# a higher version must refuse the document rather than guess at it.
FORMAT_VERSION = 1

# The canonical setting ids Ephervex's own preference model is built on. The
# generator refuses to publish a dataset that cannot describe all of them for
# the canonical release, because that would ship a build whose settings screen
# offers something the data cannot express.
REQUIRED_CANONICAL_IDS = (
    "guiScale",
    "languageCode",
    "fullscreen",
    "overrideWidth",
    "overrideHeight",
    "fov",
    "renderDistance",
    "simulationDistance",
)

# `lang` holds a language code whose validity is a property of the target
# release's language catalog, not of options.txt. It gets its own codec so the
# application knows a catalog check is required before writing.
LANGUAGE_KEY = "lang"

# `((value - 70) / 40)` and `(value - 1)`: the only storage transforms the
# research recovered. Parsed into typed parameters rather than carried as text.
_AFFINE = re.compile(r"^\(\(value - (-?[0-9.]+)\) / (-?[0-9.]+)\)$")
_OFFSET = re.compile(r"^\(value - (-?[0-9.]+)\)$")


class DatasetError(Exception):
    """The inputs cannot produce a publishable dataset."""


# --------------------------------------------------------------------- codecs


def _number(text: str) -> float:
    value = float(text)
    if not math.isfinite(value):
        raise DatasetError(f"`{text}` is not a finite number")
    return value


def _compact(value: float) -> float | int:
    """Integral floats are written as integers so fingerprints stay stable."""
    return int(value) if float(value).is_integer() else value


def _domain(setting: dict[str, Any]) -> dict[str, Any]:
    """The proven numeric domain, or an open one when it was not proven."""
    if setting.get("domain_confidence") != "high":
        return {"min": None, "max": None}
    numeric = setting.get("numeric") or {}
    minimum, maximum = numeric.get("min"), numeric.get("max")
    return {
        "min": None if minimum is None else _compact(minimum),
        "max": None if maximum is None else _compact(maximum),
    }


def codec_for(setting: dict[str, Any], code_space: str | None) -> dict[str, Any] | None:
    """The typed codec for one release's own extraction of one setting.

    `None` means the representation was not established, which the caller turns
    into *unknown* rather than into a guess.
    """
    if setting.get("confidence") != "high" or setting.get("type_confidence") != "high":
        return None

    kind = setting.get("type")
    if setting.get("persisted_key") == LANGUAGE_KEY:
        return {"kind": "language_code"}
    if kind == "boolean":
        return {"kind": "boolean"}
    if kind == "string":
        return {"kind": "text"}
    if kind == "key_binding":
        if code_space == "lwjgl2":
            return {"kind": "key_code_lwjgl2"}
        if code_space == "glfw":
            return {"kind": "key_name_glfw"}
        return None
    if kind in ("integer", "number"):
        return _numeric_codec(setting)
    if kind == "enum":
        return _enum_codec(setting)
    # `special` covers the Gson-serialised list settings, whose representation
    # Ephervex has no reason to reproduce.
    return None


def _numeric_codec(setting: dict[str, Any]) -> dict[str, Any] | None:
    domain = _domain(setting)
    # How the release writes the number is what matters, not the Java type it
    # holds in memory: 1.19 onward stores the field of view as an integral
    # value through a double codec, and writing `70` where the game writes
    # `70.0` would be a gratuitous difference from the file it maintains itself.
    encoding = setting.get("file_encoding") or ""
    if encoding == "decimal integer":
        written = "integer"
    elif encoding.startswith("decimal floating point"):
        written = "decimal"
    else:
        return None

    transform = setting.get("value_to_storage")
    if transform:
        affine = _AFFINE.match(transform)
        offset = _OFFSET.match(transform)
        if affine:
            scale = _number(affine.group(2))
            if scale == 0:
                return None
            return {
                "kind": "scaled_decimal",
                "offset": _compact(_number(affine.group(1))),
                "scale": _compact(scale),
                **domain,
            }
        if offset:
            return {
                "kind": "scaled_decimal",
                "offset": _compact(_number(offset.group(1))),
                "scale": 1,
                **domain,
            }
        # A transform shape this generator does not recognise. Leaving the
        # setting unknown is the only safe outcome: the alternative is writing
        # a value through the wrong arithmetic.
        return None
    return {"kind": written, **domain}


def _enum_codec(setting: dict[str, Any]) -> dict[str, Any] | None:
    allowed = setting.get("allowed_values") or {}
    form = allowed.get("persisted_form")
    values = allowed.get("values") or []
    if setting.get("domain_confidence") != "high" or not values:
        return None
    if form == "integer_id":
        ids = sorted({v["persisted"] for v in values if isinstance(v.get("persisted"), int)})
        return {"kind": "enum_id", "values": ids} if len(ids) == len(values) else None
    if form == "string_token":
        tokens = sorted({v["persisted"] for v in values if isinstance(v.get("persisted"), str)})
        return {"kind": "enum_token", "tokens": tokens} if len(tokens) == len(values) else None
    return None


# ------------------------------------------------------------------- building


def canonical_settings(records: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Stable canonical setting definitions across every analyzed release.

    Ids come from the canonical release, which is deobfuscated and therefore
    has real field names. A key that only older releases had keeps its
    persisted key as its id, because an obfuscated field name is not a name.
    Older releases are matched to these definitions by persisted key, which the
    research shows has never been renamed across the supported range.

    `records` must be ordered oldest first: a later release's definition wins,
    so the newest description of a setting is the one carried.
    """
    canonical = {setting["persisted_key"]: setting for setting in records[-1]["settings"]}
    definitions: dict[str, dict[str, Any]] = {}
    for record in records:
        for setting in record["settings"]:
            key = setting["persisted_key"]
            named = canonical.get(key)
            definitions[key] = {
                "id": named["logical_id"] if named else key,
                "key": key,
                "type": setting["type"],
                "category": setting.get("category"),
                "translation_key": setting.get("translation_key"),
                "manageable": bool(setting.get("ephervex_manageable")),
            }
    settings = sorted(definitions.values(), key=lambda entry: entry["id"])
    if len({entry["id"] for entry in settings}) != len(settings):
        raise DatasetError("two canonical settings share one id")
    return settings


def schema_for(record: dict[str, Any], settings: list[dict[str, Any]]) -> dict[str, Any]:
    """One release's expanded representation of every canonical setting."""
    by_key = {entry["key"]: entry for entry in settings}
    extracted = {setting["persisted_key"]: setting for setting in record["settings"]}
    code_space = record.get("keybinding_code_space")

    supported: dict[str, dict[str, Any]] = {}
    unsupported: list[str] = []
    for entry in settings:
        setting = extracted.get(entry["key"])
        if setting is None:
            # Absent from a completely extracted persistence path is proof the
            # key did not exist in this release. Without that proof it is only
            # unknown, and unknown is never written.
            if record.get("extraction_complete"):
                unsupported.append(entry["id"])
            continue
        codec = codec_for(setting, code_space)
        if codec is not None:
            supported[entry["id"]] = {"key": setting["persisted_key"], "codec": codec}

    # Keys this release has that the canonical release does not know about are
    # deliberately dropped: a canonical setting definition is the only thing
    # that gives an id meaning, and inventing one here would create a setting
    # no application build could interpret.
    unknown_to_canonical = sorted(set(extracted) - set(by_key))
    return {
        "supported": supported,
        "unsupported": sorted(unsupported),
        "extra_keys": unknown_to_canonical,
    }


def fingerprint(schema: dict[str, Any]) -> str:
    """A content-derived family id.

    Derived from the expanded representation alone, never from a version
    number, so two releases share a family only when their extracted data is
    genuinely identical. Table positions are excluded so that adding a release
    cannot renumber an existing family.
    """
    body = json.dumps(
        {"supported": schema["supported"], "unsupported": schema["unsupported"]},
        sort_keys=True,
        separators=(",", ":"),
        ensure_ascii=True,
    )
    return hashlib.sha256(body.encode("utf-8")).hexdigest()[:16]


def _representation_table(schemas: dict[str, dict[str, Any]]) -> list[dict[str, Any]]:
    seen: dict[str, dict[str, Any]] = {}
    for schema in schemas.values():
        for representation in schema["supported"].values():
            seen[
                json.dumps(representation, sort_keys=True, separators=(",", ":"))
            ] = representation
    return [seen[key] for key in sorted(seen)]


def assemble(
    settings: list[dict[str, Any]],
    canonical_version: str,
    releases: dict[str, dict[str, Any]],
    revision: int,
) -> dict[str, Any]:
    """Builds the publishable document from expanded per-release schemas."""
    schemas = {schema_id: schema for schema_id, schema in releases["schemas"].items()}
    table = _representation_table(schemas)
    index = {
        json.dumps(entry, sort_keys=True, separators=(",", ":")): position
        for position, entry in enumerate(table)
    }
    return {
        "format_version": FORMAT_VERSION,
        "revision": revision,
        "canonical_minecraft_version": canonical_version,
        "settings": settings,
        "representations": table,
        "schemas": [
            {
                "id": schema_id,
                "supported": {
                    setting_id: index[
                        json.dumps(representation, sort_keys=True, separators=(",", ":"))
                    ]
                    for setting_id, representation in sorted(schemas[schema_id]["supported"].items())
                },
                "unsupported": schemas[schema_id]["unsupported"],
            }
            for schema_id in sorted(schemas)
        ],
        "releases": [
            {"minecraft_version": version, "schema": schema_id}
            for version, schema_id in sorted(releases["mapping"].items(), key=lambda x: version_key(x[0]))
        ],
    }


def version_key(version: str) -> tuple[int, ...]:
    """Numeric ordering for a dotted release id, for stable release output."""
    parts = []
    for piece in version.split("."):
        parts.append(int(piece) if piece.isdigit() else 0)
    return tuple(parts)


def expand(document: dict[str, Any]) -> dict[str, dict[str, Any]]:
    """Turns a published document back into expanded per-family schemas."""
    table = document["representations"]
    schemas = {}
    for schema in document["schemas"]:
        schemas[schema["id"]] = {
            "supported": {
                setting_id: table[position] for setting_id, position in schema["supported"].items()
            },
            "unsupported": list(schema["unsupported"]),
            "extra_keys": [],
        }
    return schemas


def merge(
    previous: dict[str, Any] | None,
    analysed: list[dict[str, Any]],
) -> tuple[dict[str, Any], list[dict[str, str]]]:
    """Folds newly analysed releases into the committed dataset.

    Existing release mappings and family fingerprints are preserved untouched,
    so adding a release cannot rewrite what an earlier analysis proved.
    """
    changes: list[dict[str, str]] = []

    settings: list[dict[str, Any]] = list(previous["settings"]) if previous else []
    canonical_version = previous["canonical_minecraft_version"] if previous else None
    schemas: dict[str, dict[str, Any]] = expand(previous) if previous else {}
    mapping: dict[str, str] = (
        {entry["minecraft_version"]: entry["schema"] for entry in previous["releases"]}
        if previous
        else {}
    )

    analysed = sorted(analysed, key=lambda record: version_key(record["minecraft_version"]))
    if analysed:
        by_key = {entry["key"]: entry for entry in settings}
        taken = {entry["id"] for entry in settings}
        for entry in canonical_settings(analysed):
            established = by_key.get(entry["key"])
            if established is not None:
                # An id, once committed, is what every schema already refers to.
                # Only the description travels forward.
                established.update({field: entry[field] for field in entry if field != "id"})
                continue
            if entry["id"] in taken:
                raise DatasetError(f"canonical id `{entry['id']}` is already used by another key")
            by_key[entry["key"]] = dict(entry)
            taken.add(entry["id"])
            changes.append({"kind": "setting-added", "detail": entry["id"]})
        settings = sorted(by_key.values(), key=lambda entry: entry["id"])

        newest = analysed[-1]["minecraft_version"]
        if canonical_version is None or version_key(newest) > version_key(canonical_version):
            canonical_version = newest

    if not settings:
        raise DatasetError("no canonical release was analysed and no dataset was supplied")

    for record in analysed:
        version = record["minecraft_version"]
        schema = schema_for(record, settings)
        schema_id = fingerprint(schema)
        existing = mapping.get(version)
        if existing == schema_id:
            changes.append({"kind": "release-unchanged", "detail": f"{version} -> {schema_id}"})
            continue
        reuse = schema_id in schemas
        schemas[schema_id] = schema
        mapping[version] = schema_id
        changes.append(
            {
                "kind": "release-remapped" if existing else "release-added",
                "detail": f"{version} -> {schema_id}"
                + (" (existing schema)" if reuse else " (new schema)"),
            }
        )

    # A family nothing points at is dropped so the document does not accumulate
    # dead weight across regenerations.
    live = set(mapping.values())
    schemas = {schema_id: schema for schema_id, schema in schemas.items() if schema_id in live}

    revision = previous["revision"] if previous else 1
    document = assemble(settings, canonical_version, {"schemas": schemas, "mapping": mapping}, revision)
    if previous is not None and not equivalent(previous, document):
        document["revision"] = previous["revision"] + 1
    return document, changes


def equivalent(left: dict[str, Any], right: dict[str, Any]) -> bool:
    """Whether two documents say the same thing, ignoring the revision."""

    def body(document: dict[str, Any]) -> str:
        return json.dumps(
            {key: value for key, value in document.items() if key != "revision"},
            sort_keys=True,
            separators=(",", ":"),
        )

    return body(left) == body(right)


def validate(document: dict[str, Any]) -> None:
    """Checks a document is internally consistent before it is published."""
    if document.get("format_version") != FORMAT_VERSION:
        raise DatasetError(f"format version {document.get('format_version')} is not {FORMAT_VERSION}")
    if not isinstance(document.get("revision"), int) or document["revision"] < 1:
        raise DatasetError("revision must be a positive integer")

    ids = {entry["id"] for entry in document["settings"]}
    if len(ids) != len(document["settings"]):
        raise DatasetError("duplicate canonical setting id")
    missing = [required for required in REQUIRED_CANONICAL_IDS if required not in ids]
    if missing:
        raise DatasetError(f"canonical settings missing from the dataset: {', '.join(missing)}")

    table = document["representations"]
    for schema in document["schemas"]:
        if fingerprint(
            {
                "supported": {
                    setting_id: table[position]
                    for setting_id, position in schema["supported"].items()
                },
                "unsupported": schema["unsupported"],
            }
        ) != schema["id"]:
            raise DatasetError(f"schema `{schema['id']}` does not match its own content")
        for setting_id, position in schema["supported"].items():
            if setting_id not in ids:
                raise DatasetError(f"schema `{schema['id']}` names unknown setting `{setting_id}`")
            if not 0 <= position < len(table):
                raise DatasetError(f"schema `{schema['id']}` has an out-of-range representation")
        overlap = set(schema["supported"]) & set(schema["unsupported"])
        if overlap:
            raise DatasetError(f"schema `{schema['id']}` is both supported and unsupported for {overlap}")

    known_schemas = {schema["id"] for schema in document["schemas"]}
    seen_versions = set()
    for release in document["releases"]:
        version = release["minecraft_version"]
        if version in seen_versions:
            raise DatasetError(f"release `{version}` is mapped twice")
        seen_versions.add(version)
        if release["schema"] not in known_schemas:
            raise DatasetError(f"release `{version}` names an unknown schema")

    canonical = document["canonical_minecraft_version"]
    if canonical not in seen_versions:
        raise DatasetError("the canonical release is not itself mapped")


def dumps(document: dict[str, Any]) -> str:
    """The exact on-disk and on-feed text: stable, sorted, newline terminated."""
    return json.dumps(document, indent=2, sort_keys=True, ensure_ascii=True) + "\n"
