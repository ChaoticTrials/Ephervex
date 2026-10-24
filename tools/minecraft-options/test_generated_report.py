#!/usr/bin/env python3
"""Tests that the generated report and summary files state only what the analyzed data shows.

    python3 -m unittest discover -s tools/minecraft-options
"""

from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path
from typing import Any
from unittest import mock

import analyze_options as analyzer
from test_analyzer_inputs import run_main
from test_compatibility_matrix import OLDER
from test_compatibility_matrix import release as base_release
from test_compatibility_matrix import setting as base_setting

CANONICAL = "26.2"


def setting(key: str, kind: str = "string", **fields: Any) -> dict[str, Any]:
    """A normalized setting with the fields a report reads."""
    return base_setting(key, kind, **{"category": "Gameplay", "notes": [], **fields})


def release(version: str, *settings: dict[str, Any], complete: bool = True) -> dict[str, Any]:
    return {**base_release(version, *settings, complete=complete), "jar_id": version}


def binding(name: str, serialized: str | None = "key.keyboard.w", code: int | None = 87,
            unbound: bool = False) -> dict[str, Any]:
    return setting(f"key_key.{name}", "key_binding", translation_key=f"key.{name}",
                   default={"input_type": "keyboard", "code": code, "serialized": serialized,
                            "unbound": unbound})


def render(latest: dict[str, Any] | None, versions: list[dict[str, Any]] | None = None, *,
           languages: dict[str, Any] | None = None, command: str = "run") -> str:
    """The report of a canonical release compared with the given releases."""
    versions = versions if versions is not None else ([latest] if latest else [])
    matrix = analyzer.compatibility(latest, versions) if latest else None
    changes = analyzer.schema_changes(versions, {s["persisted_key"] for s in latest["settings"]}
                                      if latest else set())
    crosscheck = analyzer.keycode_crosscheck(versions)
    return analyzer.report([], latest, versions, languages, changes, matrix, crosscheck, command)


class KeyBindingEraCounts(unittest.TestCase):
    def test_an_era_spanning_releases_with_different_counts_reports_their_range(self) -> None:
        versions = [release("1.13", binding("a")), release("1.14", binding("a"), binding("b"), binding("c")),
                    release("1.15", binding("a"), binding("b"))]

        eras = analyzer.keybinding_eras(versions)

        self.assertEqual(1, len(eras))
        self.assertEqual((1, 3), (eras[0]["binding_count_min"], eras[0]["binding_count_max"]))

    def test_the_report_prints_the_range_rather_than_the_first_release_count(self) -> None:
        versions = [release("1.13", binding("a")), release("1.14", binding("a"), binding("b"))]

        text = render(versions[-1], versions)

        self.assertIn("1-2 bindings", text)

    def test_an_era_with_one_count_prints_that_count(self) -> None:
        versions = [release("1.13", binding("a")), release("1.14", binding("b"))]

        self.assertIn("1 bindings", render(versions[-1], versions))


class WriteAuthoritativeCounts(unittest.TestCase):
    def row(self, version: str, text: str) -> list[str]:
        line = next(line for line in text.splitlines() if line.startswith(f"| {version} |"))
        return [cell.strip() for cell in line.strip("|").split("|")]

    def test_only_high_confidence_managed_settings_count_as_write_authoritative(self) -> None:
        numeric = {"min": 1, "max": 2, "step": 1, "clamped": True}
        older = [setting("proven", "integer"), setting("unproven", "integer"),
                 setting("soundDevice"), setting("shown")]
        canonical = [setting("proven", "integer"),
                     setting("unproven", "integer", numeric=numeric),
                     setting("soundDevice", ephervex_manageable=False),
                     setting("shown")]
        versions = [release(OLDER, *older), release(CANONICAL, *canonical)]

        row = self.row(OLDER, render(versions[-1], versions))

        self.assertEqual(["1.19", "2", "4", "0", "0", "0"], row)

    def test_a_sound_setting_that_is_not_managed_has_no_safe_release(self) -> None:
        sound = {"category": "Sound & Music", "ephervex_manageable": False}
        versions = [release(OLDER, setting("soundDevice")),
                    release(CANONICAL, setting("soundDevice", **sound))]

        text = render(versions[-1], versions)

        self.assertIn("| `soundDevice` | `soundDevice` | - |", text)


