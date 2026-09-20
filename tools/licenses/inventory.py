#!/usr/bin/env python3
"""Turn cargo-about's output into Ephervex's third-party license inventory.

Two documents, one builder, so the machine-readable one and the human-readable
one can never disagree about what Ephervex ships:

* `crates/core/data/third-party-licenses.json`, compiled into the binaries and
  read by `App::third_party_licenses`, so the desktop application and the
  command line answer from the same data;
* `THIRD-PARTY-LICENSES.md`, which every release artifact carries, because
  Apache-2.0 attribution is satisfied by shipping the text rather than by a
  screen somebody might open.

Nothing here decides what a dependency is licensed under. `cargo-about` reads
the real graph from `Cargo.lock` and finds each crate's own license file;
`about.toml` says which licenses are acceptable and resolves the one dependency
offered under several. This turns what came back into the shape Ephervex reads,
and refuses anything it cannot account for.

**Why a builder rather than a handlebars template.** cargo-about renders its
output through templates, and the obvious thing is one emitting JSON directly.
License texts contain quotes, backslashes, and control characters, and
handlebars escapes for HTML, so that template produces invalid JSON for the
exact inputs it exists to carry. Encoding with a JSON encoder is not a smaller
tool, it is the correct one.

**Reproducible by construction.** cargo-about's raw output carries absolute
paths out of the build machine's cargo registry and lists crates in whatever
order the graph walk reached them. Neither survives: paths are dropped, and
every list here is sorted by its own content, so the same lockfile and the same
crate sources produce the same bytes on any machine.

**Licenses that are not SPDX ids.** cargo-about resolves license text from its
embedded copy of the SPDX list, so a crate offered under a vendor license -
`LicenseRef-Slint-Royalty-free-2.0`, in this tree - gets its expression
resolved and its text left out, because there is no SPDX entry to take one
from. The text still exists: the clarification in `about.toml` names the file
in the crate that carries it, with the SHA-256 it has to match. So a crate
cargo-about left without a text is read from the file its own clarification
names, and the digest is checked here as well. That is a general rule rather
than a case for one crate, and it never invents anything: the terms still come
from the crate, verified against a digest recorded in a reviewed file.

    usage: tools/licenses/inventory.py --from-cargo-about <file>
               --about-config <about.toml> [--check]

Run it through tools/licenses/generate.sh, which produces that file first.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
import tomllib
from pathlib import Path

# The inventory format Ephervex reads. Bumping it is a change to
# `crates/core/src/licenses.rs` as well; see docs/internal-format-versions.md.
INVENTORY_VERSION = 1

# How much of a mismatch the staleness check spells out before it stops listing.
MAX_REPORTED_CRATES = 8
EXCERPT_CONTEXT = 60

REPO_ROOT = Path(__file__).resolve().parents[2]
INVENTORY_PATH = REPO_ROOT / "crates" / "core" / "data" / "third-party-licenses.json"
DOCUMENT_PATH = REPO_ROOT / "THIRD-PARTY-LICENSES.md"

DOCUMENT_PREAMBLE = """\
# Third-party licenses

Ephervex is licensed under Apache-2.0; its own terms are in `LICENSE`. It is
built on the open-source crates listed here, each under its own terms, and this
document carries the full text of every one of them.

