#!/usr/bin/env python3
"""Analyze a Minecraft release and update Ephervex's options compatibility data.

This is the one maintainer workflow for this subsystem.

    python3 tools/minecraft-options/update.py --latest
    python3 tools/minecraft-options/update.py --version 26.2
    python3 tools/minecraft-options/update.py --from-analysis <dir> --offline
    python3 tools/minecraft-options/update.py --check

It reads Mojang's published version manifest, selects a release (never a
snapshot), fetches and hash-verifies that release's official client JAR into an
untracked development cache, analyzes it statically without ever executing
Minecraft, refreshes the release's official language catalog, folds the result
into the committed dataset, and writes the exact JSON to publish to the remote
options feed.

Nothing here needs GitHub credentials and nothing here publishes anything. The
generated feed file is written locally and its path is printed.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
import sys
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any

import dataset
import preferences

TOOL_DIR = Path(__file__).resolve().parent
REPO_ROOT = TOOL_DIR.parent.parent
BUNDLED_DATASET = REPO_ROOT / "crates" / "options" / "data" / "minecraft-options.json"
BUNDLED_PREFERENCE_CATALOG = REPO_ROOT / "crates" / "options" / "data" / "minecraft-preferences.json"
DEFAULT_CACHE = TOOL_DIR / ".cache"

MANIFEST_URL = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json"
USER_AGENT = "ephervex-minecraft-options/1 (static options.txt analysis)"

# Matches the launcher's own floor. Below it Ephervex neither installs nor
# launches, so compatibility data for those releases would describe something
# the application cannot produce.
MINIMUM_SUPPORTED_VERSION = "1.12.2"


class WorkflowError(Exception):
    """The workflow cannot continue, with a reason worth printing."""


# ------------------------------------------------------------------- fetching


def fetch(url: str, timeout: int = 120) -> bytes:
    if not url.startswith("https://"):
        raise WorkflowError(f"refusing to fetch over a non-https url: {url}")
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    with urllib.request.urlopen(request, timeout=timeout) as response:  # noqa: S310
        return response.read()


def manifest(cache: Path, refresh: bool) -> dict[str, Any]:
    path = cache / "version_manifest_v2.json"
    if refresh or not path.exists():
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(fetch(MANIFEST_URL))
    return json.loads(path.read_text(encoding="utf-8"))


def select_release(document: dict[str, Any], wanted: str | None) -> dict[str, Any]:
    """The requested release, or Mojang's own latest release.

    Only `type == "release"` entries qualify. A snapshot or release candidate
    is refused rather than analyzed: Ephervex does not offer them, so producing
    compatibility data for one would describe a target the launcher will not
    install.
    """
    releases = {entry["id"]: entry for entry in document["versions"] if entry["type"] == "release"}
    target = wanted or document["latest"]["release"]
    entry = releases.get(target)
    if entry is None:
        if any(version["id"] == target for version in document["versions"]):
            raise WorkflowError(f"`{target}` is not a release; only releases are analyzed")
        raise WorkflowError(f"`{target}` is not in Mojang's version manifest")
    if dataset.version_key(target) < dataset.version_key(MINIMUM_SUPPORTED_VERSION):
        raise WorkflowError(f"`{target}` predates the supported floor {MINIMUM_SUPPORTED_VERSION}")
    return entry


def client_jar(entry: dict[str, Any], cache: Path) -> Path:
    """The official client JAR, verified against Mojang's published SHA-1."""
    jars = cache / "jars"
    jars.mkdir(parents=True, exist_ok=True)
    target = jars / f"mc-{entry['id']}.jar"

    metadata_path = cache / "versions" / f"{entry['id']}.json"
    metadata_path.parent.mkdir(parents=True, exist_ok=True)
    if not metadata_path.exists():
        body = fetch(entry["url"])
        digest = hashlib.sha1(body).hexdigest()  # noqa: S324 - Mojang publishes sha1
        if entry.get("sha1") and digest != entry["sha1"]:
            raise WorkflowError(f"version metadata for {entry['id']} failed its sha1 check")
        metadata_path.write_bytes(body)
    metadata = json.loads(metadata_path.read_text(encoding="utf-8"))

    download = (metadata.get("downloads") or {}).get("client")
    if not download:
        raise WorkflowError(f"{entry['id']} publishes no client download")
    if target.exists() and hashlib.sha1(target.read_bytes()).hexdigest() == download["sha1"]:  # noqa: S324
        return target

    body = fetch(download["url"])
    digest = hashlib.sha1(body).hexdigest()  # noqa: S324
    if digest != download["sha1"]:
        raise WorkflowError(f"client jar for {entry['id']} failed its sha1 check")
    target.write_bytes(body)
    return target


