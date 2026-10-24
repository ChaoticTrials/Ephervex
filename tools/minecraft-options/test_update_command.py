#!/usr/bin/env python3
"""Offline tests for update.py's command line and verified client downloads."""

from __future__ import annotations

import contextlib
import copy
import io
import json
import tempfile
import unittest
import urllib.error
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

import dataset
import file_io
import official_sources
import preferences
import update
from official_sources import SourceError
from test_dataset import REQUIRED, release, setting
from test_official_sources import FakeMojang, encode, manifest_document, sha1


def manifest_body(*versions: str) -> bytes:
    entries = [{"id": v, "type": "release", "url": f"https://x.example/{v}.json"} for v in versions]
    return encode(manifest_document(*entries))


def manifest_entry(version: str, kind: str = "release", **extra: str) -> dict:
    return {"id": version, "type": kind, "url": f"https://x.example/{version}.json", **extra}


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

    def write_analysis(self, directory: Path, version: str = "26.2", settings: list[dict] = REQUIRED) -> Path:
        (directory / "normalized").mkdir(parents=True)
        (directory / "normalized" / f"mc-{version}.json").write_text(json.dumps(release(version, settings)))
        return directory

    def commit_dataset(self, version: str = "26.2") -> None:
        document, _ = dataset.merge(None, [release(version, REQUIRED)])
        self.bundled.parent.mkdir(parents=True)
        self.bundled.write_text(dataset.dumps(document))

    def run_main(self, *arguments: str) -> tuple[int, str]:
        stdout, stderr = io.StringIO(), io.StringIO()
        with (
            mock.patch("sys.argv", ["update.py", "--cache", str(self.cache), *arguments]),
            contextlib.redirect_stdout(stdout),
            contextlib.redirect_stderr(stderr),
        ):
            self.stdout = stdout
            try:
                return update.main(), stderr.getvalue()
            except SystemExit as exit_:
                return int(exit_.code), stderr.getvalue()

    def printed(self) -> list[str]:
        return self.stdout.getvalue().splitlines()

    def snapshot(self) -> dict[str, bytes]:
        return {str(p): p.read_bytes() for p in sorted(self.root.rglob("*")) if p.is_file()}

    def fake_release_pipeline(
        self,
        version: str = "26.2",
        catalog_error: str | None = None,
        verify_download: bool = False,
        analyzer_exit: int | None = None,
    ):
        """Stands in for the JAR download, catalog refresh and analyzer, recording their order.

        `verify_download` keeps the real hash-verified download, and `analyzer_exit` keeps the
        real analyzer step with its subprocess ending in that status.
        """

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

        replacements = {"refresh_language_catalogs": catalogs}
        if not verify_download:
            replacements["client_jar"] = client_jar
        if analyzer_exit is None:
            replacements["run_analyzer"] = analyzer
        else:
            self.analyzer_process = mock.Mock(return_value=SimpleNamespace(returncode=analyzer_exit))
            patcher = mock.patch.object(update.subprocess, "run", self.analyzer_process)
            patcher.start()
            self.addCleanup(patcher.stop)
        for name, replacement in replacements.items():
            patcher = mock.patch.object(update, name, replacement)
            patcher.start()
            self.addCleanup(patcher.stop)

    def publish_client(self, version: str = "26.2") -> str:
        """Publishes a release's metadata and client JAR, and caches a manifest listing it."""
        jar_url = f"https://x.example/{version}.jar"
        jar_sha1 = self.mojang.publish(jar_url, b"jar bytes")
        metadata = encode({"downloads": {"client": {"url": jar_url, "sha1": jar_sha1}}})
        entry = manifest_entry(version, sha1=self.mojang.publish(f"https://x.example/{version}.json", metadata))
        self.cache.mkdir(parents=True, exist_ok=True)
        (self.cache / official_sources.MANIFEST_NAME).write_bytes(encode(manifest_document(entry)))
        return jar_url

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

    def test_a_damaged_bundled_dataset_is_reported_and_left_alone(self):
        analysis = self.write_analysis(self.root / "analysis")
        self.bundled.parent.mkdir(parents=True)
        for content in ('{"format_version": 1, "revi', "[]", "{}"):
            self.bundled.write_text(content)
            for arguments in (("--from-analysis", str(analysis), "--offline"), ("--check", "--from-analysis", str(analysis))):
                with self.subTest(content=content, arguments=arguments):
                    status, error = self.run_main(*arguments)
                    self.assertEqual(status, 1)
                    self.assertIn("the bundled dataset", error)
                    self.assertIn("restore it from version control", error)
            self.assertEqual(self.bundled.read_text(), content)

    def test_a_committed_dataset_that_no_longer_passes_publication_checks_is_still_folded(self):
        analysis = self.write_analysis(self.root / "analysis")
        self.commit_dataset()
        stored = json.loads(self.bundled.read_text())
        stored["current_canonical_setting_ids"] = [
            setting_id for setting_id in stored["current_canonical_setting_ids"] if setting_id != "fov"
        ]
        with self.assertRaises(dataset.DatasetError):
            dataset.validate(stored)
        self.bundled.write_text(json.dumps(stored))
        self.assertEqual(self.run_main("--from-analysis", str(analysis), "--offline")[0], 0)
        dataset.validate(json.loads(self.bundled.read_text()))

    def test_parseable_inputs_with_the_wrong_structure_are_errors_and_write_nothing(self):
        analysis = self.write_analysis(self.root / "analysis")
        record = release("26.2", REQUIRED)
        record_path = analysis / "normalized" / "mc-26.2.json"
        self.commit_dataset()
        committed = self.bundled.read_bytes()
        for damaged in ({**record, "settings": 5}, {**record, "settings": [{"x": 1}]}):
            record_path.write_text(json.dumps(damaged))
            with self.subTest(record=damaged["settings"]):
                status, error = self.run_main("--from-analysis", str(analysis), "--offline")
                self.assertEqual(status, 1)
                self.assertIn("malformed", error)
        record_path.write_text(json.dumps(record))
        stored = json.loads(committed)
        for damaged in (
            {"format_version": 1, "releases": 3},
            {**stored, "canonical_minecraft_version": "x"},
            {**stored, "canonical_minecraft_version": []},
            {**stored, "releases": [{**stored["releases"][0], "minecraft_version": "bogus"}]},
        ):
            self.bundled.write_text(json.dumps(damaged))
            with self.subTest(bundled=damaged.get("canonical_minecraft_version")):
                status, error = self.run_main("--from-analysis", str(analysis), "--offline")
                self.assertEqual(status, 1)
                self.assertIn("malformed", error)
                self.assertFalse((self.cache / "feed").exists())
        self.bundled.write_bytes(committed)
        for content in ('{"descriptors": 1}', '{"descriptors": [], "exclusions": [3]}'):
            catalog = self.root / "preferences.json"
            catalog.write_text(content)
            with self.subTest(catalog=content), mock.patch.object(update, "BUNDLED_PREFERENCE_CATALOG", catalog):
                status, error = self.run_main("--from-analysis", str(analysis), "--offline")
                self.assertEqual(status, 1)
                self.assertIn("the bundled preference catalog", error)
                self.assertEqual(self.bundled.read_bytes(), committed)
                self.assertFalse((self.cache / "feed").exists())

    def test_a_damaged_bundled_preference_catalog_is_reported_before_anything_is_written(self):
        analysis = self.write_analysis(self.root / "analysis")
        catalog = self.root / "preferences.json"
        catalog.write_text('{"descriptors": [')
        with mock.patch.object(update, "BUNDLED_PREFERENCE_CATALOG", catalog):
            status, error = self.run_main("--from-analysis", str(analysis), "--offline")
        self.assertEqual(status, 1)
        self.assertIn("the bundled preference catalog", error)
        self.assertFalse(self.bundled.exists())
        self.assertFalse((self.cache / "feed").exists())

    def test_a_damaged_analyzer_record_asks_for_the_analyzer_to_run_again(self):
        analysis = self.write_analysis(self.root / "analysis")
        (analysis / "normalized" / "mc-26.2.json").write_text('{"minecraft_version": "26.2", "setti')
        status, error = self.run_main("--from-analysis", str(analysis), "--offline")
        self.assertEqual(status, 1)
        self.assertIn("mc-26.2.json", error)
        self.assertIn("run the analyzer again", error)
        self.assertFalse(self.bundled.exists())

    def test_an_analyzer_record_that_is_not_an_object_is_reported(self):
        analysis = self.write_analysis(self.root / "analysis")
        (analysis / "normalized" / "mc-26.2.json").write_text("[]")
        status, error = self.run_main("--from-analysis", str(analysis), "--offline")
        self.assertEqual(status, 1)
        self.assertIn("not a JSON object", error)

    def test_an_interrupted_write_keeps_the_committed_dataset_and_the_feed_complete(self):
        analysis = self.write_analysis(self.root / "analysis")
        self.commit_dataset()
        before = self.snapshot()
        with mock.patch.object(file_io.os, "replace", side_effect=OSError("killed")):
            status, error = self.run_main("--from-analysis", str(analysis), "--offline")
        self.assertEqual(status, 1)
        self.assertIn("could not write the generated dataset", error)
        self.assertEqual(self.bundled.read_bytes(), before[str(self.bundled)])
        self.assertEqual(sorted(p.name for p in self.bundled.parent.iterdir()), [self.bundled.name])

    def test_the_bundled_dataset_and_the_feed_are_written_by_replacement(self):
        analysis = self.write_analysis(self.root / "analysis")
        replaced: list[str] = []
        real_replace = file_io.os.replace

        def record(source, destination):
            replaced.append(Path(destination).name)
            real_replace(source, destination)

        with mock.patch.object(file_io.os, "replace", record):
            self.assertEqual(self.run_main("--from-analysis", str(analysis), "--offline")[0], 0)
        self.assertEqual(replaced, ["minecraft-options.json", "minecraft-options.json"])
        self.assertEqual(self.bundled.read_text(), (self.cache / "feed" / "minecraft-options.json").read_text())

    def test_a_snapshot_is_refused_before_anything_is_downloaded_or_written(self):
        self.cache.mkdir()
        (self.cache / official_sources.MANIFEST_NAME).write_bytes(
            encode(manifest_document(manifest_entry("26.1"), manifest_entry("24w14a", "snapshot")))
        )
        before = self.snapshot()
        for arguments in (("--version", "24w14a"), ("--check", "--version", "24w14a")):
            with self.subTest(arguments=arguments):
                status, error = self.run_main(*arguments)
                self.assertEqual(status, 1)
                self.assertIn("`24w14a` is not a release", error)
        self.assertEqual(self.mojang.requests, [])
        self.assertEqual(self.snapshot(), before)

    def test_a_release_below_the_supported_floor_is_refused_before_anything_is_downloaded(self):
        self.cache_manifest("1.12.1", "26.1")
        before = self.snapshot()
        for arguments in (("--version", "1.12.1"), ("--check", "--version", "1.12.1")):
            with self.subTest(arguments=arguments):
                status, error = self.run_main(*arguments)
                self.assertEqual(status, 1)
                self.assertIn("predates the supported floor 1.12.2", error)
        self.assertEqual(self.mojang.requests, [])
        self.assertEqual(self.snapshot(), before)

    def test_check_never_fetches_a_manifest_that_lacks_the_requested_version(self):
        self.cache_manifest("26.1")
        self.mojang.bodies[official_sources.MANIFEST_URL] = manifest_body("26.1", "26.9")
        status, error = self.run_main("--check", "--version", "26.9")
        self.assertEqual(status, 1)
        self.assertIn("not in Mojang's version manifest", error)
        self.assertEqual(self.mojang.requests, [])

    def test_check_without_a_cached_manifest_says_so_and_stays_offline(self):
        for selection in (("--latest",), ("--version", "26.2")):
            with self.subTest(selection=selection):
                status, error = self.run_main("--check", *selection)
                self.assertEqual(status, 1)
                self.assertIn("no usable cached version manifest", error)
        self.assertEqual(self.mojang.requests, [])
        self.assertEqual(self.snapshot(), {})

    def test_analyzing_a_release_offline_is_a_usage_error_that_names_the_alternative(self):
        for arguments in (("--version", "26.2", "--offline"), ("--latest", "--offline")):
            with self.subTest(arguments=arguments):
                status, error = self.run_main(*arguments)
                self.assertEqual(status, 2)
                self.assertIn("analyzing a release needs the network; use --from-analysis", error)
        status, error = self.run_main("--check", "--version", "26.2", "--refresh-manifest")
        self.assertEqual(status, 2)
        self.assertIn("--refresh-manifest needs the network", error)
        self.assertEqual(self.mojang.requests, [])

    def test_an_analysis_directory_without_analyzer_output_is_named_in_the_error(self):
        (self.root / "empty").mkdir()
        for directory in (self.root / "empty", self.root / "missing"):
            for arguments in (("--offline",), ("--check",)):
                with self.subTest(directory=directory.name, arguments=arguments):
                    status, error = self.run_main(*arguments, "--from-analysis", str(directory))
                    self.assertEqual(status, 1)
                    self.assertIn(f"no normalized analyzer output under {directory.resolve() / 'normalized'}", error)
        self.assertFalse(self.bundled.exists())
        self.assertFalse((self.cache / "feed").exists())

    def test_a_client_jar_that_fails_its_hash_stops_the_run_before_analysis_and_writes_nothing(self):
        jar_url = self.publish_client()
        self.mojang.bodies[jar_url] = b"tampered"
        self.fake_release_pipeline(verify_download=True)
        status, error = self.run_main("--version", "26.2")
        self.assertEqual(status, 1)
        self.assertIn("client jar for 26.2 failed its sha1 check", error)
        self.assertEqual(self.calls, [])
        self.assertFalse((self.cache / "jars" / "mc-26.2.jar").exists())
        self.assertFalse(self.bundled.exists())
        self.assertFalse((self.cache / "feed").exists())

    def test_a_verified_client_jar_is_downloaded_once_into_the_cache_before_the_catalog_and_analysis(self):
        jar_url = self.publish_client()
        self.fake_release_pipeline(verify_download=True)
        self.assertEqual(self.run_main("--version", "26.2")[0], 0)
        self.assertEqual(self.mojang.requests.count(jar_url), 1)
        self.assertEqual((self.cache.resolve() / "jars" / "mc-26.2.jar").read_bytes(), b"jar bytes")
        self.assertEqual(self.calls, ["catalog", "analyzer"])
        self.assertIn("[update] selected release 26.2", self.printed())

    def test_an_unreachable_official_source_is_a_message_and_writes_nothing(self):
        with mock.patch.object(official_sources, "fetch", side_effect=urllib.error.URLError("no route")):
            status, error = self.run_main("--latest")
        self.assertEqual(status, 1)
        self.assertIn("could not reach an official source", error)
        self.assertIn("no route", error)
        self.assertEqual(self.snapshot(), {})

    def test_a_failing_analyzer_stops_the_run_and_writes_nothing(self):
        self.cache_manifest("26.2")
        self.fake_release_pipeline(analyzer_exit=3)
        status, error = self.run_main("--version", "26.2")
        self.assertEqual(status, 1)
        self.assertIn("the static analyzer failed", error)
        self.assertEqual(self.calls, ["jar", "catalog"])
        cache = self.cache.resolve()
        self.analyzer_process.assert_called_once_with(
            update.analyzer_command(cache / "jars" / "mc-26.2.jar", cache / "analysis" / "mc-26.2", cache / "assets-cache"),
            check=False,
        )
        self.assertFalse(self.bundled.exists())
        self.assertFalse((self.cache / "feed").exists())

    def test_a_required_setting_the_canonical_release_cannot_encode_is_an_error_and_writes_nothing(self):
        unresolved = copy.deepcopy(REQUIRED)
        unresolved[5] = setting("fov", kind="number", transform="pow(value, 2)")
        analysis = self.write_analysis(self.root / "analysis", settings=unresolved)
        arguments = (("--from-analysis", str(analysis), "--offline"), ("--check", "--from-analysis", str(analysis)))
        for committed in (False, True):
            if committed:
                self.commit_dataset("1.20.1")
            before = self.snapshot()
            for argument_set in arguments:
                with self.subTest(committed=committed, arguments=argument_set):
                    status, error = self.run_main(*argument_set)
                    self.assertEqual(status, 1)
                    self.assertIn("cannot encode required settings: fov", error)
                    self.assertNotIn("Ephervex application review/update required", self.stdout.getvalue())
                    self.assertEqual(self.snapshot(), before)
        self.assertFalse((self.cache / "feed").exists())

    def test_a_product_catalog_candidate_is_reported_and_never_written_to_the_catalog(self):
        self.commit_dataset("1.20.1")
        catalog = self.root / "preferences" / "minecraft-preferences.json"
        catalog.parent.mkdir()
        catalog.write_text(preferences.dumps(preferences.build(json.loads(self.bundled.read_text()))))
        before = catalog.read_bytes()
        analysis = self.write_analysis(self.root / "analysis", "1.20.2", REQUIRED + [setting("newCurrentOption")])
        with mock.patch.object(update, "BUNDLED_PREFERENCE_CATALOG", catalog):
            status, _ = self.run_main("--from-analysis", str(analysis), "--offline")
        self.assertEqual(status, 0)
        self.assertTrue(
            any("newCurrentOption" in line and "need application review" in line for line in self.printed())
        )
        self.assertIn("newCurrentOption", json.loads(self.bundled.read_text())["current_canonical_setting_ids"])
        self.assertEqual(catalog.read_bytes(), before)
        self.assertEqual([path.name for path in catalog.parent.iterdir()], [catalog.name])

    def test_the_run_reports_what_it_wrote_and_then_that_nothing_changed(self):
        analysis = self.write_analysis(self.root / "analysis")
        feed = self.cache.resolve() / "feed" / "minecraft-options.json"
        self.assertEqual(self.run_main("--from-analysis", str(analysis), "--offline")[0], 0)
        created = self.printed()
        self.assertIn("new release 26.2", created)
        self.assertIn("bundled dataset created", created)
        self.assertEqual(created[-2:], [f"bundled dataset: {self.bundled}", f"remote feed to publish: {feed}"])
        written = self.bundled.read_bytes()
        self.assertEqual(feed.read_bytes(), written)

        self.assertEqual(self.run_main("--from-analysis", str(analysis), "--offline")[0], 0)
        again = self.printed()
        self.assertIn("release 26.2: options representation unchanged", again)
        self.assertIn("bundled dataset unchanged", again)
        self.assertNotIn("bundled dataset created", again)
        self.assertEqual(self.bundled.read_bytes(), written)

    def test_an_incremental_run_keeps_earlier_releases_unknown_for_later_settings_and_checks_clean(self):
        first = self.write_analysis(self.root / "first", "1.20.1")
        second = self.write_analysis(self.root / "second", "26.3", REQUIRED + [setting("addedLater")])
        self.assertEqual(self.run_main("--from-analysis", str(first), "--offline")[0], 0)
        earlier = json.loads(self.bundled.read_text())
        self.assertEqual(self.run_main("--from-analysis", str(second), "--offline")[0], 0)
        later = json.loads(self.bundled.read_text())

        def schema_id(document: dict, version: str) -> str:
            return next(r["schema"] for r in document["releases"] if r["minecraft_version"] == version)

        self.assertEqual(schema_id(later, "1.20.1"), schema_id(earlier, "1.20.1"))
        older = next(schema for schema in later["schemas"] if schema["id"] == schema_id(later, "1.20.1"))
        self.assertNotIn("addedLater", older["supported"])
        self.assertNotIn("addedLater", older["unsupported"])
        self.assertEqual(later["revision"], earlier["revision"] + 1)
        self.assertEqual(self.run_main("--check", "--from-analysis", str(second))[0], 0)

        advanced = self.bundled.read_bytes()
        self.assertEqual(self.run_main("--from-analysis", str(first), "--offline")[0], 0)
        self.assertIn("bundled dataset unchanged", self.printed())
        self.assertEqual(self.bundled.read_bytes(), advanced)


