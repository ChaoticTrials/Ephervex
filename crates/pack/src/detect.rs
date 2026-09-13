//! Working out what an imported archive actually is (format detection).
//!
//! # Why detection is not the Modrinth importer's job
//!
//! The import workflow opens with one file chooser and three possible answers,
//! and the user picks a file rather than a format. Detection therefore belongs
//! to the import pipeline rather than to any one provider: it looks at what the
//! archive *contains*, so a `.mrpack` renamed to `.zip` and a CurseForge export
//! renamed to `.mrpack` are both identified correctly.
//!
//! Other formats are still recognized here even though only Modrinth packs are
//! supported. A precise unsupported-format error is more useful than "this is
//! not a pack." Recognition does not make that format part of the supported
//! import scope, and the recognizer remains provider-independent plumbing.

use std::fmt;
use std::path::Path;

use ephervex_model::ImportSourceKind;

/// The archive shapes the import pipeline recognizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PackFormat {
    /// A Modrinth `.mrpack`: `modrinth.index.json` at the archive root.
    ModrinthPack,
    /// A CurseForge export: `manifest.json` at the archive root, normally with
    /// an `overrides/` directory beside it.
    CurseForgePack,
    /// A Prism-compatible instance export: `mmc-pack.json`, usually with an
    /// `instance.cfg`, either at the root or under a single wrapper directory.
    PrismExport,
}

impl PackFormat {
    /// The manifest entry that identifies this format.
    #[must_use]
    pub const fn manifest_entry(self) -> &'static str {
        match self {
            Self::ModrinthPack => MODRINTH_INDEX,
            Self::CurseForgePack => CURSEFORGE_MANIFEST,
            Self::PrismExport => PRISM_PACK,
        }
    }

    /// How an instance imported from this format records where it came from.
    #[must_use]
    pub const fn source_kind(self) -> ImportSourceKind {
        match self {
            Self::ModrinthPack => ImportSourceKind::ModrinthPack,
            Self::CurseForgePack => ImportSourceKind::CurseForgePack,
            Self::PrismExport => ImportSourceKind::PrismExport,
        }
    }

    /// Whether this build can import this format.
    ///
    /// False is not a defect: it is the honest answer for a recognized format
    /// whose importer is not available.
    #[must_use]
    pub const fn is_implemented(self) -> bool {
        matches!(self, Self::ModrinthPack | Self::CurseForgePack)
    }

    /// Whether importing this format needs a provider credential before its
    /// declared files can be resolved.
    ///
    /// Detection and manifest parsing never do: reading what is already in the
    /// archive is a local operation, and a launcher that cannot even say what a
    /// file is without a key is worse than one that says what it is and what it
    /// still needs.
    #[must_use]
    pub const fn needs_provider_credential(self) -> bool {
        matches!(self, Self::CurseForgePack)
    }
}

impl fmt::Display for PackFormat {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Provider names are product names and stay untranslated.
        formatter.write_str(match self {
            Self::ModrinthPack => "Modrinth",
            Self::CurseForgePack => "CurseForge",
            Self::PrismExport => "Prism",
        })
    }
}

/// `modrinth.index.json` - the Modrinth modpack format's manifest.
pub const MODRINTH_INDEX: &str = "modrinth.index.json";
/// `manifest.json` - the CurseForge export's manifest.
pub const CURSEFORGE_MANIFEST: &str = "manifest.json";
/// `mmc-pack.json` - the component list in a Prism-compatible export.
pub const PRISM_PACK: &str = "mmc-pack.json";

/// Identifies an archive from the entry names it contains.
///
/// Takes names rather than an archive so it can be unit-tested against a list
/// and reused by any reader. Matching is exact on the archive root first, then
/// on one level of wrapper directory - Prism exports habitually wrap everything
/// in a directory named after the instance, and a user who re-zipped a folder
/// produces the same shape for the other two.
#[must_use]
pub fn identify<'a>(entries: impl IntoIterator<Item = &'a str>) -> Option<PackFormat> {
    let mut wrapped: Option<PackFormat> = None;

    for entry in entries {
        let normalized = entry.replace('\\', "/");
        let trimmed = normalized.trim_start_matches("./");
        if let Some(format) = format_of(trimmed) {
            return Some(format);
        }
        // One level of wrapper directory, and only one: deeper matches are as
        // likely to be a pack that ships another pack's manifest as a payload.
        if let Some((_, inner)) = trimmed.split_once('/')
            && let Some(format) = format_of(inner)
        {
            wrapped = Some(wrapped.unwrap_or(format));
        }
    }
    wrapped
}

