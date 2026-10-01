//! Parsing and validating a CurseForge export's `manifest.json`.
//!
//! CurseForge's published modpack export is authoritative here; this module is
//! a reader for it and nothing else. Everything it learns is turned into the
//! provider-neutral [`PackRecipe`] and [`PendingProviderFile`], so nothing
//! downstream ever sees a `modLoaders` array or a `projectID`.
//!
//! # What a CurseForge manifest does and does not say
//!
//! It names the pack, its version, its author, the exact Minecraft version, the
//! exact mod-loader build, an overrides directory, and a list of files by
//! *project id and file id*. It contains no file names, no download addresses,
//! no hashes, and no sizes: those come from the CurseForge API, which needs a
//! credential.
//!
//! That split is the reason parsing produces [`PendingProviderFile`] entries
//! rather than [`PackFileRecipe`] ones. A recipe entry names a destination path
//! and carries the hashes its download is verified against; inventing either
//! from a project id would be inventing a fact. So the archive is identified,
//! described, and counted offline - which is what lets the launcher say "this
//! is a CurseForge pack for Minecraft 1.20.1 with 42 required files, and it
//! needs an API key before those files can be resolved" instead of failing to
//! recognize the archive at all.
//!
//! # `required` is pack inclusion, not side support
//!
//! A CurseForge manifest's `required` flag says whether a file is part of the
//! pack as installed, and says nothing whatsoever about servers. It therefore
//! maps onto the *client* side - `required` files are the client's desired
//! state, `required: false` files are an optional pack choice - while the
//! server side stays [`SideRequirement::Unknown`] for every entry, because the
//! format states nothing about it. Reading `required: true` as "required on the
//! server" would put a client-only mod into a reconstructed server on the
//! strength of a flag that never meant that.
//!
//! # What is refused, and why refusal is the right answer
//!
//! An unsupported manifest revision, a manifest that is not a Minecraft
//! modpack, a missing or empty Minecraft version, a missing, unsupported, or
//! ambiguous loader, a zero or malformed project or file id, one file declared
//! both required and optional, and an overrides directory that does not name a
//! plain directory. Every one is a hard failure rather than a repair: a pack
//! quietly adjusted to import cleanly reproduces something other than the bug.
//!
//! One project declared with two *different* files is not refused here: a
//! pack updated without trimming a stale entry is common enough upstream that
//! failing the whole import over it would be worse than resolving both and
//! choosing (see [`crate::recipe::PendingProviderFile`] and ADR 0031). Both
//! ids still parse and both still travel through as their own entry; picking
//! between them needs the provider's own file metadata, which this module
//! never contacts.

use std::collections::BTreeMap;

use ephervex_model::{
    CurseForgeFileId, CurseForgeProjectId, ImportSourceKind, LoaderKind, LoaderSelection,
    ModProvider,
};
use serde::Deserialize;
use tracing::debug;

use crate::archive::PackArchive;
use crate::detect::CURSEFORGE_MANIFEST;
use crate::error::{PackError, PackResult};
use crate::path::safe_file_name;
use crate::recipe::{
    OverrideLayer, OverrideScope, PackRecipe, PendingProviderFile, ProviderFileIdentity,
    ProviderRecipeMetadata, RECIPE_VERSION, SideRequirement,
};

/// The `manifestVersion` this build implements.
pub const SUPPORTED_MANIFEST_VERSION: u64 = 1;

/// The `manifestType` a Minecraft modpack export declares.
///
/// An exact upstream value, matched case-insensitively because it is an
/// identifier rather than prose.
pub const MINECRAFT_MODPACK: &str = "minecraftModpack";

/// The overrides directory a CurseForge export uses when it names none.
pub const DEFAULT_OVERRIDES: &str = "overrides";

/// The loader families this build maps a `modLoaders` id onto.
///
/// The id is spelled `<family>-<build>`, so the family is compared whole rather
/// than by prefix: `neoforge` must not be read as a `forge` id that happens to
/// start with three other letters.
const SUPPORTED_LOADERS: &[(&str, LoaderKind)] = &[
    ("forge", LoaderKind::Forge),
    ("neoforge", LoaderKind::NeoForge),
];