class SelectingARelease(unittest.TestCase):
    DOCUMENT = {
        "latest": {"release": "26.1", "snapshot": "26.2-rc-1"},
        "versions": [
            manifest_entry("26.2-rc-1", "snapshot"),
            manifest_entry("26.1"),
            manifest_entry("1.12.2"),
            manifest_entry("1.12.1"),
            manifest_entry("1.7.10"),
            manifest_entry("b1.7.3", "old_beta"),
            manifest_entry("a1.2.6", "old_alpha"),
        ],
    }

    def select(self, wanted: str | None) -> dict:
        return update.select_release(self.DOCUMENT, wanted)

    def test_without_a_request_the_manifests_own_latest_release_is_selected(self):
        self.assertEqual(self.select(None)["id"], "26.1")

    def test_a_requested_release_is_selected_by_id(self):
        self.assertEqual(self.select("1.12.2")["url"], "https://x.example/1.12.2.json")

    def test_only_releases_qualify(self):
        for version in ("26.2-rc-1", "b1.7.3", "a1.2.6"):
            with self.subTest(version=version):
                with self.assertRaisesRegex(update.WorkflowError, f"`{version}` is not a release;"):
                    self.select(version)

    def test_the_latest_release_is_refused_when_it_is_not_a_release_entry(self):
        document = {**self.DOCUMENT, "latest": {"release": "26.2-rc-1", "snapshot": ""}}
        with self.assertRaisesRegex(update.WorkflowError, "is not a release;"):
            update.select_release(document, None)

    def test_the_supported_floor_is_included_and_anything_older_is_refused(self):
        self.assertEqual(self.select(update.MINIMUM_SUPPORTED_VERSION)["id"], "1.12.2")
        for version in ("1.12.1", "1.7.10"):
            with self.subTest(version=version):
                with self.assertRaisesRegex(update.WorkflowError, "predates the supported floor 1.12.2"):
                    self.select(version)

    def test_a_version_absent_from_the_manifest_is_reported_as_such(self):
        for version in ("26.9", "1.20.1"):
            with self.subTest(version=version):
                with self.assertRaisesRegex(update.WorkflowError, f"`{version}` is not in Mojang's version manifest"):
                    self.select(version)
        document = {**self.DOCUMENT, "latest": {"release": "26.9", "snapshot": ""}}
        with self.assertRaisesRegex(update.WorkflowError, "`26.9` is not in Mojang's version manifest"):
            update.select_release(document, None)

    def test_a_release_entry_whose_id_is_not_a_release_identifier_is_a_message_not_a_traceback(self):
        for version in ("abc", "26.2-rc-1", "../../escape"):
            with self.subTest(version=version):
                document = manifest_document(manifest_entry(version))
                with self.assertRaisesRegex(update.WorkflowError, "is not a release identifier"):
                    update.select_release(document, version)


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

    def test_truncated_cached_metadata_and_jar_are_downloaded_again(self):
        entry = self.entry()
        path = update.client_jar(entry, self.cache)
        metadata = self.cache / "versions" / "26.2.json"
        metadata.write_bytes(metadata.read_bytes()[:10])
        path.write_bytes(self.jar[:3])
        self.assertEqual(update.client_jar(entry, self.cache).read_bytes(), self.jar)
        self.assertEqual(sha1(metadata.read_bytes()), entry["sha1"])

    def test_an_interrupted_download_leaves_neither_a_partial_jar_nor_metadata(self):
        entry = self.entry()
        with mock.patch.object(file_io.os, "replace", side_effect=OSError("killed")):
            with self.assertRaises(OSError):
                update.client_jar(entry, self.cache)
        self.assertEqual([p for p in self.cache.rglob("*") if p.is_file()], [])

    def test_a_manifest_id_cannot_name_a_path_outside_the_cache(self):
        entry = self.entry()
        entry["id"] = "../../escape"
        with self.assertRaises(SourceError):
            update.client_jar(entry, self.cache)
        self.assertFalse((self.cache.parent / "escape.jar").exists())


if __name__ == "__main__":
    unittest.main()
