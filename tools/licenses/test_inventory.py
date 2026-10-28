#!/usr/bin/env python3
"""Tests for the third-party license inventory builder.

    python3 -m unittest discover -s tools/licenses

Offline and deterministic: every input is constructed here, so nothing depends
on cargo, on a crate registry, or on the dependency graph of the day.
"""

from __future__ import annotations

import hashlib
import json
import tempfile
import unittest
from pathlib import Path

import inventory


def cargo_about_output(licenses: list[dict], crates: list[dict]) -> dict:
    """The shape `cargo about generate --format json` produces."""
    return {"overview": [], "licenses": licenses, "crates": crates}


def license_entry(identifier: str, name: str, text: str, used_by: list[tuple[str, str]]) -> dict:
    return {
        "id": identifier,
        "name": name,
        "text": text,
        "used_by": [
            {"crate": {"name": crate, "version": version}, "path": None}
            for crate, version in used_by
        ],
    }


def crate_entry(name: str, version: str, expression: str, manifest: str | None = None) -> dict:
    package = {"name": name, "version": version, "license": expression}
    if manifest is not None:
        package["manifest_path"] = manifest
    return {"package": package, "license": expression}


class BuildInventory(unittest.TestCase):
    def test_a_crate_names_the_texts_that_cover_it(self):
        raw = cargo_about_output(
            [license_entry("MIT", "MIT License", "MIT text", [("alpha", "1.0.0")])],
            [crate_entry("alpha", "1.0.0", "MIT")],
        )
        built = inventory.build_inventory(raw, {})
        self.assertEqual(built["version"], inventory.INVENTORY_VERSION)
        self.assertEqual(
            built["crates"],
            [{"name": "alpha", "version": "1.0.0", "license": "MIT", "texts": [0]}],
        )
        self.assertEqual(built["licenses"][0]["text"], "MIT text")

    def test_one_license_with_two_copyright_holders_stays_two_texts(self):
        # Collapsing them by identifier would drop the attribution the license
        # exists to require, which is the whole point of shipping the text.
        raw = cargo_about_output(
            [
                license_entry("MIT", "MIT License", "Copyright Alpha", [("alpha", "1.0.0")]),
                license_entry("MIT", "MIT License", "Copyright Beta", [("beta", "2.0.0")]),
            ],
            [crate_entry("alpha", "1.0.0", "MIT"), crate_entry("beta", "2.0.0", "MIT")],
        )
        built = inventory.build_inventory(raw, {})
        self.assertEqual(len(built["licenses"]), 2)
        self.assertNotEqual(built["crates"][0]["texts"], built["crates"][1]["texts"])

    def test_one_text_shared_by_two_crates_is_stored_once(self):
        raw = cargo_about_output(
            [license_entry("MIT", "MIT License", "shared", [("alpha", "1.0.0"), ("beta", "2.0.0")])],
            [crate_entry("alpha", "1.0.0", "MIT"), crate_entry("beta", "2.0.0", "MIT")],
        )
        built = inventory.build_inventory(raw, {})
        self.assertEqual(len(built["licenses"]), 1)
        self.assertEqual(built["crates"][0]["texts"], [0])
        self.assertEqual(built["crates"][1]["texts"], [0])

    def test_a_crate_with_no_license_text_is_refused(self):
        # Never guessed: a crate whose terms cannot be shown is a crate
        # Ephervex must not ship.
        raw = cargo_about_output([], [crate_entry("alpha", "1.0.0", "MIT")])
        with self.assertRaises(SystemExit):
            inventory.build_inventory(raw, {})

    def test_the_order_does_not_depend_on_the_order_cargo_about_reported(self):
        first = cargo_about_output(
            [
                license_entry("MIT", "MIT License", "m", [("beta", "2.0.0")]),
                license_entry("Apache-2.0", "Apache License 2.0", "a", [("alpha", "1.0.0")]),
            ],
            [crate_entry("beta", "2.0.0", "MIT"), crate_entry("alpha", "1.0.0", "Apache-2.0")],
        )
        second = cargo_about_output(
            list(reversed(first["licenses"])), list(reversed(first["crates"]))
        )
        self.assertEqual(
            inventory.render_inventory(inventory.build_inventory(first, {})),
            inventory.render_inventory(inventory.build_inventory(second, {})),
        )

    def test_the_committed_json_is_compact(self):
        raw = cargo_about_output(
            [license_entry("MIT", "MIT License", "t", [("alpha", "1.0.0")])],
            [crate_entry("alpha", "1.0.0", "MIT")],
        )
        rendered = inventory.render_inventory(inventory.build_inventory(raw, {}))
        self.assertNotIn("\n  ", rendered)
        self.assertNotIn(", ", rendered)
        self.assertTrue(rendered.endswith("\n"))
        json.loads(rendered)