class LanguageCatalogRange(unittest.TestCase):
    def test_the_range_names_the_releases_that_hold_the_extremes(self) -> None:
        counts = {"1.12.2": 5, "1.13": 9, "1.19.3": 12, "26.2": 7}
        languages = {"canonical_version": "26.2", "count": 7, "complete": True,
                     "per_version_catalog": {v: {"count": c} for v, c in counts.items()}}
        latest = release(CANONICAL, setting("lang"))

        text = render(latest, languages=languages)

        self.assertIn("range from **5** (1.12.2) to **12** (1.19.3)", text)


class FieldOfViewReport(unittest.TestCase):
    def fov(self, **fields: Any) -> dict[str, Any]:
        return setting("fov", "integer", numeric={"min": 30, "max": 110, "step": 1, "clamped": True},
                       value_to_storage="((value - 70) / 40)", **fields)

    def test_the_in_file_domain_is_the_ui_range_through_the_storage_transform(self) -> None:
        versions = [release("1.19", self.fov()), release(CANONICAL, self.fov())]

        text = render(versions[-1], versions)

        self.assertIn("in-file domain -1.0..1.0 (the 30..110 range through the storage transform)", text)

    def test_an_unread_transform_leaves_the_in_file_domain_unresolved(self) -> None:
        unread = setting("fov", "integer", numeric={"min": 30, "max": 110, "step": 1, "clamped": True},
                         transform_resolved=False)
        versions = [release("1.19", unread)]

        self.assertIn("in-file domain unresolved", render(versions[-1], versions))

    def test_a_field_stored_directly_keeps_its_codec_domain(self) -> None:
        direct = setting("fov", "integer", numeric={"min": 30, "max": 110, "step": 1, "clamped": True})
        versions = [release("1.19", direct)]

        self.assertIn("in-file domain 30..110 (codec-proven)", render(versions[-1], versions))

    def test_a_default_in_the_ui_space_is_labeled_as_such(self) -> None:
        versions = [release("1.19", self.fov(default=70, default_space="ui"))]

        self.assertIn("default 70 (UI value).", render(versions[-1], versions))

    def test_the_experiment_expects_the_values_the_recovered_transform_writes(self) -> None:
        versions = [release("1.18.2", self.fov()), release("1.19", self.fov())]

        text = render(versions[-1], versions)

        self.assertIn("Expect -1.0, 0.0 and 1.0. Repeat on 1.19 and expect the same values.", text)

    def test_the_experiment_names_different_values_for_a_release_with_another_transform(self) -> None:
        halved = setting("fov", "integer", numeric={"min": 30, "max": 110, "step": 1, "clamped": True},
                         value_to_storage="(value / 2)")
        versions = [release("1.18.2", self.fov()), release("1.19", halved)]

        text = render(versions[-1], versions)

        self.assertIn("Repeat on 1.19 and expect 15.0, 35.0 and 55.0.", text)

    def test_a_release_that_was_not_analyzed_has_no_expected_values(self) -> None:
        versions = [release("1.20", self.fov())]

        text = render(versions[-1], versions)

        self.assertIn("recovered no expected values for 1.18.2", text)
        self.assertIn("recovered no expected values for 1.19", text)