/// Which format a root-level entry name identifies.
fn format_of(name: &str) -> Option<PackFormat> {
    match name {
        MODRINTH_INDEX => Some(PackFormat::ModrinthPack),
        CURSEFORGE_MANIFEST => Some(PackFormat::CurseForgePack),
        PRISM_PACK => Some(PackFormat::PrismExport),
        _ => None,
    }
}

/// Whether a filename carries the Modrinth pack extension.
///
/// Advisory only - the archive's contents decide (see [`identify`]) - but the
/// extension is what a file chooser filters on, so it is worth naming once.
#[must_use]
pub fn has_mrpack_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("mrpack"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn a_modrinth_pack_is_identified_by_its_index() {
        assert_eq!(
            identify(["modrinth.index.json", "overrides/config/a.toml"]),
            Some(PackFormat::ModrinthPack)
        );
    }

    #[test]
    fn a_curseforge_export_is_identified_by_its_manifest() {
        assert_eq!(
            identify(["manifest.json", "overrides/mods/a.jar"]),
            Some(PackFormat::CurseForgePack)
        );
        assert!(PackFormat::CurseForgePack.is_implemented());
    }

    #[test]
    fn unsupported_pack_formats_are_recognized_before_import() {
        // A recognized but unsupported Prism export must not be reported as an
        // unrecognized archive.
        assert_eq!(
            identify(["mmc-pack.json", "instance.cfg", "minecraft/options.txt"]),
            Some(PackFormat::PrismExport)
        );
        assert!(!PackFormat::PrismExport.is_implemented());
        assert!(PackFormat::ModrinthPack.is_implemented());
    }

    #[test]
    fn only_the_format_whose_files_live_behind_an_api_needs_a_credential() {
        assert!(PackFormat::CurseForgePack.needs_provider_credential());
        assert!(!PackFormat::ModrinthPack.needs_provider_credential());
    }

    #[test]
    fn an_unrelated_archive_is_not_a_curseforge_export_because_of_a_similar_name() {
        // Detection is on the exact manifest entry, at the root or under one
        // wrapper directory. Nothing else counts.
        for entries in [
            vec!["manifest.json.txt", "readme.md"],
            vec!["my-manifest.json"],
            vec!["docs/manifest.json/notes.txt"],
            vec!["a/b/manifest.json"],
        ] {
            assert_eq!(identify(entries.clone()), None, "{entries:?}");
        }
    }

    #[test]
    fn one_wrapper_directory_is_seen_through_but_not_two() {
        assert_eq!(
            identify(["My Instance/mmc-pack.json", "My Instance/instance.cfg"]),
            Some(PackFormat::PrismExport)
        );
        assert_eq!(identify(["a/b/modrinth.index.json"]), None);
    }

    #[test]
    fn an_extension_alone_identifies_nothing() {
        assert!(has_mrpack_extension(Path::new("report.MRPACK")));
        assert!(!has_mrpack_extension(Path::new("report.zip")));
        assert_eq!(identify(["random.txt"]), None);
    }

    #[test]
    fn a_root_match_outranks_a_wrapped_one() {
        // A pack that ships another launcher's manifest inside a folder is
        // still the format its own root says it is.
        assert_eq!(
            identify(["wrapper/manifest.json", "modrinth.index.json"]),
            Some(PackFormat::ModrinthPack)
        );
    }

    #[test]
    fn each_format_maps_to_its_own_import_source_kind() {
        assert_eq!(
            PackFormat::ModrinthPack.source_kind(),
            ImportSourceKind::ModrinthPack
        );
        assert_eq!(
            PackFormat::CurseForgePack.source_kind(),
            ImportSourceKind::CurseForgePack
        );
        assert_eq!(
            PackFormat::PrismExport.source_kind(),
            ImportSourceKind::PrismExport
        );
        assert_eq!(PackFormat::ModrinthPack.to_string(), "Modrinth");
    }
}
