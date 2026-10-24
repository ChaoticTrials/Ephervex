#!/usr/bin/env python3
"""Tests for the bundled Minecraft product preference catalog builder.

    python3 -m unittest discover -s tools/minecraft-options

Offline and deterministic. `SmallCatalog` constructs its own compatibility
document from `test_dataset.setting`/`release` rather than depending on any
real analyzer output, so these tests do not depend on Mojang, on a client JAR,
or on `javap`. `RealCanonicalShape` reads the checked-in compatibility dataset
that ships with the application, which is committed data rather than a live
fetch.
"""

from __future__ import annotations

import contextlib
import copy
import io
import json
import shutil
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import dataset
import file_io
import preferences
from .test_dataset import REQUIRED, release, setting

REPO_ROOT = Path(__file__).resolve().parent.parent.parent.parent
BUNDLED_DATASET = REPO_ROOT / "crates" / "options" / "data" / "minecraft-options.json"
BUNDLED_CATALOG = REPO_ROOT / "crates" / "options" / "data" / "minecraft-preferences.json"


def overriding(base: dict, **overrides) -> dict:
    merged = dict(base)
    merged.update(overrides)
    return merged


NUMERIC_FIELD_NAMES = {"min": "minimum", "max": "maximum", "step": "step"}


def with_codec(document: dict, canonical_id: str, codec: dict) -> dict:
    """A copy of `document` whose canonical release stores one setting through
    `codec`. The representation table is shared between settings, so the
    setting is repointed at a new entry instead of editing a shared one."""
    tampered = copy.deepcopy(document)
    schema = preferences._canonical_schema(tampered)
    entry = copy.deepcopy(tampered["representations"][schema["supported"][canonical_id]])
    entry["codec"] = codec
    tampered["representations"].append(entry)
    schema["supported"][canonical_id] = len(tampered["representations"]) - 1
    return tampered


EXTRA_SETTINGS = [
    overriding(
        setting(
            "renderClouds",
            "cloudStatus",
            kind="enum",
            domain_confidence="high",
            allowed={
                "persisted_form": "string_token",
                "values": [{"persisted": "false"}, {"persisted": "fast"}, {"persisted": "true"}],
            },
        ),
        category="Video",
    ),
    overriding(setting("key_key.attack", "keybind.attack", kind="key_binding"), category="Controls"),
    overriding(
        setting(
            "soundCategory_master", "sound.volume.master", kind="number", numeric={"min": 0.0, "max": 1.0},
            domain_confidence="high",
        ),
        category="Sound & Music",
    ),
    overriding(setting("modelPart_cape", "modelPart.cape", kind="boolean"), category="Gameplay"),
    overriding(
        setting("incompatibleResourcePacks", kind="special"), category="Gameplay", ephervex_manageable=False
    ),
    overriding(setting("resourcePacks", kind="special"), category="Gameplay", ephervex_manageable=False),
    overriding(setting("joinedFirstServer"), category="Gameplay", ephervex_manageable=False),
    overriding(setting("lastServer", "lastMpIp", kind="string"), category="Gameplay", ephervex_manageable=False),
    overriding(setting("soundDevice", kind="string"), category="Sound & Music", ephervex_manageable=False),
    overriding(setting("startedCleanly"), category="Gameplay", ephervex_manageable=False),
    overriding(
        setting(
            "tutorialStep",
            kind="enum",
            domain_confidence="high",
            allowed={"persisted_form": "string_token", "values": [{"persisted": "none"}]},
        ),
        category="Gameplay",
        ephervex_manageable=False,
    ),
]


FIXTURE_REQUIRED = list(REQUIRED)
FIXTURE_REQUIRED[6] = setting(
    "renderDistance", kind="integer", numeric={"min": 2, "max": 32}, domain_confidence="high"
)
FIXTURE_SETTINGS = FIXTURE_REQUIRED + EXTRA_SETTINGS


def small_dataset_document() -> dict:
    document, _ = dataset.merge(None, [release("26.2", FIXTURE_SETTINGS)])
    dataset.validate(document)
    return document


RICH_ENRICHMENT = {
    "cloudStatus": {
        "default": "true",
        "default_confidence": "high",
        "allowed_values": {
            "values": [
                {"persisted": "false", "enum_constant": "OFF", "translation_key": "options.off", "ordinal": 0},
                {"persisted": "fast", "enum_constant": "FAST", "translation_key": "options.clouds.fast", "ordinal": 1},
                {"persisted": "true", "enum_constant": "FANCY", "translation_key": "options.clouds.fancy", "ordinal": 2},
                # Not in the proven codec (which only knows false/fast/true):
                # corroboration must drop this rather than label it.
                {"persisted": "extra", "enum_constant": "BOGUS", "translation_key": "options.bogus", "ordinal": 3},
            ]
        },
    },
    "keybind.attack": {
        "binding_category": "gameplay",
        "default_confidence": "high",
        "default": {"serialized": "key.mouse.left", "unbound": False},
    },
    "sound.volume.master": {
        "default": 1.0,
        "default_confidence": "high",
        "numeric": {"min": 0.0, "max": 1.0, "step": None},
    },
    "fov": {
        "default": 70,
        "default_confidence": "high",
        # Disagrees with the proven codec domain (2..32 below): the step must
        # not be carried when the analyzer's own domain is not corroborated.
        "numeric": {"min": 10, "max": 200, "step": 5},
    },
    "renderDistance": {
        "default": 12,
        "default_confidence": "high",
        "numeric": {"min": 2, "max": 32, "step": 1},
    },
}


class Determinism(unittest.TestCase):
    def test_identical_inputs_produce_byte_identical_output(self):
        document = small_dataset_document()
        first = preferences.build(copy.deepcopy(document), copy.deepcopy(RICH_ENRICHMENT))
        second = preferences.build(copy.deepcopy(document), copy.deepcopy(RICH_ENRICHMENT))
        self.assertEqual(preferences.dumps(first), preferences.dumps(second))

    def test_compatibility_only_changes_do_not_grow_the_catalog(self):
        # Backfilling an older release does not change which release is
        # canonical, exactly the shape of an ordinary compatibility-only
        # update: nothing about the current, exposed product surface moves.
        base = small_dataset_document()
        with_backfilled_release, _ = dataset.merge(base, [release("1.20.1", FIXTURE_SETTINGS)])
        self.assertEqual(len(with_backfilled_release["releases"]), len(base["releases"]) + 1)
        self.assertEqual(with_backfilled_release["canonical_minecraft_version"], base["canonical_minecraft_version"])
        self.assertEqual(
            preferences.dumps(preferences.build(base)),
            preferences.dumps(preferences.build(with_backfilled_release)),
        )


    def test_a_setting_only_older_releases_have_is_never_exposed(self):
        # The registry keeps every id any analyzed release ever had; only the
        # canonical release's current set may reach a settings screen.
        base = small_dataset_document()
        with_legacy, _ = dataset.merge(base, [release("1.20.1", FIXTURE_SETTINGS + [setting("legacyOnly")])])
        self.assertIn("legacyOnly", {entry["id"] for entry in with_legacy["setting_registry"]})
        self.assertNotIn("legacyOnly", with_legacy["current_canonical_setting_ids"])
        catalog = preferences.build(with_legacy)
        preferences.validate(catalog, with_legacy)
        self.assertNotIn("legacy_only", {d["id"] for d in catalog["descriptors"]})
        self.assertEqual(preferences.dumps(catalog), preferences.dumps(preferences.build(base)))