/// What a parsed CurseForge manifest produced, before anything is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedExport {
    /// The normalized recipe. Its `files` are empty: a CurseForge manifest
    /// names no file the recipe could describe until the API has been asked.
    pub recipe: PackRecipe,
    /// The files the manifest declared, each waiting on the provider for its
    /// name, hashes, size, and address.
    pub pending: Vec<PendingProviderFile>,
    /// The manifest exactly as it was in the archive, so Ephervex retains it
    /// byte for byte rather than a re-serialized approximation.
    pub manifest: Vec<u8>,
}

/// Reads and validates the `manifest.json` inside an open archive.
pub fn read(archive: &mut PackArchive) -> PackResult<ParsedExport> {
    let manifest = archive.read_manifest(CURSEFORGE_MANIFEST)?;
    let archive_name = archive.file_name().map(ToOwned::to_owned);
    let mut parsed = parse(&manifest, archive_name)?;
    // The manifest names the directory; the archive decides whether it is
    // there. A pack whose whole content is declared files carries none.
    parsed
        .recipe
        .override_layers
        .retain(|layer| archive.contains_prefix(&layer.archive_prefix));
    Ok(parsed)
}

/// Parses the manifest document itself.
///
/// Split out from [`read`] so manifest validation is testable against a fixture
/// without building an archive around it.
pub fn parse(manifest: &[u8], archive_name: Option<String>) -> PackResult<ParsedExport> {
    let raw: RawManifest =
        serde_json::from_slice(manifest).map_err(|source| PackError::ManifestInvalid {
            entry: CURSEFORGE_MANIFEST.to_owned(),
            source,
        })?;

    if raw.manifest_version > SUPPORTED_MANIFEST_VERSION {
        return Err(PackError::ManifestUnsupportedVersion {
            entry: CURSEFORGE_MANIFEST,
            found: raw.manifest_version,
            supported: SUPPORTED_MANIFEST_VERSION,
        });
    }
    if !raw.manifest_type.eq_ignore_ascii_case(MINECRAFT_MODPACK) {
        return Err(PackError::ManifestTypeUnsupported {
            entry: CURSEFORGE_MANIFEST,
            found: raw.manifest_type,
        });
    }

    let minecraft = raw.minecraft.ok_or(PackError::MinecraftDependencyMissing)?;
    let minecraft_version = minecraft.version.trim().to_owned();
    if minecraft_version.is_empty() {
        return Err(PackError::MinecraftDependencyMissing);
    }
    let loader = resolve_loader(&minecraft.mod_loaders)?;
    let overrides = overrides_layer(raw.overrides.as_deref())?;
    let pending = pending_files(raw.files)?;

    let recipe = PackRecipe {
        recipe_version: RECIPE_VERSION,
        source_kind: ImportSourceKind::CurseForgePack,
        original_name: raw
            .name
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty())
            .or_else(|| archive_name.clone())
            .unwrap_or_else(|| "CurseForge pack".to_owned()),
        original_version: non_empty(raw.version),
        author: non_empty(raw.author),
        summary: None,
        archive_name,
        minecraft_version,
        loader,
        // Every declared file is still pending: see the module documentation.
        files: Vec::new(),
        override_layers: vec![overrides],
        provider_metadata: ProviderRecipeMetadata::new(ImportSourceKind::CurseForgePack, None),
    };

    debug!(
        pack = %recipe.original_name,
        minecraft = %recipe.minecraft_version,
        loader = %recipe.loader,
        declared = pending.len(),
        "parsed a CurseForge pack manifest"
    );
    Ok(ParsedExport {
        recipe,
        pending,
        manifest: manifest.to_vec(),
    })
}

