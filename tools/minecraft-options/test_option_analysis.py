#!/usr/bin/env python3
"""Tests for how the analyzer reads the numeric domain of an OptionInstance.

    python3 -m unittest discover -s tools/minecraft-options

Offline and deterministic: the disassembly is written out here, so nothing
depends on a client JAR or on `javap`.
"""

from __future__ import annotations

import unittest

import analyze_options as analyzer
import dataset

OPTIONS = "net/minecraft/client/Options"
OPTION = "net/minecraft/client/OptionInstance"
INT_RANGE = f"{OPTION}$IntRange"

XMAP_SLIDER = (
    f"Method {INT_RANGE}.xmap:(Ljava/util/function/IntFunction;"
    f"Ljava/util/function/ToIntFunction;Z)L{OPTION}$SliderableValueSet;"
)
XMAP_CODEC = "InterfaceMethod com/mojang/serialization/Codec.xmap:(Ljava/util/function/Function;Ljava/util/function/Function;)Lcom/mojang/serialization/Codec;"
DOUBLE_RANGE = "InterfaceMethod com/mojang/serialization/Codec.doubleRange:(DD)Lcom/mojang/serialization/Codec;"
LOG_MOUSE = f"Method {OPTIONS}.logMouse:(I)D"
VALUE_OF = "Method java/lang/Double.valueOf:(D)Ljava/lang/Double;"


def listing(signature: str, body: list[tuple[str, str]]) -> str:
    lines = [f"  {signature};", "    Code:"]
    for offset, (op, comment) in enumerate(body):
        arg, _, note = comment.partition("|")
        lines.append(f"      {offset:>4}: {op:<13} {arg}{'  // ' + note if note else ''}".rstrip())
    return "\n".join(lines)


def slider_pair(decode_name: str, encode_name: str, operand: str) -> str:
    """The two lambdas of a slider `xmap`: position to value, and back."""
    decode = listing(
        f"private static java.lang.Double {decode_name}(int)",
        [("iload_0", ""), ("i2d", ""), ("ldc2_w", f"#1|double {operand}"), ("ddiv", ""),
         ("invokestatic", f"#2|{VALUE_OF}"), ("areturn", "")],
    )
    encode = listing(
        f"private static int {encode_name}(java.lang.Double)",
        [("aload_0", ""), ("invokevirtual", "#3|Method java/lang/Double.doubleValue:()D"),
         ("ldc2_w", f"#1|double {operand}"), ("dmul", ""), ("d2i", ""), ("ireturn", "")],
    )
    return decode + "\n" + encode


BOOTSTRAPS = (
    "BootstrapMethods:\n"
    "  0: #9 REF_invokeStatic java/lang/invoke/LambdaMetafactory.metafactory:()V\n"
    "    Method arguments:\n"
    f"      #1 REF_invokeStatic {OPTIONS}.lambda$new$1:(I)Ljava/lang/Double;\n"
    "  1: #9 REF_invokeStatic java/lang/invoke/LambdaMetafactory.metafactory:()V\n"
    "    Method arguments:\n"
    f"      #2 REF_invokeStatic {OPTIONS}.lambda$new$2:(Ljava/lang/Double;)I\n"
)


class Disassembly(analyzer.Disassembler):
    def __init__(self, lambdas: str) -> None:
        super().__init__(None, {OPTIONS})
        self.lambdas = lambdas

    def verbose(self, class_name: str) -> str:
        return self.lambdas + "\n" + BOOTSTRAPS

    def get(self, class_name: str) -> str:
        return ""


def construct(slider_low: int | None, slider_high: int | None, codec: list[tuple[str, str]],
              *, slider_mapping: bool = True, codec_xmap: bool = False) -> list[analyzer.Insn]:
    body: list[tuple[str, str]] = [("ldc_w", "#1|String options.example"),
                                  ("new", f"#2|class {INT_RANGE}"), ("dup", "")]
    body += [("bipush", f"{slider_low}"), ("bipush", f"{slider_high}")]
    body.append(("invokespecial", f'#3|Method {INT_RANGE}."<init>":(II)V'))
    if slider_mapping:
        body += [("invokedynamic", "#4|InvokeDynamic #0:apply:()Ljava/util/function/IntFunction;"),
                 ("invokedynamic", "#5|InvokeDynamic #1:applyAsInt:()Ljava/util/function/ToIntFunction;"),
                 ("iconst_1", ""), ("invokevirtual", f"#6|{XMAP_SLIDER}")]
    body += codec
    if codec_xmap:
        body.append(("invokeinterface", f"#7|{XMAP_CODEC}"))
    body.append(("invokestatic", f"#8|{VALUE_OF}"))
    return analyzer.parse_members(listing("public Options()", body))[0].insns