class KeycodeCrosscheck(unittest.TestCase):
    def check(self, *pairs: tuple[int, str, int | None, str | None]) -> dict[str, Any]:
        legacy = [setting(f"key_key.{n}", "key_binding", translation_key=f"key.{n}",
                          default={"code": code, "serialized": str(code), "unbound": False})
                  for n, (code, _, _, _) in enumerate(pairs)]
        modern = [setting(f"key_key.{n}", "key_binding", translation_key=f"key.{n}",
                          default={"code": new_code, "serialized": serialized, "unbound": False})
                  for n, (_, _, new_code, serialized) in enumerate(pairs)]
        versions = [{**release("1.12.2", *legacy), "keybinding_code_space": "lwjgl2"},
                    release("1.13", *modern)]
        return analyzer.keycode_crosscheck(versions)

    def test_an_unbound_binding_agrees_across_the_boundary(self) -> None:
        result = self.check((0, "", -1, "key.keyboard.unknown"), (17, "", 87, "key.keyboard.w"))

        self.assertEqual((2, 0, 0), (result["agreement_count"], result["disagreement_count"],
                                     result["unresolved_count"]))
        self.assertEqual("high", result["confidence"])

    def test_unresolved_pairs_keep_the_confidence_below_high(self) -> None:
        result = self.check((17, "", 87, "key.keyboard.w"), (-100, "", 0, "key.mouse.left"))

        self.assertEqual(1, result["unresolved_count"])
        self.assertEqual("medium", result["confidence"])

    def test_the_conversion_rule_states_the_code_an_unbound_binding_holds_before_the_boundary(self) -> None:
        rule = self.check((17, "", 87, "key.keyboard.w"))["conversion_rule"]

        self.assertIn("unbound binding is 0 before 1.13", rule)
        self.assertNotIn("-1 means", rule)


class ReportOfAnyExtraction(unittest.TestCase):
    def test_a_canonical_release_without_settings_still_produces_a_report(self) -> None:
        text = render(release(CANONICAL))

        self.assertIn("Fully typed: **0/0** (n/a)", text)
        self.assertIn("Known defaults: **0/0** (n/a)", text)

    def test_the_summary_says_why_there_is_no_canonical_release(self) -> None:
        text = analyzer.report([], None, [], None, None, None, None, "run",
                               "The newest stable supported release 26.2 was not completely extracted")

        self.assertIn("Canonical release: **none**. The newest stable supported release 26.2 was not "
                      "completely extracted.", text)

    def test_a_partly_extracted_newest_release_is_not_replaced_by_an_older_one(self) -> None:
        inventory = [{"detected_version": "1.20.1", "stable_release": True, "jar_id": "old"},
                     {"detected_version": "26.2", "stable_release": True, "jar_id": "new"}]
        normalized = [{"jar_id": "old", "extraction_complete": True},
                      {"jar_id": "new", "extraction_complete": False}]

        newest, record, problem = analyzer.canonical_release(inventory, normalized)

        self.assertIsNone(newest)
        self.assertIsNone(record)
        self.assertIn("26.2 was not completely extracted", problem)

    def test_a_completely_extracted_newest_release_is_the_canonical_release(self) -> None:
        inventory = [{"detected_version": "1.20.1", "stable_release": True, "jar_id": "old"},
                     {"detected_version": "26.2", "stable_release": True, "jar_id": "new"},
                     {"detected_version": "26.3", "stable_release": False, "jar_id": "snapshot"}]
        normalized = [{"jar_id": "old", "extraction_complete": True},
                      {"jar_id": "new", "extraction_complete": True}]

        newest, record, problem = analyzer.canonical_release(inventory, normalized)

        self.assertEqual("new", newest["jar_id"])
        self.assertEqual(normalized[1], record)
        self.assertIsNone(problem)


