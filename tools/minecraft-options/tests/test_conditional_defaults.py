#!/usr/bin/env python3
"""Tests that a default is only proven from a constant no condition chooses.

    python3 -m unittest discover -s tools/minecraft-options

Offline and deterministic: the disassembly is written out here. Jump operands
are instruction indexes, because every instruction in these listings is one
byte long.
"""

from __future__ import annotations

import unittest
from unittest import mock

import analyze_options as analyzer

OPTIONS = "net/minecraft/client/Options"
OPTION = "net/minecraft/client/OptionInstance"
INT_VALUE_OF = "Method java/lang/Integer.valueOf:(I)Ljava/lang/Integer;"
BOOLEAN_FACTORY = f"Method {OPTION}.createBoolean:(Ljava/lang/String;Z)L{OPTION};"
PLATFORM = "Method net/minecraft/Util.getPlatform:()Lnet/minecraft/Util$OS;"
WINDOWS = "Field net/minecraft/Util$OS.WINDOWS:Lnet/minecraft/Util$OS;"
FIELD = "Field syncWrites:Z"
INT_FIELD = "Field renderDistance:I"
SIDE_EFFECT = f"Method {OPTIONS}.reset:()V"


def listing(body: list[tuple[str, str]]) -> analyzer.Member:
    lines = ["  public Options();", "    Code:"]
    for offset, (op, comment) in enumerate(body):
        arg, _, note = comment.partition("|")
        lines.append(f"      {offset:>4}: {op:<13} {arg}{'  // ' + note if note else ''}".rstrip())
    return analyzer.parse_members("\n".join(lines))[0]


def initials(body: list[tuple[str, str]]) -> dict:
    return analyzer.field_initial_values([listing(body)])


class StubDisassembly(analyzer.Disassembler):
    def __init__(self) -> None:
        super().__init__(None, {OPTIONS})

    def get(self, class_name: str) -> str:
        return ""

    def verbose(self, class_name: str) -> str:
        return ""


def analyse(body: list[tuple[str, str]]) -> dict:
    window = listing(body).insns
    return analyzer.analyse_option_window(StubDisassembly(), window, OPTION, [], OPTIONS)


def boolean_store(*, branch_taken_value: int, other_value: int) -> list[tuple[str, str]]:
    """`this.syncWrites = Util.getPlatform() == Util.OS.WINDOWS`."""
    return [
        ("aload_0", ""),
        ("invokestatic", f"#1|{PLATFORM}"),
        ("getstatic", f"#2|{WINDOWS}"),
        ("if_acmpne", "6"),
        (f"iconst_{branch_taken_value}", ""),
        ("goto", "7"),
        (f"iconst_{other_value}", ""),
        ("putfield", f"#3|{FIELD}"),
        ("return", ""),
    ]


