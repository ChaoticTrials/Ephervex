#!/usr/bin/env python3
"""Tests for the options compatibility dataset builder.

    python3 -m unittest discover -s tools/minecraft-options

Offline and deterministic: every input is constructed here, so nothing depends
on Mojang, on a client JAR, or on `javap`.
"""

from __future__ import annotations

import copy
import unittest

import dataset


def setting(
    key: str,
    logical_id: str | None = None,
    *,
    kind: str = "boolean",
    confidence: str = "high",
    numeric: dict | None = None,
    domain_confidence: str = "unknown",
    transform: str | None = None,
    allowed: dict | None = None,
    encoding: str | None = None,
    default: int | float | None = None,
    default_confidence: str = "unknown",
    quoting: str | None = None,
) -> dict:
    written = {
        "boolean": "lowercase true/false",
        "integer": "decimal integer",
        "number": "decimal floating point",
        "string": "UTF-8 text after the first colon",
        "key_binding": "InputConstants key name string (for example key.keyboard.w)",
        "enum": "enum id integer",
    }
    record = {
        "file_encoding": encoding or written.get(kind, ""),
        "persisted_key": key,
        "logical_id": logical_id or key,
        "type": kind,
        "category": "Video",
        "translation_key": f"options.{key}",
        "ephervex_manageable": True,
        "confidence": confidence,
        "type_confidence": confidence,
        "domain_confidence": domain_confidence,
        "numeric": numeric,
        "value_to_storage": transform,
        "allowed_values": allowed,
        "default": default,
        "default_confidence": default_confidence,
    }
    if quoting is not None:
        record["file_quoting"] = quoting
    return record


REQUIRED = [
    setting("guiScale", "guiScale", kind="integer"),
    setting("lang", "languageCode", kind="string"),
    setting("fullscreen"),
    setting("overrideWidth", kind="integer"),
    setting("overrideHeight", kind="integer"),
    setting("fov", kind="integer"),
    setting("renderDistance", kind="integer"),
    setting("simulationDistance", kind="integer"),
]


def release(version: str, settings: list[dict], code_space: str = "glfw",
            data_version: int | None = None) -> dict:
    record = {
        "minecraft_version": version,
        "extraction_complete": True,
        "keybinding_code_space": code_space,
        "settings": copy.deepcopy(settings),
    }
    if data_version is not None:
        record["data_version"] = data_version
    return record


