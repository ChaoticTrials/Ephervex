#!/usr/bin/env python3
"""Tests for how the analyzer turns `javap -c` text into instructions.

    python3 -m unittest discover -s tools/minecraft-options

Offline and deterministic: the listings below are written out in javap's
format, so nothing depends on a client JAR or on `javap`.
"""

from __future__ import annotations

import struct
import unittest

import analyze_options as analyzer

# Format of `javap -p -c -constants` as printed by JDK 25 for a string
# constant: the value follows `String ` unquoted, and trailing whitespace is
# never printed, so an empty constant is a bare `String`.
STRING_CONSTANTS = """\
  public Probe();
    Code:
       0: aload_0
       1: ldc           #7                  // String
       3: putfield      #9                  // Field empty:Ljava/lang/String;
       6: aload_0
       7: ldc           #15                 // String a
      10: putfield      #17                 // Field plain:Ljava/lang/String;
      13: aload_0
      14: ldc           #20                 // String  a
      17: putfield      #22                 // Field leading:Ljava/lang/String;
      20: aload_0
      21: ldc           #30                 // String x  y
      24: putfield      #32                 // Field inner:Ljava/lang/String;
      27: aload_0
      28: ldc           #40                 // String http://host/path // note
      31: putfield      #42                 // Field slashes:Ljava/lang/String;
      34: return
"""

# A `lookupswitch` and a `tableswitch` whose case values collide with the
# offsets of real instructions (`32:` and `36:` below).
SWITCH_TABLES = """\
  private static java.lang.String pick(java.lang.Integer);
    Code:
       0: aload_0
       1: invokevirtual #213                // Method java/lang/Integer.intValue:()I
       4: lookupswitch  { // 2
                      70: 32
                     110: 45
                 default: 58
            }
      32: aload_0
      33: ldc_w         #2523               // String options.fov.min
      36: tableswitch   { // 1 to 3
                       1: 46
                       2: 55
                       3: 64
                 default: 36
            }
      46: areturn
"""

# javap indents a switch case row like an instruction, with a number first.
CASE_ROWS_WITH_OFFSET_VALUES = """\
  private static void collide(int);
    Code:
       0: iload_0
       1: lookupswitch  { // 2
                      32: 45
                      36: 50
                 default: 58
            }
      32: aconst_null
      36: areturn
      45: return
"""


def utf8_entry(raw: bytes) -> bytes:
    return b"\x01" + struct.pack(">H", len(raw)) + raw


def class_with_strings(*values: bytes) -> bytes:
    """A class-file pool holding one CONSTANT_String per value, at pool index 2n + 1."""
    pool = b""
    for n, raw in enumerate(values):
        pool += b"\x08" + struct.pack(">H", 2 * n + 2) + utf8_entry(raw)
    return b"\xca\xfe\xba\xbe" + struct.pack(">HHH", 0, 65, 2 * len(values) + 1) + pool


def members(text: str) -> dict[str, analyzer.Member]:
    return {m.name: m for m in analyzer.parse_members(text)}


class StringConstants(unittest.TestCase):
    def strings_of(self) -> dict[str, str | None]:
        insns = analyzer.parse_members(STRING_CONSTANTS)[0].insns
        out: dict[str, str | None] = {}
        for ins, nxt in zip(insns, insns[1:]):
            if ins.op == "ldc":
                out[nxt.field()[1]] = ins.string()
        return out

    def test_an_empty_string_constant_is_the_empty_string_not_missing(self) -> None:
        self.assertEqual(self.strings_of()["empty"], "")

    def test_string_constants_keep_their_text_exactly(self) -> None:
        strings = self.strings_of()
        self.assertEqual(strings["plain"], "a")
        self.assertEqual(strings["leading"], " a")
        self.assertEqual(strings["inner"], "x  y")

    def test_a_string_containing_a_comment_marker_is_read_whole(self) -> None:
        self.assertEqual(self.strings_of()["slashes"], "http://host/path // note")

    def test_other_constant_kinds_are_not_strings(self) -> None:
        for comment in ("int 5", "class java/lang/String",
                        "Method java/lang/String.length:()I",
                        "Field name:Ljava/lang/String;", "Stringy"):
            self.assertIsNone(analyzer.Insn(0, "ldc", "#1", comment, "").string(), comment)

    def test_an_empty_string_initializer_is_a_known_empty_default(self) -> None:
        defaults = analyzer.field_initial_values(analyzer.parse_members(STRING_CONSTANTS))
        self.assertEqual(defaults["empty"],
                         {"kind": "string", "value": "", "descriptor": "Ljava/lang/String;"})
        self.assertEqual(defaults["leading"]["value"], " a")


