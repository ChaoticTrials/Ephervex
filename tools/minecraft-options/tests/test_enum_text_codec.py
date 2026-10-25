#!/usr/bin/env python3
"""Tests for how the analyzer proves the text tokens of an enum from its codec lambdas.

    python3 -m unittest discover -s tools/minecraft-options

Offline and deterministic: the disassembly is written out here, so nothing
depends on a client JAR or on `javap`. The listings follow what javac emits for
a string switch (`hashCode`, `equals`, a second switch on the match index), an
ordinal switch through a `$SwitchMap` class, and an `Either.map` of two lambdas.
"""

from __future__ import annotations

import unittest

import analyze_options as analyzer
import token_lambdas
from tests.test_enum_extraction import enum_class, plain_constant

ROOT = "net/example/Options"
ENUM = "net/example/Mode"
MAP = f"{ROOT}$4"
OWN = f"L{ENUM};"
EITHER = "com/mojang/datafixers/util/Either"
FUNCTION = "Ljava/util/function/Function;"
CONSTANTS = [("a", "OFF", 0), ("b", "FAST", 1), ("c", "FANCY", 2)]

Row = tuple[int, str, str]


def method(signature: str, rows: list[Row | str]) -> str:
    """One member; a string row is a switch table printed as javap prints it."""
    lines = [f"  {signature};", "    Code:"]
    for row in rows:
        if isinstance(row, str):
            lines.append(row)
        else:
            offset, op, comment = row
            operand = comment if comment.isdigit() else f"#1  // {comment}" if comment else ""
            lines.append(f"      {offset:>4}: {op:<13} {operand}".rstrip())
    return "\n".join(lines)


def switch(offset: int, kind: str, cases: dict[int | str, int]) -> str:
    table = "\n".join(f"      {value:>12}: {target}" for value, target in cases.items())
    return f"      {offset:>4}: {kind}  {{ // {len(cases) - 1}\n{table}\n            }}"


def text_reader(*, fast_returns: str = "b") -> str:
    """`switch (text) { case "true" -> FANCY; case "fast" -> FAST; default -> OFF; }`."""
    equals = "Method java/lang/String.equals:(Ljava/lang/Object;)Z"
    return method(f"private static {ENUM.replace('/', '.')} read(java.lang.String)", [
        (0, "aload_0", ""), (1, "astore_1", ""), (2, "iconst_m1", ""), (3, "istore_2", ""),
        (4, "aload_1", ""), (5, "invokevirtual", "Method java/lang/String.hashCode:()I"),
        switch(8, "lookupswitch", {3135580: 51, 3569038: 36, 97196323: 63, "default": 63}),
        (36, "aload_1", ""), (37, "ldc_w", "String true"), (40, "invokevirtual", equals),
        (43, "ifeq", "63"), (46, "iconst_0", ""), (47, "istore_2", ""), (48, "goto", "63"),
        (51, "aload_1", ""), (52, "ldc_w", "String fast"), (55, "invokevirtual", equals),
        (58, "ifeq", "63"), (61, "iconst_1", ""), (62, "istore_2", ""),
        (63, "iload_2", ""),
        switch(64, "lookupswitch", {0: 92, 1: 98, "default": 104}),
        (92, "getstatic", f"Field {ENUM}.c:{OWN}"), (95, "goto", "107"),
        (98, "getstatic", f"Field {ENUM}.{fast_returns}:{OWN}"), (101, "goto", "107"),
        (104, "getstatic", f"Field {ENUM}.a:{OWN}"),
        (107, "areturn", ""),
    ])


def ordinal_writer(*, result: str = "java.lang.String") -> str:
    """`switch (mode) { case FANCY -> "true"; case FAST -> "fast"; default -> "false"; }` through a switch map."""
    ret = "areturn"
    return method(f"private static {result} write({ENUM.replace('/', '.')})", [
        (0, "getstatic", f"Field {MAP}.a:[I"), (3, "aload_0", ""),
        (4, "invokevirtual", f"Method {ENUM}.ordinal:()I"), (7, "iaload", ""),
        switch(8, "tableswitch", {1: 36, 2: 40, 3: 44, "default": 44}),
        (36, "ldc_w", "String true"), (39, ret, ""),
        (40, "ldc_w", "String fast"), (43, ret, ""),
        (44, "ldc_w", "String false"), (47, ret, ""),
    ])


