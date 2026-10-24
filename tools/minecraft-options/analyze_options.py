#!/usr/bin/env python3
"""Deterministic, static Minecraft options.txt research analyzer.

Inputs are treated strictly as ZIP/class-file data. Minecraft is never loaded or
executed.  javap is used only as a class-file disassembler for targeted roots and
for the small set of helper classes those roots directly reference.

The analyzer recognises three distinct options.txt persistence eras and extracts
types, defaults, numeric domains, storage transforms, enum domains and key
bindings from each of them.  Everything it reports carries provenance; anything
it cannot prove stays `unknown` rather than being guessed.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import struct
import subprocess
import sys
from collections import Counter, defaultdict
from pathlib import Path
from typing import Any
from zipfile import BadZipFile, ZipFile

from release_identity import RELEASE_PATTERN, is_release, version_key

TOOL_VERSION = "2.0.0"
PRUNE = {".git", ".gradle", "build", "cache", "caches", "node_modules", "minecraft-options-research"}
FILENAME_VERSION_RE = re.compile(rf"(?:^|[-_])(?:mc[-_])?({RELEASE_PATTERN})(?:\.jar)?$", re.I)
# Some 1.14-era release JARs name themselves "<release> / <build id>".
BUILD_SUFFIXED_ID_RE = re.compile(r"^(\S+) / [0-9a-fA-F]+$")
ANALYSIS_SCHEMA_VERSION = 1

CONF_ORDER = {"unknown": 0, "low": 1, "medium": 2, "high": 3}

# A JAR entry name reaches javap as a bare command-line argument, and javap
# rejects `--`, so only names that can never be read as an option or an @argfile
# are ever passed on. This excludes every `-`, which also rules out
# `package-info` and `module-info`; neither carries option persistence code.
SAFE_CLASS_NAME_RE = re.compile(r"[A-Za-z0-9_$./]+")


def is_safe_class_name(name: str) -> bool:
    return SAFE_CLASS_NAME_RE.fullmatch(name) is not None


def weakest(*values: str) -> str:
    return min(values, key=lambda c: CONF_ORDER.get(c, 0))


def stable_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    text = json.dumps(value, indent=2, ensure_ascii=False, sort_keys=True) + "\n"
    path.write_text(text, encoding="utf-8")


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


class InputError(Exception):
    """An analyzer input that cannot be used, with a message worth printing."""


def jar_path_in_root(path: Path, root: Path) -> str:
    """The POSIX path of `path` below `root`, the only form an inventory records."""
    try:
        return path.relative_to(root).as_posix()
    except ValueError:
        raise InputError(f"{path} is outside --root {root}; pass --root a directory that contains it") from None


def discover(root: Path, output_dir: Path) -> list[Path]:
    """Every JAR below `root` except under the output directory.

    A JAR symlink keeps its place below `root`, so its inventory path stays relative to it.
    """
    found: list[Path] = []
    root = root.resolve()
    output_real = output_dir.resolve()
    for base, dirs, files in os.walk(root):
        base_path = Path(base)
        dirs[:] = sorted(d for d in dirs if d not in PRUNE and (base_path / d).resolve() != output_real)
        for name in sorted(files):
            if name.lower().endswith(".jar"):
                found.append(base_path / name)
    return sorted(found, key=lambda p: p.as_posix())


def cp_utf8(data: bytes) -> list[str]:
    """Extract CONSTANT_Utf8 entries without parsing/executing bytecode."""
    if len(data) < 10 or data[:4] != b"\xca\xfe\xba\xbe":
        return []
    count = struct.unpack_from(">H", data, 8)[0]
    pos, index, out = 10, 1, []
    widths = {3: 4, 4: 4, 5: 8, 6: 8, 7: 2, 8: 2, 9: 4, 10: 4, 11: 4,
              12: 4, 15: 3, 16: 2, 17: 4, 18: 4, 19: 2, 20: 2}
    try:
        while index < count:
            tag = data[pos]
            pos += 1
            if tag == 1:
                n = struct.unpack_from(">H", data, pos)[0]
                pos += 2
                out.append(data[pos:pos + n].decode("utf-8", "replace"))
                pos += n
            elif tag in widths:
                pos += widths[tag]
                if tag in (5, 6):
                    index += 1
            else:
                return out
            index += 1
    except (IndexError, struct.error):
        pass
    return out


def modified_utf8(raw: bytes) -> str:
    """Decode a class-file Utf8 entry: NUL is C0 80 and supplementary characters are surrogate pairs."""
    text = raw.replace(b"\xc0\x80", b"\x00").decode("utf-8", "surrogatepass")
    return text.encode("utf-16", "surrogatepass").decode("utf-16", "replace")


def string_constants(data: bytes) -> dict[int, str]:
    """Map each CONSTANT_String pool index to its exact value, read straight from the class bytes.

    A pool that does not parse completely yields nothing rather than a partial map.
    """
    if len(data) < 10 or data[:4] != b"\xca\xfe\xba\xbe":
        return {}
    count = struct.unpack_from(">H", data, 8)[0]
    pos, index = 10, 1
    utf8: dict[int, str] = {}
    refs: dict[int, int] = {}
    widths = {3: 4, 4: 4, 5: 8, 6: 8, 7: 2, 9: 4, 10: 4, 11: 4,
              12: 4, 15: 3, 16: 2, 17: 4, 18: 4, 19: 2, 20: 2}
    try:
        while index < count:
            tag = data[pos]
            pos += 1
            if tag == 1:
                n = struct.unpack_from(">H", data, pos)[0]
                pos += 2
                if pos + n > len(data):
                    return {}
                utf8[index] = modified_utf8(data[pos:pos + n])
                pos += n
            elif tag == 8:
                refs[index] = struct.unpack_from(">H", data, pos)[0]
                pos += 2
            elif tag in widths:
                pos += widths[tag]
                if tag in (5, 6):
                    index += 1
            else:
                return {}
            index += 1
    except (IndexError, struct.error, UnicodeDecodeError):
        return {}
    return {i: utf8[u] for i, u in refs.items() if u in utf8}


def supported(v: str | None) -> bool:
    return is_release(v) and version_key(v) >= version_key("1.12.2")


def jar_id(version: str | None, digest: str) -> str:
    base = version if version else "unresolved"
    return re.sub(r"[^a-zA-Z0-9._-]+", "_", base) + "-" + digest[:12]


def detect_version(path: Path, zf: ZipFile) -> dict[str, Any]:
    evidence: list[dict[str, Any]] = []
    version = None
    stable = None
    identity_conflict = False
    if "version.json" in zf.namelist():
        try:
            meta = json.loads(zf.read("version.json"))
            # `release_target` is the release a pre-release build is aimed at, never its own
            # identity. A release names itself identically in `name` and `id`.
            name, build_id = meta.get("name"), meta.get("id")
            if isinstance(build_id, str) and (suffixed := BUILD_SUFFIXED_ID_RE.fullmatch(build_id)):
                build_id = suffixed.group(1)
            named = {x for x in (name, build_id) if isinstance(x, str) and x}
            if len(named) > 1:
                # Disagreeing identifiers never name a release; keep a non-release spelling if any.
                candidate = next((x for x in (meta.get("id"), name) if isinstance(x, str) and x and not is_release(x)), None)
                identity_conflict = True
                evidence.append({"kind": "resource_error", "location": "version.json",
                                 "detail": "name and id name different builds", "confidence": "unknown"})
            else:
                candidate = next(iter(named), None)
            if isinstance(candidate, str):
                version = candidate
                stable = is_release(candidate)
                evidence.append({"kind": "resource", "location": "version.json", "value": candidate,
                                 "confidence": "high", "stable": stable})
        except Exception as exc:
            evidence.append({"kind": "resource_error", "location": "version.json", "detail": str(exc),
                             "confidence": "unknown"})
    m = FILENAME_VERSION_RE.search(path.name)
    filename_version = m.group(1) if m else None
    if version is None and filename_version and not identity_conflict:
        known = "net/minecraft/realms/RealmsSharedConstants.class"
        if known in zf.namelist() and filename_version in cp_utf8(zf.read(known)):
            version = filename_version
            stable = is_release(version)
            evidence.append({"kind": "class_constant", "location": known, "value": version,
                             "description": "Exact filename token is independently embedded in RealmsSharedConstants",
                             "confidence": "high"})
        else:
            evidence.append({"kind": "filename", "location": path.name, "value": filename_version,
                             "confidence": "low"})
    if version and filename_version and filename_version == version:
        evidence.append({"kind": "filename_corroboration", "location": path.name, "value": filename_version,
                         "confidence": "medium"})
    return {"version": version,
            "stable_release": stable if stable is not None else is_release(version),
            "confidence": "high" if version else "unknown", "evidence": evidence}


# ---------------------------------------------------------------------------
# javap access with per-JAR caching
# ---------------------------------------------------------------------------


class Disassembler:
    """Caching javap front-end for a single JAR."""

    def __init__(self, jar: Path, class_names: set[str]) -> None:
        self.jar = jar
        self.available = class_names
        self.cache: dict[str, str] = {}
        self.verbose_cache: dict[str, str] = {}
        self.errors: list[dict[str, str]] = []
        self.calls = 0

    def verbose(self, class_name: str) -> str:
        """Disassembly including the constant pool and bootstrap methods.

        Only the bootstrap table is wanted, and only javap's verbose form
        prints it. An `invokedynamic` instruction names nothing but an index
        into that table, so it is the one place the method a lambda actually
        calls is written down.
        """
        key = class_name.replace(".", "/")
        if key in self.verbose_cache:
            return self.verbose_cache[key]
        if key not in self.available:
            self.verbose_cache[key] = ""
            return ""
        if not is_safe_class_name(key):
            self.errors.append({"class": key, "error": "unsafe class name"})
            self.verbose_cache[key] = ""
            return ""
        binary = shutil.which("javap")
        if not binary:
            self.errors.append({"class": key, "error": "javap unavailable"})
            self.verbose_cache[key] = ""
            return ""
        try:
            self.calls += 1
            p = subprocess.run([binary, "-classpath", str(self.jar), "-p", "-v",
                                key.replace("/", ".")],
                               check=False, capture_output=True, text=True, timeout=120)
            text = p.stdout
            if p.returncode:
                self.errors.append({"class": key,
                                    "error": p.stderr.strip() or f"javap exit {p.returncode}"})
        except (OSError, subprocess.TimeoutExpired) as exc:
            self.errors.append({"class": key, "error": str(exc)})
            text = ""
        self.verbose_cache[key] = text
        return text

    def get(self, class_name: str) -> str:
        key = class_name.replace(".", "/")
        if key in self.cache:
            return self.cache[key]
        if key not in self.available:
            self.cache[key] = ""
            return ""
        if not is_safe_class_name(key):
            self.errors.append({"class": key, "error": "unsafe class name"})
            self.cache[key] = ""
            return ""
        binary = shutil.which("javap")
        if not binary:
            self.errors.append({"class": key, "error": "javap unavailable"})
            self.cache[key] = ""
            return ""
        try:
            self.calls += 1
            p = subprocess.run([binary, "-classpath", str(self.jar), "-p", "-c", "-constants",
                                key.replace("/", ".")],
                               check=False, capture_output=True, text=True, timeout=120)
            text = p.stdout
            if p.returncode:
                self.errors.append({"class": key, "error": p.stderr.strip() or f"javap exit {p.returncode}"})
            else:
                text = with_trailing_spaces(text, self.string_values(key))
        except (OSError, subprocess.TimeoutExpired) as exc:
            self.errors.append({"class": key, "error": str(exc)})
            text = ""
        self.cache[key] = text
        return text

    def string_values(self, key: str) -> dict[int, str]:
        try:
            with ZipFile(self.jar) as zf:
                return string_constants(zf.read(f"{key}.class"))
        except (OSError, KeyError, BadZipFile):
            return {}


# ---------------------------------------------------------------------------
# Disassembly parsing primitives
# ---------------------------------------------------------------------------

LDC_STRING_RE = re.compile(r"^(\s+\d+:\s+ldc(?:_w)?\s+#(\d+)\s+// String)(.*)$")


def with_trailing_spaces(text: str, constants: dict[int, str]) -> str:
    """Put back the trailing spaces javap leaves off a printed string constant.

    javap never prints trailing whitespace, so `"a "` reads as `a` and `"  "`
    as an empty string. The class constant pool has the exact value.
    """
    lines = text.split("\n")
    for i, line in enumerate(lines):
        m = LDC_STRING_RE.match(line)
        value = constants.get(int(m.group(2))) if m else None
        if value is None:
            continue
        spaces = len(value) - len(value.rstrip(" "))
        if spaces:
            lines[i] = line + (" " if not m.group(3) else "") + " " * spaces
    return "\n".join(lines)


MEMBER_RE = re.compile(r"^  (?:\S.*\(.*\)|static \{\})")
# Only a mnemonic starts an instruction. The `value: target` rows inside a
# switch table also begin with an offset-like number, and are not instructions.
CODE_RE = re.compile(r"^\s{4,}(\d+):\s+([A-Za-z]\S*)\s*(.*)$")
COMMENT_RE = re.compile(r"//\s*(.*)$")

M_FIELD = re.compile(r'^Field (?:"?([\w/$\[\];]+)"?\.)?([\w$]+):(\S+)$')
M_METHOD = re.compile(r'^(?:Interface)?Method (?:"?([\w/$\[\];]+)"?\.)?("?[\w$<>]+"?):(\S+)$')
# javap prints an empty string constant as a bare `String`.
M_STRING = re.compile(r"^String(?: (.*))?$")
M_CLASS = re.compile(r'^class "?([\w/$\[\];]+)"?$')
M_INT = re.compile(r"^int (-?\d+)$")
M_LONG = re.compile(r"^long (-?\d+)l$")
# javap prints a finite float or double in decimal form, or in scientific form
# (`1.0E-5f`) below 1e-3 and from 1e7 up.
M_FLOAT = re.compile(r"^(?:float|double) (-?(?:\d+\.?\d*|\.\d+)(?:[eE][+-]?\d+)?)[fd]?$")


class Insn:
    __slots__ = ("off", "op", "arg", "comment", "raw", "join", "conditional")

    def __init__(self, off: int, op: str, arg: str, comment: str, raw: str) -> None:
        self.off, self.op, self.arg, self.comment, self.raw = off, op, arg, comment, raw
        # Set by `mark_control_flow`: a jump lands here, or a jump skips or repeats it.
        self.join = self.conditional = False

    def field(self) -> tuple[str | None, str, str] | None:
        m = M_FIELD.match(self.comment)
        return (m.group(1), m.group(2), m.group(3)) if m else None

    def method(self) -> tuple[str | None, str, str] | None:
        m = M_METHOD.match(self.comment)
        return (m.group(1), m.group(2).strip('"'), m.group(3)) if m else None

    def string(self) -> str | None:
        m = M_STRING.match(self.comment)
        return (m.group(1) or "") if m else None

    def klass(self) -> str | None:
        m = M_CLASS.match(self.comment)
        return m.group(1) if m else None


JUMP_OPS = frozenset({
    "goto", "goto_w", "jsr", "jsr_w", "ifnull", "ifnonnull",
    "ifeq", "ifne", "iflt", "ifge", "ifgt", "ifle",
    "if_icmpeq", "if_icmpne", "if_icmplt", "if_icmpge", "if_icmpgt", "if_icmple",
    "if_acmpeq", "if_acmpne"})
SWITCH_OPS = frozenset({"tableswitch", "lookupswitch"})
JUMP_TARGET_RE = re.compile(r"\d+")


def mark_control_flow(insns: list[Insn]) -> None:
    """Mark instructions a jump lands on (`join`) and ones a jump skips or repeats (`conditional`).

    The parser drops a switch's table rows, so its targets are unknown and everything after a
    switch counts as a jump target.
    """
    index = {ins.off: i for i, ins in enumerate(insns)}
    span = [0] * (len(insns) + 1)
    opaque = len(insns)
    for i, ins in enumerate(insns):
        if ins.op in SWITCH_OPS:
            opaque = min(opaque, i)
            continue
        if ins.op not in JUMP_OPS:
            continue
        target = index.get(int(ins.arg)) if JUMP_TARGET_RE.fullmatch(ins.arg.strip()) else None
        if target is None:
            opaque = min(opaque, i)
            continue
        insns[target].join = True
        first, last = (i + 1, target - 1) if target > i else (target, i)
        if first <= last:
            span[first] += 1
            span[last + 1] -= 1
    running = 0
    for i, ins in enumerate(insns):
        running += span[i]
        ins.conditional = running > 0
        if i > opaque:
            ins.join = True


def runs_unconditionally(insns: list[Insn]) -> bool:
    """True when nothing jumps to, over or around these consecutive instructions."""
    return not any(ins.join or ins.conditional for ins in insns)


class Member:
    __slots__ = ("signature", "name", "insns", "by_offset")

    def __init__(self, signature: str, insns: list[Insn]) -> None:
        self.signature = signature
        head = signature.split("(")[0].strip()
        self.name = head.split()[-1] if head else signature
        self.insns = insns
        self.by_offset = {ins.off: i for i, ins in enumerate(insns)}
        mark_control_flow(insns)


def parse_members(text: str) -> list[Member]:
    members: list[Member] = []
    signature: str | None = None
    insns: list[Insn] = []
    for line in text.splitlines():
        if MEMBER_RE.match(line):
            if signature is not None and insns:
                members.append(Member(signature, insns))
            signature, insns = line.strip().rstrip(";"), []
            continue
        m = CODE_RE.match(line)
        if not m:
            continue
        rest = m.group(3)
        cm = COMMENT_RE.search(rest)
        comment = cm.group(1) if cm else ""
        arg = (rest[:cm.start()] if cm else rest).strip()
        insns.append(Insn(int(m.group(1)), m.group(2), arg, comment, line.strip()))
    if signature is not None and insns:
        members.append(Member(signature, insns))
    return members


def split_descriptor(desc: str) -> tuple[list[str], str]:
    """Split a JVM method descriptor into its parameter list and return type."""
    if "(" not in desc or ")" not in desc:
        return [], ""
    inner = desc[desc.index("(") + 1:desc.index(")")]
    ret = desc[desc.index(")") + 1:]
    params: list[str] = []
    i = 0
    while i < len(inner):
        start = i
        while i < len(inner) and inner[i] == "[":
            i += 1
        if i >= len(inner):
            break
        if inner[i] == "L":
            end = inner.find(";", i)
            if end < 0:
                break
            i = end + 1
        else:
            i += 1
        params.append(inner[start:i])
    return params, ret


def const_int(ins: Insn) -> int | None:
    op = ins.op
    if op == "iconst_m1":
        return -1
    if op.startswith("iconst_"):
        return int(op[-1])
    if op in ("bipush", "sipush"):
        try:
            return int(ins.arg.split()[0])
        except (ValueError, IndexError):
            return None
    if op.startswith("ldc"):
        m = M_INT.match(ins.comment)
        if m:
            return int(m.group(1))
    return None


# A long beyond this magnitude has no exact double, so it is read as unresolved
# rather than rounded.
EXACT_DOUBLE_INTEGER = 2 ** 53


def const_long(ins: Insn) -> int | None:
    if ins.op in ("lconst_0", "lconst_1"):
        return int(ins.op[-1])
    if ins.op == "ldc2_w":
        m = M_LONG.match(ins.comment)
        if m:
            return int(m.group(1))
    return None


def const_num(ins: Insn) -> float | None:
    iv = const_int(ins)
    if iv is not None:
        return float(iv)
    lv = const_long(ins)
    if lv is not None:
        return float(lv) if abs(lv) <= EXACT_DOUBLE_INTEGER else None
    if ins.op in ("fconst_0", "dconst_0"):
        return 0.0
    if ins.op in ("fconst_1", "dconst_1"):
        return 1.0
    if ins.op == "fconst_2":
        return 2.0
    if ins.op.startswith("ldc"):
        m = M_FLOAT.match(ins.comment)
        if m:
            return float(m.group(1))
    return None


def is_constant_load(ins: Insn) -> bool:
    """Whether an instruction pushes a numeric constant, readable or not.

    `const_num` is None both for a non-constant and for a constant it cannot
    read (`NaN`, an inexact long); only the second makes a transform unresolved.
    """
    if ins.op in ("bipush", "sipush") or re.fullmatch(r"[ilfd]const_(?:m1|\d)", ins.op):
        return True
    return ins.op in ("ldc", "ldc_w", "ldc2_w") and re.match(r"(?:int|long|float|double) ", ins.comment) is not None


ARITH = {"fmul": "*", "dmul": "*", "imul": "*", "fadd": "+", "dadd": "+", "iadd": "+",
         "fsub": "-", "dsub": "-", "isub": "-", "fdiv": "/", "ddiv": "/", "idiv": "/"}


def arithmetic_chain(insns: list[Insn]) -> list[dict[str, Any]] | None:
    """Collect `<const> <arith>` pairs, in evaluation order, from a slice.

    Operands that are not constants are skipped, but a constant this analyzer
    cannot read makes the whole chain None: a chain missing a step is a
    transform written through the wrong arithmetic.
    """
    ops: list[dict[str, Any]] = []
    for i, ins in enumerate(insns):
        if ins.op in ARITH and i > 0:
            value = const_num(insns[i - 1])
            if value is not None:
                ops.append({"op": ARITH[ins.op], "operand": value})
            elif is_constant_load(insns[i - 1]):
                return None
    return ops


# Conversions that carry a constant to a wider type without changing its value.
WIDENING = {"i2d", "i2f", "i2l", "f2d"}

# Arithmetic with no `<const> <op>` shape to read: a result left unread here
# is a transform the chain would silently drop.
UNREAD_ARITH = ({f"{t}{o}" for t in "ilfd" for o in ("rem", "neg")}
                | {f"l{o}" for o in ("add", "sub", "mul", "div")}
                | {f"{t}{o}" for t in "il" for o in ("shl", "shr", "ushr", "and", "or", "xor")})


UNREAD_TRANSFORM_NOTE = ("the value is transformed on its way to and from the file through "
                         "arithmetic this analyzer could not read")


def exact_arithmetic(insns: list[Insn]) -> list[dict[str, Any]] | None:
    """The constant arithmetic of one expression, or None when any of it is
    not a plain `<value> <const> <op>` step.

    A left constant (`1.0 - x`), a nested sub-expression, a non-constant
    operand and an opcode the chain has no step for all read as "unresolved",
    never as a shorter chain: a partly read transform is how a value gets
    written through the wrong arithmetic. A constant may reach its operator
    through a widening conversion only.
    """
    ops: list[dict[str, Any]] = []
    for i, ins in enumerate(insns):
        if ins.op in UNREAD_ARITH:
            return None
        if ins.op not in ARITH:
            continue
        j = i - 1
        while j >= 0 and insns[j].op in WIDENING:
            j -= 1
        value = const_num(insns[j]) if j >= 0 else None
        if value is None:
            return None
        ops.append({"op": ARITH[ins.op], "operand": value})
    return ops


def operand_text(value: float) -> str:
    """The shortest text that reads back as exactly `value`, without a
    trailing `.0` on a whole number."""
    text = repr(float(value))
    return text[:-2] if text.endswith(".0") else text


def formula(ops: list[dict[str, Any]], var: str = "x") -> str | None:
    if not ops:
        return None
    expr = var
    for step in ops:
        expr = f"({expr} {step['op']} {operand_text(step['operand'])})"
    return expr



# ---------------------------------------------------------------------------
# Lambda bodies behind `invokedynamic`
# ---------------------------------------------------------------------------

M_INDY = re.compile(r"^InvokeDynamic #(\d+):")
BSM_ENTRY = re.compile(r"^\s{2}(\d+): #\d+ REF_invoke\w+ ")
BSM_TARGET = re.compile(r"^\s+#\d+ REF_invoke\w+ ([\w/$]+)\.([\w$<>]+):(\(\S*\)\S+)$")


def bootstrap_targets(verbose_text: str) -> dict[int, tuple[str, str, str]]:
    """Bootstrap index -> the method a lambda at that index actually calls.

    javap's verbose form lists each bootstrap method and its arguments; for a
    `LambdaMetafactory` call site the second argument is a method handle naming
    the synthetic method the lambda compiled into. Reading it is what makes a
    lambda's arithmetic recoverable, rather than the guesswork of matching a
    descriptor against every method in the class.
    """
    targets: dict[int, tuple[str, str, str]] = {}
    index: int | None = None
    in_table = False
    for line in verbose_text.splitlines():
        if line.startswith("BootstrapMethods:"):
            in_table = True
            continue
        if not in_table:
            continue
        if line and not line.startswith(" "):
            break
        entry = BSM_ENTRY.match(line)
        if entry:
            index = int(entry.group(1))
            continue
        target = BSM_TARGET.match(line)
        if target and index is not None and index not in targets:
            targets[index] = (target.group(1), target.group(2), target.group(3))
    return targets


# Calls that only convert between a primitive and its box; they leave the
# arithmetic around them unchanged.
BOXING_CALLS = {(box, name) for box in ("java/lang/Double", "java/lang/Integer", "java/lang/Float")
                for name in ("valueOf", "doubleValue", "intValue", "floatValue")}


def is_plain_arithmetic(member: Member) -> bool:
    """Whether a lambda body is straight-line arithmetic and boxing only.

    `arithmetic_chain` reads the constant operands of arithmetic and skips
    everything else, so a body that calls `Math.pow` or branches would read as
    the linear remainder of what it computes.
    """
    for ins in member.insns:
        if ins.op.startswith("if") or ins.op in ("goto", "goto_w", "tableswitch", "lookupswitch"):
            return False
        if ins.op.startswith("invoke"):
            ref = ins.method()
            if not ref or (ref[0], ref[1]) not in BOXING_CALLS:
                return False
    return True


def lambda_arithmetic(members: list[Member], owner: str, name: str,
                      descriptor: str, plain: bool = False) -> list[dict[str, Any]] | None:
    """The constant arithmetic one synthetic lambda performs, if it is that
    simple.

    Obfuscated releases reuse short method names, so the descriptor decides
    which overload this is; a name that matches more than one signature is
    left unresolved rather than guessed at.
    """
    params, ret = split_descriptor(descriptor)
    wanted = [java_type(p) for p in params]
    want_ret = java_type(ret)
    matches = [m for m in members if m.name == name and member_types(m) == (wanted, want_ret)]
    if len(matches) != 1 or (plain and not is_plain_arithmetic(matches[0])):
        return None
    return arithmetic_chain(matches[0].insns)


def java_type(descriptor: str) -> str:
    """One JVM type descriptor as javap prints it in a signature."""
    depth = 0
    while descriptor.startswith("["):
        depth += 1
        descriptor = descriptor[1:]
    primitives = {"I": "int", "J": "long", "D": "double", "F": "float", "Z": "boolean",
                  "B": "byte", "C": "char", "S": "short", "V": "void"}
    if descriptor.startswith("L"):
        name = descriptor[1:-1].replace("/", ".").replace("$", ".")
    else:
        name = primitives.get(descriptor, descriptor)
    return name + "[]" * depth


def member_types(member: Member) -> tuple[list[str], str]:
    """One parsed member's parameter and return types, as javap printed them."""
    sig = SIGNATURE_RE.match(member.signature.rstrip(";").strip())
    if not sig:
        return ([], "")
    head = sig.group("head").split()
    ret = head[-1] if head else ""
    params = [p.strip().split()[0] for p in sig.group("params").split(",") if p.strip()]
    return ([p.replace("$", ".") for p in params], ret.replace("$", "."))