class CodecDerivation(unittest.TestCase):
    def test_each_recovered_representation_becomes_one_typed_codec(self):
        cases = [
            (setting("a"), "glfw", {"kind": "boolean"}),
            (setting("lang", kind="string"), "glfw", {"kind": "language_code"}),
            (setting("soundDevice", kind="string", quoting="raw"), "glfw", {"kind": "text"}),
            (setting("key_key.attack", kind="key_binding"), "glfw", {"kind": "key_name_glfw"}),
            (setting("key_key.attack", kind="key_binding"), "lwjgl2", {"kind": "key_code_lwjgl2"}),
            (
                setting("renderDistance", kind="integer", numeric={"min": 2, "max": 32}, domain_confidence="high"),
                "glfw",
                {"kind": "integer", "min": 2, "max": 32},
            ),
            (
                setting("chatOpacity", kind="number"),
                "glfw",
                {"kind": "decimal", "min": None, "max": None},
            ),
        ]
        for extracted, code_space, expected in cases:
            self.assertEqual(dataset.codec_for(extracted, code_space), expected, extracted["persisted_key"])

    def test_the_legacy_field_of_view_transform_becomes_typed_parameters(self):
        codec = dataset.codec_for(
            setting("fov", kind="number", transform="((value - 70) / 40)"), "glfw"
        )
        self.assertEqual(
            codec, {"kind": "scaled_decimal", "offset": 70, "scale": 40, "min": None, "max": None}
        )

    def test_a_transform_on_an_integer_the_game_writes_whole_leaves_the_setting_unknown(self):
        # `scaled_decimal` is always written with a decimal point, so a
        # transformed integer would be written as `2.0` where the game writes `2`.
        for expression in ["(value - 1)", "((value - 70) / 40)"]:
            self.assertIsNone(
                dataset.codec_for(setting("level", kind="integer", transform=expression), "glfw"),
                expression,
            )

    def test_a_transform_the_analyzer_could_not_read_is_not_no_transform(self):
        # A codec that changes the value on its way to the file, through
        # arithmetic nobody could read, leaves the setting unknown. Reading it
        # as stored-as-is is how a value goes into the file through the wrong
        # arithmetic, which is exactly what a scaled field of view did.
        unread = setting("fov", kind="number")
        unread["transform_resolved"] = False
        self.assertIsNone(dataset.codec_for(unread, "glfw"))
        self.assertEqual(
            dataset.representation_issue(unread, "glfw"),
            "the storage transform could not be read",
        )
        # A setting with no transform at all is still stored as it stands.
        plain = setting("fov", kind="number")
        plain["transform_resolved"] = None
        self.assertEqual(dataset.codec_for(plain, "glfw"), {"kind": "decimal", "min": None, "max": None})

    def test_a_transform_the_generator_cannot_type_leaves_the_setting_unknown(self):
        # The alternative would be carrying an expression into the dataset, and
        # from there into something that has to evaluate it.
        for expression in ["Math.pow(value, 2)", "(value * 3) + sin(x)", "((value - 70) / 0)"]:
            self.assertIsNone(
                dataset.codec_for(setting("fov", kind="number", transform=expression), "glfw"),
                expression,
            )

    def test_an_enum_whose_persisted_form_is_unresolved_stays_unknown(self):
        unresolved = setting(
            "renderClouds",
            kind="enum",
            domain_confidence="high",
            allowed={"persisted_form": "unknown", "values": [{"persisted": None}]},
        )
        self.assertIsNone(dataset.codec_for(unresolved, "glfw"))

    def test_a_resolved_enum_keeps_only_its_persisted_domain(self):
        by_id = setting(
            "attackIndicator",
            kind="enum",
            domain_confidence="high",
            allowed={"persisted_form": "integer_id", "values": [{"persisted": 2}, {"persisted": 0}, {"persisted": 1}]},
        )
        by_token = setting(
            "mainHand",
            kind="enum",
            domain_confidence="high",
            allowed={"persisted_form": "string_token", "values": [{"persisted": "right"}, {"persisted": "left"}]},
        )
        self.assertEqual(dataset.codec_for(by_id, "glfw"), {"kind": "enum_id", "values": [0, 1, 2]})
        self.assertEqual(
            dataset.codec_for(by_token, "glfw"),
            {"kind": "enum_token", "tokens": ["left", "right"], "quoted": False},
        )
        # The same enum on a release that writes its options as JSON arrives in
        # the file inside quotes, which is a different representation of the
        # same domain rather than a different domain.
        quoted = setting(
            "mainHand",
            kind="enum",
            domain_confidence="high",
            allowed={"persisted_form": "string_token",
                     "values": [{"persisted": "right"}, {"persisted": "left"}]},
        )
        quoted["file_quoting"] = "json_string"
        self.assertEqual(
            dataset.codec_for(quoted, "glfw"),
            {"kind": "enum_token", "tokens": ["left", "right"], "quoted": True},
        )

    def test_a_string_the_release_writes_as_a_json_string_is_not_the_raw_text_codec(self):
        raw = setting("soundDevice", kind="string", quoting="raw")
        quoted = setting("soundDevice", kind="string", quoting="json_string")
        self.assertEqual(dataset.codec_for(raw, "glfw"), {"kind": "text"})
        self.assertIsNone(dataset.codec_for(quoted, "glfw"))
        self.assertIn("JSON string", dataset.representation_issue(quoted, "glfw"))

    def test_a_gson_array_line_typed_as_a_string_is_never_free_text(self):
        for key in ("resourcePacks", "incompatibleResourcePacks"):
            extracted = setting(
                key,
                kind="string",
                quoting="raw",
                encoding="JSON array of pack identifiers written on one line by Gson",
            )
            self.assertIsNone(dataset.codec_for(extracted, "glfw"), key)

    def test_a_release_whose_quoting_differs_is_a_different_family_from_a_raw_one(self):
        # Only the raw release keeps a representation; the quoted one is unknown
        # rather than sharing it.
        registry = [{"id": "soundDevice", "key": "soundDevice", "type": "string"}]
        families = []
        for quoting in ("raw", "json_string"):
            record = release("1.19", [setting("soundDevice", kind="string", quoting=quoting)])
            schema = dataset.schema_for(record, registry)
            families.append((schema["supported"], dataset.fingerprint(schema)))
        self.assertEqual(families[0][0], {"soundDevice": {"key": "soundDevice", "codec": {"kind": "text"}}})
        self.assertEqual(families[1][0], {})
        self.assertNotEqual(families[0][1], families[1][1])

    def test_a_low_confidence_extraction_is_never_promoted_to_a_representation(self):
        self.assertIsNone(dataset.codec_for(setting("a", confidence="medium"), "glfw"))

    def test_a_high_confidence_default_outside_a_direct_persisted_domain_is_contradictory(self):
        contradictory = setting(
            "simulationDistance",
            kind="integer",
            numeric={"min": 2, "max": 5},
            domain_confidence="high",
            default=12,
            default_confidence="high",
        )
        self.assertIsNone(dataset.codec_for(contradictory, "glfw"))
        self.assertIn("contradictory evidence", dataset.representation_issue(contradictory, "glfw"))


