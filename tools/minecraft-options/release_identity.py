#!/usr/bin/env python3
"""What counts as a Minecraft release identifier, shared by the maintainer tools.

A release is `1.N` or `1.N.P`, or a calendar release `YY.N` or `YY.N.P` from
26.1 onward. Snapshots, pre-releases and release candidates are not releases,
and neither is any string that merely resembles one (`10.5`, `1.21.5-rc1`,
`1.٢`). Ordering is defined only for releases: an identifier that is not one is
refused instead of being given zero components.
"""

from __future__ import annotations

import re

_NUMBER = r"(?:0|[1-9][0-9]*)"
_CALENDAR_YEAR = r"(?:2[6-9]|[3-9][0-9])"
RELEASE_PATTERN = rf"(?:1\.{_NUMBER}(?:\.{_NUMBER})?|{_CALENDAR_YEAR}\.{_NUMBER}(?:\.{_NUMBER})?)"
RELEASE_RE = re.compile(rf"^{RELEASE_PATTERN}$")

# Calendar releases follow the 1.21 line.
_CALENDAR_ERA = 2
_LEGACY_ERA = 1


def is_release(version: object) -> bool:
    return isinstance(version, str) and RELEASE_RE.fullmatch(version) is not None


def version_key(version: str) -> tuple[int, ...]:
    """Ordering key for a release identifier; raises ValueError for anything else."""
    if not is_release(version):
        raise ValueError(f"`{version}` is not a release identifier")
    numbers = tuple(int(piece) for piece in version.split("."))
    return (_CALENDAR_ERA if numbers[0] >= 20 else _LEGACY_ERA, *numbers)
