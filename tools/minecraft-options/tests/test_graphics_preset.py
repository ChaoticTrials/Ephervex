#!/usr/bin/env python3
"""Tests for recovering which options a startup graphics preset overwrites.

    python3 -m unittest discover -s tools/minecraft-options

Offline and deterministic: the disassembly is written out here, so nothing
depends on a client JAR or on `javap`.
"""

from __future__ import annotations

import copy
import unittest

import analyze_options as analyzer
import dataset
from . import test_dataset as fixtures

OPTION = "net/minecraft/client/OptionInstance"
ROOT = "net/minecraft/client/Options"
PRESET_ENUM = "net/minecraft/client/GraphicsPreset"


def accessor(name: str, field: str) -> str:
    return f"""  public {OPTION}<java.lang.Object> {name}();
    Code:
       0: aload_0
       1: getfield      #2                  // Field {field}:L{OPTION};
       4: areturn
"""


def preset_enum(cases: tuple[int, ...] = (0, 1, 2), name: str = "a", calls: tuple[str, ...] = ("b", "c")) -> str:
    rows = "\n".join(f"                     {case}: {40 + case}" for case in cases)
    invokes = "\n".join(
        f"      {40 + 4 * i}: invokevirtual #9                  // Method {ROOT}.{call}:()L{OPTION};"
        for i, call in enumerate(calls)
    )
    return f"""  public static net.minecraft.client.GraphicsPreset valueOf(java.lang.String);
    Code:
       0: aload_0
       1: areturn

  public void {name}(net.minecraft.client.Minecraft);
    Code:
       0: aload_0
       1: invokevirtual #4                  // Method ordinal:()I
       4: tableswitch   {{ // 0 to {max(cases)}
{rows}
               default: 90
          }}
{invokes}
      90: return
"""


ROOT_TEXT = "\n".join([accessor("b", "cloudStatus"), accessor("c", "ambientOcclusion"), accessor("d", "fov")])
PRESET_VALUES = [
    {"ordinal": 0, "persisted": "fast"},
    {"ordinal": 1, "persisted": "fancy"},
    {"ordinal": 2, "persisted": "fabulous"},
    {"ordinal": 3, "persisted": "custom"},
]


def options() -> dict:
    return {
        "renderClouds": {"field": "cloudStatus"},
        "ao": {"field": "ambientOcclusion"},
        "fov": {"field": "fov"},
        "graphicsPreset": {
            "field": "graphicsPreset",
            "allowed_values": {"source_enum_class": PRESET_ENUM, "values": copy.deepcopy(PRESET_VALUES)},
        },
    }


class FakeDisassembler:
    def __init__(self, text: str) -> None:
        self.text = text

    def get(self, class_name: str) -> str:
        return self.text if class_name == PRESET_ENUM else ""


def extract(enum_text: str, opts: dict | None = None) -> dict | None:
    members = analyzer.parse_members(ROOT_TEXT)
    return analyzer.extract_preset_controls(FakeDisassembler(enum_text), opts or options(), members, OPTION, [ROOT])


class PresetControlExtraction(unittest.TestCase):
    def test_every_option_the_preset_sets_is_recovered_with_its_persisted_key(self) -> None:
        found = extract(preset_enum())

        self.assertEqual(found["controls"], ["ao", "renderClouds"])
        self.assertEqual(found["setting"], "graphicsPreset")

    def test_the_value_with_no_case_in_the_switch_is_the_one_that_applies_nothing(self) -> None:
        self.assertEqual(extract(preset_enum())["neutral"], "custom")

    def test_the_apply_method_is_found_by_its_shape_when_the_names_are_obfuscated(self) -> None:
        found = extract(preset_enum(name="zq"))

        self.assertEqual(found["controls"], ["ao", "renderClouds"])

    def test_a_release_without_a_preset_option_has_nothing_to_recover(self) -> None:
        opts = options()
        del opts["graphicsPreset"]

        self.assertIsNone(extract(preset_enum(), opts))

    def test_two_unapplied_preset_values_leave_the_neutral_value_unproven(self) -> None:
        found = extract(preset_enum(cases=(0, 1)))

        self.assertIn("error", found)
        self.assertNotIn("neutral", found)

    def test_an_accessor_that_is_not_a_persisted_option_leaves_the_controls_unproven(self) -> None:
        found = extract(preset_enum(calls=("b", "z")))

        self.assertIn("error", found)
        self.assertNotIn("controls", found)

    def test_a_preset_enum_that_does_not_switch_on_its_ordinal_is_not_guessed_at(self) -> None:
        found = extract("  public void a(net.minecraft.client.Minecraft);\n    Code:\n       0: return\n")

        self.assertIn("error", found)

    def test_an_unresolved_preset_makes_the_extraction_incomplete(self) -> None:
        gaps = analyzer.extraction_gaps({"graphics_preset": {"setting": "graphicsPreset", "error": "x"}})

        self.assertTrue(any("graphics preset controls unresolved" in gap for gap in gaps))


