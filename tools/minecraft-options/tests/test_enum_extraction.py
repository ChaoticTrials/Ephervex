#!/usr/bin/env python3
"""Tests for how the analyzer reads an enum's constants, ids and persisted form.

    python3 -m unittest discover -s tools/minecraft-options

Offline and deterministic: the disassembly is written out here, so nothing
depends on a client JAR or on `javap`.
"""

from __future__ import annotations

import unittest

import analyze_options as analyzer

ENUM = "net/example/Mode"
OWN = f"L{ENUM};"
TRANSLATION = "net/example/TranslationText"
CODEC = "Lcom/mojang/serialization/Codec;"
INIT = 'Method "<init>":(Ljava/lang/String;I)V'


def member(signature: str, body: list[tuple[str, str]]) -> str:
    """One disassembled member; instructions are numbered by position."""
    lines = [f"  {signature};", "    Code:"]
    for offset, (op, comment) in enumerate(body):
        arg, _, note = comment.partition("|")
        lines.append(f"      {offset:>4}: {op:<13} {arg}{'  // ' + note if note else ''}".rstrip())
    return "\n".join(lines)


def at(entries: list[tuple[int, str, str]]) -> list[analyzer.Insn]:
    """Instructions at explicit offsets, for branch targets."""
    lines = [f"      {offset:>4}: {op:<13} {comment}".rstrip() for offset, op, comment in entries]
    return analyzer.parse_members("  static void probe();\n    Code:\n" + "\n".join(lines))[0].insns


class Disassembly(analyzer.Disassembler):
    def __init__(self, text: str) -> None:
        super().__init__(None, {ENUM})
        self.text = text

    def get(self, class_name: str) -> str:
        return self.text if class_name == ENUM else ""


def plain_constant(name: str, ordinal: int, field: str, *, owner: str = ENUM,
                   extra: list[tuple[str, str]] | None = None) -> list[tuple[str, str]]:
    return [("new", f"#1|class {owner}"), ("dup", ""), ("ldc", f"#2|String {name}"),
            ("bipush", str(ordinal)), *(extra or []),
            ("invokespecial", f"#3|Method {owner}.\"<init>\":(Ljava/lang/String;I)V"
             if owner != ENUM else f"#3|{INIT}"),
            ("putstatic", f"#4|Field {field}:{OWN}")]


def translated_constant(name: str, ordinal: int, field: str, key: str) -> list[tuple[str, str]]:
    return plain_constant(name, ordinal, field, extra=[
        ("new", f"#5|class {TRANSLATION}"), ("dup", ""), ("ldc", f"#6|String {key}"),
        ("iconst_0", ""), ("anewarray", "#7|class java/lang/Object"),
        ("invokespecial", f'#8|Method {TRANSLATION}."<init>":(Ljava/lang/String;[Ljava/lang/Object;)V')])


def values_array(fields: list[str]) -> list[tuple[str, str]]:
    body: list[tuple[str, str]] = [("bipush", str(len(fields))), ("anewarray", f"#9|class {ENUM}")]
    for index, field in enumerate(fields):
        body += [("dup", ""), ("bipush", str(index)), ("getstatic", f"#10|Field {field}:{OWN}"),
                 ("aastore", "")]
    return body + [("areturn", "")]


def enum_class(constants: list[tuple[str, list[tuple[str, str]]]], *,
               tail: list[tuple[str, str]] | None = None,
               declared: list[str] | None = None) -> str:
    """An enum whose `<clinit>` builds `constants` and then runs `tail`."""
    body = [instruction for _, part in constants for instruction in part]
    fields = declared if declared is not None else [field for field, _ in constants]
    body += [("invokestatic", f"#11|Method $values:()[{OWN}"), ("putstatic", f"#12|Field $VALUES:[{OWN}")]
    body += tail or []
    body.append(("return", ""))
    return "\n".join([
        member(f"private static {ENUM.replace('/', '.')}[] $values()", values_array(fields)),
        member("static {}", body),
    ])


def extract(text: str) -> dict | None:
    return analyzer.extract_enum(Disassembly(text), ENUM)


