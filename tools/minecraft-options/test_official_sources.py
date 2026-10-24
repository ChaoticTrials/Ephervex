#!/usr/bin/env python3
"""Offline tests for verified downloads and the language catalog refresh."""

from __future__ import annotations

import hashlib
import io
import json
import tempfile
import unittest
import urllib.request
from contextlib import redirect_stdout
from pathlib import Path
from unittest import mock
from zipfile import ZipFile

import analyze_options
import fetch_language_catalogs
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

    def publish_release(self, version: str, languages: dict | None = None, mcmeta: bool = True) -> dict:
        """Publishes a release's metadata, asset index and pack.mcmeta; returns its manifest entry."""
        objects: dict[str, dict] = {}
        if mcmeta:
            blob = encode({"language": languages or {"en_us": {"name": "English", "region": "US"}}})
            digest = self.publish(f"{official_sources.RESOURCES_URL}/{sha1(blob)[:2]}/{sha1(blob)}", blob)
            objects["pack.mcmeta"] = {"hash": digest, "size": len(blob)}
        objects["minecraft/lang/en_us.json"] = {"hash": "0" * 40, "size": 1}
        index_url = f"https://piston-meta.example/{version}/index.json"
        index_sha1 = self.publish(index_url, encode({"objects": objects}))
        meta_url = f"https://piston-meta.example/{version}.json"
        meta_sha1 = self.publish(
            meta_url, encode({"assetIndex": {"id": version, "url": index_url, "sha1": index_sha1}})
        )
        return {"id": version, "type": "release", "url": meta_url, "sha1": meta_sha1}

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

    def test_an_unreadable_cached_manifest_is_replaced_by_a_download(self):
        self.path.parent.mkdir(parents=True)
        self.path.write_text("{truncated")
        self.publish_manifest("26.2")
        document = official_sources.load_manifest(self.path, online=True)
        self.assertEqual(document["latest"]["release"], "26.2")


class LanguageCatalogCollection(unittest.TestCase):
    def setUp(self):
        self.mojang = FakeMojang()
        patcher = mock.patch.object(official_sources, "fetch", self.mojang.fetch)
        patcher.start()
        self.addCleanup(patcher.stop)

    def test_a_release_with_verified_documents_yields_its_language_catalog(self):
        entry = self.mojang.publish_release("26.2", {"en_us": {"name": "English", "region": "US"}})
        record = fetch_language_catalogs.collect("26.2", entry)
        self.assertEqual(record["status"], "ok")
        self.assertEqual(record["language_count"], 1)
        self.assertTrue(record["pack_mcmeta"]["sha1_matches"])

    def test_a_tampered_pack_mcmeta_is_a_hard_failure(self):
        entry = self.mojang.publish_release("26.2")
        for url in list(self.mojang.bodies):
            if url.startswith(official_sources.RESOURCES_URL):
                self.mojang.bodies[url] = encode({"language": {"xx_xx": {"name": "Evil"}}})
        with self.assertRaisesRegex(SourceError, "pack.mcmeta of 26.2 failed its sha1 check"):
            fetch_language_catalogs.collect("26.2", entry)

    def test_a_tampered_asset_index_or_version_metadata_is_a_hard_failure(self):
        for url_suffix, message in (("/index.json", "asset index of 26.2"), ("26.2.json", "version metadata for 26.2")):
            mojang = FakeMojang()
            entry = mojang.publish_release("26.2")
            for url in mojang.bodies:
                if url.endswith(url_suffix):
                    mojang.bodies[url] = mojang.bodies[url] + b" "
            with self.subTest(document=message), mock.patch.object(official_sources, "fetch", mojang.fetch):
                with self.assertRaisesRegex(SourceError, f"{message} failed its sha1 check"):
                    fetch_language_catalogs.collect("26.2", entry)

    def test_a_manifest_entry_without_a_sha1_is_refused_before_download(self):
        entry = self.mojang.publish_release("26.2")
        del entry["sha1"]
        with self.assertRaisesRegex(SourceError, "no valid sha1"):
            fetch_language_catalogs.collect("26.2", entry)
        self.assertEqual(self.mojang.requests, [])

    def test_a_pack_mcmeta_hash_cannot_redirect_the_resource_url(self):
        entry = self.mojang.publish_release("26.2")
        index_url = next(url for url in self.mojang.bodies if url.endswith("/index.json"))
        index = json.loads(self.mojang.bodies[index_url])
        index["objects"]["pack.mcmeta"]["hash"] = "../../evil.example/payload"
        digest = self.mojang.publish(index_url, encode(index))
        meta_url = entry["url"]
        meta = json.loads(self.mojang.bodies[meta_url])
        meta["assetIndex"]["sha1"] = digest
        entry["sha1"] = self.mojang.publish(meta_url, encode(meta))
        with self.assertRaisesRegex(SourceError, "no valid sha1"):
            fetch_language_catalogs.collect("26.2", entry)
        self.assertFalse(any("evil" in url for url in self.mojang.requests))


