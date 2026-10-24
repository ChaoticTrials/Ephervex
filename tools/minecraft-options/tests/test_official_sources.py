#!/usr/bin/env python3
"""Offline tests for verified downloads."""

from __future__ import annotations

import hashlib
import io
import json
import tempfile
import unittest
import urllib.request
from contextlib import redirect_stderr
from pathlib import Path
from unittest import mock

import file_io
import official_sources
from official_sources import SourceError


def sha1(body: bytes) -> str:
    return hashlib.sha1(body).hexdigest()


def encode(value: object) -> bytes:
    return json.dumps(value).encode()


class FakeMojang:
    """In-memory official sources, keyed by url. Unknown urls fail the test."""

    def __init__(self) -> None:
        self.bodies: dict[str, bytes] = {}
        self.requests: list[str] = []

    def publish(self, url: str, body: bytes) -> str:
        self.bodies[url] = body
        return sha1(body)

    def fetch(self, url: str, timeout: int = 120) -> bytes:
        self.requests.append(url)
        if url not in self.bodies:
            raise AssertionError(f"unexpected download of {url}")
        return self.bodies[url]


def manifest_document(*entries: dict) -> dict:
    return {
        "latest": {"release": entries[-1]["id"] if entries else "", "snapshot": ""},
        "versions": list(entries),
    }


class HttpsOnlyDownloads(unittest.TestCase):
    def test_a_redirect_to_plain_http_is_refused(self):
        handler = official_sources._HttpsOnlyRedirects()
        request = urllib.request.Request("https://piston-meta.mojang.com/a")
        with self.assertRaises(SourceError):
            handler.redirect_request(request, None, 302, "Found", {}, "http://evil.example/a")

    def test_a_redirect_to_another_https_url_is_followed(self):
        handler = official_sources._HttpsOnlyRedirects()
        request = urllib.request.Request("https://piston-meta.mojang.com/a")
        followed = handler.redirect_request(request, None, 302, "Found", {}, "https://piston-data.mojang.com/a")
        self.assertEqual(followed.full_url, "https://piston-data.mojang.com/a")

    def test_a_non_https_url_is_never_fetched(self):
        with self.assertRaises(SourceError):
            official_sources.fetch("http://piston-meta.mojang.com/a")

    def test_a_body_must_match_the_published_sha1(self):
        mojang = FakeMojang()
        digest = mojang.publish("https://x.example/a", b"payload")
        with mock.patch.object(official_sources, "fetch", mojang.fetch):
            self.assertEqual(official_sources.fetch_verified("https://x.example/a", digest, "a"), b"payload")
            with self.assertRaisesRegex(SourceError, "failed its sha1 check"):
                official_sources.fetch_verified("https://x.example/a", "0" * 40, "a")

    def test_a_missing_or_malformed_sha1_is_a_hard_failure_before_any_download(self):
        mojang = FakeMojang()
        mojang.publish("https://x.example/a", b"payload")
        with mock.patch.object(official_sources, "fetch", mojang.fetch):
            for bad in (None, "", "abc", "z" * 40, 7):
                with self.subTest(sha1=bad), self.assertRaisesRegex(SourceError, "no valid sha1"):
                    official_sources.fetch_verified("https://x.example/a", bad, "a")
        self.assertEqual(mojang.requests, [])

    def test_identifiers_that_could_escape_a_cache_directory_are_refused(self):
        for bad in ("", "../x", "a/b", "1..2", ".hidden", "a b", "x" * 65, None, 12):
            with self.subTest(identifier=bad), self.assertRaises(SourceError):
                official_sources.require_version_id(bad)
        for good in ("1.12.2", "26.2", "1.21.11", "b1.8.1"):
            self.assertEqual(official_sources.require_version_id(good), good)


