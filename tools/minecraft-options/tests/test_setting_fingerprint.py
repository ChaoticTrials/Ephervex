#!/usr/bin/env python3
"""Tests for what a setting's representation fingerprint covers.

    python3 -m unittest discover -s tools/minecraft-options

Every fact that changes the bytes of a setting's line must be part of the
fingerprint, so two settings that write differently never compare equal.
"""

from __future__ import annotations

import unittest
from typing import Any

import analyze_options as analyzer

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
            "sha256": "0" * 64, "jar_id": "jar", "detected_version": "26.2",
            "options_analysis": {"model_part_family": {"names": ["cape"], "enum_class": "Part"},
                                 "roots": ["Options"]}})

        (part,) = normalized["settings"]
        self.assertEqual(part["representation"], analyzer.representation(part))


if __name__ == "__main__":
    unittest.main()