def invert_ops(ops: list[dict[str, Any]]) -> list[dict[str, Any]]:
    inverse = {"*": "/", "/": "*", "+": "-", "-": "+"}
    return [{"op": inverse[s["op"]], "operand": s["operand"]} for s in reversed(ops) if s["op"] in inverse]


# ---------------------------------------------------------------------------
# Key-code tables
# ---------------------------------------------------------------------------

GLFW = {32: "space", 39: "apostrophe", 44: "comma", 45: "minus", 46: "period", 47: "slash",
        48: "0", 49: "1", 50: "2", 51: "3", 52: "4", 53: "5", 54: "6", 55: "7", 56: "8", 57: "9",
        59: "semicolon", 61: "equal",
        65: "a", 66: "b", 67: "c", 68: "d", 69: "e", 70: "f", 71: "g", 72: "h", 73: "i", 74: "j",
        75: "k", 76: "l", 77: "m", 78: "n", 79: "o", 80: "p", 81: "q", 82: "r", 83: "s", 84: "t",
        85: "u", 86: "v", 87: "w", 88: "x", 89: "y", 90: "z",
        91: "left.bracket", 92: "backslash", 93: "right.bracket", 96: "grave.accent",
        256: "escape", 257: "enter", 258: "tab", 259: "backspace", 260: "insert", 261: "delete",
        262: "right", 263: "left", 264: "down", 265: "up", 266: "page.up", 267: "page.down",
        268: "home", 269: "end", 280: "caps.lock", 281: "scroll.lock", 282: "num.lock",
        283: "print.screen", 284: "pause",
        290: "f1", 291: "f2", 292: "f3", 293: "f4", 294: "f5", 295: "f6", 296: "f7", 297: "f8",
        298: "f9", 299: "f10", 300: "f11", 301: "f12",
        320: "keypad.0", 321: "keypad.1", 322: "keypad.2", 323: "keypad.3", 324: "keypad.4",
        325: "keypad.5", 326: "keypad.6", 327: "keypad.7", 328: "keypad.8", 329: "keypad.9",
        330: "keypad.decimal", 331: "keypad.divide", 332: "keypad.multiply", 333: "keypad.subtract",
        334: "keypad.add", 335: "keypad.enter", 336: "keypad.equal",
        340: "left.shift", 341: "left.control", 342: "left.alt", 343: "left.win",
        344: "right.shift", 345: "right.control", 346: "right.alt", 347: "right.win"}

# org.lwjgl.input.Keyboard (LWJGL 2) scan codes.  LWJGL is NOT bundled in any
# client JAR under the analysis root, so this table is an external constant
# reference, recorded as such.  Every entry used by a pre-1.13 default is
# independently cross-checked below against the 1.12.2 -> 1.13 default pairs.
LWJGL2_KEYS = {1: "escape", 2: "1", 3: "2", 4: "3", 5: "4", 6: "5", 7: "6", 8: "7", 9: "8",
               10: "9", 11: "0", 12: "minus", 13: "equal", 14: "backspace", 15: "tab",
               16: "q", 17: "w", 18: "e", 19: "r", 20: "t", 21: "y", 22: "u", 23: "i", 24: "o",
               25: "p", 26: "left.bracket", 27: "right.bracket", 28: "enter", 29: "left.control",
               30: "a", 31: "s", 32: "d", 33: "f", 34: "g", 35: "h", 36: "j", 37: "k", 38: "l",
               39: "semicolon", 40: "apostrophe", 41: "grave.accent", 42: "left.shift",
               43: "backslash", 44: "z", 45: "x", 46: "c", 47: "v", 48: "b", 49: "n", 50: "m",
               51: "comma", 52: "period", 53: "slash", 54: "right.shift", 55: "keypad.multiply",
               56: "left.alt", 57: "space", 58: "caps.lock", 59: "f1", 60: "f2", 61: "f3", 62: "f4",
               63: "f5", 64: "f6", 65: "f7", 66: "f8", 67: "f9", 68: "f10", 69: "num.lock",
               70: "scroll.lock", 71: "keypad.7", 72: "keypad.8", 73: "keypad.9", 74: "keypad.subtract",
               75: "keypad.4", 76: "keypad.5", 77: "keypad.6", 78: "keypad.add", 79: "keypad.1",
               80: "keypad.2", 81: "keypad.3", 82: "keypad.0", 83: "keypad.decimal", 87: "f11",
               88: "f12", 156: "keypad.enter", 157: "right.control", 181: "keypad.divide",
               184: "right.alt", 197: "pause", 199: "home", 200: "up", 201: "page.up", 203: "left",
               205: "right", 207: "end", 208: "down", 209: "page.down", 210: "insert", 211: "delete",
               219: "left.win", 220: "right.win"}

MOUSE_NAMES = {0: "left", 1: "right", 2: "middle"}


def glfw_name(code: int) -> str | None:
    return GLFW.get(code)


# ---------------------------------------------------------------------------
# Enum extraction
# ---------------------------------------------------------------------------

TRANSLATION_HINT = re.compile(r"^(options|gui|menu|selectWorld|createWorld|key|narrator|soundCategory)\.")


# Codec fields that never describe how a value is written to options.txt.
NON_PERSISTED_CODECS = ("StreamCodec;", "MapCodec;")


class _Construction:
    """One `new X(...)` whose arguments are still being pushed."""

    __slots__ = ("owner", "ints", "strings", "nested_strings")

    def __init__(self, owner: str) -> None:
        self.owner = owner
        self.ints: list[int] = []
        self.strings: list[str] = []
        # Literals of constructions nested in the arguments, such as the key of a
        # `new TextComponentTranslation(key)`. They never name or number a constant.
        self.nested_strings: list[str] = []


def declared_constant_fields(members: list[Member], clinit: Member,
                             class_name: str) -> list[str] | None:
    """Constant fields in the order the enum's `$VALUES` array lists them.

    The array is built by a synthetic static method, or inline in `<clinit>` by
    older compilers; its field is found by array type because obfuscated
    releases rename `$VALUES`. Other arrays of the enum, such as an id lookup
    table, list no constants and are skipped. None when no array lists any.
    """
    own = f"L{class_name};"
    for store, ins in enumerate(clinit.insns):
        fld = ins.field()
        if ins.op != "putstatic" or not fld or fld[2] != f"[{own}" or store == 0:
            continue
        previous = clinit.insns[store - 1]
        ref = previous.method()
        if previous.op == "invokestatic" and ref and ref[2] == f"()[{own}":
            builder = next((m for m in members if m.name == ref[1] and m.signature.endswith("()")), None)
            insns = builder.insns if builder is not None else []
        else:
            start = max((j for j in range(store) if clinit.insns[j].op == "anewarray"), default=store)
            insns = clinit.insns[start:store]
        fields = [f[1] for i in insns for f in [i.field()] if i.op == "getstatic" and f and f[2] == own]
        if fields:
            return fields
    return None


def static_initializer(members: list[Member]) -> Member | None:
    """The class's static initializer, whichever way javap names it."""
    clinit = next((m for m in members if m.name == "<clinit>" or '"<clinit>"' in m.signature
                   or m.signature.strip() == "static {}"), None)
    if clinit is None:
        clinit = next((m for m in members if m.signature.strip().startswith("static {")), None)
    return clinit


def extract_enum(dis: Disassembler, class_name: str) -> dict[str, Any] | None:
    """Recover enum constants, ids, serialized tokens and the persisted codec.

    A constant is the `new` of the enum or of its anonymous body class, closed by
    the `<init>` of that same class: constructions nested in its arguments close
    on their own. The list is returned only when it matches `$VALUES`, because a
    partial domain presented as complete is worse than none.
    """
    text = dis.get(class_name)
    if not text:
        return None
    members = parse_members(text)
    clinit = static_initializer(members)
    if clinit is None:
        return None
    own = f"L{class_name};"
    constants: list[dict[str, Any]] = []
    stack: list[_Construction] = []
    awaiting_field: dict[str, Any] | None = None
    for position, ins in enumerate(clinit.insns):
        if awaiting_field is not None and ins.op == "putstatic":
            fld = ins.field()
            if fld:
                awaiting_field["field"] = fld[1]
            awaiting_field = None
        if ins.op == "new":
            owner = ins.klass()
            if owner:
                stack.append(_Construction(owner))
            continue
        if not stack:
            continue
        top = stack[-1]
        s = ins.string()
        if s is not None:
            top.strings.append(s)
            continue
        iv = const_int(ins)
        if iv is not None:
            top.ints.append(iv)
            continue
        ref = ins.method()
        if not (ins.op == "invokespecial" and ref and ref[1] == "<init>"):
            continue
        owner = ref[0] or class_name
        depth = next((i for i in range(len(stack) - 1, -1, -1) if stack[i].owner == owner), None)
        if depth is None:
            continue
        del stack[depth + 1:]
        built = stack.pop()
        if owner != class_name and not owner.startswith(class_name + "$"):
            if stack:
                stack[-1].nested_strings += built.strings + built.nested_strings
            continue
        if not built.strings:
            continue
        if owner != class_name:
            # A body class is a constant only if its instance is stored in a
            # field of the enum's own type.
            following = clinit.insns[position + 1] if position + 1 < len(clinit.insns) else None
            stored = following.field() if following is not None and following.op == "putstatic" else None
            if not stored or stored[2] != own:
                continue
        name = built.strings[0]
        ordinal = built.ints[0] if built.ints else None
        extra_ints = built.ints[1:]
        extra_strings = built.strings[1:]
        token = next((x for x in extra_strings if "." not in x and " " not in x and x), None)
        translation = next((x for x in extra_strings + built.nested_strings
                            if TRANSLATION_HINT.match(x)), None)
        entry = {"name": name, "ordinal": ordinal, "field": None,
                 "id": extra_ints[0] if extra_ints else ordinal,
                 "serialized_name": token, "translation_key": translation,
                 "constructor_descriptor": ref[2]}
        constants.append(entry)
        awaiting_field = entry
    if not constants:
        return None
    declared = declared_constant_fields(members, clinit, class_name)
    if declared is not None and declared != [c["field"] for c in constants]:
        return None
    codecs: dict[str, dict[str, Any]] = {}
    window: list[Insn] = []
    for ins in clinit.insns:
        window.append(ins)
        if ins.op != "putstatic":
            continue
        # Each static initializer is judged on its own instructions only.
        initializer, window = window, []
        fld = ins.field()
        if not (fld and fld[2].endswith("Codec;")) or fld[2].endswith(NON_PERSISTED_CODECS):
            continue
        joined = " ".join(x.comment for x in initializer)
        if "StringRepresentable.fromEnum" in joined or "Codec.STRING" in joined:
            kind = "string_token"
        elif "Codec.INT" in joined:
            kind = "integer_id"
        elif ("Ljava/util/function/Supplier;" in joined
              and any(c["serialized_name"] for c in constants)):
            # Obfuscated releases rename StringRepresentable, but the shape is
            # invariant: a Supplier of the constant array plus per-constant
            # dotless tokens is the string-representable codec.
            kind = "string_token"
        else:
            kind = "unknown"
        codecs[fld[1]] = {"persisted_form": kind,
                          "evidence": "static codec initialiser in <clinit>"}
    return {"class": class_name, "constants": constants, "codec_fields": codecs}


SIGNATURE_RE = re.compile(r"^(?P<head>.*?)\s(?P<name>[\w$]+)\((?P<params>.*)\)$")


def token_bindings(insns: list[Insn], enum_class: str) -> tuple[dict[str, str], list[str], bool]:
    """Recover `constant -> literal` pairs from a comparison/ternary chain.

    Releases that serialise an enum as a string do it with an inline chain such as
    `arm == LEFT ? "left" : "right"`, either in the save method or in a lambda.
    The branch that follows the comparison decides which literal belongs to the
    constant: after `if_acmpne` the fall-through literal does, after `if_acmpeq`
    (`arm != LEFT ? "right" : "left"`) the literal at the jump target does.
    Whatever literal is left over belongs to the fall-through branch.

    The third value is False when a constant was loaded without a comparison
    this function understands, so the polarity of its literal is a guess.
    """
    bindings: dict[str, str] = {}
    leftovers: list[str] = []
    pending: str | None = None
    target: int | None = None
    just_loaded = False
    understood = True
    for ins in insns:
        fld = ins.field()
        if ins.op == "getstatic" and fld and fld[0] == enum_class and fld[2] == f"L{enum_class};":
            pending, target, just_loaded = fld[1], None, True
            continue
        if just_loaded:
            just_loaded = False
            if ins.op == "if_acmpeq" and ins.arg.isdigit():
                target = int(ins.arg)
            elif ins.op != "if_acmpne":
                understood = False
        text = ins.string()
        if text is None:
            continue
        if pending is not None and pending not in bindings and (target is None or ins.off >= target):
            bindings[pending] = text
            pending = None
        else:
            leftovers.append(text)
    return bindings, leftovers, understood


def complete_token_map(info: dict[str, Any], bindings: dict[str, str],
                       leftovers: list[str]) -> dict[str, str]:
    """Attribute the single fall-through literal when exactly one constant is left."""
    result = dict(bindings)
    unbound = [c for c in info["constants"] if c.get("field") not in result and c["name"] not in result]
    unique = [x for x in leftovers if x not in result.values()]
    if len(unbound) == 1 and len(unique) == 1:
        key = unbound[0].get("field") or unbound[0]["name"]
        result[key] = unique[0]
    return result


def enum_string_lambda(members: list[Member], enum_class: str) -> tuple[list[Insn], bool]:
    """Locate the synthetic `<Enum> -> String` lambda used by codec/visitor calls.

    The second value is False when several such methods carry string literals:
    which of them the codec calls is then not known.
    """
    dotted = enum_class.replace("/", ".")
    found: list[list[Insn]] = []
    for m in members:
        sig = SIGNATURE_RE.match(m.signature.rstrip(";").strip())
        if not sig:
            continue
        head = sig.group("head").split()
        if not head or head[-1] != "java.lang.String":
            continue
        params = [p.strip() for p in sig.group("params").split(",") if p.strip()]
        if params == [dotted]:
            found.append(m.insns)
    if not found:
        return [], True
    lettered = [insns for insns in found if any(i.string() is not None for i in insns)]
    return (lettered or found)[0], len(lettered) <= 1


def enum_token(constant: dict[str, Any], token_map: dict[str, str] | None) -> str | None:
    if token_map:
        for key in (constant.get("field"), constant.get("name")):
            if key and key in token_map:
                return token_map[key]
    return constant.get("serialized_name")


def distinct_ids(enum_info: dict[str, Any]) -> bool:
    """Whether every constant has its own id: duplicated ids prove none of them."""
    ids = [c["id"] for c in enum_info["constants"]]
    return None not in ids and len(set(ids)) == len(ids)


def enum_persisted_value(enum_info: dict[str, Any], constant_name: str, form: str,
                         token_map: dict[str, str] | None = None) -> Any:
    for c in enum_info["constants"]:
        if constant_name not in (c["name"], c.get("field")):
            continue
        if form == "integer_id":
            return c["id"] if distinct_ids(enum_info) else None
        if form == "string_token":
            return enum_token(c, token_map)
        return None
    return None


def enum_domain(enum_info: dict[str, Any], form: str,
                token_map: dict[str, str] | None = None) -> dict[str, Any]:
    values = []
    for c in enum_info["constants"]:
        token = enum_token(c, token_map)
        entry = {"enum_constant": c["name"], "enum_field": c.get("field"), "ordinal": c["ordinal"],
                 "id": c["id"], "serialized_name": token, "translation_key": c["translation_key"]}
        if form == "integer_id":
            entry["persisted"] = c["id"] if distinct_ids(enum_info) else None
        elif form == "string_token":
            entry["persisted"] = token
        else:
            entry["persisted"] = None
        values.append(entry)
    return {"source_enum_class": enum_info["class"], "persisted_form": form, "values": values}


# ---------------------------------------------------------------------------
# Era detection
# ---------------------------------------------------------------------------

PRIMITIVE_VISITOR = {"Z", "I", "D", "F", "J"}


def visitor_shape(desc: str) -> str | None:
    """Classify a candidate options-visitor descriptor without relying on method names.

    Obfuscated releases rename the visitor method, but its descriptor shape is
    invariant across every release that uses one.
    """
    params, ret = split_descriptor(desc)
    if not params or params[0] != "Ljava/lang/String;":
        return None
    rest = params[1:]
    if len(rest) == 1 and rest[0] in PRIMITIVE_VISITOR and ret == rest[0]:
        return "primitive"
    if len(rest) == 1 and rest[0] == "Ljava/lang/String;" and ret == "Ljava/lang/String;":
        return "string"
    if (len(rest) == 1 and rest[0].startswith("L") and ret == "V"
            and not rest[0].startswith("Ljava/")):
        return "option_instance"
    if (len(rest) == 3 and rest[0] == "Ljava/lang/Object;" and ret == "Ljava/lang/Object;"
            and rest[1].startswith("Ljava/util/function/")):
        return "enum_by_id" if rest[1] == "Ljava/util/function/IntFunction;" else "custom_codec"
    return None


def detect_era(members: list[Member]) -> tuple[str, str | None]:
    """Return (era, option_instance_class)."""
    option_candidates: dict[str, int] = defaultdict(int)
    field_access = False
    for m in members:
        for ins in m.insns:
            ref = ins.method()
            if not ref or ins.op not in ("invokeinterface", "invokevirtual"):
                continue
            shape = visitor_shape(ref[2])
            if shape is None:
                continue
            if shape == "option_instance":
                option_candidates[split_descriptor(ref[2])[0][1][1:-1]] += 1
            else:
                field_access = True
    if option_candidates:
        best = max(sorted(option_candidates), key=lambda k: option_candidates[k])
        if option_candidates[best] >= 5:
            return "option_instance", best
    if field_access:
        return "field_access", None
    return "legacy_direct", None


