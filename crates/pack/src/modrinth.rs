//! Parsing and validating `modrinth.index.json`.
//!
//! Modrinth's current modpack format specification is authoritative here; this
//! module is a reader for it and nothing else. Everything it learns is
//! immediately turned into the provider-neutral [`PackRecipe`], so the rest of
//! the launcher never sees a `dependencies` map or an `env` object.
//!
//! # What is validated, and why refusal is the right answer
//!
//! Validation covers the ZIP structure, the required top-level fields
//! (`formatVersion`, `game`, `versionId`, `name`), the Minecraft dependency, the
//! loader dependencies, file paths, hashes, download URL scheme and domain, and
//! the environment metadata. A file must carry both `sha1` and `sha512`, as the
//! format requires. Every one of them is a hard failure rather than a repair,
//! because this is a *reproduction* launcher: a pack that has been quietly
//! adjusted to import cleanly reproduces something other than the bug.
//!
//! Two of the refusals are worth naming:
//!
//! * **Fabric and Quilt.** They are unsupported and must not be silently
//!   ignored. The error names the loader
//!   so the instance can carry `Needs attention - <loader> is not supported`
//!   rather than a generic failure.
//! * **A pack declaring both Forge and NeoForge.** This is ambiguous: the two
//!   are different loaders with different install semantics, and
//!   picking one would be inventing an interpretation the format does not have.

use std::collections::BTreeMap;
use std::num::NonZeroU64;

use ephervex_download::DomainAllowlist;
use ephervex_model::{
    Hash, HashAlgorithm, ImportSourceKind, LoaderKind, LoaderSelection, ModOrigin,
};
use serde::Deserialize;
use tracing::{debug, warn};

use crate::archive::PackArchive;
use crate::declared_files::require_declared_files_within;
use crate::detect::MODRINTH_INDEX;
use crate::error::{PackError, PackResult};
use crate::path::{is_reserved, safe_relative_path};
use crate::recipe::{
    OverrideLayer, OverrideScope, PackFileRecipe, PackRecipe, ProviderFileIdentity,
    ProviderRecipeMetadata, RECIPE_VERSION, SideRequirement,
};

/// The `.mrpack` revision this build implements.
pub const SUPPORTED_FORMAT_VERSION: u64 = 1;

/// The game a pack must declare.
const SUPPORTED_GAME: &str = "minecraft";

/// The dependency key naming the Minecraft version.
const MINECRAFT_DEPENDENCY: &str = "minecraft";

/// The archive directory holding files that apply to both sides.
pub const COMMON_OVERRIDES: &str = "overrides";
/// The archive directory holding client-only files.
pub const CLIENT_OVERRIDES: &str = "client-overrides";
/// The archive directory holding server-only files.
pub const SERVER_OVERRIDES: &str = "server-overrides";

/// The dependency keys this build maps to a supported loader.
const SUPPORTED_LOADERS: &[(&str, LoaderKind)] = &[
    ("forge", LoaderKind::Forge),
    ("neoforge", LoaderKind::NeoForge),
];

/// What a parsed index produced, before anything is written to disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedPack {
    /// The normalized recipe.
    pub recipe: PackRecipe,
    /// The index exactly as it was in the archive, so Ephervex can retain it byte
    /// for byte rather than a re-serialized approximation.
    pub manifest: Vec<u8>,
    /// Download addresses that were refused by the URL policy but whose file
    /// had another permitted address. Worth reporting rather than hiding: a
    /// pack pointing half its files at an unlisted mirror is a fact about the
    /// pack.
    pub rejected_urls: Vec<String>,
}

/// Reads and validates the `modrinth.index.json` inside an open archive.
pub fn read(archive: &mut PackArchive, allowlist: &DomainAllowlist) -> PackResult<ParsedPack> {
    let manifest = archive.read_manifest(MODRINTH_INDEX)?;
    let archive_name = archive.file_name().map(ToOwned::to_owned);
    let layers = present_layers(archive);
    let mut parsed = parse(
        &manifest,
        allowlist,
        archive_name,
        archive.limits().max_declared_files,
    )?;
    parsed.recipe.override_layers = layers;
    Ok(parsed)
}

/// Which override layers the archive actually carries.
///
/// Recorded from the archive rather than from the index, because the index does
/// not list them: the required layering is defined by directory presence.
fn present_layers(archive: &PackArchive) -> Vec<OverrideLayer> {
    [
        (OverrideScope::Common, COMMON_OVERRIDES),
        (OverrideScope::Client, CLIENT_OVERRIDES),
        (OverrideScope::Server, SERVER_OVERRIDES),
    ]
    .into_iter()
    .filter(|(_, prefix)| archive.contains_layer(prefix))
    .map(|(scope, prefix)| OverrideLayer {
        scope,
        archive_prefix: prefix.to_owned(),
        retained_at: None,
        files: 0,
    })
    .collect()
}

