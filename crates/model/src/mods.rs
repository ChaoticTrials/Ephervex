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
use std::num::NonZeroU32;

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
    /// catalog it came from, and so a pack imported from a CurseForge export
    /// records the catalog its files came from.
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

/// The exact numeric id CurseForge names a project by.
///
/// A typed wrapper rather than a bare integer because a CurseForge project id
/// and a CurseForge file id are both small numbers that appear side by side in
/// an export manifest, in an API path, and in a recipe entry — and swapping
/// them resolves perfectly well against the wrong thing. Zero is not a valid
/// id, so it cannot be constructed: a manifest that declares `0` is refused at
/// the parser rather than turning into a lookup that can only fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CurseForgeProjectId(NonZeroU32);

/// The exact numeric id CurseForge names one file of a project by.
///
/// Distinct from [`CurseForgeProjectId`] for the reason given there. A file id
/// is what makes a pack import exact: the launcher installs the file the pack
/// declared, never the newest file of the same project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CurseForgeFileId(NonZeroU32);

macro_rules! curseforge_id {
    ($name:ident) => {
        impl $name {
            /// The id, or `None` for zero.
            #[must_use]
            pub const fn new(value: u32) -> Option<Self> {
                match NonZeroU32::new(value) {
                    Some(value) => Some(Self(value)),
                    None => None,
                }
            }

            /// Parses the decimal spelling a manifest and a recipe both use.
            ///
            /// Deliberately strict: leading signs, whitespace, and anything a
            /// `u32` cannot hold are refused rather than truncated, because an
            /// approximated id names another project's file.
            #[must_use]
            pub fn parse(text: &str) -> Option<Self> {
                text.parse::<u32>().ok().and_then(Self::new)
            }

            /// The id as a plain number, for building an API path.
            #[must_use]
            pub const fn get(self) -> u32 {
                self.0.get()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                // The exact upstream value, which is what a recipe stores and
                // what an API path carries.
                fmt::Display::fmt(&self.0, f)
            }
        }
    };
}

curseforge_id!(CurseForgeProjectId);
curseforge_id!(CurseForgeFileId);

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
    /// Found in `mods/` with no record of how it got there.
    ///
    /// Somebody copied a jar in themselves, or another launcher did. Ephervex
    /// did not install it and does not own it: it is listed so the directory
    /// and the list agree about what will load, and no automatic operation
    /// touches it. An explicit request from the user may still delete it, which
    /// is the one thing owning a file has never been required for.
    HandPlaced,
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
            Self::HandPlaced => "hand_placed",
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
            Self::HandPlaced => "mod-origin-hand-placed",
        }
    }

    /// Whether Ephervex put this file here.
    ///
    /// A hand-placed jar is the one origin it did not: nothing recorded it,
    /// no automatic operation replaces or deletes it, and a server built from
    /// the recipe does not contain it until somebody says it should.
    #[must_use]
    pub const fn is_launcher_owned(self) -> bool {
        !matches!(self, Self::HandPlaced)
    }
}

/// What a copy does with mods the source records but no longer has on disk.
///
/// A recipe describes an instance's desired state, not the contents of its
/// `mods/` directory, so a jar deleted while bisecting leaves a record behind
/// on purpose (ADR 0013). That record is worth keeping in the instance it
/// belongs to and is a question when something new is built from it: a copy
/// made to carry on bisecting wants those mods gone, and a copy made to get
/// back to a working state wants them.
///
/// Only the user can answer it, so nothing here has a "reasonable" default that
/// is not [`Self::Keep`]: keeping is what every caller did before this existed,
/// and dropping loses a decision the recipe was the only record of.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum AbsentModPolicy {
    /// The copy records them too, exactly as the source does.
    #[default]
    Keep,
    /// The copy does not record them, as though they had been removed.
    Drop,
}

impl AbsentModPolicy {
    /// Whether a copy made under this policy forgets the absent records.
    #[must_use]
    pub const fn drops_them(self) -> bool {
        matches!(self, Self::Drop)
    }
}