def analyze(window: list[analyzer.Insn], mapping: str = "") -> dict:
    lambdas = slider_pair("lambda$new$1", "lambda$new$2", "20.0d") if not mapping else mapping
    members = analyzer.parse_members(lambdas)
    return analyzer.analyse_option_window(Disassembly(lambdas), window, OPTION, members, OPTIONS)


def double_range(low: str, high: str) -> list[tuple[str, str]]:
    return [("dconst_0" if low == "0.0" else "ldc2_w", "" if low == "0.0" else f"#9|double {low}d"),
            ("ldc2_w", f"#10|double {high}d"), ("invokestatic", f"#11|{DOUBLE_RANGE}")]


class DirectRangeBounds(unittest.TestCase):
    def test_a_bound_that_is_the_argument_of_a_helper_call_is_not_a_range(self) -> None:
        codec = [("sipush", "-200"), ("invokestatic", f"#1|{LOG_MOUSE}"),
                 ("bipush", "100"), ("invokestatic", f"#1|{LOG_MOUSE}"),
                 ("invokestatic", f"#11|{DOUBLE_RANGE}")]
        record = analyze(construct(-200, 100, codec))

        self.assertIsNone(record["codec"]["explicit_range"])
        self.assertIsNone(record["numeric"])

    def test_a_helper_result_never_reaches_the_dataset_as_a_high_confidence_domain(self) -> None:
        codec = [("sipush", "-200"), ("invokestatic", f"#1|{LOG_MOUSE}"),
                 ("bipush", "100"), ("invokestatic", f"#1|{LOG_MOUSE}"),
                 ("invokestatic", f"#11|{DOUBLE_RANGE}")]
        record = analyze(construct(-200, 100, codec))
        record.update(translation_key="options.example", field="example")

        setting = analyzer.build_setting("26.2", "0" * 64, OPTIONS, "example", record)

        self.assertNotEqual(setting["domain_confidence"], "high")
        self.assertIsNone(setting["numeric"])

    def test_constants_pushed_straight_into_a_range_call_are_its_bounds(self) -> None:
        record = analyze(construct(0, 40, double_range("0.0", "2.0")))

        self.assertEqual(record["codec"]["explicit_range"], [0.0, 2.0])
        self.assertEqual((record["numeric"]["min"], record["numeric"]["max"]), (0.0, 2.0))