class LanguageCatalogRefresh(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.cache = Path(self.directory.name)
        self.mojang = FakeMojang()
        patcher = mock.patch.object(official_sources, "fetch", self.mojang.fetch)
        patcher.start()
        self.addCleanup(patcher.stop)

    def refresh(self, document: dict, versions: list[str], force: bool = False):
        with redirect_stdout(io.StringIO()):
            return fetch_language_catalogs.refresh(self.cache, document, versions, force)

    def test_a_current_verified_record_is_reused_without_downloading(self):
        entry = self.mojang.publish_release("26.2")
        document = manifest_document(entry)
        self.refresh(document, ["26.2"])
        self.mojang.requests.clear()
        records, failures = self.refresh(document, ["26.2"])
        self.assertEqual((list(records), failures), (["26.2"], {}))
        self.assertEqual(self.mojang.requests, [])

    def test_a_cached_not_in_manifest_record_is_replaced_once_the_manifest_lists_the_release(self):
        target = self.cache / "language" / "26.3.json"
        fetch_language_catalogs.stable_json(target, {"version": "26.3", "status": "not_in_manifest"})
        entry = self.mojang.publish_release("26.3")
        records, failures = self.refresh(manifest_document(entry), ["26.3"])
        self.assertEqual(failures, {})
        self.assertEqual(json.loads(target.read_text())["status"], "ok")
        self.assertEqual(records["26.3"]["language_count"], 1)

    def test_a_release_missing_from_the_manifest_fails_and_leaves_no_cached_record(self):
        records, failures = self.refresh(manifest_document(), ["26.3"])
        self.assertEqual(records, {})
        self.assertIn("not in the version manifest", failures["26.3"])
        self.assertFalse((self.cache / "language" / "26.3.json").exists())

    def test_a_record_with_an_unverified_pack_mcmeta_is_fetched_again(self):
        entry = self.mojang.publish_release("26.2")
        target = self.cache / "language" / "26.2.json"
        fetch_language_catalogs.stable_json(target, {
            "version": "26.2", "status": "ok", "languages": {"xx_xx": {}},
            "pack_mcmeta": {"sha1_matches": False},
            "version_manifest_entry": {"sha1": entry["sha1"]},
        })
        records, failures = self.refresh(manifest_document(entry), ["26.2"])
        self.assertEqual(failures, {})
        self.assertEqual(sorted(records["26.2"]["languages"]), ["en_us"])

    def test_a_manifest_entry_without_a_sha1_fails_even_when_a_record_is_cached(self):
        entry = self.mojang.publish_release("26.2")
        target = self.cache / "language" / "26.2.json"
        fetch_language_catalogs.stable_json(target, {
            "version": "26.2", "status": "ok", "languages": {"en_us": {}},
            "pack_mcmeta": {"sha1_matches": True},
            "version_manifest_entry": {"url": entry["url"]},
        })
        del entry["sha1"]
        self.mojang.requests.clear()
        records, failures = self.refresh(manifest_document(entry), ["26.2"])
        self.assertEqual(records, {})
        self.assertIn("no valid sha1", failures["26.2"])
        self.assertEqual(self.mojang.requests, [])

    def test_a_record_collected_before_its_documents_were_hash_verified_is_fetched_again(self):
        entry = self.mojang.publish_release("26.2")
        target = self.cache / "language" / "26.2.json"
        fetch_language_catalogs.stable_json(target, {
            "version": "26.2", "status": "ok", "languages": {"xx_xx": {}},
            "pack_mcmeta": {"sha1_matches": True},
            "version_manifest_entry": {"sha1": entry["sha1"]},
        })
        records, failures = self.refresh(manifest_document(entry), ["26.2"])
        self.assertEqual(failures, {})
        self.assertEqual(sorted(records["26.2"]["languages"]), ["en_us"])
        self.assertEqual(json.loads(target.read_text())["record_format"], fetch_language_catalogs.VERIFIED_RECORD_FORMAT)

    def test_a_record_from_replaced_version_metadata_is_fetched_again(self):
        entry = self.mojang.publish_release("26.2")
        document = manifest_document(entry)
        self.refresh(document, ["26.2"])
        self.mojang.requests.clear()
        republished = self.mojang.publish_release("26.2", {"en_us": {"name": "English"}, "de_de": {"name": "Deutsch"}})
        records, failures = self.refresh(manifest_document(republished), ["26.2"])
        self.assertEqual(failures, {})
        self.assertEqual(records["26.2"]["language_count"], 2)

    def test_a_release_without_a_language_catalog_is_reported_as_a_failure(self):
        entry = self.mojang.publish_release("1.12.2", mcmeta=False)
        records, failures = self.refresh(manifest_document(entry), ["1.12.2"])
        self.assertEqual(records, {})
        self.assertIn("no_pack_mcmeta", failures["1.12.2"])

    def test_a_failed_version_does_not_stop_the_remaining_versions(self):
        good = self.mojang.publish_release("26.2")
        bad = self.mojang.publish_release("26.1")
        for url in self.mojang.bodies:
            if url.endswith("26.1.json"):
                self.mojang.bodies[url] += b" "
        records, failures = self.refresh(manifest_document(bad, good), ["26.1", "26.2"])
        self.assertEqual(list(records), ["26.2"])
        self.assertIn("failed its sha1 check", failures["26.1"])

    def test_a_version_id_that_is_not_a_file_name_is_refused(self):
        records, failures = self.refresh(manifest_document(), ["../outside"])
        self.assertEqual(records, {})
        self.assertIn("not a valid Minecraft version id", failures["../outside"])
        self.assertFalse((self.cache.parent / "outside.json").exists())


class AnalyzerLanguageOutput(unittest.TestCase):
    def output(self, record: dict | None) -> dict:
        with tempfile.TemporaryDirectory() as directory:
            jar = Path(directory) / "mc.jar"
            with ZipFile(jar, "w") as archive:
                archive.writestr("assets/minecraft/lang/en_us.json", "{}")
            cache = {"26.2": record} if record is not None else {}
            return analyze_options.language_output(
                {"detected_version": "26.2", "sha256": "0" * 64}, jar, {"settings": []}, cache, ["26.2"]
            )

    def record(self, matches: bool) -> dict:
        return {
            "version": "26.2",
            "status": "ok",
            "languages": {"en_us": {"name": "English", "region": "US"}, "de_de": {"name": "Deutsch"}},
            "pack_mcmeta": {"sha1": "a" * 40, "sha1_matches": matches},
        }

    def test_a_verified_catalog_is_complete(self):
        output = self.output(self.record(True))
        self.assertTrue(output["complete"])
        self.assertEqual(output["count"], 2)
        self.assertEqual(output["per_version_catalog"]["26.2"]["status"], "ok")

    def test_a_catalog_whose_pack_mcmeta_hash_did_not_match_is_not_used(self):
        output = self.output(self.record(False))
        self.assertFalse(output["complete"])
        self.assertEqual([entry["code"] for entry in output["languages"]], ["en_us"])
        self.assertEqual(output["per_version_catalog"]["26.2"]["status"], "unverified")

    def test_an_uncached_release_is_incomplete(self):
        output = self.output(None)
        self.assertFalse(output["complete"])
        self.assertEqual(output["per_version_catalog"]["26.2"]["status"], "not_cached")


if __name__ == "__main__":
    unittest.main()
