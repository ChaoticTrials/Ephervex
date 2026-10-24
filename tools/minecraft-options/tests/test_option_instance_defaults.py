#!/usr/bin/env python3
"""Tests for how the analyzer reports OptionInstance value sets, codec transforms and defaults.

    python3 -m unittest discover -s tools/minecraft-options

Offline and deterministic: the disassembly is written out here, so nothing
depends on a client JAR or on `javap`.
"""

from __future__ import annotations

import unittest
from unittest import mock

import analyze_options as analyzer
from .test_option_analysis import OPTION, OPTIONS, VALUE_OF, XMAP_CODEC, listing

RANGED = f"{OPTION}$Ranged"
ENUM = "net/minecraft/client/Mode"
DOUBLE_CODEC = "Field com/mojang/serialization/Codec.DOUBLE:Lcom/mojang/serialization/Codec;"
INT_VALUE_OF = "Method java/lang/Integer.valueOf:(I)Ljava/lang/Integer;"
DOUBLE_VALUE = "Method java/lang/Double.doubleValue:()D"
FUNCTION = "InvokeDynamic #{}:apply:()Ljava/util/function/Function;"

UNIT_RANGE = ("         0: dconst_0\n         1: dconst_1\n         2: invokestatic  #88"
              "                 // InterfaceMethod com/mojang/serialization/Codec.doubleRange:(DD)"
              "Lcom/mojang/serialization/Codec;")
INCIDENTAL_CONSTANTS = ("         0: dconst_0\n         1: dcmpl\n         2: dconst_1\n         3: dcmpg\n"
                        "         4: dreturn")
INT_RANGE_CALL = "invokestatic #5 // InterfaceMethod com/mojang/serialization/Codec.intRange:(II)Lcom/mojang/serialization/Codec;"
DOUBLE_RANGE_CALL = "invokestatic #5 // InterfaceMethod com/mojang/serialization/Codec.doubleRange:(DD)Lcom/mojang/serialization/Codec;"


class Disassembly(analyzer.Disassembler):
    def __init__(self, classes: dict[str, str] | None = None, verbose: str = "") -> None:
        super().__init__(None, {OPTIONS})
        self.classes = classes or {}
        self.verbose_text = verbose

    def get(self, class_name: str) -> str:
        return self.classes.get(class_name, "")

    def verbose(self, class_name: str) -> str:
        return self.verbose_text


def bootstraps(targets: list[tuple[str, str]]) -> str:
    """A `BootstrapMethods` table whose entry N hands out `targets[N]` as its method handle."""
    lines = ["BootstrapMethods:"]
    for index, (kind, handle) in enumerate(targets):
        lines += [f"  {index}: #9 REF_invokeStatic java/lang/invoke/LambdaMetafactory.metafactory:()V",
                  "    Method arguments:", f"      #{index + 1} REF_{kind} {handle}"]
    return "\n".join(lines)


def instructions(body: list[tuple[str, str]]) -> list[analyzer.Insn]:
    return analyzer.parse_members(listing("public Options()", body))[0].insns


def affine_lambdas(first: int) -> tuple[str, list[tuple[str, str]]]:
    """A decode and an encode lambda for `file * 40 + 70` and back, numbered from `first`."""
    decode = listing(
        f"private static java.lang.Double lambda$new${first}(java.lang.Double)",
        [("aload_0", ""), ("invokevirtual", f"#1|{DOUBLE_VALUE}"), ("ldc2_w", "#2|double 40.0d"),
         ("dmul", ""), ("ldc2_w", "#3|double 70.0d"), ("dadd", ""),
         ("invokestatic", f"#4|{VALUE_OF}"), ("areturn", "")])
    encode = listing(
        f"private static java.lang.Double lambda$new${first + 1}(java.lang.Double)",
        [("aload_0", ""), ("invokevirtual", f"#1|{DOUBLE_VALUE}"), ("ldc2_w", "#3|double 70.0d"),
         ("dsub", ""), ("ldc2_w", "#2|double 40.0d"), ("ddiv", ""),
         ("invokestatic", f"#4|{VALUE_OF}"), ("areturn", "")])
    handles = [("invokeStatic", f"{OPTIONS}.lambda$new${first}:(Ljava/lang/Double;)Ljava/lang/Double;"),
               ("invokeStatic", f"{OPTIONS}.lambda$new${first + 1}:(Ljava/lang/Double;)Ljava/lang/Double;")]
    return decode + "\n" + encode, handles