class SchemaChangeEntries(unittest.TestCase):
    def changes(self, older: dict[str, Any], newer: dict[str, Any], **code_spaces: str) -> dict[str, Any]:
        versions = [{**release(OLDER, older), "keybinding_code_space": code_spaces.get("before", "glfw")},
                    {**release(CANONICAL, newer), "keybinding_code_space": code_spaces.get("after", "glfw")}]
        return analyzer.schema_changes(versions, {newer["persisted_key"]})["changes"][0]

    def enum(self, *tokens: Any, extra: str = "") -> dict[str, Any]:
        return setting("mainHand", "enum", allowed_values={
            "persisted_form": "string_token",
            "values": [{"persisted": token, "label": extra + str(token)} for token in tokens]})

    def test_an_enum_whose_tokens_did_not_change_is_not_a_change(self) -> None:
        result = self.changes(self.enum("left", "right", extra="a"), self.enum("left", "right", extra="b"))

        self.assertEqual([], result["enum_changes"])

    def test_an_enum_whose_tokens_changed_lists_them_on_both_sides(self) -> None:
        result = self.changes(self.enum("left"), self.enum("left", "right"))

        self.assertEqual([{"key": "mainHand", "from": ["left"], "to": ["left", "right"]}],
                         result["enum_changes"])

    def test_an_enum_with_an_unread_token_is_omitted(self) -> None:
        result = self.changes(self.enum(None, None), self.enum("left", "right"))

        self.assertEqual([], result["enum_changes"])

    def test_a_default_reached_through_the_ui_space_is_compared_in_the_file_space(self) -> None:
        older = setting("fov", "number", default=0.0, default_space="file", value_to_storage="((value - 70) / 40)")
        newer = setting("fov", "integer", default=70, default_space="ui", value_to_storage="((value - 70) / 40)")

        self.assertEqual([], self.changes(older, newer)["default_changes"])

    def test_a_default_that_differs_only_by_float_noise_is_not_a_change(self) -> None:
        older = setting("chatHeightUnfocused", "number", default=0.44366196, default_space="file")
        newer = setting("chatHeightUnfocused", "number", default=0.44366195797920227, default_space="file")

        self.assertEqual([], self.changes(older, newer)["default_changes"])

    def test_a_different_default_is_a_change(self) -> None:
        older = setting("autoJump", "boolean", default=True, default_space="file")
        newer = setting("autoJump", "boolean", default=False, default_space="file")

        self.assertEqual([{"key": "autoJump", "from": True, "to": False}],
                         self.changes(older, newer)["default_changes"])

    def test_a_ui_space_default_behind_an_unread_transform_is_not_compared(self) -> None:
        older = setting("fov", "number", default=0.0, default_space="file")
        newer = setting("fov", "integer", default=70, default_space="ui")

        self.assertEqual([], self.changes(older, newer)["default_changes"])

    def test_a_key_binding_default_that_is_unbound_in_both_releases_is_not_a_change(self) -> None:
        older = binding("smoothCamera", "key.keyboard.unknown", -1, unbound=False)
        newer = binding("smoothCamera", "key.keyboard.unknown", -1, unbound=True)

        self.assertEqual([], self.changes(older, newer)["default_changes"])

    def test_a_key_binding_default_is_not_compared_across_a_code_space_change(self) -> None:
        older = binding("forward", "17", 17)
        newer = binding("forward", "key.keyboard.w", 87)

        result = self.changes(older, newer, before="lwjgl2")

        self.assertEqual([], result["default_changes"])

    def test_a_key_binding_default_that_moved_within_a_code_space_is_a_change(self) -> None:
        older = binding("forward", "key.keyboard.w", 87)
        newer = binding("forward", "key.keyboard.up", 265)

        self.assertEqual(["key_key.forward"],
                         [c["key"] for c in self.changes(older, newer)["default_changes"]])

    def test_a_key_the_same_single_caption_renamed_is_listed_as_a_rename(self) -> None:
        older = setting("oldName", translation_key="options.example")
        newer = setting("newName", translation_key="options.example")

        result = self.changes(older, newer)

        self.assertEqual([{"from": "oldName", "to": "newName", "translation_key": "options.example"}],
                         result["persisted_key_renames"])

    def test_a_caption_shared_by_two_current_settings_is_not_a_rename(self) -> None:
        versions = [release(OLDER, setting("oldName", translation_key="options.example")),
                    release(CANONICAL, setting("firstName", translation_key="options.example"),
                            setting("secondName", translation_key="options.example"))]

        result = analyzer.schema_changes(versions, {"firstName"})["changes"][0]

        self.assertEqual([], result["persisted_key_renames"])