class IntegerSliderBehindDecimalCodec(unittest.TestCase):
    def test_a_decimal_option_is_not_reported_as_an_integer(self) -> None:
        record = analyze(construct(0, 40, double_range("0.0", "2.0")))

        self.assertEqual(record["type"], "number")
        self.assertEqual(record["file_encoding"], "decimal floating point")

    def test_the_step_is_the_stored_value_change_of_one_slider_position(self) -> None:
        record = analyze(construct(0, 40, double_range("0.0", "2.0")))

        self.assertEqual(record["numeric"]["step"], 0.05)

    def test_a_slider_mapping_that_misses_the_codec_range_proves_no_step(self) -> None:
        record = analyze(construct(0, 40, double_range("0.0", "3.0")))

        self.assertEqual(record["type"], "number")
        self.assertIsNone(record["numeric"]["step"])

    def test_a_non_linear_slider_mapping_whose_ends_match_the_range_proves_no_step(self) -> None:
        squared = listing(
            "private static java.lang.Double lambda$new$1(int)",
            [("iload_0", ""), ("i2d", ""), ("ldc2_w", "#1|double 40.0d"), ("ddiv", ""),
             ("ldc2_w", "#1|double 2.0d"), ("invokestatic", "#5|Method java/lang/Math.pow:(DD)D"),
             ("invokestatic", f"#2|{VALUE_OF}"), ("areturn", "")],
        ) + "\n" + listing(
            "private static int lambda$new$2(java.lang.Double)",
            [("aload_0", ""), ("invokevirtual", "#3|Method java/lang/Double.doubleValue:()D"),
             ("invokestatic", "#6|Method java/lang/Math.sqrt:(D)D"), ("ldc2_w", "#1|double 40.0d"),
             ("dmul", ""), ("d2i", ""), ("ireturn", "")],
        )
        record = analyze(construct(0, 40, double_range("0.0", "1.0")), mapping=squared)

        self.assertEqual(record["type"], "number")
        self.assertIsNone(record["numeric"]["step"])

    def test_a_slider_mapping_that_cannot_be_read_proves_no_step(self) -> None:
        window = construct(0, 40, double_range("0.0", "2.0"))
        record = analyze(window, mapping=listing("private static void unrelated()", [("return", "")]))

        self.assertIsNone(record["numeric"]["step"])

    def test_an_integer_converted_by_the_codec_keeps_its_integer_slider_step(self) -> None:
        codec = [("getstatic", "#11|Field com/mojang/serialization/Codec.DOUBLE:Lcom/mojang/serialization/Codec;")]
        window = construct(30, 110, codec, slider_mapping=False, codec_xmap=True)
        record = analyze(window)

        self.assertEqual(record["type"], "integer")
        self.assertEqual((record["numeric"]["min"], record["numeric"]["max"], record["numeric"]["step"]),
                         (30, 110, 1))


class PersistedCodecXmap(unittest.TestCase):
    """The `Codec.xmap` lambdas decide what lands in the file."""

    CODEC_LAMBDAS = [
        ("getstatic", "#11|Field com/mojang/serialization/Codec.DOUBLE:Lcom/mojang/serialization/Codec;"),
        ("invokedynamic", "#4|InvokeDynamic #0:apply:()Ljava/util/function/Function;"),
        ("invokedynamic", "#5|InvokeDynamic #1:apply:()Ljava/util/function/Function;"),
    ]

    def codec_record(self, lambdas: str) -> dict:
        window = construct(0, 40, self.CODEC_LAMBDAS, slider_mapping=False, codec_xmap=True)
        return analyze(window, mapping=lambdas)

    def test_a_linear_lambda_pair_is_a_resolved_transform(self) -> None:
        record = self.codec_record(slider_pair("lambda$new$1", "lambda$new$2", "20.0d"))

        self.assertTrue(record["codec"]["transform_resolved"])

    def test_a_lambda_pair_calling_math_helpers_is_left_unresolved(self) -> None:
        logarithmic = listing(
            "private static java.lang.Double lambda$new$1(int)",
            [("iload_0", ""), ("i2d", ""), ("ldc2_w", "#1|double 100.0d"), ("ddiv", ""),
             ("ldc2_w", "#1|double 10.0d"), ("invokestatic", "#5|Method java/lang/Math.pow:(DD)D"),
             ("invokestatic", f"#2|{VALUE_OF}"), ("areturn", "")],
        ) + "\n" + listing(
            "private static int lambda$new$2(java.lang.Double)",
            [("aload_0", ""), ("invokevirtual", "#3|Method java/lang/Double.doubleValue:()D"),
             ("invokestatic", "#6|Method java/lang/Math.log10:(D)D"), ("ldc2_w", "#1|double 100.0d"),
             ("dmul", ""), ("d2i", ""), ("ireturn", "")],
        )
        record = self.codec_record(logarithmic)

        self.assertFalse(record["codec"]["transform_resolved"])

    def test_a_lambda_pair_that_branches_is_left_unresolved(self) -> None:
        branching = listing(
            "private static java.lang.Double lambda$new$1(int)",
            [("iload_0", ""), ("ifge", "3"), ("iconst_0", ""), ("istore_0", ""), ("iload_0", ""),
             ("i2d", ""), ("ldc2_w", "#1|double 20.0d"), ("ddiv", ""),
             ("invokestatic", f"#2|{VALUE_OF}"), ("areturn", "")],
        ) + "\n" + listing(
            "private static int lambda$new$2(java.lang.Double)",
            [("aload_0", ""), ("invokevirtual", "#3|Method java/lang/Double.doubleValue:()D"),
             ("ldc2_w", "#1|double 20.0d"), ("dmul", ""), ("d2i", ""), ("ireturn", "")],
        )
        record = self.codec_record(branching)

        self.assertFalse(record["codec"]["transform_resolved"])


