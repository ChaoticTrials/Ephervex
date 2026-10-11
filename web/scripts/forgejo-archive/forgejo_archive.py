#!/usr/bin/env python3
"""Read-only Forgejo API snapshot + carefully staged Docusaurus archive data.

Requires Python 3.10+, standard library only. No GitHub or Forgejo writes.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
import zipfile
from datetime import datetime, timezone
from pathlib import Path

DEFAULT_FORGEJO = "https://forgejo.lan"
DEFAULT_REPO = "MelanX/ephervex"
SCHEMA = 1


def stamp():
    return datetime.now(timezone.utc).replace(microsecond=0).isoformat()


def json_bytes(value):
    return (json.dumps(value, indent=2, sort_keys=True, ensure_ascii=False) + "\n").encode("utf-8")


def atomic_write(path: Path, data: bytes):
    path.parent.mkdir(parents=True, exist_ok=True)
    temp = path.with_name(path.name + ".tmp")
    with temp.open("wb") as f:
        f.write(data)
        f.flush()
        os.fsync(f.fileno())
    os.replace(temp, path)


def read_json(path):
    return json.loads(Path(path).read_text(encoding="utf-8"))


def digest(data):
    return hashlib.sha256(data).hexdigest()


def unique_ints(rows, entity):
    ids = [int(row.get("number") or row.get("index")) for row in rows]
    if len(ids) != len(set(ids)):
        raise RuntimeError(f"Duplicate {entity} numbers detected")
    return ids


def is_inside(path, parent):
    try:
        path.resolve().relative_to(parent.resolve())
        return True
    except ValueError:
        return False


def private_destination(path):
    path = Path(path).expanduser().resolve()
    cwd = Path.cwd().resolve()
    if is_inside(path, cwd / "web") or is_inside(path, cwd / "docs"):
        raise RuntimeError("Refusing to write raw private data under web/ or docs/")
    # The script may be run from the repository root, web/, or elsewhere.
    # Resolve both working directory and script location before allowing output.
    for start in (cwd, Path(__file__).resolve().parent):
        for directory in (start, *start.parents):
            if (directory / ".git").exists():
                if is_inside(path, directory):
                    raise RuntimeError("Archive output must be OUTSIDE the Git worktree")
                break
    return path


class APIError(RuntimeError):
    def __init__(self, code, location):
        super().__init__(f"Forgejo API returned HTTP {code} at {location}")
        self.code = code


class SameOriginRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        old = urllib.parse.urlsplit(req.full_url)
        new = urllib.parse.urlsplit(newurl)
        if (old.scheme, old.netloc) != (new.scheme, new.netloc):
            raise RuntimeError("Cross-origin redirect blocked to prevent leaking Forgejo authorization")
        return super().redirect_request(req, fp, code, msg, headers, newurl)


class ForgejoAPI:
    def __init__(self, base, owner, repo, token, interval=0.05):
        root = urllib.parse.urlsplit(base.rstrip("/"))
        if root.scheme not in ("http", "https") or not root.hostname:
            raise ValueError("Invalid Forgejo URL")
        self.root = f"{root.scheme}://{root.netloc}"
        self.api = self.root + "/api/v1/repos/" + urllib.parse.quote(owner, safe="") + "/" + urllib.parse.quote(repo, safe="")
        self.token = token
        self.interval = interval
        self.opener = urllib.request.build_opener(SameOriginRedirect())
        self.warnings = []
        self.request_count = 0

    def fetch(self, path, params=None, *, absolute=False):
        url = path if absolute else self.api + path
        if params:
            url += "?" + urllib.parse.urlencode(params)
        split = urllib.parse.urlsplit(url)
        origin = urllib.parse.urlsplit(self.root)
        if (split.scheme, split.netloc) != (origin.scheme, origin.netloc):
            raise RuntimeError("Refusing to send API token to another origin")
        headers = {"Accept": "application/json", "Authorization": "token " + self.token, "User-Agent": "EphervexRoadArchive/1"}
        request = urllib.request.Request(url, headers=headers)
        for attempt in range(6):
            try:
                if self.interval:
                    time.sleep(self.interval)
                self.request_count += 1
                with self.opener.open(request, timeout=45) as response:
                    return response.read(), dict(response.headers)
            except urllib.error.HTTPError as exc:
                if exc.code in (429, 500, 502, 503, 504) and attempt < 5:
                    retry = exc.headers.get("Retry-After", "")
                    delay = min(45.0, max(1.0, float(retry))) if retry.isdigit() else min(30.0, 2 ** attempt)
                    time.sleep(delay)
                    continue
                raise APIError(exc.code, split.path) from exc
            except urllib.error.URLError as exc:
                if attempt >= 5:
                    raise RuntimeError(f"Network error at {split.path}: {exc.reason}") from exc
                time.sleep(min(30.0, 2 ** attempt))
        raise AssertionError("unreachable")

    def obj(self, path, optional=False):
        try:
            raw, _ = self.fetch(path)
            return json.loads(raw)
        except APIError as exc:
            if optional and exc.code in (403, 404, 405, 501):
                self.warnings.append(str(exc))
                return None
            raise

    def items(self, path, params=None, *, optional=False):
        all_rows = []
        signatures = set()
        expected_total = None
        for page in range(1, 10001):
            p = dict(params or {})
            p.update(page=page, limit=50)
            try:
                raw, headers = self.fetch(path, p)
            except APIError as exc:
                if optional and exc.code in (403, 404, 405, 501):
                    self.warnings.append(str(exc))
                    return None
                raise
            data = json.loads(raw)
            # Some Forgejo list endpoints ignore the page parameter and repeat
            # their only page. The reported total lets us stop without either
            # duplicating records or silently accepting a short export.
            total_header = next((value for key, value in headers.items()
                                 if key.lower() == "x-total-count"), None)
            if total_header is not None:
                if not str(total_header).isdigit():
                    raise RuntimeError(f"Invalid X-Total-Count from {path}: {total_header!r}")
                reported_total = int(total_header)
                if expected_total is not None and reported_total != expected_total:
                    raise RuntimeError(f"X-Total-Count changed during pagination at {path}")
                expected_total = reported_total
            # Forgejo can serialize an empty collection as JSON null, including
            # the page immediately after the last populated timeline page.
            # Treat it exactly like [] while retaining the repeated-page check
            # for servers that ignore pagination parameters.
            if data is None:
                if expected_total is not None and len(all_rows) != expected_total:
                    raise RuntimeError(f"Pagination ended early at {path}: got {len(all_rows)} of {expected_total}")
                return all_rows
            if not isinstance(data, list):
                raise RuntimeError(f"Expected list from {path} page {page}, got {type(data).__name__}")
            if not data:
                if expected_total is not None and len(all_rows) != expected_total:
                    raise RuntimeError(f"Pagination ended early at {path}: got {len(all_rows)} of {expected_total}")
                return all_rows
            signature = digest(json_bytes(data))
            if signature in signatures:
                raise RuntimeError(f"Repeated page content from {path}; refusing incomplete export")
            signatures.add(signature)
            all_rows.extend(data)
            if expected_total is not None:
                if len(all_rows) > expected_total:
                    raise RuntimeError(f"Pagination exceeded X-Total-Count at {path}")
                if len(all_rows) == expected_total:
                    return all_rows
        raise RuntimeError(f"Pagination exceeded 10000 pages at {path}")

    def download(self, absolute_url, target):
        # Attachments can live on a different object-store host. Do not forward the token there.
        source = urllib.parse.urlsplit(absolute_url)
        root = urllib.parse.urlsplit(self.root)
        if (source.scheme, source.netloc) != (root.scheme, root.netloc):
            self.warnings.append("Skipped off-origin attachment URL; requires separate audited download mechanism")
            return None
        target = Path(target)
        target.parent.mkdir(parents=True, exist_ok=True)
        tmp = target.with_name(target.name + ".tmp")
        try:
            # Stream data; release assets may be large.
            req = urllib.request.Request(absolute_url, headers={"Authorization": "token " + self.token, "User-Agent": "EphervexRoadArchive/1"})
            hash_ = hashlib.sha256()
            n = 0
            with self.opener.open(req, timeout=90) as res, tmp.open("wb") as f:
                while chunk := res.read(1024 * 1024):
                    f.write(chunk)
                    hash_.update(chunk)
                    n += len(chunk)
            os.replace(tmp, target)
            return {"sha256": hash_.hexdigest(), "size": n}
        except Exception:
            tmp.unlink(missing_ok=True)
            raise


def asset_name(attachment):
    raw_id = str(attachment.get("id", "no-id"))
    id_ = re.sub(r"[^A-Za-z0-9_-]", "_", raw_id)[:48]
    raw_name = str(attachment.get("name") or attachment.get("filename") or "attachment")
    name = re.sub(r"[^A-Za-z0-9._-]", "_", raw_name)[:90].strip(".") or "attachment"
    name = re.sub(r"\.{2,}", ".", name)
    return f"{id_}-{name}"


def export(args):
    if not os.getenv("FORGEJO_TOKEN"):
        raise RuntimeError("Set FORGEJO_TOKEN in your shell (never pass it as a CLI argument)")
    output = private_destination(args.out)
    output.mkdir(parents=True, exist_ok=True, mode=0o700)
    snapshot_path = output / "snapshot.json"
    if snapshot_path.exists() and not args.force:
        raise RuntimeError(f"Snapshot exists: {snapshot_path}; use --force for a fresh export, old data will be backed up")
    if snapshot_path.exists():
        backup = output / f"snapshot.backup.{int(time.time())}.json"
        snapshot_path.replace(backup)
    owner, repo = args.repo.split("/", 1)
    api = ForgejoAPI(args.forgejo, owner, repo, os.environ["FORGEJO_TOKEN"], interval=args.delay)
    root_repo = api.obj("")
    if not isinstance(root_repo, dict):
        raise RuntimeError("Repository metadata was not returned")
    base = {
        "schema": SCHEMA,
        "exported_at": stamp(),
        "source": {"forgejo": args.forgejo, "owner": owner, "repo": repo},
        "repository": root_repo,
        "issues": [], "pulls": [], "releases": [], "labels": [], "milestones": [],
        "downloads": [], "warnings": [], "api_request_count": 0,
    }
    base["issues"] = api.items("/issues", {"state": "all", "type": "issues"})
    base["issues"] = [x for x in base["issues"] if not x.get("pull_request")]
    base["pulls"] = api.items("/pulls", {"state": "all"})
    base["labels"] = api.items("/labels")
    base["milestones"] = api.items("/milestones", {"state": "all"})
    base["releases"] = api.items("/releases")
    unique_ints(base["issues"], "issue")
    unique_ints(base["pulls"], "PR")
    inum = set(unique_ints(base["issues"], "issue"))
    pnum = set(unique_ints(base["pulls"], "PR"))
    if inum & pnum:
        raise RuntimeError(f"Number collision between issue and PR exports: {sorted(inum & pnum)[:20]}")
    total = len(base["issues"]) + len(base["pulls"])
    print(f"Exporting {len(base['issues'])} issues and {len(base['pulls'])} PRs ({total} threads)...", flush=True)
    downloaded_ids = set()

    def maybe_download(attachments, group):
        if not isinstance(attachments, list):
            return
        for attachment in attachments:
            if not isinstance(attachment, dict):
                continue
            url = attachment.get("browser_download_url") or attachment.get("download_url")
            if not url:
                api.warnings.append(f"Attachment has no download URL: {group}")
                continue
            # Same asset can appear more than once in the API.
            key = (str(attachment.get("id")), str(url))
            if key in downloaded_ids:
                continue
            downloaded_ids.add(key)
            path = output / "assets" / group / asset_name(attachment)
            result = api.download(url, path)
            if result:
                result["relative_path"] = path.relative_to(output).as_posix()
                result["source"] = group
                result["attachment_id"] = attachment.get("id")
                base["downloads"].append(result)

    for kind in ("issues", "pulls"):
        for idx, thread in enumerate(base[kind], start=1):
            number = int(thread.get("number") or thread.get("index"))
            thread_path = f"/issues/{number}"
            thread["archive_comments"] = api.items(thread_path + "/comments")
            thread["archive_timeline"] = api.items(thread_path + "/timeline", optional=True)
            thread["archive_attachments"] = api.items(thread_path + "/assets", optional=True)
            if args.download_assets:
                maybe_download(thread["archive_attachments"], f"{kind}/{number}")
            for comment in thread["archive_comments"]:
                cid = comment.get("id")
                if cid is None:
                    continue
                comment["archive_attachments"] = api.items(f"/issues/comments/{cid}/assets", optional=True)
                if args.download_assets:
                    maybe_download(comment["archive_attachments"], f"{kind}/{number}/comments/{cid}")
            if kind == "pulls":
                thread["archive_reviews"] = api.items(f"/pulls/{number}/reviews", optional=True)
                thread["archive_commits"] = api.items(f"/pulls/{number}/commits", optional=True)
                thread["archive_files"] = api.items(f"/pulls/{number}/files", optional=True)
                for review in thread["archive_reviews"] or []:
                    rid = review.get("id")
                    if rid is not None:
                        review["archive_comments"] = api.items(f"/pulls/{number}/reviews/{rid}/comments", optional=True)
            if idx % 25 == 0 or idx == len(base[kind]):
                print(f"  {kind}: {idx}/{len(base[kind])}", flush=True)

    if args.download_assets:
        for release in base["releases"]:
            rid = release.get("id", "unknown")
            maybe_download(release.get("assets", []), f"releases/{rid}")

    base["issues"].sort(key=lambda x: int(x.get("number") or x.get("index")))
    base["pulls"].sort(key=lambda x: int(x.get("number") or x.get("index")))
    base["api_request_count"] = api.request_count
    base["warnings"] = api.warnings
    atomic_write(snapshot_path, json_bytes(base))
    manifest = {
        "schema": SCHEMA, "exported_at": base["exported_at"],
        "snapshot_sha256": digest(snapshot_path.read_bytes()),
        "counts": {"issues": len(base["issues"]), "pulls": len(base["pulls"]), "labels": len(base["labels"]),
                   "milestones": len(base["milestones"]), "releases": len(base["releases"]),
                   "downloaded_assets": len(base["downloads"]), "warnings": len(api.warnings)},
    }
    atomic_write(output / "manifest.json", json_bytes(manifest))
    print("Snapshot:", snapshot_path)
    print("SHA256:", manifest["snapshot_sha256"])
    if api.warnings:
        print(f"WARNING: {len(api.warnings)} optional endpoint/download warnings. Review before publishing!", file=sys.stderr)


def actor(obj):
    user = obj.get("user") or obj.get("poster") or {}
    return str(user.get("login") or user.get("username") or "unknown") if isinstance(user, dict) else "unknown"


def view_comment(c):
    return {"id": c.get("id"), "author": actor(c), "createdAt": c.get("created_at"),
            "updatedAt": c.get("updated_at"), "bodyMarkdown": c.get("body") or ""}


def public_view(data):
    issues = []
    prs = []
    for source, sink, kind in (("issues", issues, "issue"), ("pulls", prs, "pull")):
        for entry in data[source]:
            number = int(entry.get("number") or entry.get("index"))
            milestone = entry.get("milestone") or {}
            assignees = entry.get("assignees") or []
            base = {
                "kind": kind, "number": number, "title": entry.get("title") or "",
                "state": entry.get("state") or "unknown", "author": actor(entry),
                "createdAt": entry.get("created_at"), "updatedAt": entry.get("updated_at"),
                "closedAt": entry.get("closed_at"), "bodyMarkdown": entry.get("body") or "",
                "labels": [x["name"] for x in entry.get("labels") or []
                           if isinstance(x, dict) and isinstance(x.get("name"), str) and x["name"]],
                "milestone": milestone.get("title") if isinstance(milestone, dict) else None,
                "assignees": [x["login"] for x in assignees
                              if isinstance(x, dict) and isinstance(x.get("login"), str) and x["login"]],
                "comments": [view_comment(c) for c in entry.get("archive_comments") or []],
            }
            if kind == "pull":
                base["mergedAt"] = entry.get("merged_at")
                base["merged"] = bool(entry.get("merged"))
                base["baseBranch"] = (entry.get("base") or {}).get("ref") if isinstance(entry.get("base"), dict) else None
                base["headBranch"] = (entry.get("head") or {}).get("ref") if isinstance(entry.get("head"), dict) else None
                base["reviews"] = [{"author": actor(r), "state": r.get("state"), "bodyMarkdown": r.get("body") or "",
                                    "submittedAt": r.get("submitted_at"),
                                    "comments": [view_comment(c) for c in r.get("archive_comments") or []]}
                                   for r in entry.get("archive_reviews") or []]
            sink.append(base)
    return {
        "schemaVersion": SCHEMA,
        "archive": "Road to v1.0.0 — Forgejo Era",
        "generatedAt": data["exported_at"],
        "source": "Forgejo", "repository": "MelanX/ephervex",
        "routeContract": {"home": "/road-to-v1/", "issue": "/road-to-v1/forgejo/issues/{number}/",
                          "pull": "/road-to-v1/forgejo/pulls/{number}/"},
        "issues": issues, "pulls": prs,
        "milestones": [{"title": x.get("title"), "state": x.get("state"), "description": x.get("description") or ""}
                       for x in data["milestones"]],
        "labels": [{"name": x.get("name"), "color": x.get("color"), "description": x.get("description") or ""}
                   for x in data["labels"]],
        "releases": [{"tag": x.get("tag_name"), "name": x.get("name"), "createdAt": x.get("created_at"),
                      "publishedAt": x.get("published_at"), "bodyMarkdown": x.get("body") or "", "draft": x.get("draft")}
                     for x in data["releases"]],
    }


def verify_snapshot(directory):
    root = Path(directory).expanduser().resolve()
    snapshot = root / "snapshot.json"
    man = read_json(root / "manifest.json")
    content = snapshot.read_bytes()
    if digest(content) != man["snapshot_sha256"]:
        raise RuntimeError("Snapshot SHA256 mismatch")
    data = json.loads(content)
    if data.get("schema") != SCHEMA:
        raise RuntimeError("Unsupported schema")
    ins = set(unique_ints(data["issues"], "issue"))
    prs = set(unique_ints(data["pulls"], "PR"))
    if ins & prs:
        raise RuntimeError("Issue/PR namespace collision")
    if man["counts"]["issues"] != len(ins) or man["counts"]["pulls"] != len(prs):
        raise RuntimeError("Manifest totals do not match snapshot")
    for asset in data.get("downloads", []):
        rel = Path(asset["relative_path"])
        if rel.is_absolute() or ".." in rel.parts or rel.parts[0] != "assets":
            raise RuntimeError("Unsafe asset path in manifest")
        if digest((root / rel).read_bytes()) != asset["sha256"]:
            raise RuntimeError(f"Asset integrity failure: {rel}")
    print(f"Verified: {len(ins)} issues, {len(prs)} PRs, {len(data.get('downloads', []))} assets, {len(data.get('warnings', []))} warnings")
    return data


def prepare_public(args):
    data = verify_snapshot(args.input)
    if data.get("repository", {}).get("private") and not getattr(args, "allow_private_source", False):
        raise RuntimeError("Source repository is private. Export cannot automatically be published. Review manually.")
    if data.get("warnings") and not args.allow_incomplete:
        raise RuntimeError("Export has warnings. Investigate, or explicitly use --allow-incomplete AFTER review")
    out = private_destination(args.out)
    if is_inside(out, Path(args.input)):
        raise RuntimeError("Public candidate must be outside the private snapshot")
    if out.exists() and any(out.iterdir()):
        raise RuntimeError("Public destination must be empty; never overwrite reviewed content")
    out.mkdir(parents=True, exist_ok=True)
    dataset = public_view(data)
    payload = json_bytes(dataset)
    atomic_write(out / "archive-data.json", payload)
    warnings = [
        "NOT APPROVED FOR PUBLICATION. Inspect EVERY issue/PR/comment/review body.",
        "Search for credentials, tokens, IPs, internal hostnames, user emails, private links and personal data.",
        "Check quoted code snippets and historical URLs. Original Forgejo source hostname is deliberately omitted from public JSON metadata.",
        "Attachments and release binaries are NOT included in this candidate and require individual audit before publishing.",
        "Do not expose the raw snapshot.json, raw API data, authenticated URLs or API tokens in web/static/.",
        "Public Markdown may include raw HTML; Docusaurus renderer must sanitize or escape it (no dangerous HTML).",
        "Verify licensing and redistribution rights for third-party issue comments and attached files.",
        "Verify permanent link paths BEFORE rewriting Git commit messages; never change paths afterwards.",
        "If this server was private at any time, audit the entire export even if repository.private currently reports false.",
    ]
    atomic_write(out / "REVIEW-BEFORE-PUBLISHING.md", ("# Manual publication review required\n\n" + "\n".join(f"- [ ] {x}" for x in warnings) + "\n").encode())
    atomic_write(out / "PUBLIC-DATA-SHA256.txt", (digest(payload) + "  archive-data.json\n").encode())
    print("Prepared UNAPPROVED public candidate:", out)
    print("Data SHA256:", digest(payload))
    print("Review checklist:", out / "REVIEW-BEFORE-PUBLISHING.md")


def probe(args):
    """Read-only connectivity check; never changes Forgejo or creates files."""
    token = os.getenv("FORGEJO_TOKEN")
    if not token:
        raise RuntimeError("Set FORGEJO_TOKEN in your shell")
    owner, repo = args.repo.split("/", 1)
    api = ForgejoAPI(args.forgejo, owner, repo, token, interval=0)
    record = api.obj("")
    if not isinstance(record, dict):
        raise RuntimeError("Unexpected repository response")
    issue_page = api.items("/issues", {"state": "all", "type": "issues"})
    prs_page = api.items("/pulls", {"state": "all"})
    print(json.dumps({"repository": record.get("full_name"), "private": record.get("private"),
                      "issues_observed": len(issue_page), "pulls_observed": len(prs_page),
                      "read_only": True}, indent=2))



def export_markdown(thread):
    n = thread["number"]
    kind = "Issue" if thread["kind"] == "issue" else "Pull request"
    lines = [f"# {kind} #{n}: {thread.get('title') or ''}", "",
             f"- State: {thread.get('state')}",
             f"- Author: {thread.get('author')}",
             f"- Created: {thread.get('createdAt')}",
             f"- Closed: {thread.get('closedAt')}",
             f"- Milestone: {thread.get('milestone')}",
             "- Labels: " + ", ".join(thread.get("labels") or []),
             "", "## Description", "", thread.get("bodyMarkdown") or "", ""]
    for comment in thread.get("comments") or []:
        lines += [f"## Comment by {comment.get('author')} ({comment.get('createdAt')})", "",
                  comment.get("bodyMarkdown") or "", ""]
    for review in thread.get("reviews") or []:
        lines += [f"## Review by {review.get('author')} ({review.get('state')})", "",
                  review.get("bodyMarkdown") or "", ""]
        for comment in review.get("comments") or []:
            lines += [f"### Review comment by {comment.get('author')}", "", comment.get("bodyMarkdown") or "", ""]
    return ("\n".join(lines).rstrip() + "\n").encode("utf-8")


def package_public(args):
    """Explicit checksum-confirmed package, only allowlisted public fields."""
    folder = Path(args.input).expanduser().resolve()
    file = folder / "archive-data.json"
    payload = file.read_bytes()
    current_sha = digest(payload)
    if args.approve_sha256.lower() != current_sha:
        raise RuntimeError("Approval SHA256 differs from archive-data.json; review the current file before packaging")
    data = json.loads(payload)
    if data.get("schemaVersion") != SCHEMA:
        raise RuntimeError("Unsupported archive data schema")
    issues, pulls = data.get("issues"), data.get("pulls")
    if not isinstance(issues, list) or not isinstance(pulls, list):
        raise RuntimeError("Missing public issue/PR arrays")
    unique_ints(issues, "public issue")
    unique_ints(pulls, "public PR")
    if set(unique_ints(issues, "public issue")) & set(unique_ints(pulls, "public PR")):
        raise RuntimeError("Public issue/PR namespace collision")
    dest = Path(args.out).expanduser().resolve()
    if dest.exists():
        raise RuntimeError("Destination already exists; refusing to overwrite a published archive")
    dest.parent.mkdir(parents=True, exist_ok=True)
    contents = {
        "archive-data.json": payload,
        "README.md": ("# Ephervex — Road to v1.0.0\n\n"
                      "This is a manually reviewed export of historical Forgejo discussions. "
                      "Original issue/PR IDs and historical authors/timestamps are preserved as data. "
                      "This is not a runnable Forgejo server backup. Binary attachments are not included.\n\n"
                      "Permanent web routes: /road-to-v1/forgejo/issues/NUMBER/ "
                      "and /road-to-v1/forgejo/pulls/NUMBER/.\n").encode("utf-8"),
    }
    for row in issues:
        contents[f"issues/{int(row['number'])}.md"] = export_markdown(row)
    for row in pulls:
        contents[f"pull-requests/{int(row['number'])}.md"] = export_markdown(row)
    contents["manifest.json"] = json_bytes({
        "schemaVersion": SCHEMA, "name": "Ephervex Road to v1.0.0", "datasetSha256": current_sha,
        "issues": len(issues), "pulls": len(pulls), "datasetGeneratedAt": data.get("generatedAt"),
    })
    sums = "".join(digest(content) + "  " + name + "\n" for name, content in sorted(contents.items()))
    contents["SHA256SUMS"] = sums.encode("utf-8")
    tmp = dest.with_name(dest.name + ".tmp")
    try:
        with zipfile.ZipFile(tmp, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=6) as archive:
            for name, content in sorted(contents.items()):
                entry = zipfile.ZipInfo("ephervex-road-to-v1/" + name, date_time=(1980, 1, 1, 0, 0, 0))
                entry.compress_type = zipfile.ZIP_DEFLATED
                entry.create_system = 3
                entry.external_attr = 0o644 << 16
                archive.writestr(entry, content, compress_type=zipfile.ZIP_DEFLATED, compresslevel=6)
        os.replace(tmp, dest)
    finally:
        tmp.unlink(missing_ok=True)
    print(f"Reviewed public archive: {dest} ({len(issues)} issues, {len(pulls)} PRs)")
    print("Archive SHA256:", digest(dest.read_bytes()))


def cli():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    pro = sub.add_parser("probe", help="Read-only API access/count check (no local files)")
    pro.add_argument("--forgejo", default=DEFAULT_FORGEJO)
    pro.add_argument("--repo", default=DEFAULT_REPO)
    pro.set_defaults(func=probe)
    exp = sub.add_parser("export", help="Read-only Forgejo API export into PRIVATE directory")
    exp.add_argument("--forgejo", default=DEFAULT_FORGEJO)
    exp.add_argument("--repo", default=DEFAULT_REPO)
    exp.add_argument("--out", required=True, help="Private directory outside the Git repo")
    exp.add_argument("--download-assets", action="store_true", help="Download same-origin attachments and release assets")
    exp.add_argument("--force", action="store_true", help="Back up existing snapshot, re-export")
    exp.add_argument("--delay", type=float, default=0.1, help="Delay between API requests in seconds")
    exp.set_defaults(func=export)
    ver = sub.add_parser("verify", help="Check private snapshot + downloaded file checksums")
    ver.add_argument("--input", required=True)
    ver.set_defaults(func=lambda a: verify_snapshot(a.input))
    pub = sub.add_parser("prepare-public", help="Make UNAPPROVED minimal JSON candidate for Docusaurus")
    pub.add_argument("--input", required=True)
    pub.add_argument("--out", required=True)
    pub.add_argument("--allow-incomplete", action="store_true", help="Explicitly acknowledge API export warnings")
    pub.add_argument("--allow-private-source", action="store_true", help="Create UNAPPROVED candidate even if source repo is private")
    pub.set_defaults(func=prepare_public)
    pack = sub.add_parser("package-public", help="After manual audit, create downloadable ZIP of PUBLIC data only")
    pack.add_argument("--input", required=True, help="Directory containing reviewed archive-data.json")
    pack.add_argument("--out", required=True, help="Output ZIP path, must not exist")
    pack.add_argument("--approve-sha256", required=True, help="Checksum manually confirmed after audit")
    pack.set_defaults(func=package_public)
    args = parser.parse_args()
    os.umask(0o077)
    try:
        args.func(args)
    except (RuntimeError, ValueError, OSError) as exc:
        parser.exit(2, f"ERROR: {exc}\n")


if __name__ == "__main__":
    cli()