/// A trimmed value, or nothing when the manifest supplied only whitespace.
fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// Maps the `modLoaders` array onto one exact loader selection.
///
/// The exact declared build is preserved verbatim. A NeoForge id carries a
/// NeoForge build number, which looks like a Minecraft version and is not one;
/// nothing here derives one from the other.
fn resolve_loader(loaders: &[RawModLoader]) -> PackResult<LoaderSelection> {
    // A pack that declares no loader at all is a vanilla pack, which the export
    // format permits and this launcher supports.
    if loaders.is_empty() {
        return Ok(LoaderSelection::vanilla());
    }

    let primary: Vec<&RawModLoader> = loaders.iter().filter(|loader| loader.primary).collect();
    let chosen = match (primary.len(), loaders.len()) {
        (1, _) => primary[0],
        // No entry claims to be primary, and there is exactly one, so there is
        // nothing to choose between.
        (0, 1) => &loaders[0],
        // Either nothing is primary and several are declared, or several claim
        // to be. Both are ambiguous, and picking one would be inventing an
        // interpretation the format does not have.
        _ => {
            return Err(PackError::LoaderAmbiguous {
                first: loaders[0].id.trim().to_owned(),
                second: loaders[1].id.trim().to_owned(),
            });
        }
    };

    let id = chosen.id.trim();
    let (family, build) = id.split_once('-').ok_or_else(|| {
        // Not `<family>-<build>`, so there is no build to install exactly.
        PackError::LoaderUnsupported {
            loader: id.to_owned(),
        }
    })?;
    let Some((_, kind)) = SUPPORTED_LOADERS
        .iter()
        .find(|(name, _)| family.eq_ignore_ascii_case(name))
        .copied()
    else {
        // Fabric, Quilt, or something this build has never heard of. Refused by
        // name so the failure says which loader the pack needs.
        return Err(PackError::LoaderUnsupported {
            loader: family.to_owned(),
        });
    };
    if build.trim().is_empty() {
        return Err(PackError::LoaderUnsupported {
            loader: id.to_owned(),
        });
    }
    Ok(LoaderSelection::exact(kind, build.trim()))
}

/// The single override layer a CurseForge export carries.
///
/// The format has one overrides directory and no side-specific counterpart, so
/// it becomes the common layer - the same scope Modrinth's own `overrides`
/// takes. Reading it as client-only would drop the pack's configs out of a
/// server reconstructed later; reading a side out of the paths inside it
/// (`config/`, `mods/`, `defaultconfigs/`) would be a guess.
///
/// The directory name comes from the manifest and is untrusted: it is reduced
/// to one safe path component, so it can never name a parent directory, an
/// absolute path, or launcher-owned metadata.
fn overrides_layer(declared: Option<&str>) -> PackResult<OverrideLayer> {
    let declared = declared.map_or(DEFAULT_OVERRIDES, str::trim);
    let name = if declared.is_empty() {
        DEFAULT_OVERRIDES.to_owned()
    } else {
        safe_file_name(declared)
            .and_then(|name| name.to_str().map(ToOwned::to_owned))
            .ok_or_else(|| PackError::EntryPathUnsafe {
                entry: declared.to_owned(),
            })?
    };
    Ok(OverrideLayer {
        scope: OverrideScope::Common,
        archive_prefix: name,
        retained_at: None,
        files: 0,
    })
}