# ---------------------------------------------------------------------------
# Extractor: legacy direct parse/serialise era (through 1.16.5)
# ---------------------------------------------------------------------------

PARSE_KINDS = {
    "java/lang/Integer.parseInt": ("integer", "decimal integer"),
    "java/lang/Long.parseLong": ("integer", "decimal integer"),
    "java/lang/Float.parseFloat": ("number", "decimal floating point"),
    "java/lang/Double.parseDouble": ("number", "decimal floating point"),
    "java/lang/Boolean.parseBoolean": ("boolean", "lowercase true/false"),
}

APPEND_TYPES = {"(I)": ("integer", "decimal integer"), "(J)": ("integer", "decimal integer"),
                "(F)": ("number", "decimal floating point"), "(D)": ("number", "decimal floating point"),
                "(Z)": ("boolean", "lowercase true/false"),
                "(Ljava/lang/String;)": ("string", "UTF-8 text after the first colon"),
                "(C)": ("string", "single character")}


def legacy_load_blocks(member: Member) -> dict[str, dict[str, Any]]:
    """Parse `if ("key".equals(name)) { field = <expr>; }` chains."""
    out: dict[str, dict[str, Any]] = {}
    insns = member.insns
    candidates: list[tuple[int, int, str, str]] = []  # (ldc index, equals index, key, operand)
    for i, ins in enumerate(insns):
        key = ins.string()
        if key is None or not key or ":" in key or " " in key:
            continue
        eq = None
        for j in range(i + 1, min(i + 5, len(insns))):
            ref = insns[j].method()
            if ref and ref[0] == "java/lang/String" and ref[1] == "equals":
                eq = j
                break
        if eq is None:
            continue
        operand = " ".join(x.op + " " + x.arg for x in insns[i + 1:eq]).strip()
        candidates.append((i, eq, key, operand))
    # The outer chain always compares against the same local (the key being read);
    # inner comparisons test the *value* local.  Keeping only the dominant operand
    # removes value literals such as "true", "fast" or "0" from the key set.
    operands: dict[str, int] = defaultdict(int)
    for _, _, _, operand in candidates:
        operands[operand] += 1
    dominant = max(sorted(operands), key=lambda o: operands[o]) if operands else None
    spans: list[tuple[int, int, int, str]] = []  # (ldc index, body start, body end, key)
    for i, eq, key, operand in candidates:
        if dominant is not None and operand != dominant:
            continue
        branch = None
        for j in range(eq + 1, min(eq + 3, len(insns))):
            if insns[j].op in ("ifeq", "ifne"):
                try:
                    branch = int(insns[j].arg.split()[0])
                except (ValueError, IndexError):
                    branch = None
                start = j + 1
                break
        else:
            continue
        end = member.by_offset.get(branch, len(insns)) if branch is not None else len(insns)
        spans.append((i, start, end, key))
    # A comparison nested inside another branch tests the *value*, not the key.
    # Older releases reuse the same local slot for both, so containment is the
    # reliable discriminator that removes literals such as "true", "fast" or "0".
    nested: set[int] = set()
    for a, (ia, _, _, _) in enumerate(spans):
        for b, (_, sb, eb, _) in enumerate(spans):
            if a != b and sb <= ia < eb:
                nested.add(a)
                break
    for a, (i, start, end, key) in enumerate(spans):
        if a in nested:
            continue
        body = insns[start:end]
        if not body or len(body) > 90:
            continue
        record = analyse_legacy_body(body)
        if record:
            record["persisted_key"] = key
            out.setdefault(key, record)
    return out


def compared_literals(body: list[Insn], field: str) -> tuple[list[str], dict[str, dict[str, Any]]]:
    """Every literal a load block compares the value against, and the constant each one stores."""
    literals: list[str] = []
    values: dict[str, dict[str, Any]] = {}
    compared: list[int] = []
    for i, ins in enumerate(body):
        text = ins.string()
        if text is None:
            continue
        if any((body[j].method() or ("", "", ""))[1] == "equals" for j in range(i + 1, min(i + 4, len(body)))):
            compared.append(i)
            if text not in literals:
                literals.append(text)
    for n, i in enumerate(compared):
        limit = compared[n + 1] if n + 1 < len(compared) else len(body)
        for j in range(i + 1, min(limit, i + 14)):
            fld = body[j].field()
            if body[j].op != "putfield" or not fld or fld[1] != field:
                continue
            stored = body[j - 1]
            number = const_int(stored)
            source = stored.field()
            if number is not None:
                values.setdefault(body[i].string(), {"kind": "number", "value": number})
            elif stored.op == "getstatic" and source and source[0] and source[2] == f"L{source[0]};":
                values.setdefault(body[i].string(), {"kind": "enum_constant", "enum_class": source[0],
                                                     "field": source[1]})
            break
    return literals, values


def analyse_legacy_body(body: list[Insn]) -> dict[str, Any] | None:
    typ = "unknown"
    encoding = None
    enum_class = None
    enum_from = None
    # A branch body may assign the same field several times (legacy spellings of
    # enum options accept true/false as well as an id).  The dominant field and
    # its last assignment delimit the expression that produces the stored value.
    assignments: dict[str, list[int]] = defaultdict(list)
    descriptors: dict[str, str] = {}
    for i, ins in enumerate(body):
        fld = ins.field()
        if ins.op == "putfield" and fld:
            assignments[fld[1]].append(i)
            descriptors[fld[1]] = fld[2]
    if not assignments:
        return None
    field = max(sorted(assignments), key=lambda f: len(assignments[f]))
    descriptor = descriptors[field]
    body = body[:assignments[field][-1] + 1]
    literals, literal_values = compared_literals(body, field)
    for i, ins in enumerate(body):
        ref = ins.method()
        if ins.op == "getstatic":
            fld = ins.field()
            if fld and fld[0] and fld[2] == f"L{fld[0]};" and enum_class is None:
                enum_class = fld[0]
        if ref:
            qualified = f"{ref[0]}.{ref[1]}" if ref[0] else ref[1]
            if qualified in PARSE_KINDS:
                typ, encoding = PARSE_KINDS[qualified]
            elif ref[0] is None and ref[2] == "(Ljava/lang/String;)F":
                typ, encoding = "number", "decimal floating point"
            elif ref[0] is None and ref[2] == "(Ljava/lang/String;)I":
                typ, encoding = "integer", "decimal integer"
            elif ref[0] is None and ref[2] == "(Ljava/lang/String;)D":
                typ, encoding = "number", "decimal floating point"
            elif ref[0] and ref[2].startswith("(I)L") and ref[2][4:-1] == ref[0]:
                enum_class, enum_from = ref[0], "integer_id"
            elif ref[0] and ref[2].startswith("(Ljava/lang/String;)L") and ref[2][21:-1] == ref[0]:
                enum_class, enum_from = ref[0], "string_token"
    if descriptor and descriptor.startswith("L") and not descriptor.startswith("Ljava/"):
        enum_class = enum_class or descriptor[1:-1]
    # `if ("left".equals(v)) field = Arm.LEFT;` chains name the persisted token of
    # each enum constant outright, which is the only static source for releases
    # whose enum keeps no serialized-name field.
    token_map: dict[str, str] = {}
    if enum_class:
        for i, ins in enumerate(body):
            text = ins.string()
            if not text or text in ("true", "false") or "." in text or " " in text:
                continue
            equals = any((body[j].method() or ("", "", ""))[1] == "equals"
                         for j in range(i + 1, min(i + 4, len(body))))
            if not equals:
                continue
            for j in range(i + 1, min(i + 10, len(body))):
                fld = body[j].field()
                if body[j].op == "getstatic" and fld and fld[0] == enum_class:
                    token_map.setdefault(fld[1], text)
                    break
    if token_map and enum_from is None:
        enum_from = "string_token"
    # A loader that also compares "fast" or a number reads more than a boolean.
    saw_true_compare = "true" in literals
    if (saw_true_compare and typ == "unknown" and enum_class is None
            and set(literals) <= {"true", "false"}):
        typ, encoding = "boolean", 'lowercase true/false compared against the literal "true"'
    if enum_class:
        typ = "enum"
        encoding = {"integer_id": "enum id integer",
                    "string_token": "enum serialized string token"}.get(enum_from or "", "enum, codec unresolved")
    ops = arithmetic_chain(body)
    return {"type": typ, "field": field, "field_descriptor": descriptor,
            "file_encoding": encoding, "enum_class": enum_class, "enum_persisted_form": enum_from,
            "enum_token_map": token_map or None,
            "value_literals": literals, "literal_values": literal_values,
            "accepts_legacy_boolean": saw_true_compare and typ != "boolean",
            "load_transform_ops": ops or [],
            "storage_to_value": formula(ops, "file") if ops else None,
            **({"transform_resolved": False} if ops is None else {})}


def legacy_save_blocks(member: Member) -> dict[str, dict[str, Any]]:
    """Parse `writer.println("key:" + <expr>)` chains."""
    out: dict[str, dict[str, Any]] = {}
    insns = member.insns
    for i, ins in enumerate(insns):
        s = ins.string()
        if s is None:
            continue
        s = s.rstrip(" ")  # some releases write the prefix as "key: "
        if not s.endswith(":") or len(s) < 2 or " " in s:
            continue
        key = s[:-1]
        if not re.fullmatch(r"[A-Za-z][A-Za-z0-9_.-]*", key):
            continue
        body: list[Insn] = []
        for j in range(i + 1, min(i + 40, len(insns))):
            ref = insns[j].method()
            if ref and ref[1] == "toString" and ref[0] == "java/lang/StringBuilder":
                break
            body.append(insns[j])
        if not body:
            continue
        field = None
        descriptor = None
        appends: list[str] = []
        enum_getter = None
        for ins2 in body:
            fld = ins2.field()
            if ins2.op == "getfield" and fld and field is None:
                field, descriptor = fld[1], fld[2]
            ref = ins2.method()
            if ref and ref[0] == "java/lang/StringBuilder" and ref[1] == "append":
                appends.append(ref[2].split(")")[0] + ")")
            elif ref and ref[0] and ref[2] in ("()I", "()Ljava/lang/String;") and ref[0] != "java/lang/StringBuilder":
                enum_getter = (ref[0], ref[2])
        ops = arithmetic_chain(body)
        typ = encoding = None
        for desc in appends:
            if desc in APPEND_TYPES:
                typ, encoding = APPEND_TYPES[desc]
        out[key] = {"persisted_key": key, "field": field, "field_descriptor": descriptor,
                    "save_type": typ, "save_encoding": encoding,
                    "save_transform_ops": ops or [],
                    "value_to_storage": formula(ops, "value") if ops else None,
                    "enum_getter": enum_getter, "body": body,
                    **({"transform_resolved": False} if ops is None else {})}
    return out


CONSTANT_LINE_RE = re.compile(r"([A-Za-z][A-Za-z0-9_.-]*):([A-Za-z0-9_.-]+)")
NON_BOOLEAN_PRIMITIVES = {"I", "J", "F", "D", "B", "S", "C"}


def legacy_constant_saves(member: Member) -> dict[str, list[str]]:
    """Collect `writer.println("key:token")` lines, which a switch writes one per value."""
    out: dict[str, list[str]] = defaultdict(list)
    insns = member.insns
    for i, ins in enumerate(insns[:-1]):
        match = CONSTANT_LINE_RE.fullmatch(ins.string() or "")
        following = insns[i + 1].method()
        if match and following and following[1] == "println" and match.group(2) not in out[match.group(1)]:
            out[match.group(1)].append(match.group(2))
    return dict(out)


def literal_enum_tokens(record: dict[str, Any]) -> dict[str, str]:
    """Enum field -> token for the written literals; literals sharing a constant bind nothing."""
    enum_class = record.get("enum_class")
    bound: dict[str, str] = {}
    for literal in record.get("save_literals") or []:
        stored = (record.get("literal_values") or {}).get(literal) or {}
        if stored.get("kind") == "enum_constant" and stored.get("enum_class") == enum_class:
            if stored["field"] in bound:
                return {}
            bound[stored["field"]] = literal
    return bound


def literal_token_domain(record: dict[str, Any]) -> dict[str, Any]:
    """The token domain of a key held as a plain number, with no enum class to name it."""
    stored = record.get("literal_values") or {}
    tokens = list(record.get("save_literals") or [])
    ids = {t: stored[t]["value"] for t in tokens if (stored.get(t) or {}).get("kind") == "number"}
    ordered = sorted(tokens, key=lambda t: (t not in ids, ids.get(t, 0), t))
    return {"source_enum_class": None, "persisted_form": "string_token",
            "values": [{"enum_constant": None, "enum_field": None, "ordinal": n, "id": ids.get(token),
                        "serialized_name": token, "translation_key": None, "persisted": token}
                       for n, token in enumerate(ordered)]}


def extract_legacy(members: list[Member]) -> dict[str, dict[str, Any]]:
    loads: dict[str, dict[str, Any]] = {}
    saves: dict[str, dict[str, Any]] = {}
    constants: dict[str, list[str]] = {}
    for m in members:
        found = legacy_load_blocks(m)
        for k, v in found.items():
            loads.setdefault(k, v)
        found_save = legacy_save_blocks(m)
        for k, v in found_save.items():
            saves.setdefault(k, v)
        for k, v in legacy_constant_saves(m).items():
            constants.setdefault(k, v)
    merged: dict[str, dict[str, Any]] = {}
    for key in sorted(set(loads) | set(saves)):
        load = loads.get(key, {})
        save = saves.get(key, {})
        written = constants.get(key, [])
        notes: list[str] = []
        load_type = load.get("type") or "unknown"
        # The writer decides what ends up in the file, so the save side wins any
        # disagreement.  A lenient loader is recorded as a note, never as the type.
        typ = save.get("save_type") or load_type
        if load.get("enum_class"):
            typ = "enum"
        elif (save.get("save_type") and load_type not in ("unknown", save["save_type"])):
            notes.append(f"the loader additionally accepts a {load_type} spelling of this key, but the "
                         f"writer always emits {save['save_type']}")
        enum_form = load.get("enum_persisted_form")
        getter = save.get("enum_getter")
        if load.get("enum_class") and getter:
            derived = "integer_id" if getter[1] == "()I" else "string_token"
            if enum_form and enum_form != derived:
                notes.append(f"loader reads the enum as {enum_form} while the writer emits {derived}")
            enum_form = derived
        if load.get("accepts_legacy_boolean") and "true" not in written:
            notes.append('the loader also accepts the legacy true/false spellings of this key')
        encoding = save.get("save_encoding") or load.get("file_encoding")
        # Constant `key:token` lines name the persisted tokens; the loader must read each back.
        tokens_proven = False
        if set(written) - {"true", "false"}:
            if set(written) <= set(load.get("value_literals") or []):
                tokens_proven = True
                typ, encoding = "enum", "enum serialized string token"
                enum_form = "string_token"
            else:
                if not load.get("enum_class"):
                    typ, encoding = "unknown", None
                notes.append("the writer emits " + ", ".join(written) + " but the loader does not read "
                             "every one of them back, so the persisted tokens are not proven")
        if typ == "boolean" and (load.get("field_descriptor") or save.get("field_descriptor")
                                 ) in NON_BOOLEAN_PRIMITIVES:
            typ, encoding = "unknown", None
            notes.append("the backing field is not a boolean, so the key is not typed as one")
        if load.get("enum_class"):
            encoding = {"integer_id": "enum id integer",
                        "string_token": "enum serialized string token"}.get(enum_form or "",
                                                                            "enum, codec unresolved")
        unread = any(side.get("transform_resolved") is False for side in (load, save))
        load_ops = [] if unread else load.get("load_transform_ops") or []
        ops = [] if unread else save.get("save_transform_ops") or []
        if not ops and load_ops:
            ops = invert_ops(load_ops)
        if unread:
            notes.append(UNREAD_TRANSFORM_NOTE)
        merged[key] = {
            "persisted_key": key,
            "field": load.get("field") or save.get("field"),
            "field_descriptor": load.get("field_descriptor") or save.get("field_descriptor"),
            "type": typ,
            "file_encoding": encoding,
            "enum_class": load.get("enum_class"),
            "enum_persisted_form": enum_form,
            "enum_token_map": load.get("enum_token_map"),
            "value_literals": load.get("value_literals") or [],
            "save_literals": written,
            "literal_values": load.get("literal_values") or {},
            "literal_tokens_proven": tokens_proven,
            "save_body": save.get("body"),
            "value_to_storage": formula(ops, "value") if ops else None,
            "storage_to_value": formula(load_ops, "file") if load_ops else None,
            **({"transform_resolved": False} if unread else {}),
            "transform_ops": ops,
            "notes": notes,
            "evidence_sources": [x for x in (("load if-chain" if load else None),
                                             ("save println chain" if save else None)) if x],
        }
    return merged


# ---------------------------------------------------------------------------
# Extractor: FieldAccess visitor era (1.17 - 1.18.2, plus modern primitives)
# ---------------------------------------------------------------------------

VISITOR_TYPES = {"Z": ("boolean", "lowercase true/false"), "I": ("integer", "decimal integer"),
                 "D": ("number", "decimal floating point"), "F": ("number", "decimal floating point"),
                 "J": ("integer", "decimal integer")}


# Instructions that end the statement a visitor call belongs to: the store
# that consumes its result, a discarded result, a return, or a branch.
def ends_statement(op: str) -> bool:
    return (op.startswith(("putfield", "putstatic", "pop", "if", "goto", "tableswitch", "lookupswitch"))
            or op.endswith("return") or re.fullmatch(r"[ilfda]store(?:_\d)?", op) is not None)


def statement_tail(insns: list[Insn], start: int) -> list[Insn] | None:
    """The instructions from `start` through the one that ends the statement,
    or None when the method ends first.

    What follows a visitor call up to that instruction is the arithmetic that
    turns the stored value back into the field's. Past it begins the next
    option's statement, whose arithmetic is not this option's.
    """
    for end in range(start, len(insns)):
        if ends_statement(insns[end].op):
            return insns[start:end + 1]
    return None


def read_visitor_transform(record: dict[str, Any], pre: list[Insn], post: list[Insn] | None) -> None:
    """Record the arithmetic around a numeric visitor call, or that it is unread.

    `pre` builds the stored value and `post` rebuilds the field's from the
    stored one. Each must be fully readable and the two must undo each other;
    otherwise no transform is claimed and the record says the transform is
    unresolved, which is not the same as stored as-is.
    """
    encode = exact_arithmetic(pre)
    decode = exact_arithmetic(post) if post is not None else None
    if encode is None or decode is None or invert_ops(encode) != decode:
        record["value_to_storage"] = record["storage_to_value"] = None
        record["transform_ops"] = []
        record["transform_resolved"] = False
        record.setdefault("notes", []).append(UNREAD_TRANSFORM_NOTE)
        return
    record["value_to_storage"] = formula(encode, "value") if encode else None
    record["storage_to_value"] = formula(decode, "file") if decode else None
    record["transform_ops"] = encode
    if encode:
        record["transform_resolved"] = True


def extract_visitor(dis: Disassembler, members: list[Member],
                    option_class: str | None) -> dict[str, dict[str, Any]]:
    out: dict[str, dict[str, Any]] = {}
    for m in members:
        insns = m.insns
        for i, ins in enumerate(insns):
            ref = ins.method()
            if not ref or ins.op not in ("invokeinterface", "invokevirtual"):
                continue
            desc = ref[2]
            shape = visitor_shape(desc)
            if shape is None:
                continue
            key = None
            key_at = None
            for j in range(i - 1, max(-1, i - 40), -1):
                s = insns[j].string()
                if s is not None and re.fullmatch(r"[A-Za-z][A-Za-z0-9_.-]*", s) and s != "true":
                    key, key_at = s, j
                    break
            if key is None or key in out:
                continue
            pre = insns[key_at + 1:i]
            field = None
            descriptor = None
            for ins2 in pre:
                fld = ins2.field()
                if ins2.op == "getfield" and fld:
                    field, descriptor = fld[1], fld[2]
                    break
            post = statement_tail(insns, i + 1)
            post_field = None
            for ins2 in post or []:
                fld = ins2.field()
                if ins2.op == "putfield" and fld:
                    post_field = fld[1]
                    if descriptor is None:
                        descriptor = fld[2]
                    break
            inner = split_descriptor(desc)[0][1]
            record: dict[str, Any] = {"persisted_key": key, "field": field or post_field,
                                      "field_descriptor": descriptor, "visitor_descriptor": desc,
                                      "kind": shape}
            if shape == "option_instance":
                pass
            elif shape == "primitive":
                record["type"], record["file_encoding"] = VISITOR_TYPES[inner]
                if inner == "Z":
                    record.update(value_to_storage=None, storage_to_value=None, transform_ops=[])
                else:
                    read_visitor_transform(record, pre, post)
            elif shape == "string":
                record["type"], record["file_encoding"] = "string", "UTF-8 text after the first colon"
            elif shape in ("enum_by_id", "custom_codec"):
                enum_class = None
                if descriptor and descriptor.startswith("L") and not descriptor.startswith("Ljava/"):
                    enum_class = descriptor[1:-1]
                if descriptor == "Ljava/util/List;" or enum_class is None:
                    record["type"] = "special"
                    record["file_encoding"] = ("custom to-string/from-string functions supplied at the "
                                               "call site; the value is not a simple scalar")
                else:
                    form = "integer_id" if shape == "enum_by_id" else "string_token"
                    info = extract_enum(dis, enum_class)
                    record["type"] = "enum"
                    record["file_encoding"] = ("enum id integer" if form == "integer_id"
                                               else "enum serialized string token")
                    if info:
                        record["allowed_values"] = enum_domain(info, form)
                    else:
                        record["allowed_values"] = {"source_enum_class": enum_class,
                                                    "persisted_form": form, "values": None}
            else:
                record["type"] = "unknown"
            out[key] = record
    return out


# ---------------------------------------------------------------------------
# Extractor: OptionInstance definitions (1.19+)
# ---------------------------------------------------------------------------

# `Codec.doubleRange(0.0, 1.0)`: both bounds pushed straight into the call.
UNIT_DOUBLE_RANGE = re.compile(
    r"dconst_0\b[^\n]*\n\s*\d+:\s*dconst_1\b[^\n]*\n\s*\d+:\s*invokestatic\s[^\n]*"
    r"Codec\.doubleRange:")