ENUM_HANDLES = [("invokeStatic", f"{ENUM}.byId:(I)L{ENUM};"), ("invokeVirtual", f"{ENUM}.getId:()I")]
UNREADABLE_HANDLES = [("invokeStatic", f"{OPTIONS}.lambda$new$8:(Ljava/lang/Object;)Ljava/lang/Object;"),
                      ("invokeStatic", f"{OPTIONS}.lambda$new$9:(Ljava/lang/Object;)Ljava/lang/Object;")]


def codec_xmaps(count: int) -> list[tuple[str, str]]:
    body: list[tuple[str, str]] = [("getstatic", f"#1|{DOUBLE_CODEC}")]
    for number in range(count):
        body += [("invokedynamic", f"#2|{FUNCTION.format(2 * number)}"),
                 ("invokedynamic", f"#2|{FUNCTION.format(2 * number + 1)}"),
                 ("invokeinterface", f"#3|{XMAP_CODEC}")]
    return body


def option(body: list[tuple[str, str]]) -> list[analyzer.Insn]:
    return instructions([("ldc_w", "#1|String options.example")] + body)


def analyze(window: list[analyzer.Insn], handles: list[tuple[str, str]], lambdas: str = "",
            classes: dict[str, str] | None = None) -> dict:
    dis = Disassembly(classes, bootstraps(handles))
    return analyzer.analyse_option_window(dis, window, OPTION, analyzer.parse_members(lambdas), OPTIONS)


def built(record: dict) -> dict:
    record.update(translation_key="options.example", field="example")
    return analyzer.build_setting("1.19.1", "0" * 64, OPTIONS, "example", record)


def shared_constant(class_name: str) -> list[tuple[str, str]]:
    return [("getstatic", f"#1|Field {class_name}.INSTANCE:L{class_name};")]


class ValueSetBoundsAreNotInvented(unittest.TestCase):
    def test_an_integer_range_whose_bounds_are_not_stated_has_none_instead_of_raising(self) -> None:
        classes = {RANGED: f"         0: {INT_RANGE_CALL}"}

        record = analyze(option(shared_constant(RANGED)), [], classes=classes)

        self.assertEqual(record["type"], "integer")
        self.assertIsNone(record["numeric"])
        self.assertTrue(any("could not be read" in note for note in record["notes"]))

    def test_an_integer_range_without_bounds_is_not_a_high_confidence_domain(self) -> None:
        classes = {RANGED: f"         0: {INT_RANGE_CALL}"}

        setting = built(analyze(option(shared_constant(RANGED)), [], classes=classes))

        self.assertNotEqual(setting["domain_confidence"], "high")

    def test_a_decimal_range_whose_bounds_are_not_stated_is_not_zero_to_one(self) -> None:
        classes = {RANGED: f"         0: {DOUBLE_RANGE_CALL}"}

        record = analyze(option(shared_constant(RANGED)), [], classes=classes)
        setting = built(record)

        self.assertEqual(record["type"], "number")
        self.assertIsNone(record["numeric"])
        self.assertNotEqual(setting["domain_confidence"], "high")

    def test_a_unit_double_is_recognized_by_its_zero_to_one_range_call(self) -> None:
        dis = Disassembly({RANGED: UNIT_RANGE})

        self.assertEqual(analyzer.classify_value_set(dis, RANGED),
                         {"kind": "unit_double", "min": 0.0, "max": 1.0})

    def test_a_class_that_only_happens_to_use_zero_and_one_is_not_a_unit_double(self) -> None:
        dis = Disassembly({RANGED: INCIDENTAL_CONSTANTS})

        self.assertEqual(analyzer.classify_value_set(dis, RANGED), {"kind": "unknown"})


