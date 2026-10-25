#!/usr/bin/env python3
"""Tests for the provenance and legacy-release handling of the analyzer's evidence.

    python3 -m unittest discover -s tools/minecraft-options

Offline and deterministic: the disassembly is written out here, so nothing
depends on a client JAR or on `javap`.
"""

from __future__ import annotations

import unittest
from unittest import mock

import analyze_options as analyzer


def listing(signature: str, body: list[tuple[str, str]]) -> str:
    """One member in javap's format; each body entry is (mnemonic, `operand|comment`)."""
    lines = [f"  {signature};", "    Code:"]
    for offset, (op, comment) in enumerate(body):
        arg, _, note = comment.partition("|")
        lines.append(f"      {offset:>4}: {op:<13} {arg}{'  // ' + note if note else ''}".rstrip())
    return "\n".join(lines)


class FakeDisassembler:
    def __init__(self, classes: dict[str, str]) -> None:
        self.classes = classes

    def get(self, name: str) -> str:
        return self.classes.get(name, "")


def slider_constant(translation: str, bounds: tuple[str, str, str], field: str) -> list[tuple[str, str]]:
    """One enum constant: its caption and three float constants, then the store into its field."""
    floats = [("ldc", f"#3|float {b}f") for b in bounds]
    return [("new", "#1|class bid$a"), ("dup", ""), ("ldc", "#2|String NAME"),
            ("ldc", f"#4|String {translation}"), *floats,
            ("invokespecial", '#6|Method bid$a."<init>":(Ljava/lang/String;ILjava/lang/String;FFF)V'),
            ("putstatic", f"#7|Field {field}:Lbid$a;")]


class LegacySliderProvenance(unittest.TestCase):
    def ranges(self) -> dict:
        body = (slider_constant("options.sensitivity", ("0.0", "1.0", "0.0"), "a")
                + slider_constant("options.fov", ("30.0", "110.0", "1.0"), "b"))
        dis = FakeDisassembler({"bid$a": listing("static {}", body)})
        return analyzer.legacy_ui_ranges(dis, ["bid$a"])

    def test_each_slider_names_the_constant_its_own_constructor_stores(self) -> None:
        ranges = self.ranges()

        self.assertEqual(ranges["sensitivity"]["enum_constant"], "a")
        self.assertEqual(ranges["fov"]["enum_constant"], "b")

    def test_slider_bounds_stay_those_of_their_own_constant(self) -> None:
        fov = self.ranges()["fov"]

        self.assertEqual((fov["ui_min"], fov["ui_max"], fov["ui_step"]), (30.0, 110.0, 1.0))

    def test_a_slider_that_is_never_stored_names_no_constant(self) -> None:
        body = slider_constant("options.fov", ("30.0", "110.0", "1.0"), "b")[:-1]
        dis = FakeDisassembler({"bid$a": listing("static {}", body)})

        self.assertIsNone(analyzer.legacy_ui_ranges(dis, ["bid$a"])["fov"]["enum_constant"])


def enum_info(names: list[str | None]) -> dict:
    return {"class": "Sound", "constants": [{"serialized_name": n} for n in names]}


class DynamicFamilyEvidence(unittest.TestCase):
    CONSTANTS = {"Root": ["soundCategory_"]}
    DEFAULT = {"value": 1.0, "confidence": "high", "evidence": "fallback in a(qg)"}

    def family(self, names: list[str | None], default: dict | None = None) -> dict:
        with mock.patch.object(analyzer, "extract_enum", return_value=enum_info(names)):
            return analyzer.dynamic_family(None, "soundCategory_", "Sound", self.CONSTANTS, default)

    def test_a_proven_default_leaves_the_family_confidence_and_evidence_alone(self) -> None:
        plain = self.family(["master"])
        with_default = self.family(["master"], self.DEFAULT)

        self.assertEqual(with_default["confidence"], plain["confidence"])
        self.assertEqual(with_default["evidence"], plain["evidence"])
        self.assertEqual(with_default["default"], self.DEFAULT)

    def test_a_constant_without_a_serialized_name_lowers_confidence_and_is_counted(self) -> None:
        family = self.family(["master", None])

        self.assertEqual(family["names"], ["master"])
        self.assertEqual(family["unnamed_constants"], 1)
        self.assertEqual(family["confidence"], "medium")

    def test_a_fully_named_family_is_high_confidence(self) -> None:
        self.assertEqual(self.family(["master", "music"])["confidence"], "high")

    def test_a_family_whose_prefix_never_appears_is_absent(self) -> None:
        with mock.patch.object(analyzer, "extract_enum", return_value=enum_info(["master"])):
            self.assertIsNone(analyzer.dynamic_family(None, "modelPart_", "Part", self.CONSTANTS))