class SharedVersionManifest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.path = Path(self.directory.name) / "cache" / official_sources.MANIFEST_NAME
        self.mojang = FakeMojang()
        patcher = mock.patch.object(official_sources, "fetch", self.mojang.fetch)
        patcher.start()
        self.addCleanup(patcher.stop)

    def publish_manifest(self, *versions: str) -> None:
        entries = [{"id": v, "type": "release", "url": f"https://x.example/{v}.json"} for v in versions]
        self.mojang.bodies[official_sources.MANIFEST_URL] = encode(manifest_document(*entries))

    def test_a_cached_manifest_is_reused_while_it_lists_every_needed_release(self):
        self.publish_manifest("26.2")
        official_sources.load_manifest(self.path, online=True)
        self.mojang.requests.clear()
        document = official_sources.load_manifest(self.path, online=True, needs=["26.2"])
        self.assertEqual(document["latest"]["release"], "26.2")
        self.assertEqual(self.mojang.requests, [])

    def test_a_cached_manifest_missing_a_needed_release_is_downloaded_again(self):
        self.publish_manifest("26.2")
        official_sources.load_manifest(self.path, online=True)
        self.publish_manifest("26.2", "26.3")
        document = official_sources.load_manifest(self.path, online=True, needs=["26.3"])
        self.assertEqual([v["id"] for v in document["versions"]], ["26.2", "26.3"])
        self.assertIn("26.3", self.path.read_text())

    def test_a_refresh_downloads_even_when_the_cache_lists_everything(self):
        self.publish_manifest("26.2")
        official_sources.load_manifest(self.path, online=True)
        self.mojang.requests.clear()
        official_sources.load_manifest(self.path, online=True, refresh=True)
        self.assertEqual(self.mojang.requests, [official_sources.MANIFEST_URL])

    def test_offline_never_downloads_and_uses_a_stale_cache_as_is(self):
        self.publish_manifest("26.2")
        official_sources.load_manifest(self.path, online=True)
        self.mojang.requests.clear()
        document = official_sources.load_manifest(self.path, online=False, refresh=True, needs=["26.3"])
        self.assertEqual([v["id"] for v in document["versions"]], ["26.2"])
        self.path.unlink()
        with self.assertRaises(SourceError):
            official_sources.load_manifest(self.path, online=False)
        self.assertEqual(self.mojang.requests, [])

    def test_a_malformed_download_never_replaces_the_cache(self):
        self.publish_manifest("26.2")
        official_sources.load_manifest(self.path, online=True)
        good = self.path.read_bytes()
        for body in (b"not json", b"[]", encode({"latest": {}, "versions": []})):
            self.mojang.bodies[official_sources.MANIFEST_URL] = body
            with self.subTest(body=body), self.assertRaises(SourceError):
                official_sources.load_manifest(self.path, online=True, refresh=True)
            self.assertEqual(self.path.read_bytes(), good)

    def test_an_unreadable_cached_manifest_is_reported_and_replaced_by_a_download(self):
        self.path.parent.mkdir(parents=True)
        self.path.write_text("{truncated")
        self.publish_manifest("26.2")
        stderr = io.StringIO()
        with redirect_stderr(stderr):
            document = official_sources.load_manifest(self.path, online=True)
        self.assertEqual(document["latest"]["release"], "26.2")
        self.assertIn("is damaged", stderr.getvalue())
        self.assertEqual(json.loads(self.path.read_text())["latest"]["release"], "26.2")

    def test_a_damaged_cached_manifest_offline_is_an_error_naming_the_cache(self):
        self.path.parent.mkdir(parents=True)
        self.path.write_text("{truncated")
        with redirect_stderr(io.StringIO()), self.assertRaisesRegex(SourceError, "no usable cached version manifest"):
            official_sources.load_manifest(self.path, online=False)

    def test_an_interrupted_cache_write_keeps_the_previous_manifest(self):
        self.publish_manifest("26.2")
        official_sources.load_manifest(self.path, online=True)
        good = self.path.read_bytes()
        self.publish_manifest("26.3")
        with mock.patch.object(file_io.os, "replace", side_effect=OSError("killed")):
            with self.assertRaises(OSError):
                official_sources.load_manifest(self.path, online=True, refresh=True)
        self.assertEqual(self.path.read_bytes(), good)
        self.assertEqual([p.name for p in self.path.parent.iterdir()], [self.path.name])
