#!/usr/bin/env python3
"""Tests that folding releases into a committed dataset is a pure step that keeps what earlier analyses proved."""

from __future__ import annotations

import copy
import unittest

import dataset
import update
from .test_dataset import REQUIRED, release, setting


def schema_of(document: dict, version: str) -> str:
    return next(r["schema"] for r in document["releases"] if r["minecraft_version"] == version)


def ids(document: dict) -> dict[str, str]:
    return {entry["key"]: entry["id"] for entry in document["setting_registry"]}


class RevisionTracksContent(unittest.TestCase):
    def test_folding_does_not_modify_the_dataset_it_starts_from(self):
        first, _ = dataset.merge(None, [release("26.2", REQUIRED)])
        snapshot = copy.deepcopy(first)
        changed = copy.deepcopy(REQUIRED)
        changed[5]["category"] = "Accessibility"
        changed[5]["ephervex_manageable"] = False
        dataset.merge(first, [release("26.2", changed)])
        self.assertEqual(first, snapshot)

    def test_a_registry_metadata_change_advances_the_revision(self):
        first, _ = dataset.merge(None, [release("26.2", REQUIRED)])
        for field, value in (
            ("category", "Accessibility"),
            ("translation_key", "options.renamedFov"),
            ("ephervex_manageable", False),
        ):
            changed = copy.deepcopy(REQUIRED)
            changed[5][field] = value
            second, _ = dataset.merge(first, [release("26.2", changed)])
            self.assertEqual(second["revision"], first["revision"] + 1, field)
            self.assertFalse(dataset.equivalent(first, second), field)

    def test_a_data_version_change_advances_the_revision_and_the_summary_says_so(self):
        records = [release("26.2", REQUIRED, data_version=100)]
        first, _ = dataset.merge(None, [release("26.2", REQUIRED)])
        second, changes = dataset.merge(first, records)
        self.assertEqual(second["revision"], first["revision"] + 1)
        summary = update.summarize(second, first, changes, records, None)
        self.assertNotIn("bundled dataset unchanged", summary)
        self.assertIn(f"bundled dataset revision {first['revision']} -> {second['revision']}", summary)

    def test_the_summary_reports_unchanged_only_when_the_revision_did_not_move(self):
        records = [release("26.2", REQUIRED)]
        first, _ = dataset.merge(None, records)
        again, changes = dataset.merge(first, records)
        summary = update.summarize(again, first, changes, records, None)
        self.assertIn("bundled dataset unchanged", summary)

    def test_a_changed_setting_definition_is_not_called_a_mapping_only_change(self):
        first, _ = dataset.merge(None, [release("26.2", REQUIRED)])
        changed = copy.deepcopy(REQUIRED)
        changed[5]["category"] = "Accessibility"
        records = [release("26.2", changed)]
        second, changes = dataset.merge(first, records)
        summary = update.summarize(second, first, changes, records, None)
        self.assertTrue(any("definition(s) changed: fov" in line for line in summary))
        self.assertNotIn("no Ephervex application update required for a mapping-only change", summary)

    def test_a_reanalysis_without_a_data_version_does_not_keep_the_earlier_one(self):
        first, _ = dataset.merge(None, [release("1.20.1", REQUIRED, data_version=3465)])
        second, _ = dataset.merge(first, [release("1.20.1", REQUIRED)])
        self.assertNotIn("data_version", second["releases"][0])
        self.assertEqual(second["revision"], first["revision"] + 1)