class ClarifiedTexts(unittest.TestCase):
    """A license with no SPDX entry, read from the file about.toml names."""

    def setUp(self):
        self.workspace = tempfile.TemporaryDirectory()
        self.crate_dir = Path(self.workspace.name)
        (self.crate_dir / "LICENSES").mkdir()
        self.text = "Vendor License 2.0\n\nDo as you are told.\n"
        self.license_file = self.crate_dir / "LICENSES" / "Vendor-2.0.md"
        self.license_file.write_text(self.text, encoding="utf-8")
        self.digest = hashlib.sha256(self.text.encode()).hexdigest()
        self.addCleanup(self.workspace.cleanup)

    def config(self, checksum: str | None = None) -> dict:
        return {
            "vendored": {
                "clarify": {
                    "license": "LicenseRef-Vendor-2.0",
                    "files": [
                        {
                            "path": "LICENSES/Vendor-2.0.md",
                            "checksum": checksum or self.digest,
                        }
                    ],
                }
            }
        }

    def test_the_text_comes_from_the_crate_itself(self):
        raw = cargo_about_output(
            [],
            [
                crate_entry(
                    "vendored",
                    "1.0.0",
                    "LicenseRef-Vendor-2.0",
                    manifest=str(self.crate_dir / "Cargo.toml"),
                )
            ],
        )
        built = inventory.build_inventory(raw, self.config())
        self.assertEqual(built["licenses"][0]["id"], "LicenseRef-Vendor-2.0")
        self.assertEqual(built["licenses"][0]["text"], self.text)
        self.assertEqual(built["crates"][0]["texts"], [0])

    def test_a_license_file_that_changed_upstream_is_refused(self):
        # An upgrade that rewrites a license file changes the terms Ephervex
        # ships under, so it stops generation instead of being carried along.
        raw = cargo_about_output(
            [],
            [
                crate_entry(
                    "vendored",
                    "1.0.0",
                    "LicenseRef-Vendor-2.0",
                    manifest=str(self.crate_dir / "Cargo.toml"),
                )
            ],
        )
        with self.assertRaises(SystemExit):
            inventory.build_inventory(raw, self.config(checksum="0" * 64))