/// The catalog a mod browser opens on, as durable user policy.
///
/// # Why three states rather than a [`ModProvider`]
///
/// "Nothing recorded" and "recorded as Modrinth" resolve to the same catalog
/// today and are not the same fact: the first is Ephervex's own default, which
/// a later release is free to revisit, and the second is a choice the user made
/// that happens to agree with it. A settings screen shows them differently, and
/// collapsing them would silently promote a default into a decision.
///
/// [`Self::Unrecognized`] exists because this value is written by whichever
/// Ephervex the user ran last. A newer one may know a catalog this build does
/// not, and the honest answer is to say so and leave the recorded name alone
/// rather than to read it as one of the catalogs this build happens to have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefaultModCatalog {
    /// Nothing is recorded, so [`DefaultModCatalog::BUILT_IN`] applies.
    BuiltIn,
    /// The user chose this catalog.
    Selected(ModProvider),
    /// A catalog name this build cannot resolve, preserved exactly as stored.
    Unrecognized(String),
}

impl DefaultModCatalog {
    /// The catalog Ephervex browses when the user has expressed no preference.
    pub const BUILT_IN: ModProvider = ModProvider::Modrinth;

    /// Reads a stored name, where empty or whitespace means "nothing recorded".
    #[must_use]
    pub fn from_stored(value: &str) -> Self {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Self::BuiltIn;
        }
        ModProvider::parse(trimmed)
            .map_or_else(|| Self::Unrecognized(trimmed.to_owned()), Self::Selected)
    }

    /// The name to store, empty when nothing is recorded.
    ///
    /// An unrecognized name round-trips unchanged, so running this build does
    /// not cost the user a preference a newer one understands.
    #[must_use]
    pub fn stored(&self) -> &str {
        match self {
            Self::BuiltIn => "",
            Self::Selected(provider) => provider.as_str(),
            Self::Unrecognized(value) => value,
        }
    }

    /// The catalog to browse, or `None` when the recorded name is unrecognized.
    ///
    /// Deliberately not falling back to [`Self::BUILT_IN`] here: a caller that
    /// substitutes has to do it in the open, where it can also tell the user
    /// that the catalog they asked for is not the one being searched.
    #[must_use]
    pub const fn resolved(&self) -> Option<ModProvider> {
        match self {
            Self::BuiltIn => Some(Self::BUILT_IN),
            Self::Selected(provider) => Some(*provider),
            Self::Unrecognized(_) => None,
        }
    }

    /// Whether the user recorded a choice, as opposed to taking the default.
    #[must_use]
    pub const fn is_chosen(&self) -> bool {
        !matches!(self, Self::BuiltIn)
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
    #[test]
    fn no_recorded_catalog_is_the_built_in_default_rather_than_a_choice() {
        let absent = DefaultModCatalog::from_stored("");
        assert_eq!(absent, DefaultModCatalog::BuiltIn);
        assert!(!absent.is_chosen());
        assert_eq!(absent.resolved(), Some(ModProvider::Modrinth));
        assert_eq!(absent.stored(), "");
        assert_eq!(DefaultModCatalog::from_stored("   "), absent);
    }

    #[test]
    fn a_recorded_catalog_stays_a_choice_even_when_it_matches_the_default() {
        let chosen = DefaultModCatalog::from_stored("modrinth");
        assert_eq!(chosen, DefaultModCatalog::Selected(ModProvider::Modrinth));
        assert!(chosen.is_chosen());
        assert_ne!(chosen, DefaultModCatalog::BuiltIn);
        assert_eq!(chosen.resolved(), DefaultModCatalog::BuiltIn.resolved());
    }

    #[test]
    fn a_catalog_name_this_build_does_not_know_resolves_to_nothing_and_is_kept() {
        let future = DefaultModCatalog::from_stored("a-later-catalog");
        assert_eq!(
            future,
            DefaultModCatalog::Unrecognized("a-later-catalog".to_owned())
        );
        assert!(future.is_chosen());
        assert_eq!(
            future.resolved(),
            None,
            "an unknown catalog must not read as one this build happens to have"
        );
        assert_eq!(future.stored(), "a-later-catalog");
    }
}