class ConstantBoundaries(unittest.TestCase):
    def test_a_constructor_argument_does_not_end_the_constant_it_belongs_to(self) -> None:
        info = extract(enum_class([
            ("LEFT", translated_constant("LEFT", 0, "LEFT", "options.mainHand.left")),
            ("RIGHT", translated_constant("RIGHT", 1, "RIGHT", "options.mainHand.right")),
        ]))
        self.assertEqual([c["id"] for c in info["constants"]], [0, 1])
        self.assertEqual([c["ordinal"] for c in info["constants"]], [0, 1])
        self.assertEqual([c["constructor_descriptor"] for c in info["constants"]],
                         ["(Ljava/lang/String;I)V"] * 2)

    def test_the_translation_key_of_a_nested_argument_still_belongs_to_its_constant(self) -> None:
        info = extract(enum_class([
            ("LEFT", translated_constant("LEFT", 0, "LEFT", "options.mainHand.left")),
        ]))
        self.assertEqual(info["constants"][0]["translation_key"], "options.mainHand.left")
        self.assertIsNone(info["constants"][0]["serialized_name"])

    def test_the_first_integer_after_the_ordinal_is_the_constant_id(self) -> None:
        info = extract(enum_class([
            ("EASY", plain_constant("EASY", 0, "EASY", extra=[("bipush", "5")])),
            ("HARD", plain_constant("HARD", 1, "HARD", extra=[("bipush", "9")])),
        ]))
        self.assertEqual([c["id"] for c in info["constants"]], [5, 9])

    def test_a_constant_with_a_body_is_listed_with_the_plain_ones(self) -> None:
        body_class = f"{ENUM}$1"
        info = extract(enum_class([
            ("A", plain_constant("A", 0, "A")),
            ("B", plain_constant("B", 1, "B", owner=body_class)),
        ]))
        self.assertEqual([c["name"] for c in info["constants"]], ["A", "B"])
        self.assertEqual([c["field"] for c in info["constants"]], ["A", "B"])

    def test_a_body_class_instance_that_is_not_stored_in_the_enum_is_not_a_constant(self) -> None:
        stray = [("new", f"#1|class {ENUM}$Helper"), ("dup", ""), ("ldc", "#2|String HELPER"),
                 ("iconst_0", ""),
                 ("invokespecial", f'#3|Method {ENUM}$Helper."<init>":(Ljava/lang/String;I)V'),
                 ("pop", "")]
        info = extract(enum_class([("A", plain_constant("A", 0, "A")), ("", stray)], declared=["A"]))
        self.assertEqual([c["name"] for c in info["constants"]], ["A"])


class DeclaredConstantList(unittest.TestCase):
    def test_a_constant_list_that_misses_a_declared_constant_is_not_returned(self) -> None:
        text = enum_class([("A", plain_constant("A", 0, "A")), ("B", plain_constant("B", 1, "B"))],
                          declared=["A", "B", "C"])
        self.assertIsNone(extract(text))

    def test_a_constant_the_array_does_not_declare_is_not_returned(self) -> None:
        text = enum_class([("A", plain_constant("A", 0, "A")), ("B", plain_constant("B", 1, "B"))],
                          declared=["A"])
        self.assertIsNone(extract(text))

    def test_an_array_built_inline_by_an_older_compiler_is_checked_too(self) -> None:
        constants = [("A", plain_constant("A", 0, "A")), ("B", plain_constant("B", 1, "B"))]
        inline = [instruction for _, part in constants for instruction in part]
        inline += values_array(["A", "B"])[:-1] + [("putstatic", f"#12|Field $VALUES:[{OWN}")]
        text = member("static {}", inline + [("return", "")])
        self.assertEqual([c["name"] for c in extract(text)["constants"]], ["A", "B"])
        short = member("static {}", [i for _, part in constants[:1] for i in part]
                       + values_array(["A", "B"])[:-1] + [("putstatic", f"#12|Field $VALUES:[{OWN}")]
                       + [("return", "")])
        self.assertIsNone(extract(short))

    def test_an_id_lookup_array_is_not_mistaken_for_the_constant_array(self) -> None:
        lookup = [("invokestatic", f"#13|Method values:()[{OWN}"), ("arraylength", ""),
                  ("anewarray", f"#14|class {ENUM}"), ("putstatic", f"#15|Field BY_ID:[{OWN}")]
        text = enum_class([("A", plain_constant("A", 0, "A")), ("B", plain_constant("B", 1, "B"))],
                          tail=lookup)
        self.assertEqual([c["name"] for c in extract(text)["constants"]], ["A", "B"])

    def test_an_obfuscated_array_field_is_checked_by_its_type(self) -> None:
        constants = [("A", plain_constant("A", 0, "a")), ("B", plain_constant("B", 1, "b"))]
        body = [i for _, part in constants for i in part]
        body += [("invokestatic", f"#11|Method c:()[{OWN}"), ("putstatic", f"#12|Field d:[{OWN}"),
                 ("return", "")]
        text = "\n".join([member(f"private static {ENUM.replace('/', '.')}[] c()",
                                values_array(["a"])),
                         member("static {}", body)])
        self.assertIsNone(extract(text))