class BundledFonts(unittest.TestCase):
    """Fonts are listed beside the crates, each with its own license text."""

    def setUp(self):
        self.workspace = tempfile.TemporaryDirectory()
        self.directory = Path(self.workspace.name)
        (self.directory / "licenses").mkdir()
        self.addCleanup(self.workspace.cleanup)
        self.text = "Copyright 2020 The Example Authors\r\n\r\nOFL terms.\r\n"
        (self.directory / "licenses" / "OFL-Example.txt").write_bytes(self.text.encode())
        (self.directory / "Example-Regular.ttf").write_bytes(b"font")

    def manifest(self, files: str = '["Example-Regular.ttf"]') -> None:
        (self.directory / "fonts.toml").write_text(
            '[[font]]\n'
            'family = "Example"\n'
            'version = "1.000"\n'
            'license = "OFL-1.1"\n'
            'license-name = "SIL Open Font License 1.1"\n'
            'license-file = "licenses/OFL-Example.txt"\n'
            f"files = {files}\n",
            encoding="utf-8",
        )

    def test_a_font_is_listed_with_its_own_text_unaltered(self):
        self.manifest()
        fonts = inventory.bundled_fonts(self.directory)
        built = inventory.build_inventory(
            cargo_about_output(
                [license_entry("MIT", "MIT License", "m", [("alpha", "1.0.0")])],
                [crate_entry("alpha", "1.0.0", "MIT")],
            ),
            {},
            fonts,
        )
        font = next(c for c in built["crates"] if c["name"] == "Example (font)")
        self.assertEqual((font["version"], font["license"]), ("1.000", "OFL-1.1"))
        text = built["licenses"][font["texts"][0]]
        self.assertEqual(text["id"], "OFL-1.1")
        self.assertEqual(text["name"], "SIL Open Font License 1.1")
        # CRLF and the copyright line survive, since that is the attribution.
        self.assertEqual(text["text"], self.text)

    def test_two_fonts_under_one_license_keep_their_own_copyright_lines(self):
        self.manifest()
        other = "Copyright 2021 Someone Else\r\n\r\nOFL terms.\r\n"
        (self.directory / "licenses" / "OFL-Other.txt").write_bytes(other.encode())
        (self.directory / "Other-Regular.ttf").write_bytes(b"font")
        manifest = self.directory / "fonts.toml"
        manifest.write_text(
            manifest.read_text(encoding="utf-8")
            + '\n[[font]]\nfamily = "Other"\nversion = "2.000"\nlicense = "OFL-1.1"\n'
            'license-name = "SIL Open Font License 1.1"\n'
            'license-file = "licenses/OFL-Other.txt"\nfiles = ["Other-Regular.ttf"]\n',
            encoding="utf-8",
        )
        built = inventory.build_inventory(
            cargo_about_output([], []), {}, inventory.bundled_fonts(self.directory)
        )
        self.assertEqual(len(built["licenses"]), 2)
        self.assertEqual([c["name"] for c in built["crates"]], ["Example (font)", "Other (font)"])

    def test_a_font_file_the_manifest_does_not_list_is_refused(self):
        self.manifest()
        (self.directory / "Stray-Regular.ttf").write_bytes(b"font")
        with self.assertRaises(SystemExit):
            inventory.bundled_fonts(self.directory)

    def test_an_unlisted_open_type_font_is_refused(self):
        self.manifest()
        (self.directory / "Stray.otf").write_bytes(b"font")
        with self.assertRaises(SystemExit):
            inventory.bundled_fonts(self.directory)

    def test_an_unlisted_font_in_a_subdirectory_is_refused_whatever_its_case(self):
        self.manifest()
        nested = self.directory / "markup" / "extra"
        nested.mkdir(parents=True)
        (nested / "Stray.TTF").write_bytes(b"font")
        with self.assertRaises(SystemExit):
            inventory.bundled_fonts(self.directory)

    def test_a_font_outside_the_manifest_directory_is_found_from_the_search_root(self):
        self.manifest()
        fonts_directory = self.directory / "fonts"
        fonts_directory.mkdir()
        for name in ("fonts.toml", "Example-Regular.ttf"):
            (self.directory / name).rename(fonts_directory / name)
        (self.directory / "licenses").rename(fonts_directory / "licenses")
        (self.directory / "Stray.ttc").write_bytes(b"font")
        self.assertEqual(len(inventory.bundled_fonts(fonts_directory)), 1)
        with self.assertRaises(SystemExit):
            inventory.bundled_fonts(fonts_directory, self.directory)

    def test_a_license_file_that_is_not_utf8_is_refused(self):
        self.manifest()
        (self.directory / "licenses" / "OFL-Example.txt").write_bytes(b"Copyright \xa9 2020\r\n")
        with self.assertRaises(SystemExit):
            inventory.bundled_fonts(self.directory)

    def test_a_listed_font_file_that_is_missing_is_refused(self):
        self.manifest('["Example-Regular.ttf", "Missing.ttf"]')
        with self.assertRaises(SystemExit):
            inventory.bundled_fonts(self.directory)

    def test_a_font_without_license_text_is_refused(self):
        self.manifest()
        (self.directory / "licenses" / "OFL-Example.txt").write_bytes(b"")
        with self.assertRaises(SystemExit):
            inventory.bundled_fonts(self.directory)

    def test_the_committed_fonts_are_all_accounted_for(self):
        fonts = inventory.bundled_fonts(inventory.FONT_DIRECTORY)
        self.assertEqual(
            sorted(font["name"] for font in fonts),
            ["IBM Plex Mono (font)", "Pixelify Sans (font)", "Space Grotesk (font)"],
        )
        plex = next(font for font in fonts if font["name"].startswith("IBM Plex"))
        self.assertIn('Reserved Font Name "Plex"', plex["text"])


class RenderDocument(unittest.TestCase):
    def test_every_crate_and_every_text_appears(self):
        raw = cargo_about_output(
            [license_entry("MIT", "MIT License", "the MIT terms", [("alpha", "1.0.0")])],
            [crate_entry("alpha", "1.0.0", "MIT")],
        )
        document = inventory.render_document(inventory.build_inventory(raw, {}))
        self.assertIn("| alpha | 1.0.0 | MIT |", document)
        self.assertIn("### MIT License (MIT)", document)
        self.assertIn("Applies to: alpha 1.0.0.", document)
        self.assertIn("the MIT terms", document)

    def test_a_license_with_no_spdx_name_is_not_named_twice(self):
        raw = cargo_about_output(
            [
                license_entry(
                    "LicenseRef-Vendor-2.0",
                    "LicenseRef-Vendor-2.0",
                    "vendor terms",
                    [("alpha", "1.0.0")],
                )
            ],
            [crate_entry("alpha", "1.0.0", "LicenseRef-Vendor-2.0")],
        )
        document = inventory.render_document(inventory.build_inventory(raw, {}))
        self.assertIn("### LicenseRef-Vendor-2.0\n", document)
        self.assertNotIn("(LicenseRef-Vendor-2.0)", document)