def classify_value_set(dis: Disassembler, class_name: str) -> dict[str, Any]:
    """The value set an `OptionInstance` constant stands for; unstated bounds stay None."""
    text = dis.get(class_name)
    if not text:
        return {"kind": "unknown"}
    if UNIT_DOUBLE_RANGE.search(text):
        return {"kind": "unit_double", "min": 0.0, "max": 1.0}
    if "Codec.intRange" in text or re.search(r"private final int [a-z]", text):
        return {"kind": "int_range", "min": None, "max": None}
    if "Codec.doubleRange" in text:
        return {"kind": "double_range", "min": None, "max": None}
    return {"kind": "unknown"}


def extract_option_instances(dis: Disassembler, members: list[Member],
                             option_class: str,
                             owner: str | None = None) -> dict[str, dict[str, Any]]:
    """Recover OptionInstance definitions keyed by the field they are stored in."""
    out: dict[str, dict[str, Any]] = {}
    for m in members:
        insns = m.insns
        start = 0
        for i, ins in enumerate(insns):
            fld = ins.field()
            if ins.op != "putfield" or not fld or fld[2] != f"L{option_class};":
                continue
            window = insns[max(start, i - 170):i + 1]
            start = i + 1
            field = fld[1]
            if field in out:
                continue
            # The construction starts at the last `new <OptionInstance>` in the
            # window (or, for a static factory, at the window start); its first
            # string argument is the option's translation key.  A few options key
            # off `key.*` rather than `options.*`, so the prefix is not required.
            begin = 0
            for j, x in enumerate(window):
                if x.op == "new" and x.klass() == option_class:
                    begin = j
            translation = next((x.string() for x in window[begin:]
                                if x.string() is not None
                                and re.fullmatch(r"[a-z][A-Za-z0-9_]*(?:\.[A-Za-z0-9_]+)+",
                                                 x.string())), None)
            if translation is None:
                continue
            first = next(j for j, x in enumerate(window) if x.string() == translation)
            record = analyse_option_window(dis, window[first:], option_class, members, owner)
            record["translation_key"] = translation
            record["field"] = field
            out[field] = record
    return out


def xmap_targets(dis: Disassembler, before: list[Insn],
                 owner: str | None) -> list[tuple[str, str, str]] | None:
    """The two methods behind the `xmap` call that ends `before`, decode first."""
    if not owner:
        return None
    indy = [ins for ins in before[-6:] if ins.op == "invokedynamic"]
    if len(indy) < 2:
        return None
    targets = bootstrap_targets(dis.verbose(owner))
    pair = []
    for ins in indy[-2:]:
        m = M_INDY.match(ins.comment)
        if not m:
            return None
        target = targets.get(int(m.group(1)))
        if not target:
            return None
        pair.append(target)
    return pair


def is_enum_id_mapping(dis: Disassembler, pair: list[tuple[str, str, str]]) -> bool:
    """Whether an `xmap` pair converts between an enum and one stored value,
    as `xmap(Enum::byId, Enum::getId)` does.

    That conversion is the enum's own persisted form, which the enum domain
    already describes, so it leaves no arithmetic to read.
    """
    (decode_class, _, decode_desc), (encode_class, _, encode_desc) = pair
    if decode_class != encode_class or extract_enum(dis, decode_class) is None:
        return False
    decode_params, decode_ret = split_descriptor(decode_desc)
    encode_params, encode_ret = split_descriptor(encode_desc)
    if decode_ret != f"L{decode_class};" or len(decode_params) != 1:
        return False
    # An instance method such as `getId()` takes the enum as its receiver.
    return encode_ret == decode_params[0] and encode_params in ([], [f"L{decode_class};"])


def xmap_transform(dis: Disassembler, before: list[Insn], members: list[Member] | None,
                   owner: str | None, plain: bool = False) -> list[dict[str, Any]] | None:
    """The value-to-storage arithmetic of a `Codec.xmap` pair.

    `xmap(to, from)` takes the storage-to-value function first and the
    value-to-storage function second, so the second of the two lambdas
    immediately before the call is the one that decides what lands in the file.
    Both are recovered and checked against each other: the pair must agree on
    each other's types and one must undo the other. A pair that does not is
    left unresolved, because a half-understood transform is how a value gets
    written through the wrong arithmetic.
    """
    pair = xmap_targets(dis, before, owner)
    if not members or not pair:
        return None
    (_, decode_name, decode_desc), (_, encode_name, encode_desc) = pair
    decode_params, decode_ret = split_descriptor(decode_desc)
    encode_params, encode_ret = split_descriptor(encode_desc)
    if len(decode_params) != 1 or len(encode_params) != 1:
        return None
    if decode_params[0] != encode_ret or encode_params[0] != decode_ret:
        return None
    encode = lambda_arithmetic(members, owner, encode_name, encode_desc, plain)
    decode = lambda_arithmetic(members, owner, decode_name, decode_desc, plain)
    if not encode or not decode:
        return None
    if invert_ops(encode) != decode:
        return None
    return encode


SLIDER_MAPPING_PARAMS = "(Ljava/util/function/IntFunction;Ljava/util/function/ToIntFunction;"


def direct_range_bounds(window: list[Insn], idx: int) -> list[float] | None:
    """The bounds of the `Codec.intRange`/`doubleRange` call at `idx`, if both
    are constants pushed straight into it.

    A call between a constant and the range call (`logMouse(-200)` feeding
    `doubleRange`) makes the constant an argument of that call, not a bound, so
    the bounds are then unresolved rather than the helper's arguments.
    """
    if idx < 2:
        return None
    low, high = const_num(window[idx - 2]), const_num(window[idx - 1])
    return None if low is None or high is None else [low, high]


def slider_step(dis: Disassembler, before: list[Insn], members: list[Member] | None,
                owner: str | None, slider: dict[str, Any],
                stored_range: list[float] | None) -> float | None:
    """The step of a double option behind an integer slider, in stored units.

    The slider's own `xmap` maps slider positions onto the stored value. Its
    arithmetic is trusted only when both lambdas are plain arithmetic (a call
    such as `Math.pow` makes the mapping non-linear) and it carries both slider
    ends exactly onto the stored range: a mapping that cannot be tied to the
    range the codec accepts proves nothing about the step.
    """
    if not stored_range or slider.get("min") is None or slider.get("max") is None:
        return None
    encode = xmap_transform(dis, before, members, owner, plain=True)
    if not encode:
        return None
    decode = invert_ops(encode)
    ends = [apply_ops(slider["min"], decode), apply_ops(slider["max"], decode)]
    next_position = apply_ops(slider["min"] + 1, decode)
    if None in ends or next_position is None or ends != [round(v, 6) for v in stored_range]:
        return None
    step = round(next_position - ends[0], 6)
    return step if step > 0 else None


def analyse_option_window(dis: Disassembler, window: list[Insn], option_class: str,
                         members: list[Member] | None = None,
                         owner: str | None = None) -> dict[str, Any]:
    record: dict[str, Any] = {"type": "unknown", "default": None, "numeric": None,
                              "allowed_values": None, "file_encoding": None,
                              "value_set": None, "codec": None, "notes": []}
    codec_range: list[float] | None = None
    codec_kind: str | None = None
    value_set: dict[str, Any] | None = None
    enum_class: str | None = None
    enum_codec_field: str | None = None
    xmap = False
    xmap_steps: list[list[dict[str, Any]]] = []
    xmap_unresolved = False
    slider_xmap: list[Insn] | None = None
    slider_set: dict[str, Any] = {}
    stored_step: float | None = None
    factory_desc: str | None = None
    ctor_desc: str | None = None

    for idx, ins in enumerate(window):
        ref = ins.method()
        fld = ins.field()
        if ref and ref[0] == "com/mojang/serialization/Codec":
            if ref[1] == "intRange":
                codec_range = direct_range_bounds(window, idx)
                codec_kind = "integer"
            elif ref[1] == "doubleRange":
                codec_range = direct_range_bounds(window, idx)
                codec_kind = "number"
        # Two different `xmap`s reach this window. One belongs to the codec and
        # decides what lands in the file; the other belongs to the option's own
        # slider and only maps slider positions onto the value the user picks.
        # Only the codec's changes what is written.
        if ref and ref[1] == "xmap" and (ref[0] or "").startswith("com/mojang/serialization"):
            xmap = True
            resolved = xmap_transform(dis, window[:idx], members, owner)
            pair = xmap_targets(dis, window[:idx], owner)
            if resolved is not None:
                xmap_steps.append(resolved)
            elif not (pair and is_enum_id_mapping(dis, pair)):
                xmap_unresolved = True
        # The slider's own mapping is recognized by its descriptor, because
        # obfuscated releases rename the method.
        if (ref and (ref[0] or "").startswith(option_class + "$") and value_set is not None
                and ref[2].startswith(SLIDER_MAPPING_PARAMS)):
            slider_xmap = window[:idx]
            slider_set = value_set
        if fld and fld[0] == "com/mojang/serialization/Codec":
            codec_kind = {"DOUBLE": "number", "FLOAT": "number", "INT": "integer",
                          "BOOL": "boolean", "STRING": "string", "LONG": "integer"}.get(fld[1], codec_kind)
        if fld and fld[2].endswith("Codec;") and fld[0] and fld[0] != "com/mojang/serialization/Codec":
            enum_codec_field = fld[1]
            if enum_class is None:
                enum_class = fld[0]
        if ref and ref[1] == "values" and ref[2].startswith("()[L"):
            enum_class = ref[2][4:-1]
        if ins.op == "new":
            cls = ins.klass()
            if cls and cls.startswith(option_class + "$"):
                for j in range(idx + 1, min(idx + 30, len(window))):
                    r2 = window[j].method()
                    if r2 and r2[1] == "<init>" and r2[0] == cls:
                        args = [const_num(x) for x in window[idx + 1:j]]
                        args = [v for v in args if v is not None]
                        params = split_descriptor(r2[2])[0]
                        branches = any(
                            x.op.startswith("if") or x.op in ("goto", "goto_w")
                            for x in window[idx + 1:j]
                        )
                        if r2[2] in ("(II)V", "(IIZ)V") and branches:
                            value_set = {
                                "kind": "dynamic_int_range",
                                "min": None,
                                "max": None,
                                "class": cls,
                                "descriptor": r2[2],
                                "note": "integer range bounds are selected by runtime branches; "
                                        "the persisted type is static but one fixed domain is not",
                            }
                        elif "Ljava/util/function/IntSupplier;" in params and args:
                            value_set = {"kind": "lazy_max_int_range", "min": int(args[0]),
                                         "max": None, "hard_cap": int(args[-1]) if len(args) > 1 else None,
                                         "class": cls,
                                         "note": "maximum is supplied lazily at runtime (window/monitor "
                                                 "dependent); only the minimum and the absolute cap are static"}
                        elif r2[2] in ("(II)V", "(IIZ)V") and len(args) >= 2:
                            value_set = {"kind": "int_range", "min": int(args[0]), "max": int(args[1]),
                                         "class": cls}
                        elif r2[2] == "(III)V" and len(args) >= 3:
                            value_set = {"kind": "clamping_lazy_max_int_range", "min": int(args[0]),
                                         "max": int(args[1]), "class": cls}
                        elif "Ljava/util/List;" in r2[2]:
                            value_set = {"kind": "enum", "class": cls}
                        else:
                            value_set = {"kind": "unknown", "class": cls, "descriptor": r2[2]}
                        break
        if ins.op == "getstatic" and fld and fld[2].startswith("L" + option_class + "$"):
            inner = fld[2][1:-1]
            if value_set is None:
                info = classify_value_set(dis, inner)
                if info["kind"] != "unknown":
                    value_set = {**info, "class": inner}
        if ref and ref[1] == "<init>" and ref[0] == option_class:
            ctor_desc = ref[2]
        if ref and ref[0] == option_class and ref[2].endswith(f")L{option_class};"):
            factory_desc = ref[2]

    tail = window[-24:]
    default_value: Any = None
    default_conf = "unknown"
    default_conditional = False
    enum_default_constant = None
    for ins in tail:
        ref = ins.method()
        fld = ins.field()
        if ref and ref[0] in ("java/lang/Integer", "java/lang/Double", "java/lang/Float",
                              "java/lang/Long") and ref[1] == "valueOf":
            idx = tail.index(ins)
            val = const_num(tail[idx - 1]) if idx else None
            if val is not None:
                if runs_unconditionally(tail[idx - 1:idx + 1]):
                    default_value = int(val) if ref[0] in ("java/lang/Integer", "java/lang/Long") else val
                    default_conf = "high"
                else:
                    default_value, default_conf, default_conditional = None, "unknown", True
        elif ref and ref[0] == "java/lang/Boolean" and ref[1] == "valueOf":
            idx = tail.index(ins)
            val = const_int(tail[idx - 1]) if idx else None
            if val is not None:
                if runs_unconditionally(tail[idx - 1:idx + 1]):
                    default_value, default_conf = bool(val), "high"
                else:
                    default_value, default_conf, default_conditional = None, "unknown", True
        elif fld and enum_class and fld[0] == enum_class and fld[2] == f"L{enum_class};":
            enum_default_constant = fld[1]

    factory_params = split_descriptor(factory_desc)[0] if factory_desc else []
    if factory_desc and factory_params.count("Z") == 1 and factory_params[0] == "Ljava/lang/String;":
        record["type"] = "boolean"
        record["file_encoding"] = "lowercase true/false"
        first = next((j for j, x in enumerate(window) if const_int(x) is not None), None)
        if first is not None:
            if runs_unconditionally(window[first:first + 2]):
                default_value, default_conf = bool(const_int(window[first])), "high"
            else:
                default_value, default_conf, default_conditional = None, "unknown", True
    elif codec_kind == "boolean" or (ctor_desc and value_set is None and codec_kind is None
                                     and isinstance(default_value, bool)):
        record["type"] = "boolean"
        record["file_encoding"] = "lowercase true/false"

    if enum_class and (value_set or {}).get("kind") == "enum":
        info = extract_enum(dis, enum_class)
        form = "unknown"
        if info:
            if enum_codec_field and enum_codec_field in info["codec_fields"]:
                form = info["codec_fields"][enum_codec_field]["persisted_form"]
            elif len(info["codec_fields"]) == 1:
                form = next(iter(info["codec_fields"].values()))["persisted_form"]
            if form == "unknown" and codec_kind in ("integer", "string"):
                # The codec can also be assembled inline at the construction site.
                form = "integer_id" if codec_kind == "integer" else "string_token"
            record["allowed_values"] = enum_domain(info, form)
            record["type"] = "enum"
            record["file_encoding"] = {"integer_id": "enum id integer",
                                       "string_token": "enum serialized string token"}.get(form, "enum, codec unresolved")
            record["enum_default_constant"] = enum_default_constant
            if enum_default_constant:
                value = enum_persisted_value(info, enum_default_constant, form)
                if value is not None:
                    default_value, default_conf = value, "high"
                else:
                    record["notes"].append(f"default enum constant {enum_default_constant} has no resolvable persisted form")
                    default_value, default_conf = None, "unknown"
        else:
            record["type"] = "enum"
            record["allowed_values"] = {"source_enum_class": enum_class, "persisted_form": "unknown",
                                        "values": None}
    elif (value_set and value_set["kind"] == "int_range" and codec_kind == "number"
          and not xmap and slider_xmap is not None):
        # The slider counts integer positions, but the option holds and persists
        # a double: the slider's bounds are positions, not stored values, and
        # its step is not the stored value's step.
        record["type"] = "number"
        record["file_encoding"] = "decimal floating point"
        record["notes"].append("the settings slider counts integer positions that its own mapping "
                               "turns into the stored decimal value")
        record["numeric"] = None
        stored_step = slider_step(dis, slider_xmap, members, owner, slider_set, codec_range)
    elif value_set and value_set["kind"] in ("int_range", "clamping_lazy_max_int_range",
                                             "lazy_max_int_range", "dynamic_int_range"):
        record["type"] = "integer"
        if value_set["kind"] == "int_range" and None in (value_set.get("min"), value_set.get("max")):
            record["notes"].append("the integer range bounds could not be read, so the domain is unproven")
        elif value_set["kind"] != "dynamic_int_range":
            record["numeric"] = {"min": value_set["min"], "max": value_set["max"], "step": 1,
                                 "clamped": True}
        if value_set.get("note"):
            record["notes"].append(value_set["note"])
        record["file_encoding"] = "decimal integer"
        if codec_kind == "number":
            record["file_encoding"] = "decimal floating point holding an integral value"
            record["notes"].append("UI value set is an integer range but the persisted codec is Codec.DOUBLE"
                                   + (" via xmap" if xmap else ""))
    elif value_set and value_set["kind"] in ("unit_double", "double_range"):
        record["type"] = "number"
        if None in (value_set.get("min"), value_set.get("max")):
            record["notes"].append("the decimal range bounds could not be read, so the domain is unproven")
        else:
            record["numeric"] = {"min": value_set["min"], "max": value_set["max"],
                                 "step": None, "clamped": True}
        record["file_encoding"] = "decimal floating point"
    elif codec_kind:
        record["type"] = codec_kind
        record["file_encoding"] = {"integer": "decimal integer", "number": "decimal floating point",
                                   "boolean": "lowercase true/false",
                                   "string": "UTF-8 text after the first colon"}[codec_kind]

    if codec_range:
        record["numeric"] = {"min": codec_range[0], "max": codec_range[1],
                             "step": 1 if record["type"] == "integer" else stored_step,
                             "clamped": True}
    # Chained arithmetic xmaps are not composed, so they count as unread.
    if len(xmap_steps) > 1:
        xmap_unresolved = True
    xmap_ops = xmap_steps[0] if len(xmap_steps) == 1 and not xmap_unresolved else None
    record["value_set"] = value_set
    record["codec"] = {"kind": codec_kind, "xmap": xmap, "explicit_range": codec_range,
                       "transform_resolved": None if not xmap else not xmap_unresolved}
    if xmap_ops:
        record["value_to_storage"] = formula(xmap_ops, "value")
        record["storage_to_value"] = formula(invert_ops(xmap_ops), "file")
        record["transform_ops"] = xmap_ops
        record["notes"].append(
            f"the persisted value is the UI value through {record['value_to_storage']}")
    elif xmap_unresolved:
        # The codec transforms the value on its way to the file and this
        # analyzer could not read how. Saying nothing here is what lets a
        # generator assume the value is stored as-is, which is worse than
        # admitting the setting is not understood.
        record["notes"].append("the persisted codec transforms the value through an xmap this "
                               "analyzer could not read")
    record["default"] = default_value
    record["default_confidence"] = default_conf
    if default_value is not None:
        # An enum default is already persisted; any other goes through the codec transform.
        in_ui_space = (xmap_steps or xmap_unresolved) and not record.get("enum_default_constant")
        record["default_space"] = "ui" if in_ui_space else "file"
    if default_conditional and default_value is None:
        record["notes"].append(CONDITIONAL_DEFAULT_NOTE)
    if record["type"] == "unknown" and isinstance(default_value, bool):
        record["type"] = "boolean"
        record["file_encoding"] = "lowercase true/false"
    return record


# ---------------------------------------------------------------------------
# Key bindings
# ---------------------------------------------------------------------------

def input_type_of(dis: Disassembler, owner: str, field: str) -> str | None:
    """Resolve `InputConstants.Type.<X>` through the enum's own constant names."""
    info = extract_enum(dis, owner)
    if not info:
        return None
    for constant in info["constants"]:
        if constant.get("field") == field or constant["name"] == field:
            return constant["name"].lower()
    return None


UNKNOWN_KEY_NAME = "key.keyboard.unknown"


def registers_unknown_key(dis: Disassembler, type_class: str, type_field: str) -> bool:
    """True when the key type enum registers the unknown key name for code -1."""
    for member in parse_members(dis.get(type_class)):
        insns = member.insns
        for i in range(1, len(insns) - 1):
            previous = insns[i - 1].field()
            if (insns[i].string() == UNKNOWN_KEY_NAME and insns[i - 1].op == "getstatic"
                    and previous and previous[1] == type_field and previous[0] in (type_class, None)
                    and const_int(insns[i + 1]) == -1):
                return True
    return False


def is_unbound_key_constant(dis: Disassembler, owner: str, field: str) -> bool:
    """True when a static `Key` field is the unbound key, read from its own initializer.

    The field is created once, unconditionally, from a key type constant and the
    literal -1, and that type registers -1 under the unknown key name. Neither the
    owner nor the field name is trusted, because releases are obfuscated; the
    unknown key name itself lives in the key type enum, not in the field's owner.
    """
    clinit = static_initializer(parse_members(dis.get(owner)))
    if clinit is None:
        return False
    insns = clinit.insns
    stores = [i for i, ins in enumerate(insns)
              if ins.op == "putstatic" and (fld := ins.field()) and fld[1] == field]
    if len(stores) != 1 or stores[0] < 3:
        return False
    store = stores[0]
    stored, create, value, type_const = (insns[store].field(), insns[store - 1].method(),
                                         const_int(insns[store - 2]), insns[store - 3].field())
    if (insns[store - 1].op != "invokevirtual" or not stored or not create or create[2] != f"(I){stored[2]}"
            or value != -1 or insns[store - 3].op != "getstatic" or not type_const
            or not runs_unconditionally(insns[store - 3:store + 1])):
        return False
    return registers_unknown_key(dis, type_const[0] or owner, type_const[1])