# ------------------------------------------------------------------ analysing


def analyzer_command(jar: Path, output: Path, assets_cache: Path) -> list[str]:
    """The deterministic command for analyzing exactly one selected client JAR."""
    return [
        sys.executable,
        str(TOOL_DIR / "analyze_options.py"),
        "--root",
        str(jar.parent),
        "--input-jar",
        str(jar),
        "--output",
        str(output),
        "--assets-cache",
        str(assets_cache),
    ]


def run_analyzer(cache: Path, jar: Path) -> Path:
    """Runs the static analyzer over only the selected client JAR."""
    output = cache / "analysis" / jar.stem
    output.mkdir(parents=True, exist_ok=True)
    completed = subprocess.run(  # noqa: S603
        analyzer_command(jar, output, cache / "assets-cache"),
        check=False,
    )
    if completed.returncode != 0:
        raise WorkflowError("the static analyzer failed; see its output above")
    return output


def refresh_language_catalog(cache: Path, version: str) -> dict[str, Any]:
    """Refreshes one release's official language catalog in the dev cache."""
    completed = subprocess.run(  # noqa: S603
        [
            sys.executable,
            str(TOOL_DIR / "fetch_language_catalogs.py"),
            "--cache",
            str(cache / "assets-cache"),
            "--versions",
            version,
        ],
        check=False,
    )
    if completed.returncode != 0:
        raise WorkflowError("the language catalog fetch failed; see its output above")
    path = cache / "assets-cache" / "language" / f"{version}.json"
    if not path.exists():
        return {"status": "missing"}
    return json.loads(path.read_text(encoding="utf-8"))


def normalized_records(analysis: Path, only: set[str] | None) -> list[dict[str, Any]]:
    """Every analyzed release inside the supported range, newest last."""
    directory = analysis / "normalized"
    if not directory.is_dir():
        raise WorkflowError(f"no normalized analyzer output under {directory}")
    records = []
    for path in sorted(directory.glob("*.json")):
        record = json.loads(path.read_text(encoding="utf-8"))
        version = record.get("minecraft_version")
        if not version or not record.get("extraction_complete"):
            continue
        if dataset.version_key(version) < dataset.version_key(MINIMUM_SUPPORTED_VERSION):
            continue
        if only is not None and version not in only:
            continue
        records.append(record)
    if not records:
        raise WorkflowError("the analyzer produced no usable supported release")
    return records


# -------------------------------------------------------------------- writing


def read_bundled() -> dict[str, Any] | None:
    if not BUNDLED_DATASET.exists():
        return None
    return json.loads(BUNDLED_DATASET.read_text(encoding="utf-8"))


def read_bundled_preferences() -> dict[str, Any] | None:
    if not BUNDLED_PREFERENCE_CATALOG.exists():
        return None
    return json.loads(BUNDLED_PREFERENCE_CATALOG.read_text(encoding="utf-8"))