class ConstructorFieldDefaults(unittest.TestCase):
    def test_a_constant_chosen_by_a_condition_is_not_a_default(self) -> None:
        found = initials(boolean_store(branch_taken_value=1, other_value=0))
        self.assertEqual(found["syncWrites"]["kind"], "conditional")
        value, confidence, note = analyzer.resolve_default({}, found["syncWrites"], None)
        self.assertEqual((value, confidence), (None, "unknown"))
        self.assertEqual(note, analyzer.CONDITIONAL_DEFAULT_NOTE)

    def test_a_constant_that_starts_the_straight_line_is_a_default(self) -> None:
        found = initials([
            ("aload_0", ""), ("iconst_1", ""), ("putfield", f"#1|{FIELD}"),
            ("aload_0", ""), ("bipush", "12"), ("putfield", f"#2|{INT_FIELD}"),
            ("return", ""),
        ])
        self.assertEqual(found["syncWrites"], {"kind": "number", "value": 1.0, "descriptor": "Z"})
        self.assertEqual(found["renderDistance"]["value"], 12.0)

    def test_a_constant_after_an_unrelated_completed_branch_is_a_default(self) -> None:
        found = initials([
            ("iload_1", ""), ("ifeq", "4"), ("aload_0", ""), ("invokevirtual", f"#1|{SIDE_EFFECT}"),
            ("aload_0", ""), ("bipush", "12"), ("putfield", f"#2|{INT_FIELD}"),
            ("return", ""),
        ])
        self.assertEqual(found["renderDistance"]["kind"], "number")

    def test_a_constant_assigned_only_when_a_test_passes_is_not_a_default(self) -> None:
        found = initials([
            ("iload_1", ""), ("ifeq", "5"), ("aload_0", ""), ("bipush", "12"),
            ("putfield", f"#1|{INT_FIELD}"), ("nop", ""), ("return", ""),
        ])
        self.assertEqual(found["renderDistance"]["kind"], "conditional")

    def test_a_field_assigned_twice_has_no_default_from_the_last_write(self) -> None:
        found = initials([
            ("aload_0", ""), ("bipush", "8"), ("putfield", f"#1|{INT_FIELD}"),
            ("aload_0", ""), ("bipush", "12"), ("putfield", f"#1|{INT_FIELD}"),
            ("return", ""),
        ])
        self.assertEqual(found["renderDistance"]["kind"], "conditional")

    def test_a_constant_followed_by_a_computed_write_is_not_a_default(self) -> None:
        found = initials([
            ("aload_0", ""), ("bipush", "8"), ("putfield", f"#1|{INT_FIELD}"),
            ("aload_0", ""), ("invokestatic", "#2|Method java/lang/Runtime.version:()I"),
            ("putfield", f"#1|{INT_FIELD}"), ("return", ""),
        ])
        self.assertEqual(found["renderDistance"]["kind"], "conditional")

    def test_an_enum_constant_chosen_by_a_condition_is_not_a_default(self) -> None:
        found = initials([
            ("aload_0", ""), ("iload_1", ""), ("ifeq", "5"),
            ("getstatic", "#1|Field net/minecraft/Mode.FAST:Lnet/minecraft/Mode;"), ("goto", "6"),
            ("getstatic", "#2|Field net/minecraft/Mode.FANCY:Lnet/minecraft/Mode;"),
            ("putfield", "#3|Field mode:Lnet/minecraft/Mode;"), ("return", ""),
        ])
        self.assertEqual(found["mode"]["kind"], "conditional")

    def test_a_switch_leaves_later_constants_unproven(self) -> None:
        found = initials([
            ("iload_1", ""), ("tableswitch", "{ // 0 to 0"), ("aload_0", ""), ("bipush", "12"),
            ("putfield", f"#1|{INT_FIELD}"), ("return", ""),
        ])
        self.assertEqual(found["renderDistance"]["kind"], "conditional")


class OptionInstanceDefaults(unittest.TestCase):
    @staticmethod
    def render_distance(branch_value: int, other_value: int) -> list[tuple[str, str]]:
        """`new OptionInstance(..., is64Bit ? 12 : 8)`."""
        return [
            ("ldc_w", "#1|String options.renderDistance"),
            ("iload_3", ""), ("ifeq", "5"), ("bipush", str(branch_value)), ("goto", "6"),
            ("bipush", str(other_value)),
            ("invokestatic", f"#2|{INT_VALUE_OF}"),
        ]

    def test_a_constant_inside_a_ternary_is_not_a_default(self) -> None:
        record = analyse(self.render_distance(12, 8))
        self.assertIsNone(record["default"])
        self.assertEqual(record["default_confidence"], "unknown")
        self.assertIn(analyzer.CONDITIONAL_DEFAULT_NOTE, record["notes"])

    def test_a_plain_constant_is_a_default(self) -> None:
        record = analyse([
            ("ldc_w", "#1|String options.renderDistance"),
            ("bipush", "8"), ("invokestatic", f"#2|{INT_VALUE_OF}"),
        ])
        self.assertEqual((record["default"], record["default_confidence"]), (8, "high"))
        self.assertEqual(record["notes"], [])

    def test_a_plain_constant_after_a_finished_ternary_is_a_default(self) -> None:
        record = analyse([
            ("ldc_w", "#1|String options.renderDistance"),
            ("iload_3", ""), ("ifeq", "5"), ("bipush", "32"), ("goto", "6"), ("bipush", "16"),
            ("pop", ""), ("bipush", "8"), ("invokestatic", f"#2|{INT_VALUE_OF}"),
        ])
        self.assertEqual(record["default"], 8)

    def test_a_boolean_factory_argument_chosen_by_a_condition_is_not_a_default(self) -> None:
        record = analyse([
            ("ldc_w", "#1|String options.syncWrites"),
            ("invokestatic", f"#2|{PLATFORM}"), ("getstatic", f"#3|{WINDOWS}"),
            ("if_acmpne", "6"), ("iconst_1", ""), ("goto", "7"), ("iconst_0", ""),
            ("invokestatic", f"#4|{BOOLEAN_FACTORY}"),
        ])
        self.assertIsNone(record["default"])
        self.assertIn(analyzer.CONDITIONAL_DEFAULT_NOTE, record["notes"])

    def test_a_boolean_factory_constant_is_a_default(self) -> None:
        record = analyse([
            ("ldc_w", "#1|String options.syncWrites"), ("iconst_1", ""),
            ("invokestatic", f"#4|{BOOLEAN_FACTORY}"),
        ])
        self.assertEqual((record["default"], record["default_confidence"]), (True, "high"))