def sound_lookup(*extra: tuple[str, str], params: str = "qg") -> list:
    body = [("aload_0", ""), ("aload_1", ""),
            ("invokeinterface", "#1|InterfaceMethod java/util/Map.get:(Ljava/lang/Object;)Ljava/lang/Object;"),
            *extra, ("freturn", "")]
    return analyzer.parse_members(listing(f"public float a({params})", body))


class SoundDefaultProof(unittest.TestCase):
    def test_a_lookup_taking_the_category_and_falling_back_to_one_proves_the_default(self) -> None:
        proof = analyzer.sound_default_volume(sound_lookup(("fconst_1", "")), "qg")

        self.assertEqual((proof["value"], proof["confidence"]), (1.0, "high"))

    def test_a_member_that_does_not_take_the_category_proves_nothing(self) -> None:
        members = sound_lookup(("fconst_1", ""), params="java.lang.String")

        self.assertIsNone(analyzer.sound_default_volume(members, "qg")["value"])

    def test_another_fractional_constant_in_the_member_proves_nothing(self) -> None:
        members = sound_lookup(("fconst_1", ""), ("ldc", "#2|float 0.5f"))

        self.assertIsNone(analyzer.sound_default_volume(members, "qg")["value"])

    def test_a_member_without_a_lookup_proves_nothing(self) -> None:
        members = analyzer.parse_members(listing("public float a(qg)", [("fconst_1", ""), ("freturn", "")]))

        self.assertIsNone(analyzer.sound_default_volume(members, "qg")["value"])


class FamilySettingConfidence(unittest.TestCase):
    RECORD = {"sha256": "0" * 64, "jar_id": "j", "detected_version": "1.20.1"}

    def settings(self, family: dict) -> list[dict]:
        record = {**self.RECORD, "options_analysis": {"roots": ["Root"], "sound_family": family}}
        return [s for s in analyzer.normalize(record)["settings"] if s["persisted_key"].startswith("soundCategory_")]

    def test_sound_settings_carry_the_family_confidence(self) -> None:
        family = {"names": ["master"], "enum_class": "Sound", "confidence": "medium", "unnamed_constants": 1,
                  "default": {"value": 1.0, "confidence": "high", "evidence": "proof"}}
        setting = self.settings(family)[0]

        self.assertEqual((setting["confidence"], setting["domain_confidence"], setting["type_confidence"]),
                         ("medium", "medium", "medium"))
        self.assertEqual((setting["default"], setting["default_confidence"]), (1.0, "high"))

    def test_an_unproven_default_is_unknown_even_in_a_high_confidence_family(self) -> None:
        family = {"names": ["master"], "enum_class": "Sound", "confidence": "high",
                  "default": {"value": None, "confidence": "unknown"}}
        setting = self.settings(family)[0]

        self.assertEqual((setting["default"], setting["default_confidence"]), (None, "unknown"))
        self.assertEqual(setting["confidence"], "high")


class LambdaOwner(unittest.TestCase):
    DECODE = analyzer.parse_members(listing(
        "private static java.lang.Double lambda$new$1(int)",
        [("iload_0", ""), ("i2d", ""), ("ldc2_w", "#1|double 100.0d"), ("ddiv", ""),
         ("invokestatic", "#2|Method java/lang/Double.valueOf:(D)Ljava/lang/Double;"), ("areturn", "")]))

    def test_a_handle_into_the_owning_class_resolves_its_arithmetic(self) -> None:
        ops = analyzer.lambda_arithmetic(self.DECODE, "Options", "Options", "lambda$new$1",
                                         "(I)Ljava/lang/Double;")

        self.assertEqual(ops, [{"op": "/", "operand": 100.0}])

    def test_a_handle_into_another_class_is_not_matched_to_a_same_named_local_method(self) -> None:
        ops = analyzer.lambda_arithmetic(self.DECODE, "Options", "Other", "lambda$new$1",
                                         "(I)Ljava/lang/Double;")

        self.assertIsNone(ops)


def calls(*refs: tuple[str, str]) -> list[analyzer.Member]:
    body = [("aload_0", ""), *[(op, f"#1|{ref}") for op, ref in refs], ("return", "")]
    return analyzer.parse_members(listing("public void load()", body))