class LanguageOutputClaims(unittest.TestCase):
    def output(self, cache: dict[str, Any], releases: list[dict[str, Any]]) -> dict[str, Any]:
        from zipfile import ZipFile
        with tempfile.TemporaryDirectory() as directory:
            jar = Path(directory) / "mc.jar"
            with ZipFile(jar, "w") as archive:
                archive.writestr("assets/minecraft/lang/en_us.json", "{}")
            return analyzer.language_output({"detected_version": CANONICAL, "sha256": "0" * 64}, jar,
                                            releases[-1], cache, releases)

    def verified(self, *codes: str) -> dict[str, Any]:
        return {"status": "ok", "languages": {code: {"name": code} for code in codes},
                "pack_mcmeta": {"sha1": "a" * 40, "sha1_matches": True}}

    def test_a_verified_catalog_without_languages_is_not_complete(self) -> None:
        output = self.output({CANONICAL: self.verified()}, [release(CANONICAL, setting("lang"))])

        self.assertFalse(output["complete"])

    def test_without_a_verified_canonical_catalog_no_code_is_called_missing_from_it(self) -> None:
        releases = [release(OLDER, setting("lang")), release(CANONICAL, setting("lang"))]

        output = self.output({OLDER: self.verified("en_us", "de_de")}, releases)

        entry = output["per_version_catalog"][OLDER]
        self.assertIsNone(entry["missing_from_canonical"])
        self.assertIsNone(entry["absent_here_but_canonical"])

    def test_codes_are_compared_with_a_verified_canonical_catalog(self) -> None:
        releases = [release(OLDER, setting("lang")), release(CANONICAL, setting("lang"))]
        cache = {OLDER: self.verified("en_us", "old_xx"), CANONICAL: self.verified("en_us", "de_de")}

        entry = self.output(cache, releases)["per_version_catalog"][OLDER]

        self.assertEqual((["old_xx"], ["de_de"]),
                         (entry["missing_from_canonical"], entry["absent_here_but_canonical"]))

    def test_the_lang_key_is_called_stable_only_when_every_release_stores_it_alike(self) -> None:
        same = [release(OLDER, setting("lang")), release(CANONICAL, setting("lang"))]
        changed = [release(OLDER, setting("lang", file_encoding="legacy text")),
                   release(CANONICAL, setting("lang"))]

        stable = self.output({}, same)["older_compatibility"]
        varies = self.output({}, changed)["older_compatibility"]

        self.assertEqual("resolved", stable["status"])
        self.assertEqual("varies", varies["status"])
        self.assertIn("1.19: string, legacy text", varies["detail"])

    def test_a_release_without_the_lang_key_is_not_called_stable(self) -> None:
        releases = [release(OLDER), release(CANONICAL, setting("lang"))]

        self.assertEqual("varies", self.output({}, releases)["older_compatibility"]["status"])

    def test_per_code_support_names_the_releases_whose_catalog_is_not_verified(self) -> None:
        releases = [release(OLDER, setting("lang")), release(CANONICAL, setting("lang"))]

        support = self.output({CANONICAL: self.verified("en_us")}, releases)["older_compatibility"][
            "per_code_support"]

        self.assertEqual("not verified for 1.19", support)


class LatestSchemaCounts(unittest.TestCase):
    def settings(self) -> list[dict[str, Any]]:
        return [setting("fullscreen", "boolean"),
                setting("renderDistance", "integer", numeric={"min": 2, "max": 32, "step": 1, "clamped": True},
                        default=12),
                binding("forward"), binding("unresolved", serialized=None),
                setting("mainHand", "enum", allowed_values={"persisted_form": "string_token",
                                                            "values": [{"persisted": "left"}]},
                        default="right"),
                setting("resourcePacks", "string")]

    def latest(self) -> dict[str, Any]:
        return {**release(CANONICAL, *self.settings()), "jar_sha256": "0" * 64, "unknown_keys_policy": "keep"}

    def test_a_key_binding_without_a_serialized_default_has_no_known_default(self) -> None:
        schema = analyzer.make_latest_schema(self.latest())

        self.assertEqual(3, schema["with_default_count"])

    def test_the_counts_match_the_ones_the_report_prints(self) -> None:
        latest = self.latest()
        schema = analyzer.make_latest_schema(latest)

        text = render(latest)

        total = schema["setting_count"]
        self.assertIn(f"Known defaults: **{schema['with_default_count']}/{total}**", text)
        self.assertIn(f"Proven domain (numeric range, enum domain or boolean/key-binding domain): "
                      f"**{schema['with_domain_count']}/{total}**", text)