class CodecTransformsAreReportedAsReadOrUnread(unittest.TestCase):
    def enum_mapping(self, *, is_enum: bool = True):
        return mock.patch.object(analyzer, "extract_enum",
                                 return_value={"constants": []} if is_enum else None)

    def test_an_enum_id_mapping_is_not_an_unread_transform(self) -> None:
        with self.enum_mapping():
            record = analyze(option(codec_xmaps(1)), ENUM_HANDLES)

        self.assertIs(record["codec"]["transform_resolved"], True)
        self.assertFalse(any("xmap" in note for note in record["notes"]))

    def test_a_mapping_of_a_class_that_is_not_an_enum_is_an_unread_transform(self) -> None:
        with self.enum_mapping(is_enum=False):
            record = analyze(option(codec_xmaps(1)), ENUM_HANDLES)

        self.assertIs(record["codec"]["transform_resolved"], False)
        self.assertTrue(any("xmap" in note for note in record["notes"]))

    def test_a_pair_of_unrelated_lambdas_is_an_unread_transform(self) -> None:
        record = analyze(option(codec_xmaps(1)), UNREADABLE_HANDLES)

        self.assertIs(record["codec"]["transform_resolved"], False)

    def test_one_arithmetic_xmap_is_read(self) -> None:
        lambdas, handles = affine_lambdas(0)

        record = analyze(option(codec_xmaps(1)), handles, lambdas)

        self.assertIs(record["codec"]["transform_resolved"], True)
        self.assertEqual(record["value_to_storage"], "((value - 70) / 40)")

    def test_an_arithmetic_xmap_beside_an_enum_mapping_is_still_read(self) -> None:
        lambdas, handles = affine_lambdas(0)

        with self.enum_mapping():
            record = analyze(option(codec_xmaps(2)), handles + ENUM_HANDLES, lambdas)

        self.assertIs(record["codec"]["transform_resolved"], True)
        self.assertEqual(record["value_to_storage"], "((value - 70) / 40)")

    def test_an_arithmetic_xmap_beside_an_unread_one_reports_no_formula(self) -> None:
        lambdas, handles = affine_lambdas(0)

        record = analyze(option(codec_xmaps(2)), handles + UNREADABLE_HANDLES, lambdas)

        self.assertIs(record["codec"]["transform_resolved"], False)
        self.assertNotIn("value_to_storage", record)
        self.assertNotIn("transform_ops", record)

    def test_an_unread_xmap_before_an_arithmetic_one_reports_no_formula(self) -> None:
        lambdas, handles = affine_lambdas(2)

        record = analyze(option(codec_xmaps(2)), UNREADABLE_HANDLES + handles, lambdas)

        self.assertIs(record["codec"]["transform_resolved"], False)
        self.assertNotIn("value_to_storage", record)

    def test_two_arithmetic_xmaps_are_not_reported_as_the_last_one(self) -> None:
        lambdas, handles = affine_lambdas(0)
        more, more_handles = affine_lambdas(2)

        record = analyze(option(codec_xmaps(2)), handles + more_handles, lambdas + "\n" + more)

        self.assertIs(record["codec"]["transform_resolved"], False)
        self.assertNotIn("value_to_storage", record)