def preset_record(version: str, controls: list[str] | None = None, neutral: str = "custom") -> dict:
    settings = copy.deepcopy(fixtures.REQUIRED) + [
        fixtures.setting(
            "graphicsPreset", kind="enum", domain_confidence="high",
            allowed={"persisted_form": "string_token", "values": [{"persisted": v["persisted"]} for v in PRESET_VALUES]},
            quoting="json_string",
        ),
        fixtures.setting("ao"),
    ]
    record = fixtures.release(version, settings)
    record["graphics_preset"] = {
        "setting": "graphicsPreset",
        "neutral": neutral,
        "controls": controls if controls is not None else ["ao", "renderDistance"],
    }
    return record


class StartupPresetData(unittest.TestCase):
    def test_a_proven_preset_is_recorded_in_canonical_ids(self) -> None:
        document, _ = dataset.merge(None, [preset_record("26.2")])

        preset = document["schemas"][0]["startup_preset"]
        self.assertEqual(preset, {"setting": "graphicsPreset", "neutral": "custom", "controls": ["ao", "renderDistance"]})
        dataset.validate(document)

    def test_a_release_without_a_preset_records_none(self) -> None:
        document, _ = dataset.merge(None, [fixtures.release("1.20.1", fixtures.REQUIRED)])

        self.assertNotIn("startup_preset", document["schemas"][0])

    def test_an_unresolved_preset_records_none(self) -> None:
        record = preset_record("26.2")
        record["graphics_preset"] = {"setting": "graphicsPreset", "error": "not resolved"}

        document, _ = dataset.merge(None, [record])

        self.assertNotIn("startup_preset", document["schemas"][0])

    def test_a_neutral_value_the_setting_cannot_write_records_none(self) -> None:
        document, _ = dataset.merge(None, [preset_record("26.2", neutral="missing")])

        self.assertNotIn("startup_preset", document["schemas"][0])

    def test_two_releases_with_different_controls_do_not_share_a_family(self) -> None:
        document, _ = dataset.merge(None, [preset_record("26.1"), preset_record("26.2", controls=["ao"])])

        mapping = {entry["minecraft_version"]: entry["schema"] for entry in document["releases"]}
        self.assertNotEqual(mapping["26.1"], mapping["26.2"])

    def test_adding_a_preset_to_a_release_moves_it_to_another_family(self) -> None:
        plain = preset_record("26.2")
        del plain["graphics_preset"]
        first, _ = dataset.merge(None, [plain])
        second, _ = dataset.merge(first, [preset_record("26.2")])

        self.assertNotEqual(first["releases"][0]["schema"], second["releases"][0]["schema"])
        self.assertIn("startup_preset", second["schemas"][0])

    def test_folding_again_keeps_the_preset(self) -> None:
        first, _ = dataset.merge(None, [preset_record("26.2")])
        again, _ = dataset.merge(first, [preset_record("26.3")])

        self.assertEqual(again["schemas"][0].get("startup_preset"), first["schemas"][0]["startup_preset"])
        self.assertEqual(len(again["schemas"]), 1)

    def test_a_preset_naming_an_unsupported_setting_is_refused(self) -> None:
        document, _ = dataset.merge(None, [preset_record("26.2")])
        document["schemas"][0]["startup_preset"]["setting"] = "fov"

        with self.assertRaises(dataset.DatasetError):
            dataset.validate(document)

    def test_a_preset_with_a_neutral_value_its_setting_does_not_enumerate_is_refused(self) -> None:
        document, _ = dataset.merge(None, [preset_record("26.2")])
        document["schemas"][0]["startup_preset"]["neutral"] = "missing"

        with self.assertRaises(dataset.DatasetError):
            dataset.validate(document)

    def test_a_preset_that_controls_an_unknown_setting_is_refused(self) -> None:
        document, _ = dataset.merge(None, [preset_record("26.2")])
        document["schemas"][0]["startup_preset"]["controls"].append("nothing")

        with self.assertRaises(dataset.DatasetError):
            dataset.validate(document)

    def test_a_preset_that_controls_its_own_setting_is_refused(self) -> None:
        document, _ = dataset.merge(None, [preset_record("26.2")])
        document["schemas"][0]["startup_preset"]["controls"].append("graphicsPreset")

        with self.assertRaises(dataset.DatasetError):
            dataset.validate(document)


if __name__ == "__main__":
    unittest.main()