class SettingIdsComeFromTheCanonicalRelease(unittest.TestCase):
    OLD_ONLY = REQUIRED + [setting("au", "B"), setting("aF", "F")]

    def test_an_older_release_names_a_setting_by_its_persisted_key(self):
        committed, _ = dataset.merge(None, [release("26.2", REQUIRED)])
        folded, _ = dataset.merge(committed, [release("1.12.2", self.OLD_ONLY)])
        self.assertEqual(ids(folded)["au"], "au")
        self.assertEqual(ids(folded)["aF"], "aF")
        for key, setting_id in ids(committed).items():
            self.assertEqual(ids(folded)[key], setting_id)

    def test_folding_history_one_release_at_a_time_matches_one_run(self):
        history = [
            release("1.12.2", self.OLD_ONLY),
            release("1.13", self.OLD_ONLY),
            release("26.2", REQUIRED),
        ]
        whole, _ = dataset.merge(None, history)

        stepwise, _ = dataset.merge(None, [history[2]])
        for record in (history[1], history[0]):
            stepwise, _ = dataset.merge(stepwise, [record])
        self.assertEqual(ids(stepwise), ids(whole))
        self.assertEqual(ids(stepwise)["au"], "au")
        dataset.validate(stepwise)

    def test_a_newer_release_still_names_the_settings_it_introduces(self):
        committed, _ = dataset.merge(None, [release("1.20.1", REQUIRED)])
        folded, _ = dataset.merge(committed, [release("26.2", REQUIRED + [setting("obf", "newSetting")])])
        self.assertEqual(ids(folded)["obf"], "newSetting")


class RefoldingAnAnalyzedRelease(unittest.TestCase):
    HISTORY = [release("1.20.1", REQUIRED), release("1.21.1", REQUIRED), release("26.2", REQUIRED)]
    NEWER = release("26.3", REQUIRED + [setting("addedLater")])

    def committed(self) -> dict:
        document, _ = dataset.merge(None, self.HISTORY)
        return document

    def test_a_setting_registered_after_a_release_stays_unknown_for_it(self):
        after, _ = dataset.merge(self.committed(), [self.NEWER])
        schemas = {schema["id"]: schema for schema in after["schemas"]}
        older = schemas[schema_of(after, "1.20.1")]
        self.assertNotIn("addedLater", older["supported"])
        self.assertNotIn("addedLater", older["unsupported"])

    def test_refolding_a_release_after_the_registry_grew_changes_nothing(self):
        after, _ = dataset.merge(self.committed(), [self.NEWER])
        again, changes = dataset.merge(after, [self.HISTORY[1]])
        self.assertEqual(dataset.dumps(again), dataset.dumps(after))
        self.assertEqual([change["kind"] for change in changes], ["release-unchanged"])

    def test_folding_the_full_history_matches_folding_only_the_new_release(self):
        incremental, _ = dataset.merge(self.committed(), [self.NEWER])
        everything, changes = dataset.merge(self.committed(), [*self.HISTORY, self.NEWER])
        self.assertEqual(dataset.dumps(everything), dataset.dumps(incremental))
        self.assertEqual(
            [change["kind"] for change in changes if change["kind"] != "release-unchanged"],
            ["setting-added", "release-added"],
        )

    def test_separate_folds_of_releases_that_add_no_setting_match_one_combined_fold(self):
        committed = self.committed()
        widened = copy.deepcopy(REQUIRED)
        widened[6] = setting(
            "renderDistance", kind="integer", numeric={"min": 2, "max": 32}, domain_confidence="high"
        )
        later = [release("1.20.2", REQUIRED), release("1.20.3", widened)]
        together, _ = dataset.merge(committed, later)
        forward, _ = dataset.merge(dataset.merge(committed, [later[0]])[0], [later[1]])
        backward, _ = dataset.merge(dataset.merge(committed, [later[1]])[0], [later[0]])
        # Each separate fold publishes its own revision, so only the content is compared.
        self.assertTrue(dataset.equivalent(forward, together))
        self.assertTrue(dataset.equivalent(backward, together))

    def test_a_release_whose_own_extraction_changed_moves_to_another_family(self):
        committed = self.committed()
        widened = copy.deepcopy(REQUIRED)
        widened[6] = setting(
            "renderDistance", kind="integer", numeric={"min": 2, "max": 32}, domain_confidence="high"
        )
        after, changes = dataset.merge(committed, [release("1.21.1", widened)])
        self.assertNotEqual(schema_of(after, "1.21.1"), schema_of(committed, "1.21.1"))
        self.assertEqual(changes[-1]["kind"], "release-remapped")


if __name__ == "__main__":
    unittest.main()