STRING_EQUALS = "Method java/lang/String.equals:(Ljava/lang/Object;)Z"
PARSE_INT = "Method java/lang/Integer.parseInt:(Ljava/lang/String;)I"
PRINTLN = "Method java/io/PrintWriter.println:(Ljava/lang/String;)V"
KEY_LOCAL, VALUE_LOCAL = "4", "5"


def compare(text: str, local: str) -> list[tuple[str, str]]:
    return [("ldc_w", f"#1|String {text}"), ("aload", f"#2|{local}"), ("invokevirtual", f"#3|{STRING_EQUALS}")]


def assemble(items: list[tuple[str, str]]) -> list[tuple[str, str]]:
    """Resolve `@label` branch targets and `label` markers to instruction indexes."""
    labels: dict[str, int] = {}
    code: list[tuple[str, str]] = []
    for op, arg in items:
        if op == "label":
            labels[arg] = len(code)
        else:
            code.append((op, arg))
    return [(op, str(labels[arg[1:]]) if arg.startswith("@") else arg) for op, arg in code]


def token_key(key: str, field: str, descriptor: str, stores: list[tuple[str, str]]) -> list[tuple[str, str]]:
    """`if ("key".equals(k)) { if ("lit".equals(v)) field = <value>; ... }`"""
    items = compare(key, KEY_LOCAL) + [("ifeq", f"@{key}_end")]
    for n, (literal, value) in enumerate(stores):
        if n:
            items.append(("label", f"{key}_{n}"))
        items += compare(literal, VALUE_LOCAL)
        items += [("ifeq", f"@{key}_{n + 1}" if n + 1 < len(stores) else f"@{key}_end"),
                  ("aload_0", ""), (value, ""), ("putfield", f"#4|Field {field}:{descriptor}"),
                  ("goto", f"@{key}_end")]
    items.append(("label", f"{key}_end"))
    return items


def boolean_key(key: str, field: str, descriptor: str = "Z") -> list[tuple[str, str]]:
    items = compare(key, KEY_LOCAL) + [("ifeq", f"@{key}_end"), ("aload_0", "")]
    items += compare("true", VALUE_LOCAL)
    return items + [("putfield", f"#4|Field {field}:{descriptor}"), ("label", f"{key}_end")]


def integer_key(key: str, field: str) -> list[tuple[str, str]]:
    return compare(key, KEY_LOCAL) + [("ifeq", f"@{key}_end"), ("aload_0", ""), ("aload", f"#2|{VALUE_LOCAL}"),
                                      ("invokestatic", f"#5|{PARSE_INT}"),
                                      ("putfield", f"#4|Field {field}:I"), ("label", f"{key}_end")]


def legacy_load(*blocks: list[tuple[str, str]]) -> analyzer.Member:
    # The loader tests the value local far less often than the key local, which
    # is how the analyzer tells the key chain from the value chains inside it.
    fillers = [integer_key(f"filler{n}", f"f{n}") for n in range(8)]
    items = [item for block in (*blocks, *fillers) for item in block] + [("return", "")]
    return analyzer.parse_members(listing("public void load()", assemble(items)))[0]


def legacy_save(*lines: str) -> analyzer.Member:
    body = [item for line in lines
            for item in (("aload_1", ""), ("ldc_w", f"#1|String {line}"), ("invokevirtual", f"#2|{PRINTLN}"))]
    return analyzer.parse_members(listing("public void save()", body + [("return", "")]))[0]


CLOUD_STORES = [("true", "iconst_2"), ("false", "iconst_0"), ("fast", "iconst_1")]
CLOUD_LINES = ("renderClouds:true", "renderClouds:fast", "renderClouds:false")


