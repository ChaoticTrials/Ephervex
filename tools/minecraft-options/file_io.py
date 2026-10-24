"""File access shared by the options maintainer scripts.

Every file these scripts generate or cache is written through `write_atomic`,
so a killed run leaves either the previous file or the complete new one, never
a truncated file that the next run cannot read.
"""

from __future__ import annotations

import json
import os
import secrets
from pathlib import Path
from typing import Any


def write_atomic(path: Path, content: str | bytes) -> None:
    """Writes `content` to `path` through a temporary file in the same directory.

    The temporary file is flushed to disk before it is renamed over `path`, and
    it is removed again when the write fails. Parent directories are created.
    """
    data = content.encode("utf-8") if isinstance(content, str) else content
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.{secrets.token_hex(4)}.tmp")
    try:
        # Mode 0o666 lets the process umask decide, like an ordinary file write.
        descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o666)
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(data)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
    except BaseException:
        temporary.unlink(missing_ok=True)
        raise


def stable_json(path: Path, value: Any) -> None:
    """Writes `value` as sorted, indented, newline-terminated JSON, atomically."""
    write_atomic(path, json.dumps(value, indent=2, ensure_ascii=False, sort_keys=True) + "\n")


def read_text_or_empty(path: Path) -> str:
    """The text of `path`, or "" when it does not exist.

    Undecodable bytes are replaced rather than raised, so a damaged file simply
    compares unequal to the text it is checked against.
    """
    try:
        return path.read_text(encoding="utf-8", errors="replace")
    except FileNotFoundError:
        return ""


def read_json_object(path: Path, what: str) -> dict[str, Any]:
    """The JSON object stored at `path`.

    Raises `ValueError` naming `what` and `path` when the file cannot be read,
    is not valid JSON, or is not an object, so a caller can report the damaged
    file instead of a decoder traceback.
    """
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        raise ValueError(f"{what} {path} cannot be read: {error}") from error
    if not isinstance(value, dict):
        raise ValueError(f"{what} {path} is not a JSON object")
    return value