def extract_keybinds(dis: Disassembler, members: list[Member], legacy_codes: bool,
                     option_class: str | None) -> list[dict[str, Any]]:
    out: list[dict[str, Any]] = []
    seen: set[str] = set()
    for m in members:
        insns = m.insns
        for i, ins in enumerate(insns):
            name = ins.string()
            if name is None or not name.startswith("key.") or name.startswith("key.categories."):
                continue
            ctor_at = None
            for j in range(i + 1, min(i + 25, len(insns))):
                ref = insns[j].method()
                if (ref and ref[1] == "<init>" and ref[2].startswith("(Ljava/lang/String;")
                        and ref[0] != option_class):
                    ctor_at = j
                    break
                other = insns[j].string()
                if (other is not None and other.startswith("key.")
                        and not other.startswith("key.categories.")):
                    break
            if ctor_at is None or name in seen:
                continue
            ref = insns[ctor_at].method()
            desc = ref[2]
            body = insns[i + 1:ctor_at]
            params = split_descriptor(desc)[0]
            # The last constructor argument is the binding category, either as a
            # `key.categories.*` string or as a category enum constant.  The
            # argument region ends there, before any trailing supplier plumbing.
            # Plain key mappings end with a `key.categories.*` string; newer and
            # toggle variants pass a category enum, which is the last parameter of
            # its own class type (trailing suppliers and flags are java types).
            category_type = None
            if params and params[-1] != "Ljava/lang/String;":
                category_type = next((x for x in reversed(params[1:])
                                      if x.startswith("L") and not x.startswith("Ljava/")), None)
            region: list[Insn] = []
            category = None
            category_source = None
            for ins2 in body:
                text2 = ins2.string()
                if text2 is not None and text2.startswith("key.categories."):
                    category = text2[len("key.categories."):]
                    break
                cm = re.search(r"Category\.([A-Z_]+)", ins2.comment)
                if cm:
                    category = cm.group(1).lower()
                    break
                fld2 = ins2.field()
                if (category_type and ins2.op == "getstatic" and fld2 and fld2[0]
                        and fld2[2] == category_type):
                    category = input_type_of(dis, fld2[0], fld2[1]) or "<" + fld2[1] + ">"
                    category_source = f"{fld2[0]}.{fld2[1]}"
                    break
                region.append(ins2)
            # Several toggle options reuse a binding's caption (key.sneak,
            # key.attack, ...).  Only a key-mapping constructor carries a binding
            # category, so that is what distinguishes the two constructions.
            if category is None:
                continue
            input_type = "keysym"
            unbound = False
            for ins2 in region:
                fld = ins2.field()
                if ins2.op != "getstatic" or not fld or not fld[0]:
                    continue
                if len(params) > 1 and params[1] == fld[2] and fld[2] != "Ljava/lang/String;":
                    resolved = input_type_of(dis, fld[0], fld[1])
                    if resolved in ("mouse", "keysym", "scancode"):
                        input_type = resolved
                    continue
                # A constant Key object rather than an integer argument: the default
                # is the unbound key when that constant is created as the unknown key.
                if is_unbound_key_constant(dis, fld[0], fld[1]):
                    unbound = True
            numbers = [v for v in (const_int(x) for x in region) if v is not None]
            code = numbers[-1] if numbers else None
            if unbound:
                code = -1
            serialized = None
            confidence = "medium"
            encoding = None
            if legacy_codes:
                encoding = "legacy LWJGL2 numeric key code"
                if code is not None:
                    serialized = str(code)
                    confidence = "high"
            else:
                encoding = "InputConstants key name string (for example key.keyboard.w)"
                if code == -1:
                    serialized, confidence = "key.keyboard.unknown", "high"
                elif input_type == "mouse" and code is not None:
                    serialized = "key.mouse." + MOUSE_NAMES.get(code, str(code + 1))
                    confidence = "high"
                elif code is not None and glfw_name(code):
                    serialized = "key.keyboard." + glfw_name(code)
                    confidence = "high"
            out.append({"translation_key": name, "persisted_key": "key_" + name,
                        "input_type": "mouse" if input_type == "mouse" else "keyboard",
                        "code": code, "serialized": serialized, "unbound": unbound,
                        "binding_category": None if (category or "").startswith("<") else category,
                        "binding_category_source": category_source or category,
                        "file_encoding": encoding,
                        "constructor_descriptor": desc,
                        "note": (None if code is not None else
                                 "the default is supplied as a constant Key object rather than an integer "
                                 "(InputConstants.UNKNOWN in named builds, i.e. unbound); no integer "
                                 "constant is present at the construction site to prove it"),
                        "confidence": confidence,
                        "codec_confidence": "high"})
            seen.add(name)
    out.sort(key=lambda x: x["translation_key"])
    return out


# Defaults no analyzed release has reassigned; they prove which table a release's codes use.
KEY_CODE_ANCHORS = {"key.forward": "w", "key.left": "a", "key.back": "s", "key.right": "d",
                    "key.jump": "space", "key.inventory": "e", "key.drop": "q", "key.chat": "t",
                    "key.sneak": "left.shift"}
MOUSE_CODE_ANCHORS = {"key.attack": "left", "key.use": "right", "key.pickItem": "middle"}
MIN_KEY_CODE_ANCHORS = 5
UNVERIFIED_CODE_SPACE = "unverified"


def verify_key_code_space(keybinds: list[dict[str, Any]], assumed: str) -> dict[str, Any]:
    """Check that the assumed code table decodes the anchor defaults to their keys."""
    table = LWJGL2_KEYS if assumed == "lwjgl2" else GLFW
    codes = {name: code for code, name in table.items()}
    mouse_codes = {name: code for code, name in MOUSE_NAMES.items()}
    expected: dict[str, tuple[str, int]] = {
        binding: ("keyboard", codes[key]) for binding, key in KEY_CODE_ANCHORS.items()}
    if assumed == "glfw":
        expected.update({binding: ("mouse", mouse_codes[button])
                         for binding, button in MOUSE_CODE_ANCHORS.items()})
    by_name = {kb["translation_key"]: kb for kb in keybinds}
    proven, mismatches = 0, []
    for binding, (input_type, code) in sorted(expected.items()):
        kb = by_name.get(binding)
        if kb is None or kb["code"] is None or kb["unbound"]:
            continue
        if (kb["input_type"], kb["code"]) == (input_type, code):
            proven += 1
        else:
            mismatches.append({"binding": binding, "input_type": kb["input_type"], "code": kb["code"],
                               "expected_input_type": input_type, "expected_code": code})
    if mismatches:
        status = "mismatch"
    elif proven < MIN_KEY_CODE_ANCHORS:
        status = "unproven"
    else:
        status = "verified"
    return {"assumed": assumed, "status": status, "anchors_proven": proven,
            "anchors_required": MIN_KEY_CODE_ANCHORS, "mismatches": mismatches}


def apply_key_code_check(keybinds: list[dict[str, Any]], check: dict[str, Any]) -> str:
    """The code space to record, with every binding of an unverified release demoted."""
    if check["status"] == "verified":
        return check["assumed"]
    reason = ("the default codes do not decode to the expected keys as " + check["assumed"] + " codes"
              if check["status"] == "mismatch"
              else "too few well-known default bindings were readable to prove the key code table")
    for kb in keybinds:
        kb["serialized"] = None
        kb["confidence"] = kb["codec_confidence"] = "medium"
        kb["note"] = f"the key code space is not verified: {reason}"
    return UNVERIFIED_CODE_SPACE


# ---------------------------------------------------------------------------
# Dynamic prefixed families (sound categories, model parts)
# ---------------------------------------------------------------------------

def apply_ops(value: int | float, ops: list[dict[str, Any]]) -> int | float | None:
    """Run a recovered transform over `value`.

    An integer stays an integer while every step leaves it integral. None means
    the transform cannot be applied (a division by zero, an operation this
    analyzer does not know), never a partly applied result.
    """
    out = float(value)
    for step in ops:
        operand = step["operand"]
        if step["op"] == "+":
            out += operand
        elif step["op"] == "-":
            out -= operand
        elif step["op"] == "*":
            out *= operand
        elif step["op"] == "/" and operand != 0:
            out /= operand
        else:
            return None
    out = round(out, 6)
    return int(out) if isinstance(value, int) and not isinstance(value, bool) and out.is_integer() else out


def jvm_default(descriptor: str | None, ops: list[dict[str, Any]]) -> bool | int | float | None:
    """The value a field of this type holds before anything assigns it, after
    the storage transform; None when the type has no such default or the
    transform cannot be applied."""
    initial = {"Z": False, "I": 0, "J": 0, "F": 0.0, "D": 0.0}.get(descriptor or "")
    if initial is None or isinstance(initial, bool):
        return initial
    return apply_ops(initial, ops)


CONDITIONAL_DEFAULT_NOTE = ("the default is chosen by a condition or assigned more than once, "
                            "so no default is proven")


def field_initial_values(members: list[Member]) -> dict[str, dict[str, Any]]:
    """Recover `this.field = <constant>` initialisers from the options constructor.

    Older releases keep their defaults in field initialisers rather than in a
    codec, so this is where a pre-OptionInstance default actually lives.
    """
    ctor = None
    best = 0
    for m in members:
        count = sum(1 for ins in m.insns if ins.op == "putfield")
        if count > best:
            ctor, best = m, count
    if ctor is None:
        return {}
    out: dict[str, dict[str, Any]] = {}
    writes: dict[str, int] = defaultdict(int)
    for i, ins in enumerate(ctor.insns):
        fld = ins.field()
        if ins.op != "putfield" or not fld:
            continue
        writes[fld[1]] += 1
        if i == 0:
            continue
        previous = ctor.insns[i - 1]
        if previous.op == "getstatic":
            src = previous.field()
            if src and src[0] and src[2] == f"L{src[0]};":
                out[fld[1]] = {"kind": "enum_constant", "enum_class": src[0], "constant": src[1],
                               "descriptor": fld[2]}
            else:
                continue
        elif (text := previous.string()) is not None:
            out[fld[1]] = {"kind": "string", "value": text, "descriptor": fld[2]}
        elif (number := const_num(previous)) is not None:
            out[fld[1]] = {"kind": "number", "value": number, "descriptor": fld[2]}
        else:
            continue
        if not runs_unconditionally([previous, ins]):
            out[fld[1]] = {"kind": "conditional", "descriptor": fld[2]}
    for name, count in writes.items():
        if count > 1 and name in out:
            out[name] = {"kind": "conditional", "descriptor": out[name]["descriptor"]}
    return out


def resolve_default(record: dict[str, Any], initial: dict[str, Any] | None,
                    dis: Disassembler) -> tuple[Any, str, str | None]:
    """Turn an in-memory field initialiser into the value that lands in the file."""
    if initial is None:
        return None, "unknown", None
    if initial["kind"] == "conditional":
        return None, "unknown", CONDITIONAL_DEFAULT_NOTE
    descriptor = initial.get("descriptor") or record.get("field_descriptor")
    ops = record.get("transform_ops") or []
    if initial["kind"] == "string":
        return initial["value"], "high", None
    if record.get("type") == "boolean" and descriptor in NON_BOOLEAN_PRIMITIVES:
        return None, "unknown", (f"the backing field has type {descriptor}, which a boolean cannot "
                                 "default from, so no default is proven")
    allowed = record.get("allowed_values") or {}
    if (initial["kind"] == "number" and not record.get("enum_class")
            and allowed.get("persisted_form") == "string_token"):
        tokens = [v["persisted"] for v in allowed["values"] if v.get("id") == initial["value"]]
        if len(tokens) != 1:
            return None, "unknown", f"the default {initial['value']:g} names no single persisted token"
        return tokens[0], "high", None
    if initial["kind"] == "enum_constant":
        enum_class = initial["enum_class"]
        allowed = record.get("allowed_values") or {}
        form = allowed.get("persisted_form") or record.get("enum_persisted_form") or "unknown"
        info = extract_enum(dis, enum_class)
        if not info or form == "unknown":
            return None, "unknown", (f"default enum constant {initial['constant']} is known but its "
                                     "persisted form is unresolved")
        value = enum_persisted_value(info, initial["constant"], form, record.get("enum_token_map"))
        if value is None:
            return None, "unknown", (f"default enum constant {initial['constant']} has no resolvable "
                                     "persisted representation")
        return value, "high", None
    value = initial["value"]
    if descriptor == "Z":
        return bool(value), "high", None
    note = None
    if ops:
        stored = apply_ops(value, ops)
        if stored is None:
            return None, "unknown", (f"the in-memory default is {value:g} but its storage transform "
                                     "cannot be applied, so no default is proven")
        note = (f"the in-memory default is {value:g} but the value written to options.txt is "
                f"{stored:g} because of the storage transform")
        value = stored
    if descriptor in ("I", "J") and float(value).is_integer():
        return int(value), "high", note
    if descriptor in ("F", "D") or not float(value).is_integer():
        return float(value), "high", note
    return int(value), "high", note


SOUND_MAP_RE = re.compile(
    r"^\s+(?:private|public|protected).*\b(?:Map|Object2FloatMap|Object2IntMap)"
    r"<\s*([\w./$]+)\s*[,>]")
PART_SET_RE = re.compile(r"^\s+(?:private|public|protected).*\bSet<\s*([\w./$]+)\s*>")


def field_generic_types(text: str) -> tuple[str | None, str | None]:
    """Recover the sound-source and model-part enums from the root's field declarations.

    `Options` always keeps a `Map<SoundSource, ...>` of category volumes and a
    `Set<PlayerModelPart>` of enabled skin layers.  Generic signatures survive
    obfuscation, so the element types identify both families without relying on
    any method or class name.
    """
    sound = part = None
    for line in text.splitlines():
        if line.strip().startswith(("public", "private", "protected")) and "(" in line:
            continue
        m = SOUND_MAP_RE.match(line)
        if m and sound is None and not m.group(1).startswith("java."):
            sound = m.group(1).replace(".", "/")
        m = PART_SET_RE.match(line)
        if m and part is None and not m.group(1).startswith("java."):
            part = m.group(1).replace(".", "/")
    return sound, part


def legacy_ui_ranges(dis: Disassembler, ui_classes: list[str]) -> dict[str, Any]:
    """Recover slider bounds from the pre-1.19 option-definition classes.

    Through 1.16 these are `GameSettings.Options` enum constants; 1.17 and 1.18
    use `ProgressOption` objects.  Both spell the option out as
    `<translation key>, <min>, <max>, <step>` in a constructor call.  Neither form
    names the persisted key, so entries are linked only by the exact string
    identity `translation_key == "options." + persisted key`; no semantic alias is
    inferred, and the result is kept separate from the proven `numeric` domain.
    """
    out: dict[str, Any] = {}
    for cls in ui_classes:
        text = dis.get(cls)
        if not text:
            continue
        for member in parse_members(text):
            floats: list[float] = []
            translation = None
            constant = None
            for ins in member.insns:
                s = ins.string()
                if s is not None and s.startswith("options.") and s.count(".") == 1:
                    translation, floats = s, []
                    continue
                if translation is None:
                    continue
                if ins.op.startswith("ldc") and M_FLOAT.match(ins.comment):
                    floats.append(float(M_FLOAT.match(ins.comment).group(1)))
                elif ins.op in ("fconst_0", "fconst_1", "fconst_2", "dconst_0", "dconst_1"):
                    floats.append(float(ins.op[-1]))
                fld = ins.field()
                if ins.op == "putstatic" and fld:
                    constant = fld[1]
                ref = ins.method()
                if ref and ref[1] == "<init>" and len(floats) >= 3:
                    key = translation[len("options."):]
                    lo, hi, step = floats[0], floats[1], floats[2]
                    if lo <= hi:
                        out.setdefault(key, {"translation_key": translation, "source_class": cls,
                                             "source_member": member.signature,
                                             "enum_constant": constant,
                                             "ui_min": lo, "ui_max": hi, "ui_step": step})
                    translation, floats = None, []
    return out


def dynamic_family(dis: Disassembler, prefix: str, enum_class: str | None,
                   constants: dict[str, list[str]], extra: dict[str, Any] | None = None
                   ) -> dict[str, Any] | None:
    present = any(any(s.startswith(prefix) for s in values) for values in constants.values())
    if not present:
        return None
    names: list[str] = []
    if enum_class:
        info = extract_enum(dis, enum_class)
        if info:
            names = [c["serialized_name"] for c in info["constants"] if c["serialized_name"]]
    record = {"prefix": prefix, "enum_class": enum_class, "names": names,
              "confidence": "high" if names else "medium",
              "evidence": "prefix literal present in the options root constant pool; member names come from "
                          "the enum reached through the root's own generic field signature"}
    if extra:
        record.update(extra)
    return record


def sound_default_volume(members: list[Member], sound_enum: str | None) -> dict[str, Any]:
    """Prove the 1.0 fallback used when a category has no stored volume."""
    if not sound_enum:
        return {"default": None, "confidence": "unknown"}
    for m in members:
        params, ret = split_descriptor(m.signature.split(" ")[-1] if "(" in m.signature else "")
        touches_enum = f"L{sound_enum};" in m.signature.replace(".", "/") or any(
            (ins.field() or ("", "", ""))[2] == f"L{sound_enum};" for ins in m.insns)
        if not touches_enum and sound_enum.replace("/", ".") not in m.signature:
            continue
        has_lookup = any((ins.method() or ("", "", ""))[1] in ("get", "getOrDefault", "containsKey",
                                                              "getFloat", "valueOf")
                         for ins in m.insns)
        ones = [ins for ins in m.insns if const_num(ins) == 1.0
                and ins.op in ("fconst_1", "dconst_1", "ldc", "ldc_w", "ldc2_w")]
        if has_lookup and ones:
            return {"default": 1.0, "confidence": "high",
                    "evidence": f"absent-category fallback constant 1.0 in {m.signature}"}
    return {"default": None, "confidence": "unknown"}


# ---------------------------------------------------------------------------
# Per-JAR analysis
# ---------------------------------------------------------------------------

def plausible_key(s: str) -> bool:
    if not s or len(s) > 80 or " " in s or s.startswith(("options.", "key.categories.")):
        return False
    if s in {"options.txt", "true", "false", "null", "UTF-8", ":"}:
        return False
    return bool(re.fullmatch(r"[A-Za-z][A-Za-z0-9_.-]*", s))


def roots_and_constants(zf: ZipFile) -> tuple[list[str], dict[str, list[str]], set[str], list[str]]:
    roots, constants = [], {}
    all_classes = set()
    ui_candidates: list[tuple[int, str]] = []
    for name in zf.namelist():
        if not name.endswith(".class"):
            continue
        cls = name[:-6]
        if not is_safe_class_name(cls):
            continue
        all_classes.add(cls)
        data = zf.read(name)
        is_root = b"options.txt" in data
        # Option definitions (the pre-1.19 Options enum or ProgressOption table)
        # carry many distinct `options.<name>` captions.  The cheap byte count
        # gates the much more expensive constant-pool walk.
        dense = data.count(b"options.") >= 8
        if not is_root and not dense:
            continue
        strings = cp_utf8(data)
        if is_root:
            roots.append(cls)
            constants[cls] = strings
        if dense:
            count = len({s for s in strings if s.startswith("options.") and s.count(".") == 1})
            if count >= 8:
                ui_candidates.append((count, cls))
    ui_candidates.sort(key=lambda x: (-x[0], x[1]))
    return sorted(roots), constants, all_classes, [c for _, c in ui_candidates[:6]]


DATA_VERSION_LITERAL_RE = re.compile(r"version:([0-9]+)")


def data_version_marker(constants: dict[str, list[str]], version_json: bytes | None) -> dict[str, Any] | None:
    """The `version:` data version a release stamps into options.txt, or None if it writes none.

    version.json names it from 1.14; older releases compile a `version:<n>` literal into the root.
    """
    if not any(any(s.startswith("version:") for s in values) for values in constants.values()):
        return None
    marker = {"persisted_key": "version", "value": None, "encoding": "version:<data version integer>",
              "confidence": "unknown", "evidence": "no integer data version was found in version.json or as "
                                                   "a version:<n> literal in the root class"}
    if version_json is not None:
        try:
            world_version = json.loads(version_json).get("world_version")
        except (ValueError, AttributeError):
            world_version = None
        if isinstance(world_version, int) and not isinstance(world_version, bool):
            marker.update(value=world_version, confidence="high",
                          evidence="world_version in version.json, which the root stamps as version:<value>")
            return marker
    literals = {(int(m.group(1)), cls) for cls, values in constants.items() for s in values
                if (m := DATA_VERSION_LITERAL_RE.fullmatch(s))}
    if len({value for value, _ in literals}) == 1:
        value, cls = sorted(literals)[0]
        marker.update(value=value, confidence="medium",
                      evidence=f"version:{value} literal in the constant pool of {cls}")
    return marker


MINIMUM_PERSISTED_OPTIONS = 40


def extraction_gaps(analysis: dict[str, Any]) -> list[str]:
    """What an options analysis failed to find; empty only for a complete one.

    A release is complete when every population every supported client has was
    found and no class it was read from failed to disassemble. Completeness is
    what lets a setting absent from the release count as unsupported instead of
    unknown, so a partial result must never satisfy it.
    """
    gaps = []
    if not analysis.get("roots"):
        gaps.append("no options root class")
    if len(analysis.get("options") or {}) < MINIMUM_PERSISTED_OPTIONS:
        gaps.append(f"fewer than {MINIMUM_PERSISTED_OPTIONS} persisted options")
    if not analysis.get("keybinds"):
        gaps.append("no key bindings")
    for label, family in (("sound category", "sound_family"), ("model part", "model_part_family")):
        if not (analysis.get(family) or {}).get("names"):
            gaps.append(f"no {label} family members")
    errors = analysis.get("javap_errors") or []
    if errors:
        gaps.append(f"javap failed on {len({e.get('class') for e in errors})} class(es)")
    return gaps


