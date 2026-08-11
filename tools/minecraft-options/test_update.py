#!/usr/bin/env python3
"""Offline tests for the Minecraft options maintainer workflow."""

from __future__ import annotations

import copy
import sys
import unittest
from pathlib import Path

import dataset
import update
from test_dataset import REQUIRED, release, setting


class AnalyzerSelection(unittest.TestCase):
    def test_normal_updates_pass_only_the_selected_jar_to_the_analyzer(self):
        cache = Path("/tmp/ephervex-options-test-cache")
        jar = cache / "jars" / "mc-26.2.jar"
        command = update.analyzer_command(jar, cache / "analysis" / jar.stem, cache / "assets-cache")
        self.assertEqual(command[0], sys.executable)
        self.assertEqual(command[command.index("--input-jar") + 1], str(jar))
        self.assertEqual(command[command.index("--root") + 1], str(jar.parent))
        self.assertNotEqual(command[command.index("--root") + 1], str(cache / "jars") + "/")


class MaintainerSummary(unittest.TestCase):
    def build(self, settings: list[dict]) -> tuple[dict, list[dict]]:
        records = [release("26.2", settings)]
        document, _ = dataset.merge(None, records)
        return document, records

    def test_an_existing_schema_or_fully_expressible_new_schema_needs_only_a_feed_update(self):
        previous, _ = dataset.merge(None, [release("1.20.1", REQUIRED)])
        records = [release("1.20.2", REQUIRED)]
        document, changes = dataset.merge(previous, records)
        summary = update.summarize(document, previous, changes, records, None)
        self.assertIn("no Ephervex application update required for a mapping-only change", summary)
        self.assertTrue(any("mapped to existing schema" in line for line in summary))

        changed = copy.deepcopy(REQUIRED)
        changed[6] = setting(
            "renderDistance",
            kind="integer",
            numeric={"min": 2, "max": 64},
            domain_confidence="high",
        )
        records = [release("1.20.2", changed)]
        document, changes = dataset.merge(previous, records)
        summary = update.summarize(document, previous, changes, records, None)
        self.assertIn("no Ephervex application update required for a mapping-only change", summary)
        self.assertTrue(any("new schema" in line for line in summary))

    def test_an_unrecognized_managed_transform_requires_an_application_review(self):
        unresolved = copy.deepcopy(REQUIRED)
        unresolved[5] = setting("fov", kind="number", transform="pow(value, 2)")
        document, records = self.build(unresolved)
        self.assertEqual(
            update.application_review_reasons(records, document),
            [("fov", "unrecognized storage transform")],
        )
        summary = update.summarize(document, None, [], records, None)
        self.assertIn("Ephervex application review/update required:", summary)
        self.assertNotIn("no Ephervex application update required for a mapping-only change", summary)

    def test_a_contradictory_managed_setting_requires_an_application_review(self):
        contradictory = copy.deepcopy(REQUIRED)
        contradictory[7] = setting(
            "simulationDistance",
            kind="integer",
            numeric={"min": 2, "max": 5},
            domain_confidence="high",
            default=12,
            default_confidence="high",
        )
        document, records = self.build(contradictory)
        reasons = update.application_review_reasons(records, document)
        self.assertEqual(reasons[0][0], "simulationDistance")
        self.assertIn("contradictory evidence", reasons[0][1])

    def test_a_new_unexposed_setting_is_recorded_without_requiring_an_update(self):
        document, records = self.build(REQUIRED + [setting("newCurrentOption")])
        self.assertIn("newCurrentOption", document["current_canonical_setting_ids"])
        self.assertEqual(update.application_review_reasons(records, document), [])
        summary = update.summarize(document, None, [], records, None)
        self.assertTrue(any("recorded as data only" in line for line in summary))


if __name__ == "__main__":
    unittest.main()
