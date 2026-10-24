#!/usr/bin/env python3
"""Populate the local language-catalog cache used by analyze_options.py.

This is the only part of the research that touches the network, and it is a
separate, explicitly invoked step: `analyze_options.py` itself stays offline and
deterministic, reading whatever this script has already cached.

Nothing here executes Minecraft.  It reads Mojang's published version manifest,
each release's asset index, and the single `pack.mcmeta` asset object whose
`language` metadata section is exactly what LanguageManager enumerates at
runtime.  Only that distilled section is stored, together with the hashes that
prove where it came from. Every downloaded document is checked against the
SHA-1 its source published, and a mismatch stops that release.

`update.py` calls `refresh` directly, with the same version manifest it uses for
everything else.
"""

from __future__ import annotations

import argparse
import sys
import urllib.error
from pathlib import Path
from typing import Any

import official_sources
from file_io import read_json_object, stable_json
from official_sources import SourceError

TOOL_DIR = Path(__file__).resolve().parent

# Stored in every record collected with each downloaded document hash-verified.
# Records without it predate that check and are fetched again.
VERIFIED_RECORD_FORMAT = 2


def collect(version: str, entry: dict[str, Any]) -> dict[str, Any]:
    meta = official_sources.json_object(
        official_sources.fetch_verified(
            entry.get("url"), entry.get("sha1"), f"version metadata for {version}"),
        f"version metadata for {version}")
    index_info = meta.get("assetIndex")
    if not index_info:
        return {"version": version, "record_format": VERIFIED_RECORD_FORMAT, "status": "no_asset_index"}
    if not isinstance(index_info, dict):
        raise SourceError(f"version metadata for {version} has a malformed asset index entry")
    index = official_sources.json_object(
        official_sources.fetch_verified(
            index_info.get("url"), index_info.get("sha1"), f"asset index of {version}"),
        f"asset index of {version}")
    objects = index.get("objects")
    if not isinstance(objects, dict):
        raise SourceError(f"asset index of {version} lists no objects")
    mcmeta = objects.get("pack.mcmeta")
    # Pre-1.13 releases ship .lang property files; 1.13+ ship .json.
    lang_paths = sorted(k for k in objects if "/lang/" in k and k.endswith((".json", ".lang")))
    record: dict[str, Any] = {
        "version": version,
        "record_format": VERIFIED_RECORD_FORMAT,
        "status": "ok",
        "asset_index": {"id": index_info.get("id"), "sha1": index_info.get("sha1"),
                        "url": index_info["url"]},
        "version_manifest_entry": {"url": entry["url"], "sha1": entry.get("sha1"),
                                   "release_time": entry.get("releaseTime")},
        "lang_resource_paths": lang_paths,
        "lang_resource_count": len(lang_paths),
    }
    if not mcmeta:
        record["status"] = "no_pack_mcmeta"
        record["languages"] = {}
        return record
    if not isinstance(mcmeta, dict):
        raise SourceError(f"asset index of {version} has a malformed pack.mcmeta entry")
    # The hash is validated as hex before it becomes part of the resource URL.
    sha1 = official_sources.require_sha1(mcmeta.get("hash"), f"pack.mcmeta of {version}")
    blob = official_sources.fetch_verified(
        f"{official_sources.RESOURCES_URL}/{sha1[:2]}/{sha1}", sha1, f"pack.mcmeta of {version}")
    record["pack_mcmeta"] = {"sha1": sha1, "size": mcmeta.get("size"),
                             "verified_sha1": sha1, "sha1_matches": True}
    section = official_sources.json_object(blob, f"pack.mcmeta of {version}").get("language", {})
    if not isinstance(section, dict) or not all(isinstance(info, dict) for info in section.values()):
        raise SourceError(f"pack.mcmeta of {version} has a malformed language section")
    record["languages"] = {code: {"name": info.get("name"), "region": info.get("region"),
                                  "bidirectional": bool(info.get("bidirectional"))}
                           for code, info in section.items()}
    record["language_count"] = len(record["languages"])
    return record