def preference_catalog_candidates(
    document: dict[str, Any], preference_catalog: dict[str, Any] | None
) -> list[str]:
    """Current, manageable settings this compatibility update recorded that
    the bundled product catalog does not yet expose or explicitly exclude.

    Read-only: this never regenerates or writes the catalog. Growing the
    product surface is a separate, reviewed action
    (`preferences.py --from-analysis ...`), never a side effect of an ordinary
    compatibility update.
    """
    if preference_catalog is None:
        return []
    accounted = {
        field["canonical_id"] for descriptor in preference_catalog["descriptors"] for field in descriptor["fields"]
    }
    accounted.update(exclusion["canonical_id"] for exclusion in preference_catalog["exclusions"])
    registry = {entry["id"]: entry for entry in document["setting_registry"]}
    return sorted(
        setting_id
        for setting_id in document["current_canonical_setting_ids"]
        if setting_id not in accounted and registry[setting_id]["manageable"]
    )


def summarize(
    document: dict[str, Any],
    previous: dict[str, Any] | None,
    changes: list[dict[str, str]],
    records: list[dict[str, Any]],
    catalog: dict[str, Any] | None,
    preference_catalog: dict[str, Any] | None = None,
) -> list[str]:
    lines: list[str] = []
    mapping = {entry["minecraft_version"]: entry["schema"] for entry in document["releases"]}
    before = (
        {entry["minecraft_version"]: entry["schema"] for entry in previous["releases"]}
        if previous
        else {}
    )
    settings_before = (
        {
            entry["id"]
            for entry in previous.get("setting_registry", previous.get("settings", []))
        }
        if previous
        else set()
    )
    settings_now = {entry["id"] for entry in document["setting_registry"]}

    established = set(before.values())
    for record in records:
        version = record["minecraft_version"]
        schema_id = mapping[version]
        if version in before and before[version] == schema_id:
            lines.append(f"release {version}: options representation unchanged")
        elif version in before:
            lines.append(f"release {version}: options representation changed")
        else:
            lines.append(f"new release {version}")
        if schema_id in established:
            lines.append(f"  mapped to existing schema {schema_id}")
        else:
            lines.append(f"  new schema {schema_id}")
            established.add(schema_id)
        extra = schema_for_extras(record, document)
        if extra:
            lines.append(
                f"  {len(extra)} options.txt key(s) with no canonical definition: {', '.join(extra[:8])}"
            )

    introduced = sorted(settings_now - settings_before)
    if introduced:
        lines.append(
            f"{len(introduced)} canonical setting(s) newly recorded: {', '.join(introduced[:8])}"
        )
        lines.append(
            "  recorded as data only; exposing one as an Ephervex preference needs an application release"
        )

    candidates = preference_catalog_candidates(document, preference_catalog)
    if candidates:
        lines.append(
            f"{len(candidates)} product-catalog candidate(s) need application review: "
            f"{', '.join(candidates[:8])}"
        )
        lines.append(
            "  the bundled preference catalog does not expose or exclude them yet; review with "
            "preferences.py before they can appear on a settings screen"
        )

    if catalog is not None:
        count = catalog.get("language_count")
        lines.append(
            f"language catalog refreshed: {count} codes"
            if count is not None
            else f"language catalog: {catalog.get('status')}"
        )

    review = application_review_reasons(records, document)
    remapped = [change for change in changes if change["kind"] != "release-unchanged"]
    if previous is None:
        lines.append("bundled dataset created")
    elif not remapped and settings_now == settings_before:
        lines.append("bundled dataset unchanged")
    else:
        lines.append(
            f"bundled dataset revision {previous['revision']} -> {document['revision']}"
        )
    if review:
        lines.append("Ephervex application review/update required:")
        lines.extend(f"  {setting_id}: {reason}" for setting_id, reason in review)
    elif previous is not None:
        lines.append("no Ephervex application update required for a mapping-only change")
    return lines


def schema_for_extras(record: dict[str, Any], document: dict[str, Any]) -> list[str]:
    """Persisted keys this release has that no canonical definition covers."""
    known = {entry["key"] for entry in document["setting_registry"]}
    return sorted({setting["persisted_key"] for setting in record["settings"]} - known)