class Fingerprints(unittest.TestCase):
    def test_identical_content_yields_one_family_and_different_content_does_not(self):
        base = release("1.20.1", REQUIRED)
        same = release("1.20.2", REQUIRED)
        document, _ = dataset.merge(None, [base, same])
        mapping = {entry["minecraft_version"]: entry["schema"] for entry in document["releases"]}
        self.assertEqual(mapping["1.20.1"], mapping["1.20.2"])
        self.assertEqual(len(document["schemas"]), 1)

    def test_a_patch_release_is_not_assumed_equivalent_to_its_predecessor(self):
        widened = copy.deepcopy(REQUIRED)
        widened[6] = setting(
            "renderDistance", kind="integer", numeric={"min": 2, "max": 32}, domain_confidence="high"
        )
        document, _ = dataset.merge(None, [release("1.20.1", REQUIRED), release("1.20.2", widened)])
        mapping = {entry["minecraft_version"]: entry["schema"] for entry in document["releases"]}
        self.assertNotEqual(mapping["1.20.1"], mapping["1.20.2"])

    def test_a_family_id_does_not_depend_on_which_releases_share_it(self):
        alone, _ = dataset.merge(None, [release("1.20.1", REQUIRED)])
        together, _ = dataset.merge(None, [release("1.20.1", REQUIRED), release("1.20.2", REQUIRED)])
        self.assertEqual(alone["releases"][0]["schema"], together["releases"][0]["schema"])