class EnumOptionDefaults(unittest.TestCase):
    MODE = "net/minecraft/client/Mode"
    ENUM_OPTION = f"{OPTION}$Enum"
    CODEC = f"Field {MODE}.CODEC:Lcom/mojang/serialization/Codec;"
    ENUM_INIT = f"Method {ENUM_OPTION}.\"<init>\":(Ljava/util/List;Lcom/mojang/serialization/Codec;)V"
    OPTION_INIT = f"Method {OPTION}.\"<init>\":(Ljava/lang/String;Ljava/lang/Object;)V"
    ENUM_INFO = {
        "class": MODE,
        "codec_fields": {"CODEC": {"persisted_form": "integer_id"}},
        "constants": [
            {"name": "FAST", "field": "FAST", "ordinal": 0, "id": 0, "translation_key": "mode.fast"},
            {"name": "FANCY", "field": "FANCY", "ordinal": 1, "id": 1, "translation_key": "mode.fancy"},
        ],
    }

    def constant(self, name: str) -> tuple[str, str]:
        return ("getstatic", f"#9|Field {self.MODE}.{name}:L{self.MODE};")

    def definition(self, default: list[tuple[str, str]]) -> list[tuple[str, str]]:
        """`new OptionInstance(..., new Enum(List, Mode.CODEC), <default>)`."""
        return [
            ("ldc_w", "#1|String options.mode"),
            ("new", f"#2|class {self.ENUM_OPTION}"), ("dup", ""),
            ("getstatic", f"#3|{self.CODEC}"),
            ("invokespecial", f"#4|{self.ENUM_INIT}"),
            *default,
            ("invokespecial", f"#5|{self.OPTION_INIT}"),
        ]

    def analyse_enum(self, default: list[tuple[str, str]]) -> dict:
        with mock.patch.object(analyzer, "extract_enum", return_value=self.ENUM_INFO):
            return analyse(self.definition(default))

    def test_an_enum_constant_chosen_by_a_ternary_is_not_a_default(self) -> None:
        record = self.analyse_enum([
            ("iload_3", ""), ("ifeq", "9"), self.constant("FAST"), ("goto", "10"), self.constant("FANCY"),
        ])
        self.assertEqual(record["type"], "enum")
        self.assertIsNone(record["default"])
        self.assertEqual(record["default_confidence"], "unknown")
        self.assertIsNone(record["enum_default_constant"])
        self.assertIn(analyzer.CONDITIONAL_DEFAULT_NOTE, record["notes"])

    def test_an_enum_constant_passed_unconditionally_is_a_default(self) -> None:
        record = self.analyse_enum([self.constant("FANCY")])
        self.assertEqual((record["default"], record["default_confidence"]), (1, "high"))
        self.assertEqual(record["enum_default_constant"], "FANCY")
        self.assertNotIn(analyzer.CONDITIONAL_DEFAULT_NOTE, record["notes"])

    def test_an_enum_constant_after_a_finished_ternary_is_a_default(self) -> None:
        record = self.analyse_enum([
            ("iload_3", ""), ("ifeq", "9"), self.constant("FAST"), ("goto", "10"), self.constant("FANCY"),
            ("pop", ""), self.constant("FAST"),
        ])
        self.assertEqual((record["default"], record["default_confidence"]), (0, "high"))
        self.assertEqual(record["enum_default_constant"], "FAST")


class ControlFlowMarks(unittest.TestCase):
    def flags(self, body: list[tuple[str, str]]) -> list[str]:
        return [("J" if i.join else "-") + ("C" if i.conditional else "-") for i in listing(body).insns]

    def test_both_arms_of_a_conditional_are_marked_and_the_join_is_not_conditional(self) -> None:
        marks = self.flags([("iload_1", ""), ("ifeq", "4"), ("iconst_1", ""), ("goto", "5"),
                            ("iconst_0", ""), ("istore_2", "")])
        self.assertEqual(marks, ["--", "--", "-C", "-C", "JC", "J-"])

    def test_a_backward_jump_marks_the_repeated_span(self) -> None:
        marks = self.flags([("nop", ""), ("iinc", "1, 1"), ("iload_1", ""), ("bipush", "9"),
                            ("if_icmplt", "1"), ("return", "")])
        self.assertEqual(marks, ["--", "JC", "-C", "-C", "-C", "--"])

    def test_a_jump_to_no_known_instruction_leaves_later_instructions_unproven(self) -> None:
        marks = self.flags([("goto", "99"), ("nop", ""), ("nop", "")])
        self.assertEqual(marks, ["--", "J-", "J-"])


if __name__ == "__main__":
    unittest.main()
