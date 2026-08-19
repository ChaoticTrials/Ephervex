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

import copy
import json
import unittest
from pathlib import Path

import dataset
import preferences
from test_dataset import REQUIRED, release, setting

REPO_ROOT = Path(__file__).resolve().parent.parent.parent
BUNDLED_DATASET = REPO_ROOT / "crates" / "options" / "data" / "minecraft-options.json"


def overriding(base: dict, **overrides) -> dict:
    merged = dict(base)
    merged.update(overrides)
    return merged


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


class StableIdentity(unittest.TestCase):
    def test_the_seven_pre_existing_preferences_keep_their_config_names(self):
        document = small_dataset_document()
        catalog = preferences.build(document)
        ids = {d["id"] for d in catalog["descriptors"]}
        for expected in (
            "gui_scale",
            "language",
            "fullscreen",
            "window_size",
            "fov",
            "render_distance",
            "simulation_distance",
        ):
            self.assertIn(expected, ids)

    def test_a_new_canonical_id_gets_a_mechanical_snake_case_identity(self):
        self.assertEqual(preferences.stable_id("cloudStatus"), "cloud_status")
        self.assertEqual(preferences.stable_id("keybind.attack"), "keybind_attack")
        self.assertEqual(preferences.stable_id("modelPart.cape"), "model_part_cape")
        self.assertEqual(preferences.stable_id("invertXMouse"), "invert_x_mouse")


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
        # guiScale in REQUIRED has no default_confidence override (defaults to
        # "unknown" in the shared test helper), so no rich enrichment is
        # supplied for it either; the presentation default must stay absent.
        descriptor = self.by_id["gui_scale"]
        self.assertIsNone(descriptor["presentation"]["default"])


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
    def build(self) -> tuple[dict, dict]:
        document = small_dataset_document()
        catalog = preferences.build(document, RICH_ENRICHMENT)
        preferences.validate(catalog, document)
        return catalog, document

    def test_a_well_formed_catalog_validates(self):
        self.build()

    def test_two_descriptors_sharing_one_id_are_refused(self):
        catalog, document = self.build()
        catalog["descriptors"][1]["id"] = catalog["descriptors"][0]["id"]
        with self.assertRaises(preferences.PreferenceCatalogError):
            preferences.validate(catalog, document)

    def test_an_exposed_and_excluded_overlap_is_refused(self):
        catalog, document = self.build()
        catalog["exclusions"].append({"canonical_id": "fov", "reason": "test-only overlap"})
        with self.assertRaises(preferences.PreferenceCatalogError):
            preferences.validate(catalog, document)

    def test_a_missing_exclusion_reason_is_refused(self):
        catalog, document = self.build()
        catalog["exclusions"][0]["reason"] = "   "
        with self.assertRaises(preferences.PreferenceCatalogError):
            preferences.validate(catalog, document)

    def test_an_exclusion_for_a_non_current_setting_is_refused(self):
        catalog, document = self.build()
        catalog["exclusions"].append({"canonical_id": "notCurrentAtAll", "reason": "test-only"})
        with self.assertRaises(preferences.PreferenceCatalogError):
            preferences.validate(catalog, document)

    def test_an_unaccounted_current_setting_is_refused(self):
        catalog, document = self.build()
        catalog["descriptors"] = [d for d in catalog["descriptors"] if d["id"] != "fov"]
        with self.assertRaises(preferences.PreferenceCatalogError):
            preferences.validate(catalog, document)

    def test_an_unknown_category_is_refused(self):
        catalog, document = self.build()
        catalog["descriptors"][0]["category"] = "novelty"
        with self.assertRaises(preferences.PreferenceCatalogError):
            preferences.validate(catalog, document)

    def test_an_unknown_presentation_kind_is_refused(self):
        catalog, document = self.build()
        catalog["descriptors"][0]["presentation"] = {"kind": "eval"}
        with self.assertRaises(preferences.PreferenceCatalogError):
            preferences.validate(catalog, document)

    def test_a_presentation_kind_mismatched_with_its_canonical_type_is_refused(self):
        catalog, document = self.build()
        fov = next(d for d in catalog["descriptors"] if d["id"] == "fov")
        fov["presentation"] = {"kind": "toggle", "default": None}
        with self.assertRaises(preferences.PreferenceCatalogError):
            preferences.validate(catalog, document)

    def test_a_field_claimed_by_two_descriptors_is_refused(self):
        catalog, document = self.build()
        catalog["descriptors"][0]["fields"] = [{"role": "value", "canonical_id": "fov"}]
        with self.assertRaises(preferences.PreferenceCatalogError):
            preferences.validate(catalog, document)

    def test_an_inverted_numeric_domain_is_refused(self):
        catalog, document = self.build()
        render_distance = next(d for d in catalog["descriptors"] if d["id"] == "render_distance")
        render_distance["presentation"]["min"] = 100
        with self.assertRaises(preferences.PreferenceCatalogError):
            preferences.validate(catalog, document)

    def test_a_default_outside_the_numeric_domain_is_refused(self):
        catalog, document = self.build()
        render_distance = next(d for d in catalog["descriptors"] if d["id"] == "render_distance")
        render_distance["presentation"]["default"] = 9999
        with self.assertRaises(preferences.PreferenceCatalogError):
            preferences.validate(catalog, document)

    def test_duplicate_enum_technical_values_are_refused(self):
        catalog, document = self.build()
        cloud = next(d for d in catalog["descriptors"] if d["id"] == "cloud_status")
        cloud["presentation"]["values"].append(dict(cloud["presentation"]["values"][0]))
        with self.assertRaises(preferences.PreferenceCatalogError):
            preferences.validate(catalog, document)

    def test_an_enum_default_absent_from_its_own_values_is_refused(self):
        catalog, document = self.build()
        cloud = next(d for d in catalog["descriptors"] if d["id"] == "cloud_status")
        cloud["presentation"]["default"] = "not-a-real-value"
        with self.assertRaises(preferences.PreferenceCatalogError):
            preferences.validate(catalog, document)

    def test_a_window_size_descriptor_missing_a_component_is_refused(self):
        catalog, document = self.build()
        window = next(d for d in catalog["descriptors"] if d["id"] == "window_size")
        window["fields"] = [f for f in window["fields"] if f["role"] != "height"]
        with self.assertRaises(preferences.PreferenceCatalogError):
            preferences.validate(catalog, document)

    def test_a_future_format_version_is_refused(self):
        catalog, document = self.build()
        catalog["format_version"] = preferences.FORMAT_VERSION + 1
        with self.assertRaises(preferences.PreferenceCatalogError):
            preferences.validate(catalog, document)

    def test_a_deliberately_unmanaged_setting_cannot_be_exposed(self):
        # Proves a generator regression that tried to expose one of the seven
        # unsafe settings would be caught, not just that the real generator
        # happens not to do it.
        catalog, document = self.build()
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
        with self.assertRaises(preferences.PreferenceCatalogError):
            preferences.validate(catalog, document)


