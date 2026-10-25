#!/usr/bin/env python3
"""Runs the small lambdas an enum's text codec is compiled from.

A release that stores an enum as text supplies one lambda that reads a constant
from text and one that writes a constant as text. javac turns them into string
and ordinal switches, and into `$SwitchMap` lookups held by a synthetic class,
so no instruction pairs a constant with its token. The pairs are found by
running both lambdas on every constant and requiring that the reader returns
the constant the writer was given.

Only the bytecode these lambdas are made of is understood. Anything else
(an unknown call, a branch that does not finish, a missing method) raises
`Unsupported`, and the caller then proves nothing instead of guessing.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from typing import Any, Protocol

STEP_LIMIT = 5000
CALL_DEPTH_LIMIT = 8

MOJANG_EITHER = "com/mojang/datafixers/util/Either"
INDY_COMMENT = re.compile(r"^InvokeDynamic #(\d+):[\w$<>]+:\((.*?)\)(\S+)$")
LOCAL_OP = re.compile(r"^([ai])(load|store)(?:_([0-3]))?$")
INT_CONSTANTS = {"iconst_m1": -1, "iconst_0": 0, "iconst_1": 1, "iconst_2": 2,
                 "iconst_3": 3, "iconst_4": 4, "iconst_5": 5}
UNARY_BRANCHES = {"ifeq": lambda v: v == 0, "ifne": lambda v: v != 0, "iflt": lambda v: v < 0,
                  "ifge": lambda v: v >= 0, "ifgt": lambda v: v > 0, "ifle": lambda v: v <= 0}
BINARY_BRANCHES = {"if_icmpeq": lambda a, b: a == b, "if_icmpne": lambda a, b: a != b,
                   "if_icmplt": lambda a, b: a < b, "if_icmpge": lambda a, b: a >= b,
                   "if_icmpgt": lambda a, b: a > b, "if_icmple": lambda a, b: a <= b}


class Unsupported(Exception):
    """The lambdas use bytecode this evaluator does not model."""


@dataclass(frozen=True)
class Constant:
    """One constant of the enum being proven, named by its field."""
    key: str


@dataclass(frozen=True)
class Either:
    side: str
    value: Any


@dataclass(frozen=True)
class Lambda:
    owner: str
    name: str
    descriptor: str


@dataclass(frozen=True)
class SwitchMap:
    """A `$SwitchMap` array: the case number each constant's ordinal selects."""
    enum_class: str
    cases: dict[str, int]


@dataclass
class Routine:
    insns: list[Any]
    # Offset of a switch instruction -> (case value -> target offset, default target).
    switches: dict[int, tuple[dict[int, int], int]]
    static: bool


class World(Protocol):
    def routine(self, owner: str, name: str, descriptor: str) -> Routine | None: ...

    def bootstrap(self, owner: str, index: int) -> tuple[str, str, str] | None: ...

    def switch_map(self, owner: str, field: str) -> SwitchMap | None: ...


def java_string_hash(text: str) -> int:
    """`String.hashCode()`, which a string switch compares before it calls `equals`."""
    units = text.encode("utf-16-be")
    value = 0
    for i in range(0, len(units), 2):
        value = (31 * value + int.from_bytes(units[i:i + 2], "big")) & 0xFFFFFFFF
    return value - (1 << 32) if value >= 1 << 31 else value