class JdkCallsAreNotVisitorCalls(unittest.TestCase):
    JDK = (("invokevirtual", "Method java/lang/String.indexOf:(Ljava/lang/String;I)I"),
           ("invokevirtual", "Method java/lang/String.replaceAll:(Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;"),
           ("invokevirtual", "Method java/util/Properties.getProperty:(Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;"))
    VISITOR = ("invokeinterface", "InterfaceMethod bid$b.a:(Ljava/lang/String;Z)Z")

    def test_jdk_calls_do_not_turn_a_direct_release_into_a_field_access_one(self) -> None:
        self.assertEqual(analyzer.detect_era(calls(*self.JDK))[0], "legacy_direct")
        self.assertEqual(analyzer.detect_era(calls(*self.JDK), {"bid$b"})[0], "legacy_direct")

    def test_a_visitor_call_on_a_client_class_is_still_a_field_access_era(self) -> None:
        members = calls(*self.JDK, self.VISITOR)

        self.assertEqual(analyzer.detect_era(members, {"bid$b"})[0], "field_access")
        self.assertEqual(analyzer.detect_era(members)[0], "field_access")

    def test_a_call_on_a_class_outside_the_client_is_ignored(self) -> None:
        self.assertEqual(analyzer.detect_era(calls(self.VISITOR), {"other"})[0], "legacy_direct")


def save_member(*appends: str) -> analyzer.Member:
    body = [("aload_2", ""), ("ldc", "#1|String lang:"),
            ("invokevirtual", "#2|Method java/lang/StringBuilder.append:(Ljava/lang/String;)Ljava/lang/StringBuilder;"),
            ("aload_0", ""), ("getfield", "#3|Field language:Ljava/lang/Object;")]
    body += [("invokevirtual", f"#4|Method java/lang/StringBuilder.append:({a})Ljava/lang/StringBuilder;")
             for a in appends]
    body += [("invokevirtual", "#5|Method java/lang/StringBuilder.toString:()Ljava/lang/String;"), ("return", "")]
    return analyzer.parse_members(listing("public void save()", body))[0]


class LegacySaveType(unittest.TestCase):
    def test_a_value_appended_as_an_unrecognized_type_leaves_the_save_type_unknown(self) -> None:
        for descriptor in ("Ljava/lang/Object;", "Ljava/lang/CharSequence;", "[C"):
            with self.subTest(descriptor=descriptor):
                saved = analyzer.legacy_save_blocks(save_member(descriptor))["lang"]
                self.assertIsNone(saved["save_type"])
                self.assertIsNone(saved["save_encoding"])

    def test_the_key_literal_append_never_types_the_value(self) -> None:
        self.assertIsNone(analyzer.legacy_save_blocks(save_member())["lang"]["save_type"])

    def test_a_value_appended_as_a_recognized_type_sets_the_save_type(self) -> None:
        saved = analyzer.legacy_save_blocks(save_member("I"))["lang"]

        self.assertEqual(saved["save_type"], "integer")

    def test_a_string_value_is_a_string(self) -> None:
        saved = analyzer.legacy_save_blocks(save_member("Ljava/lang/String;"))["lang"]

        self.assertEqual(saved["save_type"], "string")


LWJGL2_DEFAULTS = {"key.forward": 17, "key.left": 30, "key.back": 31, "key.right": 32,
                   "key.jump": 57, "key.inventory": 18, "key.drop": 16, "key.chat": 20,
                   "key.sneak": 42, "key.attack": -100, "key.use": -99, "key.pickItem": -98}


def key_mappings(defaults: dict[str, int]) -> list[analyzer.Member]:
    body: list[tuple[str, str]] = []
    for name, code in defaults.items():
        body += [("ldc", f"#1|String {name}"), ("bipush", str(code)),
                 ("ldc", "#2|String key.categories.gameplay"),
                 ("invokespecial", '#3|Method bib$k."<init>":(Ljava/lang/String;ILjava/lang/String;)V')]
    return analyzer.parse_members(listing("public bib(java.io.File)", body + [("return", "")]))


