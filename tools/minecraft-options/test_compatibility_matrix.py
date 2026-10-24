#!/usr/bin/env python3
"""Tests for what the cross-release compatibility matrix is allowed to claim.

    python3 -m unittest discover -s tools/minecraft-options

A high-confidence `identical` or `mapped` entry is write-authoritative, so
every fact that changes the bytes of a setting's line must be compared.
"""

from __future__ import annotations

import unittest
from typing import Any

import analyze_options as analyzer

CANONICAL = "26.2"
OLDER = "1.19"


def setting(key: str, kind: str = "string", **fields: Any) -> dict[str, Any]:
    record: dict[str, Any] = {
        "logical_id": key, "persisted_key": key, "translation_key": None, "type": kind,
        "file_encoding": "text", "file_quoting": "raw", "numeric": None, "allowed_values": None,
        "value_to_storage": None, "storage_to_value": None, "transform_resolved": None,
        "ui_domain": None,
    }
    record.update(fields)
    record["representation"] = analyzer.representation(record)
    return record


def release(version: str, *settings: dict[str, Any], complete: bool = True) -> dict[str, Any]:
    return {"minecraft_version": version, "settings": list(settings), "extraction_complete": complete,
            "keybinding_code_space": "glfw", "persistence_era": "option_instance"}


def entry(older: list[dict[str, Any]], canonical: list[dict[str, Any]], key: str) -> dict[str, Any]:
    versions = [release(OLDER, *older), release(CANONICAL, *canonical)]
    matrix = analyzer.compatibility(versions[-1], versions)
    row = next(row for row in matrix["settings"] if row["canonical_key"] == key)
    return row["versions"][OLDER]


def enum(*tokens: str, form: str = "string_token") -> dict[str, Any]:
    return {"persisted_form": form, "values": [{"persisted": token} for token in tokens]}


class FingerprintCoversWhatTheFileHolds(unittest.TestCase):
    def test_quoting_changes_the_fingerprint(self) -> None:
        raw = setting("mainHand")
        quoted = setting("mainHand", file_quoting="json_string")

        self.assertNotEqual(raw["representation"], quoted["representation"])

    def test_raw_text_and_json_that_print_the_same_bytes_share_a_fingerprint(self) -> None:
        raw = setting("fullscreen", "boolean", file_quoting="raw")
        json = setting("fullscreen", "boolean", file_quoting="json")

        self.assertEqual(raw["representation"], json["representation"])

    def test_the_read_transform_the_step_and_clamping_change_the_fingerprint(self) -> None:
        base = setting("fov", "number", numeric={"min": 0.0, "max": 1.0, "step": None, "clamped": True})
        variants = [
            setting("fov", "number", numeric=base["numeric"], storage_to_value="((file - 70) / 40)"),
            setting("fov", "number", numeric={**base["numeric"], "step": 0.5}),
            setting("fov", "number", numeric={**base["numeric"], "clamped": False}),
            setting("fov", "number", numeric=base["numeric"], transform_resolved=False),
        ]

        for variant in variants:
            self.assertNotEqual(base["representation"], variant["representation"])

    def test_a_dynamic_family_setting_gets_a_computed_fingerprint(self) -> None:
        normalized = analyzer.normalize({
            "sha256": "0" * 64, "jar_id": "jar", "detected_version": CANONICAL,
            "options_analysis": {"model_part_family": {"names": ["cape"], "enum_class": "Part"},
                                 "roots": ["Options"]}})

        (part,) = normalized["settings"]
        self.assertEqual(part["representation"], analyzer.representation(part))