class Accounting(unittest.TestCase):
    def test_every_current_setting_is_exposed_or_excluded_exactly_once(self):
        document = small_dataset_document()
        catalog = preferences.build(document, RICH_ENRICHMENT)
        preferences.validate(catalog, document)

        exposed = {field["canonical_id"] for descriptor in catalog["descriptors"] for field in descriptor["fields"]}
        excluded = {exclusion["canonical_id"] for exclusion in catalog["exclusions"]}
        current = set(document["current_canonical_setting_ids"])

        self.assertEqual(exposed | excluded, current)
        self.assertEqual(exposed & excluded, set())
        self.assertEqual(len(excluded), 7)
        registry = {entry["id"]: entry for entry in document["setting_registry"]}
        self.assertTrue(all(registry[cid]["manageable"] for cid in exposed))
        self.assertTrue(all(not registry[cid]["manageable"] for cid in excluded))

    def test_window_size_is_one_compound_descriptor_over_two_fields(self):
        document = small_dataset_document()
        catalog = preferences.build(document)
        window = next(d for d in catalog["descriptors"] if d["id"] == "window_size")
        self.assertEqual(window["presentation"], {"kind": "window_size"})
        self.assertEqual(
            {(f["role"], f["canonical_id"]) for f in window["fields"]},
            {("width", "overrideWidth"), ("height", "overrideHeight")},
        )

    def test_a_setting_no_longer_current_needs_no_exclusion(self):
        without_tutorial = [s for s in REQUIRED + EXTRA_SETTINGS if s["persisted_key"] != "tutorialStep"]
        document, _ = dataset.merge(None, [release("26.2", without_tutorial)])
        catalog = preferences.build(document)
        preferences.validate(catalog, document)
        self.assertNotIn("tutorialStep", {e["canonical_id"] for e in catalog["exclusions"]})


class CanonicalCodecAccounting(unittest.TestCase):
    def _catalog_with(self, extra: dict) -> tuple[dict, dict]:
        document, _ = dataset.merge(None, [release("26.2", REQUIRED + [extra])])
        catalog = preferences.build(document)
        preferences.validate(catalog, document)
        return document, catalog

    def test_a_current_manageable_setting_without_a_canonical_codec_is_excluded_with_a_reason(self):
        unproven = overriding(
            setting("key_key.test", "keybind.test", kind="key_binding", confidence="medium"), category="Controls"
        )
        document, catalog = self._catalog_with(unproven)

        self.assertIsNone(dataset.codec_for(unproven, "glfw"))
        self.assertNotIn("keybind_test", {d["id"] for d in catalog["descriptors"]})
        self.assertIn(
            {"canonical_id": "keybind.test", "reason": preferences.UNPROVEN_CODEC_REASON}, catalog["exclusions"]
        )

    def test_the_same_setting_with_a_proven_codec_is_exposed(self):
        proven = overriding(setting("key_key.test", "keybind.test", kind="key_binding"), category="Controls")
        _, catalog = self._catalog_with(proven)

        self.assertIn("keybind_test", {d["id"] for d in catalog["descriptors"]})
        self.assertNotIn("keybind.test", {e["canonical_id"] for e in catalog["exclusions"]})

    def test_a_key_binding_whose_default_is_unknown_still_has_a_codec(self):
        unknown_default = overriding(
            setting("key_key.test", "keybind.test", kind="key_binding", default_confidence="medium"),
            category="Controls",
        )
        _, catalog = self._catalog_with(unknown_default)

        descriptor = next(d for d in catalog["descriptors"] if d["id"] == "keybind_test")
        self.assertIsNone(descriptor["presentation"]["default"])


class Presentation(unittest.TestCase):
    def setUp(self):
        self.document = small_dataset_document()
        self.catalog = preferences.build(self.document, RICH_ENRICHMENT)
        preferences.validate(self.catalog, self.document)
        self.by_id = {d["id"]: d for d in self.catalog["descriptors"]}

    def test_enum_values_are_labeled_and_uncorroborated_entries_are_dropped(self):
        descriptor = self.by_id["cloud_status"]
        values = {v["technical_value"]: v for v in descriptor["presentation"]["values"]}
        self.assertEqual(set(values), {"false", "fast", "true"})
        self.assertEqual(values["true"]["enum_constant"], "FANCY")
        self.assertEqual(values["true"]["translation_key"], "options.clouds.fancy")
        self.assertEqual(descriptor["presentation"]["default"], "true")

    def test_a_keybinding_carries_its_group_and_proven_default(self):
        descriptor = self.by_id["keybind_attack"]
        self.assertEqual(descriptor["section"], "gameplay")
        self.assertEqual(
            descriptor["presentation"]["default"], {"serialized": "key.mouse.left", "unbound": False}
        )

    def test_debug_keybindings_are_tiered_advanced(self):
        document, _ = dataset.merge(
            None,
            [
                release(
                    "26.2",
                    REQUIRED
                    + [overriding(setting("key_key.debug", "keybind.debug", kind="key_binding"), category="Controls")],
                )
            ],
        )
        catalog = preferences.build(document, {"keybind.debug": {"binding_category": "debug"}})
        descriptor = next(d for d in catalog["descriptors"] if d["id"] == "keybind_debug")
        self.assertEqual(descriptor["tier"], "advanced")
        self.assertEqual(descriptor["section"], "debug")

    def test_a_numeric_step_is_kept_only_when_the_proven_domain_agrees(self):
        # fov's enrichment claims a 10..200 domain the codec never proved
        # (REQUIRED leaves fov's own domain unproven); render_distance's
        # enrichment matches its proven codec domain exactly.
        fov = self.by_id["fov"]
        self.assertIsNone(fov["presentation"]["step"])
        self.assertEqual(fov["presentation"]["default"], 70)

        render_distance = self.by_id["render_distance"]
        self.assertEqual(render_distance["presentation"]["min"], 2)
        self.assertEqual(render_distance["presentation"]["max"], 32)
        self.assertEqual(render_distance["presentation"]["step"], 1)

    def test_sound_volume_settings_are_grouped_by_section(self):
        descriptor = self.by_id["sound_volume_master"]
        self.assertEqual(descriptor["category"], "sound_and_music")
        self.assertEqual(descriptor["section"], "volume")
        self.assertEqual(descriptor["presentation"], {"kind": "decimal", "default": 1.0, "min": 0.0, "max": 1.0, "step": None})

    def test_a_default_without_high_confidence_is_not_carried(self):
        # guiScale has enrichment here, so only the confidence gate decides.
        for confidence, expected in (("medium", None), ("low", None), ("unknown", None), ("high", 2)):
            enrichment = dict(RICH_ENRICHMENT, guiScale={"default": 2, "default_confidence": confidence})
            catalog = preferences.build(self.document, enrichment)
            gui_scale = next(d for d in catalog["descriptors"] if d["id"] == "gui_scale")
            self.assertEqual(gui_scale["presentation"]["default"], expected, confidence)


class NumericStep(unittest.TestCase):
    """A step is presented only when stepping from the minimum reaches both the
    maximum and the default, so a control never offers values the game would
    not itself persist."""

    def _presentation(self, step, *, default=1.0, minimum=0.5, maximum=5.0):
        codec = {"kind": "decimal", "min": minimum, "max": maximum}
        rich = {
            "default": default,
            "default_confidence": "high",
            "numeric": {"min": minimum, "max": maximum, "step": step},
        }
        return preferences._numeric_presentation("decimal", codec, rich)

    def test_a_step_that_reaches_the_range_ends_and_the_default_is_kept(self):
        self.assertEqual(self._presentation(0.25)["step"], 0.25)

    def test_a_step_that_cannot_reach_the_default_is_dropped(self):
        self.assertIsNone(self._presentation(1.0)["step"])

    def test_a_step_that_cannot_reach_the_maximum_is_dropped(self):
        self.assertIsNone(self._presentation(0.4, default=0.5)["step"])

    def test_a_step_is_dropped_when_the_analyzer_range_disagrees_with_the_codec(self):
        codec = {"kind": "decimal", "min": 0.0, "max": 2.0}
        rich = {"numeric": {"min": 0.0, "max": 3.0, "step": 0.05}}
        self.assertIsNone(preferences._numeric_presentation("decimal", codec, rich)["step"])

    def test_a_non_positive_step_is_dropped(self):
        self.assertIsNone(self._presentation(0.0)["step"])