def analyze_jar(path: Path, root: Path) -> dict[str, Any]:
    digest = sha256(path)
    base: dict[str, Any] = {"path": jar_path_in_root(path, root), "filename": path.name,
                            "size": path.stat().st_size, "sha256": digest}
    try:
        zf = ZipFile(path)
    except BadZipFile as exc:
        base.update({"jar_id": jar_id(None, digest), "detected_version": None,
                     "version_confidence": "unknown", "stable_release": False, "version_evidence": [],
                     "error": str(exc),
                     "options_analysis": {"roots": [], "persisted_pairs": [], "extraction_complete": False}})
        return base
    with zf:
        vd = detect_version(path, zf)
        roots, constants, all_classes, ui_classes = roots_and_constants(zf)
        base.update({"detected_version": vd["version"], "version_confidence": vd["confidence"],
                     "stable_release": vd["stable_release"], "version_evidence": vd["evidence"]})
        base["jar_id"] = jar_id(vd["version"], digest)
        dis = Disassembler(path, all_classes)
        members: list[Member] = []
        root_text = ""
        for cls in roots:
            text = dis.get(cls)
            root_text += text
            members.extend(parse_members(text))
        era, option_class = detect_era(members)
        version = vd["version"]
        legacy_codes = is_release(version) and version_key(version) < version_key("1.13")

        options: dict[str, dict[str, Any]] = {}
        if era == "legacy_direct":
            options = extract_legacy(members)
            for record in options.values():
                save_body = record.pop("save_body", None) or []
                if not record.get("enum_class"):
                    if record.pop("literal_tokens_proven", False):
                        record["allowed_values"] = literal_token_domain(record)
                    continue
                proven = record.pop("literal_tokens_proven", False)
                info = extract_enum(dis, record["enum_class"])
                if not info:
                    continue
                form = record.get("enum_persisted_form") or "unknown"
                tokens = dict(record.get("enum_token_map") or {})
                if proven:
                    tokens.update(literal_enum_tokens(record))
                if form == "string_token":
                    bindings, leftovers, understood = token_bindings(save_body, record["enum_class"])
                    tokens.update(bindings)
                    tokens = complete_token_map(info, tokens, leftovers)
                    if not understood:
                        record["domain_confidence"] = "low"
                        record.setdefault("notes", []).append(
                            "a token comparison could not be read, so which constant a token names is a guess")
                    if not all(v["persisted"] for v in enum_domain(info, form, tokens)["values"]):
                        form = "unknown"
                        record["enum_persisted_form"] = "unknown"
                        record.setdefault("notes", []).append(
                            "the enum is serialised as a string but no static source names every token")
                record["enum_token_map"] = tokens or None
                record["allowed_values"] = enum_domain(info, form, tokens)
                record["file_encoding"] = {
                    "integer_id": "enum id integer",
                    "string_token": "enum serialized string token"}.get(form, "enum, codec unresolved")
        else:
            visitor = extract_visitor(dis, members, option_class)
            defs = (extract_option_instances(dis, members, option_class, roots[0] if roots else None)
                    if option_class else {})
            for key, record in visitor.items():
                merged = dict(record)
                if record.get("kind") == "option_instance" and record.get("field") in defs:
                    definition = defs[record["field"]]
                    merged.update({k: v for k, v in definition.items() if k != "field"})
                    merged["source"] = "OptionInstance definition joined to visitor key"
                options[key] = merged
            options = {k: v for k, v in sorted(options.items())}
            # String-serialised enums build their tokens in an inline lambda, which
            # the compiler emits as a synthetic `<Enum> -> String` method on the
            # options class itself.  That method is the static source of the tokens.
            for record in options.values():
                allowed = record.get("allowed_values")
                if not allowed or allowed.get("persisted_form") != "string_token":
                    continue
                if allowed.get("values") and all(v.get("persisted") for v in allowed["values"]):
                    continue
                enum_class = allowed.get("source_enum_class")
                info = extract_enum(dis, enum_class) if enum_class else None
                if not info:
                    continue
                insns, single_lambda = enum_string_lambda(members, enum_class)
                bindings, leftovers, understood = (token_bindings(insns, enum_class)
                                                   if insns else ({}, [], True))
                tokens = complete_token_map(info, bindings, leftovers) if insns else {}
                domain = enum_domain(info, "string_token", tokens)
                if tokens and all(v["persisted"] for v in domain["values"]):
                    record["enum_token_map"] = tokens
                    record["allowed_values"] = domain
                    record["file_encoding"] = "enum serialized string token"
                    if not (understood and single_lambda):
                        record["domain_confidence"] = "low"
                        record.setdefault("notes", []).append(
                            "the token lambda is ambiguous or has a comparison that could not be read, "
                            "so which constant a token names is a guess")
                    constant = record.get("enum_default_constant")
                    if record.get("default") is None and constant:
                        value = enum_persisted_value(info, constant, "string_token", tokens)
                        if value is not None:
                            record["default"] = value
                            record["default_confidence"] = "high"
                            record["notes"] = [n for n in (record.get("notes") or [])
                                               if "persisted form" not in n]
                else:
                    record["allowed_values"] = enum_domain(info, "unknown", tokens)
                    record["file_encoding"] = "enum, codec unresolved"
                    record.setdefault("notes", []).append(
                        "the enum is serialised as a string but no static source names every token")

        # Defaults that live in a field initialiser rather than a codec, plus the
        # transform that converts them into what is actually written to the file.
        initials = field_initial_values(members)
        for record in options.values():
            if record.get("default") is not None or not record.get("field"):
                continue
            initial = initials.get(record["field"])
            value, conf, note = resolve_default(record, initial, dis)
            if value is not None or note:
                record["default"] = value
                record["default_confidence"] = conf
                record["default_source"] = "constructor field initialiser"
                if note:
                    record.setdefault("notes", []).append(note)
                continue
            if initial is None:
                # No initialiser was compiled into the constructor, so the field
                # starts at the JVM default for its type.  That is only true if
                # nothing else assigns it before the first save, which cannot be
                # proven here, so it is offered as a candidate and never as fact.
                stored = jvm_default(record.get("field_descriptor"), record.get("transform_ops") or [])
                if stored is None:
                    continue
                record["default_candidate"] = stored
                record["default_candidate_confidence"] = "low"
                record.setdefault("notes", []).append(
                    "no constructor initialiser was compiled for this field, so the JVM default for its "
                    f"type ({stored!r} after any storage transform) is the likely written default; this is "
                    "a candidate, not a proven fact")

        if era != "option_instance":
            for key, ui in legacy_ui_ranges(dis, ui_classes).items():
                record = options.get(key)
                if record is None:
                    continue
                record.setdefault("translation_key", ui["translation_key"])
                if "ui_min" not in ui:
                    continue
                ops = record.get("transform_ops") or []
                file_min, file_max = apply_ops(ui["ui_min"], ops), apply_ops(ui["ui_max"], ops)
                if file_min is None or file_max is None:
                    continue
                record["ui_domain"] = {
                    "ui_min": ui["ui_min"], "ui_max": ui["ui_max"], "ui_step": ui["ui_step"],
                    "file_min": min(file_min, file_max), "file_max": max(file_min, file_max),
                    "confidence": "medium",
                    "source": f"{ui['source_class']}.{ui['enum_constant']} slider constants",
                    "link_rule": "linked by the exact string identity translation_key == 'options.' + "
                                 "persisted key; no semantic alias was inferred",
                    "note": "these are the slider bounds enforced by the settings screen, transformed into "
                            "file units; the loader itself does not clamp"}

        keybinds = extract_keybinds(dis, members, legacy_codes, option_class)
        code_check = verify_key_code_space(keybinds, "lwjgl2" if legacy_codes else "glfw")
        code_space = apply_key_code_check(keybinds, code_check)
        sound_enum, part_enum = field_generic_types(root_text)
        sounds = dynamic_family(dis, "soundCategory_", sound_enum, constants,
                                sound_default_volume(members, sound_enum))
        model_parts = dynamic_family(dis, "modelPart_", part_enum, constants)

        version_json = zf.read("version.json") if "version.json" in zf.namelist() else None
        schema_marker = data_version_marker(constants, version_json)

        root_strings = sorted({s for values in constants.values() for s in values
                               if plausible_key(s.rstrip(":"))})
        base["options_analysis"] = {
            "roots": roots,
            "root_evidence": [{"class": c, "literal": "options.txt", "confidence": "high",
                               "description": "Literal in class constant pool"} for c in roots],
            "persistence_era": era,
            "option_instance_class": option_class,
            "keybinding_code_space": code_space,
            "keybinding_code_space_check": code_check,
            "persisted_pairs": [{"key": k, "field": v.get("field"), "descriptor": v.get("field_descriptor"),
                                 "type": v.get("type", "unknown"), "confidence": "high"}
                                for k, v in sorted(options.items())],
            "options": options,
            "keybinds": keybinds,
            "sound_family": sounds,
            "model_part_family": model_parts,
            "root_constant_candidates": root_strings,
            "javap_errors": dis.errors,
            "javap_calls": dis.calls,
            "dynamic_prefixes": {prefix: any(any(s.startswith(prefix) for s in values)
                                            for values in constants.values())
                                 for prefix in ("key_", "soundCategory_", "modelPart_")},
            "schema_marker": schema_marker,
        }
        gaps = extraction_gaps(base["options_analysis"])
        base["options_analysis"]["extraction_gaps"] = gaps
        base["options_analysis"]["extraction_complete"] = not gaps
        return base


# ---------------------------------------------------------------------------
# Normalisation
# ---------------------------------------------------------------------------

SPECIAL_KEYS = {
    "resourcePacks": ("JSON array of pack identifiers written on one line by Gson",
                      "List<String> serialised with Gson; order is significant and entries are validated "
                      "against the local pack repository at load time."),
    "incompatibleResourcePacks": ("JSON array of pack identifiers written on one line by Gson",
                                  "List<String> serialised with Gson; mirrors resourcePacks and is rewritten "
                                  "by the client whenever pack compatibility changes."),
    "fullscreenResolution": ("free-form monitor/video-mode descriptor string",
                             "Serialised VideoMode descriptor tied to the local monitor; the exact value is "
                             "hardware dependent and is re-derived by the client."),
    "tutorialStep": ("tutorial state token", "Client tutorial progress state, not a user preference."),
    "lastServer": ("free-form server address", "Last multiplayer address; session state, not a preference."),
    "startedCleanly": ("lowercase true/false", "Crash-detection flag owned by the client."),
    "joinedFirstServer": ("lowercase true/false", "One-shot onboarding flag owned by the client."),
    "version": ("data version integer", "Schema marker written by the client."),
    "soundDevice": ("free-form audio device name or empty for the system default",
                    "Device identity is environment dependent."),
}

UNSAFE_FOR_EPHERVEX = {"resourcePacks", "incompatibleResourcePacks", "fullscreenResolution",
                       "tutorialStep", "lastServer", "startedCleanly", "joinedFirstServer",
                       "version", "soundDevice"}


def category_for(key: str, field: str | None) -> str:
    s = (key + " " + (field or "")).lower()
    if key.startswith("soundCategory_") or any(x in s for x in ("sound", "music", "subtitles", "audio")):
        return "Sound & Music"
    if key.startswith("key_") or any(x in s for x in ("mouse", "sensitivity", "toggle", "sprintwindow")):
        return "Controls"
    if key == "lang" or any(x in s for x in ("language", "unicode", "glyph")):
        return "Language / Accessibility"
    if any(x in s for x in ("render", "graphics", "fullscreen", "fov", "gamma", "cloud", "particle",
                            "mipmap", "vsync", "framerate", "gui", "vignette", "biome", "weather",
                            "transparency", "anisotrop", "texture", "screen")):
        return "Video"
    if any(x in s for x in ("chat", "narrator", "contrast", "notification", "accessib", "lightning",
                            "splash", "background")):
        return "Language / Accessibility"
    if key.startswith("modelPart_"):
        return "Gameplay"
    return "Gameplay"


def written_quoting(setting: dict[str, Any]) -> str:
    """Whether the file holds this setting's value inside quotes.

    `file_quoting` also separates raw text from JSON that prints the same bytes
    (a boolean or a number), which is not a difference in what is written.
    """
    return "quoted" if setting.get("file_quoting") == "json_string" else "bare"


def representation(setting: dict[str, Any]) -> str:
    """Stable fingerprint of everything that matters for read/write compatibility."""
    numeric = setting.get("numeric") or {}
    allowed = setting.get("allowed_values") or {}
    domain = ""
    if allowed.get("values"):
        domain = allowed.get("persisted_form", "?") + ":" + ",".join(
            sorted(str(v.get("persisted")) for v in allowed["values"]))
    elif allowed:
        domain = allowed.get("persisted_form", "?")
    return "|".join([
        setting.get("type") or "unknown",
        setting.get("file_encoding") or "?",
        written_quoting(setting),
        str(setting.get("value_to_storage") or ""),
        str(setting.get("storage_to_value") or ""),
        "unread" if setting.get("transform_resolved") is False else "",
        f"{numeric.get('min')}..{numeric.get('max')}/{numeric.get('step')}/{numeric.get('clamped')}",
        domain,
    ])


def file_quoting(record: dict[str, Any]) -> str:
    """Whether the release writes this setting's value as JSON or as raw text.

    A release that keeps its options in `OptionInstance` writes each one by
    encoding it with the option's own codec and printing the resulting JSON, so
    anything whose JSON form is a string arrives in the file inside quotes:
    `renderClouds:"true"`. Everything written by the older direct path is
    printed as it stands: `renderClouds:true`. The difference is per setting and
    per release, not a matter of how modern the release looks, so it is read off
    the persistence path each time rather than assumed from a version number.
    """
    if record.get("kind") != "option_instance":
        return "raw"
    json_string = record.get("type") == "string"
    allowed = record.get("allowed_values") or {}
    if allowed.get("persisted_form") == "string_token":
        json_string = True
    return "json_string" if json_string else "json"


def build_setting(version: str | None, sha: str, root_class: str | None, key: str,
                  record: dict[str, Any]) -> dict[str, Any]:
    field = record.get("field")
    typ = record.get("type") or "unknown"
    analyzed_type = typ
    special = SPECIAL_KEYS.get(key)
    encoding = record.get("file_encoding")
    notes = list(record.get("notes") or [])
    if special:
        encoding = encoding if typ not in ("unknown", "special", "string") else special[0]
        notes.append(special[1])
        if typ in ("unknown", "special"):
            typ = "special"
    setting: dict[str, Any] = {
        "logical_id": field or key,
        "category": category_for(key, field),
        "persisted_key": key,
        "translation_key": record.get("translation_key"),
        "type": typ,
        "default": record.get("default"),
        # "file": the value written to options.txt; "ui": the value before the codec transforms it.
        "default_space": record.get("default_space",
                                    None if record.get("default") is None else "file"),
        "numeric": record.get("numeric"),
        "allowed_values": record.get("allowed_values"),
        "file_encoding": encoding or "unresolved",
        "ui_to_file_transform": ({"value_to_storage": record.get("value_to_storage"),
                                  "storage_to_value": record.get("storage_to_value"),
                                  "confidence": "high"}
                                 if record.get("value_to_storage") or record.get("storage_to_value") else None),
        "value_to_storage": record.get("value_to_storage"),
        "storage_to_value": record.get("storage_to_value"),
        "file_quoting": file_quoting(record),
        # False when the persisted codec transforms the value and this analyzer
        # could not read how, which is not the same as no transform at all.
        "transform_resolved": record.get("transform_resolved",
                                         (record.get("codec") or {}).get("transform_resolved")),
        "ui_domain": record.get("ui_domain"),
        "support_status": "supported",
        # The special label does not prove the codec the analyzer failed to type.
        "type_confidence": "high" if analyzed_type != "unknown" else "unknown",
        "default_confidence": record.get("default_confidence",
                                         "high" if record.get("default") is not None else "unknown"),
        "domain_confidence": record.get("domain_confidence")
        or ("high" if (record.get("numeric") or (record.get("allowed_values") or {}).get("values"))
            else "unknown"),
        "notes": notes,
        "ephervex_manageable": key not in UNSAFE_FOR_EPHERVEX and typ not in ("special", "unknown"),
        "evidence": [{"jar_sha256": sha, "minecraft_version": version, "class": root_class,
                      "method": "options load/save persistence path located by bytecode",
                      "description": "Persisted key, backing field and codec/parser relationship",
                      "constant": key, "field": field}],
    }
    setting["confidence"] = weakest(setting["type_confidence"], "high")
    setting["representation"] = representation(setting)
    return setting


def normalize(record: dict[str, Any]) -> dict[str, Any]:
    analysis = record.get("options_analysis", {})
    sha = record["sha256"]
    version = record.get("detected_version")
    root_class = analysis.get("roots", [None])[0] if analysis.get("roots") else None
    settings: list[dict[str, Any]] = []
    for key, rec in sorted((analysis.get("options") or {}).items()):
        settings.append(build_setting(version, sha, root_class, key, rec))

    sounds = analysis.get("sound_family")
    if sounds:
        for name in sounds["names"] or []:
            key = "soundCategory_" + name
            settings.append({
                "logical_id": "sound.volume." + name, "category": "Sound & Music",
                "persisted_key": key, "translation_key": None, "type": "number",
                "default": sounds.get("default"),
                "numeric": {"min": 0.0, "max": 1.0, "step": None, "clamped": True},
                "allowed_values": None,
                "file_encoding": "decimal floating point in the closed interval 0.0 to 1.0",
                "ui_to_file_transform": None, "value_to_storage": None, "storage_to_value": None,
                "support_status": "supported", "type_confidence": "high",
                "default_confidence": sounds.get("confidence", "unknown") if sounds.get("default") is not None
                else "unknown",
                "domain_confidence": "high",
                "confidence": "high", "notes": [], "ephervex_manageable": True,
                "evidence": [{"jar_sha256": sha, "minecraft_version": version, "class": root_class,
                              "method": "options persistence path, soundCategory_ family loop",
                              "description": "Dynamic soundCategory_<name> family; the member set comes from "
                                             f"{sounds['enum_class']} reached through the root's own generic "
                                             "field signature. "
                                             + str(sounds.get("evidence_note") or sounds.get("evidence", "")),
                              "constant": key}]})
    parts = analysis.get("model_part_family")
    if parts:
        for name in parts["names"] or []:
            key = "modelPart_" + name
            settings.append({
                "logical_id": "modelPart." + name, "category": "Gameplay", "persisted_key": key,
                "translation_key": None, "type": "boolean", "default": True,
                "numeric": None, "allowed_values": None,
                "file_encoding": "lowercase true/false", "ui_to_file_transform": None,
                "value_to_storage": None, "storage_to_value": None, "support_status": "supported",
                "type_confidence": "high", "default_confidence": "medium", "domain_confidence": "high",
                "confidence": "high", "notes": ["All skin model parts are enabled by default; the default is "
                                                "the initial EnumSet population rather than a literal constant."],
                "ephervex_manageable": True,
                "evidence": [{"jar_sha256": sha, "minecraft_version": version, "class": root_class,
                              "method": "options persistence path, modelPart_ family loop",
                              "description": f"Dynamic modelPart_<id> family driven by {parts['enum_class']}.values()",
                              "constant": key}]})

    legacy_codes = analysis.get("keybinding_code_space") == "lwjgl2"
    for kb in analysis.get("keybinds") or []:
        settings.append({
            "logical_id": "keybind." + kb["translation_key"][4:], "category": "Controls",
            "persisted_key": kb["persisted_key"], "translation_key": kb["translation_key"],
            "type": "key_binding",
            "default": {"input_type": kb["input_type"], "code": kb["code"],
                        "serialized": kb["serialized"], "unbound": kb["unbound"]},
            "numeric": None, "allowed_values": None,
            "file_encoding": kb["file_encoding"], "ui_to_file_transform": None,
            "value_to_storage": None, "storage_to_value": None,
            "binding_category": kb["binding_category"], "support_status": "supported",
            "notes": [kb["note"]] if kb.get("note") else [],
            "type_confidence": "high", "default_confidence": kb["confidence"],
            "domain_confidence": "high", "confidence": kb["codec_confidence"],
            "ephervex_manageable": True,
            "evidence": [{"jar_sha256": sha, "minecraft_version": version, "class": root_class,
                          "method": "<init> KeyMapping construction",
                          "description": "Translation key, default code and input type from the key-mapping "
                                         "constructor; persisted under key_<translation key>",
                          "constant": kb["translation_key"],
                          "constructor_descriptor": kb["constructor_descriptor"]}]})

    for setting in settings:
        setting.setdefault("default_space", None if setting.get("default") is None else "file")
        setting.setdefault("representation", representation(setting))
    settings.sort(key=lambda x: (x["category"], x["logical_id"], x["persisted_key"]))
    gaps = extraction_gaps(analysis)
    return {"schema_version": ANALYSIS_SCHEMA_VERSION, "tool_version": TOOL_VERSION, "jar_id": record["jar_id"],
            "minecraft_version": version, "jar_sha256": sha,
            "persistence_era": analysis.get("persistence_era"),
            # The release's own data version, which it stamps into the file as
            # `version:` and reads back to decide which historical fixes the
            # rest of the file still needs. A file without one is treated as
            # predating all of them.
            "data_version": (analysis.get("schema_marker") or {}).get("value"),
            "keybinding_code_space": analysis.get("keybinding_code_space"),
            "keybinding_code_space_check": analysis.get("keybinding_code_space_check"),
            # Re-derived from the populations rather than trusted from the raw
            # record, so a record written by an older analyzer is judged by the
            # current definition.
            "extraction_complete": bool(analysis.get("extraction_complete")) and not gaps,
            "extraction_gaps": gaps,
            "analysis_scope": "static targeted options root plus directly referenced helper classes",
            "settings": settings,
            "unknown_keys_policy": "Preserve every options.txt line whose key is not explicitly and safely mapped."}


# ---------------------------------------------------------------------------
# Cross-version products
# ---------------------------------------------------------------------------

def make_latest_schema(latest: dict[str, Any]) -> dict[str, Any]:
    groups: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for setting in latest["settings"]:
        groups[setting["category"]].append(setting)
    typed = [s for s in latest["settings"] if s["type"] not in ("unknown",)]
    with_default = [s for s in latest["settings"] if s.get("default") is not None]
    with_domain = [s for s in latest["settings"]
                   if s.get("numeric") or (s.get("allowed_values") or {}).get("values")]
    return {"schema_version": ANALYSIS_SCHEMA_VERSION, "canonical_minecraft_version": latest["minecraft_version"],
            "source_jar_sha256": latest["jar_sha256"], "setting_count": len(latest["settings"]),
            "fully_typed_count": len(typed), "with_default_count": len(with_default),
            "with_domain_count": len(with_domain),
            "categories": [{"name": name, "settings": groups[name]} for name in sorted(groups)],
            "unknown_keys_policy": latest["unknown_keys_policy"],
            "qualification": "Only high-confidence facts are authoritative; null fields and lower "
                             "confidence remain unresolved."}


def key_index(version: dict[str, Any]) -> dict[str, dict[str, Any]]:
    return {s["persisted_key"]: s for s in version["settings"]}


def translation_index(version: dict[str, Any]) -> dict[str, list[dict[str, Any]]]:
    """Every setting that shares each translation key; a key can name several."""
    index: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for setting in version["settings"]:
        if setting.get("translation_key"):
            index[setting["translation_key"]].append(setting)
    return index


def renamed_from(canonical: dict[str, Any], older_settings: list[dict[str, Any]],
                 canonical_keys: set[str], same_kind_claims: int) -> dict[str, Any] | None:
    """The one older setting a shared translation key proves was renamed, if any.

    A shared caption is rename evidence only when it points at a single setting
    of the same kind on each side and the older one is not itself a current
    setting: two live settings that reuse a caption (`guiScale` and
    `debugGuiScale`) are different settings, not a rename.
    """
    if same_kind_claims != 1:
        return None
    kind = canonical["type"] == "key_binding"
    candidates = [o for o in older_settings if (o["type"] == "key_binding") == kind]
    if len(candidates) != 1 or candidates[0]["persisted_key"] in canonical_keys:
        return None
    return candidates[0]