Generated from `Cargo.lock` by `tools/licenses/generate.sh`. It is not written
or edited by hand: a dependency change that is not regenerated fails the build.
The same data is compiled into the binaries, so `ephervex licenses` and the
desktop application's "About & Licenses" screen answer from it too.
"""


def fail(message: str) -> None:
    """Stop with a message on standard error."""
    print(f"inventory: {message}", file=sys.stderr)
    sys.exit(1)


def clarified_texts(name: str, crate_dir: Path, config: dict) -> list[tuple[str, str]]:
    """The license texts `about.toml` clarifies for one crate, as (id, text).

    Each file the clarification names is read from the crate itself and checked
    against the digest recorded beside it, so this reports the crate's own terms
    or nothing. A file entry may name its own license; otherwise the
    clarification's expression covers it.
    """
    clarify = config.get(name, {}).get("clarify")
    if not clarify:
        return []

    expression = clarify.get("license")
    found = []
    for entry in clarify.get("files", []):
        path, checksum = entry.get("path"), entry.get("checksum")
        if not path or not checksum:
            fail(f"the clarification for {name} has a file with no path or no checksum")
        source = crate_dir / path
        try:
            content = source.read_bytes()
        except OSError as error:
            fail(f"the clarification for {name} names {path}, which could not be read: {error}")
        digest = hashlib.sha256(content).hexdigest()
        if digest != checksum:
            fail(
                f"{name} ships a different {path} than about.toml records.\n"
                f"  recorded {checksum}\n  found    {digest}\n"
                f"An upgrade that changes a license file changes the terms Ephervex "
                f"ships under. Read the new file, then update about.toml deliberately."
            )
        identifier = entry.get("license") or expression
        if not identifier:
            fail(f"the clarification for {name} names {path} under no license")
        found.append((identifier, content.decode("utf-8")))
    return found


def build_inventory(raw: dict, config: dict) -> dict:
    """The committed inventory, from what cargo-about reported.

    `raw["licenses"]` is one entry per (license, text) pair with the crates
    using it; `raw["crates"]` carries each crate's declared license expression.
    The inventory inverts the first so a crate names its texts, because that is
    the direction both frontends read it in.
    """
    for key in ("licenses", "crates"):
        if key not in raw:
            fail(f"cargo-about produced no {key!r}; nothing can be generated from it")

    # Deduplicated by text, not by identifier: forty crates under MIT ship
    # forty MIT files that differ in their copyright line, and collapsing them
    # to one would drop the attribution the license exists to require.
    texts: dict[tuple[str, str], dict] = {}
    for entry in raw["licenses"]:
        identifier = entry.get("id")
        text = entry.get("text")
        if not identifier or not text:
            fail(f"a license entry has no id or no text: {entry.get('name')!r}")
        texts.setdefault((identifier, text), {
            "id": identifier,
            "name": entry.get("name") or identifier,
            "text": text,
        })

    # Which crates each text covers, from the same entries read the other way
    # round. Collected by (id, text) rather than by index, because the texts
    # below are not numbered until every one of them is known.
    used: dict[tuple[str, str], set[tuple[str, str]]] = {}
    for entry in raw["licenses"]:
        key = (entry["id"], entry["text"])
        for user in entry.get("used_by", []):
            package = user.get("crate", {})
            name, version = package.get("name"), package.get("version")
            if not name or not version:
                fail("a license is used by a crate with no name or version")
            used.setdefault((name, version), set()).add(key)

    # Crates cargo-about could not resolve a text for, read from the file their
    # own clarification names. This is where a vendor license arrives.
    for entry in raw["crates"]:
        package = entry.get("package", {})
        name, version = package.get("name"), package.get("version")
        if (name, version) in used:
            continue
        manifest = package.get("manifest_path")
        if not manifest:
            continue
        for identifier, text in clarified_texts(name, Path(manifest).parent, config):
            texts.setdefault((identifier, text), {
                "id": identifier,
                # No SPDX entry, so no name other than the identifier itself.
                "name": identifier,
                "text": text,
            })
            used.setdefault((name, version), set()).add((identifier, text))

    licenses = sorted(
        texts.values(),
        key=lambda item: (item["id"], hashlib.sha256(item["text"].encode()).hexdigest()),
    )
    index_of = {
        (item["id"], item["text"]): position for position, item in enumerate(licenses)
    }

    crates = []
    for entry in raw["crates"]:
        package = entry.get("package", {})
        name, version = package.get("name"), package.get("version")
        if not name or not version:
            fail("cargo-about reported a crate with no name or version")
        expression = entry.get("license") or package.get("license")
        if not expression:
            fail(f"{name} {version} reports no license expression")
        keys = used.get((name, version))
        if not keys:
            # Never guessed. A crate whose text was not found is a crate
            # Ephervex would be shipping under terms it cannot show, which is
            # the one thing this tooling exists to prevent.
            fail(
                f"{name} {version} declares {expression!r} but no license text was "
                f"found for it. Add a clarification to about.toml naming the file "
                f"that carries its terms."
            )
        crates.append({
            "name": name,
            "version": version,
            "license": expression,
            "texts": sorted(index_of[key] for key in keys),
        })

    crates.sort(key=lambda item: (item["name"], item["version"]))
    return {"version": INVENTORY_VERSION, "licenses": licenses, "crates": crates}


def render_inventory(inventory: dict) -> str:
    """The inventory as it is committed: compact, sorted, one trailing newline.

    Compact because it is machine-owned storage compiled into every binary, and
    sorted so a regeneration that changed nothing produces no diff.
    """
    return json.dumps(
        inventory, separators=(",", ":"), sort_keys=True, ensure_ascii=False
    ) + "\n"


def render_document(inventory: dict) -> str:
    """The inventory as a reader meets it in a release artifact."""
    licenses = inventory["licenses"]
    crates = inventory["crates"]

    lines = [DOCUMENT_PREAMBLE, ""]
    lines.append(f"{len(crates)} crates, under {len(licenses)} license texts.")
    lines.append("")
    lines.append("## Dependencies")
    lines.append("")
    lines.append("| Crate | Version | License |")
    lines.append("| --- | --- | --- |")
    for crate in crates:
        lines.append(f"| {crate['name']} | {crate['version']} | {crate['license']} |")
    lines.append("")
    lines.append("## License texts")
    lines.append("")

    # Which crates each text covers, so a reader can go from a text back to
    # what it applies to without re-reading the table above.
    users: dict[int, list[str]] = {}
    for crate in crates:
        for position in crate["texts"]:
            users.setdefault(position, []).append(f"{crate['name']} {crate['version']}")

    for position, license_text in enumerate(licenses):
        heading = license_text["name"]
        if license_text["id"] != heading:
            heading = f"{heading} ({license_text['id']})"
        lines.append(f"### {heading}")
        lines.append("")
        lines.append("Applies to: " + ", ".join(users.get(position, [])) + ".")
        lines.append("")
        lines.append("```text")
        # A fence long enough that a license text containing a fence of its own
        # cannot end this one early.
        lines.extend(license_text["text"].rstrip("\n").split("\n"))
        lines.append("```")
        lines.append("")

    return "\n".join(lines).rstrip("\n") + "\n"


def structural_difference(committed: bytes, generated: bytes) -> list[str]:
    """What changed between two inventory documents, in terms a reader can act on."""
    try:
        was, now = json.loads(committed), json.loads(generated)
    except json.JSONDecodeError:
        return []

    reported = []
    for field in ("crates", "licenses"):
        before, after = len(was.get(field, [])), len(now.get(field, []))
        if before != after:
            reported.append(f"{field}: {before} committed, {after} generated")

    def named(entries: list) -> set[str]:
        return {f"{entry['name']} {entry['version']}" for entry in entries}

    committed_crates, generated_crates = named(was.get("crates", [])), named(now.get("crates", []))
    for label, names in (
        ("only in the committed file", committed_crates - generated_crates),
        ("only in the generated one", generated_crates - committed_crates),
    ):
        if names:
            shown = ", ".join(sorted(names)[:MAX_REPORTED_CRATES])
            rest = len(names) - MAX_REPORTED_CRATES
            reported.append(f"crates {label}: {shown}" + (f", and {rest} more" if rest > 0 else ""))

    for line in crates_whose_texts_moved(was, now):
        reported.append(line)

    if not reported:
        reported.append("the same crates under the same texts, so only their order differs")
    return reported


def crate_texts(document: dict) -> dict[str, set[tuple[str, str]]]:
    """Each crate's licenses as (identifier, digest), so an index shift is not a difference."""
    texts = [
        (entry["id"], hashlib.sha256(entry["text"].encode()).hexdigest()[:12])
        for entry in document.get("licenses", [])
    ]
    return {
        f"{crate['name']} {crate['version']}": {
            texts[index] for index in crate.get("texts", []) if index < len(texts)
        }
        for crate in document.get("crates", [])
    }