class KeybindingDefaults(unittest.TestCase):
    """A keybinding's default must distinguish a proven bound value, a proven
    unbound value, and no evidence at all - the last of those must never be
    presented as an invented `unbound: False`."""

    def _descriptor_for(self, rich: dict) -> dict:
        document, _ = dataset.merge(
            None,
            [
                release(
                    "26.2",
                    REQUIRED
                    + [overriding(setting("key_key.test", "keybind.test", kind="key_binding"), category="Controls")],
                )
            ],
        )
        catalog = preferences.build(document, rich)
        preferences.validate(catalog, document)
        return next(d for d in catalog["descriptors"] if d["id"] == "keybind_test")

    def test_a_proven_serialized_bound_default_is_kept(self):
        descriptor = self._descriptor_for(
            {
                "keybind.test": {
                    "default_confidence": "high",
                    "default": {"serialized": "key.keyboard.w", "unbound": False},
                }
            }
        )
        self.assertEqual(
            descriptor["presentation"]["default"], {"serialized": "key.keyboard.w", "unbound": False}
        )

    def test_a_proven_unbound_default_is_kept(self):
        descriptor = self._descriptor_for(
            {
                "keybind.test": {
                    "default_confidence": "high",
                    "default": {"serialized": "key.keyboard.unknown", "unbound": True},
                }
            }
        )
        self.assertEqual(
            descriptor["presentation"]["default"], {"serialized": "key.keyboard.unknown", "unbound": True}
        )

    def test_no_default_evidence_leaves_the_default_absent_rather_than_invented(self):
        # This is the analyzer's own shape for an unresolved default: a
        # placeholder serialized=None/unbound=False object at medium
        # confidence, not a proof of anything. It must not surface as a
        # proven "not unbound" value.
        descriptor = self._descriptor_for(
            {
                "keybind.test": {
                    "binding_category": "misc",
                    "default_confidence": "medium",
                    "default": {"serialized": None, "unbound": False},
                }
            }
        )
        self.assertIsNone(descriptor["presentation"]["default"])
        self.assertEqual(descriptor["section"], "misc")

    def test_missing_enrichment_also_leaves_the_default_absent(self):
        descriptor = self._descriptor_for({})
        self.assertIsNone(descriptor["presentation"]["default"])


