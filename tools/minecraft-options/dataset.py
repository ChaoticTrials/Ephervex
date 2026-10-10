#!/usr/bin/env python3
"""Turn analyzer output into Ephervex's options compatibility dataset.

The dataset is the single generated source of truth for "how does this
Minecraft release store this setting in options.txt". It ships bundled with the
application and is also the exact document published to the remote options
feed, so one builder produces both and the two can never disagree in shape.

Four things are kept apart on purpose:

* **setting registry** - cumulative stable ids required by every historical
  schema, independent of any release's obfuscated field names;
* **current canonical setting ids** - only the settings present in the newest
  supported release;
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

# The options file's data-version marker line. Only the registry's own marker
# setting may be stored under it; every other setting would overwrite it.
VERSION_KEY = "version"
VERSION_MARKER_SETTING_ID = "version"

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
    if representation_issue(setting, code_space) is not None:
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


def representation_issue(setting: dict[str, Any], code_space: str | None) -> str | None:
    """Why this extraction cannot be represented authoritatively, if any."""
    if setting.get("confidence") != "high" or setting.get("type_confidence") != "high":
        return "the analyzer could not establish its codec"

    kind = setting.get("type")
    if kind == "key_binding" and code_space not in ("lwjgl2", "glfw"):
        return "the key binding code space is unknown"
    if kind in ("integer", "number"):
        encoding = setting.get("file_encoding") or ""
        if encoding != "decimal integer" and not encoding.startswith("decimal floating point"):
            return "the numeric file encoding is unknown"
        transform = setting.get("value_to_storage")
        if transform and not (_AFFINE.match(transform) or _OFFSET.match(transform)):
            return "unrecognized storage transform"
        # A codec that transforms the value on its way to the file, through
        # arithmetic the analyzer could not read, is not a setting stored
        # as-is. Treating an unread transform as no transform is how a value
        # gets written through the wrong arithmetic.
        if setting.get("transform_resolved") is False:
            return "the storage transform could not be read"
        if transform and (match := _AFFINE.match(transform)) and _number(match.group(2)) == 0:
            return "storage transform has a zero scale"
        contradiction = _default_domain_contradiction(setting)
        if contradiction:
            return contradiction
    if kind == "enum":
        allowed = setting.get("allowed_values") or {}
        if setting.get("domain_confidence") != "high" or not allowed.get("values"):
            return "the enum domain is unknown"
        if allowed.get("persisted_form") not in ("integer_id", "string_token"):
            return "the enum persisted form is unknown"
    if kind == "special" or kind not in (
        "boolean", "string", "key_binding", "integer", "number", "enum"
    ):
        return "the persisted type is not supported by this build"
    return None


def _default_domain_contradiction(setting: dict[str, Any]) -> str | None:
    """Reject mutually impossible high-confidence direct-persistence evidence."""
    if (
        setting.get("default_confidence") != "high"
        or setting.get("domain_confidence") != "high"
        or setting.get("value_to_storage")
        or not isinstance(setting.get("default"), (int, float))
        or isinstance(setting.get("default"), bool)
    ):
        return None
    numeric = setting.get("numeric") or {}
    default = setting["default"]
    minimum, maximum = numeric.get("min"), numeric.get("max")
    if minimum is not None and default < minimum or maximum is not None and default > maximum:
        return f"contradictory evidence: default {default} is outside persisted domain {minimum}..{maximum}"
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
        if len(tokens) != len(values):
            return None
        # A release that writes its options as JSON puts a token in quotes.
        # Which releases do is a fact about each one's persistence path, so it
        # travels with the codec rather than being inferred downstream.
        return {"kind": "enum_token", "tokens": tokens,
                "quoted": setting.get("file_quoting") == "json_string"}
    return None


# ------------------------------------------------------------------- building


def registry_settings(records: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Stable registry definitions across every analyzed release.

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
    setting_registry: list[dict[str, Any]],
    current_canonical_setting_ids: list[str],
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
        "setting_registry": setting_registry,
        "current_canonical_setting_ids": current_canonical_setting_ids,
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
            {"minecraft_version": version, "schema": schema_id,
             **({"data_version": releases["data_versions"][version]}
                if releases.get("data_versions", {}).get(version) is not None else {})}
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

    setting_registry: list[dict[str, Any]] = (
        list(previous.get("setting_registry", previous.get("settings", []))) if previous else []
    )
    current_canonical_setting_ids: list[str] = (
        list(previous.get("current_canonical_setting_ids", [])) if previous else []
    )
    canonical_version = previous["canonical_minecraft_version"] if previous else None
    schemas: dict[str, dict[str, Any]] = expand(previous) if previous else {}
    mapping: dict[str, str] = (
        {entry["minecraft_version"]: entry["schema"] for entry in previous["releases"]}
        if previous
        else {}
    )
    data_versions: dict[str, int] = (
        {entry["minecraft_version"]: entry["data_version"] for entry in previous["releases"]
         if entry.get("data_version") is not None}
        if previous
        else {}
    )

    analysed = sorted(analysed, key=lambda record: version_key(record["minecraft_version"]))
    if analysed:
        newest = analysed[-1]["minecraft_version"]
        refresh_current_definitions = (
            canonical_version is None
            or version_key(newest) >= version_key(canonical_version)
        )
        by_key = {entry["key"]: entry for entry in setting_registry}
        taken = {entry["id"] for entry in setting_registry}
        for entry in registry_settings(analysed):
            established = by_key.get(entry["key"])
            if established is not None:
                # An id, once committed, is what every schema already refers to.
                # Descriptions travel only from the canonical release forward;
                # re-analyzing history must not make old metadata current.
                if refresh_current_definitions:
                    established.update({field: entry[field] for field in entry if field != "id"})
                continue
            if entry["id"] in taken:
                raise DatasetError(f"canonical id `{entry['id']}` is already used by another key")
            by_key[entry["key"]] = dict(entry)
            taken.add(entry["id"])
            changes.append({"kind": "setting-added", "detail": entry["id"]})
        setting_registry = sorted(by_key.values(), key=lambda entry: entry["id"])

        if canonical_version is None or version_key(newest) > version_key(canonical_version):
            canonical_version = newest
            current_keys = {setting["persisted_key"] for setting in analysed[-1]["settings"]}
            current_canonical_setting_ids = sorted(
                entry["id"] for entry in setting_registry if entry["key"] in current_keys
            )
        elif newest == canonical_version:
            current_keys = {setting["persisted_key"] for setting in analysed[-1]["settings"]}
            current_canonical_setting_ids = sorted(
                entry["id"] for entry in setting_registry if entry["key"] in current_keys
            )

    if not setting_registry:
        raise DatasetError("no canonical release was analysed and no dataset was supplied")
    if not current_canonical_setting_ids:
        # Migration from the unpublished original shape. The selected canonical
        # release supplies the real current set on the first normal regeneration.
        current_canonical_setting_ids = sorted(entry["id"] for entry in setting_registry)

    for record in analysed:
        version = record["minecraft_version"]
        if record.get("data_version") is not None:
            data_versions[version] = record["data_version"]
        schema = schema_for(record, setting_registry)
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
    data_versions = {version: value for version, value in data_versions.items() if version in mapping}

    revision = previous["revision"] if previous else 1
    document = assemble(
        setting_registry,
        current_canonical_setting_ids,
        canonical_version,
        {"schemas": schemas, "mapping": mapping, "data_versions": data_versions},
        revision,
    )
    unpublished_shape_migration = previous is not None and "setting_registry" not in previous
    if previous is not None and not unpublished_shape_migration and not equivalent(previous, document):
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

    registry = document["setting_registry"]
    ids = {entry["id"] for entry in registry}
    if len(ids) != len(registry):
        raise DatasetError("duplicate setting registry id")
    current = document["current_canonical_setting_ids"]
    if len(set(current)) != len(current):
        raise DatasetError("duplicate current canonical setting id")
    unknown_current = sorted(set(current) - ids)
    if unknown_current:
        raise DatasetError(
            f"current canonical settings are absent from the registry: {', '.join(unknown_current)}"
        )
    missing = [required for required in REQUIRED_CANONICAL_IDS if required not in current]
    if missing:
        raise DatasetError(f"current canonical settings missing from the dataset: {', '.join(missing)}")

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
        writers: dict[str, str] = {}
        for setting_id, position in schema["supported"].items():
            if setting_id not in ids:
                raise DatasetError(f"schema `{schema['id']}` names unknown setting `{setting_id}`")
            if not 0 <= position < len(table):
                raise DatasetError(f"schema `{schema['id']}` has an out-of-range representation")
            key = table[position]["key"]
            if key == VERSION_KEY and setting_id != VERSION_MARKER_SETTING_ID:
                raise DatasetError(
                    f"schema `{schema['id']}` stores `{setting_id}` under the reserved `{VERSION_KEY}` key"
                )
            if key in writers:
                raise DatasetError(
                    f"schema `{schema['id']}` stores both `{writers[key]}` and `{setting_id}` under the key `{key}`"
                )
            writers[key] = setting_id
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