class GeneratedFilesOfARun(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.base = Path(self.directory.name)
        self.root = self.base / "jar files"
        self.root.mkdir()
        self.output = self.base / "output dir"

    def analyze(self) -> tuple[int, str]:
        status, _, error = run_main("--root", str(self.root), "--output", str(self.output),
                                    "--assets-cache", str(self.base / "assets cache"))
        return status, error

    def report(self) -> str:
        return (self.output / "report.md").read_text(encoding="utf-8")

    def test_the_rerun_command_repeats_every_path_and_quotes_it_for_the_shell(self) -> None:
        (self.root / "a.jar").write_bytes(b"not a zip")

        self.assertEqual(0, self.analyze()[0])

        command = self.report().split("```bash\n")[1].split("\n```")[0]
        self.assertIn(f"--root '{self.root.resolve()}'", command)
        self.assertIn(f"--output '{self.output.resolve()}'", command)
        self.assertIn(f"--assets-cache '{(self.base / 'assets cache').resolve()}'", command)

    def test_a_run_without_a_canonical_release_says_so_in_every_generated_file(self) -> None:
        (self.root / "a.jar").write_bytes(b"not a zip")

        self.assertEqual(0, self.analyze()[0])

        self.assertIn("Canonical release: **none**. No confidently identified stable supported release",
                      self.report())
        latest = json.loads((self.output / "latest-schema.json").read_text(encoding="utf-8"))
        self.assertEqual({"error": "No confidently identified stable supported release"}, latest)

    def test_a_partly_extracted_newest_release_completes_without_becoming_the_baseline(self) -> None:
        (self.root / "a.jar").write_bytes(b"jar")

        def fake_analysis(path: Path, root: Path) -> dict[str, Any]:
            digest = analyzer.sha256(path)
            return {"path": path.name, "detected_version": CANONICAL, "stable_release": True, "sha256": digest,
                    "jar_id": analyzer.jar_id(CANONICAL, digest)}

        def fake_normalize(record: dict[str, Any]) -> dict[str, Any]:
            return {**release(CANONICAL, setting("fullscreen", "boolean"), complete=False),
                    "jar_id": record["jar_id"]}

        with mock.patch.object(analyzer, "analyze_jar", fake_analysis), \
                mock.patch.object(analyzer, "normalize", fake_normalize):
            status, _ = self.analyze()

        self.assertEqual(0, status)
        self.assertIn("The newest stable supported release 26.2 was not completely extracted", self.report())
        latest = json.loads((self.output / "latest-schema.json").read_text(encoding="utf-8"))
        self.assertEqual({"error": "The newest stable supported release 26.2 was not completely extracted"},
                         latest)
        matrix = json.loads((self.output / "compatibility-matrix.json").read_text(encoding="utf-8"))
        self.assertEqual({"error": "No canonical release"}, matrix)


class FormulaEvaluation(unittest.TestCase):
    def test_a_recovered_formula_is_evaluated(self) -> None:
        self.assertEqual(-1.0, analyzer.evaluate_formula("((value - 70) / 40)", "value", 30))
        self.assertEqual(0.0, analyzer.evaluate_formula("((value - 70) / 40)", "value", 70))

    def test_text_that_is_not_plain_arithmetic_over_the_variable_is_not_evaluated(self) -> None:
        for expression in (None, "", "other + 1", "__import__('os').getcwd()", "value ** 2", "value / 0",
                           "(value", "value if value else 0", "[value]"):
            self.assertIsNone(analyzer.evaluate_formula(expression, "value", 3), expression)

    def test_a_negative_operand_is_evaluated(self) -> None:
        self.assertEqual(7.0, analyzer.evaluate_formula("(value * -1)", "value", -7))


if __name__ == "__main__":
    unittest.main()