def crates_whose_texts_moved(was: dict, now: dict) -> list[str]:
    """The crates whose own license texts changed, which is what a mismatch is really about."""
    committed, generated = crate_texts(was), crate_texts(now)
    moved = sorted(
        name
        for name in committed.keys() & generated.keys()
        if committed[name] != generated[name]
    )
    if not moved:
        return []

    reported = []
    for name in moved[:MAX_REPORTED_CRATES]:
        gone = ", ".join(f"{i}:{d}" for i, d in sorted(committed[name] - generated[name]))
        came = ", ".join(f"{i}:{d}" for i, d in sorted(generated[name] - committed[name]))
        reported.append(f"{name}: committed {gone or 'nothing'}, generated {came or 'nothing'}")
    rest = len(moved) - MAX_REPORTED_CRATES
    if rest > 0:
        reported.append(f"and {rest} more crates whose texts differ")
    return reported


def byte_difference(committed: bytes, generated: bytes) -> list[str]:
    """Where two documents first diverge, with enough either side to recognize it."""
    shared = min(len(committed), len(generated))
    offset = next((at for at in range(shared) if committed[at] != generated[at]), shared)

    def excerpt(data: bytes) -> str:
        window = data[max(0, offset - EXCERPT_CONTEXT):offset + EXCERPT_CONTEXT]
        return window.decode("utf-8", "replace").replace("\r", "\\r").replace("\n", "\\n")

    return [
        f"{len(committed)} bytes committed, {len(generated)} generated",
        f"first difference at byte {offset}",
        f"  committed {excerpt(committed)}",
        f"  generated {excerpt(generated)}",
    ]


