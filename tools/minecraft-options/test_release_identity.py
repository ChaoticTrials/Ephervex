#!/usr/bin/env python3
"""Tests for which client builds count as stable Minecraft releases.

    python3 -m unittest discover -s tools/minecraft-options

Offline and deterministic: version metadata is written out here, so nothing
depends on a client JAR.
"""

from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path
from zipfile import ZipFile

import analyze_options as analyzer
import dataset
import update
from release_identity import is_release, version_key
from test_dataset import REQUIRED, release


def detect(version_json: dict | None, filename: str = "client.jar") -> dict:
    with tempfile.TemporaryDirectory() as directory:
        path = Path(directory) / filename
        with ZipFile(path, "w") as archive:
            if version_json is not None:
                archive.writestr("version.json", json.dumps(version_json))
        with ZipFile(path) as archive:
            return analyzer.detect_version(path, archive)


class ReleaseIdentifiers(unittest.TestCase):
    def test_only_release_shaped_identifiers_are_releases(self):
        for version in ("1.12.2", "1.21", "1.21.11", "26.1", "26.2.1", "27.3"):
            self.assertTrue(is_release(version), version)

    def test_look_alikes_and_non_releases_are_not_releases(self):
        for version in (
            "10.5", "19.3", "25.9", "1.21.5-rc1", "26.2.1-rc1", "24w14a", "1.21-pre1",
            "1.²", "1.٢", "1.021", "01.2", "1.", "1", "", " 1.21", "1.21 ", None, 21,
        ):
            self.assertFalse(is_release(version), repr(version))

    def test_ordering_follows_release_order_across_the_calendar_switch(self):
        ordered = ["1.12.2", "1.13", "1.20.6", "1.21", "1.21.11", "26.1", "26.1.2", "26.2"]
        self.assertEqual(sorted(reversed(ordered), key=version_key), ordered)

    def test_ordering_refuses_identifiers_that_are_not_releases(self):
        for version in ("1.21.5-rc1", "26.2.1-rc1", "1.²", "10.5", "24w14a"):
            with self.assertRaises(ValueError, msg=version):
                version_key(version)
            with self.assertRaises(ValueError, msg=version):
                dataset.version_key(version)


class AnalyzerVersionDetection(unittest.TestCase):
    def test_a_release_candidate_is_not_a_release_of_its_target(self):
        detected = detect({"id": "1.21.6-rc1", "name": "1.21.6 Release Candidate 1", "release_target": "1.21.6"})
        self.assertEqual(detected["version"], "1.21.6-rc1")
        self.assertFalse(detected["stable_release"])
        self.assertFalse(analyzer.supported(detected["version"]))

    def test_a_snapshot_is_not_a_release_of_its_target(self):
        detected = detect({"id": "19w14a", "name": "19w14a", "release_target": "1.14"})
        self.assertEqual(detected["version"], "19w14a")
        self.assertFalse(detected["stable_release"])

    def test_a_release_target_alone_does_not_name_a_build(self):
        detected = detect({"release_target": "1.21.6"})
        self.assertIsNone(detected["version"])
        self.assertFalse(detected["stable_release"])

    def test_a_name_and_id_that_disagree_are_not_a_release(self):
        detected = detect({"id": "1.21.6-rc1", "name": "1.21.6"})
        self.assertEqual(detected["version"], "1.21.6-rc1")
        self.assertFalse(detected["stable_release"])
        detected = detect({"id": "1.21.5", "name": "1.21.6"})
        self.assertFalse(detected["stable_release"])
        self.assertFalse(analyzer.supported(detected["version"]))

    def test_a_release_names_itself_the_same_in_name_and_id(self):
        detected = detect({"id": "1.21.6", "name": "1.21.6", "release_target": None})
        self.assertEqual((detected["version"], detected["stable_release"]), ("1.21.6", True))

    def test_a_single_identifier_is_enough_to_name_a_release(self):
        self.assertEqual(detect({"id": "26.2"})["version"], "26.2")
        self.assertEqual(detect({"name": "1.20.1"})["version"], "1.20.1")

    def test_an_id_with_a_build_suffix_still_names_its_release(self):
        detected = detect({"id": "1.14.1 / a8f78b0d43c74598a199d6d80cda413f", "name": "1.14.1",
                           "release_target": "1.14.1", "stable": False})
        self.assertEqual((detected["version"], detected["stable_release"]), ("1.14.1", True))

    def test_a_build_suffix_cannot_hide_a_different_release(self):
        detected = detect({"id": "1.14.2 / a8f78b0d43c74598a199d6d80cda413f", "name": "1.14.1"})
        self.assertFalse(detected["stable_release"])

    def test_a_look_alike_identifier_is_not_a_supported_release(self):
        for identifier in ("10.5", "19.3", "1.²"):
            detected = detect({"id": identifier, "name": identifier})
            self.assertFalse(detected["stable_release"], identifier)
            self.assertFalse(analyzer.supported(detected["version"]), identifier)

    def test_a_filename_cannot_name_a_look_alike_release(self):
        detected = detect(None, filename="mc-10.5.jar")
        self.assertIsNone(detected["version"])
        self.assertFalse(detected["stable_release"])


class DatasetFolding(unittest.TestCase):
    def write_analysis(self, directory: Path, records: list[dict]) -> None:
        normalized = directory / "normalized"
        normalized.mkdir()
        for index, record in enumerate(records):
            (normalized / f"{index}.json").write_text(json.dumps(record), encoding="utf-8")

    def test_the_analysis_path_skips_records_that_are_not_releases(self):
        records = [
            release("1.21.5-rc1", REQUIRED),
            release("26.2.1-rc1", REQUIRED),
            release("1.²", REQUIRED),
            release("10.5", REQUIRED),
            release("24w14a", REQUIRED),
            release("1.20.1", REQUIRED),
            release("26.2", REQUIRED),
        ]
        with tempfile.TemporaryDirectory() as directory:
            self.write_analysis(Path(directory), records)
            kept = update.normalized_records(Path(directory), None)
        self.assertEqual(sorted(record["minecraft_version"] for record in kept), ["1.20.1", "26.2"])

    def test_the_analysis_path_refuses_when_only_non_releases_were_analyzed(self):
        with tempfile.TemporaryDirectory() as directory:
            self.write_analysis(Path(directory), [release("1.21.5-rc1", REQUIRED)])
            with self.assertRaises(update.WorkflowError):
                update.normalized_records(Path(directory), None)

    def test_a_pre_release_cannot_become_the_canonical_release(self):
        base, _ = dataset.merge(None, [release("26.2", REQUIRED)])
        with self.assertRaises(dataset.DatasetError):
            dataset.merge(base, [release("26.2.1-rc1", REQUIRED)])


if __name__ == "__main__":
    unittest.main()