/// Parses the index document itself.
///
/// Split out from [`read`] so manifest validation is testable against a fixture
/// without building an archive around it. A manifest declaring more than
/// `max_declared_files` files is refused.
pub fn parse(
    manifest: &[u8],
    allowlist: &DomainAllowlist,
    archive_name: Option<String>,
    max_declared_files: u64,
) -> PackResult<ParsedPack> {
    require_declared_files_within(MODRINTH_INDEX, manifest, max_declared_files)?;
    let index: RawIndex =
        serde_json::from_slice(manifest).map_err(|source| PackError::ManifestInvalid {
            entry: MODRINTH_INDEX.to_owned(),
            source,
        })?;

    let format_version = index.format_version.get();
    if format_version > SUPPORTED_FORMAT_VERSION {
        return Err(PackError::ManifestUnsupportedVersion {
            entry: MODRINTH_INDEX,
            found: format_version,
            supported: SUPPORTED_FORMAT_VERSION,
        });
    }
    if !index.game.eq_ignore_ascii_case(SUPPORTED_GAME) {
        return Err(PackError::GameUnsupported { game: index.game });
    }

    let minecraft_version = index
        .dependencies
        .get(MINECRAFT_DEPENDENCY)
        .map(|version| version.trim().to_owned())
        .filter(|version| !version.is_empty())
        .ok_or(PackError::MinecraftDependencyMissing)?;
    let loader = resolve_loader(&index.dependencies)?;

    let mut files = Vec::with_capacity(index.files.len());
    let mut rejected_urls = Vec::new();
    for file in index.files {
        files.push(normalize_file(file, allowlist, &mut rejected_urls)?);
    }

    let recipe = PackRecipe {
        recipe_version: RECIPE_VERSION,
        source_kind: ImportSourceKind::ModrinthPack,
        original_name: index.name,
        original_version: Some(index.version_id),
        author: None,
        summary: index.summary,
        archive_name,
        minecraft_version,
        loader,
        files,
        override_layers: Vec::new(),
        provider_metadata: ProviderRecipeMetadata::new(ImportSourceKind::ModrinthPack, None),
    };

    debug!(
        pack = %recipe.original_name,
        minecraft = %recipe.minecraft_version,
        loader = %recipe.loader,
        files = recipe.files.len(),
        "parsed a Modrinth pack index"
    );
    Ok(ParsedPack {
        recipe,
        manifest: manifest.to_vec(),
        rejected_urls,
    })
}

/// Maps the `dependencies` map onto one supported loader.
fn resolve_loader(dependencies: &BTreeMap<String, String>) -> PackResult<LoaderSelection> {
    let mut chosen: Option<(&str, LoaderKind, String)> = None;

    for (key, version) in dependencies {
        if key.eq_ignore_ascii_case(MINECRAFT_DEPENDENCY) {
            continue;
        }
        let Some((name, kind)) = SUPPORTED_LOADERS
            .iter()
            .find(|(name, _)| key.eq_ignore_ascii_case(name))
            .copied()
        else {
            // Fabric, Quilt, or something this build has never heard of. Both
            // are refusals: an unsupported loader must not be silently ignored,
            // and an unknown key cannot be assumed harmless because a pack that
            // needs it will not run without it.
            return Err(PackError::LoaderUnsupported {
                loader: key.clone(),
            });
        };

        if let Some((first, _, _)) = chosen {
            return Err(PackError::LoaderAmbiguous {
                first: first.to_owned(),
                second: name.to_owned(),
            });
        }
        chosen = Some((name, kind, version.trim().to_owned()));
    }

    match chosen {
        // the exact declared build, never a floating "latest".
        Some((_, kind, version)) if !version.is_empty() => {
            Ok(LoaderSelection::exact(kind, version))
        }
        Some((name, _, _)) => Err(PackError::LoaderUnsupported {
            loader: format!("{name} (no version)"),
        }),
        None => Ok(LoaderSelection::vanilla()),
    }
}

