#!/usr/bin/env python3
"""Tests for how the analyzer reads the arithmetic around an options-visitor call.

    python3 -m unittest discover -s tools/minecraft-options

Offline and deterministic: the disassembly is written out here, so nothing
depends on a client JAR or on `javap`.
"""

from __future__ import annotations

import unittest

import analyze_options as analyzer

OWNER = "net/minecraft/client/Options"
VISITOR_DOUBLE = "InterfaceMethod net/minecraft/client/Options$FieldAccess.process:(Ljava/lang/String;D)D"
VISITOR_INT = "InterfaceMethod net/minecraft/client/Options$FieldAccess.process:(Ljava/lang/String;I)I"
VISITOR_BOOL = "InterfaceMethod net/minecraft/client/Options$FieldAccess.process:(Ljava/lang/String;Z)Z"
MOUSE = "Field mouseSensitivity:D"
FOV = "Field fov:D"
DISTANCE = "Field renderDistance:I"
CLAMP = "Method net/minecraft/util/Mth.clamp:(DDD)D"


def members(body: list[tuple[str, str]]) -> list[analyzer.Member]:
    lines = ["  public void processOptions(FieldAccess);", "    Code:"]
    for offset, (op, comment) in enumerate(body):
        operand = comment if op in ("bipush", "sipush") else f"#1  // {comment}" if comment else ""
        lines.append(f"      {offset:>4}: {op:<13} {operand}".rstrip())
    return analyzer.parse_members("\n".join(lines))


def instructions(body: list[tuple[str, str]]) -> list[analyzer.Insn]:
    return members(body)[0].insns


def statement(key: str, field: str, visitor: str, before: list[tuple[str, str]] = (),
              after: list[tuple[str, str]] = ()) -> list[tuple[str, str]]:
    """`this.field = visitor.process("key", <before this.field>) <after>;`"""
    return ([("aload_0", ""), ("aload_1", ""), ("ldc_w", f"String {key}"), ("aload_0", ""),
             ("getfield", field)] + list(before) + [("invokeinterface", visitor)] + list(after)
            + [("putfield", field)])


def mouse_sensitivity() -> list[tuple[str, str]]:
    return statement("mouseSensitivity", MOUSE, VISITOR_DOUBLE)


def fov() -> list[tuple[str, str]]:
    return statement(
        "fov", FOV, VISITOR_DOUBLE,
        before=[("ldc2_w", "double 70.0d"), ("dsub", ""), ("ldc2_w", "double 40.0d"), ("ddiv", "")],
        after=[("ldc2_w", "double 40.0d"), ("dmul", ""), ("ldc2_w", "double 70.0d"), ("dadd", "")])


def extract(body: list[tuple[str, str]]) -> dict[str, dict]:
    return analyzer.extract_visitor(analyzer.Disassembler(None, set()), members(body), None)


class VisitorDecodeStopsAtTheStatement(unittest.TestCase):
    def test_the_next_options_arithmetic_is_not_this_options_transform(self) -> None:
        records = extract(mouse_sensitivity() + fov())

        mouse = records["mouseSensitivity"]
        self.assertIsNone(mouse["value_to_storage"])
        self.assertIsNone(mouse["storage_to_value"])
        self.assertEqual(mouse["transform_ops"], [])
        self.assertNotIn("transform_resolved", mouse)

    def test_a_plain_option_before_a_transformed_one_reports_no_ui_to_file_transform(self) -> None:
        record = extract(mouse_sensitivity() + fov())["mouseSensitivity"]
        record.update(translation_key="options.sensitivity")

        setting = analyzer.build_setting("1.18.2", "0" * 64, OWNER, "mouseSensitivity", record)

        self.assertIsNone(setting["ui_to_file_transform"])
        self.assertIsNone(setting["transform_resolved"])

    def test_the_transformed_option_keeps_both_directions(self) -> None:
        records = extract(mouse_sensitivity() + fov())

        fov_record = records["fov"]
        self.assertEqual(fov_record["value_to_storage"], "((value - 70) / 40)")
        self.assertEqual(fov_record["storage_to_value"], "((file * 40) + 70)")
        self.assertIs(fov_record["transform_resolved"], True)

    def test_the_field_comes_from_the_statement_that_stores_the_result(self) -> None:
        body = [("aload_0", ""), ("aload_1", ""), ("ldc_w", "String mouseSensitivity"),
                ("aload_0", ""), ("getfield", MOUSE), ("invokeinterface", VISITOR_DOUBLE),
                ("pop2", "")] + fov()

        record = extract(body)["mouseSensitivity"]

        self.assertEqual(record["field"], "mouseSensitivity")
        self.assertIsNone(record["value_to_storage"])

    def test_a_result_that_is_not_stored_has_no_field_from_the_next_statement(self) -> None:
        body = [("aload_1", ""), ("ldc_w", "String mouseSensitivity"), ("dconst_0", ""),
                ("invokeinterface", VISITOR_DOUBLE), ("pop2", "")] + fov()

        self.assertIsNone(extract(body)["mouseSensitivity"]["field"])


