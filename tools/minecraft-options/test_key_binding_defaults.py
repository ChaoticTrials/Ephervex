#!/usr/bin/env python3
"""Tests for reading a key binding's default and how it relates to the binding's codec.

    python3 -m unittest discover -s tools/minecraft-options

Offline and deterministic: the disassembly is written out here, so nothing
depends on a client JAR or on `javap`.
"""

from __future__ import annotations

import unittest

import analyze_options as analyzer
import dataset

OPTIONS = "net/minecraft/client/Options"
INPUT = "com/mojang/blaze3d/platform/InputConstants"
KEY = f"{INPUT}$Key"
KEY_TYPE = f"{INPUT}$Type"
CATEGORY = "net/minecraft/client/KeyMapping$Category"
MAPPING = "net/minecraft/client/KeyMapping"
MAPPING_INIT = f'Method {MAPPING}."<init>":(Ljava/lang/String;IL{CATEGORY};)V'
UNKNOWN_FIELD = f"Field {INPUT}.UNKNOWN:L{KEY};"
GET_VALUE = f"Method {KEY}.getValue:()I"
CATEGORY_FIELD = f"Field {CATEGORY}.MISC:L{CATEGORY};"


def member(signature: str, body: list[tuple[str, str]]) -> str:
    lines = [f"  {signature};", "    Code:"]
    for offset, (op, comment) in enumerate(body):
        arg, _, note = comment.partition("|")
        lines.append(f"      {offset:>4}: {op:<13} {arg}{'  // ' + note if note else ''}".rstrip())
    return "\n".join(lines)


class Classes(analyzer.Disassembler):
    def __init__(self, classes: dict[str, str]) -> None:
        super().__init__(None, set(classes))
        self.classes = classes

    def get(self, class_name: str) -> str:
        return self.classes.get(class_name, "")

    def verbose(self, class_name: str) -> str:
        return ""


def input_constants(*, value: tuple[str, str] = ("iconst_m1", ""), stores: int = 1) -> str:
    create = [("getstatic", f"#1|Field {KEY_TYPE}.KEYSYM:L{KEY_TYPE};"), value,
              ("invokevirtual", f"#2|Method {KEY_TYPE}.getOrCreate:(I)L{KEY};"),
              ("putstatic", f"#3|Field UNKNOWN:L{KEY};")]
    return member("static {}", create * stores + [("return", "")])


def key_type(*, registers_unknown: bool = True, registered_code: str = "iconst_m1") -> str:
    body = [("getstatic", f"#1|Field KEYSYM:L{KEY_TYPE};"),
            ("ldc", f"#2|String {'key.keyboard.unknown' if registers_unknown else 'key.keyboard.w'}"),
            (registered_code, ""),
            ("invokestatic", f"#3|Method addKey:(L{KEY_TYPE};Ljava/lang/String;I)V"),
            ("return", "")]
    return member("static {}", body)


def options(name: str, default: list[tuple[str, str]]) -> list[analyzer.Member]:
    body = [("new", f"#1|class {MAPPING}"), ("dup", ""), ("ldc", f"#2|String {name}"), *default,
            ("getstatic", f"#4|{CATEGORY_FIELD}"), ("invokespecial", f"#5|{MAPPING_INIT}"),
            ("putfield", "#6|Field binding:Lnet/minecraft/client/KeyMapping;"), ("return", "")]
    return analyzer.parse_members(member("public Options()", body))


UNKNOWN_DEFAULT = [("getstatic", f"#3|{UNKNOWN_FIELD}"), ("invokevirtual", f"#3|{GET_VALUE}")]


def extract(name: str, default: list[tuple[str, str]], classes: dict[str, str]) -> dict:
    found = analyzer.extract_keybinds(Classes(classes), options(name, default), False, OPTIONS)
    return next(kb for kb in found if kb["translation_key"] == name)


