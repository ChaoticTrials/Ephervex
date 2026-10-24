"""JAR entry names are untrusted: none may reach javap as an option."""

from __future__ import annotations

import io
import struct
import unittest
from unittest import mock
from zipfile import ZipFile

import analyze_options as analyzer

UNSAFE_NAMES = ["-version", "-J-XX:+PrintFlagsFinal", "-J-Dsome.prop=1", "@argfile", "a b", "a;b", "a\nb"]


def class_with_utf8(text: str) -> bytes:
    raw = text.encode("utf-8")
    return b"\xca\xfe\xba\xbe" + struct.pack(">HHH", 0, 52, 2) + b"\x01" + struct.pack(">H", len(raw)) + raw


def jar_with(names: list[str], payload: bytes) -> ZipFile:
    buffer = io.BytesIO()
    with ZipFile(buffer, "w") as zf:
        for name in names:
            zf.writestr(name, payload)
    return ZipFile(io.BytesIO(buffer.getvalue()))


class JarEntryNameSelection(unittest.TestCase):
    def test_an_entry_that_javap_would_read_as_an_option_is_never_a_root_or_a_class(self) -> None:
        names = [f"{name}.class" for name in UNSAFE_NAMES] + ["net/minecraft/client/Options.class"]
        with jar_with(names, class_with_utf8("options.txt")) as zf:
            roots, constants, all_classes, _ = analyzer.roots_and_constants(zf)
        self.assertEqual(["net/minecraft/client/Options"], roots)
        self.assertEqual({"net/minecraft/client/Options"}, set(constants))
        self.assertEqual({"net/minecraft/client/Options"}, all_classes)

    def test_nested_and_synthetic_class_names_are_kept(self) -> None:
        names = ["a/b/C$1.class", "a/b/C$Inner$2.class", "x.class", "net/minecraft/client/Options.class"]
        with jar_with(names, class_with_utf8("options.txt")) as zf:
            _, _, all_classes, _ = analyzer.roots_and_constants(zf)
        self.assertEqual({name[:-6] for name in names}, all_classes)

    def test_package_and_module_descriptors_are_not_disassembled(self) -> None:
        names = ["a/package-info.class", "module-info.class"]
        with jar_with(names, class_with_utf8("options.txt")) as zf:
            roots, _, all_classes, _ = analyzer.roots_and_constants(zf)
        self.assertEqual([], roots)
        self.assertEqual(set(), all_classes)


class DisassemblerArguments(unittest.TestCase):
    def test_an_unsafe_name_never_starts_a_javap_process(self) -> None:
        for method in ("get", "verbose"):
            for name in UNSAFE_NAMES:
                with self.subTest(method=method, name=name):
                    key = name.replace(".", "/")
                    dis = analyzer.Disassembler(None, {key})
                    with mock.patch.object(analyzer.shutil, "which", return_value="/usr/bin/javap"), \
                            mock.patch.object(analyzer.subprocess, "run") as run:
                        text = getattr(dis, method)(name)
                    self.assertEqual("", text)
                    run.assert_not_called()
                    self.assertEqual([{"class": key, "error": "unsafe class name"}], dis.errors)

    def test_a_safe_name_is_passed_to_javap_as_a_dotted_class_name(self) -> None:
        completed = mock.Mock(stdout="disassembly", stderr="", returncode=0)
        for method in ("get", "verbose"):
            with self.subTest(method=method):
                dis = analyzer.Disassembler("client.jar", {"a/b/C$1"})
                with mock.patch.object(analyzer.shutil, "which", return_value="/usr/bin/javap"), \
                        mock.patch.object(analyzer.subprocess, "run", return_value=completed) as run:
                    text = getattr(dis, method)("a/b/C$1")
                self.assertEqual("disassembly", text)
                self.assertEqual("a.b.C$1", run.call_args.args[0][-1])
                self.assertEqual([], dis.errors)

    def test_a_name_absent_from_the_jar_is_still_empty_without_an_error(self) -> None:
        dis = analyzer.Disassembler(None, set())
        self.assertEqual("", dis.get("-version"))
        self.assertEqual([], dis.errors)


if __name__ == "__main__":
    unittest.main()
