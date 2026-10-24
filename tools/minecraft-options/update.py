#!/usr/bin/env python3
"""Analyze a Minecraft release and update Ephervex's options compatibility data.

This is the one maintainer workflow for this subsystem.

    python3 tools/minecraft-options/update.py --latest
    python3 tools/minecraft-options/update.py --version 26.2
    python3 tools/minecraft-options/update.py --from-analysis <dir> --offline
    python3 tools/minecraft-options/update.py --check --version 26.2
    python3 tools/minecraft-options/update.py --check --from-analysis <dir>

It reads Mojang's published version manifest, selects a release (never a
snapshot), fetches and hash-verifies that release's official client JAR into an
untracked development cache, refreshes the release's official language catalog,
analyzes the JAR statically without ever executing Minecraft, folds the result
into the committed dataset, and writes the exact JSON to publish to the remote
options feed.

Exactly one of --latest, --version and --from-analysis selects the input.
--check writes nothing and never uses the network: it reads the cached manifest
and the release's existing analysis in the cache.

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
from pathlib import Path
from typing import Any

import dataset
import fetch_language_catalogs
import official_sources
import preferences
from official_sources import SourceError
from release_identity import is_release

TOOL_DIR = Path(__file__).resolve().parent
REPO_ROOT = TOOL_DIR.parent.parent
BUNDLED_DATASET = REPO_ROOT / "crates" / "options" / "data" / "minecraft-options.json"
BUNDLED_PREFERENCE_CATALOG = REPO_ROOT / "crates" / "options" / "data" / "minecraft-preferences.json"
DEFAULT_CACHE = official_sources.DEFAULT_CACHE

# Matches the launcher's own floor. Below it Ephervex neither installs nor
# launches, so compatibility data for those releases would describe something
# the application cannot produce.
MINIMUM_SUPPORTED_VERSION = "1.12.2"


class WorkflowError(Exception):
    """The workflow cannot continue, with a reason worth printing."""


# ------------------------------------------------------------------- fetching


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
    official_sources.require_version_id(entry["id"])
    return entry


def client_jar(entry: dict[str, Any], cache: Path) -> Path:
    """The official client JAR, verified against Mojang's published SHA-1.

    Cached version metadata is reused only while it still matches the SHA-1 in
    the current manifest entry, so a refreshed manifest also refreshes it.
    """
    version = official_sources.require_version_id(entry.get("id"))
    jars = cache / "jars"
    jars.mkdir(parents=True, exist_ok=True)
    target = jars / f"mc-{version}.jar"

    metadata_label = f"version metadata for {version}"
    metadata_sha1 = official_sources.require_sha1(entry.get("sha1"), metadata_label)
    metadata_path = cache / "versions" / f"{version}.json"
    metadata_path.parent.mkdir(parents=True, exist_ok=True)
    body = None
    if metadata_path.exists():
        cached = metadata_path.read_bytes()
        if hashlib.sha1(cached).hexdigest() == metadata_sha1:  # noqa: S324 - Mojang publishes sha1
            body = cached
    if body is None:
        body = official_sources.fetch_verified(entry.get("url"), metadata_sha1, metadata_label)
        metadata_path.write_bytes(body)
    metadata = official_sources.json_object(body, metadata_label)

    download = (metadata.get("downloads") or {}).get("client")
    if not isinstance(download, dict):
        raise WorkflowError(f"{version} publishes no client download")
    jar_label = f"client jar for {version}"
    jar_sha1 = official_sources.require_sha1(download.get("sha1"), jar_label)
    jar_url = official_sources.require_https_url(download.get("url"), jar_label)
    if target.exists() and hashlib.sha1(target.read_bytes()).hexdigest() == jar_sha1:  # noqa: S324
        return target

    target.write_bytes(official_sources.fetch_verified(jar_url, jar_sha1, jar_label))
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


def refresh_language_catalogs(
    cache: Path, versions: list[str], refresh_manifest: bool = False
) -> dict[str, dict[str, Any]]:
    """Brings each release's official language catalog in the cache up to date.

    Uses the workflow's one version manifest, fetching it again when it does not
    list a requested release. Raises unless every release has a verified catalog.
    """
    document = official_sources.load_manifest(
        cache / official_sources.MANIFEST_NAME,
        online=True,
        refresh=refresh_manifest,
        needs=versions,
    )
    records, failures = fetch_language_catalogs.refresh(cache / "assets-cache", document, versions)
    if failures:
        detail = "; ".join(f"{version}: {reason}" for version, reason in sorted(failures.items()))
        raise WorkflowError(f"the language catalog could not be produced ({detail})")
    return records


def normalized_records(analysis: Path, only: set[str] | None) -> list[dict[str, Any]]:
    """Every analyzed release inside the supported range, newest last.

    A release analyzed from two different JARs is refused: which one is current
    is not something file names can say.
    """
    directory = analysis / "normalized"
    if not directory.is_dir():
        raise WorkflowError(f"no normalized analyzer output under {directory}")
    by_version: dict[str, list[tuple[Path, dict[str, Any]]]] = {}
    for path in sorted(directory.glob("*.json")):
        record = json.loads(path.read_text(encoding="utf-8"))
        version = record.get("minecraft_version")
        if not is_release(version):
            continue
        if dataset.version_key(version) < dataset.version_key(MINIMUM_SUPPORTED_VERSION):
            continue
        if only is not None and version not in only:
            continue
        by_version.setdefault(version, []).append((path, record))
    records = []
    for version, found in by_version.items():
        if any(record != found[0][1] for _, record in found):
            names = ", ".join(path.name for path, _ in found)
            raise WorkflowError(
                f"release {version} has different analyzer records in {directory} ({names}); "
                "run the analyzer again so the output holds only the current JAR's analysis"
            )
        if found[0][1].get("extraction_complete"):
            records.append(found[0][1])
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
    catalogs: dict[str, dict[str, Any]] | None,
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

    if catalogs:
        newest = max(catalogs, key=dataset.version_key)
        scope = newest if len(catalogs) == 1 else f"{len(catalogs)} releases, newest {newest}"
        lines.append(f"language catalog refreshed ({scope}): {catalogs[newest].get('language_count')} codes")

    review = application_review_reasons(records, document)
    changed = previous is not None and previous["revision"] != document["revision"]
    if previous is None:
        lines.append("bundled dataset created")
    elif not changed:
        lines.append("bundled dataset unchanged")
    else:
        lines.append(
            f"bundled dataset revision {previous['revision']} -> {document['revision']}"
        )
    redefined = redefined_settings(previous, document)
    if redefined:
        lines.append(
            f"{len(redefined)} existing setting definition(s) changed: {', '.join(redefined[:8])}"
        )
    if review:
        lines.append("Ephervex application review/update required:")
        lines.extend(f"  {setting_id}: {reason}" for setting_id, reason in review)
    elif changed and not redefined:
        lines.append("no Ephervex application update required for a mapping-only change")
    return lines


def redefined_settings(previous: dict[str, Any] | None, document: dict[str, Any]) -> list[str]:
    """Ids whose registry definition differs from the previous dataset's."""
    if previous is None:
        return []
    before = {
        entry["id"]: entry
        for entry in previous.get("setting_registry", previous.get("settings", []))
    }
    return sorted(
        entry["id"]
        for entry in document["setting_registry"]
        if entry["id"] in before and before[entry["id"]] != entry
    )