class MatrixOnlyClaimsWhatItCompared(unittest.TestCase):
    def test_a_string_quoted_in_one_release_and_raw_in_the_other_is_not_identical(self) -> None:
        result = entry([setting("mainHand")], [setting("mainHand", file_quoting="json_string")], "mainHand")

        self.assertEqual((result["status"], result["confidence"]), ("mapped", "high"))
        self.assertIn("file quoting differs", result["reason"])
        self.assertEqual(result["mapping"]["older_quoting"], "bare")
        self.assertEqual(result["mapping"]["canonical_quoting"], "quoted")

    def test_an_older_transform_the_analyzer_could_not_read_is_unknown(self) -> None:
        older = setting("ao", "integer", transform_resolved=False)
        canonical = setting("ao", "integer", transform_resolved=True)

        result = entry([older], [canonical], "ao")

        self.assertEqual((result["status"], result["confidence"]), ("unknown", "medium"))

    def test_enum_values_that_differ_are_not_identical(self) -> None:
        older = setting("particles", "enum", allowed_values=enum("all", "decreased"))
        canonical = setting("particles", "enum", allowed_values=enum("all", "decreased", "minimal"))

        result = entry([older], [canonical], "particles")

        self.assertEqual((result["status"], result["confidence"]), ("mapped", "high"))
        self.assertIn("enum values differ", result["reason"])

    def test_the_same_enum_values_in_a_different_order_are_identical(self) -> None:
        older = setting("particles", "enum", allowed_values=enum("minimal", "all"))
        canonical = setting("particles", "enum", allowed_values=enum("all", "minimal"))

        result = entry([older], [canonical], "particles")

        self.assertEqual((result["status"], result["confidence"]), ("identical", "high"))

    def test_an_enum_domain_proven_on_one_side_only_is_not_high_confidence(self) -> None:
        older = setting("particles", "enum")
        canonical = setting("particles", "enum", allowed_values=enum("all", "minimal"))

        result = entry([older], [canonical], "particles")

        self.assertEqual((result["status"], result["confidence"]), ("identical", "medium"))

    def test_a_mapping_with_an_unproven_numeric_domain_is_not_high_confidence(self) -> None:
        numeric = {"min": 30.0, "max": 110.0, "step": None, "clamped": True}
        older = setting("fov", "integer", file_encoding="decimal integer")
        canonical = setting("fov", "number", file_encoding="decimal floating point", numeric=numeric)

        result = entry([older], [canonical], "fov")

        self.assertEqual((result["status"], result["confidence"]), ("mapped", "medium"))
        self.assertIn("numeric domain is unproven on one side", result["reason"])


class RenamesNeedAnUnambiguousOlderKey(unittest.TestCase):
    def test_an_older_key_that_is_itself_a_current_setting_is_not_a_rename_source(self) -> None:
        gui_scale = setting("guiScale", "integer", translation_key="options.guiScale")
        debug_scale = setting("debugGuiScale", "integer", translation_key="options.guiScale")

        result = entry([gui_scale], [gui_scale, debug_scale], "debugGuiScale")

        self.assertEqual(result["status"], "unsupported")
        self.assertNotIn("mapping", result)

    def test_a_unique_older_key_that_is_no_longer_current_is_a_rename(self) -> None:
        older = setting("oldName", translation_key="options.example")
        canonical = setting("newName", translation_key="options.example")

        result = entry([older], [canonical], "newName")

        self.assertEqual(result["status"], "mapped")
        self.assertEqual(result["mapping"]["older_key"], "oldName")
        self.assertIn("persisted key renamed", result["reason"])

    def test_several_older_settings_sharing_a_translation_key_prove_no_rename(self) -> None:
        first = setting("firstName", translation_key="options.example")
        second = setting("secondName", translation_key="options.example")
        canonical = setting("newName", translation_key="options.example")

        result = entry([first, second], [canonical], "newName")

        self.assertNotEqual(result["status"], "mapped")

    def test_several_current_settings_sharing_a_translation_key_prove_no_rename(self) -> None:
        older = setting("oldName", translation_key="options.example")
        first = setting("firstName", translation_key="options.example")
        second = setting("secondName", translation_key="options.example")

        result = entry([older], [first, second], "firstName")

        self.assertNotEqual(result["status"], "mapped")

    def test_the_translation_index_keeps_every_setting_that_shares_a_key(self) -> None:
        first = setting("a", translation_key="options.example")
        second = setting("b", translation_key="options.example")

        index = analyzer.translation_index(release(OLDER, first, second))

        self.assertEqual([s["persisted_key"] for s in index["options.example"]], ["a", "b"])


class SchemaChangesSeeSerializationDifferences(unittest.TestCase):
    def serialization(self, older: dict[str, Any], newer: dict[str, Any]) -> list[dict[str, Any]]:
        versions = [release(OLDER, older), release(CANONICAL, newer)]
        changes = analyzer.schema_changes(versions, {newer["persisted_key"]})["changes"]
        return changes[0]["serialization_changes"]

    def test_a_change_of_quoting_is_a_serialization_change(self) -> None:
        changes = self.serialization(setting("mainHand"), setting("mainHand", file_quoting="json_string"))

        self.assertEqual([c["key"] for c in changes], ["mainHand"])
        self.assertEqual((changes[0]["from"]["quoting"], changes[0]["to"]["quoting"]), ("bare", "quoted"))

    def test_a_change_of_read_transform_is_a_serialization_change(self) -> None:
        older = setting("fov", "number", storage_to_value="(file - 70)")

        self.assertEqual(len(self.serialization(older, setting("fov", "number"))), 1)

    def test_an_unread_transform_is_omitted_rather_than_reported(self) -> None:
        older = setting("ao", "integer", transform_resolved=False)

        self.assertEqual(self.serialization(older, setting("ao", "integer", storage_to_value="(file + 1)")), [])

    def test_identical_serialization_is_not_reported(self) -> None:
        self.assertEqual(self.serialization(setting("mainHand"), setting("mainHand")), [])


if __name__ == "__main__":
    unittest.main()