class Validation(unittest.TestCase):
    """Each rule is exercised through the message it raises, so removing the
    rule cannot be masked by another one refusing the same tampered catalog."""

    def build(self) -> tuple[dict, dict]:
        document = small_dataset_document()
        catalog = preferences.build(document, RICH_ENRICHMENT)
        preferences.validate(catalog, document)
        return catalog, document

    def refused(self, pattern: str, catalog: dict, document: dict) -> None:
        with self.assertRaisesRegex(preferences.PreferenceCatalogError, pattern):
            preferences.validate(catalog, document)

    @staticmethod
    def descriptor(catalog: dict, preference_id: str) -> dict:
        return next(d for d in catalog["descriptors"] if d["id"] == preference_id)

    def test_a_well_formed_catalog_validates(self):
        self.build()

    def test_a_future_format_version_is_refused(self):
        catalog, document = self.build()
        catalog["format_version"] = preferences.FORMAT_VERSION + 1
        self.refused("format version", catalog, document)

    def test_a_catalog_for_another_canonical_release_is_refused(self):
        catalog, document = self.build()
        catalog["canonical_minecraft_version"] = "1.20.1"
        self.refused("canonical release does not match", catalog, document)

    def test_two_descriptors_sharing_one_id_are_refused(self):
        catalog, document = self.build()
        catalog["descriptors"][1]["id"] = catalog["descriptors"][0]["id"]
        self.refused("share one product preference id", catalog, document)

    def test_a_descriptor_id_the_application_cannot_represent_is_refused(self):
        for bad in ("BAD-ID", "Fov", "has space", "dotted.id", "caf\u00e9", "", "a" * 65, "fov\n", None):
            catalog, document = self.build()
            catalog["descriptors"][0]["id"] = bad
            with self.assertRaisesRegex(preferences.PreferenceCatalogError, "not a valid preference id", msg=repr(bad)):
                preferences.validate(catalog, document)

    def test_a_descriptor_id_of_the_longest_accepted_length_is_kept(self):
        catalog, document = self.build()
        catalog["descriptors"][0]["id"] = "a" * 64
        preferences.validate(catalog, document)

    def test_an_unknown_category_is_refused(self):
        catalog, document = self.build()
        self.descriptor(catalog, "fov")["category"] = "novelty"
        self.refused("unknown category", catalog, document)

    def test_an_unknown_presentation_tier_is_refused(self):
        catalog, document = self.build()
        self.descriptor(catalog, "fov")["tier"] = "expert"
        self.refused("unknown presentation tier", catalog, document)

    def test_an_unknown_presentation_kind_is_refused(self):
        catalog, document = self.build()
        self.descriptor(catalog, "fov")["presentation"] = {"kind": "eval"}
        self.refused("unknown presentation kind", catalog, document)

    def test_a_descriptor_missing_the_fields_its_kind_requires_is_refused(self):
        catalog, document = self.build()
        window = self.descriptor(catalog, "window_size")
        window["fields"] = [f for f in window["fields"] if f["role"] != "height"]
        self.refused("does not reference the fields", catalog, document)

    def test_a_simple_descriptor_over_a_compound_field_set_is_refused(self):
        catalog, document = self.build()
        fov = self.descriptor(catalog, "fov")
        fov["fields"] = [{"role": "width", "canonical_id": "fov"}, {"role": "height", "canonical_id": "guiScale"}]
        self.refused("does not reference the fields", catalog, document)

    def test_a_field_claimed_by_two_descriptors_is_refused(self):
        catalog, document = self.build()
        self.descriptor(catalog, "gui_scale")["fields"] = [{"role": "value", "canonical_id": "fov"}]
        self.refused("is claimed by both", catalog, document)

    def test_a_reference_to_an_unknown_canonical_setting_is_refused(self):
        catalog, document = self.build()
        self.descriptor(catalog, "fov")["fields"] = [{"role": "value", "canonical_id": "notInTheRegistry"}]
        self.refused("unknown canonical setting", catalog, document)

    def test_a_reference_to_a_historical_setting_is_refused(self):
        catalog, document = self.build()
        legacy_document, _ = dataset.merge(document, [release("1.20.1", FIXTURE_SETTINGS + [setting("legacyOnly")])])
        self.assertNotIn("legacyOnly", legacy_document["current_canonical_setting_ids"])
        self.assertIn("legacyOnly", {entry["id"] for entry in legacy_document["setting_registry"]})
        self.descriptor(catalog, "fov")["fields"] = [{"role": "value", "canonical_id": "legacyOnly"}]
        self.refused("historical", catalog, legacy_document)

    def test_a_deliberately_unmanaged_setting_cannot_be_exposed(self):
        # Proves a generator regression that tried to expose one of the unsafe
        # settings would be caught, not just that the real generator happens
        # not to do it.
        catalog, document = self.build()
        catalog["exclusions"] = [e for e in catalog["exclusions"] if e["canonical_id"] != "resourcePacks"]
        catalog["descriptors"].append(
            {
                "id": "resource_packs_regression",
                "category": "gameplay",
                "section": None,
                "tier": "standard",
                "translation_key": None,
                "help_translation_key": None,
                "fields": [{"role": "value", "canonical_id": "resourcePacks"}],
                "presentation": {"kind": "text", "default": None},
            }
        )
        self.refused("which is not manageable", catalog, document)

    def test_a_presentation_kind_mismatched_with_its_canonical_type_is_refused(self):
        catalog, document = self.build()
        self.descriptor(catalog, "fov")["presentation"] = {"kind": "toggle", "default": None}
        self.refused("does not fit canonical type", catalog, document)

    def test_a_presentation_kind_that_disagrees_with_its_codec_is_refused(self):
        # Both kinds accept the registry's numeric types, so only the proven
        # codec can tell an integer setting from a decimal one.
        catalog, document = self.build()
        render_distance = self.descriptor(catalog, "render_distance")
        render_distance["presentation"] = {
            "kind": "decimal", "default": 12.0, "min": 2.0, "max": 32.0, "step": 1.0,
        }
        self.refused("does not fit the `integer` codec of `renderDistance`", catalog, document)

    def test_every_presentation_kind_is_tied_to_the_codec_that_stores_it(self):
        document = small_dataset_document()
        wrong_codec = {
            "fullscreen": {"kind": "text"},
            "renderDistance": {"kind": "boolean"},
            "cloudStatus": {"kind": "integer", "min": None, "max": None},
            "keybind.attack": {"kind": "boolean"},
            "guiScale": {"kind": "decimal", "min": None, "max": None},
            "languageCode": {"kind": "text"},
        }
        for canonical_id, codec in wrong_codec.items():
            catalog = preferences.build(document, RICH_ENRICHMENT)
            tampered = with_codec(document, canonical_id, codec)
            self.refused(f"does not fit the `{codec['kind']}` codec of `{canonical_id}`", catalog, tampered)

    def test_an_enum_over_a_setting_stored_as_another_shape_is_refused(self):
        catalog, document = self.build()
        document = with_codec(document, "cloudStatus", {"kind": "boolean"})
        self.refused("does not fit the `boolean` codec of `cloudStatus`", catalog, document)

    def test_a_window_size_carries_no_default(self):
        catalog, document = self.build()
        self.descriptor(catalog, "window_size")["presentation"]["default"] = {"serialized": None, "unbound": False}
        self.refused("default .* is not a valid `window_size` default", catalog, document)

    def test_a_window_size_over_ranged_integer_codecs_is_not_tied_to_their_range(self):
        catalog, document = self.build()
        for canonical_id in ("overrideWidth", "overrideHeight"):
            document = with_codec(document, canonical_id, {"kind": "integer", "min": 1, "max": 16384})
        preferences.validate(catalog, document)

    def test_an_enum_without_a_proven_codec_is_not_checked_against_one(self):
        catalog, document = self.build()
        del preferences._canonical_schema(document)["supported"]["cloudStatus"]
        preferences.validate(catalog, document)

    def test_a_setting_without_a_proven_codec_is_not_tied_to_one(self):
        catalog, document = self.build()
        schema = preferences._canonical_schema(document)
        del schema["supported"]["fullscreen"]
        preferences.validate(catalog, document)

    def test_a_numeric_range_that_differs_from_the_codec_is_refused(self):
        catalog, document = self.build()
        volume = self.descriptor(catalog, "sound_volume_master")
        volume["presentation"].update(min=-50.0, max=500.0)
        self.refused("differs from the canonical release's", catalog, document)

    def test_each_numeric_bound_is_compared_with_the_codec_on_its_own(self):
        for bound, value in (("min", 1), ("max", 31), ("min", None), ("max", None)):
            catalog, document = self.build()
            self.descriptor(catalog, "render_distance")["presentation"][bound] = value
            self.refused("differs from the canonical release's", catalog, document)

    def test_an_enum_that_omits_a_proven_value_is_refused(self):
        catalog, document = self.build()
        cloud = self.descriptor(catalog, "cloud_status")
        cloud["presentation"]["values"] = cloud["presentation"]["values"][:1]
        cloud["presentation"]["default"] = None
        self.refused("omits value\\(s\\).*", catalog, document)

    def test_an_id_enum_that_omits_a_proven_id_is_refused(self):
        catalog, document = self.build()
        document = with_codec(document, "cloudStatus", {"kind": "enum_id", "values": [0, 1, 2]})
        cloud = self.descriptor(catalog, "cloud_status")
        cloud["presentation"]["values"] = [
            {"technical_value": str(i), "enum_constant": None, "translation_key": None, "ordinal": None}
            for i in (0, 2)
        ]
        cloud["presentation"]["default"] = None
        self.refused("omits value\\(s\\).*: 1$", catalog, document)

    def test_an_enum_value_outside_the_canonical_codec_domain_is_refused(self):
        catalog, document = self.build()
        cloud = self.descriptor(catalog, "cloud_status")
        cloud["presentation"]["values"].append(
            {"technical_value": "999999", "enum_constant": None, "translation_key": None, "ordinal": None}
        )
        self.refused("999999", catalog, document)

    def test_a_value_is_only_a_member_of_an_enum_codec(self):
        self.assertIsNone(preferences._enum_value_in_codec("true", {"kind": "boolean"}))
        self.assertIsNone(preferences._enum_value_in_codec("true", {"kind": "text"}))

    def test_enum_ids_are_checked_against_the_integer_the_text_parses_to(self):
        codec = {"kind": "enum_id", "values": [0, 1, 2]}
        for inside in ("0", "2", "+1", "02"):
            self.assertTrue(preferences._enum_value_in_codec(inside, codec), inside)
        for outside in ("3", "-1", "two", "", " 1", "1_0", "\uff11"):
            self.assertFalse(preferences._enum_value_in_codec(outside, codec), repr(outside))
        self.assertEqual(preferences._missing_enum_values(["0", "+2"], codec), ["1"])
        self.assertEqual(preferences._missing_enum_values(["0", "1", "2"], codec), [])

    def test_duplicate_enum_technical_values_are_refused(self):
        catalog, document = self.build()
        cloud = self.descriptor(catalog, "cloud_status")
        cloud["presentation"]["values"].append(dict(cloud["presentation"]["values"][0]))
        self.refused("same technical value", catalog, document)

    def test_an_enum_default_absent_from_its_own_values_is_refused(self):
        catalog, document = self.build()
        self.descriptor(catalog, "cloud_status")["presentation"]["default"] = "not-a-real-value"
        self.refused("not one of its own enum values", catalog, document)

    def test_an_inverted_numeric_domain_is_refused(self):
        catalog, document = self.build()
        self.descriptor(catalog, "render_distance")["presentation"].update(min=40, max=32)
        self.refused("domain is inverted", catalog, document)

    def test_a_degenerate_numeric_domain_is_not_inverted(self):
        catalog, document = self.build()
        document = with_codec(document, "renderDistance", {"kind": "integer", "min": 8, "max": 8})
        self.descriptor(catalog, "render_distance")["presentation"].update(min=8, max=8, default=8)
        preferences.validate(catalog, document)

    def test_a_non_positive_numeric_step_is_refused(self):
        for step in (0, -1):
            catalog, document = self.build()
            self.descriptor(catalog, "render_distance")["presentation"]["step"] = step
            self.refused("step must be positive", catalog, document)

    def test_a_default_outside_the_numeric_domain_is_refused_on_either_side(self):
        for default in (1, 33, 9999):
            catalog, document = self.build()
            self.descriptor(catalog, "render_distance")["presentation"]["default"] = default
            self.refused("outside its own numeric domain", catalog, document)

    def test_a_default_at_either_end_of_the_numeric_domain_is_kept(self):
        for default in (2, 32):
            catalog, document = self.build()
            self.descriptor(catalog, "render_distance")["presentation"]["default"] = default
            preferences.validate(catalog, document)

    def test_a_default_of_the_wrong_type_is_refused(self):
        wrong = {
            "fullscreen": "yes",
            "render_distance": 12.0,
            "sound_volume_master": "loud",
            "cloud_status": 3,
            "language": 5,
            "keybind_attack": "w",
        }
        for preference_id, default in wrong.items():
            catalog, document = self.build()
            self.descriptor(catalog, preference_id)["presentation"]["default"] = default
            self.refused("default .* is not a valid", catalog, document)

    def test_a_boolean_is_not_a_numeric_default(self):
        for preference_id in ("render_distance", "sound_volume_master"):
            catalog, document = self.build()
            self.descriptor(catalog, preference_id)["presentation"]["default"] = True
            self.refused("default .* is not a valid", catalog, document)

    def test_a_key_binding_default_must_be_a_bound_or_unbound_object(self):
        for default in (
            {"serialized": 5, "unbound": False},
            {"serialized": None, "unbound": "no"},
            {"serialized": None},
            [],
        ):
            catalog, document = self.build()
            self.descriptor(catalog, "keybind_attack")["presentation"]["default"] = default
            self.refused("default .* is not a valid", catalog, document)

    def test_an_integer_presentation_refuses_numbers_the_application_could_not_read(self):
        for field, value in (("step", 1.0), ("min", 2.0), ("max", 32.5), ("min", True)):
            catalog, document = self.build()
            self.descriptor(catalog, "render_distance")["presentation"][field] = value
            self.refused(f"numeric {NUMERIC_FIELD_NAMES[field]}", catalog, document)

    def test_a_decimal_presentation_refuses_non_finite_numbers(self):
        for value in (float("inf"), float("nan"), "wide"):
            catalog, document = self.build()
            self.descriptor(catalog, "sound_volume_master")["presentation"]["step"] = value
            self.refused("numeric step", catalog, document)

    def test_a_decimal_presentation_accepts_whole_and_fractional_numbers(self):
        catalog, document = self.build()
        presentation = self.descriptor(catalog, "sound_volume_master")["presentation"]
        presentation["step"] = 1
        presentation["default"] = 0.5
        preferences.validate(catalog, document)

    def test_a_missing_exclusion_reason_is_refused(self):
        catalog, document = self.build()
        catalog["exclusions"][0]["reason"] = "   "
        self.refused("has no reason", catalog, document)

    def test_an_exclusion_for_a_non_current_setting_is_refused(self):
        catalog, document = self.build()
        catalog["exclusions"].append({"canonical_id": "notCurrentAtAll", "reason": "test-only"})
        self.refused("is not a current canonical setting", catalog, document)

    def test_a_setting_excluded_twice_is_refused(self):
        catalog, document = self.build()
        catalog["exclusions"].append(dict(catalog["exclusions"][0]))
        self.refused("is excluded twice", catalog, document)

    def test_an_exposed_and_excluded_overlap_is_refused(self):
        catalog, document = self.build()
        catalog["exclusions"].append({"canonical_id": "fov", "reason": "test-only overlap"})
        self.refused("both exposed and excluded", catalog, document)

    def test_an_unaccounted_current_setting_is_refused(self):
        catalog, document = self.build()
        catalog["descriptors"] = [d for d in catalog["descriptors"] if d["id"] != "fov"]
        self.refused("not accounted for: fov", catalog, document)