def switch_map(stores: dict[str, int] | None = None) -> str:
    stores = stores if stores is not None else {"c": 1, "b": 2, "a": 3}
    rows: list[Row] = [(0, "invokestatic", f"Method {ENUM}.values:()[{OWN}"), (3, "arraylength", ""),
                       (4, "newarray", ""), (6, "putstatic", "Field a:[I")]
    offset = 9
    for field, number in stores.items():
        rows += [(offset, "getstatic", "Field a:[I"), (offset + 3, "getstatic", f"Field {ENUM}.{field}:{OWN}"),
                 (offset + 6, "invokevirtual", f"Method {ENUM}.ordinal:()I"),
                 (offset + 9, "iconst_" + str(number), ""), (offset + 10, "iastore", "")]
        offset += 15
    return method("static {}", rows + [(offset, "return", "")])


def bootstrap_text(targets: list[tuple[str, str]]) -> str:
    lines = ["BootstrapMethods:"]
    for index, (name, descriptor) in enumerate(targets):
        lines += [f"  {index}: #1 REF_invokeStatic java/lang/invoke/LambdaMetafactory.metafactory:(...)V",
                  "    Method arguments:",
                  f"      #2 REF_invokeStatic {ROOT}.{name}:{descriptor}"]
    return "\n".join(lines)


class Jar(analyzer.Disassembler):
    def __init__(self, classes: dict[str, str], bootstraps: list[tuple[str, str]] | None = None) -> None:
        super().__init__(None, set(classes))
        self.classes = classes
        self.bootstraps = bootstrap_text(bootstraps or [])

    def get(self, class_name: str) -> str:
        return self.classes.get(class_name, "")

    def verbose(self, class_name: str) -> str:
        return self.bootstraps if class_name == ROOT else ""


STRING_LAMBDAS = [("read", f"(Ljava/lang/String;){OWN}"), ("write", f"({OWN})Ljava/lang/String;")]


def mode_enum() -> str:
    return enum_class([(field, plain_constant(name, ordinal, field)) for field, name, ordinal in CONSTANTS])


def jar(reader: str | None = None, writer: str | None = None, maps: str | None = None,
        bootstraps: list[tuple[str, str]] | None = None) -> Jar:
    return Jar({ROOT: (reader or text_reader()) + "\n" + (writer or ordinal_writer()),
                MAP: maps or switch_map(), ENUM: mode_enum()},
               bootstraps if bootstraps is not None else STRING_LAMBDAS)


def prove(dis: Jar, reader: tuple[str, str, str], writer: tuple[str, str, str], *, wrapped: bool = False):
    info = analyzer.extract_enum(dis, ENUM)
    world = analyzer.ClassWorld(dis)
    return token_lambdas.prove_enum_tokens(world, ENUM, info["constants"], reader, writer, wrapped)


PLAIN_READER = (ROOT, "read", f"(Ljava/lang/String;){OWN}")
PLAIN_WRITER = (ROOT, "write", f"({OWN})Ljava/lang/String;")


