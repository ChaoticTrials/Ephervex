import argparse
import json
import os
import sys
import tempfile
import zipfile
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import forgejo_archive as arc


def fixture():
    return {
        "schema": 1, "exported_at": "2026-10-10T12:00:00Z",
        "repository": {"private": False, "full_name": "MelanX/ephervex"},
        "source": {"forgejo": "https://forgejo.lan", "owner": "MelanX", "repo": "ephervex"},
        "issues": [{"number": 295, "title": "Encoding fix", "state": "closed", "body": "Safe markdown",
                    "user": {"login": "dev"}, "created_at": "2026-10-01T12:00:00Z", "labels": [{"name": "bug"}],
                    "milestone": {"title": "v0.11.0"}, "archive_comments": [{"id": 5, "user": {"login": "reviewer"}, "body": "LGTM"}],
                    "html_url": "https://forgejo.lan/MelanX/ephervex/issues/295", "archive_timeline": []}],
        "pulls": [{"number": 3, "title": "Web", "state": "closed", "body": "PR", "merged": True,
                   "archive_comments": [], "archive_reviews": [{"user": {"login": "reviewer"}, "body": "Approved", "state": "APPROVED"}]}],
        "labels": [{"name": "bug", "color": "ff0000"}], "milestones": [{"title": "v0.11.0"}],
        "releases": [{"tag_name": "v0.10.0", "body": "Notes"}], "downloads": [], "warnings": [],
        "api_request_count": 19,
    }


def write_fixture(path, data=None):
    data = data if data is not None else fixture()
    content = arc.json_bytes(data)
    arc.atomic_write(path / "snapshot.json", content)
    arc.atomic_write(path / "manifest.json", arc.json_bytes({
        "schema": 1, "snapshot_sha256": arc.digest(content),
        "counts": {"issues": len(data["issues"]), "pulls": len(data["pulls"])}
    }))