def schema_for_extras(record: dict[str, Any], document: dict[str, Any]) -> list[str]:
    """Persisted keys this release has that no canonical definition covers."""
    known = {entry["key"] for entry in document["setting_registry"]}
    return sorted({setting["persisted_key"] for setting in record["settings"]} - known)


def application_review_reasons(
    records: list[dict[str, Any]], document: dict[str, Any]
) -> list[tuple[str, str]]:
    """Required canonical ids the canonical release cannot safely encode.

    Only the canonical release is judged: a setting a historical release does
    not have is not a problem with the dataset, and the codec decision is the
    one the dataset itself makes, so a reason is reported exactly when the
    setting would end up unwritable.
    """
    canonical = next(
        (
            record
            for record in records
            if record["minecraft_version"] == document["canonical_minecraft_version"]
        ),
        None,
    )
    if canonical is None:
        return []
    registry = {entry["key"]: entry["id"] for entry in document["setting_registry"]}
    code_space = canonical.get("keybinding_code_space")
    by_id = {
        registry[setting["persisted_key"]]: setting
        for setting in canonical["settings"]
        if setting["persisted_key"] in registry
    }
    reasons: list[tuple[str, str]] = []
    for setting_id in dataset.REQUIRED_CANONICAL_IDS:
        setting = by_id.get(setting_id)
        if setting is None:
            reasons.append((setting_id, "the analyzer did not recover the setting"))
        elif dataset.codec_for(setting, code_space) is None:
            reasons.append(
                (
                    setting_id,
                    dataset.representation_issue(setting, code_space)
                    or "no codec could be derived from what the analyzer recovered",
                )
            )
    return sorted(set(reasons))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    selection = parser.add_mutually_exclusive_group(required=True)
    selection.add_argument("--latest", action="store_true", help="analyze Mojang's newest release")
    selection.add_argument("--version", help="analyze one specific Minecraft release")
    selection.add_argument(
        "--from-analysis",
        type=Path,
        help="fold an existing analyzer output directory in instead of running the analyzer",
    )
    parser.add_argument("--offline", action="store_true", help="never touch the network")
    parser.add_argument("--cache", type=Path, default=DEFAULT_CACHE, help="untracked development cache")
    parser.add_argument(
        "--check",
        action="store_true",
        help="verify the committed dataset matches what the inputs regenerate, and write nothing; "
        "never uses the network",
    )
    parser.add_argument("--refresh-manifest", action="store_true", help="re-read the version manifest")
    args = parser.parse_args()

    if args.refresh_manifest and (args.check or args.offline):
        parser.error("--refresh-manifest needs the network, which --check and --offline never use")
    if args.offline and not args.check and not args.from_analysis:
        parser.error("analyzing a release needs the network; use --from-analysis")
    online = not (args.check or args.offline)

    cache: Path = args.cache.resolve()
    previous = read_bundled()
    catalogs: dict[str, dict[str, Any]] | None = None

    try:
        if args.from_analysis:
            analysis = args.from_analysis.resolve()
            records = normalized_records(analysis, None)
            if online:
                catalogs = refresh_language_catalogs(
                    cache, [record["minecraft_version"] for record in records], args.refresh_manifest
                )
        else:
            document = official_sources.load_manifest(
                cache / official_sources.MANIFEST_NAME,
                online=online,
                refresh=online and (args.latest or args.refresh_manifest),
                needs=[args.version] if args.version else [],
            )
            entry = select_release(document, args.version)
            if args.check:
                # The analyzer writes into the cache, so a check reads the analysis a
                # previous run left there.
                analysis = cache / "analysis" / f"mc-{entry['id']}"
                if not analysis.is_dir():
                    raise WorkflowError(
                        f"no analysis of {entry['id']} in the cache; run without --check or pass --from-analysis"
                    )
            else:
                print(f"[update] selected release {entry['id']}", flush=True)
                jar = client_jar(entry, cache)
                catalogs = refresh_language_catalogs(cache, [entry["id"]])
                analysis = run_analyzer(cache, jar)
            records = normalized_records(analysis, {entry["id"]})

        merged, changes = dataset.merge(previous, records)
        dataset.validate(merged)
    except (WorkflowError, SourceError, dataset.DatasetError) as error:
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

    for line in summarize(merged, previous, changes, records, catalogs, read_bundled_preferences()):
        print(line)
    print(f"bundled dataset: {BUNDLED_DATASET}")
    print(f"remote feed to publish: {feed}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
