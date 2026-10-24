#!/usr/bin/env python3
"""Tests for proving which numeric table a release's key binding defaults use.

    python3 -m unittest discover -s tools/minecraft-options

Offline and deterministic: the key bindings are written out here, so nothing
depends on a client JAR or on `javap`.
"""

from __future__ import annotations

import unittest

import analyze_options as analyzer
import dataset

GLFW_DEFAULTS = {"key.forward": 87, "key.left": 65, "key.back": 83, "key.right": 68,
                 "key.jump": 32, "key.inventory": 69, "key.drop": 81, "key.chat": 84,
                 "key.sneak": 340}
LWJGL2_DEFAULTS = {"key.forward": 17, "key.left": 30, "key.back": 31, "key.right": 32,
                   "key.jump": 57, "key.inventory": 18, "key.drop": 16, "key.chat": 20,
                   "key.sneak": 42}
GLFW_MOUSE_DEFAULTS = {"key.attack": 0, "key.use": 1, "key.pickItem": 2}
# The 26.3 client's key bindings: HID usage ids for keys and 1-based mouse buttons.
HID_KEYS = {"key.forward": 26, "key.left": 4, "key.back": 22, "key.right": 7, "key.jump": 44,
            "key.inventory": 8, "key.drop": 20, "key.chat": 23, "key.sneak": 225}
HID_MOUSE = {"key.attack": 1, "key.use": 3, "key.pickItem": 2}


def binding(name: str, code: int | None, input_type: str = "keyboard", unbound: bool = False) -> dict:
    return {"translation_key": name, "input_type": input_type, "code": code, "unbound": unbound,
            "serialized": None if code is None else "decoded", "confidence": "high",
            "codec_confidence": "high", "note": None}


def bindings(keys: dict[str, int], mouse: dict[str, int] | None = None) -> list[dict]:
    return ([binding(name, code) for name, code in keys.items()]
            + [binding(name, code, "mouse") for name, code in (mouse or {}).items()])


class KeyCodeSpaceVerification(unittest.TestCase):
    def test_default_codes_that_decode_to_their_keys_verify_a_glfw_release(self) -> None:
        check = analyzer.verify_key_code_space(bindings(GLFW_DEFAULTS, GLFW_MOUSE_DEFAULTS), "glfw")

        self.assertEqual(check["status"], "verified")
        self.assertEqual(check["mismatches"], [])
        self.assertEqual(check["anchors_proven"], 12)

    def test_default_codes_that_decode_to_their_keys_verify_a_legacy_release(self) -> None:
        check = analyzer.verify_key_code_space(bindings(LWJGL2_DEFAULTS), "lwjgl2")

        self.assertEqual(check["status"], "verified")
        self.assertEqual(check["anchors_proven"], 9)

    def test_scancode_style_codes_do_not_verify_as_glfw(self) -> None:
        check = analyzer.verify_key_code_space(bindings(HID_KEYS, HID_MOUSE), "glfw")

        self.assertEqual(check["status"], "mismatch")
        # Only the middle mouse button has the same number in both tables.
        self.assertEqual({m["binding"] for m in check["mismatches"]},
                         set(HID_KEYS) | (set(HID_MOUSE) - {"key.pickItem"}))
        jump = next(m for m in check["mismatches"] if m["binding"] == "key.jump")
        self.assertEqual((jump["code"], jump["expected_code"]), (44, 32))

    def test_one_contradicting_anchor_fails_a_release_whose_other_anchors_agree(self) -> None:
        keys = dict(GLFW_DEFAULTS, **{"key.jump": 44})

        check = analyzer.verify_key_code_space(bindings(keys, GLFW_MOUSE_DEFAULTS), "glfw")

        self.assertEqual(check["status"], "mismatch")
        self.assertEqual([m["binding"] for m in check["mismatches"]], ["key.jump"])

    def test_a_mouse_default_is_not_taken_for_the_keyboard_key_with_the_same_code(self) -> None:
        keys = dict(GLFW_DEFAULTS)
        records = bindings(keys, GLFW_MOUSE_DEFAULTS)
        next(r for r in records if r["translation_key"] == "key.attack")["input_type"] = "keyboard"

        check = analyzer.verify_key_code_space(records, "glfw")

        self.assertEqual([m["binding"] for m in check["mismatches"]], ["key.attack"])

    def test_too_few_readable_defaults_leave_the_code_space_unproven(self) -> None:
        few = {name: GLFW_DEFAULTS[name] for name in ("key.forward", "key.left", "key.jump")}

        check = analyzer.verify_key_code_space(bindings(few), "glfw")

        self.assertEqual(check["status"], "unproven")
        self.assertEqual(check["anchors_proven"], 3)

    def test_unreadable_and_unbound_defaults_prove_nothing_and_contradict_nothing(self) -> None:
        records = bindings(GLFW_DEFAULTS)
        by_name = {r["translation_key"]: r for r in records}
        by_name["key.chat"].update(code=None, serialized=None)
        by_name["key.drop"].update(code=-1, unbound=True)

        check = analyzer.verify_key_code_space(records, "glfw")

        self.assertEqual(check["status"], "verified")
        self.assertEqual(check["anchors_proven"], 7)

    def test_a_release_with_no_key_bindings_is_unproven(self) -> None:
        self.assertEqual(analyzer.verify_key_code_space([], "glfw")["status"], "unproven")


class UnverifiedCodeSpaceHandling(unittest.TestCase):
    def test_a_verified_space_is_recorded_and_its_bindings_are_untouched(self) -> None:
        records = bindings(GLFW_DEFAULTS, GLFW_MOUSE_DEFAULTS)
        check = analyzer.verify_key_code_space(records, "glfw")

        self.assertEqual(analyzer.apply_key_code_check(records, check), "glfw")
        self.assertTrue(all(r["confidence"] == r["codec_confidence"] == "high" and r["serialized"]
                            for r in records))

    def test_a_failed_check_records_an_unverified_space_and_demotes_every_binding(self) -> None:
        records = bindings(HID_KEYS, HID_MOUSE)
        check = analyzer.verify_key_code_space(records, "glfw")

        space = analyzer.apply_key_code_check(records, check)

        self.assertEqual(space, analyzer.UNVERIFIED_CODE_SPACE)
        for record in records:
            self.assertIsNone(record["serialized"], record["translation_key"])
            self.assertEqual(record["confidence"], "medium", record["translation_key"])
            self.assertEqual(record["codec_confidence"], "medium", record["translation_key"])
            self.assertIn("not verified", record["note"])
        self.assertEqual({r["code"] for r in records}, set(HID_KEYS.values()) | set(HID_MOUSE.values()))

    def test_an_unproven_check_also_demotes_the_bindings(self) -> None:
        records = bindings({"key.forward": 87})
        check = analyzer.verify_key_code_space(records, "glfw")

        self.assertEqual(analyzer.apply_key_code_check(records, check), analyzer.UNVERIFIED_CODE_SPACE)
        self.assertEqual(records[0]["confidence"], "medium")

    def test_an_unverified_space_gives_no_key_binding_codec(self) -> None:
        setting = {"persisted_key": "key_key.jump", "type": "key_binding", "confidence": "high",
                   "type_confidence": "high"}

        self.assertIsNone(dataset.codec_for(setting, analyzer.UNVERIFIED_CODE_SPACE))
        self.assertIsNotNone(dataset.representation_issue(setting, analyzer.UNVERIFIED_CODE_SPACE))


if __name__ == "__main__":
    unittest.main()