/// Validates one file entry and normalizes it.
fn normalize_file(
    file: RawFile,
    allowlist: &DomainAllowlist,
    rejected_urls: &mut Vec<String>,
) -> PackResult<PackFileRecipe> {
    let declared_path = file.path.clone();
    let Some(logical_path) = safe_relative_path(file.path.as_bytes(), 0) else {
        return Err(PackError::EntryPathUnsafe {
            entry: declared_path,
        });
    };
    if logical_path.as_os_str().is_empty() {
        return Err(PackError::EntryPathUnsafe {
            entry: declared_path,
        });
    }
    if is_reserved(&logical_path) {
        return Err(PackError::EntryReserved {
            entry: declared_path,
        });
    }

    let Some(hashes) = normalize_hashes(&file.hashes) else {
        // Verification is mandatory, and the format requires both digests, so
        // a file missing either one (or carrying a malformed one) is a file
        // this launcher will not fetch.
        return Err(PackError::FileHashesMissing {
            path: declared_path,
        });
    };

    if file.downloads.is_empty() {
        return Err(PackError::FileDownloadsMissing {
            path: declared_path,
        });
    }
    let mut downloads = Vec::new();
    let mut last_rejection = None;
    for raw in &file.downloads {
        match allowlist.validate(raw) {
            Ok(url) => downloads.push(url.to_string()),
            Err(error) => {
                warn!(
                    file = %declared_path,
                    %error,
                    "refused a download address from pack metadata"
                );
                rejected_urls.push(redacted_address(raw));
                last_rejection = Some(error);
            }
        }
    }
    if downloads.is_empty() {
        return Err(PackError::DownloadUrlRejected {
            path: declared_path,
            // Every address was refused; the last refusal is as good an
            // explanation as the first and is the one the log ends on.
            source: last_rejection
                .unwrap_or(ephervex_download::UrlPolicyError::Malformed { url: String::new() }),
        });
    }

    let environment = file.env.unwrap_or_default();
    let identity = provider_identity(&downloads);
    Ok(PackFileRecipe {
        logical_path,
        hashes,
        downloads,
        size: file.file_size,
        client: environment.client.into(),
        server: environment.server.into(),
        selected: None,
        // The pack asked for this file; nobody browsed for it.
        origin: ModOrigin::PackDeclared,
        identity,
        original_identity: None,
        // A Modrinth index names a path, hashes and addresses. It declares no
        // relations, so nothing has asked rather than nothing is needed.
        relations: None,
    })
}

/// The catalog identity a pack's own download addresses prove, if any.
///
/// A `modrinth.index.json` has no project or version fields: the format
/// identifies a file by path, hashes, and download address. The address is
/// nevertheless authoritative provider data, because Modrinth's own CDN lays
/// its artifacts out as
/// `https://cdn.modrinth.com/data/<project>/versions/<version>/<file>`, and the
/// pack tooling writes those addresses from the catalog entries it resolved.
/// Reading the ids back out is therefore reconstruction, not inference - unlike
/// a file name, which proves nothing and is never consulted here.
///
/// Two restrictions keep it that way:
///
/// * only an address on Modrinth's own CDN counts. A pack may legitimately host
///   a file on one of the other permitted mirrors, and a `github.com` URL says
///   nothing about a Modrinth project;
/// * the version segment is accepted only when it is shaped like a Modrinth id.
///   Older packs put a human version *number* there, and recording "1.2.3" as a
///   version id would produce a record nothing can look up. Those files keep
///   their project and stay honestly version-less.
#[must_use]
pub fn provider_identity(downloads: &[String]) -> Option<ProviderFileIdentity> {
    downloads.iter().find_map(|address| {
        let url = ephervex_download::Url::parse(address.trim()).ok()?;
        if url.host_str() != Some(MODRINTH_CDN_HOST) {
            return None;
        }

        let mut segments = url.path_segments()?;
        if segments.next()? != "data" {
            return None;
        }
        let project_id = catalog_id(segments.next()?)?.to_owned();
        let version_id = (segments.next() == Some("versions"))
            .then(|| segments.next().and_then(catalog_id).map(ToOwned::to_owned))
            .flatten();

        Some(ProviderFileIdentity {
            provider: Some(ephervex_model::ModProvider::Modrinth),
            project_id: Some(project_id),
            version_id,
            // Modrinth has no file id distinct from the version id, and
            // duplicating the version into this field would invent a second
            // identifier for the same thing.
            file_id: None,
        })
    })
}

/// The host whose layout the reconstruction above is defined against.
const MODRINTH_CDN_HOST: &str = "cdn.modrinth.com";

/// The exact shape of a Modrinth catalog id: eight base62 characters.
///
/// Checked rather than assumed, because the same URL position can hold a
/// human-written version number in packs built by older tooling. Requiring at
/// least one letter rejects an all-digit release date such as `20240115`, which
/// is a plausible version number and would otherwise be recorded as an id the
/// catalog has never heard of.
fn catalog_id(segment: &str) -> Option<&str> {
    const ID_LENGTH: usize = 8;
    let looks_like_an_id = segment.len() == ID_LENGTH
        && segment.chars().all(|c| c.is_ascii_alphanumeric())
        && segment.chars().any(|c| c.is_ascii_alphabetic());
    looks_like_an_id.then_some(segment)
}

/// Fills in the provider identity of a recipe read back from disk.
///
/// A recipe written before pack files carried an identity records the same
/// authoritative download addresses, so the same reconstruction applies to it -
/// which is what lets an instance imported by an earlier build show its pack's
/// mods without being re-imported, and without a destructive migration.
///
/// Idempotent, and never overwrites: an identity that is already recorded was
/// written by an import or an installation and outranks anything derived here.
pub(crate) fn reconcile_identities(recipe: &mut PackRecipe) {
    for file in &mut recipe.files {
        if file
            .identity
            .as_ref()
            .is_none_or(ProviderFileIdentity::is_empty)
        {
            file.identity = provider_identity(&file.downloads);
        }
    }
}