class OfflineTests(unittest.TestCase):
    def test_public_excludes_internal_metadata_and_paths_stable(self):
        public = arc.public_view(fixture())
        self.assertEqual(public["routeContract"]["issue"], "/road-to-v1/forgejo/issues/{number}/")
        self.assertEqual(public["issues"][0]["number"], 295)
        self.assertEqual(public["pulls"][0]["number"], 3)
        self.assertEqual(public["issues"][0]["comments"][0]["author"], "reviewer")
        self.assertEqual(public["releases"], [])
        self.assertNotIn("v0.10.0", json.dumps(public))
        self.assertNotIn("forgejo.lan", json.dumps(public))
        self.assertNotIn("html_url", json.dumps(public))

    def test_public_does_not_embed_private_attachment_metadata(self):
        f = fixture()
        f["issues"][0]["archive_attachments"] = [{"browser_download_url": "https://forgejo.lan/sensitive", "name": "private.key"}]
        pub = arc.public_view(f)
        self.assertNotIn("private.key", json.dumps(pub))
        self.assertNotIn("sensitive", json.dumps(pub))

    def test_public_omits_incomplete_label_and_assignee_names(self):
        f = fixture()
        f["issues"][0]["labels"] = [{"name": "bug"}, {"name": None}, {}]
        f["issues"][0]["assignees"] = [{"login": "dev"}, {"login": None}, {}]
        pub = arc.public_view(f)
        self.assertEqual(pub["issues"][0]["labels"], ["bug"])
        self.assertEqual(pub["issues"][0]["assignees"], ["dev"])

    def test_duplicate_numbers_rejected(self):
        with self.assertRaisesRegex(RuntimeError, "Duplicate"):
            arc.unique_ints([{"number": 5}, {"number": 5}], "issue")

    def test_verify_snapshot_and_tamper(self):
        with tempfile.TemporaryDirectory() as td:
            path = Path(td)
            write_fixture(path)
            self.assertEqual(len(arc.verify_snapshot(path)["issues"]), 1)
            with (path / "snapshot.json").open("ab") as file:
                file.write(b" ")
            with self.assertRaisesRegex(RuntimeError, "SHA256 mismatch"):
                arc.verify_snapshot(path)

    def test_verify_rejects_issue_pr_namespace_collision(self):
        with tempfile.TemporaryDirectory() as td:
            f = fixture()
            f["pulls"][0]["number"] = 295
            write_fixture(Path(td), f)
            with self.assertRaisesRegex(RuntimeError, "namespace collision"):
                arc.verify_snapshot(td)

    def test_verify_asset_checksum(self):
        with tempfile.TemporaryDirectory() as td:
            f = fixture()
            p = Path(td) / "assets" / "issues" / "295" / "attachment.dat"
            p.parent.mkdir(parents=True)
            p.write_bytes(b"data")
            f["downloads"] = [{"relative_path": "assets/issues/295/attachment.dat", "sha256": arc.digest(b"data")}]
            write_fixture(Path(td), f)
            arc.verify_snapshot(td)
            p.write_bytes(b"tampered")
            with self.assertRaisesRegex(RuntimeError, "Asset integrity failure"):
                arc.verify_snapshot(td)

    def test_verify_rejects_asset_path_traversal(self):
        with tempfile.TemporaryDirectory() as td:
            f = fixture()
            f["downloads"] = [{"relative_path": "assets/../../private", "sha256": arc.digest(b"x")}]
            write_fixture(Path(td), f)
            with self.assertRaisesRegex(RuntimeError, "Unsafe asset path"):
                arc.verify_snapshot(td)

    def test_public_requires_manual_review_artifact(self):
        with tempfile.TemporaryDirectory() as td:
            path = Path(td)
            private = path / "private"
            public = path / "public"
            private.mkdir()
            write_fixture(private)
            arc.prepare_public(argparse.Namespace(input=str(private), out=str(public), allow_incomplete=False))
            self.assertTrue((public / "REVIEW-BEFORE-PUBLISHING.md").exists())
            self.assertTrue((public / "PUBLIC-DATA-SHA256.txt").exists())
            self.assertEqual(arc.read_json(public / "archive-data.json")["issues"][0]["number"], 295)
            with self.assertRaisesRegex(RuntimeError, "must be empty"):
                arc.prepare_public(argparse.Namespace(input=str(private), out=str(public), allow_incomplete=False))

    def test_public_zip_requires_exact_reviewed_checksum(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            public = root / "public"
            public.mkdir()
            payload = arc.json_bytes(arc.public_view(fixture()))
            (public / "archive-data.json").write_bytes(payload)
            output = root / "download.zip"
            args = argparse.Namespace(input=str(public), out=str(output), approve_sha256="bad")
            with self.assertRaisesRegex(RuntimeError, "Approval SHA256"):
                arc.package_public(args)
            self.assertFalse(output.exists())
            args.approve_sha256 = arc.digest(payload)
            arc.package_public(args)
            with zipfile.ZipFile(output) as z:
                names = z.namelist()
                self.assertIn("ephervex-road-to-v1/issues/295.md", names)
                self.assertIn("ephervex-road-to-v1/pull-requests/3.md", names)
                self.assertIn("ephervex-road-to-v1/SHA256SUMS", names)
                self.assertNotIn("snapshot.json", "\n".join(names))
                self.assertEqual(z.read("ephervex-road-to-v1/archive-data.json"), payload)
            other = root / "another.zip"
            args.out = str(other)
            arc.package_public(args)
            self.assertEqual(output.read_bytes(), other.read_bytes())
            args.out = str(output)
            with self.assertRaisesRegex(RuntimeError, "already exists"):
                arc.package_public(args)

    def test_warning_blocks_public_export(self):
        with tempfile.TemporaryDirectory() as td:
            f = fixture()
            f["warnings"] = ["optional endpoint unsupported"]
            path = Path(td)
            (path / "private").mkdir()
            write_fixture(path / "private", f)
            with self.assertRaisesRegex(RuntimeError, "warnings"):
                arc.prepare_public(argparse.Namespace(input=str(path / "private"), out=str(path / "public"), allow_incomplete=False))

    def test_private_repo_blocks_public_export(self):
        with tempfile.TemporaryDirectory() as td:
            f = fixture()
            f["repository"]["private"] = True
            p = Path(td) / "private"
            p.mkdir()
            write_fixture(p, f)
            with self.assertRaisesRegex(RuntimeError, "Source repository is private"):
                arc.prepare_public(argparse.Namespace(input=str(p), out=str(Path(td) / "public"), allow_incomplete=False))

    def test_filename_cannot_traverse(self):
        self.assertNotIn("/", arc.asset_name({"id": "abcd", "name": "../../etc/passwd"}))
        self.assertNotIn("..", arc.asset_name({"id": "abcd", "name": "../../etc/passwd"}))

    def test_archive_output_stays_outside_worktree_when_run_from_web(self):
        with tempfile.TemporaryDirectory() as td:
            repo = Path(td) / "repo"
            web = repo / "web"
            web.mkdir(parents=True)
            (repo / ".git").write_text("gitdir: somewhere\n")
            previous = Path.cwd()
            try:
                os.chdir(web)
                with self.assertRaisesRegex(RuntimeError, "OUTSIDE the Git worktree"):
                    arc.private_destination(web / "archive-candidate")
                self.assertEqual(arc.private_destination(Path(td) / "safe"), Path(td) / "safe")
            finally:
                os.chdir(previous)

    def test_cross_origin_redirect_rejected(self):
        h = arc.SameOriginRedirect()
        request = __import__("urllib.request", fromlist=["Request"]).Request("https://forgejo.lan/api/v1/test")
        with self.assertRaisesRegex(RuntimeError, "Cross-origin"):
            h.redirect_request(request, None, 302, "Found", {}, "https://attacker.example/harvest")

    def test_api_pagination_fetches_until_empty_even_if_server_caps_size(self):
        a = arc.ForgejoAPI("https://forgejo.lan", "MelanX", "ephervex", "fake", interval=0)
        calls = []
        def fake_fetch(path, params=None, **kwargs):
            calls.append(params["page"])
            return (arc.json_bytes([{"id": params["page"]}]) if params["page"] < 4 else b"[]"), {}
        a.fetch = fake_fetch
        self.assertEqual(len(a.items("/labels")), 3)
        self.assertEqual(calls, [1, 2, 3, 4])

    def test_api_null_is_an_empty_page_after_timeline_data(self):
        a = arc.ForgejoAPI("https://forgejo.lan", "MelanX", "ephervex", "fake", interval=0)
        calls = []
        def fake_fetch(path, params=None, **kwargs):
            calls.append(params["page"])
            return (b'[{"id": 1}]' if params["page"] == 1 else b"null"), {}
        a.fetch = fake_fetch
        self.assertEqual(a.items("/issues/445/timeline", optional=True), [{"id": 1}])
        self.assertEqual(calls, [1, 2])

    def test_api_null_first_page_is_empty_collection(self):
        a = arc.ForgejoAPI("https://forgejo.lan", "MelanX", "ephervex", "fake", interval=0)
        a.fetch = lambda *args, **kwargs: (b"null", {})
        self.assertEqual(a.items("/issues/445/assets", optional=True), [])

    def test_api_total_count_stops_endpoint_that_ignores_page(self):
        a = arc.ForgejoAPI("https://forgejo.lan", "MelanX", "ephervex", "fake", interval=0)
        calls = []
        def fake_fetch(path, params=None, **kwargs):
            calls.append(params["page"])
            return b'[{"id": 1}]', {"X-Total-Count": "1"}
        a.fetch = fake_fetch
        self.assertEqual(a.items("/issues/392/comments"), [{"id": 1}])
        self.assertEqual(calls, [1])

    def test_api_total_count_rejects_early_empty_page(self):
        a = arc.ForgejoAPI("https://forgejo.lan", "MelanX", "ephervex", "fake", interval=0)
        def fake_fetch(path, params=None, **kwargs):
            return (b'[{"id": 1}]' if params["page"] == 1 else b"[]"), {"X-Total-Count": "2"}
        a.fetch = fake_fetch
        with self.assertRaisesRegex(RuntimeError, "ended early"):
            a.items("/issues/392/comments")

    def test_api_repeated_page_fails_closed(self):
        a = arc.ForgejoAPI("https://forgejo.lan", "MelanX", "ephervex", "fake", interval=0)
        a.fetch = lambda *args, **kwargs: (b'[{"id": 7}]', {})
        with self.assertRaisesRegex(RuntimeError, "Repeated page"):
            a.items("/labels")


if __name__ == "__main__":
    unittest.main()