class VisitorTransformThatCannotBeReadIsUnresolved(unittest.TestCase):
    def assert_unresolved(self, record: dict) -> None:
        self.assertIsNone(record["value_to_storage"])
        self.assertIsNone(record["storage_to_value"])
        self.assertEqual(record["transform_ops"], [])
        self.assertIs(record["transform_resolved"], False)
        self.assertTrue(record["notes"])

    def test_a_constant_on_the_left_of_the_operator_is_unresolved(self) -> None:
        record = extract(statement(
            "inverted", FOV, VISITOR_DOUBLE,
            before=[("dconst_1", ""), ("dload_0", ""), ("dsub", "")]))["inverted"]

        self.assert_unresolved(record)

    def test_a_nested_sub_expression_is_unresolved(self) -> None:
        record = extract(statement(
            "nested", FOV, VISITOR_DOUBLE,
            after=[("aload_0", ""), ("getfield", MOUSE), ("ldc2_w", "double 2.0d"), ("dadd", ""),
                   ("dmul", "")]))["nested"]

        self.assert_unresolved(record)

    def test_an_opcode_without_a_readable_step_is_unresolved(self) -> None:
        record = extract(statement("remainder", FOV, VISITOR_DOUBLE, after=[("drem", "")]))["remainder"]

        self.assert_unresolved(record)

    def test_one_direction_without_the_other_is_unresolved(self) -> None:
        record = extract(statement(
            "one_sided", FOV, VISITOR_DOUBLE,
            before=[("ldc2_w", "double 70.0d"), ("dsub", "")]))["one_sided"]

        self.assert_unresolved(record)

    def test_directions_that_do_not_undo_each_other_are_unresolved(self) -> None:
        record = extract(statement(
            "mismatch", FOV, VISITOR_DOUBLE,
            before=[("ldc2_w", "double 70.0d"), ("dsub", "")],
            after=[("ldc2_w", "double 60.0d"), ("dadd", "")]))["mismatch"]

        self.assert_unresolved(record)

    def test_a_statement_that_never_ends_leaves_the_decode_unresolved(self) -> None:
        body = [("aload_1", ""), ("ldc_w", "String open"), ("dconst_0", ""),
                ("invokeinterface", VISITOR_DOUBLE), ("ldc2_w", "double 40.0d"), ("dmul", "")]

        self.assert_unresolved(extract(body)["open"])

    def test_an_unresolved_transform_is_never_offered_as_a_high_confidence_transform(self) -> None:
        record = extract(statement("remainder", FOV, VISITOR_DOUBLE, after=[("drem", "")]))["remainder"]
        record.update(translation_key="options.remainder")

        setting = analyzer.build_setting("1.18.2", "0" * 64, OWNER, "remainder", record)

        self.assertIsNone(setting["ui_to_file_transform"])
        self.assertIs(setting["transform_resolved"], False)


class VisitorTransformThatCanBeReadIsKept(unittest.TestCase):
    def test_a_constant_reaching_its_operator_through_a_widening_conversion_is_read(self) -> None:
        record = extract(statement(
            "widened", FOV, VISITOR_DOUBLE,
            before=[("bipush", "70"), ("i2d", ""), ("dsub", "")],
            after=[("bipush", "70"), ("i2d", ""), ("dadd", "")]))["widened"]

        self.assertEqual(record["value_to_storage"], "(value - 70)")
        self.assertEqual(record["storage_to_value"], "(file + 70)")
        self.assertIs(record["transform_resolved"], True)

    def test_a_conversion_and_clamp_around_an_integer_option_is_not_a_transform(self) -> None:
        record = extract(statement(
            "renderDistance", DISTANCE, VISITOR_INT,
            after=[("i2d", ""), ("dconst_0", ""), ("ldc2_w", "double 32.0d"),
                   ("invokestatic", CLAMP), ("d2i", "")]))["renderDistance"]

        self.assertIsNone(record["value_to_storage"])
        self.assertIsNone(record["storage_to_value"])
        self.assertNotIn("transform_resolved", record)

    def test_a_boolean_option_has_no_arithmetic_to_read(self) -> None:
        record = extract(statement("flag", "Field flag:Z", VISITOR_BOOL))["flag"]

        self.assertNotIn("transform_resolved", record)


class ExactArithmetic(unittest.TestCase):
    def test_a_chain_of_constant_steps_is_read_in_evaluation_order(self) -> None:
        insns = instructions([("ldc2_w", "double 70.0d"), ("dsub", ""), ("ldc2_w", "double 40.0d"), ("ddiv", "")])

        self.assertEqual(analyzer.exact_arithmetic(insns),
                         [{"op": "-", "operand": 70.0}, {"op": "/", "operand": 40.0}])

    def test_a_narrowing_conversion_before_the_operator_is_unresolved(self) -> None:
        insns = instructions([("ldc2_w", "double 2.0d"), ("d2f", ""), ("fmul", "")])

        self.assertIsNone(analyzer.exact_arithmetic(insns))

    def test_the_lenient_chain_is_unchanged_for_other_readers(self) -> None:
        insns = instructions([("dconst_1", ""), ("dload_0", ""), ("dsub", "")])

        self.assertEqual(analyzer.arithmetic_chain(insns), [])
        self.assertIsNone(analyzer.exact_arithmetic(insns))


if __name__ == "__main__":
    unittest.main()
