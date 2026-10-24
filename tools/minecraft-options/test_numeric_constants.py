#!/usr/bin/env python3
"""Tests for how the analyzer reads numeric constants and renders them into a transform.

    python3 -m unittest discover -s tools/minecraft-options

Offline and deterministic: the disassembly is written out here, so nothing
depends on a client JAR or on `javap`.
"""

from __future__ import annotations

import unittest

import analyze_options as analyzer

VISITOR_DOUBLE = "InterfaceMethod net/minecraft/client/Options$FieldAccess.process:(Ljava/lang/String;D)D"
FIELD = "Field scale:D"
APPEND_DOUBLE = "Method java/lang/StringBuilder.append:(D)Ljava/lang/StringBuilder;"


def listing(body: list[tuple[str, str]]) -> str:
    lines = ["  public void run(FieldAccess);", "    Code:"]
    for offset, (op, comment) in enumerate(body):
        operand = comment if op in ("bipush", "sipush") else f"#1  // {comment}" if comment else ""
        lines.append(f"      {offset:>4}: {op:<13} {operand}".rstrip())
    return "\n".join(lines)


def instructions(body: list[tuple[str, str]]) -> list[analyzer.Insn]:
    return analyzer.parse_members(listing(body))[0].insns


def constant(op: str, comment: str = "") -> analyzer.Insn:
    return instructions([(op, comment)])[0]


class ConstantParsing(unittest.TestCase):
    def test_a_float_or_double_in_scientific_notation_is_read(self) -> None:
        self.assertEqual(analyzer.const_num(constant("ldc", "float 1.0E-5f")), 1e-5)
        self.assertEqual(analyzer.const_num(constant("ldc2_w", "double 1.0E-4d")), 1e-4)
        self.assertEqual(analyzer.const_num(constant("ldc2_w", "double -2.5E-7d")), -2.5e-7)
        self.assertEqual(analyzer.const_num(constant("ldc", "float 1.0E10f")), 1e10)
        self.assertEqual(analyzer.const_num(constant("ldc", "float 1.5E+3f")), 1500.0)

    def test_a_decimal_float_is_read_exactly_as_printed(self) -> None:
        self.assertEqual(analyzer.const_num(constant("ldc", "float 0.33333334f")), 0.33333334)
        self.assertEqual(analyzer.const_num(constant("ldc2_w", "double -70.0d")), -70.0)

    def test_a_long_is_read_from_its_constant_and_its_short_forms(self) -> None:
        self.assertEqual(analyzer.const_num(constant("ldc2_w", "long 1234567890123l")), 1234567890123.0)
        self.assertEqual(analyzer.const_num(constant("ldc2_w", "long -5l")), -5.0)
        self.assertEqual(analyzer.const_num(constant("lconst_0")), 0.0)
        self.assertEqual(analyzer.const_num(constant("lconst_1")), 1.0)

    def test_a_long_that_has_no_exact_double_is_unread(self) -> None:
        big = constant("ldc2_w", f"long {2 ** 53 + 1}l")

        self.assertIsNone(analyzer.const_num(big))
        self.assertTrue(analyzer.is_constant_load(big))

    def test_a_long_is_not_an_int_constant(self) -> None:
        self.assertIsNone(analyzer.const_int(constant("lconst_1")))
        self.assertIsNone(analyzer.const_int(constant("ldc2_w", "long 7l")))

    def test_non_finite_constants_are_constants_that_cannot_be_read(self) -> None:
        for comment in ("float NaNf", "double NaNd", "float Infinityf", "double -Infinityd"):
            with self.subTest(comment=comment):
                insn = constant("ldc", comment)
                self.assertIsNone(analyzer.const_num(insn))
                self.assertTrue(analyzer.is_constant_load(insn))

    def test_a_string_or_a_load_is_not_a_constant_load(self) -> None:
        self.assertFalse(analyzer.is_constant_load(constant("ldc", "String 1.0E-5f")))
        self.assertFalse(analyzer.is_constant_load(constant("dload_0")))
        self.assertFalse(analyzer.is_constant_load(constant("aconst_null")))


class UnreadConstantsMakeAChainUnresolved(unittest.TestCase):
    def test_a_chain_keeps_a_scientific_operand(self) -> None:
        insns = instructions([("ldc2_w", "double 1.0E-5d"), ("dmul", "")])

        self.assertEqual(analyzer.arithmetic_chain(insns), [{"op": "*", "operand": 1e-5}])
        self.assertEqual(analyzer.exact_arithmetic(insns), [{"op": "*", "operand": 1e-5}])

    def test_a_chain_with_an_unreadable_constant_is_unresolved(self) -> None:
        insns = instructions([("ldc2_w", "double 2.0d"), ("dmul", ""), ("ldc2_w", "double NaNd"), ("dadd", "")])

        self.assertIsNone(analyzer.arithmetic_chain(insns))
        self.assertIsNone(analyzer.exact_arithmetic(insns))

    def test_a_chain_still_skips_an_operand_that_is_not_a_constant(self) -> None:
        insns = instructions([("dload_0", ""), ("dmul", ""), ("ldc2_w", "double 2.0d"), ("dadd", "")])

        self.assertEqual(analyzer.arithmetic_chain(insns), [{"op": "+", "operand": 2.0}])


