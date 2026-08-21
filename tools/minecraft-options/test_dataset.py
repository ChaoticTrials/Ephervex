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
) -> dict:
    written = {
        "boolean": "lowercase true/false",
        "integer": "decimal integer",
        "number": "decimal floating point",
        "string": "UTF-8 text after the first colon",
        "key_binding": "InputConstants key name string (for example key.keyboard.w)",
        "enum": "enum id integer",
    }
    return {
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
            (setting("soundDevice", kind="string"), "glfw", {"kind": "text"}),
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

    def test_a_canonical_release_that_is_not_mapped_is_refused(self):
        broken = self.build()
        broken["canonical_minecraft_version"] = "27.0"
        with self.assertRaises(dataset.DatasetError):
            dataset.validate(broken)


if __name__ == "__main__":
    unittest.main()