class Merging(unittest.TestCase):
    def test_a_new_release_with_an_unchanged_representation_reuses_the_family(self):
        first, _ = dataset.merge(None, [release("1.20.1", REQUIRED)])
        second, changes = dataset.merge(first, [release("1.20.2", REQUIRED)])
        mapping = {entry["minecraft_version"]: entry["schema"] for entry in second["releases"]}
        self.assertEqual(mapping["1.20.1"], mapping["1.20.2"])
        self.assertEqual(len(second["schemas"]), 1)
        self.assertIn("existing schema", changes[-1]["detail"])

    def test_a_release_carries_the_data_version_it_stamps_into_the_file(self):
        # The number a release writes as `version:` is what tells it which
        # historical fixes the rest of the file still needs. It belongs to the
        # release, so it travels with the release rather than being guessed
        # from a version number or copied from a neighbour.
        document, _ = dataset.merge(None, [release("1.20.1", REQUIRED, data_version=3465)])
        entry = next(r for r in document["releases"] if r["minecraft_version"] == "1.20.1")
        self.assertEqual(entry["data_version"], 3465)

        # A release whose data version could not be read carries none at all,
        # rather than a neighbour's.
        document, _ = dataset.merge(document, [release("1.20.2", REQUIRED)])
        entry = next(r for r in document["releases"] if r["minecraft_version"] == "1.20.2")
        self.assertNotIn("data_version", entry)
        kept = next(r for r in document["releases"] if r["minecraft_version"] == "1.20.1")
        self.assertEqual(kept["data_version"], 3465)

    def test_regenerating_the_same_inputs_does_not_move_the_revision(self):
        first, _ = dataset.merge(None, [release("1.20.1", REQUIRED)])
        again, _ = dataset.merge(first, [release("1.20.1", REQUIRED)])
        self.assertEqual(again["revision"], first["revision"])
        self.assertEqual(dataset.dumps(again), dataset.dumps(first))

    def test_a_changed_dataset_advances_the_revision(self):
        first, _ = dataset.merge(None, [release("1.20.1", REQUIRED)])
        second, _ = dataset.merge(first, [release("1.20.2", REQUIRED)])
        self.assertEqual(second["revision"], first["revision"] + 1)

    def test_a_setting_a_newer_release_removed_keeps_its_definition(self):
        with_extra = REQUIRED + [setting("touchscreen")]
        first, _ = dataset.merge(None, [release("1.20.1", with_extra)])
        second, _ = dataset.merge(first, [release("26.2", REQUIRED)])
        self.assertIn("touchscreen", {entry["id"] for entry in second["setting_registry"]})
        self.assertNotIn("touchscreen", second["current_canonical_setting_ids"])

    def test_a_new_current_setting_is_data_without_becoming_a_compiled_preference(self):
        discovered = REQUIRED + [setting("newCurrentOption")]
        document, _ = dataset.merge(None, [release("26.2", discovered)])
        self.assertIn("newCurrentOption", document["current_canonical_setting_ids"])
        self.assertNotIn("newCurrentOption", dataset.REQUIRED_CANONICAL_IDS)

    def test_reanalyzing_history_does_not_replace_current_registry_metadata(self):
        current = copy.deepcopy(REQUIRED)
        current[5]["translation_key"] = "options.currentFov"
        document, _ = dataset.merge(None, [release("26.2", current)])

        historical = copy.deepcopy(REQUIRED)
        historical[5]["translation_key"] = "options.oldFov"
        updated, _ = dataset.merge(document, [release("1.20.1", historical)])
        fov = next(entry for entry in updated["setting_registry"] if entry["id"] == "fov")
        self.assertEqual(fov["translation_key"], "options.currentFov")

    def test_a_key_missing_from_a_complete_extraction_is_unsupported_not_unknown(self):
        without = [entry for entry in REQUIRED if entry["persisted_key"] != "simulationDistance"]
        document, _ = dataset.merge(None, [release("1.16.5", without), release("26.2", REQUIRED)])
        schemas = {schema["id"]: schema for schema in document["schemas"]}
        mapping = {entry["minecraft_version"]: entry["schema"] for entry in document["releases"]}
        legacy = schemas[mapping["1.16.5"]]
        self.assertIn("simulationDistance", legacy["unsupported"])
        self.assertNotIn("simulationDistance", legacy["supported"])

    def schemas_by_release(self, document: dict) -> dict:
        schemas = {schema["id"]: schema for schema in document["schemas"]}
        return {entry["minecraft_version"]: schemas[entry["schema"]] for entry in document["releases"]}

    def test_the_data_version_marker_is_supported_wherever_a_release_stamps_one(self):
        # The stamp comes from the release's version data, so its population
        # not listing it says nothing about whether the release writes it.
        listed = REQUIRED + [setting("version", kind="integer")]
        document, _ = dataset.merge(
            None,
            [
                release("1.16.5", listed, data_version=2586),
                release("1.20.1", REQUIRED, data_version=3465),
                release("26.2", REQUIRED, data_version=4800),
            ],
        )
        for version, schema in self.schemas_by_release(document).items():
            with self.subTest(version=version):
                self.assertNotIn("version", schema["unsupported"])
                position = schema["supported"]["version"]
                self.assertEqual(
                    document["representations"][position],
                    {"key": "version", "codec": {"kind": "integer", "min": None, "max": None}},
                )

    def test_a_release_without_a_readable_stamp_leaves_the_marker_unknown(self):
        listed = REQUIRED + [setting("version", kind="integer")]
        document, _ = dataset.merge(
            None, [release("1.16.5", listed, data_version=2586), release("1.20.1", REQUIRED)]
        )
        schema = self.schemas_by_release(document)["1.20.1"]
        self.assertNotIn("version", schema["supported"])
        self.assertNotIn("version", schema["unsupported"])

    def test_an_unresolved_representation_is_neither_supported_nor_unsupported(self):
        unresolved = copy.deepcopy(REQUIRED)
        unresolved[5] = setting("fov", kind="number", transform="unrecognised(value)")
        document, _ = dataset.merge(None, [release("1.16.5", unresolved)])
        schema = document["schemas"][0]
        self.assertNotIn("fov", schema["supported"])
        self.assertNotIn("fov", schema["unsupported"])


