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

import copy
import hashlib
import json
import math
import re
import unicodedata
from typing import Any

from release_identity import is_release, version_key

# Bumped only when the document's own shape changes. An application that reads
# a higher version must refuse the document rather than guess at it.
FORMAT_VERSION = 1

# The options file's data-version marker line. Only the registry's own marker
# setting may be stored under it; every other setting would overwrite it.
VERSION_KEY = "version"
VERSION_MARKER_SETTING_ID = "version"
VERSION_MARKER_CODEC = {"kind": "integer", "min": None, "max": None}

# The canonical setting ids Ephervex's own preference model is built on.
# `validate` refuses a dataset whose canonical release does not support every
# one of them, because that would ship a build whose settings screen offers
# something the data cannot express.
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

# `((value - K) / S)` and `(value - K)`: the only storage transforms the
# generator types. The field of view is `((value - 70) / 40)` in every release.
# Parsed into typed parameters rather than carried as text.
# A constant is digits with at most one decimal point, so `float()` can read it.
_CONSTANT = r"-?(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+)"
_AFFINE = re.compile(rf"^\(\(value - ({_CONSTANT})\) / ({_CONSTANT})\)$")
_OFFSET = re.compile(rf"^\(value - ({_CONSTANT})\)$")

# How the analyzer describes a setting stored as a Gson array on one line.
_JSON_ARRAY_LINE = "JSON array"


class DatasetError(Exception):
    """The inputs cannot produce a publishable dataset."""


# --------------------------------------------------------------------- codecs


def _number(text: str) -> float:
    try:
        value = float(text)
    except ValueError:
        raise DatasetError(f"`{text}` is not a number") from None
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


# Code spaces whose releases persist a binding as the name the game gives its key
# (`key.keyboard.w`); the numbering the defaults are written in does not reach the file.
NAMED_KEY_CODE_SPACES = ("glfw", "hid")


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
        if code_space in NAMED_KEY_CODE_SPACES:
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
    if kind == "key_binding" and code_space not in ("lwjgl2", *NAMED_KEY_CODE_SPACES):
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
    if kind == "string":
        # `text` is raw text after the first colon. A release that prints the
        # value as a JSON string writes it inside quotes, and a Gson array
        # line is structured data; neither is that, and a published `text`
        # entry would be written back unquoted or as free text.
        if setting.get("file_quoting") == "json_string":
            return "the value is written as a JSON string, which `text` cannot express"
        if (setting.get("file_encoding") or "").startswith(_JSON_ARRAY_LINE):
            return "the value is a JSON array line, not free text"
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
    # holds in memory: writing `70` where the game writes `70.0` would be a
    # gratuitous difference from the file it maintains itself.
    encoding = setting.get("file_encoding") or ""
    if encoding == "decimal integer":
        written = "integer"
    elif encoding.startswith("decimal floating point"):
        written = "decimal"
    else:
        return None

    transform = setting.get("value_to_storage")
    if transform:
        # `scaled_decimal` is always written as a decimal, so a transformed
        # integer would gain a `.0` the game does not write.
        if written == "integer":
            return None
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