def read_cached(path: Path) -> dict[str, Any] | None:
    """The cached record at `path`, or None when it is absent or damaged.

    A damaged record is reported and then fetched again.
    """
    if not path.exists():
        return None
    try:
        return read_json_object(path, "cached language catalog")
    except ValueError as error:
        print(f"[cache] {error}; fetching it again", file=sys.stderr, flush=True)
        return None


def is_current(record: dict[str, Any], entry: dict[str, Any]) -> bool:
    """A cached record that is complete, verified, and from the manifest's current metadata."""
    return (
        record.get("record_format") == VERIFIED_RECORD_FORMAT
        and record.get("status") == "ok"
        and (record.get("pack_mcmeta") or {}).get("sha1_matches") is True
        and (record.get("version_manifest_entry") or {}).get("sha1") == entry.get("sha1")
    )


def refresh(cache: Path, document: dict[str, Any], versions: list[str],
            force: bool = False) -> tuple[dict[str, dict[str, Any]], dict[str, str]]:
    """Brings each release's catalog in `cache` up to date with `document`.

    A cached record is reused only when it is complete, verified and from the
    manifest's current metadata; a failed or partial record is fetched again.
    Returns the usable records and, for every release without one, the reason.
    """
    by_id = {entry["id"]: entry for entry in document["versions"]}
    records: dict[str, dict[str, Any]] = {}
    failures: dict[str, str] = {}
    for version in versions:
        try:
            official_sources.require_version_id(version)
            entry = by_id.get(version)
            if entry is None:
                raise SourceError("not in the version manifest")
            # Checked before the cache is consulted: an entry with no hash must fail
            # whether or not a record for it exists.
            official_sources.require_sha1(entry.get("sha1"), f"version metadata for {version}")
            target = cache / "language" / f"{version}.json"
            cached = None if force else read_cached(target)
            if cached is not None and is_current(cached, entry):
                print(f"[cache] {version}: already present", flush=True)
                records[version] = cached
                continue
            record = collect(version, entry)
        except (SourceError, urllib.error.URLError, OSError, ValueError) as error:
            failures[version] = str(error)
            print(f"[error] {version}: {error}", flush=True)
            continue
        stable_json(target, record)
        if record["status"] != "ok":
            failures[version] = f"the release publishes no language catalog ({record['status']})"
            print(f"[error] {version}: {failures[version]}", flush=True)
            continue
        records[version] = record
        print(f"[fetch] {version}: {record.get('language_count', 0)} languages, "
              f"{record.get('lang_resource_count', 0)} lang resources", flush=True)
    return records, failures


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cache", type=Path, default=TOOL_DIR / "assets-cache")
    parser.add_argument("--manifest", type=Path,
                        default=official_sources.DEFAULT_CACHE / official_sources.MANIFEST_NAME,
                        help="cached version manifest, shared with update.py")
    parser.add_argument("--versions", nargs="*", help="Release ids to collect (default: all cached JAR versions)")
    parser.add_argument("--inventory", type=Path, default=TOOL_DIR / "output" / "inventory.json")
    parser.add_argument("--force", action="store_true",
                        help="re-read the version manifest and fetch every catalog again")
    args = parser.parse_args()

    versions = args.versions
    if not versions:
        if args.inventory.exists():
            try:
                inventory = read_json_object(args.inventory, "analyzer inventory")
                versions = sorted({j["detected_version"] for j in inventory["jars"]
                                   if j.get("detected_version")})
            except (ValueError, KeyError, TypeError, AttributeError) as error:
                print(f"[error] the analyzer inventory {args.inventory} is unusable ({error}); "
                      "run the analyzer again or pass --versions", file=sys.stderr)
                return 1
        else:
            print("no --versions and no inventory.json; nothing to do", file=sys.stderr)
            return 1

    try:
        document = official_sources.load_manifest(
            args.manifest, online=True, refresh=args.force, needs=versions)
    except (SourceError, urllib.error.URLError, OSError) as error:
        print(f"[error] version manifest: {error}", file=sys.stderr)
        return 1
    args.cache.mkdir(parents=True, exist_ok=True)
    records, failures = refresh(args.cache, document, versions, args.force)
    print(f"[done ] {len(records)}/{len(versions)} versions cached in {args.cache}", flush=True)
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