/// The file's `sha512` and `sha1` digests, strongest first, or `None` unless
/// both are present and well formed.
///
/// The format requires both, so a missing or malformed one is not narrowed down
/// to the other: the strongest hash would otherwise be silently dropped.
fn normalize_hashes(raw: &BTreeMap<String, String>) -> Option<Vec<Hash>> {
    // SHA-512 first: it is the one Modrinth's own tooling treats as canonical,
    // and a stable order keeps the recipe file diffable.
    [
        ("sha512", HashAlgorithm::Sha512),
        ("sha1", HashAlgorithm::Sha1),
    ]
    .into_iter()
    .map(|(name, algorithm)| {
        raw.iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .and_then(|(_, digest)| Hash::new(algorithm, digest))
    })
    .collect()
}

/// A refused address with anything secret-shaped removed.
///
/// A rejected address is still reported - a pack pointing at an unlisted mirror
/// is a fact worth showing - but a CDN URL routinely carries a signed token in
/// its query string, and this text can end up in a log a user pastes into a bug
/// report. An address that does not even parse has no structure to strip, so
/// only its query and fragment are dropped.
fn redacted_address(raw: &str) -> String {
    let trimmed = raw.trim();
    ephervex_download::Url::parse(trimmed).map_or_else(
        |_| {
            trimmed
                .split(['?', '#'])
                .next()
                .unwrap_or_default()
                .to_owned()
        },
        |url| ephervex_download::redacted(&url),
    )
}

/// `modrinth.index.json` as the format defines it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawIndex {
    // Zero is not a format revision, and deserializing it as a non-zero number
    // refuses it as an unreadable manifest rather than as a "newer" one.
    format_version: NonZeroU64,
    game: String,
    version_id: String,
    name: String,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    files: Vec<RawFile>,
    #[serde(default)]
    dependencies: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawFile {
    path: String,
    #[serde(default)]
    hashes: BTreeMap<String, String>,
    #[serde(default)]
    env: Option<RawEnv>,
    #[serde(default)]
    downloads: Vec<String>,
    #[serde(default)]
    file_size: Option<u64>,
}

/// A file's declared environment support.
///
/// Both sides default to `required`, which is what the format means by omitting
/// the object: a file with no environment metadata belongs everywhere.
#[derive(Debug, Deserialize)]
struct RawEnv {
    #[serde(default)]
    client: RawRequirement,
    #[serde(default)]
    server: RawRequirement,
}

impl Default for RawEnv {
    fn default() -> Self {
        Self {
            client: RawRequirement::Required,
            server: RawRequirement::Required,
        }
    }
}

/// The three environment values the format defines.
///
/// Deliberately *not* tolerant of a fourth: an unknown value is a pack this
/// build does not understand, and guessing which way it leans is how a
/// server-only mod ends up in a client.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum RawRequirement {
    #[default]
    Required,
    Optional,
    Unsupported,
}

impl From<RawRequirement> for SideRequirement {
    fn from(value: RawRequirement) -> Self {
        match value {
            RawRequirement::Required => Self::Required,
            RawRequirement::Optional => Self::Optional,
            RawRequirement::Unsupported => Self::Unsupported,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::archive::ExtractionLimits;

    fn allowlist() -> DomainAllowlist {
        DomainAllowlist::modrinth_pack_defaults()
    }

    fn index(dependencies: &str, files: &str) -> String {
        format!(
            r#"{{
                "formatVersion": 1,
                "game": "minecraft",
                "versionId": "1.0.0",
                "name": "Test Pack",
                "files": [{files}],
                "dependencies": {dependencies}
            }}"#
        )
    }