class Validation(unittest.TestCase):
    def build(self) -> dict:
        document, _ = dataset.merge(None, [release("26.2", REQUIRED)])
        return document

    def test_a_well_formed_dataset_validates(self):
        dataset.validate(self.build())

    def test_a_document_missing_structure_is_refused_as_a_dataset_error(self):
        for value in ([], "text", {}):
            with self.subTest(value=value), self.assertRaises(dataset.DatasetError):
                dataset.validate(value)
        for key in ("setting_registry", "schemas", "releases", "representations", "canonical_minecraft_version"):
            broken = self.build()
            del broken[key]
            with self.subTest(missing=key), self.assertRaisesRegex(dataset.DatasetError, "malformed"):
                dataset.validate(broken)
        broken = self.build()
        broken["releases"][0] = "26.2"
        with self.assertRaisesRegex(dataset.DatasetError, "not an object"):
            dataset.validate(broken)

    def test_a_dataset_missing_a_setting_this_build_manages_is_refused(self):
        broken = self.build()
        broken["current_canonical_setting_ids"] = [
            entry for entry in broken["current_canonical_setting_ids"] if entry != "fov"
        ]
        with self.assertRaises(dataset.DatasetError):
            dataset.validate(broken)

    def test_a_current_setting_must_exist_in_the_cumulative_registry(self):
        broken = self.build()
        broken["current_canonical_setting_ids"].append("futureOption")
        with self.assertRaises(dataset.DatasetError):
            dataset.validate(broken)

    def test_a_schema_whose_id_does_not_match_its_content_is_refused(self):
        broken = self.build()
        broken["schemas"][0]["id"] = "0" * 16
        broken["releases"][0]["schema"] = "0" * 16
        with self.assertRaises(dataset.DatasetError):
            dataset.validate(broken)

    def test_a_future_format_version_is_refused(self):
        broken = self.build()
        broken["format_version"] = dataset.FORMAT_VERSION + 1
        with self.assertRaises(dataset.DatasetError):
            dataset.validate(broken)

    def test_a_setting_that_is_both_supported_and_unsupported_is_refused(self):
        broken = self.build()
        broken["schemas"][0]["unsupported"] = ["fov"]
        broken["schemas"][0]["id"] = dataset.fingerprint(
            {
                "supported": {
                    setting_id: broken["representations"][position]
                    for setting_id, position in broken["schemas"][0]["supported"].items()
                },
                "unsupported": ["fov"],
            }
        )
        broken["releases"][0]["schema"] = broken["schemas"][0]["id"]
        with self.assertRaises(dataset.DatasetError):
            dataset.validate(broken)

    def reseal(self, document: dict) -> None:
        """Recomputes the family id so only the edit under test can fail."""
        schema = document["schemas"][0]
        schema["id"] = dataset.fingerprint(
            {
                "supported": {
                    setting_id: document["representations"][position]
                    for setting_id, position in schema["supported"].items()
                },
                "unsupported": schema["unsupported"],
            }
        )
        document["releases"][0]["schema"] = schema["id"]

    def test_a_managed_setting_stored_under_the_version_marker_key_is_refused(self):
        broken = self.build()
        position = broken["schemas"][0]["supported"]["fullscreen"]
        broken["representations"][position]["key"] = "version"
        self.reseal(broken)
        with self.assertRaisesRegex(dataset.DatasetError, "reserved"):
            dataset.validate(broken)

    def test_the_marker_setting_may_use_the_version_marker_key(self):
        document = self.build()
        document["setting_registry"].append(
            {"id": "version", "key": "version", "type": "integer", "manageable": False}
        )
        document["representations"].append(
            {"key": "version", "codec": {"kind": "integer", "min": None, "max": None}}
        )
        document["schemas"][0]["supported"]["version"] = len(document["representations"]) - 1
        self.reseal(document)
        dataset.validate(document)

    def test_a_release_with_a_data_version_cannot_have_the_marker_unsupported(self):
        listed = REQUIRED + [setting("version", kind="integer")]
        document, _ = dataset.merge(
            None,
            [release("1.20.1", REQUIRED, data_version=3465), release("1.16.5", listed, data_version=2586)],
        )
        dataset.validate(document)
        schema = next(s for s in document["schemas"] if "version" in s["supported"])
        del schema["supported"]["version"]
        schema["unsupported"] = sorted(schema["unsupported"] + ["version"])
        self.reseal_schema(document, schema)
        with self.assertRaisesRegex(dataset.DatasetError, "marker"):
            dataset.validate(document)

    def reseal_schema(self, document: dict, schema: dict) -> None:
        old = schema["id"]
        schema["id"] = dataset.fingerprint(
            {
                "supported": {
                    setting_id: document["representations"][position]
                    for setting_id, position in schema["supported"].items()
                },
                "unsupported": schema["unsupported"],
            }
        )
        for release in document["releases"]:
            if release["schema"] == old:
                release["schema"] = schema["id"]

    def test_two_settings_of_one_schema_cannot_share_a_stored_key(self):
        broken = self.build()
        supported = broken["schemas"][0]["supported"]
        supported["fullscreen"] = supported["fov"]
        self.reseal(broken)
        with self.assertRaisesRegex(dataset.DatasetError, "both"):
            dataset.validate(broken)

    def test_a_canonical_release_that_is_not_mapped_is_refused(self):
        broken = self.build()
        broken["canonical_minecraft_version"] = "27.0"
        with self.assertRaises(dataset.DatasetError):
            dataset.validate(broken)


