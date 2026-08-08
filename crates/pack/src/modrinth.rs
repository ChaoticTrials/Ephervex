//! Parsing and validating `modrinth.index.json`.
//!
//! Modrinth's current modpack format specification is authoritative here; this
//! module is a reader for it and nothing else. Everything it learns is
//! immediately turned into the provider-neutral [`PackRecipe`], so the rest of
//! the launcher never sees a `dependencies` map or an `env` object.
//!
//! # What is validated, and why refusal is the right answer
//!
//! Validation covers the ZIP structure, `formatVersion`, `game`, the
//! Minecraft dependency, the loader dependencies, file paths, hashes, download
//! URL scheme and domain, and the environment metadata. Every one of them is a
//! hard failure rather than a repair, because this is a *reproduction* launcher:
//! a pack that has been quietly adjusted to import cleanly reproduces something
//! other than the bug.
//!
//! Two of the refusals are worth naming:
//!
//! * **Fabric and Quilt.** They are unsupported and must not be silently
//!   ignored. The error names the loader
//!   so the instance can carry `Needs attention — <loader> is not supported`
//!   rather than a generic failure.
//! * **A pack declaring both Forge and NeoForge.** This is ambiguous: the two
//!   are different loaders with different install semantics, and
//!   picking one would be inventing an interpretation the format does not have.

use std::collections::BTreeMap;

use ephervex_download::DomainAllowlist;
use ephervex_model::{Hash, HashAlgorithm, ImportSourceKind, LoaderKind, LoaderSelection};
use serde::Deserialize;
use tracing::{debug, warn};

use crate::archive::PackArchive;
use crate::detect::MODRINTH_INDEX;
use crate::error::{PackError, PackResult};
use crate::path::{is_reserved, safe_relative_path};
use crate::recipe::{
    OverrideLayer, OverrideScope, PackFileRecipe, PackRecipe, ProviderRecipeMetadata,
    RECIPE_VERSION, SideRequirement,
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
    let manifest = archive.read_entry(MODRINTH_INDEX)?;
    let archive_name = archive.file_name().map(ToOwned::to_owned);
    let layers = present_layers(archive);
    let mut parsed = parse(&manifest, allowlist, archive_name)?;
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
    .filter(|(_, prefix)| archive.contains_prefix(prefix))
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
/// without building an archive around it.
pub fn parse(
    manifest: &[u8],
    allowlist: &DomainAllowlist,
    archive_name: Option<String>,
) -> PackResult<ParsedPack> {
    let index: RawIndex =
        serde_json::from_slice(manifest).map_err(|source| PackError::ManifestInvalid {
            entry: MODRINTH_INDEX.to_owned(),
            source,
        })?;

    if index.format_version > SUPPORTED_FORMAT_VERSION {
        return Err(PackError::ManifestUnsupportedVersion {
            entry: MODRINTH_INDEX,
            found: index.format_version,
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
        original_name: index.name.unwrap_or_else(|| {
            archive_name
                .clone()
                .unwrap_or_else(|| "Modrinth pack".to_owned())
        }),
        original_version: index.version_id,
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
            // are refusals: Ephervex requires an unsupported loader must not be silently
            // ignored, and an unknown key cannot be assumed harmless because a
            // pack that needs it will not run without it.
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

    let hashes = normalize_hashes(&file.hashes);
    if hashes.is_empty() {
        // Verification is mandatory, so a file nobody published a
        // usable hash for is a file this launcher will not fetch.
        return Err(PackError::FileHashesMissing {
            path: declared_path,
        });
    }

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
    Ok(PackFileRecipe {
        logical_path,
        hashes,
        downloads,
        size: file.file_size,
        client: environment.client.into(),
        server: environment.server.into(),
        selected: None,
        // A Modrinth pack index identifies its files by hash and address only.
        // Leaving this `None` rather than inventing an id is what keeps the
        // normalized model honest for the providers that do have one.
        identity: None,
    })
}

/// Keeps the hashes this launcher can actually verify, in a stable order.
fn normalize_hashes(raw: &BTreeMap<String, String>) -> Vec<Hash> {
    // SHA-512 first: it is the one Modrinth's own tooling treats as canonical,
    // and a stable order keeps the recipe file diffable.
    [
        ("sha512", HashAlgorithm::Sha512),
        ("sha1", HashAlgorithm::Sha1),
    ]
    .into_iter()
    .filter_map(|(name, algorithm)| {
        raw.iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .and_then(|(_, digest)| Hash::new(algorithm, digest))
    })
    .collect()
}

/// A refused address with anything secret-shaped removed.
///
/// A rejected address is still reported — a pack pointing at an unlisted mirror
/// is a fact worth showing — but a CDN URL routinely carries a signed token in
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
    #[serde(default = "default_format_version")]
    format_version: u64,
    #[serde(default = "default_game")]
    game: String,
    #[serde(default)]
    version_id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    files: Vec<RawFile>,
    #[serde(default)]
    dependencies: BTreeMap<String, String>,
}

const fn default_format_version() -> u64 {
    SUPPORTED_FORMAT_VERSION
}

fn default_game() -> String {
    SUPPORTED_GAME.to_owned()
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

    fn parse_index(document: &str) -> PackResult<ParsedPack> {
        parse(
            document.as_bytes(),
            &allowlist(),
            Some("test.mrpack".to_owned()),
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
        let document =
            r#"{"formatVersion": 2, "game": "minecraft", "dependencies": {"minecraft": "1.20.1"}}"#;
        let error = parse_index(document).expect_err("must refuse");
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
        let document =
            r#"{"formatVersion": 1, "game": "terraria", "dependencies": {"minecraft": "1.20.1"}}"#;
        let error = parse_index(document).expect_err("must refuse");
        assert!(matches!(error, PackError::GameUnsupported { .. }));
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
                r#"{{"path": "mods/a.jar", "hashes": {{"sha1": "{SHA1}"}}, "downloads": ["https://cdn.modrinth.com/a.jar"]}}"#
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
    fn a_file_with_no_download_address_is_refused() {
        let document = index(
            r#"{"minecraft": "1.20.1"}"#,
            &format!(
                r#"{{"path": "mods/a.jar", "hashes": {{"sha1": "{SHA1}"}}, "downloads": []}}"#
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
                r#"{{"path": "mods/a.jar", "hashes": {{"sha1": "{SHA1}"}}, "downloads": ["https://evil.example/a.jar", "https://cdn.modrinth.com/a.jar"]}}"#
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
                    r#"{{"path": "mods/a.jar", "hashes": {{"sha1": "{SHA1}"}}, "downloads": ["{address}"]}}"#
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

    #[test]
    fn a_document_that_is_not_json_names_the_manifest_it_came_from() {
        let error = parse_index("{not json").expect_err("must refuse");
        assert!(matches!(
            error,
            PackError::ManifestInvalid { ref entry,.. } if entry == "modrinth.index.json"
        ));
    }
}
