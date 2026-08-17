//! Which catalog a mod in an instance came from.
//!
//! # Why there is no `Unknown`
//!
//! A `mods/` directory is a directory users edit. Jars appear in it that the
//! launcher did not put there: a local dev build, a jar from a forum post, the
//! file a bug reporter attached. It is tempting to give those a provider of
//! `Unknown` so every jar has a record.
//!
//! That would be a record the launcher invented. It would claim a project it
//! cannot re-resolve, carry hashes of whatever happened to be on disk, and —
//! because a recorded mod is part of an instance's desired state — put that jar
//! into a reconstructed server on the strength of a guess. The absence of a
//! provider identity already says "nothing trustworthy is known about this
//! file", and saying nothing is the honest answer.
//!
//! So this enum names only providers something can actually be resolved
//! against, and a jar with no provenance stays outside the managed model
//! entirely.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A mod catalog Ephervex can resolve a project and version against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ModProvider {
    Modrinth,
    /// CurseForge, reached through its official third-party API.
    ///
    /// Present so that everything the CurseForge transport reports — an absent
    /// API key, a rejected one, a response that could not be read — names the
    /// catalog it came from. Nothing writes it into a recipe yet.
    CurseForge,
}

impl ModProvider {
    /// A stable identifier for the recipe file, CLI arguments, and `--json`.
    ///
    /// Deliberately not derived from [`fmt::Display`], which exists to be read
    /// by humans: this string is part of an on-disk format.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Modrinth => "modrinth",
            Self::CurseForge => "curseforge",
        }
    }

    /// Parses the identifier back.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "modrinth" => Some(Self::Modrinth),
            "curseforge" => Some(Self::CurseForge),
            _ => None,
        }
    }
}

impl fmt::Display for ModProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Provider names are product names and stay untranslated.
        f.write_str(match self {
            Self::Modrinth => "Modrinth",
            Self::CurseForge => "CurseForge",
        })
    }
}

/// Why a file is part of an instance's desired state.
///
/// The distinction the mod browser needs and the pack importer does not: a
/// dependency pulled in automatically, an optional one a human ticked, and a
/// mod someone chose by name are three different things to show, and — once mod
/// removal exists — three different things to do when the mod that wanted them
/// goes away.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModOrigin {
    /// Declared by the imported pack. The default, and what every entry
    /// written before managed mods existed is.
    #[default]
    PackDeclared,
    /// Chosen by name in the mod browser.
    UserSelected,
    /// Installed to satisfy another mod's required dependency.
    RequiredDependency,
    /// An optional dependency a human explicitly selected.
    SelectedOptionalDependency,
}

impl ModOrigin {
    /// A stable identifier for `--json` output and the recipe file.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PackDeclared => "pack_declared",
            Self::UserSelected => "user_selected",
            Self::RequiredDependency => "required_dependency",
            Self::SelectedOptionalDependency => "selected_optional_dependency",
        }
    }

    /// Whether a human asked for this file by name.
    #[must_use]
    pub const fn is_explicit_choice(self) -> bool {
        matches!(self, Self::UserSelected | Self::SelectedOptionalDependency)
    }

    /// Whether the imported pack, rather than a later decision in this
    /// instance, is what put this file here.
    ///
    /// The distinction outlives display: a pack-owned mod's version is part of
    /// the pack, so changing it independently is a different operation from
    /// changing one the user installed themselves, and the frontend must not
    /// offer the two as though they were the same.
    #[must_use]
    pub const fn is_pack_owned(self) -> bool {
        matches!(self, Self::PackDeclared)
    }

    /// Whether this file is here only because something else needed it.
    #[must_use]
    pub const fn is_dependency(self) -> bool {
        matches!(
            self,
            Self::RequiredDependency | Self::SelectedOptionalDependency
        )
    }

    /// The `ephervex-i18n` message id naming this origin to a user.
    #[must_use]
    pub const fn message_id(self) -> &'static str {
        match self {
            Self::PackDeclared => "mod-origin-pack-declared",
            Self::UserSelected => "mod-origin-user-selected",
            Self::RequiredDependency => "mod-origin-required-dependency",
            Self::SelectedOptionalDependency => "mod-origin-selected-optional-dependency",
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn the_persisted_identifier_round_trips() {
        assert_eq!(ModProvider::parse("modrinth"), Some(ModProvider::Modrinth));
        assert_eq!(
            ModProvider::parse(" Modrinth "),
            Some(ModProvider::Modrinth)
        );
        assert_eq!(ModProvider::Modrinth.as_str(), "modrinth");
        assert_eq!(
            ModProvider::parse("CurseForge"),
            Some(ModProvider::CurseForge)
        );
        assert_eq!(ModProvider::CurseForge.as_str(), "curseforge");
    }

    #[test]
    fn a_provider_this_build_does_not_know_is_refused_rather_than_guessed_at() {
        // The alternative — folding it into a placeholder — is how a recipe
        // ends up naming a project nothing can resolve.
        assert_eq!(ModProvider::parse("technic"), None);
        assert_eq!(ModProvider::parse(""), None);
    }

    #[test]
    fn the_display_name_is_the_product_name_and_the_stored_one_is_not() {
        assert_eq!(ModProvider::Modrinth.to_string(), "Modrinth");
        assert_eq!(ModProvider::CurseForge.to_string(), "CurseForge");
        assert_ne!(
            ModProvider::Modrinth.to_string(),
            ModProvider::Modrinth.as_str(),
            "the on-disk spelling must not be a `Display` implementation's output"
        );
    }

    #[test]
    fn the_stored_spelling_is_what_serde_writes() {
        let json = serde_json::to_string(&ModProvider::Modrinth).expect("serialize");
        assert_eq!(json, "\"modrinth\"");
        let back: ModProvider = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, ModProvider::Modrinth);
    }
}
