#!/usr/bin/env python3
"""Tests that generated files are replaced whole or not at all, and damaged inputs are described."""

from __future__ import annotations

import ast
import os
import re
import stat
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import file_io

TOOL_DIR = Path(__file__).resolve().parent.parent


class AtomicWrite(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.target = self.root / "nested" / "out.json"

    def leftovers(self) -> list[str]:
        return sorted(path.name for path in self.target.parent.iterdir() if path != self.target)

    def test_text_and_bytes_replace_the_previous_content_and_create_parents(self):
        file_io.write_atomic(self.target, "first\n")
        self.assertEqual(self.target.read_text(), "first\n")
        file_io.write_atomic(self.target, b"second")
        self.assertEqual(self.target.read_bytes(), b"second")
        self.assertEqual(self.leftovers(), [])

    def test_an_interrupted_replace_keeps_the_previous_file_and_leaves_no_temporary_file(self):
        file_io.write_atomic(self.target, "previous")
        with mock.patch.object(file_io.os, "replace", side_effect=OSError("killed")):
            with self.assertRaises(OSError):
                file_io.write_atomic(self.target, "replacement")
        self.assertEqual(self.target.read_text(), "previous")
        self.assertEqual(self.leftovers(), [])

    def test_a_failure_while_writing_keeps_the_previous_file_and_leaves_no_temporary_file(self):
        file_io.write_atomic(self.target, "previous")
        with mock.patch.object(file_io.os, "fsync", side_effect=OSError("disk full")):
            with self.assertRaises(OSError):
                file_io.write_atomic(self.target, "replacement")
        self.assertEqual(self.target.read_text(), "previous")
        self.assertEqual(self.leftovers(), [])

    def test_the_new_file_gets_the_mode_an_ordinary_write_would_give_it(self):
        previous = os.umask(0o022)
        self.addCleanup(os.umask, previous)
        file_io.write_atomic(self.target, "x")
        self.assertEqual(stat.S_IMODE(self.target.stat().st_mode), 0o644)

    def test_stable_json_is_sorted_indented_and_newline_terminated(self):
        file_io.stable_json(self.target, {"b": 1, "a": "é"})
        self.assertEqual(self.target.read_text(encoding="utf-8"), '{\n  "a": "é",\n  "b": 1\n}\n')


class Reading(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.path = Path(self.directory.name) / "data.json"

    def test_a_missing_file_reads_as_empty_text(self):
        self.assertEqual(file_io.read_text_or_empty(self.path), "")

    def test_undecodable_bytes_read_as_text_that_matches_nothing(self):
        self.path.write_bytes(b'{"a": "\xff')
        self.assertNotEqual(file_io.read_text_or_empty(self.path), '{"a": ""}\n')

    def test_a_damaged_json_file_is_described_with_its_name_and_path(self):
        for content in (b'{"truncated": ', b"", b"[]", b"\xff\xfe"):
            self.path.write_bytes(content)
            with self.subTest(content=content), self.assertRaises(ValueError) as caught:
                file_io.read_json_object(self.path, "the example file")
            self.assertIn("the example file", str(caught.exception))
            self.assertIn(str(self.path), str(caught.exception))

    def test_a_missing_json_file_is_described_instead_of_raising_file_not_found(self):
        with self.assertRaisesRegex(ValueError, "cannot be read"):
            file_io.read_json_object(self.path, "the example file")


def is_write_mode(text: str) -> bool:
    """Whether `text` is an `open` mode that can modify a file."""
    return re.fullmatch(r"[rwax](\+?[bt]?|[bt]\+)", text) is not None and (text[0] != "r" or "+" in text)


def direct_writes(source: str) -> list[str]:
    """The calls in `source` that write a file without going through `file_io`."""
    found = []
    for node in ast.walk(ast.parse(source)):
        if not isinstance(node, ast.Call) or not isinstance(node.func, ast.Attribute):
            continue
        name = node.func.attr
        modes = [
            argument.value
            for argument in node.args[:2]
            if isinstance(argument, ast.Constant) and isinstance(argument.value, str)
        ]
        if name in ("write_text", "write_bytes") or (name == "open" and any(is_write_mode(mode) for mode in modes)):
            found.append(f"line {node.lineno}: {name}")
    return found


class EveryOutputIsWrittenAtomically(unittest.TestCase):
    """The maintainer scripts write nothing except through `file_io.write_atomic`."""

    def test_the_scan_recognizes_direct_writes(self):
        source = "path.write_text('x')\npath.write_bytes(b'x')\npath.open('w')\npath.open('rb')\nzf.open('a.txt')\n"
        self.assertEqual(len(direct_writes(source)), 3)

    def test_no_script_writes_a_file_directly(self):
        offenders = []
        for source in sorted(TOOL_DIR.glob("*.py")):
            if source.name == "file_io.py":
                continue
            offenders += [f"{source.name} {found}" for found in direct_writes(source.read_text(encoding="utf-8"))]
        self.assertEqual(offenders, [], "write through file_io.write_atomic instead")


if __name__ == "__main__":
    unittest.main()