class CodecInitializers(unittest.TestCase):
    TAIL_STRING = [("invokedynamic", "#20|InvokeDynamic #0:get:()Ljava/util/function/Supplier;"),
                   ("invokestatic", "#21|Method net/minecraft/util/StringRepresentable.fromEnum:"
                                    "(Ljava/util/function/Supplier;)Lcom/mojang/serialization/Codec;"),
                   ("putstatic", f"#22|Field CODEC:{CODEC}")]
    TAIL_INT = [("getstatic", f"#23|Field com/mojang/serialization/Codec.INT:{CODEC}"),
                ("invokestatic", "#24|Method net/minecraft/util/ByIdMap.codec:()V"),
                ("putstatic", f"#25|Field INT_CODEC:{CODEC}")]
    TAIL_STREAM = [("invokestatic", "#26|Method net/minecraft/network/codec/ByteBufCodecs.idMapper:()V"),
                   ("putstatic", "#27|Field STREAM_CODEC:Lnet/minecraft/network/codec/StreamCodec;")]

    def constants(self) -> list[tuple[str, list[tuple[str, str]]]]:
        return [("A", plain_constant("A", 0, "A")), ("B", plain_constant("B", 1, "B"))]

    def test_a_codec_field_is_judged_on_its_own_initializer_only(self) -> None:
        codecs = extract(enum_class(self.constants(),
                                    tail=self.TAIL_STRING + self.TAIL_INT))["codec_fields"]
        self.assertEqual(codecs["CODEC"]["persisted_form"], "string_token")
        self.assertEqual(codecs["INT_CODEC"]["persisted_form"], "integer_id")

    def test_the_order_of_the_codec_fields_does_not_change_their_forms(self) -> None:
        codecs = extract(enum_class(self.constants(),
                                    tail=self.TAIL_INT + self.TAIL_STRING))["codec_fields"]
        self.assertEqual(codecs["CODEC"]["persisted_form"], "string_token")
        self.assertEqual(codecs["INT_CODEC"]["persisted_form"], "integer_id")

    def test_a_network_codec_is_not_a_persisted_form(self) -> None:
        codecs = extract(enum_class(self.constants(),
                                    tail=self.TAIL_STRING + self.TAIL_STREAM))["codec_fields"]
        self.assertEqual(list(codecs), ["CODEC"])


