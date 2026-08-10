#!/usr/bin/env python3
"""Populate the local language-catalog cache used by analyze_options.py.

This is the only part of the research that touches the network, and it is a
separate, explicitly invoked step: `analyze_options.py` itself stays offline and
deterministic, reading whatever this script has already cached.

Nothing here executes Minecraft.  It reads Mojang's published version manifest,
each release's asset index, and the single `pack.mcmeta` asset object whose
`language` metadata section is exactly what LanguageManager enumerates at
runtime.  Only that distilled section is stored, together with the hashes that
prove where it came from.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any

MANIFEST_URL = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json"
RESOURCES = "https://resources.download.minecraft.net"
USER_AGENT = "minecraft-options-research/2.0 (static options.txt analysis; contact: local research)"


def get(url: str, timeout: int = 60) -> bytes:
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    with urllib.request.urlopen(request, timeout=timeout) as response:
        return response.read()


def stable_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False, sort_keys=True) + "\n",
                    encoding="utf-8")


def collect(version: str, entry: dict[str, Any], cache: Path) -> dict[str, Any]:
    meta = json.loads(get(entry["url"]))
    index_info = meta.get("assetIndex")
    if not index_info:
        return {"version": version, "status": "no_asset_index"}
    index = json.loads(get(index_info["url"]))
    objects = index.get("objects", {})
    mcmeta = objects.get("pack.mcmeta")
    # Pre-1.13 releases ship .lang property files; 1.13+ ship .json.
    lang_paths = sorted(k for k in objects if "/lang/" in k and k.endswith((".json", ".lang")))
    record: dict[str, Any] = {
        "version": version,
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
    blob = get(f"{RESOURCES}/{mcmeta['hash'][:2]}/{mcmeta['hash']}")
    digest = hashlib.sha1(blob).hexdigest()
    record["pack_mcmeta"] = {"sha1": mcmeta["hash"], "size": mcmeta["size"],
                             "verified_sha1": digest, "sha1_matches": digest == mcmeta["hash"]}
    section = json.loads(blob).get("language", {})
    record["languages"] = {code: {"name": info.get("name"), "region": info.get("region"),
                                  "bidirectional": bool(info.get("bidirectional"))}
                           for code, info in section.items()}
    record["language_count"] = len(record["languages"])
    return record


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cache", type=Path,
                        default=Path(__file__).resolve().parent / "assets-cache")
    parser.add_argument("--versions", nargs="*", help="Release ids to collect (default: all cached JAR versions)")
    parser.add_argument("--inventory", type=Path,
                        default=Path(__file__).resolve().parent / "output" / "inventory.json")
    parser.add_argument("--force", action="store_true")
    args = parser.parse_args()

    cache = args.cache
    cache.mkdir(parents=True, exist_ok=True)
    manifest_path = cache / "version_manifest_v2.json"
    if args.force or not manifest_path.exists():
        manifest_path.write_bytes(get(MANIFEST_URL))
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    by_id = {v["id"]: v for v in manifest["versions"]}

    versions = args.versions
    if not versions:
        if args.inventory.exists():
            inventory = json.loads(args.inventory.read_text(encoding="utf-8"))
            versions = sorted({j["detected_version"] for j in inventory["jars"]
                               if j.get("detected_version")})
        else:
            print("no --versions and no inventory.json; nothing to do", file=sys.stderr)
            return 1

    ok = 0
    for version in versions:
        target = cache / "language" / f"{version}.json"
        if target.exists() and not args.force:
            print(f"[cache] {version}: already present", flush=True)
            ok += 1
            continue
        entry = by_id.get(version)
        if entry is None:
            stable_json(target, {"version": version, "status": "not_in_manifest"})
            print(f"[miss ] {version}: not in the version manifest", flush=True)
            continue
        try:
            record = collect(version, entry, cache)
        except (urllib.error.URLError, OSError, ValueError) as exc:
            print(f"[error] {version}: {exc}", flush=True)
            continue
        stable_json(target, record)
        ok += 1
        print(f"[fetch] {version}: {record.get('language_count', 0)} languages, "
              f"{record.get('lang_resource_count', 0)} lang resources", flush=True)
    print(f"[done ] {ok}/{len(versions)} versions cached in {cache}", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