def compatibility(latest: dict[str, Any], versions: list[dict[str, Any]]) -> dict[str, Any]:
    indexes = {v["minecraft_version"]: key_index(v) for v in versions}
    translations = {v["minecraft_version"]: translation_index(v) for v in versions}
    complete = {v["minecraft_version"]: v.get("extraction_complete", False) for v in versions}
    ordered = [v["minecraft_version"] for v in versions]
    canonical_keys = {s["persisted_key"] for s in latest["settings"]}
    caption_claims = Counter((s["translation_key"], s["type"] == "key_binding")
                             for s in latest["settings"] if s.get("translation_key"))
    ever_seen_before: dict[str, set[str]] = {}
    seen: set[str] = set()
    for name in ordered:
        ever_seen_before[name] = set(seen)
        seen |= set(indexes[name])

    rows = []
    for canonical in latest["settings"]:
        key = canonical["persisted_key"]
        canonical_repr = canonical["representation"]
        per: dict[str, Any] = {}
        for name in ordered:
            index = indexes[name]
            if name == latest["minecraft_version"]:
                per[name] = {"status": "identical", "confidence": "high",
                             "reason": "canonical source version"}
                continue
            older = index.get(key)
            if older is None and canonical.get("translation_key"):
                older = renamed_from(
                    canonical, translations[name].get(canonical["translation_key"], []), canonical_keys,
                    caption_claims[(canonical["translation_key"], canonical["type"] == "key_binding")])
            if older is None:
                if complete[name] and key not in ever_seen_before[name]:
                    per[name] = {"status": "unsupported", "confidence": "high",
                                 "reason": "key absent from a fully extracted persistence path and absent "
                                           "from every older analysed release, so it was introduced later"}
                elif complete[name]:
                    per[name] = {"status": "unknown", "confidence": "medium",
                                 "reason": "key absent here but present in an older release; a rename or "
                                           "removal is possible and is not guessed"}
                else:
                    per[name] = {"status": "unknown", "confidence": "unknown",
                                 "reason": "persistence path not fully extracted for this release"}
                continue
            renamed = older["persisted_key"] != key
            older_repr = older["representation"]
            unresolved = ("unknown" in (older["type"], canonical["type"])
                          or "unresolved" in (older["file_encoding"], canonical["file_encoding"])
                          or older.get("transform_resolved") is False
                          or canonical.get("transform_resolved") is False)
            if unresolved:
                per[name] = {"status": "unknown", "confidence": "medium",
                             "reason": "matching entry found but its representation is not fully resolved"}
                continue
            if older_repr == canonical_repr and not renamed:
                per[name] = {"status": "identical", "confidence": "high",
                             "reason": "same persisted key, type, encoding, domain and storage transform"}
                continue
            mapping: dict[str, Any] = {"older_key": older["persisted_key"], "canonical_key": key,
                       "older_type": older["type"], "canonical_type": canonical["type"],
                       "older_encoding": older["file_encoding"],
                       "canonical_encoding": canonical["file_encoding"],
                       "older_quoting": written_quoting(older),
                       "canonical_quoting": written_quoting(canonical),
                       "older_value_to_storage": older.get("value_to_storage"),
                       "canonical_value_to_storage": canonical.get("value_to_storage"),
                       "older_storage_to_value": older.get("storage_to_value"),
                       "canonical_storage_to_value": canonical.get("storage_to_value"),
                       "older_domain": older.get("numeric") or older.get("allowed_values"),
                       "canonical_domain": canonical.get("numeric") or canonical.get("allowed_values")}
            reasons, unproven = [], []
            if renamed:
                reasons.append("persisted key renamed (matched on the identical translation key)")
            if older["type"] != canonical["type"]:
                reasons.append("type differs")
            if older["file_encoding"] != canonical["file_encoding"]:
                reasons.append("file encoding differs")
            if written_quoting(older) != written_quoting(canonical):
                reasons.append("file quoting differs")
            if older.get("value_to_storage") != canonical.get("value_to_storage"):
                reasons.append("storage transform differs")
            if older.get("storage_to_value") != canonical.get("storage_to_value"):
                reasons.append("read transform differs")
            older_domain, canonical_domain = older.get("numeric"), canonical.get("numeric")
            if (older_domain or {}) != (canonical_domain or {}):
                if bool(older_domain) != bool(canonical_domain):
                    unproven.append("numeric domain is unproven on one side")
                else:
                    reasons.append("numeric domain differs")
            older_allowed, canonical_allowed = older.get("allowed_values") or {}, canonical.get("allowed_values") or {}
            older_tokens = {str(v.get("persisted")) for v in older_allowed.get("values") or []}
            canonical_tokens = {str(v.get("persisted")) for v in canonical_allowed.get("values") or []}
            if bool(older_tokens) != bool(canonical_tokens):
                if older["type"] == canonical["type"]:
                    unproven.append("enum values are unproven on one side")
            elif (older_tokens != canonical_tokens
                  or older_allowed.get("persisted_form") != canonical_allowed.get("persisted_form")):
                reasons.append("enum values differ")
            if not reasons and not unproven:
                per[name] = {"status": "identical", "confidence": "high",
                             "reason": "same persisted key, type, encoding, quoting, domain and storage "
                                       "transform"}
                continue
            if not reasons:
                # The encoding and transform are proven identical; only a domain is
                # unproven on one side, so the value is readable and writable but
                # out-of-range clamping cannot be guaranteed.  Medium keeps it out of
                # the write-authoritative set without pretending the encoding differs.
                per[name] = {"status": "identical", "confidence": "medium",
                             "reason": "identical type, encoding and storage transform; "
                                       + "; ".join(unproven) + ", so behavior outside the canonical "
                                       "domain is not guaranteed",
                             "older_ui_domain": older.get("ui_domain")}
                continue
            per[name] = {"status": "mapped", "confidence": "medium" if unproven else "high",
                         "reason": "; ".join(reasons + unproven), "mapping": mapping}
        rows.append({"canonical_id": canonical["logical_id"], "canonical_key": key,
                     "canonical_type": canonical["type"],
                     "translation_key": canonical.get("translation_key"),
                     "ephervex_manageable": canonical.get("ephervex_manageable", True),
                     "versions": per})
    return {"schema_version": ANALYSIS_SCHEMA_VERSION, "canonical_version": latest["minecraft_version"],
            "classifications": ["identical", "mapped", "unsupported", "unknown"],
            "safety_rule": "Only high-confidence identical/mapped entries are write-authoritative. Unknown "
                           "entries must be skipped and existing lines preserved.",
            "settings": rows}


def keybinding_eras(versions: list[dict[str, Any]]) -> list[dict[str, Any]]:
    eras: list[dict[str, Any]] = []
    for v in versions:
        space = v.get("keybinding_code_space")
        encodings = sorted({s["file_encoding"] for s in v["settings"] if s["type"] == "key_binding"})
        signature = (space, tuple(encodings))
        if eras and eras[-1]["_signature"] == signature:
            eras[-1]["through"] = v["minecraft_version"]
            continue
        eras.append({"_signature": signature, "from": v["minecraft_version"],
                     "through": v["minecraft_version"], "code_space": space,
                     "file_encodings": encodings,
                     "binding_count": sum(1 for s in v["settings"] if s["type"] == "key_binding")})
    for era in eras:
        era.pop("_signature")
        era["confidence"] = "high"
    return eras


def facet_eras(versions: list[dict[str, Any]], key: str) -> list[dict[str, Any]]:
    eras: list[dict[str, Any]] = []
    for v in versions:
        setting = next((s for s in v["settings"] if s["persisted_key"] == key), None)
        signature = setting["representation"] if setting else "<absent>"
        detail = {"type": setting["type"] if setting else None,
                  "file_encoding": setting["file_encoding"] if setting else None,
                  "quoting": written_quoting(setting) if setting else None,
                  "value_to_storage": setting.get("value_to_storage") if setting else None,
                  "storage_to_value": setting.get("storage_to_value") if setting else None,
                  "numeric": setting.get("numeric") if setting else None,
                  "ui_domain": setting.get("ui_domain") if setting else None,
                  "default": setting.get("default") if setting else None}
        if eras and eras[-1]["_signature"] == signature:
            eras[-1]["through"] = v["minecraft_version"]
            continue
        eras.append({"_signature": signature, "from": v["minecraft_version"],
                     "through": v["minecraft_version"], **detail})
    for era in eras:
        era.pop("_signature")
        era["confidence"] = "high"
    return eras


def schema_changes(versions: list[dict[str, Any]], canonical_keys: set[str]) -> dict[str, Any]:
    changes = []
    previous = None
    for item in versions:
        index = key_index(item)
        if previous is not None:
            prev_index = key_index(previous)
            keys, oldkeys = set(index), set(prev_index)
            added, removed = sorted(keys - oldkeys), sorted(oldkeys - keys)
            union = keys | oldkeys
            similarity = len(keys & oldkeys) / len(union) if union else 1.0
            type_changes, default_changes, numeric_changes, enum_changes, serialization_changes = [], [], [], [], []
            for key in sorted(keys & oldkeys):
                new, old = index[key], prev_index[key]
                if new["type"] != old["type"] and "unknown" not in (new["type"], old["type"]):
                    type_changes.append({"key": key, "from": old["type"], "to": new["type"]})
                if (new.get("default") != old.get("default")
                        and new.get("default") is not None and old.get("default") is not None):
                    default_changes.append({"key": key, "from": old["default"], "to": new["default"]})
                if (new.get("numeric") or {}) != (old.get("numeric") or {}) and new.get("numeric") and old.get("numeric"):
                    numeric_changes.append({"key": key, "from": old["numeric"], "to": new["numeric"]})
                new_domain = (new.get("allowed_values") or {}).get("values")
                old_domain = (old.get("allowed_values") or {}).get("values")
                if new_domain and old_domain and new_domain != old_domain:
                    enum_changes.append({"key": key,
                                         "from": [v["persisted"] for v in old_domain],
                                         "to": [v["persisted"] for v in new_domain]})
                wire_new = (new["file_encoding"], written_quoting(new), new.get("value_to_storage"),
                            new.get("storage_to_value"))
                wire_old = (old["file_encoding"], written_quoting(old), old.get("value_to_storage"),
                            old.get("storage_to_value"))
                if wire_new != wire_old:
                    if ("unresolved" not in (new["file_encoding"], old["file_encoding"])
                            and new.get("transform_resolved") is not False
                            and old.get("transform_resolved") is not False):
                        serialization_changes.append({"key": key,
                                                      "from": {"encoding": old["file_encoding"],
                                                               "quoting": written_quoting(old),
                                                               "value_to_storage": old.get("value_to_storage"),
                                                               "storage_to_value": old.get("storage_to_value")},
                                                      "to": {"encoding": new["file_encoding"],
                                                             "quoting": written_quoting(new),
                                                             "value_to_storage": new.get("value_to_storage"),
                                                             "storage_to_value": new.get("storage_to_value")}})
            keybind_change = None
            if previous.get("keybinding_code_space") != item.get("keybinding_code_space"):
                keybind_change = (f"{previous.get('keybinding_code_space')} -> "
                                  f"{item.get('keybinding_code_space')}")
            changes.append({"from": previous["minecraft_version"], "to": item["minecraft_version"],
                            "options_added": added, "options_removed": removed,
                            "persisted_key_renames": [], "type_changes": type_changes,
                            "default_changes": default_changes, "numeric_changes": numeric_changes,
                            "enum_changes": enum_changes, "serialization_changes": serialization_changes,
                            "keybind_encoding_change": keybind_change,
                            "persistence_era_change": (f"{previous.get('persistence_era')} -> "
                                                       f"{item.get('persistence_era')}")
                            if previous.get("persistence_era") != item.get("persistence_era") else None,
                            "language_change": None, "key_set_similarity": round(similarity, 4),
                            "qualification": "Add/remove and semantic-change arrays are derived from resolved "
                                             "bytecode facts; entries whose representation stayed unresolved on "
                                             "either side are omitted rather than assumed unchanged."})
        previous = item

    eras: list[dict[str, Any]] = []
    for item in versions:
        index = key_index(item)
        signature = tuple(sorted((k, index[k]["representation"]) for k in canonical_keys if k in index))
        missing = tuple(sorted(k for k in canonical_keys if k not in index))
        full = (signature, missing, item.get("persistence_era"), item.get("keybinding_code_space"))
        if eras and eras[-1]["_signature"] == full:
            eras[-1]["through"] = item["minecraft_version"]
            eras[-1]["releases"].append(item["minecraft_version"])
            continue
        eras.append({"_signature": full, "from": item["minecraft_version"],
                     "through": item["minecraft_version"], "releases": [item["minecraft_version"]],
                     "persistence_era": item.get("persistence_era"),
                     "keybinding_code_space": item.get("keybinding_code_space"),
                     "canonical_keys_present": len(signature),
                     "canonical_keys_absent": len(missing)})
    previous_era = None
    for era in eras:
        signature = era.pop("_signature")
        if previous_era is not None:
            prev_sig, prev_missing = previous_era[0], previous_era[1]
            prev_map, cur_map = dict(prev_sig), dict(signature[0])
            boundary = []
            if previous_era[2] != era["persistence_era"]:
                boundary.append(f"persistence era changed to {era['persistence_era']}")
            if previous_era[3] != era["keybinding_code_space"]:
                boundary.append(f"key-binding code space changed to {era['keybinding_code_space']}")
            changed = sorted(k for k in set(prev_map) & set(cur_map) if prev_map[k] != cur_map[k])
            appeared = sorted(set(cur_map) - set(prev_map))
            vanished = sorted(set(prev_map) - set(cur_map))
            if changed:
                boundary.append("representation changed for " + ", ".join(changed[:12])
                                + (f" and {len(changed) - 12} more" if len(changed) > 12 else ""))
            if appeared:
                boundary.append(f"{len(appeared)} canonical keys became available")
            if vanished:
                boundary.append(f"{len(vanished)} canonical keys disappeared")
            era["boundary_reason"] = "; ".join(boundary) or "canonical representation set changed"
        else:
            era["boundary_reason"] = "oldest analysed supported release"
        era["basis"] = ("identical canonical-setting representation, persistence era and key-binding "
                        "code space across every release in the range")
        era["confidence"] = "high"
        previous_era = (signature[0], signature[1], era["persistence_era"], era["keybinding_code_space"])
    return {"schema_version": ANALYSIS_SCHEMA_VERSION, "versions": [x["minecraft_version"] for x in versions],
            "eras": eras,
            "keybinding_eras": keybinding_eras(versions),
            "fov_eras": facet_eras(versions, "fov"),
            "changes": changes,
            "limitations": ["Persisted-key renames are only asserted when the same translation key is present "
                            "on both sides.", "Representation comparisons ignore facets that stayed unresolved."]}


def keycode_crosscheck(versions: list[dict[str, Any]]) -> dict[str, Any]:
    """Derive LWJGL2 -> GLFW default-code pairs locally, then compare with the reference table."""
    legacy = [v for v in versions if v.get("keybinding_code_space") == "lwjgl2"]
    modern = [v for v in versions if v.get("keybinding_code_space") == "glfw"]
    if not legacy or not modern:
        return {"status": "unavailable", "reason": "boundary versions not both present"}
    before, after = legacy[-1], modern[0]
    before_map = {s["translation_key"]: s for s in before["settings"] if s["type"] == "key_binding"}
    after_map = {s["translation_key"]: s for s in after["settings"] if s["type"] == "key_binding"}
    pairs, agree, disagree, unresolved = [], [], [], []
    for name in sorted(set(before_map) & set(after_map)):
        old_code = before_map[name]["default"]["code"]
        new_code = after_map[name]["default"]["code"]
        new_name = after_map[name]["default"]["serialized"]
        if old_code is None or new_code is None:
            continue
        expected = LWJGL2_KEYS.get(old_code)
        observed = new_name.split("key.keyboard.")[-1] if new_name and new_name.startswith("key.keyboard.") else None
        entry = {"binding": name, "lwjgl2_code": old_code, "glfw_code": new_code,
                 "glfw_name": observed, "reference_table_name": expected}
        pairs.append(entry)
        if expected is None or observed is None:
            unresolved.append(entry)
        elif expected == observed:
            agree.append(entry)
        else:
            disagree.append(entry)
    return {"status": "derived",
            "boundary": {"legacy_release": before["minecraft_version"],
                         "namespaced_release": after["minecraft_version"]},
            "method": "Pair every key binding whose translation key exists on both sides of the boundary and "
                      "compare its pre-1.13 LWJGL2 default code with its post-1.13 GLFW default name.",
            "assumption": "Default bindings were not re-assigned across the boundary release; agreement with "
                          "the independent LWJGL2 reference table is what raises confidence.",
            "reference_table": {"source": "org.lwjgl.input.Keyboard (LWJGL 2) constants",
                                "provenance": "external constant table; LWJGL is not bundled in any analysed JAR",
                                "confidence": "medium"},
            "agreement_count": len(agree), "disagreement_count": len(disagree),
            "unresolved_count": len(unresolved),
            "confidence": "high" if agree and not disagree else "medium",
            "pairs": pairs,
            "conversion_rule": "A pre-1.13 options.txt stores key_<translation key>:<LWJGL2 integer>. A 1.13+ "
                               "options.txt stores key_<translation key>:key.keyboard.<name> or key.mouse.<name>. "
                               "Converting between the two requires the LWJGL2 code table above; -1 means "
                               "explicitly unbound on both sides (key.keyboard.unknown from 1.13).",
            "disagreements": disagree}


def load_language_cache(cache: Path, versions: list[str]) -> dict[str, dict[str, Any]]:
    out: dict[str, dict[str, Any]] = {}
    for version in versions:
        path = cache / "language" / f"{version}.json"
        if not path.exists():
            continue
        try:
            out[version] = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            continue
    return out


def verified_languages(record: dict[str, Any]) -> dict[str, Any] | None:
    """A cached record's language section, only when its `pack.mcmeta` hash was verified."""
    if record.get("status") != "ok" or (record.get("pack_mcmeta") or {}).get("sha1_matches") is not True:
        return None
    return record.get("languages") or {}


def language_output(latest_rec: dict[str, Any], latest_jar: Path, latest_norm: dict[str, Any],
                    cache: dict[str, dict[str, Any]], versions: list[str]) -> dict[str, Any]:
    canonical_version = latest_rec["detected_version"]
    entries = []
    with ZipFile(latest_jar) as zf:
        in_jar = "assets/minecraft/lang/en_us.json" in zf.namelist()
    canonical = cache.get(canonical_version, {})
    verified = verified_languages(canonical)
    catalog = verified or {}
    for code in sorted(catalog):
        info = catalog[code]
        evidence = [{"source": "asset index object `pack.mcmeta`, `language` metadata section",
                     "asset_index_id": (canonical.get("asset_index") or {}).get("id"),
                     "asset_index_sha1": (canonical.get("asset_index") or {}).get("sha1"),
                     "pack_mcmeta_sha1": (canonical.get("pack_mcmeta") or {}).get("sha1"),
                     "description": "Exactly the section LanguageManager enumerates at runtime"}]
        if code == "en_us" and in_jar:
            evidence.append({"jar_sha256": latest_rec["sha256"],
                             "resource": "assets/minecraft/lang/en_us.json",
                             "description": "Built-in default translation, also present in the client JAR"})
        entries.append({"code": code, "name": info.get("name"), "region": info.get("region"),
                        "bidirectional": bool(info.get("bidirectional")),
                        "confidence": "high", "evidence": evidence})
    if not entries and in_jar:
        entries.append({"code": "en_us", "name": "English", "region": "US", "bidirectional": False,
                        "confidence": "high", "evidence": [
                            {"jar_sha256": latest_rec["sha256"],
                             "resource": "assets/minecraft/lang/en_us.json",
                             "description": "Embedded default translation resource"}]})

    per_version = {}
    for version in versions:
        record = cache.get(version)
        if not record or verified_languages(record) is None:
            status = (record or {}).get("status", "not_cached")
            if status == "ok":
                status = "unverified"
            per_version[version] = {"status": status, "count": None, "codes": None}
            continue
        codes = sorted(record.get("languages") or {})
        per_version[version] = {
            "status": "ok", "count": len(codes), "codes": codes,
            "asset_index_id": (record.get("asset_index") or {}).get("id"),
            "asset_index_sha1": (record.get("asset_index") or {}).get("sha1"),
            "pack_mcmeta_sha1": (record.get("pack_mcmeta") or {}).get("sha1"),
            "missing_from_canonical": sorted(set(codes) - set(catalog)),
            "absent_here_but_canonical": sorted(set(catalog) - set(codes)),
        }
    setting = next((s for s in latest_norm["settings"] if s["persisted_key"] == "lang"), None)
    return {"schema_version": ANALYSIS_SCHEMA_VERSION, "canonical_version": canonical_version,
            "complete": bool(entries and verified is not None),
            "count": len(entries), "languages": entries,
            "per_version_catalog": per_version,
            "option": {"persisted_key": "lang",
                       "logical_id": setting["logical_id"] if setting else None,
                       "type": setting["type"] if setting else "string",
                       "file_encoding": setting["file_encoding"] if setting else None,
                       "default": "en_us",
                       "default_confidence": "high",
                       "serialization": "Lower-case ISO-639 language code, underscore, lower-case region code "
                                        "(for example en_us, pt_br). Written verbatim after the first colon "
                                        "with no quoting or escaping.",
                       "evidence": "lang is processed by the FieldAccess string overload; the default field "
                                   "initialiser and LanguageManager both use the constant en_us."},
            "catalog_source": {
                "mechanism": "LanguageManager extracts the `language` metadata section from each PackResources "
                             "(pack.mcmeta), then loads assets/<namespace>/lang/<code>.json resources.",
                "local_state": "The client JAR embeds only the built-in en_us translation. The complete "
                               "catalog is resolved from the cached `pack.mcmeta` asset object for each "
                               "release, populated by fetch_language_catalogs.py into assets-cache/language/.",
                "collection_recipe": [
                    "Read the version manifest at "
                    "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json and take the target "
                    "release's package URL.",
                    "Read that package JSON and take assetIndex.url plus its id and sha1.",
                    "Read the asset index and take the entry named `pack.mcmeta`.",
                    "Fetch https://resources.download.minecraft.net/<first two hex chars>/<sha1> for that "
                    "entry; its `language` object maps every code to its name, region and bidirectional flag.",
                    "The same asset index also lists minecraft/lang/<code>.json (1.13+) or "
                    "minecraft/lang/<code>.lang (pre-1.13) resources, which corroborate the code list.",
                    "An already-installed launcher has all of this on disk under assets/indexes and "
                    "assets/objects, so the same recipe works entirely offline and never launches the game."],
                "confidence": "high",
                "evidence": [
                    {"class": "net/minecraft/client/resources/language/LanguageManager",
                     "methods": ["extractLanguages", "lambda$extractLanguages$0"]},
                    {"class": "net/minecraft/client/resources/metadata/language/LanguageMetadataSection",
                     "metadata_section": "language"},
                    {"class": "net/minecraft/client/resources/language/LanguageInfo",
                     "fields": ["region", "name", "bidirectional"]}]},
            "older_compatibility": {
                "status": "resolved",
                "detail": "The `lang` persisted key and its plain-string encoding are stable across every "
                          "analysed supported release; the set of valid codes is version dependent and is "
                          "enumerated per release in `per_version_catalog`.",
                "per_code_support": "enumerated",
                "rule": "A canonical code may be written to an older release only if it appears in that "
                        "release's own catalog; otherwise the client falls back to the default language."}}