class CanonicalReleaseGate(unittest.TestCase):
    """`validate` is the only gate before the bundled file and the feed are
    written, so a canonical release that cannot encode a required setting must
    stop here."""

    def canonical(self, settings: list[dict]) -> dict:
        document, _ = dataset.merge(None, [release("26.2", settings)])
        return document

    def test_a_required_setting_with_an_unrecognized_transform_is_refused(self):
        settings = copy.deepcopy(REQUIRED)
        settings[5] = setting("fov", kind="number", transform="pow(value, 2)")
        with self.assertRaisesRegex(dataset.DatasetError, "fov \\(not established\\)"):
            dataset.validate(self.canonical(settings))

    def test_a_canonical_release_that_supports_none_of_the_required_settings_is_refused(self):
        special = [
            setting(entry["persisted_key"], entry["logical_id"], kind="special") for entry in REQUIRED
        ]
        document = self.canonical(special)
        self.assertEqual(document["schemas"][0]["supported"], {})
        with self.assertRaisesRegex(dataset.DatasetError, "cannot encode required settings"):
            dataset.validate(document)

    def test_a_required_setting_the_canonical_release_lacks_is_refused(self):
        settings = [entry for entry in REQUIRED if entry["persisted_key"] != "simulationDistance"]
        document, _ = dataset.merge(
            None, [release("1.17.1", REQUIRED), release("26.2", settings)]
        )
        with self.assertRaisesRegex(dataset.DatasetError, "simulationDistance"):
            dataset.validate(document)

    def test_a_required_setting_listed_unsupported_for_the_canonical_release_is_refused(self):
        document = self.canonical(REQUIRED)
        schema = document["schemas"][0]
        del schema["supported"]["simulationDistance"]
        schema["unsupported"] = ["simulationDistance"]
        schema["id"] = dataset.fingerprint(
            {
                "supported": {
                    setting_id: document["representations"][position]
                    for setting_id, position in schema["supported"].items()
                },
                "unsupported": schema["unsupported"],
            }
        )
        document["releases"][0]["schema"] = schema["id"]
        with self.assertRaisesRegex(dataset.DatasetError, "simulationDistance \\(absent from the release\\)"):
            dataset.validate(document)

    def test_a_historical_release_without_a_required_setting_is_accepted(self):
        before = [entry for entry in REQUIRED if entry["persisted_key"] != "simulationDistance"]
        document, _ = dataset.merge(
            None, [release("1.17.1", before), release("26.2", REQUIRED)]
        )
        dataset.validate(document)