class UnboundDefaultDetection(unittest.TestCase):
    def test_the_unknown_key_constant_is_an_unbound_default_proven_from_its_own_initializer(self) -> None:
        kb = extract("key.smoothCamera", UNKNOWN_DEFAULT, {INPUT: input_constants(), KEY_TYPE: key_type()})

        self.assertTrue(kb["unbound"])
        self.assertEqual((kb["code"], kb["serialized"]), (-1, "key.keyboard.unknown"))
        self.assertEqual((kb["confidence"], kb["codec_confidence"]), ("high", "high"))

    def test_the_owner_of_the_constant_does_not_need_to_hold_the_unknown_key_name(self) -> None:
        classes = {INPUT: input_constants(), KEY_TYPE: key_type()}

        self.assertNotIn("key.keyboard.unknown", classes[INPUT])
        self.assertTrue(extract("key.smoothCamera", UNKNOWN_DEFAULT, classes)["unbound"])

    def test_a_constant_that_the_key_type_does_not_register_as_unknown_is_not_an_unbound_default(self) -> None:
        kb = extract("key.smoothCamera", UNKNOWN_DEFAULT,
                     {INPUT: input_constants(), KEY_TYPE: key_type(registers_unknown=False)})

        self.assertFalse(kb["unbound"])
        self.assertIsNone(kb["code"])
        self.assertEqual(kb["confidence"], "medium")

    def test_a_constant_created_from_another_code_is_not_an_unbound_default(self) -> None:
        kb = extract("key.smoothCamera", UNKNOWN_DEFAULT,
                     {INPUT: input_constants(value=("bipush", "87")), KEY_TYPE: key_type()})

        self.assertFalse(kb["unbound"])
        self.assertEqual(kb["confidence"], "medium")

    def test_the_unknown_name_registered_for_another_code_is_not_an_unbound_default(self) -> None:
        kb = extract("key.smoothCamera", UNKNOWN_DEFAULT,
                     {INPUT: input_constants(), KEY_TYPE: key_type(registered_code="iconst_0")})

        self.assertFalse(kb["unbound"])

    def test_a_constant_assigned_twice_is_not_an_unbound_default(self) -> None:
        kb = extract("key.smoothCamera", UNKNOWN_DEFAULT,
                     {INPUT: input_constants(stores=2), KEY_TYPE: key_type()})

        self.assertFalse(kb["unbound"])

    def test_a_constant_whose_class_cannot_be_read_is_not_an_unbound_default(self) -> None:
        kb = extract("key.smoothCamera", UNKNOWN_DEFAULT, {})

        self.assertFalse(kb["unbound"])
        self.assertIsNone(kb["code"])
        self.assertIsNotNone(kb["note"])


class CodecConfidenceIndependence(unittest.TestCase):
    def test_an_unreadable_default_does_not_lower_the_codec_confidence(self) -> None:
        kb = extract("key.smoothCamera", UNKNOWN_DEFAULT, {})

        self.assertEqual(kb["confidence"], "medium")
        self.assertEqual(kb["codec_confidence"], "high")

    def test_a_setting_takes_its_confidence_from_the_codec_and_keeps_the_default_confidence_apart(self) -> None:
        kb = extract("key.smoothCamera", UNKNOWN_DEFAULT, {})
        record = {"jar_id": "j", "detected_version": "26.2", "sha256": "0" * 64,
                  "options_analysis": {"keybinds": [kb], "keybinding_code_space": "glfw"}}

        setting = analyzer.normalize(record)["settings"][0]

        self.assertEqual((setting["confidence"], setting["default_confidence"]), ("high", "medium"))
        self.assertEqual(dataset.codec_for(setting, "glfw"), {"kind": "key_name_glfw"})

    def test_an_unverified_code_space_lowers_the_codec_confidence_too(self) -> None:
        kb = extract("key.smoothCamera", UNKNOWN_DEFAULT, {})

        analyzer.apply_key_code_check([kb], {"status": "unproven", "assumed": "glfw"})

        self.assertEqual(kb["codec_confidence"], "medium")


if __name__ == "__main__":
    unittest.main()