def cloud_record(stores=None, lines=CLOUD_LINES, descriptor: str = "I") -> dict:
    load = legacy_load(token_key("renderClouds", "j", descriptor, stores or CLOUD_STORES))
    return analyzer.extract_legacy([load, legacy_save(*lines)])["renderClouds"]


class LegacyLiteralTokens(unittest.TestCase):
    def test_a_key_loaded_against_fast_as_well_as_true_is_not_a_boolean(self) -> None:
        record = analyzer.legacy_load_blocks(legacy_load(token_key("renderClouds", "j", "I", CLOUD_STORES)))

        self.assertEqual(record["renderClouds"]["value_literals"], ["true", "false", "fast"])
        self.assertNotEqual(record["renderClouds"]["type"], "boolean")

    def test_a_key_compared_only_against_true_stays_a_boolean(self) -> None:
        record = analyzer.legacy_load_blocks(legacy_load(boolean_key("fullscreen", "g")))

        self.assertEqual(record["fullscreen"]["type"], "boolean")
        self.assertEqual(record["fullscreen"]["value_literals"], ["true"])

    def test_constant_key_value_lines_are_save_evidence(self) -> None:
        written = analyzer.legacy_constant_saves(legacy_save(*CLOUD_LINES, "lastServer:", "version:1139"))

        self.assertEqual(written["renderClouds"], ["true", "fast", "false"])
        self.assertEqual(written["version"], ["1139"])
        self.assertNotIn("lastServer", written)

    def test_a_constant_line_that_is_not_printed_is_not_save_evidence(self) -> None:
        body = [("ldc_w", "#1|String renderClouds:true"), ("pop", "")]
        member = analyzer.parse_members(listing("public void save()", body))[0]

        self.assertEqual(analyzer.legacy_constant_saves(member), {})

    def test_tokens_the_writer_emits_and_the_loader_reads_make_a_three_token_enum(self) -> None:
        record = cloud_record()

        self.assertEqual(record["type"], "enum")
        self.assertEqual(record["enum_persisted_form"], "string_token")
        self.assertTrue(record["literal_tokens_proven"])
        self.assertEqual(record["save_literals"], ["true", "fast", "false"])
        self.assertNotIn("legacy true/false", " ".join(record["notes"]))

    def test_the_token_domain_lists_every_written_token_in_stored_order(self) -> None:
        domain = analyzer.literal_token_domain(cloud_record())

        self.assertEqual([(v["persisted"], v["id"]) for v in domain["values"]],
                         [("false", 0), ("fast", 1), ("true", 2)])
        self.assertEqual(domain["persisted_form"], "string_token")

    def test_a_token_the_loader_does_not_read_back_proves_nothing(self) -> None:
        record = cloud_record(stores=[("true", "iconst_2"), ("false", "iconst_0")])

        self.assertEqual(record["type"], "unknown")
        self.assertFalse(record["literal_tokens_proven"])
        self.assertTrue(any("not proven" in note for note in record["notes"]))

    def test_a_loader_that_accepts_extra_literals_does_not_widen_the_written_set(self) -> None:
        record = cloud_record(lines=("renderClouds:true", "renderClouds:false"),
                              stores=CLOUD_STORES + [("1", "iconst_1")])

        self.assertEqual(record["save_literals"], ["true", "false"])
        self.assertFalse(record["literal_tokens_proven"])

    def test_a_boolean_backed_by_an_integer_field_is_not_typed_boolean(self) -> None:
        load = legacy_load(boolean_key("fullscreen", "g", descriptor="I"))
        record = analyzer.extract_legacy([load])["fullscreen"]

        self.assertEqual(record["type"], "unknown")
        self.assertTrue(any("not a boolean" in note for note in record["notes"]))

    def test_the_default_of_a_token_enum_is_the_token_its_stored_number_names(self) -> None:
        record = cloud_record()
        record["allowed_values"] = analyzer.literal_token_domain(record)
        initial = {"kind": "number", "value": 2.0, "descriptor": "I"}

        self.assertEqual(analyzer.resolve_default(record, initial, None), ("true", "high", None))

    def test_a_number_no_token_names_is_not_a_default(self) -> None:
        record = cloud_record()
        record["allowed_values"] = analyzer.literal_token_domain(record)
        value, confidence, _ = analyzer.resolve_default(
            record, {"kind": "number", "value": 7.0, "descriptor": "I"}, None)

        self.assertIsNone(value)
        self.assertEqual(confidence, "unknown")

    def test_a_boolean_never_takes_a_default_from_an_integer_field(self) -> None:
        record = {"type": "boolean", "field_descriptor": "I", "transform_ops": []}
        value, confidence, note = analyzer.resolve_default(
            record, {"kind": "number", "value": 2.0, "descriptor": "I"}, None)

        self.assertIsNone(value)
        self.assertEqual(confidence, "unknown")
        self.assertIn("boolean", note)

    def test_literals_that_store_one_enum_constant_each_bind_their_tokens(self) -> None:
        record = {"enum_class": "cuz", "save_literals": ["true", "fast", "false"], "literal_values": {
            "true": {"kind": "enum_constant", "enum_class": "cuz", "field": "c"},
            "false": {"kind": "enum_constant", "enum_class": "cuz", "field": "a"},
            "fast": {"kind": "enum_constant", "enum_class": "cuz", "field": "b"}}}

        self.assertEqual(analyzer.literal_enum_tokens(record), {"c": "true", "a": "false", "b": "fast"})

    def test_two_literals_storing_the_same_constant_bind_no_token(self) -> None:
        stored = {"kind": "enum_constant", "enum_class": "cuz", "field": "c"}
        record = {"enum_class": "cuz", "save_literals": ["true", "fast"],
                  "literal_values": {"true": stored, "fast": stored}}

        self.assertEqual(analyzer.literal_enum_tokens(record), {})

    def test_a_proven_token_enum_becomes_an_unquoted_enum_token_codec(self) -> None:
        record = cloud_record()
        record["allowed_values"] = analyzer.literal_token_domain(record)
        record.update(field="j", default="true", default_confidence="high")

        built = analyzer.build_setting("1.12.2", "0" * 64, "bid", "renderClouds", record)

        self.assertEqual(dataset.codec_for(built, "lwjgl2"),
                         {"kind": "enum_token", "tokens": ["false", "fast", "true"], "quoted": False})