class TokenComparisons(unittest.TestCase):
    def bind(self, entries: list[tuple[int, str, str]]) -> tuple[dict[str, str], list[str], bool]:
        return analyzer.token_bindings(at(entries), ENUM)

    def test_a_literal_after_a_not_equal_branch_belongs_to_the_compared_constant(self) -> None:
        bindings, leftovers, understood = self.bind([
            (0, "aload_0", ""), (1, "getstatic", f"#1  // Field {ENUM}.A:{OWN}"),
            (4, "if_acmpne", "11"), (7, "ldc", "#2  // String a"), (9, "goto", "13"),
            (11, "ldc", "#3  // String b"), (13, "areturn", "")])
        self.assertEqual((bindings, leftovers, understood), ({"A": "a"}, ["b"], True))

    def test_an_inverted_comparison_binds_the_literal_at_the_jump_target(self) -> None:
        bindings, leftovers, understood = self.bind([
            (0, "aload_0", ""), (1, "getstatic", f"#1  // Field {ENUM}.A:{OWN}"),
            (4, "if_acmpeq", "11"), (7, "ldc", "#2  // String b"), (9, "goto", "13"),
            (11, "ldc", "#3  // String a"), (13, "areturn", "")])
        self.assertEqual((bindings, leftovers, understood), ({"A": "a"}, ["b"], True))

    def test_the_remaining_constant_takes_the_other_branch_of_an_inverted_comparison(self) -> None:
        info = {"constants": [{"name": "A", "field": "A"}, {"name": "B", "field": "B"}]}
        bindings, leftovers, _ = self.bind([
            (1, "getstatic", f"#1  // Field {ENUM}.A:{OWN}"), (4, "if_acmpeq", "11"),
            (7, "ldc", "#2  // String b"), (9, "goto", "13"), (11, "ldc", "#3  // String a")])
        self.assertEqual(analyzer.complete_token_map(info, bindings, leftovers), {"A": "a", "B": "b"})

    def test_a_constant_loaded_without_a_known_comparison_is_not_trusted(self) -> None:
        _, _, understood = self.bind([
            (1, "getstatic", f"#1  // Field {ENUM}.A:{OWN}"), (4, "areturn", ""),
            (5, "ldc", "#2  // String a")])
        self.assertFalse(understood)


class StringLambdas(unittest.TestCase):
    def lambda_member(self, name: str, literal: str | None) -> str:
        body = ([("ldc", f"#1|String {literal}")] if literal else [("aload_0", "")]) + [("areturn", "")]
        return member(f"private static java.lang.String {name}({ENUM.replace('/', '.')})", body)

    def test_a_single_lambda_with_literals_is_unambiguous(self) -> None:
        members = analyzer.parse_members("\n".join([self.lambda_member("lambda$a", "x"),
                                                    self.lambda_member("lambda$b", None)]))
        insns, single = analyzer.enum_string_lambda(members, ENUM)
        self.assertTrue(single)
        self.assertEqual(insns[0].string(), "x")

    def test_two_lambdas_with_literals_leave_the_tokens_ambiguous(self) -> None:
        members = analyzer.parse_members("\n".join([self.lambda_member("lambda$a", "x"),
                                                    self.lambda_member("lambda$b", "y")]))
        _, single = analyzer.enum_string_lambda(members, ENUM)
        self.assertFalse(single)


class IntegerIds(unittest.TestCase):
    def test_duplicate_ids_prove_no_persisted_id(self) -> None:
        info = {"class": ENUM, "codec_fields": {}, "constants": [
            {"name": "A", "field": "A", "ordinal": 0, "id": 0, "serialized_name": None, "translation_key": None},
            {"name": "B", "field": "B", "ordinal": 1, "id": 0, "serialized_name": None, "translation_key": None}]}
        domain = analyzer.enum_domain(info, "integer_id")
        self.assertEqual([v["persisted"] for v in domain["values"]], [None, None])
        self.assertIsNone(analyzer.enum_persisted_value(info, "A", "integer_id"))

    def test_distinct_ids_are_the_persisted_values(self) -> None:
        info = {"class": ENUM, "codec_fields": {}, "constants": [
            {"name": "A", "field": "A", "ordinal": 0, "id": 3, "serialized_name": None, "translation_key": None},
            {"name": "B", "field": "B", "ordinal": 1, "id": 7, "serialized_name": None, "translation_key": None}]}
        domain = analyzer.enum_domain(info, "integer_id")
        self.assertEqual([v["persisted"] for v in domain["values"]], [3, 7])
        self.assertEqual(analyzer.enum_persisted_value(info, "B", "integer_id"), 7)


class DomainConfidence(unittest.TestCase):
    def test_a_record_that_lowers_its_domain_confidence_keeps_it_in_the_setting(self) -> None:
        record = {"type": "enum", "field": "f", "allowed_values": {"values": [{"persisted": "a"}]},
                  "domain_confidence": "low"}
        setting = analyzer.build_setting("1.21", "sha", None, "key", record)
        self.assertEqual(setting["domain_confidence"], "low")


if __name__ == "__main__":
    unittest.main()