def application_review_reasons(
    records: list[dict[str, Any]], document: dict[str, Any]
) -> list[tuple[str, str]]:
    """Managed canonical ids the selected release cannot safely encode."""
    registry = {entry["key"]: entry["id"] for entry in document["setting_registry"]}
    reasons: list[tuple[str, str]] = []
    for record in records:
        code_space = record.get("keybinding_code_space")
        by_id = {
            registry[setting["persisted_key"]]: setting
            for setting in record["settings"]
            if setting["persisted_key"] in registry
        }
        for setting_id in dataset.REQUIRED_CANONICAL_IDS:
            setting = by_id.get(setting_id)
            if setting is None:
                reasons.append((setting_id, "the analyzer did not recover the setting"))
                continue
            issue = dataset.representation_issue(setting, code_space)
            if issue is not None:
                reasons.append((setting_id, issue))
    return sorted(set(reasons))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    selection = parser.add_mutually_exclusive_group()
    selection.add_argument("--latest", action="store_true", help="analyze Mojang's newest release")
    selection.add_argument("--version", help="analyze one specific Minecraft release")
    parser.add_argument(
        "--from-analysis",
        type=Path,
        help="fold an existing analyzer output directory in instead of running the analyzer",
    )
    parser.add_argument("--offline", action="store_true", help="never touch the network")
    parser.add_argument("--cache", type=Path, default=DEFAULT_CACHE, help="untracked development cache")
    parser.add_argument(
        "--check",
        action="store_true",
        help="verify the committed dataset matches what the inputs regenerate, and write nothing",
    )
    parser.add_argument("--refresh-manifest", action="store_true", help="re-read the version manifest")
    args = parser.parse_args()

    cache: Path = args.cache.resolve()
    previous = read_bundled()
    catalog: dict[str, Any] | None = None

    try:
        if args.from_analysis:
            analysis = args.from_analysis.resolve()
            records = normalized_records(analysis, None)
            if not args.offline:
                for record in records:
                    catalog = refresh_language_catalog(cache, record["minecraft_version"])
        elif args.check:
            raise WorkflowError("--check needs --from-analysis or a release selection")
        else:
            if args.offline:
                raise WorkflowError("analyzing a release needs the network; use --from-analysis")
            document = manifest(cache, args.refresh_manifest or bool(args.latest))
            entry = select_release(document, args.version)
            print(f"[update] selected release {entry['id']}", flush=True)
            jar = client_jar(entry, cache)
            analysis = run_analyzer(cache, jar)
            catalog = refresh_language_catalog(cache, entry["id"])
            records = normalized_records(analysis, {entry["id"]})

        merged, changes = dataset.merge(previous, records)
        dataset.validate(merged)
    except (WorkflowError, dataset.DatasetError) as error:
        print(f"[update] {error}", file=sys.stderr)
        return 1
    except (urllib.error.URLError, OSError) as error:
        print(f"[update] could not reach an official source: {error}", file=sys.stderr)
        return 1

    text = dataset.dumps(merged)
    if args.check:
        current = BUNDLED_DATASET.read_text(encoding="utf-8") if BUNDLED_DATASET.exists() else ""
        if current != text:
            print("[update] the committed dataset does not match what these inputs regenerate", file=sys.stderr)
            return 1
        print("[update] committed dataset is exactly what these inputs regenerate")
        return 0

    BUNDLED_DATASET.parent.mkdir(parents=True, exist_ok=True)
    BUNDLED_DATASET.write_text(text, encoding="utf-8")

    feed = cache / "feed" / "minecraft-options.json"
    feed.parent.mkdir(parents=True, exist_ok=True)
    feed.write_text(text, encoding="utf-8")

    for line in summarize(merged, previous, changes, records, catalog, read_bundled_preferences()):
        print(line)
    print(f"bundled dataset: {BUNDLED_DATASET}")
    print(f"remote feed to publish: {feed}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