class RealCanonicalShape(unittest.TestCase):
    """Checks the shape the plan describes against the dataset actually
    checked into the repository, so an unexplained drift is visible here
    rather than only inside the generated catalog."""

    @classmethod
    def setUpClass(cls):
        cls.dataset_document = json.loads(BUNDLED_DATASET.read_text(encoding="utf-8"))
        cls.catalog = preferences.build(cls.dataset_document)
        preferences.validate(cls.catalog, cls.dataset_document)

    def test_current_canonical_setting_count(self):
        self.assertEqual(len(self.dataset_document["current_canonical_setting_ids"]), 189)

    def test_exposed_and_excluded_accounting(self):
        exposed = {f["canonical_id"] for d in self.catalog["descriptors"] for f in d["fields"]}
        self.assertEqual(len(exposed), 182)
        self.assertEqual(len(self.catalog["exclusions"]), 7)

    def test_keybinding_count(self):
        keybindings = [d for d in self.catalog["descriptors"] if d["presentation"]["kind"] == "key_binding"]
        self.assertEqual(len(keybindings), 61)

    def test_sound_and_music_accounting(self):
        registry = {entry["id"]: entry for entry in self.dataset_document["setting_registry"]}
        current = set(self.dataset_document["current_canonical_setting_ids"])
        sound_current = [cid for cid in current if registry[cid]["category"] == "Sound & Music"]
        self.assertEqual(len(sound_current), 16)
        sound_manageable = [cid for cid in sound_current if registry[cid]["manageable"]]
        self.assertEqual(len(sound_manageable), 15)

    def test_the_seven_deliberate_exclusions_match_the_plan(self):
        self.assertEqual(
            {e["canonical_id"] for e in self.catalog["exclusions"]},
            {
                "incompatibleResourcePacks",
                "joinedFirstServer",
                "lastMpIp",
                "resourcePacks",
                "soundDevice",
                "startedCleanly",
                "tutorialStep",
            },
        )

    def test_regenerating_from_the_same_dataset_is_deterministic(self):
        again = preferences.build(json.loads(BUNDLED_DATASET.read_text(encoding="utf-8")))
        self.assertEqual(preferences.dumps(again), preferences.dumps(self.catalog))


if __name__ == "__main__":
    unittest.main()
