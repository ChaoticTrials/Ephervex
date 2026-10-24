"""Verified access to Mojang's published version metadata and assets.

Both maintainer scripts download through this module. Every artifact is checked
against the strongest hash its source publishes, and a missing or malformed hash
is as fatal as a mismatch. Identifiers read from downloaded documents are
validated before they become part of a path or URL.
"""

from __future__ import annotations

import hashlib
import json
import re
import sys
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Any, Iterable

from file_io import write_atomic

TOOL_DIR = Path(__file__).resolve().parent
DEFAULT_CACHE = TOOL_DIR / ".cache"

MANIFEST_NAME = "version_manifest_v2.json"
MANIFEST_URL = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json"
RESOURCES_URL = "https://resources.download.minecraft.net"
USER_AGENT = "ephervex-minecraft-options/1 (static options.txt analysis)"

_SHA1 = re.compile(r"[0-9a-f]{40}")
_VERSION_ID = re.compile(r"[0-9A-Za-z][0-9A-Za-z._-]*")


class SourceError(Exception):
    """An official source supplied data that cannot be trusted or used."""


def require_version_id(value: Any) -> str:
    """A release id that is safe to use as a file name component."""
    if (
        not isinstance(value, str)
        or len(value) > 64
        or ".." in value
        or not _VERSION_ID.fullmatch(value)
    ):
        raise SourceError(f"{value!r} is not a valid Minecraft version id")
    return value


def require_sha1(value: Any, what: str) -> str:
    """The lowercase hex SHA-1 a source published for `what`."""
    if not isinstance(value, str) or not _SHA1.fullmatch(value.lower()):
        raise SourceError(f"{what} publishes no valid sha1")
    return value.lower()


def require_https_url(value: Any, what: str) -> str:
    if not isinstance(value, str):
        raise SourceError(f"{what} publishes no url")
    parts = urllib.parse.urlsplit(value)
    if parts.scheme != "https" or not parts.netloc:
        raise SourceError(f"{what} publishes a url that is not https")
    return value


class _HttpsOnlyRedirects(urllib.request.HTTPRedirectHandler):
    """urllib follows an https to http redirect unless told otherwise."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):  # noqa: ANN001
        if urllib.parse.urlsplit(newurl).scheme != "https":
            raise SourceError(f"refusing a redirect to a non-https url: {newurl}")
        return super().redirect_request(req, fp, code, msg, headers, newurl)


_OPENER = urllib.request.build_opener(_HttpsOnlyRedirects)


def fetch(url: str, timeout: int = 120) -> bytes:
    if urllib.parse.urlsplit(url).scheme != "https":
        raise SourceError(f"refusing to fetch over a non-https url: {url}")
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    with _OPENER.open(request, timeout=timeout) as response:  # noqa: S310
        return response.read()


def fetch_verified(url: Any, sha1: Any, what: str, timeout: int = 120) -> bytes:
    """The body at `url`, only if it matches the SHA-1 its source published."""
    expected = require_sha1(sha1, what)
    body = fetch(require_https_url(url, what), timeout)
    if hashlib.sha1(body).hexdigest() != expected:  # noqa: S324 - Mojang publishes sha1
        raise SourceError(f"{what} failed its sha1 check")
    return body


def json_object(body: bytes, what: str) -> dict[str, Any]:
    try:
        value = json.loads(body)
    except ValueError as error:
        raise SourceError(f"{what} is not valid JSON") from error
    if not isinstance(value, dict):
        raise SourceError(f"{what} is not a JSON object")
    return value


def _is_manifest(document: dict[str, Any]) -> bool:
    latest = document.get("latest")
    versions = document.get("versions")
    return (
        isinstance(latest, dict)
        and isinstance(latest.get("release"), str)
        and isinstance(versions, list)
        and all(
            isinstance(entry, dict) and all(isinstance(entry.get(key), str) for key in ("id", "type", "url"))
            for entry in versions
        )
    )


def _read_cached_manifest(path: Path) -> dict[str, Any] | None:
    try:
        body = path.read_bytes()
    except FileNotFoundError:
        return None
    except OSError as error:
        print(f"[cache] cannot read {path}: {error}; fetching the version manifest again", file=sys.stderr)
        return None
    try:
        document = json_object(body, "the cached version manifest")
    except SourceError:
        document = None
    if document is None or not _is_manifest(document):
        print(f"[cache] {path} is damaged; fetching the version manifest again", file=sys.stderr)
        return None
    return document


def load_manifest(
    path: Path, *, online: bool, refresh: bool = False, needs: Iterable[str] = ()
) -> dict[str, Any]:
    """Mojang's version manifest from the cache at `path`.

    The manifest is downloaded when the cache lacks a usable copy, when `refresh`
    asks for one, or when the cached copy does not list a release in `needs`.
    Mojang publishes no hash for the manifest itself, so a download is checked
    for shape before it replaces the cache. Offline, only the cached copy is
    used, even if it is stale.
    """
    cached = None if refresh and online else _read_cached_manifest(path)
    if cached is not None:
        known = {entry["id"] for entry in cached["versions"]}
        if not online or all(version in known for version in needs):
            return cached
    if not online:
        raise SourceError(f"no usable cached version manifest at {path}")
    body = fetch(MANIFEST_URL)
    document = json_object(body, "Mojang's version manifest")
    if not _is_manifest(document):
        raise SourceError("Mojang's version manifest is malformed")
    write_atomic(path, body)
    return document