class OperandRendering(unittest.TestCase):
    def render(self, *operands: float) -> str | None:
        return analyzer.formula([{"op": "*", "operand": value} for value in operands], "value")

    def test_a_whole_number_has_no_fraction(self) -> None:
        self.assertEqual(analyzer.formula([{"op": "-", "operand": 70.0}, {"op": "/", "operand": 40.0}], "value"),
                         "((value - 70) / 40)")

    def test_an_operand_keeps_every_digit_it_was_read_with(self) -> None:
        self.assertEqual(self.render(0.33333334), "(value * 0.33333334)")
        self.assertEqual(self.render(1234567.0), "(value * 1234567)")
        self.assertEqual(self.render(123.4567), "(value * 123.4567)")

    def test_a_small_operand_is_not_rounded_to_zero(self) -> None:
        self.assertEqual(self.render(1e-5), "(value * 1e-05)")
        self.assertEqual(self.render(-2.5e-7), "(value * -2.5e-07)")

    def test_every_rendered_operand_reads_back_as_the_same_number(self) -> None:
        for operand in (0.33333334, 1234567.0, 1e-5, -70.0, 0.1, 2.5e-7, 9007199254740992.0):
            with self.subTest(operand=operand):
                text = analyzer.operand_text(operand)
                self.assertEqual(float(text), operand)


class TransformsAreReadAtTheirExactScale(unittest.TestCase):
    @staticmethod
    def record(encode: str, decode: str) -> dict:
        body = [("aload_0", ""), ("aload_1", ""), ("ldc_w", "String scale"), ("aload_0", ""),
                ("getfield", FIELD), ("ldc2_w", encode), ("dmul", ""),
                ("invokeinterface", VISITOR_DOUBLE), ("ldc2_w", decode), ("ddiv", ""),
                ("putfield", FIELD)]
        members = analyzer.parse_members(listing(body))
        return analyzer.extract_visitor(analyzer.Disassembler(None, set()), members, None)["scale"]

    def test_a_scientific_scale_survives_into_both_directions(self) -> None:
        record = self.record("double 1.0E-5d", "double 1.0E-5d")

        self.assertEqual(record["value_to_storage"], "(value * 1e-05)")
        self.assertEqual(record["storage_to_value"], "(file / 1e-05)")
        self.assertIs(record["transform_resolved"], True)

    def test_a_precise_scale_is_not_truncated(self) -> None:
        record = self.record("double 0.33333334d", "double 0.33333334d")

        self.assertEqual(record["value_to_storage"], "(value * 0.33333334)")

    def test_an_unreadable_scale_leaves_the_transform_unresolved(self) -> None:
        record = self.record("double NaNd", "double NaNd")

        self.assertIsNone(record["value_to_storage"])
        self.assertIsNone(record["storage_to_value"])
        self.assertIs(record["transform_resolved"], False)


class LegacyTransformsThatCannotBeReadAreUnresolved(unittest.TestCase):
    @staticmethod
    def save(constant_comment: str) -> analyzer.Member:
        body = [("aload_2", ""), ("ldc", "String scale:"),
                ("invokevirtual", "Method java/lang/StringBuilder.append:(Ljava/lang/String;)Ljava/lang/StringBuilder;"),
                ("aload_0", ""), ("getfield", FIELD), ("ldc2_w", constant_comment), ("dmul", ""),
                ("invokevirtual", APPEND_DOUBLE),
                ("invokevirtual", "Method java/lang/StringBuilder.toString:()Ljava/lang/String;"),
                ("return", "")]
        return analyzer.parse_members(listing(body))[0]

    def test_a_readable_scale_becomes_the_storage_transform(self) -> None:
        record = analyzer.extract_legacy([self.save("double 1.0E-5d")])["scale"]

        self.assertEqual(record["value_to_storage"], "(value * 1e-05)")
        self.assertNotIn("transform_resolved", record)

    def test_an_unreadable_scale_is_recorded_as_unread(self) -> None:
        record = analyzer.extract_legacy([self.save("double Infinityd")])["scale"]

        self.assertIsNone(record["value_to_storage"])
        self.assertEqual(record["transform_ops"], [])
        self.assertIs(record["transform_resolved"], False)
        self.assertIn(analyzer.UNREAD_TRANSFORM_NOTE, record["notes"])


if __name__ == "__main__":
    unittest.main()