    fn with_header(format_version: u64, game: &str) -> String {
        format!(
            r#"{{
                "formatVersion": {format_version},
                "game": "{game}",
                "versionId": "1.0.0",
                "name": "Test Pack",
                "files": [],
                "dependencies": {{"minecraft": "1.20.1"}}
            }}"#
        )
    }

    fn parse_index(document: &str) -> PackResult<ParsedPack> {
        parse(
            document.as_bytes(),
            &allowlist(),
            Some("test.mrpack".to_owned()),
            ExtractionLimits::default().max_declared_files,
        )
    }

    const SHA1: &str = "da39a3ee5e6b4b0d3255bfef95601890afd80709";
    const SHA512: &str = "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e";

    fn file_entry(path: &str, env: &str) -> String {
        format!(
            r#"{{
                "path": "{path}",
                "hashes": {{"sha1": "{SHA1}", "sha512": "{SHA512}"}},
                "env": {env},
                "downloads": ["https://cdn.modrinth.com/data/AABB/versions/1/{path}"],
                "fileSize": 42
            }}"#
        )
    }

    #[test]
    fn a_neoforge_pack_becomes_an_exact_loader_selection() {
        let parsed = parse_index(&index(
            r#"{"minecraft": "1.20.1", "neoforge": "47.1.106"}"#,
            &file_entry("mods/a.jar", r#"{"client":"required","server":"required"}"#),
        ))
        .expect("valid pack");

        assert_eq!(parsed.recipe.minecraft_version, "1.20.1");
        assert_eq!(
            parsed.recipe.loader,
            LoaderSelection::exact(LoaderKind::NeoForge, "47.1.106")
        );
        assert_eq!(parsed.recipe.original_name, "Test Pack");
        assert_eq!(parsed.recipe.original_version.as_deref(), Some("1.0.0"));
    }

    #[test]
    fn a_pack_with_only_a_minecraft_dependency_is_vanilla() {
        let parsed = parse_index(&index(r#"{"minecraft": "1.20.1"}"#, "")).expect("valid pack");
        assert_eq!(parsed.recipe.loader, LoaderSelection::vanilla());
    }

    #[test]
    fn fabric_and_quilt_are_refused_by_name_rather_than_ignored() {
        // an unsupported loader is a clear error, not a silent
        // downgrade to vanilla.
        for loader in ["fabric-loader", "quilt-loader"] {
            let error = parse_index(&index(
                &format!(r#"{{"minecraft": "1.20.1", "{loader}": "0.15.0"}}"#),
                "",
            ))
            .expect_err("must refuse");
            assert_eq!(error.unsupported_loader(), Some(loader));
            assert_eq!(error.message_id(), "error-pack-loader-unsupported");
        }
    }

    #[test]
    fn declaring_both_forge_and_neoforge_is_ambiguous_rather_than_a_guess() {
        let error = parse_index(&index(
            r#"{"minecraft": "1.20.1", "forge": "47.4.0", "neoforge": "47.1.106"}"#,
            "",
        ))
        .expect_err("must refuse");
        assert!(matches!(error, PackError::LoaderAmbiguous { .. }));
    }

    #[test]
    fn an_unknown_dependency_key_is_refused() {
        let error = parse_index(&index(
            r#"{"minecraft": "1.20.1", "liteloader": "1.2.3"}"#,
            "",
        ))
        .expect_err("must refuse");
        assert_eq!(error.unsupported_loader(), Some("liteloader"));
    }

    #[test]
    fn a_pack_with_no_minecraft_version_is_refused() {
        let error = parse_index(&index(r#"{"forge": "47.4.0"}"#, "")).expect_err("must refuse");
        assert!(matches!(error, PackError::MinecraftDependencyMissing));
    }

    #[test]
    fn a_newer_format_version_is_refused_rather_than_guessed_at() {
        let document = with_header(2, "minecraft");
        let error = parse_index(&document).expect_err("must refuse");
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
    fn a_pack_for_another_game_is_refused() {
        let document = with_header(1, "terraria");
        let error = parse_index(&document).expect_err("must refuse");
        assert!(matches!(error, PackError::GameUnsupported { .. }));
    }

    #[test]
    fn format_version_zero_is_refused_as_an_unreadable_manifest() {
        let document = with_header(0, "minecraft");
        let error = parse_index(&document).expect_err("must refuse");
        assert!(matches!(error, PackError::ManifestInvalid { .. }));
    }

    #[test]
    fn a_missing_required_top_level_field_is_refused_rather_than_defaulted() {
        let complete: serde_json::Value =
            serde_json::from_str(&index(r#"{"minecraft": "1.20.1"}"#, "")).unwrap();
        for field in ["formatVersion", "game", "versionId", "name"] {
            let mut document = complete.clone();
            document.as_object_mut().unwrap().remove(field);
            let error = parse_index(&document.to_string()).expect_err("must refuse");
            assert!(
                matches!(error, PackError::ManifestInvalid { .. }),
                "a pack without `{field}` should have been refused"
            );
        }
    }

    #[test]
    fn a_document_with_only_a_minecraft_dependency_is_refused() {
        let document = r#"{"dependencies": {"minecraft": "1.20.1"}}"#;
        let error = parse_index(document).expect_err("must refuse");
        assert!(matches!(error, PackError::ManifestInvalid { .. }));
    }

    #[test]
    fn the_environment_split_survives_into_the_normalized_recipe() {
        let files = format!(
            "{}, {}, {}",
            file_entry(
                "mods/client.jar",
                r#"{"client":"required","server":"unsupported"}"#
            ),
            file_entry(
                "mods/server.jar",
                r#"{"client":"unsupported","server":"required"}"#
            ),
            file_entry(
                "mods/optional.jar",
                r#"{"client":"optional","server":"optional"}"#
            ),
        );
        let parsed = parse_index(&index(r#"{"minecraft": "1.20.1"}"#, &files)).expect("valid");

        let requirements: Vec<(String, SideRequirement, SideRequirement)> = parsed
            .recipe
            .files
            .iter()
            .map(|file| (file.display_name(), file.client, file.server))
            .collect();
        assert_eq!(
            requirements,
            vec![
                (
                    "mods/client.jar".to_owned(),
                    SideRequirement::Required,
                    SideRequirement::Unsupported
                ),
                (
                    "mods/server.jar".to_owned(),
                    SideRequirement::Unsupported,
                    SideRequirement::Required
                ),
                (
                    "mods/optional.jar".to_owned(),
                    SideRequirement::Optional,
                    SideRequirement::Optional
                ),
            ]
        );
    }

    #[test]
    fn a_file_with_no_environment_object_is_required_on_both_sides() {
        let document = index(
            r#"{"minecraft": "1.20.1"}"#,
            &format!(
                r#"{{"path": "mods/a.jar", "hashes": {{"sha1": "{SHA1}", "sha512": "{SHA512}"}}, "downloads": ["https://cdn.modrinth.com/a.jar"]}}"#
            ),
        );
        let parsed = parse_index(&document).expect("valid");
        let file = &parsed.recipe.files[0];
        assert_eq!(file.client, SideRequirement::Required);
        assert_eq!(file.server, SideRequirement::Required);
    }

    #[test]
    fn an_unknown_environment_value_is_refused_rather_than_leaned_one_way() {
        let document = index(
            r#"{"minecraft": "1.20.1"}"#,
            &file_entry("mods/a.jar", r#"{"client":"maybe","server":"required"}"#),
        );
        let error = parse_index(&document).expect_err("must refuse");
        assert!(matches!(error, PackError::ManifestInvalid { .. }));
    }

    #[test]
    fn a_traversing_file_path_is_refused() {
        let document = index(
            r#"{"minecraft": "1.20.1"}"#,
            &file_entry(
                "../../../../home/dev/.bashrc",
                r#"{"client":"required","server":"required"}"#,
            ),
        );
        let error = parse_index(&document).expect_err("must refuse");
        assert!(matches!(error, PackError::EntryPathUnsafe { .. }));
    }

    #[test]
    fn a_file_aimed_at_launcher_metadata_is_refused() {
        let document = index(
            r#"{"minecraft": "1.20.1"}"#,
            &file_entry(
                ".launcher/instance.json",
                r#"{"client":"required","server":"required"}"#,
            ),
        );
        let error = parse_index(&document).expect_err("must refuse");
        assert!(matches!(error, PackError::EntryReserved { .. }));
    }

    #[test]
    fn a_file_with_no_usable_hash_is_refused_because_verification_is_mandatory() {
        let document = index(
            r#"{"minecraft": "1.20.1"}"#,
            r#"{"path": "mods/a.jar", "hashes": {"md5": "0123456789abcdef0123456789abcdef"}, "downloads": ["https://cdn.modrinth.com/a.jar"]}"#,
        );
        let error = parse_index(&document).expect_err("must refuse");
        assert!(matches!(error, PackError::FileHashesMissing { .. }));
    }

    #[test]
    fn a_malformed_digest_is_not_accepted_as_a_hash() {
        let document = index(
            r#"{"minecraft": "1.20.1"}"#,
            r#"{"path": "mods/a.jar", "hashes": {"sha1": "not-a-digest"}, "downloads": ["https://cdn.modrinth.com/a.jar"]}"#,
        );
        let error = parse_index(&document).expect_err("must refuse");
        assert!(matches!(error, PackError::FileHashesMissing { .. }));
    }

    #[test]
    fn a_file_without_both_required_digests_is_refused() {
        let cases = [
            ("sha1 only", format!(r#"{{"sha1": "{SHA1}"}}"#)),
            ("sha512 only", format!(r#"{{"sha512": "{SHA512}"}}"#)),
            (
                "malformed sha1",
                format!(r#"{{"sha1": "not-a-digest", "sha512": "{SHA512}"}}"#),
            ),
            (
                "malformed sha512",
                format!(r#"{{"sha1": "{SHA1}", "sha512": "not-a-digest"}}"#),
            ),
        ];
        for (label, hashes) in cases {
            let document = index(
                r#"{"minecraft": "1.20.1"}"#,
                &format!(
                    r#"{{"path": "mods/a.jar", "hashes": {hashes}, "downloads": ["https://cdn.modrinth.com/a.jar"]}}"#
                ),
            );
            let error = parse_index(&document).expect_err("must refuse");
            assert!(
                matches!(error, PackError::FileHashesMissing { .. }),
                "{label} should have been refused"
            );
        }
    }

    #[test]
    fn a_file_with_no_download_address_is_refused() {
        let document = index(
            r#"{"minecraft": "1.20.1"}"#,
            &format!(
                r#"{{"path": "mods/a.jar", "hashes": {{"sha1": "{SHA1}", "sha512": "{SHA512}"}}, "downloads": []}}"#
            ),
        );
        let error = parse_index(&document).expect_err("must refuse");
        assert!(matches!(error, PackError::FileDownloadsMissing { .. }));
    }

    #[test]
    fn an_unlisted_download_host_is_refused_and_a_permitted_sibling_is_kept() {
        // the allowlist is what stops an untrusted archive turning the
        // launcher into an SSRF probe.
        let document = index(
            r#"{"minecraft": "1.20.1"}"#,
            &format!(
                r#"{{"path": "mods/a.jar", "hashes": {{"sha1": "{SHA1}", "sha512": "{SHA512}"}}, "downloads": ["https://evil.example/a.jar", "https://cdn.modrinth.com/a.jar"]}}"#
            ),
        );
        let parsed = parse_index(&document).expect("one address is permitted");
        assert_eq!(
            parsed.recipe.files[0].downloads,
            vec!["https://cdn.modrinth.com/a.jar"]
        );
        assert_eq!(parsed.rejected_urls, vec!["https://evil.example/a.jar"]);
    }

    #[test]
    fn a_file_whose_every_address_is_refused_fails_the_import() {
        for address in [
            "file:///etc/passwd",
            "http://cdn.modrinth.com/a.jar",
            "https://evil.example/a.jar",
        ] {
            let document = index(
                r#"{"minecraft": "1.20.1"}"#,
                &format!(
                    r#"{{"path": "mods/a.jar", "hashes": {{"sha1": "{SHA1}", "sha512": "{SHA512}"}}, "downloads": ["{address}"]}}"#
                ),
            );
            let error = parse_index(&document).expect_err("must refuse");
            assert!(
                matches!(error, PackError::DownloadUrlRejected { .. }),
                "{address} should have been refused"
            );
        }
    }

    #[test]
    fn both_published_hashes_are_kept_for_verification() {
        let document = index(
            r#"{"minecraft": "1.20.1"}"#,
            &file_entry("mods/a.jar", r#"{"client":"required","server":"required"}"#),
        );
        let parsed = parse_index(&document).expect("valid");
        let algorithms: Vec<HashAlgorithm> = parsed.recipe.files[0]
            .hashes
            .iter()
            .map(|hash| hash.algorithm)
            .collect();
        assert_eq!(
            algorithms,
            vec![HashAlgorithm::Sha512, HashAlgorithm::Sha1],
            "verification uses whichever supported hash the pack published"
        );
    }

    // -- reconstructing catalog identity ----------------------------------

    fn identity_of(address: &str) -> Option<ProviderFileIdentity> {
        provider_identity(&[address.to_owned()])
    }

    #[test]
    fn a_catalog_address_yields_the_project_and_version_it_names() {
        let identity =
            identity_of("https://cdn.modrinth.com/data/lfHFW1mp/versions/GKQEbfSt/journeymap.jar")
                .expect("an identity");
        assert_eq!(
            identity.provider,
            Some(ephervex_model::ModProvider::Modrinth)
        );
        assert_eq!(identity.project_id.as_deref(), Some("lfHFW1mp"));
        assert_eq!(identity.version_id.as_deref(), Some("GKQEbfSt"));
        assert_eq!(
            identity.file_id, None,
            "Modrinth has no file id apart from the version, and one is not invented"
        );
    }

    #[test]
    fn a_version_number_in_the_version_position_is_not_recorded_as_a_version_id() {
        // Older pack tooling wrote the human version number here. Anything that
        // is not shaped like a catalog id leaves the release unproven.
        for segment in ["1.4.2", "5.9.18", "20240115", "v2", "GKQEbfSt-beta"] {
            let identity = identity_of(&format!(
                "https://cdn.modrinth.com/data/lfHFW1mp/versions/{segment}/mod.jar"
            ))
            .expect("the project is still proven");
            assert_eq!(identity.project_id.as_deref(), Some("lfHFW1mp"));
            assert_eq!(
                identity.version_id, None,
                "`{segment}` is not a catalog id and must not be recorded as one"
            );
        }
    }

    #[test]
    fn an_address_that_is_not_the_catalogs_own_proves_nothing_about_the_catalog() {
        // Every one of these is a permitted download host. None of them says
        // anything about a Modrinth project.
        for address in [
            "https://github.com/example/mod/releases/download/v1/lfHFW1mp.jar",
            "https://raw.githubusercontent.com/example/mod/main/data/lfHFW1mp/versions/GKQEbfSt/mod.jar",
            "https://gitlab.com/example/mod/-/raw/main/mod.jar",
        ] {
            assert_eq!(identity_of(address), None, "{address} proved too much");
        }
    }

    #[test]
    fn an_address_that_is_not_shaped_like_an_artifact_path_proves_nothing() {
        for address in [
            "https://cdn.modrinth.com/mod.jar",
            "https://cdn.modrinth.com/other/lfHFW1mp/versions/GKQEbfSt/mod.jar",
            "https://cdn.modrinth.com/data/not-an-id/versions/GKQEbfSt/mod.jar",
            "not a url at all",
        ] {
            assert_eq!(identity_of(address), None, "{address} proved too much");
        }
    }

    #[test]
    fn a_mirror_alongside_the_catalog_does_not_hide_the_catalogs_own_address() {
        let identity = provider_identity(&[
            "https://github.com/example/mod/releases/download/v1/mod.jar".to_owned(),
            "https://cdn.modrinth.com/data/lfHFW1mp/versions/GKQEbfSt/mod.jar".to_owned(),
        ])
        .expect("an identity");
        assert_eq!(identity.version_id.as_deref(), Some("GKQEbfSt"));
    }

    #[test]
    fn reconstruction_fills_gaps_and_overwrites_nothing() {
        let mut recipe = parse_index(&index(
            r#"{"minecraft": "1.20.1"}"#,
            &file_entry("mods/a.jar", r#"{"client":"required","server":"required"}"#),
        ))
        .expect("valid")
        .recipe;

        // The fixture's own address names project `AABB`, which is not a
        // catalog id, so parsing recorded nothing for it.
        assert_eq!(recipe.files[0].identity, None);

        recipe.files[0].downloads =
            vec!["https://cdn.modrinth.com/data/lfHFW1mp/versions/GKQEbfSt/a.jar".to_owned()];
        reconcile_identities(&mut recipe);
        assert_eq!(
            recipe.files[0]
                .identity
                .as_ref()
                .and_then(|it| it.version_id.as_deref()),
            Some("GKQEbfSt")
        );

        // A second pass changes nothing, and a recorded identity survives one.
        let after_first = recipe.clone();
        reconcile_identities(&mut recipe);
        assert_eq!(recipe, after_first, "reconstruction is idempotent");
    }

    #[test]
    fn a_document_that_is_not_json_names_the_manifest_it_came_from() {
        let error = parse_index("{not json").expect_err("must refuse");
        assert!(matches!(
            error,
            PackError::ManifestInvalid { ref entry,.. } if entry == "modrinth.index.json"
        ));
    }

    fn declared(count: u32) -> String {
        (1..=count)
            .map(|id| {
                file_entry(
                    &format!("mods/{id}.jar"),
                    r#"{"client":"required","server":"required"}"#,
                )
            })
            .collect::<Vec<_>>()
            .join(",")
    }

    fn parse_with_ceiling(files: &str, ceiling: u64) -> PackResult<ParsedPack> {
        parse(
            index(r#"{"minecraft": "1.20.1", "neoforge": "47.1.106"}"#, files).as_bytes(),
            &allowlist(),
            None,
            ceiling,
        )
    }

    #[test]
    fn an_index_declaring_more_files_than_the_ceiling_is_refused() {
        let error = parse_with_ceiling(&declared(3), 2).expect_err("over the ceiling");
        assert!(
            matches!(
                error,
                PackError::LimitExceeded {
                    limit: "declared-files",
                    allowed: 2,
                    reached: 3
                }
            ),
            "{error:?}"
        );
    }

    #[test]
    fn an_index_declaring_exactly_the_ceiling_is_accepted() {
        let parsed = parse_with_ceiling(&declared(3), 3).expect("at the ceiling");
        assert_eq!(parsed.recipe.files.len(), 3);
    }

    #[test]
    fn the_declaration_past_the_ceiling_is_counted_without_being_read() {
        let files = format!("{}, \"not a declaration\"", declared(2));
        let error = parse_with_ceiling(&files, 2).expect_err("over the ceiling");
        assert!(
            matches!(error, PackError::LimitExceeded { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn a_files_member_that_is_not_an_array_is_an_unreadable_index() {
        let document = r#"{"formatVersion": 1, "game": "minecraft", "versionId": "1",
            "name": "Test Pack", "files": null,
            "dependencies": {"minecraft": "1.20.1", "neoforge": "47.1.106"}}"#;
        let error = parse(document.as_bytes(), &allowlist(), None, 10).expect_err("null files");
        assert!(
            matches!(error, PackError::ManifestInvalid { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn an_index_far_over_the_ceiling_reports_how_many_files_it_declared() {
        let error = parse_with_ceiling(&declared(7), 2).expect_err("over the ceiling");
        assert!(
            matches!(
                error,
                PackError::LimitExceeded {
                    limit: "declared-files",
                    allowed: 2,
                    reached: 7
                }
            ),
            "{error:?}"
        );
    }

    #[test]
    fn a_malformed_declaration_is_reported_where_it_sits_in_the_index() {
        let document = "{\n  \"formatVersion\": 1,\n  \"game\": \"minecraft\",\n  \"versionId\": \"1\",\n  \"name\": \"Test Pack\",\n  \"dependencies\": {\"minecraft\": \"1.20.1\", \"neoforge\": \"47.1.106\"},\n  \"files\": [\n    {\"path\": 3}\n  ]\n}";
        let error = parse(document.as_bytes(), &allowlist(), None, 10).expect_err("malformed");
        assert!(
            matches!(&error, PackError::ManifestInvalid { source, .. } if source.line() == 8),
            "{error:?}"
        );
    }

    #[test]
    fn an_index_written_as_an_array_is_refused_rather_than_read_by_position() {
        let document = format!(
            r#"[1, "minecraft", "1", "Test Pack", null, [{}], {{"minecraft": "1.20.1", "neoforge": "47.1.106"}}]"#,
            declared(3)
        );
        let error = parse(document.as_bytes(), &allowlist(), None, 2).expect_err("array form");
        assert!(
            matches!(error, PackError::ManifestInvalid { .. }),
            "{error:?}"
        );
    }
}