class WindowSizeComposition(unittest.TestCase):
    def test_a_release_without_the_window_fields_cannot_compose_window_size(self):
        without = [s for s in FIXTURE_SETTINGS if s["persisted_key"] not in ("overrideWidth", "overrideHeight")]
        document, _ = dataset.merge(None, [release("26.2", without)])
        with self.assertRaisesRegex(preferences.PreferenceCatalogError, "non-current canonical setting"):
            preferences.build(document)

    def test_a_release_with_only_the_window_fields_still_builds(self):
        only = [s for s in FIXTURE_SETTINGS if s["persisted_key"] in ("overrideWidth", "overrideHeight")]
        document, _ = dataset.merge(None, [release("26.2", only)])
        catalog = preferences.build(document)
        preferences.validate(catalog, document)
        self.assertEqual([d["id"] for d in catalog["descriptors"]], ["window_size"])


class NumericKind(unittest.TestCase):
    def test_the_codec_decides_the_kind_not_the_field_type(self):
        self.assertEqual(preferences._numeric_kind("integer", {"kind": "decimal"}), "decimal")
        self.assertEqual(preferences._numeric_kind("integer", {"kind": "scaled_decimal"}), "decimal")
        self.assertEqual(preferences._numeric_kind("number", {"kind": "integer"}), "integer")

    def test_without_a_numeric_codec_the_field_type_decides(self):
        self.assertEqual(preferences._numeric_kind("integer", None), "integer")
        self.assertEqual(preferences._numeric_kind("number", None), "decimal")
        self.assertEqual(preferences._numeric_kind("integer", {"kind": "text"}), "integer")
        self.assertEqual(preferences._numeric_kind("number", {"kind": "text"}), "decimal")


class IntegerPresentation(unittest.TestCase):
    """The application reads an `integer` presentation as `i64`, so whole
    numbers serialize without a fractional part and fractions are never
    rounded into one."""

    def _presentation(self, codec: dict, rich: dict | None) -> dict:
        return preferences._numeric_presentation("integer", codec, rich)

    def test_whole_valued_floats_are_written_as_integers(self):
        codec = {"kind": "integer", "min": 2.0, "max": 32.0}
        rich = {
            "default": 12.0,
            "default_confidence": "high",
            "numeric": {"min": 2, "max": 32, "step": 1.0},
        }
        presentation = self._presentation(codec, rich)
        self.assertEqual(presentation, {"kind": "integer", "default": 12, "min": 2, "max": 32, "step": 1})
        for field in ("default", "min", "max", "step"):
            self.assertIs(type(presentation[field]), int, field)
        self.assertNotIn("1.0", preferences.dumps(presentation))

    def test_decimal_bounds_are_written_as_floats(self):
        presentation = preferences._numeric_presentation("decimal", {"kind": "decimal", "min": 0, "max": 1}, None)
        self.assertEqual((presentation["min"], presentation["max"]), (0.0, 1.0))
        self.assertIs(type(presentation["min"]), float)
        self.assertIs(type(presentation["max"]), float)
        self.assertIsNone(preferences._numeric_presentation("decimal", {"kind": "decimal"}, None)["min"])

    def test_a_fractional_default_is_dropped_not_rounded(self):
        rich = {"default": 12.5, "default_confidence": "high"}
        self.assertIsNone(self._presentation({"kind": "integer"}, rich)["default"])

    def test_a_fractional_step_has_no_integer_presentation(self):
        codec = {"kind": "integer", "min": 0, "max": 10}
        rich = {"numeric": {"min": 0, "max": 10, "step": 0.5}}
        self.assertIsNone(self._presentation(codec, rich)["step"])

    def test_a_fractional_codec_bound_is_kept_so_validation_can_name_it(self):
        document = with_codec(small_dataset_document(), "renderDistance", {"kind": "integer", "min": 2.5, "max": 32})
        catalog = preferences.build(document, RICH_ENRICHMENT)
        with self.assertRaisesRegex(preferences.PreferenceCatalogError, "numeric minimum"):
            preferences.validate(catalog, document)


class DefaultConfidence(unittest.TestCase):
    """A default is carried only when the analyzer proved it."""

    def _presentation(self, cid: str, value_type: str, codec: dict | None, default, confidence: str) -> dict:
        rich = {"default": default, "default_confidence": confidence}
        return preferences._presentation(cid, value_type, codec, rich)

    def test_each_kind_carries_a_high_confidence_default_and_drops_any_other(self):
        cases = [
            ("fullscreen", "boolean", {"kind": "boolean"}, True),
            ("someText", "string", {"kind": "text"}, "hello"),
            (preferences.LANGUAGE_CANONICAL_ID, "string", {"kind": "language_code"}, "en_us"),
            ("guiScale", "integer", {"kind": "integer"}, 2),
            ("chatOpacity", "number", {"kind": "decimal"}, 0.5),
            ("cloudStatus", "enum", {"kind": "enum_token", "tokens": ["fast"]}, "fast"),
            ("mapped", "enum", {"kind": "enum_id", "values": [3]}, 3),
        ]
        for cid, value_type, codec, default in cases:
            proven = self._presentation(cid, value_type, codec, default, "high")["default"]
            self.assertEqual(proven, str(default) if value_type == "enum" else default, cid)
            for confidence in ("medium", "low", "unknown"):
                self.assertIsNone(self._presentation(cid, value_type, codec, default, confidence)["default"], cid)

    def test_a_default_of_the_wrong_type_is_not_carried(self):
        self.assertIsNone(self._presentation("fullscreen", "boolean", {"kind": "boolean"}, 1, "high")["default"])
        self.assertIsNone(self._presentation("someText", "string", {"kind": "text"}, 5, "high")["default"])
        self.assertIsNone(self._presentation("guiScale", "integer", {"kind": "integer"}, True, "high")["default"])
        self.assertIsNone(self._presentation("guiScale", "integer", {"kind": "integer"}, "2", "high")["default"])

    def test_the_language_setting_is_presented_as_a_language_not_as_text(self):
        language = self._presentation(preferences.LANGUAGE_CANONICAL_ID, "string", {"kind": "language_code"}, "en_us", "high")
        self.assertEqual(language, {"kind": "language", "default": "en_us"})
        text = self._presentation("someText", "string", {"kind": "text"}, "en_us", "high")
        self.assertEqual(text, {"kind": "text", "default": "en_us"})

    def test_an_unknown_canonical_type_has_no_presentation(self):
        with self.assertRaisesRegex(preferences.PreferenceCatalogError, "no presentation kind covers"):
            preferences._presentation("odd", "special", None, None)

    def test_a_step_is_dropped_when_either_analyzer_bound_disagrees_with_the_codec(self):
        codec = {"kind": "decimal", "min": 0.0, "max": 2.0}
        for numeric in (
            {"min": 0.5, "max": 2.0, "step": 0.5},
            {"min": 0.0, "max": 3.0, "step": 0.5},
            {"min": None, "max": 2.0, "step": 0.5},
            {"min": 0.0, "max": None, "step": 0.5},
        ):
            presentation = preferences._numeric_presentation("decimal", codec, {"numeric": numeric})
            self.assertIsNone(presentation["step"], numeric)
        agreeing = preferences._numeric_presentation(
            "decimal", codec, {"numeric": {"min": 0, "max": 2, "step": 0.5}}
        )
        self.assertEqual(agreeing["step"], 0.5)

    def test_a_step_needs_a_proven_codec_range_to_agree_with(self):
        codec = {"kind": "decimal", "min": 0.0, "max": None}
        presentation = preferences._numeric_presentation(
            "decimal", codec, {"numeric": {"min": 0.0, "max": 2.0, "step": 0.5}}
        )
        self.assertIsNone(presentation["step"])