class StorageTransformsKeepTheirTypes(unittest.TestCase):
    def test_an_integer_stays_an_integer(self) -> None:
        stored = analyzer.apply_ops(3, [{"op": "*", "operand": 2.0}, {"op": "+", "operand": 1.0}])

        self.assertEqual(stored, 7)
        self.assertIsInstance(stored, int)

    def test_a_decimal_stays_a_decimal(self) -> None:
        stored = analyzer.apply_ops(0.0, [{"op": "+", "operand": 1.0}])

        self.assertEqual(stored, 1.0)
        self.assertIsInstance(stored, float)

    def test_an_integer_that_becomes_fractional_is_a_decimal(self) -> None:
        self.assertEqual(analyzer.apply_ops(1, [{"op": "/", "operand": 4.0}]), 0.25)

    def test_a_division_by_zero_applies_nothing(self) -> None:
        ops = [{"op": "+", "operand": 1.0}, {"op": "/", "operand": 0.0}, {"op": "*", "operand": 5.0}]

        self.assertIsNone(analyzer.apply_ops(2, ops))

    def test_an_operation_it_does_not_know_applies_nothing(self) -> None:
        self.assertIsNone(analyzer.apply_ops(2, [{"op": "%", "operand": 3.0}]))

    def test_the_default_of_an_integer_field_is_an_integer(self) -> None:
        stored = analyzer.jvm_default("I", [])

        self.assertEqual(stored, 0)
        self.assertIsInstance(stored, int)

    def test_the_default_of_a_decimal_field_is_a_decimal(self) -> None:
        self.assertIsInstance(analyzer.jvm_default("D", []), float)

    def test_the_default_of_a_boolean_field_ignores_arithmetic(self) -> None:
        self.assertIs(analyzer.jvm_default("Z", [{"op": "+", "operand": 1.0}]), False)

    def test_a_field_type_without_a_default_has_none(self) -> None:
        self.assertIsNone(analyzer.jvm_default("Ljava/lang/String;", []))

    def test_a_default_whose_transform_cannot_be_applied_is_not_proven(self) -> None:
        record = {"transform_ops": [{"op": "/", "operand": 0.0}], "field_descriptor": "D"}
        initial = {"kind": "number", "value": 5.0, "descriptor": "D"}

        value, confidence, note = analyzer.resolve_default(record, initial, Disassembly())

        self.assertIsNone(value)
        self.assertEqual(confidence, "unknown")
        self.assertIn("cannot be applied", note)


def with_default(body: list[tuple[str, str]], value: str) -> list[tuple[str, str]]:
    return body + [("ldc2_w", f"#5|double {value}d"), ("invokestatic", f"#6|{VALUE_OF}")]


class DefaultsDeclareTheirValueSpace(unittest.TestCase):
    def test_a_default_that_a_codec_transform_still_converts_is_in_the_ui_space(self) -> None:
        lambdas, handles = affine_lambdas(0)

        record = analyze(option(with_default(codec_xmaps(1), "70.0")), handles, lambdas)

        self.assertEqual(record["default"], 70.0)
        self.assertEqual(record["default_space"], "ui")
        self.assertEqual(built(record)["default_space"], "ui")

    def test_a_default_behind_an_unread_transform_is_in_the_ui_space(self) -> None:
        record = analyze(option(with_default(codec_xmaps(1), "70.0")), UNREADABLE_HANDLES)

        self.assertEqual(record["default_space"], "ui")

    def test_a_default_without_a_codec_transform_is_in_the_file_space(self) -> None:
        body = [("getstatic", f"#1|{DOUBLE_CODEC}")]

        record = analyze(option(with_default(body, "0.5")), [])

        self.assertEqual(record["default_space"], "file")

    def test_a_setting_without_a_default_declares_no_value_space(self) -> None:
        setting = built({"type": "number", "default": None, "notes": []})

        self.assertIsNone(setting["default_space"])

    def test_a_default_recovered_outside_the_option_window_is_in_the_file_space(self) -> None:
        setting = built({"type": "integer", "default": 3, "notes": []})

        self.assertEqual(setting["default_space"], "file")


class UnrecognizedSettingsAreNotManageable(unittest.TestCase):
    def test_an_unknown_type_is_unmanageable_and_unknown_in_confidence(self) -> None:
        setting = analyzer.build_setting("1.19.1", "0" * 64, OPTIONS, "mystery", {"type": "unknown"})

        self.assertFalse(setting["ephervex_manageable"])
        self.assertEqual(setting["type_confidence"], "unknown")

    def test_a_special_key_the_analyzer_could_not_type_keeps_the_unknown_confidence(self) -> None:
        key = next(iter(analyzer.SPECIAL_KEYS))

        setting = analyzer.build_setting("1.19.1", "0" * 64, OPTIONS, key, {"type": "unknown"})

        self.assertEqual(setting["type"], "special")
        self.assertEqual(setting["type_confidence"], "unknown")
        self.assertFalse(setting["ephervex_manageable"])

    def test_a_typed_setting_is_manageable_with_high_type_confidence(self) -> None:
        setting = analyzer.build_setting("1.19.1", "0" * 64, OPTIONS, "example", {"type": "boolean"})

        self.assertTrue(setting["ephervex_manageable"])
        self.assertEqual(setting["type_confidence"], "high")


if __name__ == "__main__":
    unittest.main()
