#!/usr/bin/env python3
"""A release counts as completely extracted only when every population was found.

    python3 -m unittest discover -s tools/minecraft-options

Completeness is what turns a setting missing from a release into a provably
unsupported one, so a partial result must leave such settings unknown.
"""

from __future__ import annotations

import copy
import subprocess
import unittest
from unittest import mock

import analyze_options as analyzer
import dataset


def option(key: str) -> dict:
    return {"persisted_key": key, "type": "boolean", "kind": "option_instance", "field": key,
            "field_descriptor": "Z", "default": False, "default_confidence": "high"}


def keybind(name: str) -> dict:
    return {"translation_key": f"key.{name}", "persisted_key": f"key_key.{name}", "input_type": "keyboard",
            "code": 1, "serialized": f"key.keyboard.{name}", "unbound": False, "file_encoding": "key name",
            "binding_category": "key.categories.misc", "confidence": "high", "codec_confidence": "high",
            "constructor_descriptor": "(Ljava/lang/String;ILjava/lang/String;)V"}


def raw_record(**overrides) -> dict:
    analysis = {
        "roots": ["net/minecraft/client/Options"],
        "options": {f"option{n}": option(f"option{n}") for n in range(analyzer.MINIMUM_PERSISTED_OPTIONS)},
        "keybinds": [keybind("attack"), keybind("use")],
        "sound_family": {"prefix": "soundCategory_", "enum_class": "Sound", "names": ["master", "music"],
                         "default": 1.0, "confidence": "high"},
        "model_part_family": {"prefix": "modelPart_", "enum_class": "Part", "names": ["cape", "jacket"]},
        "javap_errors": [],
        "keybinding_code_space": "glfw",
        "extraction_complete": True,
    }
    analysis.update(overrides)
    return {"sha256": "0" * 64, "jar_id": "jar", "detected_version": "26.2", "options_analysis": analysis}


def canonical(record: dict) -> list[dict]:
    """Registry definitions of the complete record, as dataset.schema_for reads them."""
    return dataset.registry_settings([analyzer.normalize(record)], None)


class ExtractionGaps(unittest.TestCase):
    def test_every_expected_population_found_leaves_no_gap(self) -> None:
        self.assertEqual([], analyzer.extraction_gaps(raw_record()["options_analysis"]))

    def test_each_missing_population_is_a_gap(self) -> None:
        cases = {
            "roots": [],
            "options": {f"option{n}": option(f"option{n}") for n in range(analyzer.MINIMUM_PERSISTED_OPTIONS - 1)},
            "keybinds": [],
            "sound_family": None,
            "model_part_family": None,
        }
        for field, value in cases.items():
            with self.subTest(field=field):
                analysis = raw_record(**{field: value})["options_analysis"]
                self.assertTrue(analyzer.extraction_gaps(analysis))

    def test_a_family_whose_members_were_not_found_is_a_gap(self) -> None:
        for family in ("sound_family", "model_part_family"):
            with self.subTest(family=family):
                analysis = raw_record()["options_analysis"]
                analysis[family] = {**analysis[family], "names": []}
                self.assertTrue(analyzer.extraction_gaps(analysis))

    def test_a_javap_failure_on_any_class_is_a_gap(self) -> None:
        errors = [{"class": "net/minecraft/client/KeyMapping", "error": "javap exit 1"}]
        analysis = raw_record(javap_errors=errors)["options_analysis"]

        self.assertEqual(["javap failed on 1 class(es)"], analyzer.extraction_gaps(analysis))


class NormalizedCompleteness(unittest.TestCase):
    def test_a_complete_record_stays_complete(self) -> None:
        normalized = analyzer.normalize(raw_record())

        self.assertTrue(normalized["extraction_complete"])
        self.assertEqual([], normalized["extraction_gaps"])

    def test_a_record_flagged_complete_by_an_older_analyzer_is_judged_by_its_populations(self) -> None:
        blanked = raw_record(keybinds=[], sound_family=None, model_part_family=None)

        normalized = analyzer.normalize(blanked)

        self.assertFalse(normalized["extraction_complete"])
        self.assertIn("no key bindings", normalized["extraction_gaps"])

    def test_a_record_the_analyzer_marked_incomplete_stays_incomplete(self) -> None:
        self.assertFalse(analyzer.normalize(raw_record(extraction_complete=False))["extraction_complete"])

    def test_a_partial_release_leaves_absent_settings_unknown_instead_of_unsupported(self) -> None:
        settings = canonical(raw_record())
        partial = analyzer.normalize(raw_record(keybinds=[], sound_family=None, model_part_family=None))

        schema = dataset.schema_for(partial, settings)

        self.assertEqual([], schema["unsupported"])
        self.assertTrue(all(not entry.startswith(("keybind.", "sound.", "modelPart.")) for entry in schema["supported"]))

    def test_a_complete_release_still_proves_a_missing_setting_unsupported(self) -> None:
        settings = canonical(raw_record())
        older = copy.deepcopy(raw_record())
        older["options_analysis"]["options"]["renamedOption"] = option("renamedOption")
        del older["options_analysis"]["options"]["option0"]

        schema = dataset.schema_for(analyzer.normalize(older), settings)

        self.assertEqual(["option0"], schema["unsupported"])


class DisassemblerFailures(unittest.TestCase):
    def disassemble(self, method: str, **run) -> analyzer.Disassembler:
        dis = analyzer.Disassembler("client.jar", {"a/B"})
        with mock.patch.object(analyzer.shutil, "which", return_value="/usr/bin/javap"), \
                mock.patch.object(analyzer.subprocess, "run", **run):
            getattr(dis, method)("a/B")
        return dis

    def test_a_javap_timeout_is_recorded_against_its_class(self) -> None:
        timeout = subprocess.TimeoutExpired("javap", 120)
        for method in ("get", "verbose"):
            with self.subTest(method=method):
                dis = self.disassemble(method, side_effect=timeout)
                self.assertEqual(["a/B"], [error["class"] for error in dis.errors])

    def test_a_javap_failure_is_recorded_against_its_class(self) -> None:
        failed = mock.Mock(stdout="partial", stderr="boom", returncode=1)
        for method in ("get", "verbose"):
            with self.subTest(method=method):
                dis = self.disassemble(method, return_value=failed)
                self.assertEqual([{"class": "a/B", "error": "boom"}], dis.errors)

    def test_a_missing_javap_is_recorded(self) -> None:
        dis = analyzer.Disassembler("client.jar", {"a/B"})
        with mock.patch.object(analyzer.shutil, "which", return_value=None):
            dis.get("a/B")
        self.assertEqual([{"class": "a/B", "error": "javap unavailable"}], dis.errors)


if __name__ == "__main__":
    unittest.main()