class StringSwitchCodec(unittest.TestCase):
    def test_the_tokens_are_the_ones_the_writer_returns_for_each_constant(self) -> None:
        self.assertEqual(prove(jar(), PLAIN_READER, PLAIN_WRITER), {"c": "true", "b": "fast", "a": "false"})

    def test_a_reader_that_returns_another_constant_for_a_written_token_proves_nothing(self) -> None:
        wrong = jar(reader=text_reader(fast_returns="a"))
        self.assertIsNone(prove(wrong, PLAIN_READER, PLAIN_WRITER))

    def test_a_constant_that_falls_through_to_another_constants_token_proves_nothing(self) -> None:
        unmapped = jar(maps=switch_map({"b": 2, "a": 3}))
        self.assertIsNone(prove(unmapped, PLAIN_READER, PLAIN_WRITER))

    def test_a_constant_the_default_arm_names_is_proven_by_its_round_trip(self) -> None:
        unmapped = jar(maps=switch_map({"c": 1, "b": 2}))
        self.assertEqual(prove(unmapped, PLAIN_READER, PLAIN_WRITER), {"c": "true", "b": "fast", "a": "false"})

    def test_a_writer_that_does_not_return_text_proves_nothing(self) -> None:
        self.assertIsNone(prove(jar(), PLAIN_READER, PLAIN_WRITER, wrapped=True))

    def test_a_missing_lambda_proves_nothing(self) -> None:
        self.assertIsNone(prove(jar(), PLAIN_READER, (ROOT, "absent", PLAIN_WRITER[2])))

    def test_the_hash_a_string_switch_compares_matches_javas(self) -> None:
        self.assertEqual([token_lambdas.java_string_hash(t) for t in ("true", "fast", "false")],
                         [3569038, 3135580, 97196323])

    def test_switch_tables_are_read_with_their_default(self) -> None:
        member = analyzer.parse_members(text_reader())[0]
        self.assertEqual(member.switches[64], ({0: 92, 1: 98}, 104))
        self.assertEqual(member.switches[8][1], 63)


def visitor_body() -> str:
    call = ("InterfaceMethod net/example/Options$FieldAccess.process:(Ljava/lang/String;Ljava/lang/Object;"
            f"{FUNCTION}{FUNCTION})Ljava/lang/Object;")
    return method("public void processOptions(net.example.Options$FieldAccess)", [
        (0, "aload_0", ""), (1, "aload_1", ""), (2, "ldc_w", "String renderClouds"), (5, "aload_0", ""),
        (6, "getfield", f"Field clouds:{OWN}"),
        (9, "invokedynamic", f"InvokeDynamic #0:apply:(){FUNCTION}"),
        (14, "invokedynamic", f"InvokeDynamic #1:apply:(){FUNCTION}"),
        (19, "invokeinterface", call), (24, "checkcast", f"class {ENUM}"),
        (27, "putfield", f"Field clouds:{OWN}"),
    ])


class FieldAccessEnum(unittest.TestCase):
    def extract(self, dis: Jar) -> dict:
        members = analyzer.parse_members(dis.get(ROOT) + "\n" + visitor_body())
        return analyzer.extract_visitor(dis, members, None, ROOT)["renderClouds"]

    def test_a_proven_codec_gives_each_constant_its_written_token(self) -> None:
        record = self.extract(jar())
        self.assertEqual(record["allowed_values"]["persisted_form"], "string_token")
        self.assertEqual({v["enum_constant"]: v["persisted"] for v in record["allowed_values"]["values"]},
                         {"OFF": "false", "FAST": "fast", "FANCY": "true"})
        self.assertEqual(record["enum_token_map"], {"c": "true", "b": "fast", "a": "false"})

    def test_an_unproven_codec_leaves_every_token_unknown(self) -> None:
        record = self.extract(jar(reader=text_reader(fast_returns="a")))
        self.assertEqual([v["persisted"] for v in record["allowed_values"]["values"]], [None] * 3)
        self.assertNotIn("enum_token_map", record)

    def test_lambdas_of_the_wrong_types_are_not_read_as_a_text_codec(self) -> None:
        swapped = jar(bootstraps=list(reversed(STRING_LAMBDAS)))
        self.assertIsNone(analyzer.visitor_text_lambdas(
            swapped, ROOT, analyzer.parse_members(visitor_body())[0].insns[:-3], ENUM))