class ExactStringConstants(unittest.TestCase):
    """javap drops trailing whitespace; the class constant pool keeps it."""

    def listing(self, *printed: str) -> str:
        lines = ["  public Probe();", "    Code:"]
        offset = 0
        for n, text in enumerate(printed):
            lines.append(f"{offset:>8}: ldc           #{2 * n + 1}                  // String{text}")
            lines.append(f"{offset + 2:>8}: putfield      #50                 // Field f{n}:Ljava/lang/String;")
            offset += 5
        return "\n".join(lines) + "\n"

    def test_string_constants_are_read_exactly_from_the_constant_pool(self) -> None:
        pool = class_with_strings(b"", b"  ", b"a ", b" a")
        self.assertEqual(analyzer.string_constants(pool), {1: "", 3: "  ", 5: "a ", 7: " a"})

    def test_modified_utf8_is_decoded(self) -> None:
        nul = b"a\xc0\x80 "
        pair = b"\xed\xa0\xbd\xed\xb8\x80"
        pool = class_with_strings(nul, pair)
        self.assertEqual(analyzer.string_constants(pool), {1: "a\x00 ", 3: "\U0001f600"})

    def test_a_truncated_or_foreign_file_has_no_string_constants(self) -> None:
        self.assertEqual(analyzer.string_constants(b""), {})
        self.assertEqual(analyzer.string_constants(b"not a class file"), {})
        self.assertEqual(analyzer.string_constants(class_with_strings(b"abc")[:-2]), {})

    def test_a_pool_with_an_undecodable_entry_has_no_string_constants(self) -> None:
        for raw in (b"\xff", b"\xc0", b"\x80abc "):
            self.assertEqual(analyzer.string_constants(class_with_strings(b"ok ", raw)), {}, raw)

    def test_trailing_spaces_javap_omitted_are_restored(self) -> None:
        text = self.listing("", "", "  a", " b")
        pool = class_with_strings(b"", b"  ", b" a ", b"b")
        exact = analyzer.with_exact_strings(text, analyzer.string_constants(pool))
        insns = analyzer.parse_members(exact)[0].insns
        values = [ins.string() for ins in insns if ins.op == "ldc"]
        self.assertEqual(values, ["", "  ", " a ", "b"])

    def test_a_listing_with_no_constant_to_restore_is_unchanged(self) -> None:
        text = self.listing("", "  a")
        self.assertEqual(analyzer.with_exact_strings(text, {}), text)
        self.assertEqual(analyzer.with_exact_strings(text, {1: "", 3: " a"}), text)

    def test_a_padded_initializer_is_recorded_with_its_padding(self) -> None:
        text = self.listing("  a")
        exact = analyzer.with_exact_strings(text, analyzer.string_constants(class_with_strings(b" a  ")))
        defaults = analyzer.field_initial_values(analyzer.parse_members(exact))
        self.assertEqual(defaults["f0"]["value"], " a  ")