/// Validates the declared file list and normalizes it.
///
/// Two entries naming the same project *and* the same file with the same
/// inclusion are one declaration written twice, and are collapsed: the recipe's
/// desired state has one entry per file, and refusing an exporter's harmless
/// repetition would reject packs that install perfectly well. Two entries
/// disagreeing about whether that one file is required is a genuine
/// contradiction and is refused, because there is no honest way to install a
/// file that is both compulsory and optional.
///
/// Two entries naming the same project with *different* files both survive as
/// their own [`PendingProviderFile`], instead: resolving which one the pack
/// actually means needs the provider's own metadata for each, which is a
/// decision left to whatever resolves these entries against the API (see ADR
/// 0031), not to this offline parser.
fn pending_files(files: Vec<RawFile>) -> PackResult<Vec<PendingProviderFile>> {
    let mut by_pair: BTreeMap<(u32, u32), bool> = BTreeMap::new();
    let mut pending = Vec::with_capacity(files.len());

    for file in files {
        let project = CurseForgeProjectId::new(file.project_id).ok_or_else(|| {
            PackError::PackEntryIdInvalid {
                field: "projectID",
                value: file.project_id.to_string(),
            }
        })?;
        let file_id =
            CurseForgeFileId::new(file.file_id).ok_or_else(|| PackError::PackEntryIdInvalid {
                field: "fileID",
                value: file.file_id.to_string(),
            })?;

        let pair = (project.get(), file_id.get());
        if let Some(declared_required) = by_pair.get(&pair) {
            if *declared_required != file.required {
                return Err(PackError::PackEntryInclusionConflict {
                    project_id: project.to_string(),
                    file_id: file_id.to_string(),
                });
            }
            // The same file declared twice the same way: one entry, not two.
            continue;
        }
        by_pair.insert(pair, file.required);

        pending.push(PendingProviderFile {
            identity: ProviderFileIdentity {
                provider: Some(ModProvider::CurseForge),
                project_id: Some(project.to_string()),
                // CurseForge's release identifier *is* its file id, so both
                // fields carry it rather than one of them carrying an invented
                // semantic version parsed out of a display name.
                version_id: Some(file_id.to_string()),
                file_id: Some(file_id.to_string()),
            },
            // Pack inclusion, mapped onto the side it is a statement about.
            client: if file.required {
                SideRequirement::Required
            } else {
                SideRequirement::Optional
            },
            // The export format states nothing about servers, and an absence of
            // evidence is not evidence of support.
            server: SideRequirement::Unknown,
        });
    }
    Ok(pending)
}

/// `manifest.json` as the CurseForge export format defines it.
///
/// Only the members this launcher reads are declared. `serde` ignores the rest,
/// so a shape that grows upstream cannot break an import.
/// `manifestVersion` and `manifestType` have no defaults. They are what makes a
/// document a CurseForge export rather than any other file called
/// `manifest.json`, and assuming them would let an unrelated archive that
/// happens to carry one be read as a modpack.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawManifest {
    manifest_version: u64,
    manifest_type: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    minecraft: Option<RawMinecraft>,
    #[serde(default)]
    files: Vec<RawFile>,
    #[serde(default)]
    overrides: Option<String>,
}

/// The `minecraft` object: the exact game version and the loaders it runs.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawMinecraft {
    version: String,
    #[serde(default)]
    mod_loaders: Vec<RawModLoader>,
}

/// One `modLoaders` entry, spelled `<family>-<build>`.
#[derive(Debug, Deserialize)]
struct RawModLoader {
    id: String,
    #[serde(default)]
    primary: bool,
}

/// One declared file, named only by ids.
///
/// `required` is optional in the export format, and an absent value means the
/// same as `required: true`: the file is part of the pack as installed. Only
/// an explicit `false` marks it as an optional pack choice.
#[derive(Debug, Deserialize)]
struct RawFile {
    #[serde(rename = "projectID")]
    project_id: u32,
    #[serde(rename = "fileID")]
    file_id: u32,
    #[serde(default = "default_required")]
    required: bool,
}