def either_reader() -> str:
    """`either.map(flag -> flag ? FANCY : OFF, text)` where the text lambda is `read`."""
    return method(f"private static {ENUM.replace('/', '.')} either({EITHER.replace('/', '.')})", [
        (0, "aload_0", ""), (1, "invokedynamic", f"InvokeDynamic #2:apply:(){FUNCTION}"),
        (6, "invokedynamic", f"InvokeDynamic #3:apply:(){FUNCTION}"),
        (11, "invokevirtual", f"Method {EITHER}.map:({FUNCTION}{FUNCTION})Ljava/lang/Object;"),
        (14, "checkcast", f"class {ENUM}"), (17, "areturn", ""),
    ]) + "\n" + method(f"private static {ENUM.replace('/', '.')} flag(java.lang.Boolean)", [
        (0, "aload_0", ""), (1, "invokevirtual", "Method java/lang/Boolean.booleanValue:()Z"),
        (4, "ifeq", "13"), (7, "getstatic", f"Field {ENUM}.c:{OWN}"), (10, "goto", "16"),
        (13, "getstatic", f"Field {ENUM}.a:{OWN}"), (16, "areturn", ""),
    ])


def either_writer(wrap: str = "right") -> str:
    return method(f"private static {EITHER.replace('/', '.')} wrapped({ENUM.replace('/', '.')})", [
        (0, "aload_0", ""), (1, "invokestatic", f"Method write:({OWN})Ljava/lang/String;"),
        (4, "invokestatic", f"Method {EITHER}.{wrap}:(Ljava/lang/Object;)L{EITHER};"),
        (7, "areturn", ""),
    ])


EITHER_LAMBDAS = STRING_LAMBDAS + [("flag", f"(Ljava/lang/Boolean;){OWN}"), ("read", f"(Ljava/lang/String;){OWN}")]
EITHER_READER = (ROOT, "either", f"(L{EITHER};){OWN}")
EITHER_WRITER = (ROOT, "wrapped", f"({OWN})L{EITHER};")


def either_jar(wrap: str = "right") -> Jar:
    return Jar({ROOT: text_reader() + "\n" + ordinal_writer() + "\n" + either_reader() + "\n" + either_writer(wrap),
                MAP: switch_map(), ENUM: mode_enum()}, EITHER_LAMBDAS)


class EitherCodec(unittest.TestCase):
    def test_the_text_on_the_right_of_an_either_is_the_token(self) -> None:
        self.assertEqual(prove(either_jar(), EITHER_READER, EITHER_WRITER, wrapped=True),
                         {"c": "true", "b": "fast", "a": "false"})

    def test_a_writer_that_puts_the_text_on_the_left_proves_nothing(self) -> None:
        self.assertIsNone(prove(either_jar("left"), EITHER_READER, EITHER_WRITER, wrapped=True))


OPTION = "net/example/OptionInstance"
CODEC = "com/mojang/serialization/Codec"
EITHER_CALL = (f"InterfaceMethod {CODEC}.either:(L{CODEC};L{CODEC};)L{CODEC};")
XMAP_CALL = f"InterfaceMethod {CODEC}.xmap:({FUNCTION}{FUNCTION})L{CODEC};"
WINDOW_LAMBDAS = [("either", f"(L{EITHER};){OWN}"), ("wrapped", f"({OWN})L{EITHER};"),
                  ("flag", f"(Ljava/lang/Boolean;){OWN}"), ("read", f"(Ljava/lang/String;){OWN}")]


def option_window(codecs: tuple[str, str] = ("BOOL", "STRING")) -> list[analyzer.Insn]:
    """`new OptionInstance("options.renderClouds", ..., new Enum(values, Codec.either(a, b).xmap(f, g)), FANCY, ...)`."""
    body = [
        "String options.renderClouds", f"Method {OPTION}.a:()L{OPTION}$l;", f"Method {OPTION}.b:()L{OPTION}$b;",
        f"class {OPTION}$e", None, f"Method {ENUM}.values:()[{OWN}",
        "Method java/util/Arrays.asList:([Ljava/lang/Object;)Ljava/util/List;",
        f"Field {CODEC}.{codecs[0]}:L{CODEC};", f"Field {CODEC}.{codecs[1]}:L{CODEC};", EITHER_CALL,
        f"InvokeDynamic #0:apply:(){FUNCTION}", f"InvokeDynamic #1:apply:(){FUNCTION}", XMAP_CALL,
        f'Method {OPTION}$e."<init>":(Ljava/util/List;L{CODEC};)V', f"Field {ENUM}.c:{OWN}",
        "InvokeDynamic #4:accept:()Ljava/util/function/Consumer;",
        f'Method {OPTION}."<init>":(Ljava/lang/String;L{OPTION}$l;L{OPTION}$b;L{OPTION}$n;Ljava/lang/Object;'
        "Ljava/util/function/Consumer;)V",
    ]
    ops = ["ldc_w", "invokestatic", "invokestatic", "new", "dup", "invokestatic", "invokestatic", "getstatic",
           "getstatic", "invokestatic", "invokedynamic", "invokedynamic", "invokeinterface", "invokespecial",
           "getstatic", "invokedynamic", "invokespecial"]
    rows = [(index * 3, op, comment or "") for index, (op, comment) in enumerate(zip(ops, body))]
    return analyzer.parse_members(method("public Options()", rows))[0].insns