class StalenessReport(unittest.TestCase):
    """What the staleness check says when the committed files do not match.

    A byte comparison of two documents this size is useless to whoever has to
    act on it, so the report has to name what actually moved.
    """

    @staticmethod
    def rendered(crates: list[tuple[str, str]], text: str = "terms") -> bytes:
        raw = cargo_about_output(
            [license_entry("MIT", "MIT License", text, crates)],
            [crate_entry(name, version, "MIT") for name, version in crates],
        )
        return inventory.render_inventory(inventory.build_inventory(raw, {})).encode("utf-8")

    def test_a_crate_that_left_the_graph_is_named(self):
        committed = self.rendered([("alpha", "1.0.0"), ("beta", "2.0.0")])
        generated = self.rendered([("alpha", "1.0.0")])
        report = "\n".join(inventory.describe_difference(committed, generated))
        self.assertIn("crates only in the committed file: beta 2.0.0", report)
        self.assertIn("crates: 2 committed, 1 generated", report)

    def test_a_crate_that_joined_the_graph_is_named(self):
        committed = self.rendered([("alpha", "1.0.0")])
        generated = self.rendered([("alpha", "1.0.0"), ("beta", "2.0.0")])
        report = "\n".join(inventory.describe_difference(committed, generated))
        self.assertIn("crates only in the generated one: beta 2.0.0", report)

    def test_the_same_crates_under_a_changed_text_says_so(self):
        crates = [("alpha", "1.0.0")]
        report = "\n".join(
            inventory.describe_difference(
                self.rendered(crates, "old terms"), self.rendered(crates, "new terms")
            )
        )
        self.assertIn("alpha 1.0.0:", report)
        self.assertIn("first difference at byte", report)

    def test_a_missing_file_is_reported_as_missing(self):
        self.assertEqual(
            inventory.describe_difference(None, self.rendered([("alpha", "1.0.0")])),
            ["the file is missing"],
        )

    def test_the_crate_whose_text_changed_is_named(self):
        """The whole point: a mismatch has to say which dependency moved."""
        def rendered(beta_text: str) -> bytes:
            raw = cargo_about_output(
                [
                    license_entry("MIT", "MIT License", "alpha terms", [("alpha", "1.0.0")]),
                    license_entry("MIT", "MIT License", beta_text, [("beta", "2.0.0")]),
                ],
                [
                    crate_entry("alpha", "1.0.0", "MIT"),
                    crate_entry("beta", "2.0.0", "MIT"),
                ],
            )
            return inventory.render_inventory(inventory.build_inventory(raw, {})).encode("utf-8")

        report = "\n".join(
            inventory.describe_difference(rendered("beta terms"), rendered("beta terms, revised"))
        )
        self.assertIn("beta 2.0.0:", report)
        self.assertNotIn("alpha 1.0.0:", report)

    def test_two_crates_sharing_a_text_that_splits_is_reported(self):
        """One shared text becoming two is the shape a per-file difference takes."""
        shared = cargo_about_output(
            [license_entry("MIT", "MIT License", "same terms", [("alpha", "1.0.0"), ("beta", "2.0.0")])],
            [crate_entry("alpha", "1.0.0", "MIT"), crate_entry("beta", "2.0.0", "MIT")],
        )
        split = cargo_about_output(
            [
                license_entry("MIT", "MIT License", "same terms", [("alpha", "1.0.0")]),
                license_entry("MIT", "MIT License", "same terms\r\n", [("beta", "2.0.0")]),
            ],
            [crate_entry("alpha", "1.0.0", "MIT"), crate_entry("beta", "2.0.0", "MIT")],
        )
        report = "\n".join(
            inventory.describe_difference(
                inventory.render_inventory(inventory.build_inventory(shared, {})).encode("utf-8"),
                inventory.render_inventory(inventory.build_inventory(split, {})).encode("utf-8"),
            )
        )
        self.assertIn("licenses: 1 committed, 2 generated", report)
        self.assertIn("beta 2.0.0:", report)

    def test_a_long_list_of_crates_is_cut_short(self):
        committed = self.rendered([(f"crate{index:02d}", "1.0.0") for index in range(12)])
        generated = self.rendered([("crate00", "1.0.0")])
        report = "\n".join(inventory.describe_difference(committed, generated))
        self.assertIn("and 3 more", report)


if __name__ == "__main__":
    unittest.main()