def describe_difference(committed: bytes | None, generated: bytes) -> list[str]:
    """Why one generated document does not match the one beside it."""
    if committed is None:
        return ["the file is missing"]
    return structural_difference(committed, generated) + byte_difference(committed, generated)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--from-cargo-about",
        required=True,
        type=Path,
        help="the JSON cargo-about produced with --format json",
    )
    parser.add_argument(
        "--about-config",
        required=True,
        type=Path,
        help="the about.toml whose clarifications name non-SPDX license files",
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="report whether the committed files match, and write nothing",
    )
    args = parser.parse_args()

    try:
        raw = json.loads(args.from_cargo_about.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        fail(f"could not read {args.from_cargo_about}: {error}")

    try:
        config = tomllib.loads(args.about_config.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as error:
        fail(f"could not read {args.about_config}: {error}")

    inventory = build_inventory(raw, config)
    # Bytes rather than text from here on. At least one dependency's license
    # file uses CRLF, and its text ships exactly as that crate wrote it; reading
    # a generated file back in text mode would translate those line endings and
    # make an unchanged document look stale forever.
    generated = {
        INVENTORY_PATH: render_inventory(inventory).encode("utf-8"),
        DOCUMENT_PATH: render_document(inventory).encode("utf-8"),
    }

    if args.check:
        stale = []
        for path, content in generated.items():
            current = path.read_bytes() if path.exists() else None
            if current != content:
                stale.append((path, current, content))
        if stale:
            for path, current, content in stale:
                print(
                    f"inventory: {path.relative_to(REPO_ROOT)} is not what the current "
                    f"dependency graph generates",
                    file=sys.stderr,
                )
                # A byte comparison of two documents this size says nothing a
                # reader can act on unless it also says what differs.
                for line in describe_difference(current, content):
                    print(f"  {line}", file=sys.stderr)
            print(
                "Run tools/licenses/generate.sh and commit the result.", file=sys.stderr
            )
            sys.exit(1)
        print(
            f"inventory: up to date, {len(inventory['crates'])} crates under "
            f"{len(inventory['licenses'])} license texts"
        )
        return

    for path, content in generated.items():
        path.write_bytes(content)
        print(f"inventory: wrote {path.relative_to(REPO_ROOT)}")
    print(
        f"inventory: {len(inventory['crates'])} crates under "
        f"{len(inventory['licenses'])} license texts"
    )


if __name__ == "__main__":
    main()