class OptionInstanceEnum(unittest.TestCase):
    def analyze(self, window: list[analyzer.Insn]) -> dict:
        dis = either_jar()
        dis.bootstraps = bootstrap_text(WINDOW_LAMBDAS)
        members = analyzer.parse_members(dis.get(ROOT))
        return analyzer.analyse_option_window(dis, window, OPTION, members, ROOT)

    def test_a_proven_inline_text_codec_gives_the_enum_its_tokens_and_default(self) -> None:
        record = self.analyze(option_window())

        self.assertEqual(record["type"], "enum")
        self.assertEqual(record["file_encoding"], "enum serialized string token")
        self.assertEqual({v["enum_constant"]: v["persisted"] for v in record["allowed_values"]["values"]},
                         {"OFF": "false", "FAST": "fast", "FANCY": "true"})
        self.assertEqual(record["default"], "true")
        self.assertIs(record["codec"]["transform_resolved"], True)
        self.assertFalse(any("could not read" in note for note in record["notes"]))

    def test_a_codec_whose_text_side_is_not_proven_stays_an_unread_transform(self) -> None:
        record = self.analyze(option_window(("STRING", "BOOL")))

        self.assertIs(record["codec"]["transform_resolved"], False)
        self.assertNotIn("enum_token_map", record)
        self.assertIsNone(record["default"])


def codec_ops(*names: str) -> list[analyzer.Insn]:
    rows: list[Row] = []
    for offset, name in enumerate(names):
        if name == "either":
            rows.append((offset, "invokestatic", "InterfaceMethod com/mojang/serialization/Codec.either:"
                         "(Lcom/mojang/serialization/Codec;Lcom/mojang/serialization/Codec;)"
                         "Lcom/mojang/serialization/Codec;"))
        elif name == "indy":
            rows.append((offset, "invokedynamic", f"InvokeDynamic #0:apply:(){FUNCTION}"))
        else:
            rows.append((offset, "getstatic", f"Field com/mojang/serialization/Codec.{name}:"
                         "Lcom/mojang/serialization/codecs/PrimitiveCodec;"))
    return analyzer.parse_members(method("static void probe()", rows))[0].insns


class CodecShape(unittest.TestCase):
    def test_either_of_a_flag_and_text_carries_the_text_on_the_right(self) -> None:
        self.assertEqual(analyzer.text_codec_shape(codec_ops("BOOL", "STRING", "either", "indy", "indy")),
                         "wrapped")

    def test_the_plain_string_codec_carries_the_text_itself(self) -> None:
        self.assertEqual(analyzer.text_codec_shape(codec_ops("STRING", "indy", "indy")), "plain")

    def test_either_in_the_other_order_says_nothing_about_where_the_text_sits(self) -> None:
        self.assertIsNone(analyzer.text_codec_shape(codec_ops("STRING", "BOOL", "either", "indy", "indy")))

    def test_another_codec_says_nothing_about_text(self) -> None:
        self.assertIsNone(analyzer.text_codec_shape(codec_ops("INT", "indy", "indy")))


if __name__ == "__main__":
    unittest.main()