def registry_settings(
    records: list[dict[str, Any]], canonical: dict[str, Any] | None
) -> list[dict[str, Any]]:
    """Stable registry definitions across the given analyzed releases.

    Ids come from `canonical`, the release the dataset treats as current. It is
    deobfuscated and therefore has real field names. A key that `canonical` does
    not have keeps its persisted key as its id, because an obfuscated field name
    is not a name. When `canonical` is `None` the batch does not reach the
    current release, so no record in it may name a setting. Older releases are
    matched to these definitions by persisted key, which the research shows has
    never been renamed across the supported range.

    `records` must be ordered oldest first: a later release's definition wins,
    so the newest description of a setting is the one carried.
    """
    named = {setting["persisted_key"]: setting for setting in canonical["settings"]} if canonical else {}
    definitions: dict[str, dict[str, Any]] = {}
    for record in records:
        for setting in record["settings"]:
            key = setting["persisted_key"]
            source = named.get(key)
            definitions[key] = {
                "id": source["logical_id"] if source else key,
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
    marker_written = record.get("data_version") is not None
    for entry in settings:
        if entry["id"] == VERSION_MARKER_SETTING_ID and marker_written:
            # The stamp comes from version data, not the options population.
            supported[entry["id"]] = {"key": VERSION_KEY, "codec": dict(VERSION_MARKER_CODEC)}
            continue
        setting = extracted.get(entry["key"])
        if setting is None:
            if entry["id"] == VERSION_MARKER_SETTING_ID:
                # A complete extraction does not cover the stamp, so its absence proves nothing.
                continue
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

    A previous dataset or analyzer record that lacks the structure the fold
    reads is refused with a `DatasetError`, never a lookup traceback.
    """
    try:
        return _merge(previous, analysed)
    except (KeyError, IndexError, TypeError, AttributeError, ValueError) as error:
        raise DatasetError(
            f"the dataset or analyzer records are malformed ({type(error).__name__}: {error})"
        ) from error


def _merge(
    previous: dict[str, Any] | None,
    analysed: list[dict[str, Any]],
) -> tuple[dict[str, Any], list[dict[str, str]]]:
    """The fold behind `merge`, which assumes well-formed structure.

    `previous` is never modified. Setting ids and release families that an
    earlier analysis established are preserved, so adding or re-folding a
    release cannot rewrite what an earlier analysis proved. Folding the full
    history together with a new release gives the same result as folding only
    the new release. A release first folded after the registry gained a key is
    judged against that key, so separate folds of brand-new releases can differ
    when a setting is registered between them.
    """
    changes: list[dict[str, str]] = []

    by_version: dict[str, dict[str, Any]] = {}
    for record in analysed:
        version = record.get("minecraft_version")
        if not is_release(version):
            raise DatasetError(f"`{version}` is not a release identifier")
        if by_version.setdefault(version, record) != record:
            raise DatasetError(f"release `{version}` was analyzed more than once with different results")
    analysed = list(by_version.values())

    setting_registry: list[dict[str, Any]] = (
        copy.deepcopy(previous.get("setting_registry", previous.get("settings", [])))
        if previous
        else []
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
        # Only a batch that reaches the committed canonical release may name a
        # new setting; an older release has no authority over ids.
        canonical_record = analysed[-1] if refresh_current_definitions else None
        for entry in registry_settings(analysed, canonical_record):
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
        # The number belongs to this analysis of the release: one that could not
        # read it must not keep an earlier analysis's value.
        if record.get("data_version") is not None:
            data_versions[version] = record["data_version"]
        else:
            data_versions.pop(version, None)
        schema = schema_for(record, setting_registry)
        existing = mapping.get(version)
        if existing in schemas and _gained_only_later_settings(schemas[existing], schema):
            changes.append({"kind": "release-unchanged", "detail": f"{version} -> {existing}"})
            continue
        schema_id = fingerprint(schema)
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


def _gained_only_later_settings(existing: dict[str, Any], fresh: dict[str, Any]) -> bool:
    """Whether `fresh` differs from `existing` only by settings registered later.

    A key absent from a completely extracted release is provably unsupported,
    but only against the registry that existed when the release was analyzed.
    A setting registered afterwards is unknown to that analysis and stays
    unknown, so it must not move the release to another family.
    """
    return fresh["supported"] == existing["supported"] and set(existing["unsupported"]) <= set(
        fresh["unsupported"]
    )


def equivalent(left: dict[str, Any], right: dict[str, Any]) -> bool:
    """Whether two documents say the same thing, ignoring the revision."""

    def body(document: dict[str, Any]) -> str:
        return json.dumps(
            {key: value for key, value in document.items() if key != "revision"},
            sort_keys=True,
            separators=(",", ":"),
        )

    return body(left) == body(right)


_U32_MAX = 2**32 - 1
_I64_MIN, _I64_MAX = -(2**63), 2**63 - 1

# Codec kinds that carry no parameters.
_PLAIN_CODECS = frozenset(
    ("boolean", "text", "language_code", "key_code_lwjgl2", "key_name_glfw")
)


def _is_int(value: Any) -> bool:
    return isinstance(value, int) and not isinstance(value, bool)


def _is_i64(value: Any) -> bool:
    return _is_int(value) and _I64_MIN <= value <= _I64_MAX


def _is_finite_number(value: Any) -> bool:
    if _is_int(value):
        try:
            return math.isfinite(float(value))
        except OverflowError:
            return False
    return isinstance(value, float) and math.isfinite(value)


def _is_writable(text: Any) -> bool:
    return isinstance(text, str) and all(unicodedata.category(char) != "Cc" for char in text)


def _is_valid_key(key: Any) -> bool:
    """Mirrors `is_valid_key` in `crates/options/src/file.rs`."""
    return isinstance(key, str) and bool(key) and ":" not in key and _is_writable(key)


def _bounds_problem(codec: dict[str, Any], is_bound: Any, noun: str) -> str | None:
    low, high = codec.get("min"), codec.get("max")
    if any(bound is not None and not is_bound(bound) for bound in (low, high)):
        return f"has a bound that is not {noun}"
    if low is not None and high is not None and low > high:
        return "has a minimum above its maximum"
    return None


def _codec_problem(codec: Any) -> str | None:
    """Why the application would refuse this codec, mirroring `is_well_formed`
    in `crates/options/src/codec.rs` and the shape its deserializer requires."""
    if not isinstance(codec, dict):
        return "is not an object"
    kind = codec.get("kind")
    if not isinstance(kind, str):
        return "has no kind"
    if kind in _PLAIN_CODECS:
        return None
    if kind == "integer":
        return _bounds_problem(codec, _is_i64, "a 64-bit integer")
    if kind == "decimal":
        return _bounds_problem(codec, _is_finite_number, "a finite number")
    if kind == "scaled_decimal":
        offset, scale = codec.get("offset"), codec.get("scale")
        if not (_is_finite_number(offset) and _is_finite_number(scale)):
            return "needs a finite offset and scale"
        if scale == 0:
            return "has a zero scale"
        return _bounds_problem(codec, _is_finite_number, "a finite number")
    if kind == "enum_id":
        values = codec.get("values")
        if not isinstance(values, list) or not values:
            return "has no values"
        if not all(_is_i64(value) for value in values):
            return "has a value that is not a 64-bit integer"
        return None
    if kind == "enum_token":
        tokens = codec.get("tokens")
        if not isinstance(tokens, list) or not tokens:
            return "has no tokens"
        if not all(_is_writable(token) for token in tokens):
            return "has a token that cannot be written as one options line"
        if not isinstance(codec.get("quoted", False), bool):
            return "has a quoting flag that is not a boolean"
        return None
    return f"has the unknown kind `{kind}`"


def validate(document: Any) -> None:
    """Checks a document is internally consistent before it is published.

    A document whose structure is missing or has the wrong type is refused with
    a `DatasetError` like any other inconsistency, never a lookup traceback.
    """
    if not isinstance(document, dict):
        raise DatasetError("the document is not a JSON object")
    try:
        _validate(document)
    except (KeyError, IndexError, TypeError, AttributeError) as error:
        raise DatasetError(f"the document is malformed ({type(error).__name__}: {error})") from error


def _check_field_types(document: dict[str, Any]) -> None:
    """Refuses the field types the application's loader deserializes into.

    Mirrors the serde structs in `crates/options/src/dataset.rs`: an absent
    required field is a `KeyError` that `validate` reports as malformed, and an
    optional field may be absent or null.
    """
    if not isinstance(document["canonical_minecraft_version"], str):
        raise DatasetError("the canonical release is not a string")
    for name in ("setting_registry", "current_canonical_setting_ids", "representations", "schemas", "releases"):
        if not isinstance(document[name], list):
            raise DatasetError(f"`{name}` is not a list")
    for entry in document["setting_registry"]:
        if not isinstance(entry, dict):
            raise DatasetError("a setting registry entry is not an object")
        for field in ("id", "key", "type"):
            if not isinstance(entry[field], str):
                raise DatasetError(f"a setting registry entry has a `{field}` that is not a string")
        setting_id = entry["id"]
        for field in ("category", "translation_key"):
            if entry.get(field) is not None and not isinstance(entry[field], str):
                raise DatasetError(f"setting `{setting_id}` has a `{field}` that is not a string")
        if not isinstance(entry["manageable"], bool):
            raise DatasetError(f"setting `{setting_id}` has a `manageable` flag that is not a boolean")
    if not all(isinstance(setting_id, str) for setting_id in document["current_canonical_setting_ids"]):
        raise DatasetError("a current canonical setting id is not a string")
    for schema in document["schemas"]:
        if not isinstance(schema, dict):
            raise DatasetError("a schema is not an object")
        if not isinstance(schema["id"], str):
            raise DatasetError("a schema has an id that is not a string")
        supported = schema["supported"]
        if not isinstance(supported, dict) or not all(isinstance(key, str) for key in supported):
            raise DatasetError(f"schema `{schema['id']}` has a supported map that is not keyed by setting id")
    for release in document["releases"]:
        if not isinstance(release, dict):
            raise DatasetError("a release is not an object")
        for field in ("minecraft_version", "schema"):
            if not isinstance(release[field], str):
                raise DatasetError(f"a release has a `{field}` that is not a string")


def _validate(document: dict[str, Any]) -> None:
    """The consistency checks behind `validate`, which assume the document's structure.

    This is the only gate between the generator and the published document, so
    it refuses exactly what the application's loader refuses
    (`crates/options/src/dataset.rs`): a document the application would reject
    wholesale must never reach the bundled file or the feed. It also refuses a
    canonical release that cannot describe every setting Ephervex's preference
    model needs.
    """
    if document.get("format_version") != FORMAT_VERSION:
        raise DatasetError(f"format version {document.get('format_version')} is not {FORMAT_VERSION}")
    revision = document.get("revision")
    if not _is_int(revision) or not 1 <= revision <= _U32_MAX:
        raise DatasetError("revision must be a positive 32-bit integer")

    _check_field_types(document)

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
    for representation in table:
        key = representation.get("key") if isinstance(representation, dict) else None
        if not _is_valid_key(key):
            raise DatasetError(f"{key!r} is not usable as an options.txt key")
        problem = _codec_problem(representation.get("codec"))
        if problem is not None:
            raise DatasetError(f"the codec for `{key}` {problem}")

    families: dict[str, dict[str, Any]] = {}
    for schema in document["schemas"]:
        schema_id = schema["id"]
        if schema_id in families:
            raise DatasetError(f"schema `{schema_id}` is declared twice")
        families[schema_id] = schema
        for setting_id, position in schema["supported"].items():
            if setting_id not in ids:
                raise DatasetError(f"schema `{schema_id}` names unknown setting `{setting_id}`")
            if not _is_int(position) or not 0 <= position < len(table):
                raise DatasetError(f"schema `{schema_id}` has an out-of-range representation")
        unsupported = schema["unsupported"]
        if not isinstance(unsupported, list) or not all(isinstance(entry, str) for entry in unsupported):
            raise DatasetError(f"schema `{schema_id}` has an unsupported list that is not a list of ids")
        if len(set(unsupported)) != len(unsupported):
            raise DatasetError(f"schema `{schema_id}` lists an unsupported setting twice")
        for setting_id in unsupported:
            if setting_id not in ids:
                raise DatasetError(f"schema `{schema_id}` names unknown unsupported setting `{setting_id}`")
        if fingerprint(
            {
                "supported": {
                    setting_id: table[position]
                    for setting_id, position in schema["supported"].items()
                },
                "unsupported": unsupported,
            }
        ) != schema_id:
            raise DatasetError(f"schema `{schema_id}` does not match its own content")
        writers: dict[str, str] = {}
        for setting_id, position in schema["supported"].items():
            key = table[position]["key"]
            if key == VERSION_KEY and setting_id != VERSION_MARKER_SETTING_ID:
                raise DatasetError(
                    f"schema `{schema_id}` stores `{setting_id}` under the reserved `{VERSION_KEY}` key"
                )
            if key in writers:
                raise DatasetError(
                    f"schema `{schema_id}` stores both `{writers[key]}` and `{setting_id}` under the key `{key}`"
                )
            writers[key] = setting_id
        overlap = set(schema["supported"]) & set(unsupported)
        if overlap:
            raise DatasetError(f"schema `{schema_id}` is both supported and unsupported for {overlap}")

    mapped: dict[str, str] = {}
    for release in document["releases"]:
        version = release["minecraft_version"]
        if version in mapped:
            raise DatasetError(f"release `{version}` is mapped twice")
        if release["schema"] not in families:
            raise DatasetError(f"release `{version}` names an unknown schema")
        data_version = release.get("data_version")
        if data_version is not None and not _is_i64(data_version):
            raise DatasetError(f"release `{version}` has a data version that is not a 64-bit integer")
        if data_version is not None and VERSION_MARKER_SETTING_ID in families[release["schema"]]["unsupported"]:
            raise DatasetError(f"release `{version}` has a data version but its schema lists the marker as unsupported")
        mapped[version] = release["schema"]

    canonical = document["canonical_minecraft_version"]
    if canonical not in mapped:
        raise DatasetError("the canonical release is not itself mapped")

    canonical_schema = families[mapped[canonical]]
    unwritable = [
        f"{required} ({'absent from the release' if required in canonical_schema['unsupported'] else 'not established'})"
        for required in REQUIRED_CANONICAL_IDS
        if required not in canonical_schema["supported"]
    ]
    if unwritable:
        raise DatasetError(
            f"the canonical release `{canonical}` cannot encode required settings: {', '.join(unwritable)}"
        )


def dumps(document: dict[str, Any]) -> str:
    """The exact on-disk and on-feed text: stable, sorted, newline terminated."""
    return json.dumps(document, indent=2, sort_keys=True, ensure_ascii=True) + "\n"
