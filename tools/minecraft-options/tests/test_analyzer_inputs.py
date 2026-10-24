#!/usr/bin/env python3
"""Tests that the analyzer accepts only usable inputs and leaves an output that is exactly one run's."""

from __future__ import annotations

import contextlib
import copy
import io
import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import analyze_options as analyzer
import dataset
import preferences
import update
from .test_dataset import REQUIRED, release


def run_main(*arguments: str) -> tuple[int, str, str]:
    """Runs the analyzer's command line, returning its exit status and both output streams."""
    stdout, stderr = io.StringIO(), io.StringIO()
    status = 0
    with mock.patch("sys.argv", ["analyze_options.py", *arguments]):
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            try:
                status = analyzer.main()
            except SystemExit as exit_request:
                status = exit_request.code
    return status, stdout.getvalue(), stderr.getvalue()


class AnalyzerCommandLine(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.base = Path(self.directory.name)
        self.root = self.base / "jars"
        self.root.mkdir()
        self.output = self.base / "output"

    def write_jar(self, name: str, content: bytes, folder: Path | None = None) -> Path:
        path = (folder or self.root) / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content)
        return path

    def analyze(self, *extra: str) -> tuple[int, str, str]:
        return run_main("--root", str(self.root), "--output", str(self.output), *extra)

    def records(self, kind: str) -> set[str]:
        return {path.stem for path in (self.output / kind).glob("*.json")}

    def test_a_root_without_jars_fails_and_leaves_the_existing_output_untouched(self) -> None:
        self.output.mkdir()
        (self.output / "inventory.json").write_text('{"kept": true}', encoding="utf-8")
        status, _, error = self.analyze()
        self.assertEqual(1, status)
        self.assertIn("no JAR files found", error)
        self.assertEqual('{"kept": true}', (self.output / "inventory.json").read_text(encoding="utf-8"))
        self.assertFalse((self.output / "normalized").exists())

    def test_a_root_that_is_not_a_directory_is_refused_before_anything_is_written(self) -> None:
        status, _, error = run_main("--root", str(self.base / "typo"), "--output", str(self.output))
        self.assertEqual(2, status)
        self.assertIn("--root is not a directory", error)
        self.assertFalse(self.output.exists())

    def test_an_input_jar_outside_the_root_is_refused_with_a_message_naming_the_root(self) -> None:
        outside = self.write_jar("outside.jar", b"not a zip", folder=self.base / "elsewhere")
        status, _, error = self.analyze("--input-jar", str(outside))
        self.assertEqual(2, status)
        self.assertIn("outside --root", error)
        self.assertFalse(self.output.exists())

    def test_a_jar_symlinked_from_outside_the_root_is_analyzed_at_its_place_in_the_root(self) -> None:
        outside = self.write_jar("real.jar", b"not a zip", folder=self.base / "elsewhere")
        (self.root / "linked.jar").symlink_to(outside)
        status, _, _ = self.analyze()
        self.assertEqual(0, status)
        inventory = json.loads((self.output / "inventory.json").read_text(encoding="utf-8"))
        self.assertEqual(["linked.jar"], [jar["path"] for jar in inventory["jars"]])

    def test_a_run_does_not_analyze_jars_inside_its_own_output_directory(self) -> None:
        self.output = self.root / "output"
        self.write_jar("a.jar", b"a")
        self.write_jar("kept-by-an-earlier-run.jar", b"b", folder=self.output)
        status, _, _ = self.analyze()
        self.assertEqual(0, status)
        inventory = json.loads((self.output / "inventory.json").read_text(encoding="utf-8"))
        self.assertEqual(["a.jar"], [jar["path"] for jar in inventory["jars"]])

    def test_a_new_run_replaces_the_records_of_a_jar_whose_bytes_changed(self) -> None:
        jar = self.write_jar("a.jar", b"first build")
        self.assertEqual(0, self.analyze()[0])
        before = self.records("normalized")
        self.assertEqual(1, len(before))
        jar.write_bytes(b"second build")
        self.assertEqual(0, self.analyze()[0])
        after = self.records("normalized")
        self.assertEqual(1, len(after))
        self.assertNotEqual(before, after)
        self.assertEqual(after, self.records("raw"))

    def test_records_outside_the_json_files_of_the_output_are_kept(self) -> None:
        self.write_jar("a.jar", b"jar")
        (self.output / "normalized").mkdir(parents=True)
        (self.output / "normalized" / "note.txt").write_text("keep", encoding="utf-8")
        self.assertEqual(0, self.analyze()[0])
        self.assertTrue((self.output / "normalized" / "note.txt").exists())

    def test_two_copies_of_one_jar_produce_a_single_record(self) -> None:
        self.write_jar("a.jar", b"same bytes")
        self.write_jar("copy.jar", b"same bytes")
        status, out, _ = self.analyze()
        self.assertEqual(0, status)
        self.assertIn("copy.jar is a copy of a.jar", out)
        self.assertEqual(1, len(self.records("normalized")))

    def test_two_different_jars_of_one_release_fail_the_run_before_anything_is_written(self) -> None:
        self.write_jar("a.jar", b"a")
        self.write_jar("b.jar", b"b")

        def fake_analysis(path: Path, root: Path) -> dict:
            digest = analyzer.sha256(path)
            return {"path": path.name, "detected_version": "1.20.1", "sha256": digest,
                    "jar_id": analyzer.jar_id("1.20.1", digest)}

        with mock.patch.object(analyzer, "analyze_jar", fake_analysis):
            status, _, error = self.analyze()
        self.assertEqual(1, status)
        self.assertIn("1.20.1 comes from a.jar, b.jar", error)
        self.assertFalse(self.output.exists())