# ---------------------------------------------------------------------------
# Report
# ---------------------------------------------------------------------------

def count_status(matrix: dict[str, Any], version: str, statuses: tuple[str, ...],
                 high_only: bool = True) -> list[str]:
    out = []
    for row in matrix["settings"]:
        entry = row["versions"].get(version)
        if not entry:
            continue
        if entry["status"] in statuses and (not high_only or entry["confidence"] == "high"):
            out.append(row["canonical_id"])
    return sorted(out)


def report(inventory: list[dict[str, Any]], latest: dict[str, Any] | None,
           normalized: list[dict[str, Any]], languages: dict[str, Any] | None,
           changes: dict[str, Any] | None, matrix: dict[str, Any] | None,
           crosscheck: dict[str, Any] | None, command: str) -> str:
    identified = [x for x in inventory if x.get("detected_version")]
    lines = ["# Minecraft options.txt static research", "",
             "Static analysis only: JARs are read as ZIP/class data and `javap` disassembles the "
             "options root plus the helper classes it directly references. Minecraft is never executed.",
             "", "## Summary", "",
             f"- Discovered JARs: **{len(inventory)}**.",
             f"- Confidently identified versions: **{len(identified)}**."]
    if latest:
        settings = latest["settings"]
        typed = [s for s in settings if s["type"] != "unknown"]
        defaults = [s for s in settings if s.get("default") is not None]
        domains = [s for s in settings
                   if s.get("numeric") or (s.get("allowed_values") or {}).get("values")
                   or s["type"] in ("boolean", "key_binding")]
        sounds = [s for s in settings if s["category"] == "Sound & Music"]
        keys = [s for s in settings if s["type"] == "key_binding"]
        key_defaults = sum(1 for s in keys if (s.get("default") or {}).get("serialized"))
        unmanaged = [s for s in settings if not s.get("ephervex_manageable", True)]
        total = len(settings)
        lines += [f"- Newest stable supported release: **{latest['minecraft_version']}**.",
                  f"- Canonical settings recovered: **{total}**.",
                  f"- Fully typed: **{len(typed)}/{total}** ({100 * len(typed) / total:.1f}%).",
                  f"- Known defaults: **{len(defaults)}/{total}** ({100 * len(defaults) / total:.1f}%).",
                  f"- Proven domain (numeric range, enum domain or boolean/key-binding domain): "
                  f"**{len(domains)}/{total}** ({100 * len(domains) / total:.1f}%).",
                  f"- Sound & Music settings: **{len(sounds)}**.",
                  f"- Key bindings: **{len(keys)}** with **{key_defaults}/{len(keys)}** resolved default "
                  f"serialisations.",
                  f"- Deliberately not Ephervex-manageable: **{len(unmanaged)}**.",
                  f"- Language catalog: **{'complete' if (languages or {}).get('complete') else 'incomplete'}**; "
                  f"**{languages['count'] if languages else 0}** codes at "
                  f"{latest['minecraft_version']}, enumerated per release."]

    lines += ["", "## 1-4. Canonical coverage", ""]
    if latest:
        settings = latest["settings"]
        total = len(settings)
        typed = [s for s in settings if s["type"] != "unknown"]
        defaults = [s for s in settings if s.get("default") is not None]
        domains = [s for s in settings
                   if s.get("numeric") or (s.get("allowed_values") or {}).get("values")
                   or s["type"] in ("boolean", "key_binding")]
        untyped = [s["persisted_key"] for s in settings if s["type"] == "unknown"]
        no_default = [s["persisted_key"] for s in settings if s.get("default") is None]
        lines += [f"1. Canonical {latest['minecraft_version']} settings: **{total}**.",
                  f"2. Fully typed: **{len(typed)}** ({100 * len(typed) / total:.1f}%). "
                  + ("Remaining untyped: " + ", ".join(f"`{k}`" for k in untyped) + "." if untyped
                     else "No canonical setting is untyped."),
                  f"3. Known defaults: **{len(defaults)}**. Without a statically provable default: "
                  + (", ".join(f"`{k}`" for k in no_default) + "." if no_default else "none."),
                  f"4. Proven ranges/domains: **{len(domains)}** "
                  "(explicit numeric range, enumerated enum domain, or an inherently closed boolean/"
                  "key-binding domain).", ""]
        by_category: dict[str, int] = defaultdict(int)
        for s in settings:
            by_category[s["category"]] += 1
        lines += ["| Category | Settings |", "|---|---:|"]
        for name in sorted(by_category):
            lines.append(f"| {name} | {by_category[name]} |")

    lines += ["", "## 5. Canonical settings safely representable per release", "",
              "`identical` and `mapped` at high confidence are write-authoritative. `unknown` must never be "
              "written; `unsupported` means the key provably did not exist yet.", "",
              "| Release | identical | mapped | unsupported | unknown |", "|---|---:|---:|---:|---:|"]
    if matrix and normalized:
        for v in normalized:
            name = v["minecraft_version"]
            counts = defaultdict(int)
            for row in matrix["settings"]:
                entry = row["versions"].get(name)
                if entry:
                    counts[entry["status"]] += 1
            lines.append(f"| {name} | {counts['identical']} | {counts['mapped']} | "
                         f"{counts['unsupported']} | {counts['unknown']} |")

    if matrix and normalized and latest:
        oldest = normalized[0]["minecraft_version"]
        unknown_oldest = count_status(matrix, oldest, ("unknown",), high_only=False)
        lines += ["", "## 6. Canonical settings still unknown on older releases", "",
                  f"At **{oldest}** (oldest supported target) **{len(unknown_oldest)}** canonical settings "
                  "remain `unknown`:", "",
                  ", ".join(f"`{x}`" for x in unknown_oldest) if unknown_oldest
                  else "None; every canonical setting resolved to identical, mapped or unsupported.", ""]
        rows_unknown_anywhere = defaultdict(list)
        for row in matrix["settings"]:
            for name, entry in row["versions"].items():
                if entry["status"] == "unknown":
                    rows_unknown_anywhere[row["canonical_id"]].append(name)
        if rows_unknown_anywhere:
            lines += ["Canonical settings unknown on at least one supported release:", ""]
            for canonical_id in sorted(rows_unknown_anywhere)[:60]:
                versions_list = rows_unknown_anywhere[canonical_id]
                lines.append(f"- `{canonical_id}`: {len(versions_list)} release(s), "
                             f"{versions_list[0]}–{versions_list[-1]}.")
            if len(rows_unknown_anywhere) > 60:
                lines.append(f"- … and {len(rows_unknown_anywhere) - 60} more; see "
                             "`compatibility-matrix.json`.")

    lines += ["", "## 7. Major compatibility / schema eras", ""]
    if changes:
        lines += ["| Era | Releases | Persistence era | Key codes | Boundary reason |",
                  "|---|---|---|---|---|"]
        for era in changes["eras"]:
            span = era["from"] if era["from"] == era["through"] else f"{era['from']}–{era['through']}"
            lines.append(f"| {span} | {len(era['releases'])} | {era['persistence_era']} | "
                         f"{era['keybinding_code_space']} | {era['boundary_reason']} |")

    lines += ["", "## 8. Key-binding encoding eras", ""]
    if changes:
        for era in changes["keybinding_eras"]:
            span = era["from"] if era["from"] == era["through"] else f"{era['from']}–{era['through']}"
            lines.append(f"- **{span}** - code space `{era['code_space']}`, {era['binding_count']} bindings, "
                         f"encoding: {'; '.join(era['file_encodings'])}.")
    if crosscheck and crosscheck.get("status") == "derived":
        lines += ["",
                  f"The pre-1.13 boundary was cross-checked locally: every binding present in both "
                  f"{crosscheck['boundary']['legacy_release']} and "
                  f"{crosscheck['boundary']['namespaced_release']} was paired by translation key, and its "
                  f"LWJGL2 default code compared with its GLFW default name. "
                  f"**{crosscheck['agreement_count']} agreements, {crosscheck['disagreement_count']} "
                  f"disagreements, {crosscheck['unresolved_count']} unresolved.** The reference code table is "
                  "external (LWJGL 2 is not bundled in any JAR) and is recorded as such in "
                  "`compatibility-matrix.json`'s sibling `schema-changes.json` cross-check block."]

    lines += ["", "## 9. FOV representation eras", ""]
    if changes:
        for era in changes["fov_eras"]:
            span = era["from"] if era["from"] == era["through"] else f"{era['from']}–{era['through']}"
            if era["type"] is None:
                lines.append(f"- **{span}** - `fov` not present in the extracted persistence path.")
                continue
            transform = era.get("value_to_storage") or "none (stored directly)"
            numeric, ui = era.get("numeric"), era.get("ui_domain")
            if numeric:
                domain = f"{numeric['min']}..{numeric['max']} (codec-proven)"
            elif ui:
                domain = (f"{ui['file_min']}..{ui['file_max']} (from the {ui['ui_min']:g}..{ui['ui_max']:g} "
                          "degree slider through the storage transform)")
            else:
                domain = "unresolved"
            lines.append(f"- **{span}** - type `{era['type']}`, encoding {era['file_encoding']}, "
                         f"stored value = {transform}, in-file domain {domain}, "
                         f"default {era['default']!r}.")

    lines += ["", "## 10. Sound & Music compatibility", ""]
    if matrix and normalized:
        sound_rows = [r for r in matrix["settings"]
                      if any(s["persisted_key"] == r["canonical_key"] and s["category"] == "Sound & Music"
                             for s in (latest["settings"] if latest else []))]
        lines += ["| Canonical setting | Key | Safe from | Notes |", "|---|---|---|---|"]
        for row in sorted(sound_rows, key=lambda r: r["canonical_id"]):
            safe = [v["minecraft_version"] for v in normalized
                    if row["versions"].get(v["minecraft_version"], {}).get("status") in ("identical", "mapped")
                    and row["versions"][v["minecraft_version"]]["confidence"] == "high"]
            unsupported_from = [v["minecraft_version"] for v in normalized
                                if row["versions"].get(v["minecraft_version"], {}).get("status") == "unsupported"]
            note = ("provably absent up to and including " + unsupported_from[-1]) if unsupported_from else ""
            lines.append(f"| `{row['canonical_id']}` | `{row['canonical_key']}` | "
                         f"{safe[0] if safe else '-'} | {note} |")

    lines += ["", "## 11. Settings Ephervex should not manage", ""]
    if latest:
        for s in latest["settings"]:
            if not s.get("ephervex_manageable", True):
                reason = s["notes"][0] if s["notes"] else "special serialisation"
                lines.append(f"- `{s['persisted_key']}` - {reason}")
        lines += ["",
                  "In addition, any options.txt key that the analyzer did not resolve for a target release "
                  "must be left byte-identical, as must every key Ephervex does not know about (mod-added "
                  "keys included)."]

    lines += ["", "## 12. Ambiguities that static analysis cannot settle", "",
              "- Lambda bodies referenced through `invokedynamic` are compiled into synthetic methods, which "
              "the analyzer resolves through the class's bootstrap-method table. A codec's own `xmap` pair is "
              "read this way and reported as the setting's storage transform; a pair whose arithmetic is not "
              "constant folding, or whose two halves do not undo each other, is reported as unresolved rather "
              "than as no transform at all. The same holds for a field-access visitor call: the arithmetic "
              "before it and the statement after it must be fully readable and undo each other. UI-side slider "
              "transforms are deliberately not read: they change which values a slider offers, not what "
              "is written.",
              "- Enum options whose codec is neither `Codec.INT`-derived nor `StringRepresentable`-derived are "
              "reported with `persisted_form: unknown` rather than a guess.",
              "- Whether the client rewrites an unknown key on save (rather than preserving it) cannot be "
              "observed statically for every era.",
              "- The pre-1.13 numeric key-code table itself is external; only its agreement with locally "
              "derived boundary pairs is evidence.",
              "- Defaults that are computed at runtime (window size, monitor-dependent values, sound device) "
              "have no static constant.",
              "",
              "## 13. Minimal empirical experiments that would resolve them", "",
              "Each experiment is a clean-profile run followed by a single-key diff. None of these were "
              "performed here.", ""]
    lines += ["- **FOV storage transform boundary.** Minecraft 1.18.2: launch a clean profile; record "
              "`options.txt`; set FOV to 30, 70 and 110 in the video settings; exit; diff only key `fov`. "
              "Expect -1.0, 0.0 and 1.0. Repeat on 1.19 and expect the same values.",
              "- **Enum id versus token.** Minecraft 26.2: clean profile; set Attack Indicator to Off, "
              "Crosshair and Hotbar; exit after each; diff only key `attackIndicator`. Expect 0, 1, 2. "
              "Repeat with Main Hand and diff only `mainHand`, expecting `left` / `right`.",
              "- **Key-binding conversion.** Minecraft 1.12.2: clean profile; rebind Forward to the up arrow; "
              "exit; diff only key `key_key.forward` (expect 200). Minecraft 1.13: repeat the same rebinding; "
              "diff only `key_key.forward` (expect `key.keyboard.up`). This pins the LWJGL2 to GLFW pair for "
              "one non-letter key and validates the reference table end to end.",
              "- **Unknown-key preservation.** Minecraft 26.2: clean profile; append a line "
              "`ephervexProbe:1` to `options.txt`; launch to the title screen; change any setting; exit; "
              "diff the whole file and check whether `ephervexProbe` survived. Repeat on 1.12.2 and 1.16.5, "
              "since the legacy writer rebuilds the file from scratch.",
              "- **Sound category defaults.** Minecraft 1.12.2: clean profile; do not touch audio; exit; "
              "diff for the presence of any `soundCategory_*` line. This settles whether unmodified defaults "
              "are written at all in the legacy era.",
              "- **Runtime-computed defaults.** Minecraft 26.2: clean profile on two machines with different "
              "monitors; diff only `fullscreenResolution` and `soundDevice` to confirm they are environment "
              "derived and must not be synchronised."]

    lines += ["", "## Language catalog", "",
              "The client JAR embeds only `assets/minecraft/lang/en_us.json`. `LanguageManager` builds its "
              "catalog by reading the `language` metadata section from every resource pack (`pack.mcmeta`), "
              "whose entries carry `region`, `name` and an optional `bidirectional` flag."]
    if languages:
        complete = languages.get("complete")
        lines += ["",
                  f"The catalog is now resolved from that exact `pack.mcmeta` asset object, cached per release "
                  f"by `fetch_language_catalogs.py`. Canonical {languages['canonical_version']} exposes "
                  f"**{languages['count']}** languages"
                  + (" and the catalog is complete." if complete else " (catalog incomplete).")]
        per = languages.get("per_version_catalog") or {}
        counts = [(v, d.get("count")) for v, d in per.items() if d.get("count") is not None]
        if counts:
            counts.sort(key=lambda x: version_key(x[0]))
            lines += ["",
                      f"Per-release catalog sizes range from **{min(c for _, c in counts)}** "
                      f"({counts[0][0]}) to **{max(c for _, c in counts)}** ({counts[-1][0]}). "
                      "`languages.json` lists every release's codes together with the asset-index and "
                      "`pack.mcmeta` hashes that prove them, plus which canonical codes are missing from each "
                      "older release.",
                      "",
                      "A language code may therefore only be written to an older release if that release's own "
                      "catalog contains it; the option key `lang` and its plain-string encoding are otherwise "
                      "stable everywhere."]

    lines += ["", "## Inventory", "", "| Path | Version | Era | Keys | Bindings | SHA-256 | options.txt root |",
              "|---|---|---|---:|---:|---|---|"]
    norm_by_id = {n["jar_id"]: n for n in normalized}
    for x in inventory:
        analysis = x.get("options_analysis", {})
        roots = ", ".join(analysis.get("roots", [])) or "-"
        norm = norm_by_id.get(x["jar_id"])
        keys = len(analysis.get("options") or {})
        binds = len(analysis.get("keybinds") or [])
        lines.append(f"| `{x['path']}` | {x.get('detected_version') or 'unresolved'} | "
                     f"{analysis.get('persistence_era') or '-'} | {keys} | {binds} | "
                     f"`{x['sha256'][:16]}…` | `{roots}` |")

    lines += ["", "## Re-run", "", "```bash", command, "```", ""]
    return "\n".join(lines) + "\n"


def one_record_per_release(inventory: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """The inventory with one record per JAR and at most one JAR per detected release.

    Copies of the same bytes collapse into the first by path. Two different JARs of one release
    leave the schema mapping undecided, so they are refused rather than ranked.
    """
    by_id: dict[str, dict[str, Any]] = {}
    for rec in inventory:
        if by_id.setdefault(rec["jar_id"], rec) is not rec:
            print(f"[analyzer] {rec['path']} is a copy of {by_id[rec['jar_id']]['path']}; skipped", flush=True)
    by_version: dict[str, list[str]] = defaultdict(list)
    for rec in by_id.values():
        if rec.get("detected_version"):
            by_version[rec["detected_version"]].append(rec["path"])
    conflicts = [f"{version} comes from {', '.join(paths)}"
                 for version, paths in sorted(by_version.items()) if len(paths) > 1]
    if conflicts:
        raise InputError("more than one JAR was detected for a release (" + "; ".join(conflicts) + ")")
    return list(by_id.values())


def prune_stale_records(out: Path, jar_ids: set[str]) -> None:
    """Drops raw and normalized records of JARs this run did not analyze, so one run's output is only its own."""
    for kind in ("raw", "normalized"):
        for path in sorted((out / kind).glob("*.json")):
            if path.stem not in jar_ids:
                path.unlink()


# ---------------------------------------------------------------------------

def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path("."), help="Directory recursively containing input JARs")
    parser.add_argument(
        "--input-jar",
        type=Path,
        help="analyze exactly this JAR instead of discovering every JAR under --root",
    )
    parser.add_argument("--output", type=Path, default=Path(__file__).resolve().parent / "output")
    parser.add_argument("--assets-cache", type=Path,
                        default=Path(__file__).resolve().parent / "assets-cache",
                        help="Offline language-catalog cache populated by fetch_language_catalogs.py")
    args = parser.parse_args()
    root, out = args.root.resolve(), args.output.resolve()
    if not root.is_dir():
        parser.error(f"--root is not a directory: {root}")
    if args.input_jar is not None:
        # Only the directory is resolved: a JAR symlink stays where it sits below --root.
        jar = args.input_jar.parent.resolve() / args.input_jar.name
        if not jar.is_file() or jar.suffix.lower() != ".jar":
            parser.error(f"--input-jar is not a JAR file: {jar}")
        try:
            jar_path_in_root(jar, root)
        except InputError as exc:
            parser.error(f"--input-jar {exc}")
        jars = [jar]
    else:
        jars = discover(root, out)
        if not jars:
            print(f"[analyzer] no JAR files found under {root}; the output was left unchanged",
                  file=sys.stderr, flush=True)
            return 1
    inventory: list[dict[str, Any]] = []
    print(f"[analyzer] inventory: {len(jars)} JARs", flush=True)
    for index, path in enumerate(jars, 1):
        rec = analyze_jar(path, root)
        inventory.append(rec)
        analysis = rec.get("options_analysis", {})
        print(f"[{index:02d}/{len(jars):02d}] {path.name}: {rec.get('detected_version') or 'unresolved'}, "
              f"era={analysis.get('persistence_era')}, {len(analysis.get('options') or {})} keys, "
              f"{len(analysis.get('keybinds') or [])} bindings, {analysis.get('javap_calls', 0)} javap calls",
              flush=True)
    inventory.sort(key=lambda x: x["path"])
    try:
        inventory = one_record_per_release(inventory)
    except InputError as exc:
        print(f"[analyzer] {exc}; the output was left unchanged", file=sys.stderr, flush=True)
        return 1
    stable_json(out / "inventory.json", {"schema_version": ANALYSIS_SCHEMA_VERSION, "tool_version": TOOL_VERSION,
                                         "root": str(root), "jar_count": len(inventory), "jars": inventory})

    candidates = [x for x in inventory if supported(x.get("detected_version")) and x.get("stable_release")]
    newest = max(candidates, key=lambda x: version_key(x["detected_version"])) if candidates else None

    normalized: list[dict[str, Any]] = []
    for rec in inventory:
        stable_json(out / "raw" / f"{rec['jar_id']}.json", {"schema_version": ANALYSIS_SCHEMA_VERSION, "tool_version": TOOL_VERSION, **rec})
        norm = normalize(rec)
        normalized.append(norm)
        stable_json(out / "normalized" / f"{rec['jar_id']}.json", norm)

    normalized_supported = sorted([x for x in normalized if supported(x.get("minecraft_version"))],
                                  key=lambda x: version_key(x["minecraft_version"]))
    latest_norm = next((x for x in normalized if newest and x["jar_id"] == newest["jar_id"]), None)

    languages = None
    matrix = None
    crosscheck = keycode_crosscheck(normalized_supported) if normalized_supported else None
    canonical_keys = {s["persisted_key"] for s in latest_norm["settings"]} if latest_norm else set()
    changes = schema_changes(normalized_supported, canonical_keys)
    if crosscheck:
        changes["keycode_crosscheck"] = crosscheck
    stable_json(out / "schema-changes.json", changes)

    if latest_norm and newest:
        stable_json(out / "latest-schema.json", make_latest_schema(latest_norm))
        matrix = compatibility(latest_norm, normalized_supported)
        stable_json(out / "compatibility-matrix.json", matrix)
        cache = load_language_cache(args.assets_cache.resolve(),
                                    [x["minecraft_version"] for x in normalized_supported])
        languages = language_output(newest, root / newest["path"], latest_norm, cache,
                                    [x["minecraft_version"] for x in normalized_supported])
        stable_json(out / "languages.json", languages)
    else:
        stable_json(out / "latest-schema.json", {"error": "No confidently identified stable supported release"})
        stable_json(out / "compatibility-matrix.json", {"error": "No canonical release"})
        stable_json(out / "languages.json", {"error": "No canonical release"})

    selected = f" --input-jar {jars[0]}" if args.input_jar is not None else ""
    cmd = f"python3 {Path(__file__).resolve()} --root {root}{selected} --output {out}"
    (out / "report.md").write_text(
        report(inventory, latest_norm, normalized_supported, languages, changes, matrix, crosscheck, cmd),
        encoding="utf-8")
    prune_stale_records(out, {x["jar_id"] for x in inventory})
    print(f"[analyzer] complete: {out}", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