/// CurseForge's own default for an omitted `required` flag: included.
const fn default_required() -> bool {
    true
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn manifest(minecraft: &str, files: &str) -> String {
        format!(
            r#"{{
                "minecraft": {minecraft},
                "manifestType": "minecraftModpack",
                "manifestVersion": 1,
                "name": "Test Pack",
                "version": "1.2.3",
                "author": "A Reporter",
                "files": [{files}],
                "overrides": "overrides"
            }}"#
        )
    }

    fn parse_manifest(document: &str) -> PackResult<ParsedExport> {
        parse(document.as_bytes(), Some("report.zip".to_owned()))
    }

    const FORGE: &str =
        r#"{"version": "1.20.1", "modLoaders": [{"id": "forge-47.4.10", "primary": true}]}"#;

    #[test]
    fn a_forge_export_becomes_an_exact_target_and_keeps_the_packs_own_facts() {
        let parsed = parse_manifest(&manifest(
            FORGE,
            r#"{"projectID": 238222, "fileID": 4593548, "required": true}"#,
        ))
        .expect("a valid export");

        assert_eq!(parsed.recipe.minecraft_version, "1.20.1");
        assert_eq!(
            parsed.recipe.loader,
            LoaderSelection::exact(LoaderKind::Forge, "47.4.10")
        );
        assert_eq!(parsed.recipe.original_name, "Test Pack");
        assert_eq!(parsed.recipe.original_version.as_deref(), Some("1.2.3"));
        assert_eq!(parsed.recipe.author.as_deref(), Some("A Reporter"));
        assert_eq!(parsed.recipe.source_kind, ImportSourceKind::CurseForgePack);
        assert!(
            parsed.recipe.files.is_empty(),
            "a manifest names no file the recipe can describe yet"
        );
    }

    #[test]
    fn a_neoforge_build_is_preserved_exactly_and_is_not_read_as_a_minecraft_version() {
        // A NeoForge build number is shaped like a Minecraft version and is not
        // one. Both must survive the parse as they were written.
        let parsed = parse_manifest(&manifest(
            r#"{"version": "1.20.4", "modLoaders": [{"id": "neoforge-20.4.190", "primary": true}]}"#,
            "",
        ))
        .expect("a valid export");
        assert_eq!(parsed.recipe.minecraft_version, "1.20.4");
        assert_eq!(
            parsed.recipe.loader,
            LoaderSelection::exact(LoaderKind::NeoForge, "20.4.190")
        );
    }

    #[test]
    fn a_forge_id_and_a_neoforge_id_are_not_confused_for_one_another() {
        for (id, kind, build) in [
            ("forge-14.23.5.2860", LoaderKind::Forge, "14.23.5.2860"),
            ("neoforge-21.1.72", LoaderKind::NeoForge, "21.1.72"),
            ("NeoForge-21.1.72", LoaderKind::NeoForge, "21.1.72"),
        ] {
            let parsed = parse_manifest(&manifest(
                &format!(r#"{{"version": "1.20.1", "modLoaders": [{{"id": "{id}"}}]}}"#),
                "",
            ))
            .expect("a valid export");
            assert_eq!(parsed.recipe.loader, LoaderSelection::exact(kind, build));
        }
    }

    #[test]
    fn a_pack_declaring_no_loader_is_vanilla() {
        let parsed = parse_manifest(&manifest(r#"{"version": "1.20.1"}"#, "")).expect("valid");
        assert_eq!(parsed.recipe.loader, LoaderSelection::vanilla());
    }

    #[test]
    fn fabric_and_quilt_are_refused_by_name_rather_than_ignored() {
        for (id, named) in [
            ("fabric-0.15.7", "fabric"),
            ("quilt-0.19.2", "quilt"),
            ("liteloader-1.2.3", "liteloader"),
        ] {
            let error = parse_manifest(&manifest(
                &format!(
                    r#"{{"version": "1.20.1", "modLoaders": [{{"id": "{id}", "primary": true}}]}}"#
                ),
                "",
            ))
            .expect_err("must refuse");
            assert_eq!(error.unsupported_loader(), Some(named));
            assert_eq!(error.message_id(), "error-pack-loader-unsupported");
        }
    }

    #[test]
    fn a_loader_id_with_no_build_is_refused_rather_than_resolved_to_a_latest() {
        for id in ["forge", "neoforge-", "  "] {
            let error = parse_manifest(&manifest(
                &format!(
                    r#"{{"version": "1.20.1", "modLoaders": [{{"id": "{id}", "primary": true}}]}}"#
                ),
                "",
            ))
            .expect_err("must refuse");
            assert!(
                matches!(error, PackError::LoaderUnsupported { .. }),
                "`{id}`: {error:?}"
            );
        }
    }

    #[test]
    fn two_loaders_with_no_primary_are_ambiguous_rather_than_a_guess() {
        let error = parse_manifest(&manifest(
            r#"{"version": "1.20.1", "modLoaders": [{"id": "forge-47.4.10"}, {"id": "neoforge-20.4.190"}]}"#,
            "",
        ))
        .expect_err("must refuse");
        assert!(matches!(error, PackError::LoaderAmbiguous { .. }));
    }

    #[test]
    fn two_loaders_both_claiming_to_be_primary_are_ambiguous_too() {
        let error = parse_manifest(&manifest(
            r#"{"version": "1.20.1", "modLoaders": [{"id": "forge-47.4.10", "primary": true}, {"id": "neoforge-20.4.190", "primary": true}]}"#,
            "",
        ))
        .expect_err("must refuse");
        assert!(matches!(error, PackError::LoaderAmbiguous { .. }));
    }

    #[test]
    fn a_secondary_loader_beside_one_primary_does_not_change_the_target() {
        let parsed = parse_manifest(&manifest(
            r#"{"version": "1.20.1", "modLoaders": [{"id": "forge-47.4.10", "primary": true}, {"id": "neoforge-20.4.190"}]}"#,
            "",
        ))
        .expect("the primary entry decides");
        assert_eq!(
            parsed.recipe.loader,
            LoaderSelection::exact(LoaderKind::Forge, "47.4.10")
        );
    }

    #[test]
    fn a_manifest_with_no_minecraft_section_is_refused() {
        let document = r#"{"manifestType": "minecraftModpack", "manifestVersion": 1, "files": []}"#;
        let error = parse_manifest(document).expect_err("must refuse");
        assert!(matches!(error, PackError::MinecraftDependencyMissing));
    }

    #[test]
    fn an_empty_minecraft_version_is_refused() {
        let error =
            parse_manifest(&manifest(r#"{"version": "   "}"#, "")).expect_err("must refuse");
        assert!(matches!(error, PackError::MinecraftDependencyMissing));
    }

    #[test]
    fn a_newer_manifest_revision_is_refused_rather_than_guessed_at() {
        let document = r#"{"manifestType": "minecraftModpack", "manifestVersion": 2,
            "minecraft": {"version": "1.20.1"}, "files": []}"#;
        let error = parse_manifest(document).expect_err("must refuse");
        assert!(matches!(
            error,
            PackError::ManifestUnsupportedVersion {
                found: 2,
                supported: 1,
                ..
            }
        ));
    }

    #[test]
    fn an_unsupported_manifest_revision_is_distinguishable_from_malformed_json() {
        let malformed = parse_manifest("{not json").expect_err("must refuse");
        assert!(matches!(
            malformed,
            PackError::ManifestInvalid { ref entry, .. } if entry == "manifest.json"
        ));

        let revision = parse_manifest(
            r#"{"manifestType": "minecraftModpack", "manifestVersion": 9,
                "minecraft": {"version": "1.20.1"}, "files": []}"#,
        )
        .expect_err("must refuse");
        assert!(matches!(
            revision,
            PackError::ManifestUnsupportedVersion { .. }
        ));
        assert_ne!(malformed.message_id(), revision.message_id());
    }

    #[test]
    fn an_export_that_is_not_a_minecraft_modpack_is_refused() {
        let document = r#"{"manifestType": "wowAddonPack", "manifestVersion": 1,
            "minecraft": {"version": "1.20.1"}, "files": []}"#;
        let error = parse_manifest(document).expect_err("must refuse");
        assert!(matches!(
            error,
            PackError::ManifestTypeUnsupported { ref found, .. } if found == "wowAddonPack"
        ));
    }

    #[test]
    fn an_unsupported_loader_is_distinguishable_from_a_malformed_manifest() {
        let loader = parse_manifest(&manifest(
            r#"{"version": "1.20.1", "modLoaders": [{"id": "fabric-0.15.7", "primary": true}]}"#,
            "",
        ))
        .expect_err("must refuse");
        assert_eq!(loader.message_id(), "error-pack-loader-unsupported");

        let malformed = parse_manifest("{").expect_err("must refuse");
        assert_eq!(malformed.message_id(), "error-pack-manifest-invalid");
    }

    // -- declared files ----------------------------------------------------

    #[test]
    fn the_exact_project_and_file_ids_survive_into_the_pending_entry() {
        let parsed = parse_manifest(&manifest(
            FORGE,
            r#"{"projectID": 238222, "fileID": 4593548, "required": true}"#,
        ))
        .expect("valid");

        assert_eq!(parsed.pending.len(), 1);
        let identity = &parsed.pending[0].identity;
        assert_eq!(identity.provider, Some(ModProvider::CurseForge));
        assert_eq!(identity.project_id.as_deref(), Some("238222"));
        assert_eq!(
            identity.version_id.as_deref(),
            Some("4593548"),
            "the release identifier is the file id, never an invented version"
        );
        assert_eq!(identity.file_id.as_deref(), Some("4593548"));
    }

    #[test]
    fn pack_inclusion_becomes_a_client_decision_and_never_a_server_claim() {
        let parsed = parse_manifest(&manifest(
            FORGE,
            r#"{"projectID": 238222, "fileID": 1, "required": true},
               {"projectID": 238223, "fileID": 2, "required": false}"#,
        ))
        .expect("valid");

        assert_eq!(parsed.pending[0].client, SideRequirement::Required);
        assert_eq!(parsed.pending[1].client, SideRequirement::Optional);
        for entry in &parsed.pending {
            assert_eq!(
                entry.server,
                SideRequirement::Unknown,
                "the export format states nothing about servers"
            );
        }
    }

    #[test]
    fn a_zero_project_or_file_id_is_refused_rather_than_coerced() {
        for (files, field) in [
            (
                r#"{"projectID": 0, "fileID": 4593548, "required": true}"#,
                "projectID",
            ),
            (
                r#"{"projectID": 238222, "fileID": 0, "required": true}"#,
                "fileID",
            ),
        ] {
            let error = parse_manifest(&manifest(FORGE, files)).expect_err("must refuse");
            assert!(
                matches!(error, PackError::PackEntryIdInvalid { field: found, .. } if found == field),
                "{error:?}"
            );
        }
    }

    #[test]
    fn a_negative_or_oversized_id_is_a_malformed_manifest() {
        for files in [
            r#"{"projectID": -1, "fileID": 1, "required": true}"#,
            r#"{"projectID": 1, "fileID": 99999999999999, "required": true}"#,
            r#"{"projectID": "238222", "fileID": 1, "required": true}"#,
        ] {
            let error = parse_manifest(&manifest(FORGE, files)).expect_err("must refuse");
            assert!(
                matches!(error, PackError::ManifestInvalid { .. }),
                "{error:?}"
            );
        }
    }

    #[test]
    fn an_entry_with_no_inclusion_flag_defaults_to_required_and_included() {
        let parsed = parse_manifest(&manifest(FORGE, r#"{"projectID": 1, "fileID": 2}"#))
            .expect("an omitted `required` flag defaults to included, per the export format");
        assert_eq!(parsed.pending.len(), 1);
        assert_eq!(
            parsed.pending[0].client,
            SideRequirement::Required,
            "an omitted flag means the same as `required: true`"
        );
        assert_eq!(
            parsed.pending[0].server,
            SideRequirement::Unknown,
            "inclusion says nothing about server support"
        );
    }

    #[test]
    fn an_omitted_flag_and_an_explicit_false_stay_distinguishable_in_one_manifest() {
        let parsed = parse_manifest(&manifest(
            FORGE,
            r#"{"projectID": 1, "fileID": 2},
               {"projectID": 3, "fileID": 4, "required": false}"#,
        ))
        .expect("valid");
        assert_eq!(parsed.pending.len(), 2);
        assert_eq!(
            parsed.pending[0].client,
            SideRequirement::Required,
            "the entry with no flag is included"
        );
        assert_eq!(
            parsed.pending[1].client,
            SideRequirement::Optional,
            "the entry with an explicit `false` stays optional"
        );
    }

    #[test]
    fn the_same_file_declared_twice_identically_produces_one_entry() {
        // An exporter that wrote one declaration twice has described one file.
        // Two entries would ask the API for it twice and record it twice.
        let parsed = parse_manifest(&manifest(
            FORGE,
            r#"{"projectID": 238222, "fileID": 4593548, "required": true},
               {"projectID": 238222, "fileID": 4593548, "required": true}"#,
        ))
        .expect("a repeated declaration is one file");
        assert_eq!(parsed.pending.len(), 1);
    }

    #[test]
    fn one_project_declared_with_two_different_files_produces_two_entries() {
        // A pack updated without trimming a stale entry, or a manifest that
        // simply changed its mind mid-export: both files parse, and picking
        // between them is left to whatever resolves these against the API
        // (ADR 0031), not to this offline parser.
        let parsed = parse_manifest(&manifest(
            FORGE,
            r#"{"projectID": 238222, "fileID": 1, "required": true},
               {"projectID": 238222, "fileID": 2, "required": true}"#,
        ))
        .expect("two different files for one project both parse");
        assert_eq!(parsed.pending.len(), 2);
        assert_eq!(parsed.pending[0].identity.file_id.as_deref(), Some("1"));
        assert_eq!(parsed.pending[1].identity.file_id.as_deref(), Some("2"));
    }

    #[test]
    fn one_file_declared_both_required_and_optional_is_refused() {
        let error = parse_manifest(&manifest(
            FORGE,
            r#"{"projectID": 238222, "fileID": 1, "required": true},
               {"projectID": 238222, "fileID": 1, "required": false}"#,
        ))
        .expect_err("must refuse");
        assert!(matches!(
            error,
            PackError::PackEntryInclusionConflict { ref project_id, ref file_id }
                if project_id == "238222" && file_id == "1"
        ));
    }

    // -- overrides ---------------------------------------------------------

    #[test]
    fn the_overrides_directory_becomes_the_common_layer() {
        let parsed = parse_manifest(&manifest(FORGE, "")).expect("valid");
        assert_eq!(parsed.recipe.override_layers.len(), 1);
        let layer = &parsed.recipe.override_layers[0];
        assert_eq!(layer.scope, OverrideScope::Common);
        assert_eq!(layer.archive_prefix, "overrides");
    }

    #[test]
    fn a_manifest_naming_no_overrides_directory_still_gets_the_default_one() {
        let document = r#"{"manifestType": "minecraftModpack", "manifestVersion": 1,
            "minecraft": {"version": "1.20.1"}, "files": []}"#;
        let parsed = parse_manifest(document).expect("valid");
        assert_eq!(parsed.recipe.override_layers[0].archive_prefix, "overrides");
    }

    #[test]
    fn a_document_that_does_not_declare_itself_a_curseforge_export_is_refused() {
        // Plenty of archives contain a file called `manifest.json`. Only one
        // that says what kind of manifest it is has been proven to be this one.
        for document in [
            r#"{"minecraft": {"version": "1.20.1"}, "files": []}"#,
            r#"{"manifestType": "minecraftModpack", "minecraft": {"version": "1.20.1"}}"#,
            r#"{"manifestVersion": 1, "minecraft": {"version": "1.20.1"}}"#,
        ] {
            let error = parse_manifest(document).expect_err("must refuse");
            assert!(
                matches!(error, PackError::ManifestInvalid { .. }),
                "{document}: {error:?}"
            );
        }
    }

    #[test]
    fn an_overrides_directory_that_climbs_out_is_refused() {
        for declared in ["../../etc", "/etc", "..", "over/../../rides"] {
            let document = format!(
                r#"{{"manifestType": "minecraftModpack", "manifestVersion": 1,
                    "minecraft": {{"version": "1.20.1"}}, "files": [], "overrides": "{declared}"}}"#
            );
            let error = parse_manifest(&document).expect_err("must refuse");
            assert!(
                matches!(error, PackError::EntryPathUnsafe { .. }),
                "`{declared}`: {error:?}"
            );
        }
    }

    #[test]
    fn a_custom_overrides_directory_is_honored() {
        let document = r#"{"manifestType": "minecraftModpack", "manifestVersion": 1,
            "minecraft": {"version": "1.20.1"}, "files": [], "overrides": "pack-files"}"#;
        let parsed = parse_manifest(document).expect("valid");
        assert_eq!(
            parsed.recipe.override_layers[0].archive_prefix,
            "pack-files"
        );
    }

    #[test]
    fn the_manifest_is_kept_exactly_as_it_arrived() {
        let document = manifest(FORGE, "");
        let parsed = parse_manifest(&document).expect("valid");
        assert_eq!(parsed.manifest, document.as_bytes());
    }
}