class ReleaseUniqueness(unittest.TestCase):
    def record(self, path: str, version: str | None, digest: str) -> dict:
        return {"path": path, "detected_version": version, "jar_id": analyzer.jar_id(version, digest)}

    def test_two_different_jars_of_one_release_are_refused(self) -> None:
        inventory = [self.record("a.jar", "1.20.1", "a" * 64), self.record("b.jar", "1.20.1", "b" * 64)]
        with self.assertRaises(analyzer.InputError) as raised:
            analyzer.one_record_per_release(inventory)
        self.assertIn("1.20.1 comes from a.jar, b.jar", str(raised.exception))

    def test_jars_without_a_detected_release_never_conflict(self) -> None:
        inventory = [self.record("a.jar", None, "a" * 64), self.record("b.jar", None, "b" * 64)]
        self.assertEqual(inventory, analyzer.one_record_per_release(inventory))


class RecordSelection(unittest.TestCase):
    def write_analysis(self, directory: Path, records: dict[str, dict]) -> None:
        normalized = directory / "normalized"
        normalized.mkdir()
        for name, record in records.items():
            (normalized / f"{name}.json").write_text(json.dumps(record), encoding="utf-8")

    def complete(self, version: str, **changes) -> dict:
        record = release(version, REQUIRED)
        record["extraction_complete"] = True
        record.update(changes)
        return record

    def test_identical_records_of_one_release_count_once(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            record = self.complete("1.20.1")
            self.write_analysis(Path(directory), {"x": record, "y": copy.deepcopy(record)})
            self.assertEqual([record], update.normalized_records(Path(directory), None))

    def test_different_records_of_one_release_are_refused_whatever_their_file_names(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            records = {"1.20.1-0aaa": self.complete("1.20.1", jar_id="1.20.1-0aaa"),
                       "1.20.1-fbbb": self.complete("1.20.1", jar_id="1.20.1-fbbb")}
            self.write_analysis(Path(directory), records)
            with self.assertRaises(update.WorkflowError) as raised:
                update.normalized_records(Path(directory), None)
            self.assertIn("1.20.1-0aaa.json, 1.20.1-fbbb.json", str(raised.exception))

    def test_a_stale_record_beside_an_incomplete_current_one_is_refused(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            records = {"old": self.complete("1.20.1", jar_id="old"),
                       "new": self.complete("1.20.1", jar_id="new", extraction_complete=False)}
            self.write_analysis(Path(directory), records)
            with self.assertRaises(update.WorkflowError):
                update.normalized_records(Path(directory), None)

    def test_a_conflicting_release_outside_the_selection_is_ignored(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            records = {"a": self.complete("1.20.1", jar_id="a"), "b": self.complete("1.20.1", jar_id="b"),
                       "c": self.complete("26.2")}
            self.write_analysis(Path(directory), records)
            kept = update.normalized_records(Path(directory), {"26.2"})
            self.assertEqual(["26.2"], [record["minecraft_version"] for record in kept])


class PreferenceEnrichmentRecord(unittest.TestCase):
    def write(self, directory: Path, records: dict[str, dict]) -> None:
        (directory / "normalized").mkdir()
        for name, record in records.items():
            (directory / "normalized" / f"{name}.json").write_text(json.dumps(record), encoding="utf-8")

    def document(self) -> dict:
        return dataset.merge(None, [release("26.2", REQUIRED)])[0]

    def test_different_records_of_the_canonical_release_are_refused(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            self.write(Path(directory), {"26.2-0aaa": {**release("26.2", REQUIRED), "jar_id": "0aaa"},
                                         "26.2-fbbb": {**release("26.2", REQUIRED), "jar_id": "fbbb"}})
            with self.assertRaises(preferences.PreferenceCatalogError):
                preferences._load_normalized_record(Path(directory), self.document())

    def test_identical_records_and_other_releases_resolve_to_one_record(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            record = release("26.2", REQUIRED)
            self.write(Path(directory), {"x": record, "y": copy.deepcopy(record), "z": release("1.20.1", REQUIRED)})
            self.assertEqual(record, preferences._load_normalized_record(Path(directory), self.document()))


class DatasetFoldDuplicates(unittest.TestCase):
    def test_different_analyses_of_one_release_are_refused(self) -> None:
        other = copy.deepcopy(REQUIRED)
        other[5]["category"] = "Accessibility"
        with self.assertRaises(dataset.DatasetError):
            dataset.merge(None, [release("26.2", REQUIRED), release("26.2", other)])

    def test_identical_analyses_of_one_release_fold_as_one(self) -> None:
        single, _ = dataset.merge(None, [release("26.2", REQUIRED)])
        doubled, _ = dataset.merge(None, [release("26.2", REQUIRED), release("26.2", REQUIRED)])
        self.assertEqual(single, doubled)

    def test_conflicting_analyses_are_refused_in_either_order(self) -> None:
        other = copy.deepcopy(REQUIRED)
        other[5]["category"] = "Accessibility"
        for records in ([release("26.2", REQUIRED), release("26.2", other)],
                        [release("26.2", other), release("26.2", REQUIRED)]):
            with self.assertRaises(dataset.DatasetError):
                dataset.merge(None, records)


if __name__ == "__main__":
    unittest.main()