class EscapedAndLineSeparatorStringConstants(unittest.TestCase):
    """javap escapes control characters and quoting, and prints U+2028 and U+2029 raw."""

    def values(self, text: str) -> list[str | None]:
        insns = analyzer.parse_members(text)[0].insns
        return [ins.string() for ins in insns if ins.op == "ldc"]

    def listing(self, *printed: str) -> str:
        return ExactStringConstants.listing(self, *printed)

    def test_javap_escapes_are_undone_without_a_constant_pool(self) -> None:
        text = self.listing(r" x\n", r" t\t", r" back\\slash", r" q\"uote\'s", r" \u0085\u0000z")
        self.assertEqual(self.values(text),
                         ["x\n", "t\t", "back\\slash", "q\"uote's", "\x85\x00z"])

    def test_an_unknown_escape_is_kept_as_written(self) -> None:
        self.assertEqual(self.values(self.listing(r" a\qb\u12")), ["a\\qb\\u12"])

    def test_a_line_separator_inside_a_constant_does_not_end_the_instruction(self) -> None:
        for separator in ("\u2028", "\u2029"):
            text = self.listing(f" a{separator}b", f" a{separator}")
            self.assertEqual(self.values(text), [f"a{separator}b", f"a{separator}"], repr(separator))
            self.assertEqual([i.op for i in analyzer.parse_members(text)[0].insns],
                             ["ldc", "putfield", "ldc", "putfield"])

    def test_exact_constants_replace_javap_text_including_newlines_and_backslashes(self) -> None:
        values = ["x\n", "a\u2028 ", "back\\slash", "\\n", "t\t", " \n ", "\x00"]
        pool = class_with_strings(*(v.encode("utf-8").replace(b"\x00", b"\xc0\x80") for v in values))
        text = self.listing(*([" ignored"] * len(values)))
        exact = analyzer.with_exact_strings(text, analyzer.string_constants(pool))
        self.assertEqual(self.values(exact), values)
        self.assertEqual(len(analyzer.parse_members(exact)[0].insns), 2 * len(values))

    def test_a_constant_with_a_line_separator_is_recorded_exactly_as_an_initializer(self) -> None:
        text = self.listing(" a")
        exact = analyzer.with_exact_strings(text, analyzer.string_constants(class_with_strings("a\u2028 ".encode())))
        defaults = analyzer.field_initial_values(analyzer.parse_members(exact))
        self.assertEqual(defaults["f0"]["value"], "a\u2028 ")


class PaddedSavePrefix(unittest.TestCase):
    """Some releases write `println("key: " + value)`; javap prints the prefix without its space."""

    SAVE = """\
  public void save();
    Code:
       0: aload_2
       1: new           #10                 // class java/lang/StringBuilder
       4: dup
       5: invokespecial #11                 // Method java/lang/StringBuilder."<init>":()V
       8: ldc           #1                  // String chatDelay:
      10: invokevirtual #12                 // Method java/lang/StringBuilder.append:(Ljava/lang/String;)Ljava/lang/StringBuilder;
      13: aload_0
      14: getfield      #13                 // Field chatDelay:D
      17: invokevirtual #14                 // Method java/lang/StringBuilder.append:(D)Ljava/lang/StringBuilder;
      20: invokevirtual #15                 // Method java/lang/StringBuilder.toString:()Ljava/lang/String;
      23: return
"""

    def test_a_save_prefix_with_a_trailing_space_still_names_its_key(self) -> None:
        pool = class_with_strings(b"chatDelay: ")
        text = analyzer.with_exact_strings(self.SAVE, analyzer.string_constants(pool))
        saved = analyzer.legacy_save_blocks(analyzer.parse_members(text)[0])
        self.assertEqual(saved["chatDelay"]["field"], "chatDelay")
        self.assertEqual(saved["chatDelay"]["field_descriptor"], "D")


class SwitchTables(unittest.TestCase):
    def test_switch_table_rows_are_not_instructions(self) -> None:
        insns = members(SWITCH_TABLES)["pick"].insns
        self.assertEqual([i.op for i in insns],
                         ["aload_0", "invokevirtual", "lookupswitch", "aload_0", "ldc_w",
                          "tableswitch", "areturn"])
        self.assertEqual([i.off for i in insns], [0, 1, 4, 32, 33, 36, 46])

    def test_the_switch_instruction_itself_is_kept(self) -> None:
        insns = members(SWITCH_TABLES)["pick"].insns
        switch = next(i for i in insns if i.op == "lookupswitch")
        self.assertEqual(switch.off, 4)

    def test_case_values_never_replace_a_real_instruction_offset(self) -> None:
        member = members(CASE_ROWS_WITH_OFFSET_VALUES)["collide"]
        for offset in (32, 36):
            self.assertEqual(member.insns[member.by_offset[offset]].op,
                             {32: "aconst_null", 36: "areturn"}[offset])
        self.assertEqual(len(member.by_offset), len(member.insns))

    def test_a_string_after_a_switch_is_still_adjacent_to_its_consumer(self) -> None:
        insns = members(SWITCH_TABLES)["pick"].insns
        at = next(i for i, ins in enumerate(insns) if ins.string() == "options.fov.min")
        self.assertEqual(insns[at - 1].op, "aload_0")
        self.assertEqual(insns[at + 1].op, "tableswitch")


if __name__ == "__main__":
    unittest.main()