class LegacyKeyBindings(unittest.TestCase):
    def test_legacy_mouse_defaults_are_mouse_bindings(self) -> None:
        keybinds = analyzer.extract_keybinds(None, key_mappings(LWJGL2_DEFAULTS), True, None)
        types = {kb["translation_key"]: kb["input_type"] for kb in keybinds}

        self.assertEqual([types["key.attack"], types["key.use"], types["key.pickItem"]], ["mouse"] * 3)
        self.assertEqual(types["key.forward"], "keyboard")

    def test_a_modern_release_does_not_read_negative_codes_as_mouse_buttons(self) -> None:
        keybinds = analyzer.extract_keybinds(None, key_mappings({"key.forward": 87, "key.x": -1}), False, None)

        self.assertEqual({kb["input_type"] for kb in keybinds}, {"keyboard"})

    def test_an_unidentified_old_jar_is_proven_as_a_legacy_code_space(self) -> None:
        keybinds, space, check = analyzer.resolve_keybinds(None, key_mappings(LWJGL2_DEFAULTS), None, None)

        self.assertEqual((space, check["status"]), ("lwjgl2", "verified"))
        self.assertEqual({kb["translation_key"]: kb["serialized"] for kb in keybinds}["key.jump"], "57")

    def test_a_release_version_decides_its_code_space_without_trying_the_other(self) -> None:
        _, space, _ = analyzer.resolve_keybinds(None, key_mappings(LWJGL2_DEFAULTS), "1.20.1", None)

        self.assertEqual(space, analyzer.UNVERIFIED_CODE_SPACE)

    def test_an_unidentified_jar_whose_codes_prove_nothing_stays_unverified(self) -> None:
        _, space, _ = analyzer.resolve_keybinds(None, key_mappings({"key.a": 1000}), None, None)

        self.assertEqual(space, analyzer.UNVERIFIED_CODE_SPACE)


class KeyTables(unittest.TestCase):
    def test_glfw_names_function_keys_up_to_f25_the_menu_key_and_the_world_keys(self) -> None:
        names = {code: analyzer.key_file_name(code, "keyboard", "glfw") for code in (302, 314, 348, 161, 162, 283)}

        self.assertEqual(names, {302: "key.keyboard.f13", 314: "key.keyboard.f25", 348: "key.keyboard.menu",
                                 161: "key.keyboard.world.1", 162: "key.keyboard.world.2",
                                 283: "key.keyboard.print.screen"})

    def test_lwjgl2_names_the_keys_it_shares_with_glfw(self) -> None:
        names = {code: analyzer.key_file_name(code, "keyboard", "lwjgl2") for code in (100, 102, 183, 221)}

        self.assertEqual(names, {100: "key.keyboard.f13", 102: "key.keyboard.f15",
                                 183: "key.keyboard.print.screen", 221: "key.keyboard.menu"})

    def test_no_table_names_two_codes_alike(self) -> None:
        for space, table in analyzer.KEY_TABLES.items():
            with self.subTest(space=space):
                self.assertEqual(len(set(table.values())), len(table))


class SettingCategories(unittest.TestCase):
    def test_a_word_inside_an_unrelated_word_does_not_decide_the_category(self) -> None:
        self.assertEqual(analyzer.category_for("anaglyph3d"), "Video")
        self.assertEqual(analyzer.category_for("menuBackgroundBlurriness"), "Video")
        self.assertEqual(analyzer.category_for("realmsNotifications"), "Gameplay")

    def test_every_distance_setting_is_a_video_setting(self) -> None:
        for key in ("renderDistance", "simulationDistance", "entityDistanceScaling"):
            with self.subTest(key=key):
                self.assertEqual(analyzer.category_for(key), "Video")

    def test_frame_rate_limits_are_video_settings(self) -> None:
        self.assertEqual(analyzer.category_for("maxFps"), "Video")
        self.assertEqual(analyzer.category_for("inactivityFpsLimit"), "Video")

    def test_whole_word_matches_keep_the_established_groups(self) -> None:
        expected = {"soundCategory_master": "Sound & Music", "musicToast": "Sound & Music",
                    "key_key.jump": "Controls", "mouseSensitivity": "Controls", "sprintWindow": "Controls",
                    "lang": "Language / Accessibility", "forceUnicodeFont": "Language / Accessibility",
                    "chatScale": "Language / Accessibility", "textBackgroundOpacity": "Language / Accessibility",
                    "notificationDisplayTime": "Language / Accessibility", "touchscreen": "Video",
                    "fullscreenResolution": "Video", "guiScale": "Video", "difficulty": "Gameplay"}

        self.assertEqual({key: analyzer.category_for(key) for key in expected}, expected)


if __name__ == "__main__":
    unittest.main()