class DataVersionMarker(unittest.TestCase):
    def test_a_release_without_version_json_reads_the_literal_compiled_into_the_root(self) -> None:
        marker = analyzer.data_version_marker({"bao": ["options.txt", "version:1343"]}, None)

        self.assertEqual(marker["value"], 1343)
        self.assertEqual(marker["confidence"], "medium")
        self.assertIn("bao", marker["evidence"])

    def test_version_json_names_the_data_version_when_the_root_only_concatenates_it(self) -> None:
        marker = analyzer.data_version_marker({"Options": ["version:"]}, b'{"world_version": 3465}')

        self.assertEqual(marker["value"], 3465)
        self.assertEqual(marker["confidence"], "high")

    def test_a_bare_prefix_and_no_version_json_leaves_the_value_unknown(self) -> None:
        marker = analyzer.data_version_marker({"Options": ["version:"]}, None)

        self.assertIsNone(marker["value"])
        self.assertEqual(marker["confidence"], "unknown")

    def test_a_world_version_that_is_not_an_integer_is_not_trusted(self) -> None:
        for raw in (b'{"world_version": "1952"}', b'{"world_version": true}', b'{"world_version": 1.5}',
                    b"not json", b"[]"):
            with self.subTest(raw=raw):
                marker = analyzer.data_version_marker({"Options": ["version:"]}, raw)

                self.assertIsNone(marker["value"])
                self.assertEqual(marker["confidence"], "unknown")

    def test_conflicting_literals_leave_the_value_unknown(self) -> None:
        marker = analyzer.data_version_marker({"a": ["version:1343"], "b": ["version:1519"]}, None)

        self.assertIsNone(marker["value"])

    def test_a_release_that_never_mentions_the_marker_has_none(self) -> None:
        self.assertIsNone(analyzer.data_version_marker({"Options": ["options.txt"]}, b'{"world_version": 1}'))


if __name__ == "__main__":
    unittest.main()