class Evaluator:
    def __init__(self, world: World, enum_class: str, constants: list[dict[str, Any]]) -> None:
        self.world = world
        self.enum_class = enum_class
        self.ordinals = {constant_key(c): c["ordinal"] for c in constants}
        self.by_ordinal = {ordinal: key for key, ordinal in self.ordinals.items()}
        self.steps = 0

    def call(self, owner: str, name: str, descriptor: str, args: list[Any], depth: int = 0) -> Any:
        if depth > CALL_DEPTH_LIMIT:
            raise Unsupported("calls nest too deeply")
        routine = self.world.routine(owner, name, descriptor)
        if routine is None or not routine.static:
            raise Unsupported(f"{owner}.{name}{descriptor} is not a static method of the analyzed classes")
        return self.run(owner, routine, args, depth)

    def apply(self, function: Any, argument: Any, depth: int) -> Any:
        if not isinstance(function, Lambda):
            raise Unsupported("a function argument is not a lambda")
        return self.call(function.owner, function.name, function.descriptor, [argument], depth + 1)

    def run(self, owner: str, routine: Routine, args: list[Any], depth: int) -> Any:
        insns = routine.insns
        at = {ins.off: i for i, ins in enumerate(insns)}
        local: dict[int, Any] = dict(enumerate(args))
        stack: list[Any] = []

        def jump(target: str | int) -> int:
            try:
                return at[int(target)]
            except (KeyError, ValueError):
                raise Unsupported("a branch leaves the method") from None

        pc = 0
        while True:
            self.steps += 1
            if self.steps > STEP_LIMIT or pc >= len(insns):
                raise Unsupported("the method does not finish")
            ins = insns[pc]
            op = ins.op
            pc += 1
            slot = LOCAL_OP.match(op)
            if slot:
                index = int(slot.group(3) if slot.group(3) is not None else ins.arg)
                if slot.group(2) == "load":
                    if index not in local:
                        raise Unsupported("a local is read before it is written")
                    stack.append(local[index])
                else:
                    local[index] = stack.pop()
            elif op in INT_CONSTANTS:
                stack.append(INT_CONSTANTS[op])
            elif op in ("bipush", "sipush"):
                stack.append(int(ins.arg))
            elif op in ("ldc", "ldc_w"):
                text = ins.string()
                number = re.fullmatch(r"int (-?\d+)", ins.comment)
                if text is not None:
                    stack.append(text)
                elif number:
                    stack.append(int(number.group(1)))
                else:
                    raise Unsupported("a constant that is neither a string nor an int")
            elif op in ("nop", "checkcast"):
                pass
            elif op == "dup":
                stack.append(stack[-1])
            elif op == "pop":
                stack.pop()
            elif op == "goto":
                pc = jump(ins.arg)
            elif op in UNARY_BRANCHES:
                value = stack.pop()
                if not isinstance(value, int):
                    raise Unsupported("an int branch on a value that is not an int")
                if UNARY_BRANCHES[op](value):
                    pc = jump(ins.arg)
            elif op in BINARY_BRANCHES:
                right, left = stack.pop(), stack.pop()
                if not (isinstance(left, int) and isinstance(right, int)):
                    raise Unsupported("an int comparison on values that are not ints")
                if BINARY_BRANCHES[op](left, right):
                    pc = jump(ins.arg)
            elif op in ("if_acmpeq", "if_acmpne"):
                right, left = stack.pop(), stack.pop()
                if not (isinstance(left, Constant) and isinstance(right, Constant)):
                    raise Unsupported("a reference comparison of something other than enum constants")
                if (left == right) == (op == "if_acmpeq"):
                    pc = jump(ins.arg)
            elif op in ("tableswitch", "lookupswitch"):
                table = routine.switches.get(ins.off)
                value = stack.pop()
                if table is None or not isinstance(value, int):
                    raise Unsupported("a switch whose table was not read")
                cases, default = table
                pc = jump(cases.get(value, default))
            elif op == "getstatic":
                stack.append(self.get_static(owner, ins))
            elif op == "iaload":
                index, array = stack.pop(), stack.pop()
                if not isinstance(array, SwitchMap) or not isinstance(index, int):
                    raise Unsupported("an array read other than a switch map")
                if index not in self.by_ordinal:
                    raise Unsupported("a switch map read with an ordinal no constant has")
                stack.append(array.cases.get(self.by_ordinal[index], 0))
            elif op in ("invokevirtual", "invokestatic"):
                self.invoke(ins, owner, op, stack, depth)
            elif op == "invokedynamic":
                stack.append(self.lambda_for(ins, owner))
            elif op in ("areturn", "ireturn"):
                return stack.pop()
            else:
                raise Unsupported(f"the instruction `{op}`")

    def get_static(self, owner: str, ins: Any) -> Any:
        fld = ins.field()
        if not fld:
            raise Unsupported("a static field that could not be read")
        cls, name, descriptor = fld
        if cls == self.enum_class and descriptor == f"L{self.enum_class};":
            if name not in self.ordinals:
                raise Unsupported("a static field of the enum that is not one of its constants")
            return Constant(name)
        if descriptor == "[I":
            found = self.world.switch_map(cls or owner, name)
            if found is None or found.enum_class != self.enum_class:
                raise Unsupported("a switch map that was not read")
            return found
        raise Unsupported(f"the static field {name}:{descriptor}")

    def lambda_for(self, ins: Any, owner: str) -> Lambda:
        m = INDY_COMMENT.match(ins.comment)
        if not m or m.group(2):
            raise Unsupported("an invokedynamic that captures values")
        target = self.world.bootstrap(owner, int(m.group(1)))
        if target is None:
            raise Unsupported("an invokedynamic whose lambda was not found")
        return Lambda(*target)

    def invoke(self, ins: Any, owner: str, op: str, stack: list[Any], depth: int) -> None:
        ref = ins.method()
        if not ref:
            raise Unsupported("a call that could not be read")
        cls, name, descriptor = ref
        if op == "invokestatic" and cls in (None, owner) and self.world.routine(owner, name, descriptor):
            args = [stack.pop() for _ in split_parameters(descriptor)][::-1]
            stack.append(self.call(owner, name, descriptor, args, depth + 1))
            return
        key = (cls, name, descriptor)
        if key == ("java/lang/String", "hashCode", "()I"):
            value = stack.pop()
            if not isinstance(value, str):
                raise Unsupported("hashCode of something other than a string")
            stack.append(java_string_hash(value))
        elif key == ("java/lang/String", "equals", "(Ljava/lang/Object;)Z"):
            other, value = stack.pop(), stack.pop()
            if not (isinstance(value, str) and isinstance(other, str)):
                raise Unsupported("a string compared with something other than a string")
            stack.append(int(value == other))
        elif key == ("java/lang/Boolean", "booleanValue", "()Z"):
            if not isinstance(stack[-1], int):
                raise Unsupported("booleanValue of something other than a boolean")
        elif key == ("java/lang/Boolean", "valueOf", "(Z)Ljava/lang/Boolean;"):
            pass
        elif name == "ordinal" and descriptor == "()I" and cls == self.enum_class:
            value = stack.pop()
            if not isinstance(value, Constant):
                raise Unsupported("ordinal of something other than an enum constant")
            stack.append(self.ordinals[value.key])
        elif cls == MOJANG_EITHER and op == "invokestatic" and name in ("left", "right"):
            stack.append(Either(name, stack.pop()))
        elif (cls == MOJANG_EITHER and name == "map"
              and descriptor == "(Ljava/util/function/Function;Ljava/util/function/Function;)Ljava/lang/Object;"):
            right, left, either = stack.pop(), stack.pop(), stack.pop()
            if not isinstance(either, Either):
                raise Unsupported("Either.map on something other than an Either")
            stack.append(self.apply(left if either.side == "left" else right, either.value, depth))
        else:
            raise Unsupported(f"the call {cls}.{name}{descriptor}")