class Sections(unittest.TestCase):
    def test_sections_come_from_the_setting_family(self):
        self.assertEqual(preferences._section("sound.volume.music", "number", None), "volume")
        self.assertEqual(preferences._section("modelPart.cape", "boolean", None), "skin_parts")
        self.assertIsNone(preferences._section("modelPartial", "boolean", None))
        self.assertIsNone(preferences._section("soundVolume", "number", None))
        self.assertIsNone(preferences._section("fov", "integer", {"binding_category": "movement"}))

    def test_a_key_binding_section_is_its_binding_category(self):
        self.assertEqual(preferences._section("keybind.attack", "key_binding", {"binding_category": "gameplay"}), "gameplay")
        self.assertIsNone(preferences._section("keybind.attack", "key_binding", None))

    def test_only_debug_key_bindings_are_advanced(self):
        self.assertEqual(preferences._tier("key_binding", {"binding_category": "debug"}), "advanced")
        self.assertEqual(preferences._tier("key_binding", {"binding_category": "movement"}), "standard")
        self.assertEqual(preferences._tier("key_binding", None), "standard")
        self.assertEqual(preferences._tier("boolean", {"binding_category": "debug"}), "standard")

    def test_built_descriptors_carry_their_family_section_and_no_help_text(self):
        document = small_dataset_document()
        catalog = preferences.build(document, RICH_ENRICHMENT)
        by_id = {d["id"]: d for d in catalog["descriptors"]}
        self.assertEqual(by_id["model_part_cape"]["section"], "skin_parts")
        self.assertIsNone(by_id["fov"]["section"])
        for descriptor in catalog["descriptors"]:
            self.assertIn("help_translation_key", descriptor)
            self.assertIsNone(descriptor["help_translation_key"], descriptor["id"])


class EnumValues(unittest.TestCase):
    def _values(self, codec: dict, labeled: list[dict] | None) -> list[str]:
        rich = None if labeled is None else {"allowed_values": {"values": labeled}}
        return [v["technical_value"] for v in preferences._enum_values(codec, rich)]

    @staticmethod
    def _label(persisted, ordinal) -> dict:
        return {"persisted": persisted, "enum_constant": f"C{ordinal}", "translation_key": f"k.{ordinal}", "ordinal": ordinal}

    def test_proven_values_without_enrichment_are_offered_unlabeled(self):
        values = preferences._enum_values({"kind": "enum_token", "tokens": ["b", "a"]}, None)
        self.assertEqual(
            values,
            [
                {"technical_value": token, "enum_constant": None, "translation_key": None, "ordinal": None}
                for token in ("a", "b")
            ],
        )

    def test_values_enrichment_does_not_cover_are_appended_unlabeled(self):
        codec = {"kind": "enum_token", "tokens": ["a", "b", "c"]}
        values = preferences._enum_values(codec, {"allowed_values": {"values": [self._label("c", 0)]}})
        self.assertEqual([v["technical_value"] for v in values], ["c", "a", "b"])
        self.assertEqual(values[0]["enum_constant"], "C0")
        self.assertIsNone(values[1]["enum_constant"])

    def test_unlabeled_ids_are_appended_in_numeric_order(self):
        codec = {"kind": "enum_id", "values": [0, 1, 2, 10, 11, 3]}
        self.assertEqual(self._values(codec, None), ["0", "1", "2", "3", "10", "11"])
        labeled = [self._label(11, 0)]
        self.assertEqual(self._values(codec, labeled), ["11", "0", "1", "2", "3", "10"])

    def test_labeled_values_follow_their_ordinal_not_their_text(self):
        codec = {"kind": "enum_id", "values": [0, 1, 2, 10]}
        labeled = [self._label(10, 0), self._label(2, 1), self._label(0, 2), self._label(1, 3)]
        self.assertEqual(self._values(codec, labeled), ["10", "2", "0", "1"])

    def test_a_label_the_codec_does_not_prove_is_dropped(self):
        codec = {"kind": "enum_token", "tokens": ["a"]}
        self.assertEqual(self._values(codec, [self._label("a", 0), self._label("zzz", 1)]), ["a"])

    def test_a_value_labeled_twice_is_offered_once(self):
        codec = {"kind": "enum_token", "tokens": ["a"]}
        self.assertEqual(self._values(codec, [self._label("a", 0), self._label("a", 1)]), ["a"])

    def test_a_label_without_a_persisted_value_is_skipped(self):
        values = [{"persisted": None, "enum_constant": "NONE", "translation_key": None, "ordinal": 0}, self._label("x", 1)]
        self.assertEqual(self._values({}, values), ["x"])

    def test_without_a_proven_domain_only_labeled_values_are_offered(self):
        self.assertEqual(self._values({}, [self._label("x", 0)]), ["x"])
        self.assertEqual(self._values({}, None), [])


class StableIdentity(unittest.TestCase):
    def test_the_preferences_that_predate_the_catalog_keep_their_exact_ids(self):
        expected = {
            "guiScale": "gui_scale",
            "languageCode": "language",
            "fullscreen": "fullscreen",
            "fov": "fov",
            "renderDistance": "render_distance",
            "simulationDistance": "simulation_distance",
        }
        self.assertEqual(preferences.STABLE_ID_OVERRIDES, expected)
        for canonical_id, preference_id in expected.items():
            self.assertEqual(preferences.stable_id(canonical_id), preference_id)
        self.assertNotEqual(preferences.snake_case("languageCode"), "language")

    def test_a_new_canonical_id_gets_a_mechanical_snake_case_identity(self):
        self.assertEqual(preferences.stable_id("cloudStatus"), "cloud_status")
        self.assertEqual(preferences.stable_id("keybind.attack"), "keybind_attack")
        self.assertEqual(preferences.stable_id("modelPart.cape"), "model_part_cape")
        self.assertEqual(preferences.stable_id("invertXMouse"), "invert_x_mouse")

    def test_snake_case_handles_the_edges_of_an_identifier(self):
        self.assertEqual(preferences.snake_case("Fov"), "fov")
        self.assertEqual(preferences.snake_case("scaleX"), "scale_x")
        self.assertEqual(preferences.snake_case("useHTTP"), "use_http")
        self.assertEqual(preferences.snake_case("sound.volume.master"), "sound_volume_master")
        self.assertEqual(preferences.snake_case("gui2Scale"), "gui2_scale")


class Serialization(unittest.TestCase):
    def test_the_on_disk_text_is_compact_sorted_ascii_and_newline_terminated(self):
        text = preferences.dumps({"b": [1, 2], "a": "caf\u00e9"})
        self.assertEqual(text, '{"a":"caf\\u00e9","b":[1,2]}\n')