class ApplicationLoadParity(unittest.TestCase):
    """What the application's loader refuses must not pass the generator."""

    reseal = Validation.reseal

    def build(self) -> dict:
        document, _ = dataset.merge(None, [release("26.2", REQUIRED)])
        return document

    def representation(self, document: dict, setting_id: str) -> dict:
        return document["representations"][document["schemas"][0]["supported"][setting_id]]

    def assert_refused(self, document: dict, pattern: str = "") -> None:
        with self.assertRaisesRegex(dataset.DatasetError, pattern):
            dataset.validate(document)

    def test_an_integer_codec_with_its_minimum_above_its_maximum_is_refused(self):
        broken = self.build()
        self.representation(broken, "guiScale")["codec"].update(min=10, max=2)
        self.reseal(broken)
        self.assert_refused(broken, "minimum above its maximum")

    def test_a_decimal_codec_with_a_non_finite_bound_is_refused(self):
        broken = self.build()
        self.representation(broken, "renderDistance")["codec"] = {
            "kind": "decimal", "min": 0, "max": float("inf")}
        self.reseal(broken)
        self.assert_refused(broken, "finite number")

    def test_a_scaled_decimal_codec_with_a_zero_scale_is_refused(self):
        broken = self.build()
        self.representation(broken, "fov")["codec"] = {
            "kind": "scaled_decimal", "offset": 70, "scale": 0, "min": None, "max": None}
        self.reseal(broken)
        self.assert_refused(broken, "zero scale")

    def test_enum_codecs_without_values_or_with_unwritable_tokens_are_refused(self):
        for codec, pattern in (
            ({"kind": "enum_id", "values": []}, "no values"),
            ({"kind": "enum_token", "tokens": []}, "no tokens"),
            ({"kind": "enum_token", "tokens": ["a\nb"], "quoted": False}, "token"),
        ):
            with self.subTest(codec=codec):
                broken = self.build()
                self.representation(broken, "fullscreen")["codec"] = codec
                self.reseal(broken)
                self.assert_refused(broken, pattern)

    def test_a_codec_kind_the_application_does_not_implement_is_refused(self):
        broken = self.build()
        self.representation(broken, "fullscreen")["codec"] = {"kind": "expression", "text": "x"}
        self.reseal(broken)
        self.assert_refused(broken, "unknown kind")

    def test_a_representation_key_the_options_file_cannot_hold_is_refused(self):
        for key in ("", "we:ird", "tab\tkey", "line\nbreak"):
            with self.subTest(key=key):
                broken = self.build()
                self.representation(broken, "fullscreen")["key"] = key
                self.reseal(broken)
                self.assert_refused(broken, "not usable as an options.txt key")

    def test_an_unsupported_id_missing_from_the_registry_is_refused(self):
        broken = self.build()
        broken["schemas"][0]["unsupported"] = ["notInTheRegistry"]
        self.reseal(broken)
        self.assert_refused(broken, "unknown unsupported setting")

    def test_a_setting_listed_twice_as_unsupported_is_refused(self):
        broken = self.build()
        broken["setting_registry"].append(
            {"id": "gone", "key": "gone", "type": "boolean", "manageable": False})
        broken["schemas"][0]["unsupported"] = ["gone", "gone"]
        self.reseal(broken)
        self.assert_refused(broken, "twice")

    def test_a_schema_declared_twice_is_refused(self):
        broken = self.build()
        broken["schemas"].append(copy.deepcopy(broken["schemas"][0]))
        self.assert_refused(broken, "declared twice")

    def test_a_malformed_representation_position_is_refused_without_crashing(self):
        for position in (10_000, -1, True, "0", 1.0, None):
            with self.subTest(position=position):
                broken = self.build()
                broken["schemas"][0]["supported"]["fullscreen"] = position
                self.assert_refused(broken, "out-of-range representation")

    def test_a_registry_field_of_the_wrong_type_is_refused(self):
        for field, value, pattern in (
            ("id", 7, "`id` that is not a string"),
            ("key", None, "`key` that is not a string"),
            ("type", ["boolean"], "`type` that is not a string"),
            ("category", 3, "`category` that is not a string"),
            ("translation_key", {}, "`translation_key` that is not a string"),
            ("manageable", None, "`manageable` flag that is not a boolean"),
            ("manageable", 1, "`manageable` flag that is not a boolean"),
        ):
            with self.subTest(field=field, value=value):
                broken = self.build()
                broken["setting_registry"][0][field] = value
                self.assert_refused(broken, pattern)

    def test_a_registry_entry_may_leave_its_optional_strings_null_or_absent(self):
        document = self.build()
        entry = document["setting_registry"][0]
        entry["category"] = None
        del entry["translation_key"]
        dataset.validate(document)

    def test_a_field_of_the_wrong_type_outside_the_registry_is_refused(self):
        for path, value, pattern in (
            (("canonical_minecraft_version",), 26, "canonical release is not a string"),
            (("current_canonical_setting_ids", 0), 1, "current canonical setting id is not a string"),
            (("schemas", 0, "id"), ["x"], "id that is not a string"),
            (("schemas", 0, "supported"), [], "not keyed by setting id"),
            (("schemas", 0, "supported"), None, "not keyed by setting id"),
            (("releases", 0, "minecraft_version"), 26.2, "`minecraft_version` that is not a string"),
            (("releases", 0, "schema"), {}, "`schema` that is not a string"),
        ):
            with self.subTest(path=path, value=value):
                broken = self.build()
                target = broken
                for step in path[:-1]:
                    target = target[step]
                target[path[-1]] = value
                self.assert_refused(broken, pattern)

    def test_a_structure_of_the_wrong_shape_is_a_dataset_error_not_a_crash(self):
        for key in ("setting_registry", "schemas", "releases"):
            for value in ("text", {}, None):
                with self.subTest(key=key, value=value):
                    broken = self.build()
                    broken[key] = value
                    self.assert_refused(broken, "is not a list")
            broken = self.build()
            broken[key][0] = "text"
            self.assert_refused(broken, "is not an object")

    def test_a_data_version_that_is_not_an_integer_is_refused(self):
        for value in ("1", True, 1.5, 2**63):
            with self.subTest(value=value):
                broken = self.build()
                broken["releases"][0]["data_version"] = value
                self.assert_refused(broken, "data version")

    def test_a_revision_beyond_what_the_application_can_read_is_refused(self):
        for value in (0, -1, True, 2**32, "3"):
            with self.subTest(value=value):
                broken = self.build()
                broken["revision"] = value
                self.assert_refused(broken, "revision")

    def test_a_malformed_transform_constant_leaves_the_setting_unknown(self):
        for transform in ("((value - 1.2.3) / 40)", "(value - .)", "((value - 70) / 4.0.)"):
            with self.subTest(transform=transform):
                unreadable = setting("fov", kind="number", transform=transform)
                self.assertIsNone(dataset.codec_for(unreadable, "glfw"))
                self.assertEqual(
                    dataset.representation_issue(unreadable, "glfw"),
                    "unrecognized storage transform",
                )

    def test_a_number_that_does_not_parse_is_a_dataset_error(self):
        with self.assertRaises(dataset.DatasetError):
            dataset._number("1.2.3")


if __name__ == "__main__":
    unittest.main()