def split_parameters(descriptor: str) -> list[str]:
    inner = descriptor[descriptor.index("(") + 1:descriptor.index(")")]
    return re.findall(r"\[*(?:L[^;]+;|[ZBCSIJFD])", inner)


def constant_key(constant: dict[str, Any]) -> str:
    return constant.get("field") or constant["name"]


def prove_enum_tokens(world: World, enum_class: str, constants: list[dict[str, Any]],
                      reader: tuple[str, str, str], writer: tuple[str, str, str],
                      wrapped: bool) -> dict[str, str] | None:
    """The token each constant is stored as, or None when that is not proven.

    Proven means: for every constant the writer returns one text token, and the
    reader maps what the writer returned back to that same constant. The
    writer's result is handed to the reader unchanged, so the pair agrees on
    their shared type. `wrapped` says the text travels as the right side of an
    `Either` (the codec is `Codec.either(Codec.BOOL, Codec.STRING)`); otherwise
    the writer returns the text itself.
    """
    evaluator = Evaluator(world, enum_class, constants)
    tokens: dict[str, str] = {}
    try:
        for constant in constants:
            key = constant_key(constant)
            written = evaluator.call(*writer, [Constant(key)])
            if wrapped:
                token = written.value if isinstance(written, Either) and written.side == "right" else None
            else:
                token = written
            if not isinstance(token, str) or not token:
                return None
            if evaluator.call(*reader, [written]) != Constant(key):
                return None
            tokens[key] = token
    except Unsupported:
        return None
    return tokens if len(set(tokens.values())) == len(tokens) else None