class MaintainerCommand(unittest.TestCase):
    """`main` regenerates and verifies the bundled catalog; a request for
    enrichment that cannot be met must fail instead of writing a stripped
    catalog."""

    def setUp(self):
        self.directory = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.directory, ignore_errors=True)
        self.document = small_dataset_document()
        self.dataset_path = self.directory / "dataset.json"
        self.dataset_path.write_text(json.dumps(self.document), encoding="utf-8")
        self.out = self.directory / "out" / "preferences.json"

    def record(self, **overrides) -> dict:
        record = release("26.2", FIXTURE_SETTINGS)
        for extracted in record["settings"]:
            if extracted["persisted_key"] == "fov":
                extracted.update(default=70, default_confidence="high")
        record.update(jar_sha256="a" * 64)
        record.update(overrides)
        return record

    def analysis(self, record: dict, *, layout: str = "per-release", name: str = "26.2.json") -> Path:
        root = self.directory / "analysis"
        directory = root / "mc-26.2" / "normalized" if layout == "per-release" else root / "normalized"
        directory.mkdir(parents=True, exist_ok=True)
        (directory / name).write_text(json.dumps(record), encoding="utf-8")
        return root

    def run_main(self, *arguments: str) -> tuple[int, str, str]:
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = preferences.main(["--dataset", str(self.dataset_path), "--out", str(self.out), *arguments])
        return code, out.getvalue(), err.getvalue()

    def written(self) -> dict:
        return json.loads(self.out.read_text(encoding="utf-8"))

    def fov_default(self) -> object:
        return next(d for d in self.written()["descriptors"] if d["id"] == "fov")["presentation"]["default"]

    def test_the_catalog_is_written_with_the_proven_enrichment(self):
        code, out, err = self.run_main("--from-analysis", str(self.analysis(self.record())))
        self.assertEqual((code, err), (0, ""), err)
        self.assertIn("bundled preference catalog", out)
        self.assertEqual(self.fov_default(), 70)
        expected = preferences.build(self.document, preferences.rich_settings_by_canonical_id(self.record()))
        self.assertEqual(self.out.read_text(encoding="utf-8"), preferences.dumps(expected))

    def test_the_analysis_root_layout_of_an_analyzed_release_is_read_too(self):
        code, _, err = self.run_main("--from-analysis", str(self.analysis(self.record(), layout="release")))
        self.assertEqual(code, 0, err)
        self.assertEqual(self.fov_default(), 70)

    def test_a_stale_root_leftover_beside_the_per_release_analysis_is_ignored(self):
        stale = self.record()
        stale["settings"] = [s for s in stale["settings"] if s["persisted_key"] != "lang"]
        root = self.analysis(stale, layout="release", name="old.json")
        self.analysis(self.record())
        code, _, err = self.run_main("--from-analysis", str(root))
        self.assertEqual(code, 0, err)
        self.assertEqual(self.fov_default(), 70)

    def test_check_accepts_exactly_what_was_written(self):
        analysis = str(self.analysis(self.record()))
        self.assertEqual(self.run_main("--from-analysis", analysis)[0], 0)
        code, out, err = self.run_main("--from-analysis", analysis, "--check")
        self.assertEqual((code, err), (0, ""), err)
        self.assertIn("exactly what these inputs regenerate", out)

    def test_check_reports_a_catalog_that_drifted(self):
        analysis = str(self.analysis(self.record()))
        self.run_main("--from-analysis", analysis)
        self.out.write_text(self.out.read_text(encoding="utf-8").replace('"default":70', '"default":71'), encoding="utf-8")
        code, out, err = self.run_main("--from-analysis", analysis, "--check")
        self.assertEqual(code, 1)
        self.assertIn("does not match what these inputs regenerate", err)
        self.assertNotIn("--from-analysis", err)

    def test_check_writes_nothing_and_a_missing_catalog_does_not_match(self):
        code, _, err = self.run_main("--from-analysis", str(self.analysis(self.record())), "--check")
        self.assertEqual(code, 1)
        self.assertIn("does not match", err)
        self.assertFalse(self.out.exists())

    def test_a_missing_analysis_directory_is_an_error_that_writes_nothing(self):
        for arguments in (("--from-analysis", str(self.directory / "mistyped")), ("--from-analysis", str(self.directory))):
            code, _, err = self.run_main(*arguments)
            self.assertEqual(code, 1, arguments)
            self.assertIn("no analyzer output", err)
            self.assertFalse(self.out.exists())

    def test_check_names_a_missing_analysis_instead_of_reporting_a_mismatch(self):
        self.run_main()
        code, _, err = self.run_main("--from-analysis", str(self.directory / "mistyped"), "--check")
        self.assertEqual(code, 1)
        self.assertIn("no analyzer output", err)
        self.assertNotIn("does not match", err)

    def test_a_requested_analysis_never_overwrites_an_existing_catalog_when_it_fails(self):
        self.out.parent.mkdir(parents=True)
        self.out.write_text("untouched\n", encoding="utf-8")
        self.assertEqual(self.run_main("--from-analysis", str(self.directory / "mistyped"))[0], 1)
        self.assertEqual(self.out.read_text(encoding="utf-8"), "untouched\n")

    def test_an_analysis_of_another_release_is_refused(self):
        root = self.analysis(release("1.20.1", FIXTURE_SETTINGS))
        code, _, err = self.run_main("--from-analysis", str(root))
        self.assertEqual(code, 1)
        self.assertIn("no record for 26.2", err)

    def test_an_incomplete_analysis_is_refused(self):
        root = self.analysis(self.record(extraction_complete=False))
        code, _, err = self.run_main("--from-analysis", str(root))
        self.assertEqual(code, 1)
        self.assertIn("did not extract completely", err)

    def test_an_analysis_that_no_longer_matches_the_dataset_is_refused_as_stale(self):
        stale = self.record()
        stale["settings"] = [s for s in stale["settings"] if s["persisted_key"] != "lang"]
        code, _, err = self.run_main("--from-analysis", str(self.analysis(stale)))
        self.assertEqual(code, 1)
        self.assertIn("stale", err)
        self.assertFalse(self.out.exists())

    def test_two_different_analyses_of_the_canonical_release_are_ambiguous(self):
        root = self.analysis(self.record(jar_sha256="a" * 64), name="a.json")
        self.analysis(self.record(jar_sha256="b" * 64), name="b.json")
        code, _, err = self.run_main("--from-analysis", str(root))
        self.assertEqual(code, 1)
        self.assertIn("more than one distinct analysis", err)

    def test_the_same_analysis_found_twice_is_not_ambiguous(self):
        root = self.analysis(self.record(), name="a.json")
        self.analysis(self.record(), layout="release", name="b.json")
        self.assertEqual(self.run_main("--from-analysis", str(root))[0], 0)

    def test_an_unreadable_analysis_file_is_named(self):
        root = self.analysis(self.record())
        (root / "mc-26.2" / "normalized" / "broken.json").write_text("{", encoding="utf-8")
        code, _, err = self.run_main("--from-analysis", str(root))
        self.assertEqual(code, 1)
        self.assertIn("cannot read analyzer output", err)

    def test_an_analysis_file_that_is_not_an_object_is_named(self):
        root = self.analysis(self.record())
        (root / "mc-26.2" / "normalized" / "list.json").write_text("[]", encoding="utf-8")
        code, _, err = self.run_main("--from-analysis", str(root))
        self.assertEqual(code, 1)
        self.assertIn("not a JSON object", err)

    def test_without_an_analysis_the_catalog_is_written_without_enrichment_and_says_so(self):
        code, _, err = self.run_main()
        self.assertEqual(code, 0)
        self.assertIn("without enrichment", err)
        self.assertIsNone(self.fov_default())

    def test_check_without_an_analysis_names_the_missing_enrichment(self):
        self.run_main("--from-analysis", str(self.analysis(self.record())))
        code, _, err = self.run_main("--check")
        self.assertEqual(code, 1)
        self.assertIn("no --from-analysis was given", err)

    def test_a_catalog_that_fails_validation_is_not_written(self):
        document = with_codec(self.document, "renderDistance", {"kind": "integer", "min": 2.5, "max": 32})
        self.dataset_path.write_text(json.dumps(document), encoding="utf-8")
        code, _, err = self.run_main()
        self.assertEqual(code, 1)
        self.assertIn("numeric minimum", err)
        self.assertFalse(self.out.exists())

    def test_a_damaged_or_missing_dataset_is_an_error_not_a_traceback(self):
        for content in ('{"format_version": 1, "setting_reg', "[]", ""):
            self.dataset_path.write_text(content, encoding="utf-8")
            with self.subTest(content=content):
                code, _, err = self.run_main()
                self.assertEqual(code, 1)
                self.assertIn("the compatibility dataset", err)
                self.assertFalse(self.out.exists())
        self.dataset_path.unlink()
        code, _, err = self.run_main()
        self.assertEqual(code, 1)
        self.assertIn("cannot be read", err)

    def test_a_dataset_missing_the_structure_the_catalog_is_built_from_is_an_error(self):
        def without_canonical_schema(document):
            document["schemas"] = []

        def unregistered_current_setting(document):
            document["current_canonical_setting_ids"].append("no.such.setting")

        def without_registry(document):
            del document["setting_registry"]

        def non_object_codec(document):
            for representation in document["representations"]:
                representation["codec"] = "x"

        def non_string_current_id(document):
            document["current_canonical_setting_ids"].append([1])

        def dangling_representation(document):
            schema = next(s for s in document["schemas"] if s["supported"])
            schema["supported"][next(iter(schema["supported"]))] = 10_000

        for mutate in (
            without_canonical_schema, unregistered_current_setting, without_registry, dangling_representation,
            non_object_codec, non_string_current_id,
        ):
            document = copy.deepcopy(self.document)
            mutate(document)
            self.dataset_path.write_text(json.dumps(document), encoding="utf-8")
            with self.subTest(mutation=mutate.__name__):
                code, _, err = self.run_main()
                self.assertEqual(code, 1)
                self.assertIn("the compatibility dataset is malformed", err)
                self.assertFalse(self.out.exists())

    def test_a_matching_analyzer_record_with_unusable_settings_is_an_error(self):
        for damage in (
            lambda setting: setting.pop("logical_id"),
            lambda setting: setting.update(logical_id=["unhashable"]),
        ):
            record = self.record()
            for setting in record["settings"]:
                damage(setting)
            with self.subTest(record=record["settings"][0].get("logical_id")):
                code, _, err = self.run_main("--from-analysis", str(self.analysis(record)))
                self.assertEqual(code, 1)
                self.assertIn("analyzer output", err)
                self.assertIn("malformed", err)
                self.assertFalse(self.out.exists())

    def test_an_interrupted_write_keeps_the_previous_catalog(self):
        self.assertEqual(self.run_main()[0], 0)
        before = self.out.read_bytes()
        self.dataset_path.write_text(
            json.dumps(with_codec(self.document, "renderDistance", {"kind": "integer", "min": 4, "max": 24})),
            encoding="utf-8",
        )
        with mock.patch.object(file_io.os, "replace", side_effect=OSError("killed")):
            code, _, err = self.run_main()
        self.assertEqual(code, 1)
        self.assertIn("could not write", err)
        self.assertEqual(self.out.read_bytes(), before)
        self.assertEqual([p.name for p in self.out.parent.iterdir()], [self.out.name])

    def test_check_reports_a_catalog_path_that_cannot_be_read(self):
        self.out.mkdir(parents=True)
        code, _, err = self.run_main("--check")
        self.assertEqual(code, 1)
        self.assertIn("cannot read", err)

    def test_check_treats_a_damaged_catalog_as_a_mismatch(self):
        self.out.parent.mkdir(parents=True)
        self.out.write_bytes(b'{"descriptors": [\xff')
        code, _, err = self.run_main("--check")
        self.assertEqual(code, 1)
        self.assertIn("does not match", err)


