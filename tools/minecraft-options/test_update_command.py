#!/usr/bin/env python3
"""Offline tests for update.py's command line and verified client downloads."""

from __future__ import annotations

import contextlib
import io
import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import dataset
import official_sources
import update
from official_sources import SourceError
from test_dataset import REQUIRED, release
from test_official_sources import FakeMojang, encode, manifest_document, sha1


def manifest_body(*versions: str) -> bytes:
    entries = [{"id": v, "type": "release", "url": f"https://x.example/{v}.json"} for v in versions]
    return encode(manifest_document(*entries))


class CommandLine(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.cache = self.root / "cache"
        self.bundled = self.root / "bundled" / "minecraft-options.json"
        self.mojang = FakeMojang()
        self.calls: list[str] = []
        for patcher in (
            mock.patch.object(official_sources, "fetch", self.mojang.fetch),
            mock.patch.object(update, "BUNDLED_DATASET", self.bundled),
            mock.patch.object(update, "BUNDLED_PREFERENCE_CATALOG", self.root / "absent.json"),
        ):
            patcher.start()
            self.addCleanup(patcher.stop)

    def cache_manifest(self, *versions: str) -> None:
        self.cache.mkdir(parents=True, exist_ok=True)
        (self.cache / official_sources.MANIFEST_NAME).write_bytes(manifest_body(*versions))

    def write_analysis(self, directory: Path, version: str = "26.2") -> Path:
        (directory / "normalized").mkdir(parents=True)
        (directory / "normalized" / f"mc-{version}.json").write_text(json.dumps(release(version, REQUIRED)))
        return directory

    def commit_dataset(self, version: str = "26.2") -> None:
        document, _ = dataset.merge(None, [release(version, REQUIRED)])
        self.bundled.parent.mkdir(parents=True)
        self.bundled.write_text(dataset.dumps(document))

    def run_main(self, *arguments: str) -> tuple[int, str]:
        stderr = io.StringIO()
        with (
            mock.patch("sys.argv", ["update.py", "--cache", str(self.cache), *arguments]),
            contextlib.redirect_stdout(io.StringIO()),
            contextlib.redirect_stderr(stderr),
        ):
            try:
                return update.main(), stderr.getvalue()
            except SystemExit as exit_:
                return int(exit_.code), stderr.getvalue()

    def snapshot(self) -> dict[str, bytes]:
        return {str(p): p.read_bytes() for p in sorted(self.root.rglob("*")) if p.is_file()}

    def fake_release_pipeline(self, version: str = "26.2", catalog_error: str | None = None):
        """Stands in for the JAR download, catalog refresh and analyzer, recording their order."""

        def client_jar(entry, cache):
            self.calls.append("jar")
            return cache / "jars" / f"mc-{entry['id']}.jar"

        def catalogs(cache, versions, refresh_manifest=False):
            self.calls.append("catalog")
            if catalog_error:
                raise update.WorkflowError(catalog_error)
            return {v: {"language_count": 143} for v in versions}

        def analyzer(cache, jar):
            self.calls.append("analyzer")
            return self.write_analysis(cache / "analysis" / jar.stem, version)

        for name, replacement in (
            ("client_jar", client_jar),
            ("refresh_language_catalogs", catalogs),
            ("run_analyzer", analyzer),
        ):
            patcher = mock.patch.object(update, name, replacement)
            patcher.start()
            self.addCleanup(patcher.stop)

    def test_a_selection_is_required_and_the_flags_are_mutually_exclusive(self):
        self.assertEqual(self.run_main()[0], 2)
        self.assertEqual(self.run_main("--check")[0], 2)
        self.assertEqual(self.run_main("--latest", "--version", "26.2")[0], 2)
        self.assertEqual(self.run_main("--latest", "--from-analysis", str(self.root))[0], 2)

    def test_contradictory_network_flags_are_rejected(self):
        self.assertEqual(self.run_main("--latest", "--offline")[0], 2)
        self.assertEqual(self.run_main("--check", "--latest", "--refresh-manifest")[0], 2)
        self.assertEqual(self.run_main("--from-analysis", str(self.root), "--offline", "--refresh-manifest")[0], 2)

    def test_check_with_a_release_selection_reads_the_cache_and_never_the_network(self):
        self.cache_manifest("26.1", "26.2")
        self.write_analysis(self.cache / "analysis" / "mc-26.2")
        self.commit_dataset()
        before = self.snapshot()
        for selection in (("--latest",), ("--version", "26.2")):
            with self.subTest(selection=selection):
                self.assertEqual(self.run_main("--check", *selection)[0], 0)
        self.assertEqual(self.mojang.requests, [])
        self.assertEqual(self.snapshot(), before)

    def test_check_fails_when_the_committed_dataset_differs(self):
        self.cache_manifest("26.2")
        self.write_analysis(self.cache / "analysis" / "mc-26.2")
        status, error = self.run_main("--check", "--version", "26.2")
        self.assertEqual(status, 1)
        self.assertIn("does not match", error)

    def test_check_names_the_missing_analysis_instead_of_running_the_analyzer(self):
        self.cache_manifest("26.2")
        status, error = self.run_main("--check", "--version", "26.2")
        self.assertEqual(status, 1)
        self.assertIn("no analysis of 26.2 in the cache", error)
        self.assertEqual(self.mojang.requests, [])

    def test_check_from_analysis_without_offline_never_uses_the_network_or_writes(self):
        analysis = self.write_analysis(self.root / "analysis")
        self.commit_dataset()
        before = self.snapshot()
        self.assertEqual(self.run_main("--check", "--from-analysis", str(analysis))[0], 0)
        self.assertEqual(self.mojang.requests, [])
        self.assertEqual(self.snapshot(), before)

    def test_a_version_newer_than_the_cached_manifest_refreshes_the_manifest_once(self):
        self.cache_manifest("26.1")
        self.mojang.bodies[official_sources.MANIFEST_URL] = manifest_body("26.1", "26.2")
        self.fake_release_pipeline()
        self.assertEqual(self.run_main("--version", "26.2")[0], 0)
        self.assertEqual(self.mojang.requests.count(official_sources.MANIFEST_URL), 1)

    def test_a_version_missing_even_after_the_refresh_is_reported(self):
        self.cache_manifest("26.1")
        self.mojang.bodies[official_sources.MANIFEST_URL] = manifest_body("26.1")
        status, error = self.run_main("--version", "26.9")
        self.assertEqual(status, 1)
        self.assertIn("not in Mojang's version manifest", error)
        self.assertEqual(self.mojang.requests.count(official_sources.MANIFEST_URL), 1)

    def test_latest_always_reads_the_current_manifest(self):
        self.cache_manifest("26.1")
        self.mojang.bodies[official_sources.MANIFEST_URL] = manifest_body("26.1", "26.2")
        self.fake_release_pipeline()
        self.assertEqual(self.run_main("--latest")[0], 0)
        self.assertEqual(self.calls, ["jar", "catalog", "analyzer"])
        self.assertTrue(self.bundled.exists())

    def test_the_catalog_is_fetched_before_the_analyzer_runs(self):
        self.cache_manifest("26.2")
        self.fake_release_pipeline()
        self.assertEqual(self.run_main("--version", "26.2")[0], 0)
        self.assertEqual(self.calls, ["jar", "catalog", "analyzer"])

    def test_a_missing_language_catalog_fails_the_run_before_analysis_and_writes_nothing(self):
        self.cache_manifest("26.2")
        self.fake_release_pipeline(catalog_error="the language catalog could not be produced (26.2: boom)")
        status, error = self.run_main("--version", "26.2")
        self.assertEqual(status, 1)
        self.assertIn("could not be produced", error)
        self.assertEqual(self.calls, ["jar", "catalog"])
        self.assertFalse(self.bundled.exists())

    def test_from_analysis_refreshes_the_catalog_of_every_folded_release(self):
        analysis = self.write_analysis(self.root / "analysis")
        asked: list[list[str]] = []

        def catalogs(cache, versions, refresh_manifest=False):
            asked.append(versions)
            return {v: {"language_count": 143} for v in versions}

        with mock.patch.object(update, "refresh_language_catalogs", catalogs):
            self.assertEqual(self.run_main("--from-analysis", str(analysis))[0], 0)
            self.assertEqual(self.run_main("--from-analysis", str(analysis), "--offline")[0], 0)
        self.assertEqual(asked, [["26.2"]])


class CatalogRefreshThroughTheWorkflowManifest(unittest.TestCase):
    def test_a_release_the_cached_manifest_lacks_is_refreshed_from_the_current_manifest(self):
        mojang = FakeMojang()
        entry = mojang.publish_release("26.3")
        mojang.bodies[official_sources.MANIFEST_URL] = encode(manifest_document(entry))
        with tempfile.TemporaryDirectory() as directory, contextlib.redirect_stdout(io.StringIO()):
            cache = Path(directory)
            (cache / official_sources.MANIFEST_NAME).write_bytes(manifest_body("26.2"))
            with mock.patch.object(official_sources, "fetch", mojang.fetch):
                records = update.refresh_language_catalogs(cache, ["26.3"])
            self.assertEqual(records["26.3"]["status"], "ok")
            self.assertEqual(mojang.requests.count(official_sources.MANIFEST_URL), 1)

    def test_any_release_without_a_catalog_raises_with_its_reason(self):
        mojang = FakeMojang()
        mojang.bodies[official_sources.MANIFEST_URL] = manifest_body("26.2")
        with tempfile.TemporaryDirectory() as directory, contextlib.redirect_stdout(io.StringIO()):
            with mock.patch.object(official_sources, "fetch", mojang.fetch):
                with self.assertRaisesRegex(update.WorkflowError, "26.3: not in the version manifest"):
                    update.refresh_language_catalogs(Path(directory), ["26.3"])

    def test_the_summary_reports_the_newest_refreshed_catalog_of_several(self):
        document, _ = dataset.merge(None, [release("1.20.1", REQUIRED), release("26.2", REQUIRED)])
        records = [release("1.20.1", REQUIRED), release("26.2", REQUIRED)]
        summary = update.summarize(
            document, None, [], records, {"1.20.1": {"language_count": 130}, "26.2": {"language_count": 143}}
        )
        self.assertIn("language catalog refreshed (2 releases, newest 26.2): 143 codes", summary)


class ClientJarDownload(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.cache = Path(self.directory.name)
        self.mojang = FakeMojang()
        patcher = mock.patch.object(official_sources, "fetch", self.mojang.fetch)
        patcher.start()
        self.addCleanup(patcher.stop)
        self.jar = b"jar bytes"
        self.jar_sha1 = self.mojang.publish("https://x.example/client.jar", self.jar)

    def entry(self, downloads: dict | None = None) -> dict:
        downloads = {"client": {"url": "https://x.example/client.jar", "sha1": self.jar_sha1}} if downloads is None else downloads
        metadata_sha1 = self.mojang.publish("https://x.example/26.2.json", encode({"downloads": downloads}))
        return {"id": "26.2", "type": "release", "url": "https://x.example/26.2.json", "sha1": metadata_sha1}

    def test_the_verified_jar_is_written_to_the_cache(self):
        path = update.client_jar(self.entry(), self.cache)
        self.assertEqual(path.read_bytes(), self.jar)

    def test_version_metadata_without_a_manifest_sha1_is_refused(self):
        entry = self.entry()
        del entry["sha1"]
        with self.assertRaisesRegex(SourceError, "no valid sha1"):
            update.client_jar(entry, self.cache)
        self.assertEqual(self.mojang.requests, [])

    def test_version_metadata_that_fails_its_sha1_is_refused_and_not_cached(self):
        entry = self.entry()
        self.mojang.bodies[entry["url"]] += b" "
        with self.assertRaisesRegex(SourceError, "version metadata for 26.2 failed its sha1 check"):
            update.client_jar(entry, self.cache)
        self.assertFalse((self.cache / "versions" / "26.2.json").exists())

    def test_cached_metadata_that_no_longer_matches_the_manifest_is_downloaded_again(self):
        stale = self.cache / "versions" / "26.2.json"
        stale.parent.mkdir(parents=True)
        stale.write_bytes(encode({"downloads": {"client": {"url": "https://evil.example/c.jar", "sha1": "0" * 40}}}))
        path = update.client_jar(self.entry(), self.cache)
        self.assertEqual(path.read_bytes(), self.jar)
        self.assertEqual(sha1(stale.read_bytes()), sha1(self.mojang.bodies["https://x.example/26.2.json"]))

    def test_cached_metadata_that_matches_the_manifest_is_not_downloaded_again(self):
        entry = self.entry()
        update.client_jar(entry, self.cache)
        self.mojang.requests.clear()
        update.client_jar(entry, self.cache)
        self.assertEqual(self.mojang.requests, [])

    def test_a_client_download_without_a_sha1_or_url_is_a_message_not_a_traceback(self):
        for download in (
            {"url": "https://x.example/client.jar"},
            {"sha1": self.jar_sha1},
            {"url": "http://x.example/client.jar", "sha1": self.jar_sha1},
        ):
            with self.subTest(download=download):
                with self.assertRaises(SourceError):
                    update.client_jar(self.entry({"client": download}), self.cache)
        with self.assertRaisesRegex(update.WorkflowError, "publishes no client download"):
            update.client_jar(self.entry({}), self.cache)

    def test_a_jar_that_fails_its_sha1_is_never_installed(self):
        self.mojang.bodies["https://x.example/client.jar"] = b"tampered"
        with self.assertRaisesRegex(SourceError, "client jar for 26.2 failed its sha1 check"):
            update.client_jar(self.entry(), self.cache)
        self.assertFalse((self.cache / "jars" / "mc-26.2.jar").exists())

    def test_a_manifest_id_cannot_name_a_path_outside_the_cache(self):
        entry = self.entry()
        entry["id"] = "../../escape"
        with self.assertRaises(SourceError):
            update.client_jar(entry, self.cache)
        self.assertFalse((self.cache.parent / "escape.jar").exists())


if __name__ == "__main__":
    unittest.main()