class OfferedSettings(unittest.TestCase):
    """The curated set of settings the shipped catalog offers."""

    @classmethod
    def setUpClass(cls):
        cls.dataset_document = json.loads(BUNDLED_DATASET.read_text(encoding="utf-8"))
        cls.catalog = preferences.build(cls.dataset_document)
        cls.current = set(cls.dataset_document["current_canonical_setting_ids"])

    def test_every_offered_setting_is_a_current_canonical_setting(self):
        self.assertEqual(preferences.OFFERED_SETTINGS - self.current, set())

    def test_the_shipped_dataset_covers_the_offered_settings_and_nothing_else(self):
        registered = {entry["id"] for entry in self.dataset_document["setting_registry"]}
        self.assertEqual(self.current, preferences.OFFERED_SETTINGS)
        self.assertEqual(registered, preferences.OFFERED_SETTINGS | dataset.ALWAYS_RETAINED)

    def test_a_setting_the_policy_excludes_is_never_offered(self):
        self.assertEqual(set(preferences.EXCLUSIONS) & preferences.OFFERED_SETTINGS, set())

    def test_exactly_the_offered_settings_are_exposed(self):
        exposed = {f["canonical_id"] for d in self.catalog["descriptors"] for f in d["fields"]}
        compound = {cid for spec in preferences.COMPOUND_DESCRIPTORS for _, cid in spec["fields"]}
        self.assertEqual(exposed, preferences.OFFERED_SETTINGS | compound)

    def test_the_committed_catalog_offers_the_same_preferences(self):
        committed = json.loads(BUNDLED_CATALOG.read_text(encoding="utf-8"))
        self.assertEqual(
            {d["id"] for d in committed["descriptors"]},
            {d["id"] for d in self.catalog["descriptors"]},
        )


class RealCanonicalShape(unittest.TestCase):
    """Checks the shape the plan describes against the dataset actually
    checked into the repository, so an unexplained drift is visible here
    rather than only inside the generated catalog."""

    @classmethod
    def setUpClass(cls):
        cls.dataset_document = json.loads(BUNDLED_DATASET.read_text(encoding="utf-8"))
        cls.catalog = preferences.build(cls.dataset_document)
        preferences.validate(cls.catalog, cls.dataset_document)

    def test_every_current_setting_is_exposed_or_excluded_exactly_once(self):
        exposed = [f["canonical_id"] for d in self.catalog["descriptors"] for f in d["fields"]]
        excluded = [e["canonical_id"] for e in self.catalog["exclusions"]]
        self.assertEqual(len(set(exposed)), len(exposed))
        self.assertEqual(set(exposed) & set(excluded), set())
        self.assertEqual(set(exposed) | set(excluded), set(self.dataset_document["current_canonical_setting_ids"]))

    def test_the_exclusions_are_the_reviewed_policy_that_is_still_current(self):
        current = set(self.dataset_document["current_canonical_setting_ids"])
        self.assertEqual(
            {e["canonical_id"] for e in self.catalog["exclusions"]},
            set(preferences.EXCLUSIONS) & current,
        )

    def test_the_deliberately_unmanaged_settings_are_the_excluded_ones(self):
        registry = {entry["id"]: entry for entry in self.dataset_document["setting_registry"]}
        current = set(self.dataset_document["current_canonical_setting_ids"])
        unmanaged = {cid for cid in current if not registry[cid]["manageable"]}
        self.assertEqual(unmanaged, {e["canonical_id"] for e in self.catalog["exclusions"]})

    def test_every_current_manageable_key_binding_has_a_descriptor(self):
        registry = {entry["id"]: entry for entry in self.dataset_document["setting_registry"]}
        current = set(self.dataset_document["current_canonical_setting_ids"])
        expected = {
            cid for cid in current
            if registry[cid]["type"] == "key_binding" and registry[cid]["manageable"]
        }
        shown = {
            d["fields"][0]["canonical_id"]
            for d in self.catalog["descriptors"]
            if d["presentation"]["kind"] == "key_binding"
        }
        self.assertEqual(shown, expected)

    def test_sound_and_music_settings_are_exposed_in_their_category(self):
        registry = {entry["id"]: entry for entry in self.dataset_document["setting_registry"]}
        current = set(self.dataset_document["current_canonical_setting_ids"])
        expected = {
            cid for cid in current
            if registry[cid]["category"] == "Sound & Music" and registry[cid]["manageable"]
        }
        shown = {
            f["canonical_id"]
            for d in self.catalog["descriptors"]
            if d["category"] == "sound_and_music"
            for f in d["fields"]
        }
        self.assertEqual(shown, expected)

    def test_the_bundled_catalog_satisfies_every_validation_rule(self):
        bundled = json.loads(BUNDLED_CATALOG.read_text(encoding="utf-8"))
        preferences.validate(bundled, self.dataset_document)

    def test_the_bundled_catalog_steps_match_what_the_game_stores(self):
        bundled = json.loads(BUNDLED_CATALOG.read_text(encoding="utf-8"))
        steps = {d["id"]: d["presentation"].get("step") for d in bundled["descriptors"]}
        # The field of view is whole degrees; a sound volume is a free fraction.
        self.assertEqual(steps["fov"], 1.0)
        self.assertIsNone(steps["sound_volume_master"])

    def test_regenerating_from_the_same_dataset_is_deterministic(self):
        again = preferences.build(json.loads(BUNDLED_DATASET.read_text(encoding="utf-8")))
        self.assertEqual(preferences.dumps(again), preferences.dumps(self.catalog))


if __name__ == "__main__":
    unittest.main()
