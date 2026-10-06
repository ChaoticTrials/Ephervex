//! The normalized source recipe.
//!
//! # The one rule this module exists to enforce
//!
//! > Do not infer server state only from the mutated client directory.
//!
//! An imported client is a directory a user then edits: they delete a mod to
//! bisect a crash, drop a debug build in, and change a config. Reconstructing
//! the server from that directory later reconstructs their debugging session
//! rather than the pack. So an import records what the *pack* said - every
//! file, its hashes, its download addresses, and its client **and** server
//! environment - and a server materialized months later is built from that
//! record without the original archive being present.
//!
//! # Why this model has no Modrinth in it
//!
//! Provider-specific formats are not the shape of the problem. Modrinth names
//! files by logical path and supplies download addresses, while CurseForge
//! names files by project and file id and resolves their addresses through an
//! API. Both have to land in the same normalized structure, because
//! everything downstream - the installer, "Create Server", the instance UI -
//! is written against this type and must not learn a provider's vocabulary.
//!
//! Provider-specific state therefore lives in exactly two places, both of them
//! quarantined: [`ProviderFileIdentity`], whose fields are optional because no
//! provider fills all of them, and [`ProviderRecipeMetadata`], which points at
//! the verbatim original manifest and carries whatever else a provider needs as
//! opaque JSON. Nothing in the launcher branches on the contents of either.
//!
//! # Why this describes an instance rather than an archive
//!
//! A recipe starts as what an import learned, but it is not frozen there. A
//! mod installed later through the mod browser is part of what this instance is
//! *supposed to contain*, and a server built from the recipe has to include it.
//! Keeping such a mod anywhere else would mean two records of the same `mods/`
//! directory that can disagree, so managed mods are entries here, told apart
//! from the pack's own files by [`PackFileRecipe::origin`]. An instance created
//! by hand acquires a recipe the first time something is installed into it
//! ([`PackRecipe::for_manual_instance`]).
//!
//! What is still *not* here: a jar the user dropped into `mods/` themselves.
//! Nothing scans that directory, and a file with no provenance is left alone
//! rather than given an invented record.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use ephervex_model::{
    Hash, ImportSourceKind, LoaderSelection, ModOrigin, ModProvider, ModRelationKind,
};
use serde::{Deserialize, Serialize};

use crate::error::{PackError, PackResult};

/// The recipe format version, stamped into `recipe.json`.
///
/// Separate from the instance metadata version: a recipe is copied into a
/// server when one is created from a client, so it travels independently of the
/// record that referenced it.
///
/// Frozen at 1 until Ephervex's 1.0.0 release (ADR 0033: internal format
/// versions do not advance before then). A recipe-shape change made while this
/// is frozen has to stay readable by every other pre-1.0.0 build - a new or
/// changed field defaults when absent - because there is no version bump left
/// to signal that an older build should refuse the file instead of misreading
/// it. A change that cannot be made that way is not made; re-import is the
/// recovery path for an instance whose on-disk recipe predates it.
pub const RECIPE_VERSION: u32 = 1;

/// The extension a mod loader loads out of `mods/`.
///
/// Compared case-insensitively. A pack author who ships `Mod.JAR` has shipped a
/// mod; one who ships `mod.jar.disabled` deliberately has not.
const MOD_EXTENSION: &str = "jar";

/// Whether one logical path is a mod jar of the instance.
///
/// It must sit directly in `mods/`, which is the one directory a loader reads,
/// and it must actually be a `.jar`, because packs also ship a `README.txt`, a
/// generated `metadata.json`, or a `.disabled` jar into that directory and
/// none of those is a mod the instance is running.
///
/// The extension is the whole test. Nothing here opens the file: deciding what
/// a jar contains would mean parsing an archive out of an untrusted pack to
/// answer a question about presentation.
#[must_use]
pub fn is_mod_file_path(path: &Path) -> bool {
    path.parent() == Some(Path::new(crate::managed::MODS_DIR))
        && path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case(MOD_EXTENSION))
}

/// Whether one side of the game needs a file.
///
/// Four states rather than a boolean. "Optional" is a genuine third answer that
/// the UI has to offer and the recipe has to remember - collapsing it either
/// way silently changes what gets installed.
///
/// [`Unknown`](Self::Unknown) is the fourth, and it is not a synonym for any of
/// the others. It means the source declared nothing about this side. A
/// `.mrpack` cannot produce it: that format has three environment values, and
/// an *omitted* `env` object is defined to mean required on both sides. A
/// Modrinth project can, because its `client_side`/`server_side` fields have an
/// `unknown` value an author may leave in place. Reading that as `Required`
/// would assert something the author declined to assert - and would put the mod
/// into a reconstructed server on that assertion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SideRequirement {
    Required,
    Optional,
    Unsupported,
    /// The source says nothing about this side. Never installed automatically;
    /// distinct from `Optional` so "the author said optional" and "the author
    /// said nothing" stay tellable apart.
    Unknown,
}

impl SideRequirement {
    /// Whether a file with this requirement is installed without asking.
    ///
    /// False for `Unknown`: nothing is installed on the strength of an
    /// undeclared value.
    #[must_use]
    pub const fn is_automatic(self) -> bool {
        matches!(self, Self::Required)
    }

    /// Whether a file with this requirement may ever be installed on this side.
    #[must_use]
    pub const fn is_installable(self) -> bool {
        !matches!(self, Self::Unsupported)
    }

    /// Whether installing on this side waits for an explicit decision.
    #[must_use]
    pub const fn needs_decision(self) -> bool {
        matches!(self, Self::Optional | Self::Unknown)
    }

    /// The `ephervex-i18n` message id naming this requirement to a user.
    #[must_use]
    pub const fn message_id(self) -> &'static str {
        match self {
            Self::Required => "pack-requirement-required",
            Self::Optional => "pack-requirement-optional",
            Self::Unsupported => "pack-requirement-unsupported",
            Self::Unknown => "pack-requirement-unknown",
        }
    }

    /// A stable identifier for `--json` output and the recipe file.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Required => "required",
            Self::Optional => "optional",
            Self::Unsupported => "unsupported",
            Self::Unknown => "unknown",
        }
    }
}

/// Which side of the game a plan is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackSide {
    Client,
    Server,
}

impl PackSide {
    /// A stable identifier for CLI arguments and `--json` output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Client => "client",
            Self::Server => "server",
        }
    }

    /// Parses the identifier back.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "client" => Some(Self::Client),
            "server" => Some(Self::Server),
            _ => None,
        }
    }

    /// The override layer that applies to this side after the common one
    #[must_use]
    pub const fn override_scope(self) -> OverrideScope {
        match self {
            Self::Client => OverrideScope::Client,
            Self::Server => OverrideScope::Server,
        }
    }
}

/// How a provider names a file in its own catalog.
///
/// Every field is optional because no provider supplies all of them: a Modrinth
/// pack index identifies a file only by hash and download address, while a
/// CurseForge manifest identifies it by project and file id and has no address
/// at all until the API resolves one. Keeping both shapes in one optional
/// structure is what lets the mod list, dependency handling, and update checks
/// be shared by all importers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderFileIdentity {
    /// Which catalog the ids below belong to.
    ///
    /// `None` for a file whose identity came from a pack manifest that names no
    /// catalog - a Modrinth pack index gives a path and hashes and nothing to
    /// resolve a project against. Absence is the honest answer there, and it is
    /// what keeps such a file from being mistaken for a managed mod.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<ModProvider>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_id: Option<String>,
}

impl ProviderFileIdentity {
    /// Whether this identity names anything at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.provider.is_none()
            && self.project_id.is_none()
            && self.version_id.is_none()
            && self.file_id.is_none()
    }

    /// The provider and project this identity resolves against, if it names
    /// both.
    ///
    /// The pair the mod browser keys on. Half an identity resolves nothing, so
    /// this returns nothing rather than letting a caller improvise.
    #[must_use]
    pub fn project(&self) -> Option<(ModProvider, &str)> {
        Some((self.provider?, self.project_id.as_deref()?))
    }

    /// How much of this file the launcher can actually prove.
    #[must_use]
    pub fn resolution(&self) -> ModIdentity {
        match (self.provider, self.project_id.as_deref()) {
            (Some(provider), Some(project_id)) => match self.version_id.as_deref() {
                Some(version_id) => ModIdentity::Version {
                    provider,
                    project_id: project_id.to_owned(),
                    version_id: version_id.to_owned(),
                    file_id: self.file_id.clone(),
                },
                None => ModIdentity::Project {
                    provider,
                    project_id: project_id.to_owned(),
                },
            },
            _ => ModIdentity::Unresolved,
        }
    }
}

/// How completely a mod's provider identity is known.
///
/// The three states are what stop a partially identified file from being
/// presented as a fully resolvable one. A `.mrpack` imported by an older build,
/// a pack whose download address names a project but not a version, and a jar
/// with no provenance at all are three different situations, and each has a
/// different honest answer to "which version is this?".
///
/// Nothing here is ever inferred from a file name: a name is a label, not a
/// catalog entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModIdentity {
    /// Provider, project, and the exact catalog version are all known, so the
    /// mod can be looked up again without reading the jar.
    Version {
        provider: ModProvider,
        project_id: String,
        version_id: String,
        /// The provider's own file id, where it has one distinct from the
        /// version. Modrinth does not; CurseForge will.
        file_id: Option<String>,
    },
    /// The project is known but the exact version is not. Enough to say what
    /// the mod *is*, never enough to claim which release is installed.
    Project {
        provider: ModProvider,
        project_id: String,
    },
    /// Nothing the launcher can re-resolve. The file is still installed and
    /// still belongs to the instance; only its provenance is unknown.
    Unresolved,
}

impl ModIdentity {
    /// The catalog this mod resolves against, when one is known.
    #[must_use]
    pub const fn provider(&self) -> Option<ModProvider> {
        match self {
            Self::Version { provider, .. } | Self::Project { provider, .. } => Some(*provider),
            Self::Unresolved => None,
        }
    }

    #[must_use]
    pub fn project_id(&self) -> Option<&str> {
        match self {
            Self::Version { project_id, .. } | Self::Project { project_id, .. } => Some(project_id),
            Self::Unresolved => None,
        }
    }

    #[must_use]
    pub fn version_id(&self) -> Option<&str> {
        match self {
            Self::Version { version_id, .. } => Some(version_id),
            Self::Project { .. } | Self::Unresolved => None,
        }
    }

    /// Whether an exact catalog version is known.
    #[must_use]
    pub const fn is_fully_resolved(&self) -> bool {
        matches!(self, Self::Version { .. })
    }

    /// A stable identifier for `--json` output and frontend state.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Version { .. } => "version",
            Self::Project { .. } => "project",
            Self::Unresolved => "unresolved",
        }
    }
}

/// One file a pack declares that its own manifest cannot fully describe.
///
/// Some pack formats name a file by catalog id and nothing else: a CurseForge
/// export declares a project id and a file id, and the file's name, size,
/// hashes, and download address exist only in that provider's API. Such an
/// entry cannot be a [`PackFileRecipe`] yet - a recipe entry names a
/// destination and carries the hashes its download is verified against, and
/// inventing either from an id would be inventing a fact.
///
/// So it waits here instead. Analysis can still count these, report which side
/// they belong to, and say that resolving them needs the provider, all without
/// a credential and without a network. Resolving one produces a real
/// [`PackFileRecipe`]; nothing else in the launcher ever sees this type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingProviderFile {
    /// The exact catalog identity the pack declared. Durable: it is what the
    /// resolved recipe entry records, so the recipe keeps saying which file of
    /// which project the pack asked for.
    pub identity: ProviderFileIdentity,
    /// What the pack's own inclusion rules make this on the client.
    pub client: SideRequirement,
    /// What the pack declares about servers, which for a format that says
    /// nothing is [`SideRequirement::Unknown`].
    pub server: SideRequirement,
}

impl PendingProviderFile {
    /// What this entry's requirement is on one side.
    #[must_use]
    pub const fn requirement(&self, side: PackSide) -> SideRequirement {
        match side {
            PackSide::Client => self.client,
            PackSide::Server => self.server,
        }
    }

    /// The provider, project, and release this entry names, when it names all
    /// three.
    ///
    /// The triple a resolver needs. Half an identity resolves nothing, so this
    /// returns nothing rather than letting a caller improvise.
    #[must_use]
    pub fn resolvable(&self) -> Option<(ModProvider, &str, &str)> {
        Some((
            self.identity.provider?,
            self.identity.project_id.as_deref()?,
            self.identity.version_id.as_deref()?,
        ))
    }
}

/// A [`PendingProviderFile`] the provider answered about but declined to
/// serve, because its author turned off third-party distribution.
///
/// Not an error: a pack import that meets one of these does not abort. It
/// stays here for a caller to display, offer a manual-download link for, or
/// let the user explicitly skip. Nothing in this type is invented - every
/// field is either the pack's own declaration or something the provider said
/// about the exact file it refused, which is why `project_name`, `file_name`,
/// and `browser_url` are all optional rather than defaulted to a raw id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockedProviderFile {
    /// The same identity a resolved [`PendingProviderFile`] carries, so a
    /// caller's skip/found decision can be matched back to the pack's own
    /// declaration.
    pub identity: ProviderFileIdentity,
    pub project_name: Option<String>,
    pub file_name: Option<String>,
    /// A link that starts this exact file's download on the provider's own
    /// site, when enough is known to build one.
    pub browser_url: Option<String>,
    /// The provider's own digest(s) for the file, so a hand-placed copy can
    /// be recognized without trusting anything about where it came from.
    pub hashes: Vec<Hash>,
    /// What the provider said this file needs from other projects.
    ///
    /// `Some` - including `Some(vec![])` - is knowledge: the provider was
    /// asked and answered. `None` means nothing ever described this file's
    /// relations. A refusal to serve the bytes is not a refusal to describe
    /// them, so a blocked file whose document was read carries `Some`, and
    /// the recipe entry it eventually becomes records the same distinction.
    pub relations: Option<Vec<RecipeRelation>>,
    pub client: SideRequirement,
    pub server: SideRequirement,
}

/// Why one of two files a manifest declared for the same project was kept
/// over the other, for [`DuplicateProviderFile`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DuplicateFileReason {
    /// The kept file's own declared Minecraft versions name the pack's exact
    /// target; the discarded file's did not.
    MatchesMinecraftVersion,
    /// Neither candidate's declared Minecraft versions named the pack's exact
    /// target (or more than one did equally), so the more recently published
    /// file was kept.
    MoreRecentlyPublished,
}

/// Two files a manifest declared for the same project, and which one was
/// kept.
///
/// Only produced for a CurseForge manifest naming one project under two
/// different file ids at once - typically a pack updated without trimming a
/// stale entry (ADR 0031). Not an error and not a reason to refuse the pack:
/// the file that best fits is kept automatically, and this is purely
/// informational, for a caller to show before the user commits to importing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateProviderFile {
    /// The identity of the file that was kept and will be installed.
    pub kept: ProviderFileIdentity,
    /// The identity of the file that was discarded.
    pub discarded: ProviderFileIdentity,
    pub project_name: Option<String>,
    pub kept_file_name: Option<String>,
    pub discarded_file_name: Option<String>,
    pub reason: DuplicateFileReason,
}

/// What a catalog named when it stated a relation.
///
/// Two shapes because the catalogs give two. CurseForge always names a project
/// and never a release. Modrinth usually names a project, may pin a release
/// with it, and may name only a jar. A relation that names neither cannot be
/// matched against anything and is not representable here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationTarget {
    /// A project, plus the exact release where the catalog pinned one.
    Project {
        project_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version_id: Option<String>,
    },
    /// A jar, which is all the catalog gave.
    File { file_name: String },
}

impl RelationTarget {
    /// The target a catalog's own optional fields describe, if any.
    #[must_use]
    pub fn from_declared(
        project_id: Option<String>,
        version_id: Option<String>,
        file_name: Option<String>,
    ) -> Option<Self> {
        project_id.map_or_else(
            || file_name.map(|file_name| Self::File { file_name }),
            |project_id| {
                Some(Self::Project {
                    project_id,
                    version_id,
                })
            },
        )
    }

    /// The project, where one was named.
    #[must_use]
    pub fn project_id(&self) -> Option<&str> {
        match self {
            Self::Project { project_id, .. } => Some(project_id),
            Self::File { .. } => None,
        }
    }

    /// Whatever the catalog gave, as something to show.
    #[must_use]
    pub fn display_name(&self) -> &str {
        match self {
            Self::Project { project_id, .. } => project_id,
            Self::File { file_name } => file_name,
        }
    }
}

/// One relation a catalog stated about a file, as the catalog stated it.
///
/// The catalog is the entry's own [`ProviderFileIdentity::provider`], not a
/// field here: a dependency graph never crosses catalogs, so a second place to
/// record a provider would be a second place for it to be wrong.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecipeRelation {
    pub kind: ModRelationKind,
    pub target: RelationTarget,
}

impl RecipeRelation {
    /// Whether this relation is about one particular file of the instance.
    ///
    /// A relation that named a project matches by project alone: falling back
    /// to the jar name for it would match a file the catalog never mentioned.
    /// One that named only a jar matches by that name, case-insensitively,
    /// because that is the whole of what the catalog gave.
    #[must_use]
    pub fn names(&self, project_id: Option<&str>, file_name: &str) -> bool {
        match &self.target {
            RelationTarget::Project {
                project_id: named, ..
            } => project_id == Some(named.as_str()),
            RelationTarget::File { file_name: named } => named.eq_ignore_ascii_case(file_name),
        }
    }
}

/// One file the pack declares (the project's `PackFileRecipe`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackFileRecipe {
    /// Where the file goes, relative to the game directory. Already validated:
    /// it cannot escape, and it cannot name launcher-owned metadata.
    pub logical_path: PathBuf,
    /// Every hash the provider published for it. A mismatch on any of them is a
    /// hard failure.
    pub hashes: Vec<Hash>,
    /// Download addresses, in the order the provider listed them, already
    /// checked against the URL policy.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub downloads: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    pub client: SideRequirement,
    pub server: SideRequirement,
    /// What the user chose for this file when it was optional, if they were
    /// asked. `None` means "nobody has decided", which is what a server created
    /// later needs to know so it can prompt rather than assume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected: Option<bool>,
    /// Why this file is in the instance's desired state. Defaults to
    /// `PackDeclared`, which is what every entry a pack import writes is and
    /// what every recipe written before managed mods existed contains.
    #[serde(default)]
    pub origin: ModOrigin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<ProviderFileIdentity>,
    /// The release the imported pack originally declared for this file, once a
    /// mutation has moved it away from that release.
    ///
    /// `None` for a mod never mutated away from what the pack declared - in
    /// which case `identity` itself already *is* the pack's version - and for
    /// anything not [`ModOrigin::PackDeclared`]. Set once, by the mutation
    /// that first changes a pack-owned entry's identity, and never overwritten
    /// after that, so it always names what the pack originally shipped rather
    /// than whatever it was most recently changed from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_identity: Option<ProviderFileIdentity>,
    /// What this file's catalog said it needs (ADR 0034).
    ///
    /// `None` means nothing has asked the catalog yet. `Some([])` means it
    /// answered that this file needs nothing. Collapsing the two would make an
    /// unasked file look independent, and a warning built on that is
    /// confidently wrong rather than merely incomplete.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relations: Option<Vec<RecipeRelation>>,
}

impl PackFileRecipe {
    /// The relations recorded for this file, or nothing when none were.
    ///
    /// An empty slice and an absent answer are different facts; see
    /// [`Self::relations`].
    #[must_use]
    pub fn recorded_relations(&self) -> Option<&[RecipeRelation]> {
        self.relations.as_deref()
    }

    /// The jar's own name, as the catalog would have named it.
    #[must_use]
    pub fn file_name(&self) -> &str {
        self.logical_path
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or_default()
    }

    /// Whether this file declares that it does not load without `other`.
    ///
    /// False for a file whose relations were never recorded, so a caller that
    /// needs to tell "does not require it" from "nobody asked" checks
    /// [`Self::recorded_relations`] first.
    #[must_use]
    pub fn requires(&self, other: &Self) -> bool {
        let project_id = other
            .identity
            .as_ref()
            .and_then(|identity| identity.project_id.as_deref());
        self.recorded_relations().is_some_and(|relations| {
            relations.iter().any(|relation| {
                relation.kind.is_required() && relation.names(project_id, other.file_name())
            })
        })
    }

    /// What this file's requirement is on one side.
    #[must_use]
    pub const fn requirement(&self, side: PackSide) -> SideRequirement {
        match side {
            PackSide::Client => self.client,
            PackSide::Server => self.server,
        }
    }

    /// The file's name as the pack spells it, for display.
    #[must_use]
    pub fn display_name(&self) -> String {
        self.logical_path.to_string_lossy().into_owned()
    }

    /// Whether this file is installed on a side, given a selection.
    ///
    /// `required` installs, `unsupported` never installs, and both `optional`
    /// and `unknown` install only if something explicitly selected them - the
    /// default for an unanswered one is off. An undeclared side is treated as
    /// needing a decision for the same reason an optional one is: nothing here
    /// justifies installing it on its own.
    #[must_use]
    pub fn is_installed_on(&self, side: PackSide, selection: &OptionalSelection) -> bool {
        match self.requirement(side) {
            SideRequirement::Required => true,
            SideRequirement::Unsupported => false,
            SideRequirement::Optional | SideRequirement::Unknown => {
                selection.includes(&self.logical_path, self.selected)
            }
        }
    }

    /// Whether this entry describes a jar in the game's `mods/` directory.
    /// See [`is_mod_file_path`].
    #[must_use]
    pub fn is_mod_file(&self) -> bool {
        is_mod_file_path(&self.logical_path)
    }

    /// Whether extracting the pack's own override tree is what puts this file
    /// in place.
    ///
    /// Such an entry has no download address and never will: the pack shipped
    /// the bytes rather than a reference to them. It belongs in the desired
    /// state, so the mod list is complete and a reinstall restores it, and it
    /// stays out of every download plan, where an entry with no address is a
    /// failure rather than a fact.
    #[must_use]
    pub const fn is_layer_supplied(&self) -> bool {
        matches!(self.origin, ModOrigin::PackOverride)
    }

    /// This entry as a mod of the instance, if it is one.
    ///
    /// Returns nothing for a file that is not a jar in `mods/`, so a pack's
    /// config file never appears in a mod list.
    #[must_use]
    pub fn recorded_mod(&self) -> Option<RecordedMod> {
        if !self.is_mod_file() {
            return None;
        }
        Some(RecordedMod {
            logical_path: self.logical_path.clone(),
            file_name: self
                .logical_path
                .file_name()?
                .to_string_lossy()
                .into_owned(),
            origin: self.origin,
            identity: self
                .identity
                .as_ref()
                .map_or(ModIdentity::Unresolved, ProviderFileIdentity::resolution),
            original_identity: self
                .original_identity
                .as_ref()
                .map(ProviderFileIdentity::resolution),
            relations: self.relations.clone(),
            hashes: self.hashes.clone(),
            size: self.size,
            client: self.client,
            server: self.server,
        })
    }

    /// The managed mod this entry records, if it is one.
    ///
    /// Two conditions, both necessary, the same as [`Self::is_mod_file`]
    /// checks for [`Self::recorded_mod`]: the entry must be a jar directly in
    /// `mods/`, and it must name a provider, a project, *and* a version:
    /// those are what make it re-resolvable. A provider-declared file outside
    /// `mods/`, a CurseForge Resource Pack included, can carry that same full
    /// identity for exact reconstruction and still correctly not be a managed
    /// mod: it is still installed and still part of the server plan, just not
    /// something the mod browser owns. A jar someone dropped into `mods/` has
    /// no entry here at all.
    #[must_use]
    pub fn managed_mod(&self) -> Option<ManagedMod> {
        if !self.is_mod_file() {
            return None;
        }
        let identity = self.identity.as_ref()?;
        Some(ManagedMod {
            provider: identity.provider?,
            project_id: identity.project_id.clone()?,
            version_id: identity.version_id.clone()?,
            file_name: self
                .logical_path
                .file_name()?
                .to_string_lossy()
                .into_owned(),
            hashes: self.hashes.clone(),
            origin: self.origin,
        })
    }
}

/// One mod in an instance's desired state, however much is known about it.
///
/// The difference from [`ManagedMod`] is deliberate. `ManagedMod` answers "what
/// can the mod browser re-resolve", so it exists only for a fully identified
/// mod. This answers "what mods does this instance have", which has to include
/// the ones whose provider identity is partial or absent - a pack's own files,
/// and anything imported before identity was recorded. Leaving those out is
/// what made imported pack mods invisible.
///
/// Every field is either recorded fact or absent. Nothing here is derived from
/// a file name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedMod {
    /// Where the jar lives, relative to the game directory.
    pub logical_path: PathBuf,
    /// The jar's own name. A label, and explicitly not evidence of identity.
    pub file_name: String,
    /// Why this mod belongs to the instance.
    pub origin: ModOrigin,
    /// How much of its provider identity is proven.
    pub identity: ModIdentity,
    /// The release the imported pack originally declared, when this mod has
    /// been moved away from it. See [`PackFileRecipe::original_identity`].
    pub original_identity: Option<ModIdentity>,
    /// What this mod's catalog said it needs, or nothing when nothing has
    /// asked. See [`PackFileRecipe::relations`].
    pub relations: Option<Vec<RecipeRelation>>,
    pub hashes: Vec<Hash>,
    pub size: Option<u64>,
    pub client: SideRequirement,
    pub server: SideRequirement,
}

/// A mod this instance installed through a provider.
///
/// A read model assembled from [`PackFileRecipe::managed_mod`] rather than a
/// second place the same facts are stored. Everything in it is durable: the
/// provider and ids survive a reopen, so the mod can be re-resolved without
/// reading a jar's file name and hoping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedMod {
    pub provider: ModProvider,
    pub project_id: String,
    pub version_id: String,
    pub file_name: String,
    pub hashes: Vec<Hash>,
    pub origin: ModOrigin,
}

/// Which override directory a layer came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverrideScope {
    /// Applies to both sides.
    Common,
    Client,
    Server,
}

impl OverrideScope {
    /// Whether this layer applies to a side, and in what order.
    ///
    /// The common layer is laid down first and the side-specific one second, so
    /// the later layer wins.
    #[must_use]
    pub const fn applies_to(self, side: PackSide) -> bool {
        match self {
            Self::Common => true,
            Self::Client => matches!(side, PackSide::Client),
            Self::Server => matches!(side, PackSide::Server),
        }
    }

    /// The order this layer is applied in. Lower goes first.
    #[must_use]
    pub const fn order(self) -> u8 {
        match self {
            Self::Common => 0,
            Self::Client | Self::Server => 1,
        }
    }

    /// A stable identifier for the recipe file and `--json` output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Common => "common",
            Self::Client => "client",
            Self::Server => "server",
        }
    }
}

/// One directory of files the pack ships verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverrideLayer {
    pub scope: OverrideScope,
    /// The entry prefix inside the original archive, e.g. `overrides`. Kept as
    /// the provider spelled it so the recipe describes the source faithfully.
    pub archive_prefix: String,
    /// Where the layer was retained under the instance's `.launcher/source/`,
    /// relative to that directory, or `None` when it was not retained.
    ///
    /// The common and server layers are retained because creating a server must
    /// not depend on the original archive. The client layer is already present
    /// in the game directory and is not duplicated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retained_at: Option<PathBuf>,
    /// How many files the layer contained when it was imported. Display only.
    #[serde(default)]
    pub files: u64,
}

impl OverrideLayer {
    /// Whether this layer can still be applied without the original archive.
    #[must_use]
    pub const fn is_retained(&self) -> bool {
        self.retained_at.is_some()
    }
}

/// Provider-specific state, quarantined.
///
/// The normalized model above answers every question the launcher asks. This
/// answers the questions only the provider can: where its verbatim manifest was
/// kept, and whatever else it needs to resolve its own files later. The launcher
/// stores it, copies it, and shows its `kind`; it never branches on `extra`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderRecipeMetadata {
    pub kind: ImportSourceKind,
    /// The original manifest, retained byte for byte under `.launcher/source/`
    /// and named relative to that directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_file: Option<PathBuf>,
    /// Opaque provider state. Empty for Modrinth, whose index needs nothing the
    /// normalized model does not already carry; CurseForge will use it for the
    /// server-pack relationship and the restricted-file state.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub extra: serde_json::Value,
}

impl ProviderRecipeMetadata {
    /// Metadata naming only the provider and its retained manifest.
    #[must_use]
    pub fn new(kind: ImportSourceKind, manifest_file: Option<PathBuf>) -> Self {
        Self {
            kind,
            manifest_file,
            extra: serde_json::Value::Null,
        }
    }
}

/// Everything an import learned about a pack (the project's `PackRecipe`).
///
/// Written to `.launcher/source/recipe.json` at import time and read back
/// whenever the launcher needs to know what the pack *said*, as opposed to what
/// the instance directory currently contains.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackRecipe {
    #[serde(default = "default_recipe_version")]
    pub recipe_version: u32,
    pub source_kind: ImportSourceKind,
    /// The pack's own name, as the provider spelled it. User-visible, never
    /// translated, and never used to build a path.
    pub original_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_version: Option<String>,
    /// Who the pack's own manifest credits, where the format has such a field.
    ///
    /// Purely descriptive, so it is additive rather than a recipe-format
    /// change: a build that has never heard of it reads every other field
    /// exactly as before, and a recipe written without it loads unchanged.
    /// Absent for Modrinth, whose pack index does not name an author.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// The file the pack was imported from. Display only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_name: Option<String>,
    pub minecraft_version: String,
    /// The exact loader build the pack declared. Never a floating "latest"
    pub loader: LoaderSelection,
    #[serde(default)]
    pub files: Vec<PackFileRecipe>,
    #[serde(default)]
    pub override_layers: Vec<OverrideLayer>,
    pub provider_metadata: ProviderRecipeMetadata,
}

const fn default_recipe_version() -> u32 {
    RECIPE_VERSION
}

impl PackRecipe {
    /// The files that would be installed on one side, in pack order.
    #[must_use]
    pub fn files_for(&self, side: PackSide, selection: &OptionalSelection) -> Vec<&PackFileRecipe> {
        self.files
            .iter()
            .filter(|file| !file.is_layer_supplied())
            .filter(|file| file.is_installed_on(side, selection))
            .collect()
    }

    /// The override layers that apply to one side, in application order.
    #[must_use]
    pub fn layers_for(&self, side: PackSide) -> Vec<&OverrideLayer> {
        let mut layers: Vec<&OverrideLayer> = self
            .override_layers
            .iter()
            .filter(|layer| layer.scope.applies_to(side))
            .collect();
        layers.sort_by_key(|layer| layer.scope.order());
        layers
    }

    /// Every file for this side that is waiting on a decision, in pack order.
    ///
    /// Both `optional` and `unknown` qualify: neither installs on its own, and
    /// both are questions a human has to answer. The component carries which of
    /// the two it is, because "the author said optional" and "the author said
    /// nothing" deserve different words in front of a checkbox.
    #[must_use]
    pub fn optional_components(&self, side: PackSide) -> Vec<OptionalComponent> {
        self.files
            .iter()
            .filter(|file| file.requirement(side).needs_decision())
            .map(|file| OptionalComponent {
                logical_path: file.logical_path.clone(),
                size: file.size,
                requirement: file.requirement(side),
                // default unchecked unless the pack format gains an
                // explicit default-selection mechanism. Neither Modrinth's
                // index nor a CurseForge manifest has one, so the only thing
                // that can turn this on is a recorded human decision.
                default_selected: false,
                recorded_selection: file.selected,
                opposite_side: file.requirement(match side {
                    PackSide::Client => PackSide::Server,
                    PackSide::Server => PackSide::Client,
                }),
            })
            .collect()
    }

    /// What materializing one side would install, without touching the disk.
    ///
    /// This is the type "Create Server" is built on: it is derived entirely
    /// from the retained recipe, so producing it never reads the original
    /// archive or the client's current `mods/` folder.
    #[must_use]
    pub fn plan(&self, side: PackSide, selection: &OptionalSelection) -> SidePlan<'_> {
        SidePlan {
            side,
            files: self.files_for(side, selection),
            layers: self.layers_for(side),
            minecraft_version: &self.minecraft_version,
            loader: &self.loader,
        }
    }

    /// Plans reproducible server content from recorded desired state.
    ///
    /// Required files are automatic, unsupported files are excluded, and an
    /// optional file is included only when the recipe records an affirmative
    /// choice. What happens to a file whose server side nothing declared is the
    /// caller's policy; see [`UnknownServerFiles`]. Entries left out are
    /// returned for actionable diagnostics.
    pub fn server_reconstruction_plan(
        &self,
        unknown: &UnknownServerFiles,
    ) -> PackResult<ServerReconstructionPlan<'_>> {
        if let UnknownServerFiles::OnlyConfirmed(confirmed) = unknown {
            let unknown_paths: BTreeSet<&Path> = self
                .files
                .iter()
                .filter(|file| file.server == SideRequirement::Unknown)
                .map(|file| file.logical_path.as_path())
                .collect();
            for path in confirmed.paths() {
                if !unknown_paths.contains(path) {
                    return Err(PackError::UnknownServerSelection {
                        path: path.to_path_buf(),
                    });
                }
            }
        }

        let mut files = Vec::new();
        let mut unconfirmed_unknown = Vec::new();
        for file in &self.files {
            // The retained override layer places these, so a plan that also
            // listed them would be asking for an address the pack never gave.
            if file.is_layer_supplied() {
                continue;
            }
            match file.server {
                SideRequirement::Required => files.push(file),
                SideRequirement::Optional => {
                    if file.selected == Some(true) {
                        files.push(file);
                    }
                }
                SideRequirement::Unsupported => {}
                SideRequirement::Unknown if unknown.includes(&file.logical_path) => {
                    files.push(file);
                }
                SideRequirement::Unknown => unconfirmed_unknown.push(file),
            }
        }

        Ok(ServerReconstructionPlan {
            files,
            layers: self.layers_for(PackSide::Server),
            minecraft_version: &self.minecraft_version,
            loader: &self.loader,
            unconfirmed_unknown,
        })
    }

    /// Records what the user chose for the optional files of one side.
    ///
    /// Stored on the file rather than in a side table so that a recipe copied
    /// into a server carries the decision with it, and so a later prompt can
    /// distinguish "unchecked by default" from "the user said no".
    pub fn record_selection(&mut self, side: PackSide, selection: &OptionalSelection) {
        for file in &mut self.files {
            if file.requirement(side).needs_decision() {
                file.selected = Some(selection.includes(&file.logical_path, file.selected));
            }
        }
    }

    /// A recipe for an instance that was created by hand rather than imported.
    ///
    /// An instance with no pack still has a desired state the moment a mod is
    /// installed into it, and that state has to survive a restart and reach
    /// "Create Server". Rather than inventing a second store for the
    /// no-pack case, such an instance acquires a recipe on first use: same
    /// file, same reader, same server planner.
    ///
    /// The Minecraft version and loader come from the instance record, so the
    /// recipe agrees with what the instance actually is; nothing here resolves
    /// or substitutes either.
    #[must_use]
    pub fn for_manual_instance(
        name: impl Into<String>,
        minecraft_version: impl Into<String>,
        loader: LoaderSelection,
    ) -> Self {
        Self {
            recipe_version: RECIPE_VERSION,
            source_kind: ImportSourceKind::Manual,
            original_name: name.into(),
            original_version: None,
            author: None,
            summary: None,
            archive_name: None,
            minecraft_version: minecraft_version.into(),
            loader,
            files: Vec::new(),
            override_layers: Vec::new(),
            provider_metadata: ProviderRecipeMetadata::new(ImportSourceKind::Manual, None),
        }
    }

    /// Every recorded file that does not load without `target`.
    ///
    /// The reverse of what each entry stores. Edges are kept forward, on the
    /// entry the catalog described, so installing or removing one mod never has
    /// to rewrite another mod's record; the direction a warning needs is
    /// derived here instead (ADR 0034).
    ///
    /// Proven only. A file whose relations were never recorded is absent from
    /// this list and is not thereby independent - [`PackRecipe::files`] with
    /// [`PackFileRecipe::recorded_relations`] is what answers that.
    #[must_use]
    pub fn required_by<'a>(&'a self, target: &PackFileRecipe) -> Vec<&'a PackFileRecipe> {
        self.files
            .iter()
            .filter(|file| file.requires(target))
            .collect()
    }

    /// Every mod this instance's client actually has, in recipe order.
    ///
    /// The list a mod view is built from. It covers the pack's own mods and
    /// the ones installed later through the browser alike, because they are the
    /// same thing to a user looking at `mods/` - what tells them apart is
    /// [`RecordedMod::origin`], not whether they appear at all.
    ///
    /// A mod the pack declares but that this client did not take is left out:
    /// an unselected optional file is not installed, and listing it would
    /// claim a jar that is not there. The recorded selection is what decides
    /// that, so this is a pure function of the recipe and never reads `mods/`.
    #[must_use]
    pub fn recorded_mods(&self) -> Vec<RecordedMod> {
        self.recorded_mods_for(PackSide::Client)
    }

    /// The same list for either side of the game.
    ///
    /// A server keeps its own recipe, and a decision recorded there is about
    /// the server: an entry the client takes and the server does not is not one
    /// of the server's mods, and a managed-mod operation on that server must
    /// not treat it as one.
    #[must_use]
    pub fn recorded_mods_for(&self, side: PackSide) -> Vec<RecordedMod> {
        let selection = OptionalSelection::none();
        self.files
            .iter()
            .filter(|file| file.is_installed_on(side, &selection))
            .filter_map(PackFileRecipe::recorded_mod)
            .collect()
    }

    /// Every mod installed through a provider, in recipe order.
    #[must_use]
    pub fn managed_mods(&self) -> Vec<ManagedMod> {
        self.files
            .iter()
            .filter_map(PackFileRecipe::managed_mod)
            .collect()
    }

    /// The entry recording one provider's project, if this instance has it.
    #[must_use]
    pub fn managed_project(
        &self,
        provider: ModProvider,
        project_id: &str,
    ) -> Option<&PackFileRecipe> {
        self.files.iter().find(|file| {
            file.identity
                .as_ref()
                .and_then(ProviderFileIdentity::project)
                .is_some_and(|(kind, id)| kind == provider && id == project_id)
        })
    }

    /// Fills in what a recorded entry turned out to be, without changing whose
    /// it is.
    ///
    /// For an entry the launcher placed but the pack gave no identity for: a
    /// digest lookup can say which project and release a jar is, and that is
    /// worth keeping, but it does not make the file anybody else's. Returns
    /// whether anything changed, and never overwrites an identity already
    /// recorded.
    pub fn record_identity(&mut self, path: &Path, identity: ProviderFileIdentity) -> bool {
        let Some(file) = self
            .files
            .iter_mut()
            .find(|file| file.logical_path == path && file.identity.is_none())
        else {
            return false;
        };
        file.identity = Some(identity);
        true
    }

    /// Records the mods the pack shipped inside its override layers.
    ///
    /// Takes every layer's jars at once, because what a path means depends on
    /// all the layers that place it. Re-importing the same pack leaves the
    /// recipe byte-identical. No provider identity is required, because an
    /// override jar has none: the pack shipped the file rather than a
    /// reference to a catalog entry, and none can be invented from a file name.
    ///
    /// Side requirements follow the layers, which are the only thing the pack
    /// said about where the file belongs. A path shipped by several layers is
    /// required on every side one of them applies to.
    ///
    /// A layer's bytes win over a declared file at the same path on the sides
    /// the layer applies to. When layers win on every side a declared file
    /// applies to, its entry stops describing the declared release and becomes
    /// a shipped mod. Every entry declared at the path is reconciled this way
    /// and they collapse into one. When the declared file would still be installed on some
    /// side, one entry would have to describe different bytes per side, so the
    /// pack is refused with [`PackError::OverrideShadowsDeclaredFile`].
    pub fn record_layer_mods(&mut self, shipped: &[(PathBuf, OverrideScope)]) -> PackResult<()> {
        let mut sides: Vec<(&PathBuf, [bool; 2])> = Vec::new();
        let mut positions: HashMap<&PathBuf, usize> = HashMap::new();
        for (path, scope) in shipped {
            let position = *positions.entry(path).or_insert_with(|| {
                sides.push((path, [false; 2]));
                sides.len() - 1
            });
            sides[position].1[0] |= scope.applies_to(PackSide::Client);
            sides[position].1[1] |= scope.applies_to(PackSide::Server);
        }

        let shipped_mod = |path: &PathBuf, client: bool, server: bool| {
            let required = |shipped: bool| {
                if shipped {
                    SideRequirement::Required
                } else {
                    SideRequirement::Unsupported
                }
            };
            PackFileRecipe {
                logical_path: path.clone(),
                hashes: Vec::new(),
                downloads: Vec::new(),
                size: None,
                client: required(client),
                server: required(server),
                selected: None,
                origin: ModOrigin::PackOverride,
                identity: None,
                original_identity: None,
                relations: None,
            }
        };
        // Refused before anything changes, so a refusal leaves the recipe as it was.
        for &(path, [client, server]) in &sides {
            let shadowed = self.files.iter().any(|file| {
                &file.logical_path == path
                    && file.origin != ModOrigin::PackOverride
                    && (file.origin != ModOrigin::PackDeclared
                        || (file.client != SideRequirement::Unsupported && !client)
                        || (file.server != SideRequirement::Unsupported && !server))
            });
            if shadowed {
                return Err(PackError::OverrideShadowsDeclaredFile { path: path.clone() });
            }
        }

        for (path, [client, server]) in sides {
            // Every entry at the path is reconciled, not only the first: a pack
            // can declare one path twice, once per side.
            let matching: Vec<usize> = self
                .files
                .iter()
                .enumerate()
                .filter(|(_, file)| &file.logical_path == path)
                .map(|(index, _)| index)
                .collect();
            let Some((&first, rest)) = matching.split_first() else {
                self.files.push(shipped_mod(path, client, server));
                continue;
            };
            if matching
                .iter()
                .all(|&index| self.files[index].origin == ModOrigin::PackOverride)
            {
                for &index in &matching {
                    if client {
                        self.files[index].client = SideRequirement::Required;
                    }
                    if server {
                        self.files[index].server = SideRequirement::Required;
                    }
                }
                continue;
            }
            self.files[first] = shipped_mod(path, client, server);
            for &index in rest.iter().rev() {
                self.files.remove(index);
            }
        }
        Ok(())
    }

    /// Adds a managed mod to the desired state.
    ///
    /// **Insert-only, on purpose.** If the project is already recorded at a
    /// different version this refuses rather than replacing. An upsert would
    /// have to decide what happens to the old, differently-named jar, and the
    /// honest answer - delete it - is a mod *update*, which this build does not
    /// implement. Enforcing that here rather than only at the call site means
    /// the "two jars for one project" state cannot be reached at all, including
    /// by a future caller who has forgotten the rule.
    ///
    /// Re-recording the identical version is a no-op, so an install that turns
    /// out to be redundant leaves the recipe byte-identical.
    pub fn record_managed(&mut self, file: PackFileRecipe) -> PackResult<()> {
        let Some((provider, project_id)) = file
            .identity
            .as_ref()
            .and_then(ProviderFileIdentity::project)
            .map(|(provider, id)| (provider, id.to_owned()))
        else {
            return Err(PackError::ManagedIdentityMissing {
                path: file.display_name(),
            });
        };

        if let Some(existing) = self.managed_project(provider, &project_id) {
            let installed = existing
                .identity
                .as_ref()
                .and_then(|identity| identity.version_id.clone())
                .unwrap_or_default();
            let requested = file
                .identity
                .as_ref()
                .and_then(|identity| identity.version_id.clone())
                .unwrap_or_default();
            if installed != requested {
                return Err(PackError::ManagedVersionConflict {
                    provider,
                    project_id,
                    installed,
                    requested,
                });
            }
            return Ok(());
        }

        self.files.push(file);
        Ok(())
    }

    /// Drops the record of one hand-placed jar.
    ///
    /// The counterpart to the adoption that wrote it: a jar somebody copied in
    /// is recorded only so its identity survives a restart, and once the file
    /// is gone that record describes nothing. Leaving it behind is what turns a
    /// deleted jar into a row for a mod the instance cannot produce.
    ///
    /// Deliberately narrow. Only [`ModOrigin::HandPlaced`] is forgettable here,
    /// because only a hand-placed jar is the user's to delete outright;
    /// everything else is the launcher's desired state, and taking an entry out
    /// of that is a mod removal, which resolves dependencies and retires files.
    /// Returns whether anything was forgotten.
    pub fn forget_hand_placed(&mut self, logical_path: &Path) -> bool {
        let before = self.files.len();
        self.files.retain(|file| {
            file.logical_path != logical_path || file.origin != ModOrigin::HandPlaced
        });
        self.files.len() != before
    }

    /// Drops the record of every mod whose jar is not in `game_dir`.
    ///
    /// For a copy, never for the instance the copy was made from: a recipe
    /// describes desired state, and a jar deleted while bisecting is a jar the
    /// instance still wants back (ADR 0013). What is true of the original is
    /// not automatically true of something built from it, which is the whole
    /// reason this is a decision somebody makes rather than a rule.
    ///
    /// Mods only. A pack's `config/` file is not a mod, and whether it is on
    /// disk is not this question. Both spellings count as present, so a mod
    /// the user turned off is kept.
    ///
    /// Returns what it forgot, so a caller can say so.
    pub fn forget_absent_files(&mut self, game_dir: &Path, side: PackSide) -> Vec<PathBuf> {
        let selection = OptionalSelection::none();
        let mut forgotten = Vec::new();
        self.files.retain(|file| {
            if !file.is_mod_file() || !file.is_installed_on(side, &selection) {
                return true;
            }
            let installed = game_dir.join(&file.logical_path);
            let mut disabled = installed.clone().into_os_string();
            disabled.push(crate::managed::DISABLED_SUFFIX);
            if installed.is_file() || Path::new(&disabled).is_file() {
                return true;
            }
            forgotten.push(file.logical_path.clone());
            false
        });
        forgotten
    }

    /// Whether any file is optional on either side, i.e. whether any optional
    /// step is worth showing at all.
    #[must_use]
    pub fn has_optional_files(&self) -> bool {
        self.files
            .iter()
            .any(|file| file.client.needs_decision() || file.server.needs_decision())
    }
}

/// One file awaiting a decision, as the import wizard's step 4 needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionalComponent {
    pub logical_path: PathBuf,
    pub size: Option<u64>,
    /// Whether this side is `optional` or `unknown`. Both wait for an answer;
    /// only one of them is a statement the author actually made.
    pub requirement: SideRequirement,
    /// Always false today. Present as a field rather than a constant because
    /// a future pack format may encode a default selection directly.
    pub default_selected: bool,
    /// What a previous import recorded, if this component has been decided
    /// before.
    pub recorded_selection: Option<bool>,
    /// What the *other* side thinks of this file, which is what makes a
    /// checkbox meaningful: "optional on the client, required on the server" is
    /// a different decision from "optional on both".
    pub opposite_side: SideRequirement,
}

impl OptionalComponent {
    /// Whether this component should start ticked.
    #[must_use]
    pub fn starts_selected(&self) -> bool {
        self.recorded_selection.unwrap_or(self.default_selected)
    }

    #[must_use]
    pub fn display_name(&self) -> String {
        self.logical_path.to_string_lossy().into_owned()
    }
}

/// What to do with a pack file whose server applicability nothing declared.
///
/// # Why the default reconstructs them
///
/// A CurseForge `manifest.json` says only that a file belongs to the pack, and
/// the CurseForge API has no client/server field either, so every file of a
/// CurseForge pack reaches a recipe as [`SideRequirement::Unknown`]. Skipping
/// those produced a linked server holding none of the pack it was created from,
/// with the omission reported only as a general warning on the record.
///
/// A linked server exists to mirror its client. Reconstructing an undeclared
/// file is recoverable, because a mod that does not belong on a server is one
/// the user can delete; omitting it is not, because nothing on the server says
/// what is missing. So the default is to reconstruct, and a caller that wants
/// the stricter behavior asks for it.
///
/// This concerns `Unknown` alone. A file whose provider declared
/// [`SideRequirement::Unsupported`] is still excluded, and a declared optional
/// still needs its recorded choice.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum UnknownServerFiles {
    /// Reconstruct every file with no declared server side.
    #[default]
    IncludeAll,
    /// Reconstruct only the named paths, reporting the rest as unconfirmed.
    OnlyConfirmed(OptionalSelection),
}

impl UnknownServerFiles {
    /// Only the named paths, which is also how "none of them" is spelled.
    #[must_use]
    pub fn only(selection: OptionalSelection) -> Self {
        Self::OnlyConfirmed(selection)
    }

    /// Whether a file with no declared server side is reconstructed.
    #[must_use]
    pub fn includes(&self, path: &Path) -> bool {
        match self {
            Self::IncludeAll => true,
            // `None` rather than the file's recorded choice: what a client
            // import decided about an optional file says nothing about a side
            // the pack never declared.
            Self::OnlyConfirmed(confirmed) => confirmed.includes(path, None),
        }
    }
}

/// Which optional files a caller wants.
///
/// Deliberately not a `Vec<bool>` parallel to the file list: selections are
/// made by path in a UI, on a CLI, and in a recipe read back months later, and
/// a positional encoding would silently shift if the pack were re-imported.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OptionalSelection {
    paths: BTreeSet<PathBuf>,
    /// Take every optional file. Exists for `--all-optional`, which is the only
    /// honest way to spell "I know what this pack is" on a command line.
    all: bool,
}

impl OptionalSelection {
    /// Nothing selected - the product default.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// Every optional file.
    #[must_use]
    pub fn all() -> Self {
        Self {
            paths: BTreeSet::new(),
            all: true,
        }
    }

    /// The named optional files, by their logical path inside the game
    /// directory.
    #[must_use]
    pub fn of<I, P>(paths: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        Self {
            paths: paths.into_iter().map(Into::into).collect(),
            all: false,
        }
    }

    /// Whether this selection covers a file, falling back to what a previous
    /// import recorded for it.
    #[must_use]
    pub fn includes(&self, path: &Path, recorded: Option<bool>) -> bool {
        if self.all {
            return true;
        }
        if self.paths.contains(path) {
            return true;
        }
        // Nothing was said about this file now, so what was said at import time
        // stands. That is what lets a server created later follow "recorded
        // import selection... rules" without being asked again.
        recorded.unwrap_or(false)
    }

    /// Whether anything was explicitly asked for.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self.all && self.paths.is_empty()
    }

    /// The explicitly selected paths, sorted.
    pub fn paths(&self) -> impl ExactSizeIterator<Item = &Path> {
        self.paths.iter().map(PathBuf::as_path)
    }
}

/// What materializing one side of a pack involves.
///
/// Borrowed from the recipe rather than cloned: it is a view for an installer
/// or a UI, and the recipe outlives both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidePlan<'a> {
    pub side: PackSide,
    pub files: Vec<&'a PackFileRecipe>,
    pub layers: Vec<&'a OverrideLayer>,
    pub minecraft_version: &'a str,
    pub loader: &'a LoaderSelection,
}

/// The conservative, provider-neutral plan used to reconstruct a server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerReconstructionPlan<'a> {
    pub files: Vec<&'a PackFileRecipe>,
    pub layers: Vec<&'a OverrideLayer>,
    pub minecraft_version: &'a str,
    pub loader: &'a LoaderSelection,
    /// Files omitted because their provider declared no server applicability
    /// and this creation request asked for only a narrower set of them.
    pub unconfirmed_unknown: Vec<&'a PackFileRecipe>,
}

impl ServerReconstructionPlan<'_> {
    /// Whether every override layer needed for reconstruction is retained.
    #[must_use]
    pub fn layers_are_retained(&self) -> bool {
        self.layers.iter().all(|layer| layer.is_retained())
    }
}

impl SidePlan<'_> {
    /// Total bytes to fetch, where the pack declared sizes.
    #[must_use]
    pub fn total_bytes(&self) -> u64 {
        self.files
            .iter()
            .filter_map(|file| file.size)
            .fold(0, u64::saturating_add)
    }

    /// Whether every layer this plan needs is available without the original
    /// archive.
    ///
    /// False means a server can still be created, but one of its override
    /// layers would be missing - which is something to say out loud rather than
    /// to discover as an absent config file.
    #[must_use]
    pub fn layers_are_retained(&self) -> bool {
        self.layers.iter().all(|layer| layer.is_retained())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use ephervex_model::{HashAlgorithm, LoaderKind};

    use super::*;

    #[test]
    fn a_shipped_mod_takes_the_side_requirements_of_the_layer_that_placed_it() {
        let mut recipe = recipe();
        recipe.files.clear();
        recipe
            .record_layer_mods(&[
                ("mods/common.jar".into(), OverrideScope::Common),
                ("mods/client.jar".into(), OverrideScope::Client),
                ("mods/server.jar".into(), OverrideScope::Server),
            ])
            .unwrap();

        let by_path = |name: &str| {
            recipe
                .files
                .iter()
                .find(|file| file.display_name() == name)
                .unwrap_or_else(|| panic!("{name} is recorded"))
        };
        assert_eq!(by_path("mods/common.jar").client, SideRequirement::Required);
        assert_eq!(by_path("mods/common.jar").server, SideRequirement::Required);
        assert_eq!(by_path("mods/client.jar").client, SideRequirement::Required);
        assert_eq!(
            by_path("mods/client.jar").server,
            SideRequirement::Unsupported
        );
        assert_eq!(
            by_path("mods/server.jar").client,
            SideRequirement::Unsupported
        );
        assert_eq!(by_path("mods/server.jar").server, SideRequirement::Required);
    }

    #[test]
    fn recording_the_same_shipped_mod_twice_leaves_the_recipe_unchanged() {
        let mut recipe = recipe();
        recipe.files.clear();
        let shipped = [("mods/common.jar".into(), OverrideScope::Common)];
        recipe.record_layer_mods(&shipped).unwrap();
        let after_first = recipe.files.clone();
        recipe.record_layer_mods(&shipped).unwrap();

        assert_eq!(
            recipe.files, after_first,
            "importing the same pack again is not a second mod"
        );
    }

    #[test]
    fn a_mod_shipped_by_a_common_and_a_side_layer_is_required_where_either_applies() {
        let mut recipe = recipe();
        recipe.files.clear();
        recipe
            .record_layer_mods(&[
                ("mods/both.jar".into(), OverrideScope::Client),
                ("mods/both.jar".into(), OverrideScope::Server),
                ("mods/shared.jar".into(), OverrideScope::Server),
                ("mods/shared.jar".into(), OverrideScope::Common),
            ])
            .unwrap();

        assert_eq!(recipe.files.len(), 2, "one entry per path");
        for file in &recipe.files {
            assert_eq!(file.origin, ModOrigin::PackOverride);
            assert_eq!(file.client, SideRequirement::Required, "{file:?}");
            assert_eq!(file.server, SideRequirement::Required, "{file:?}");
        }
    }

    #[test]
    fn a_layer_replacing_a_declared_file_on_every_side_replaces_its_provenance() {
        let mut recipe = recipe();
        recipe.files.clear();
        let mut declared = file(
            "mods/a.jar",
            SideRequirement::Required,
            SideRequirement::Required,
        );
        declared.selected = Some(true);
        declared.identity = Some(ProviderFileIdentity {
            provider: Some(ModProvider::Modrinth),
            project_id: Some("p".to_owned()),
            version_id: Some("v".to_owned()),
            file_id: None,
        });
        recipe.files.push(declared);

        recipe
            .record_layer_mods(&[
                ("mods/a.jar".into(), OverrideScope::Common),
                ("mods/a.jar".into(), OverrideScope::Client),
            ])
            .unwrap();

        assert_eq!(recipe.files.len(), 1);
        let entry = &recipe.files[0];
        assert_eq!(entry.origin, ModOrigin::PackOverride);
        assert!(entry.hashes.is_empty());
        assert!(entry.downloads.is_empty());
        assert_eq!(entry.size, None);
        assert_eq!(entry.identity, None);
        assert_eq!(entry.original_identity, None);
        assert_eq!(entry.selected, None);
        assert_eq!(entry.client, SideRequirement::Required);
        assert_eq!(entry.server, SideRequirement::Required);
    }

    #[test]
    fn a_layer_replacing_a_client_only_declared_file_keeps_the_sides_it_applies_to() {
        let mut recipe = recipe();
        recipe.files.clear();
        recipe.files.push(file(
            "mods/a.jar",
            SideRequirement::Required,
            SideRequirement::Unsupported,
        ));

        recipe
            .record_layer_mods(&[("mods/a.jar".into(), OverrideScope::Client)])
            .unwrap();

        let entry = &recipe.files[0];
        assert_eq!(entry.origin, ModOrigin::PackOverride);
        assert_eq!(entry.client, SideRequirement::Required);
        assert_eq!(entry.server, SideRequirement::Unsupported);
    }

    #[test]
    fn a_path_declared_once_per_side_collapses_into_one_shipped_mod() {
        let mut recipe = recipe();
        recipe.files.clear();
        recipe.files.push(file(
            "mods/a.jar",
            SideRequirement::Required,
            SideRequirement::Unsupported,
        ));
        recipe.files.push(file(
            "mods/other.jar",
            SideRequirement::Required,
            SideRequirement::Required,
        ));
        recipe.files.push(file(
            "mods/a.jar",
            SideRequirement::Unsupported,
            SideRequirement::Required,
        ));

        recipe
            .record_layer_mods(&[("mods/a.jar".into(), OverrideScope::Common)])
            .unwrap();

        let at_path: Vec<_> = recipe
            .files
            .iter()
            .filter(|file| file.logical_path == Path::new("mods/a.jar"))
            .collect();
        assert_eq!(at_path.len(), 1, "one path is one entry");
        assert_eq!(at_path[0].origin, ModOrigin::PackOverride);
        assert!(at_path[0].hashes.is_empty());
        assert_eq!(at_path[0].client, SideRequirement::Required);
        assert_eq!(at_path[0].server, SideRequirement::Required);
        assert_eq!(recipe.files.len(), 2, "other mods are untouched");
    }

    #[test]
    fn a_layer_covering_only_one_of_two_declarations_at_a_path_changes_nothing() {
        let mut recipe = recipe();
        recipe.files.clear();
        recipe.files.push(file(
            "mods/a.jar",
            SideRequirement::Required,
            SideRequirement::Unsupported,
        ));
        recipe.files.push(file(
            "mods/a.jar",
            SideRequirement::Unsupported,
            SideRequirement::Required,
        ));
        let before = recipe.files.clone();

        let error = recipe
            .record_layer_mods(&[
                ("mods/other.jar".into(), OverrideScope::Common),
                ("mods/a.jar".into(), OverrideScope::Client),
            ])
            .unwrap_err();

        assert!(matches!(
            error,
            PackError::OverrideShadowsDeclaredFile { .. }
        ));
        assert_eq!(
            recipe.files, before,
            "nothing is recorded from a refused set"
        );
    }

    #[test]
    fn a_layer_that_leaves_the_declared_file_installed_on_some_side_is_refused() {
        for (declared_client, declared_server, scope) in [
            (
                SideRequirement::Required,
                SideRequirement::Required,
                OverrideScope::Client,
            ),
            (
                SideRequirement::Required,
                SideRequirement::Required,
                OverrideScope::Server,
            ),
            (
                SideRequirement::Required,
                SideRequirement::Unsupported,
                OverrideScope::Server,
            ),
        ] {
            let mut recipe = recipe();
            recipe.files.clear();
            recipe
                .files
                .push(file("mods/a.jar", declared_client, declared_server));
            let before = recipe.files.clone();

            let error = recipe
                .record_layer_mods(&[("mods/a.jar".into(), scope)])
                .unwrap_err();

            assert!(
                matches!(error, PackError::OverrideShadowsDeclaredFile { ref path } if path == Path::new("mods/a.jar")),
                "{error:?}"
            );
            assert_eq!(
                recipe.files, before,
                "a refused collision leaves the declared record as it was"
            );
        }
    }

    #[test]
    fn a_shipped_mod_is_kept_out_of_the_plans_that_download() {
        let mut recipe = recipe();
        recipe.files.clear();
        recipe.files.push(file(
            "mods/declared.jar",
            SideRequirement::Required,
            SideRequirement::Required,
        ));
        recipe
            .record_layer_mods(&[("mods/shipped.jar".into(), OverrideScope::Common)])
            .unwrap();

        let installed = recipe.files_for(PackSide::Client, &OptionalSelection::none());
        assert_eq!(installed.len(), 1);
        assert_eq!(installed[0].display_name(), "mods/declared.jar");

        let plan = recipe
            .server_reconstruction_plan(&UnknownServerFiles::IncludeAll)
            .expect("plan");
        assert!(
            plan.files
                .iter()
                .all(|file| file.display_name() != "mods/shipped.jar"),
            "the retained layer places it, so a plan that fetched it would fail"
        );

        // Still the instance's mod, which is the whole point of recording it.
        assert!(
            recipe
                .recorded_mods_for(PackSide::Client)
                .iter()
                .any(|recorded| recorded.logical_path == Path::new("mods/shipped.jar")),
            "it is listed as a mod even though nothing downloads it"
        );
    }

    #[test]
    fn identifying_a_shipped_mod_says_what_it_is_without_changing_whose_it_is() {
        let mut recipe = recipe();
        recipe.files.clear();
        recipe
            .record_layer_mods(&[("mods/shipped.jar".into(), OverrideScope::Common)])
            .unwrap();
        let identity = ProviderFileIdentity {
            provider: Some(ModProvider::CurseForge),
            project_id: Some("358191".to_owned()),
            version_id: Some("4711".to_owned()),
            file_id: Some("4711".to_owned()),
        };

        assert!(recipe.record_identity(Path::new("mods/shipped.jar"), identity.clone()));

        let shipped = &recipe.files[0];
        assert_eq!(shipped.identity.as_ref(), Some(&identity));
        assert_eq!(
            shipped.origin,
            ModOrigin::PackOverride,
            "knowing what a jar is does not make it somebody else's"
        );
        assert!(
            !recipe.record_identity(Path::new("mods/shipped.jar"), identity),
            "an identity already recorded is never overwritten"
        );
    }

    fn file(path: &str, client: SideRequirement, server: SideRequirement) -> PackFileRecipe {
        PackFileRecipe {
            logical_path: PathBuf::from(path),
            hashes: vec![
                Hash::new(
                    HashAlgorithm::Sha1,
                    "da39a3ee5e6b4b0d3255bfef95601890afd80709",
                )
                .expect("valid sha1"),
            ],
            downloads: vec!["https://cdn.modrinth.com/data/x/mod.jar".to_owned()],
            size: Some(10),
            client,
            server,
            selected: None,
            origin: ModOrigin::PackDeclared,
            identity: None,
            original_identity: None,
            relations: None,
        }
    }

    fn recipe() -> PackRecipe {
        PackRecipe {
            recipe_version: RECIPE_VERSION,
            source_kind: ImportSourceKind::ModrinthPack,
            original_name: "Test Pack".to_owned(),
            original_version: Some("1.0.0".to_owned()),
            author: None,
            summary: None,
            archive_name: Some("test.mrpack".to_owned()),
            minecraft_version: "1.20.1".to_owned(),
            loader: LoaderSelection::exact(LoaderKind::NeoForge, "47.1.106"),
            files: vec![
                file(
                    "mods/both.jar",
                    SideRequirement::Required,
                    SideRequirement::Required,
                ),
                file(
                    "mods/client-only.jar",
                    SideRequirement::Required,
                    SideRequirement::Unsupported,
                ),
                file(
                    "mods/server-only.jar",
                    SideRequirement::Unsupported,
                    SideRequirement::Required,
                ),
                file(
                    "mods/optional.jar",
                    SideRequirement::Optional,
                    SideRequirement::Optional,
                ),
            ],
            override_layers: vec![
                OverrideLayer {
                    scope: OverrideScope::Common,
                    archive_prefix: "overrides".to_owned(),
                    retained_at: Some(PathBuf::from("modrinth/overrides")),
                    files: 2,
                },
                OverrideLayer {
                    scope: OverrideScope::Client,
                    archive_prefix: "client-overrides".to_owned(),
                    retained_at: None,
                    files: 1,
                },
                OverrideLayer {
                    scope: OverrideScope::Server,
                    archive_prefix: "server-overrides".to_owned(),
                    retained_at: Some(PathBuf::from("modrinth/server-overrides")),
                    files: 1,
                },
            ],
            provider_metadata: ProviderRecipeMetadata::new(
                ImportSourceKind::ModrinthPack,
                Some(PathBuf::from("modrinth/modrinth.index.json")),
            ),
        }
    }

    #[test]
    fn a_client_install_takes_neither_server_only_nor_unselected_optional_files() {
        let recipe = recipe();
        let plan = recipe.plan(PackSide::Client, &OptionalSelection::none());
        let names: Vec<String> = plan.files.iter().map(|file| file.display_name()).collect();
        assert_eq!(names, vec!["mods/both.jar", "mods/client-only.jar"]);
    }

    #[test]
    fn a_server_plan_takes_the_server_side_of_the_same_recipe() {
        let recipe = recipe();
        let plan = recipe.plan(PackSide::Server, &OptionalSelection::none());
        let names: Vec<String> = plan.files.iter().map(|file| file.display_name()).collect();
        assert_eq!(names, vec!["mods/both.jar", "mods/server-only.jar"]);

        // the required layering, for the server: common first, then server.
        let layers: Vec<&str> = plan
            .layers
            .iter()
            .map(|layer| layer.scope.as_str())
            .collect();
        assert_eq!(layers, vec!["common", "server"]);
        assert!(
            plan.layers_are_retained(),
            "server creation requires both retained layers"
        );
    }

    #[test]
    fn a_client_plan_layers_common_then_client() {
        let recipe = recipe();
        let plan = recipe.plan(PackSide::Client, &OptionalSelection::none());
        let layers: Vec<&str> = plan
            .layers
            .iter()
            .map(|layer| layer.scope.as_str())
            .collect();
        assert_eq!(layers, vec!["common", "client"]);
        assert!(
            !plan.layers_are_retained(),
            "the client layer is already in the game directory and is not duplicated"
        );
    }

    #[test]
    fn an_optional_file_is_off_until_something_says_otherwise() {
        let recipe = recipe();
        let component = recipe
            .optional_components(PackSide::Client)
            .into_iter()
            .next()
            .expect("one optional file");
        assert!(
            !component.starts_selected(),
            "optional files default to unselected"
        );
        assert_eq!(component.display_name(), "mods/optional.jar");

        let chosen = OptionalSelection::of(["mods/optional.jar"]);
        let names: Vec<String> = recipe
            .files_for(PackSide::Client, &chosen)
            .iter()
            .map(|file| file.display_name())
            .collect();
        assert!(names.contains(&"mods/optional.jar".to_owned()));
    }

    #[test]
    fn a_recorded_selection_survives_into_a_later_server_plan() {
        // optional server files follow the recorded import selection
        // rather than prompting again from nothing.
        let mut recipe = recipe();
        recipe.record_selection(
            PackSide::Client,
            &OptionalSelection::of(["mods/optional.jar"]),
        );

        let optional = recipe
            .files
            .iter()
            .find(|file| file.display_name() == "mods/optional.jar")
            .expect("present");
        assert_eq!(optional.selected, Some(true));

        // The server plan is built with no new selection at all and still
        // honors what the import recorded.
        let plan = recipe.plan(PackSide::Server, &OptionalSelection::none());
        let names: Vec<String> = plan.files.iter().map(|file| file.display_name()).collect();
        assert!(names.contains(&"mods/optional.jar".to_owned()));
    }

    #[test]
    fn recording_a_refusal_is_distinguishable_from_never_having_asked() {
        let mut recipe = recipe();
        recipe.record_selection(PackSide::Client, &OptionalSelection::none());
        let optional = recipe
            .files
            .iter()
            .find(|file| file.display_name() == "mods/optional.jar")
            .expect("present");
        assert_eq!(
            optional.selected,
            Some(false),
            "`the user said no` is not the same as `nobody asked`"
        );
    }

    #[test]
    fn selecting_everything_takes_every_optional_file_and_no_unsupported_one() {
        let recipe = recipe();
        let plan = recipe.plan(PackSide::Client, &OptionalSelection::all());
        let names: Vec<String> = plan.files.iter().map(|file| file.display_name()).collect();
        assert_eq!(
            names,
            vec!["mods/both.jar", "mods/client-only.jar", "mods/optional.jar"]
        );
        assert!(!names.contains(&"mods/server-only.jar".to_owned()));
    }

    #[test]
    fn the_recipe_round_trips_through_json_which_is_how_it_is_stored() {
        let recipe = recipe();
        let json = serde_json::to_string(&recipe).expect("serialize");
        let back: PackRecipe = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(recipe, back);

        // The one place a provider's own file name is allowed to appear.
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert_eq!(
            value["provider_metadata"]["manifest_file"],
            "modrinth/modrinth.index.json"
        );
    }

    #[test]
    fn the_normalized_model_names_no_provider_in_its_own_fields() {
        // A regression guard for the rule this module exists to keep: the
        // moment a Modrinth-shaped field appears here, CurseForge and other
        // providers have to fake one.
        let json = serde_json::to_value(recipe()).expect("serialize");
        let object = json.as_object().expect("object");
        for key in object.keys() {
            assert!(
                !key.contains("modrinth") && !key.contains("curseforge"),
                "provider-specific key in the normalized recipe: {key}"
            );
        }
        assert!(object.contains_key("provider_metadata"));
    }

    #[test]
    fn side_and_requirement_identifiers_are_stable() {
        assert_eq!(PackSide::parse("Server"), Some(PackSide::Server));
        assert_eq!(PackSide::parse("neither"), None);
        assert_eq!(SideRequirement::Optional.as_str(), "optional");
        assert!(SideRequirement::Optional.is_installable());
        assert!(!SideRequirement::Optional.is_automatic());
        assert!(!SideRequirement::Unsupported.is_installable());
        assert_eq!(SideRequirement::Unknown.as_str(), "unknown");
    }

    // -- managed mods ------------------------------------------------------

    /// A mod as the browser installs one: on the client because the user asked,
    /// and undeclared on the server because the project says nothing.
    fn managed(project: &str, version: &str, file_name: &str) -> PackFileRecipe {
        PackFileRecipe {
            logical_path: PathBuf::from("mods").join(file_name),
            hashes: vec![
                Hash::new(
                    HashAlgorithm::Sha1,
                    "da39a3ee5e6b4b0d3255bfef95601890afd80709",
                )
                .expect("valid sha1"),
            ],
            downloads: vec![format!(
                "https://cdn.modrinth.com/data/{project}/{file_name}"
            )],
            size: Some(10),
            client: SideRequirement::Required,
            server: SideRequirement::Unknown,
            selected: None,
            origin: ModOrigin::UserSelected,
            identity: Some(ProviderFileIdentity {
                provider: Some(ModProvider::Modrinth),
                project_id: Some(project.to_owned()),
                version_id: Some(version.to_owned()),
                file_id: None,
            }),
            original_identity: None,
            relations: None,
        }
    }

    /// The same mod as adoption records one: identified, but still the user's.
    fn adopted_by_hand(project: &str, file_name: &str) -> PackFileRecipe {
        PackFileRecipe {
            origin: ModOrigin::HandPlaced,
            downloads: Vec::new(),
            ..managed(project, "Orv4ObRj", file_name)
        }
    }

    #[test]
    fn forgetting_a_hand_placed_jar_leaves_every_other_entry_alone() {
        let mut recipe = PackRecipe::for_manual_instance(
            "Repro",
            "1.20.1",
            LoaderSelection::exact(LoaderKind::NeoForge, "47.1.106"),
        );
        recipe
            .record_managed(managed("u6dRKJwZ", "Orv4ObRj", "library-1.2.3.jar"))
            .expect("record");
        recipe
            .record_managed(adopted_by_hand("P7dR8mSH", "from-a-forum-post.jar"))
            .expect("record");

        assert!(recipe.forget_hand_placed(Path::new("mods/from-a-forum-post.jar")));

        let remaining: Vec<PathBuf> = recipe
            .recorded_mods()
            .into_iter()
            .map(|entry| entry.logical_path)
            .collect();
        assert_eq!(remaining, vec![PathBuf::from("mods/library-1.2.3.jar")]);
    }

    #[test]
    fn forgetting_refuses_an_entry_the_launcher_owns() {
        // Taking a launcher-owned mod out of the desired state is a removal,
        // which resolves dependencies and retires files. This is neither.
        let mut recipe = PackRecipe::for_manual_instance(
            "Repro",
            "1.20.1",
            LoaderSelection::exact(LoaderKind::NeoForge, "47.1.106"),
        );
        recipe
            .record_managed(managed("u6dRKJwZ", "Orv4ObRj", "library-1.2.3.jar"))
            .expect("record");

        assert!(!recipe.forget_hand_placed(Path::new("mods/library-1.2.3.jar")));
        assert_eq!(recipe.recorded_mods().len(), 1);
    }

    #[test]
    fn forgetting_a_path_no_entry_claims_changes_nothing() {
        let mut recipe = PackRecipe::for_manual_instance(
            "Repro",
            "1.20.1",
            LoaderSelection::exact(LoaderKind::NeoForge, "47.1.106"),
        );
        recipe
            .record_managed(adopted_by_hand("P7dR8mSH", "from-a-forum-post.jar"))
            .expect("record");

        assert!(!recipe.forget_hand_placed(Path::new("mods/never-here.jar")));
        assert_eq!(recipe.recorded_mods().len(), 1);
    }

    #[test]
    fn an_instance_created_by_hand_gets_a_recipe_that_matches_what_it_is() {
        let recipe = PackRecipe::for_manual_instance(
            "By hand",
            "1.20.1",
            LoaderSelection::exact(LoaderKind::NeoForge, "47.1.106"),
        );
        assert_eq!(recipe.source_kind, ImportSourceKind::Manual);
        assert_eq!(recipe.minecraft_version, "1.20.1");
        assert_eq!(recipe.loader.version.as_deref(), Some("47.1.106"));
        assert!(recipe.files.is_empty());
        assert!(recipe.managed_mods().is_empty());
    }

    #[test]
    fn a_managed_mod_carries_everything_needed_to_resolve_it_again() {
        let mut recipe = PackRecipe::for_manual_instance(
            "Repro",
            "1.20.1",
            LoaderSelection::exact(LoaderKind::NeoForge, "47.1.106"),
        );
        recipe
            .record_managed(managed("u6dRKJwZ", "Orv4ObRj", "library-1.2.3.jar"))
            .expect("record");

        let mods = recipe.managed_mods();
        assert_eq!(mods.len(), 1);
        assert_eq!(mods[0].provider, ModProvider::Modrinth);
        assert_eq!(mods[0].project_id, "u6dRKJwZ");
        assert_eq!(mods[0].version_id, "Orv4ObRj");
        assert_eq!(mods[0].file_name, "library-1.2.3.jar");
        assert_eq!(mods[0].origin, ModOrigin::UserSelected);
        assert!(!mods[0].hashes.is_empty(), "verification survives a reopen");
    }

    #[test]
    fn only_a_jar_directly_in_the_mods_folder_is_a_mod_of_the_instance() {
        // `mods/` is a directory, not a manifest. Packs ship notes, generated
        // indexes, and deliberately disabled jars into it, and a mod list that
        // reported those would be describing the folder rather than the game.
        for path in [
            "mods/README.txt",
            "mods/metadata.json",
            "mods/something.jar.disabled",
            "mods/no-extension",
            "mods/nested/deep.jar",
            "config/thing.jar",
            "mods",
        ] {
            let entry = file(path, SideRequirement::Required, SideRequirement::Required);
            assert!(
                !entry.is_mod_file(),
                "`{path}` is not a mod the loader would read"
            );
            assert_eq!(entry.recorded_mod(), None, "`{path}` produced an entry");
        }

        for path in ["mods/library.jar", "mods/Library.JAR"] {
            let entry = file(path, SideRequirement::Required, SideRequirement::Required);
            assert!(entry.is_mod_file(), "`{path}` is a mod");
            assert!(entry.recorded_mod().is_some());
        }
    }

    #[test]
    fn a_pack_declared_file_is_not_a_managed_mod_but_is_still_installed() {
        // The pack's own files stay in the recipe and in the server plan; they
        // are simply not something the mod browser owns.
        let recipe = recipe();
        assert!(recipe.managed_mods().is_empty());
        assert!(
            recipe
                .files_for(PackSide::Client, &OptionalSelection::none())
                .iter()
                .any(|file| file.display_name() == "mods/both.jar")
        );
    }

    #[test]
    fn a_file_with_half_an_identity_is_not_promoted_to_a_managed_mod() {
        // Half an identity resolves nothing. Reporting it as managed would
        // claim a version the launcher cannot look up.
        let mut file = managed("u6dRKJwZ", "Orv4ObRj", "library.jar");
        if let Some(identity) = file.identity.as_mut() {
            identity.version_id = None;
        }
        assert_eq!(file.managed_mod(), None);

        let mut file = managed("u6dRKJwZ", "Orv4ObRj", "library.jar");
        if let Some(identity) = file.identity.as_mut() {
            identity.provider = None;
        }
        assert_eq!(file.managed_mod(), None);
    }

    #[test]
    fn recording_the_same_version_twice_leaves_the_recipe_untouched() {
        let mut recipe =
            PackRecipe::for_manual_instance("Repro", "1.20.1", LoaderSelection::vanilla());
        let entry = managed("u6dRKJwZ", "Orv4ObRj", "library-1.2.3.jar");
        recipe.record_managed(entry.clone()).expect("record");
        let after_first = recipe.clone();

        recipe
            .record_managed(entry)
            .expect("an exact repeat is a no-op");
        assert_eq!(recipe, after_first);
        assert_eq!(recipe.files.len(), 1);
    }

    #[test]
    fn recording_a_different_version_of_an_installed_project_is_refused() {
        // Replacing it is a mod update, which this build does not implement.
        // Refusing here means no code path can leave two jars for one project.
        let mut recipe =
            PackRecipe::for_manual_instance("Repro", "1.20.1", LoaderSelection::vanilla());
        recipe
            .record_managed(managed("u6dRKJwZ", "Orv4ObRj", "library-1.2.3.jar"))
            .expect("record");

        let error = recipe
            .record_managed(managed("u6dRKJwZ", "PPPPPPPP", "library-2.0.0.jar"))
            .expect_err("must refuse");
        assert!(matches!(
            error,
            PackError::ManagedVersionConflict {
                ref installed,
                ref requested,
                ..
            } if installed == "Orv4ObRj" && requested == "PPPPPPPP"
        ));
        assert_eq!(
            recipe.files.len(),
            1,
            "the refused entry left nothing behind"
        );
    }

    #[test]
    fn recording_a_file_with_no_provider_identity_is_refused() {
        let mut recipe =
            PackRecipe::for_manual_instance("Repro", "1.20.1", LoaderSelection::vanilla());
        let mut file = managed("u6dRKJwZ", "Orv4ObRj", "library.jar");
        file.identity = None;
        assert!(matches!(
            recipe.record_managed(file),
            Err(PackError::ManagedIdentityMissing { .. })
        ));
    }

    #[test]
    fn an_undeclared_server_side_never_installs_on_its_own() {
        // The whole point of keeping `unknown` distinct: it must not behave
        // like `required` on the side nobody declared.
        let mut recipe =
            PackRecipe::for_manual_instance("Repro", "1.20.1", LoaderSelection::vanilla());
        recipe
            .record_managed(managed("u6dRKJwZ", "Orv4ObRj", "library.jar"))
            .expect("record");

        let client = recipe.plan(PackSide::Client, &OptionalSelection::none());
        assert_eq!(client.files.len(), 1, "the user asked for it on the client");

        let server = recipe.plan(PackSide::Server, &OptionalSelection::none());
        assert!(
            server.files.is_empty(),
            "an undeclared server side is a question, not a yes"
        );

        // ...and it is a question that is asked, rather than silently dropped.
        let waiting = recipe.optional_components(PackSide::Server);
        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[0].requirement, SideRequirement::Unknown);
        assert!(!waiting[0].starts_selected());
    }

    #[test]
    fn an_undeclared_side_is_distinguishable_from_an_optional_one_after_a_reopen() {
        let mut recipe =
            PackRecipe::for_manual_instance("Repro", "1.20.1", LoaderSelection::vanilla());
        recipe
            .record_managed(managed("u6dRKJwZ", "Orv4ObRj", "library.jar"))
            .expect("record");

        let json = serde_json::to_string(&recipe).expect("serialize");
        assert!(
            json.contains("\"server\":\"unknown\""),
            "the undeclared state is what is written, not a resolved guess: {json}"
        );
        let back: PackRecipe = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.files[0].server, SideRequirement::Unknown);
        assert_eq!(back, recipe);
    }

    #[test]
    fn a_version_3_shaped_entry_with_no_original_identity_key_reads_as_absent() {
        // A recipe written before this field existed never mutated a pack-owned
        // entry away from what the pack declared, so an absent key is exactly
        // that entry's honest state.
        let json = r#"{
            "logical_path": "mods/thing.jar",
            "hashes": [],
            "client": "required",
            "server": "unknown",
            "origin": "pack_declared",
            "identity": {"provider": "modrinth", "project_id": "ROOTPROJ", "version_id": "ROOTV1"}
        }"#;
        let file: PackFileRecipe = serde_json::from_str(json).expect("deserialize");
        assert_eq!(file.original_identity, None);
    }

    /// The real shape this exists for: one library, three mods that need it.
    fn requiring(
        file_name: &str,
        project: &str,
        requires: &[(&str, ModRelationKind)],
    ) -> PackFileRecipe {
        PackFileRecipe {
            relations: Some(
                requires
                    .iter()
                    .map(|(project_id, kind)| RecipeRelation {
                        kind: *kind,
                        target: RelationTarget::Project {
                            project_id: (*project_id).to_owned(),
                            version_id: None,
                        },
                    })
                    .collect(),
            ),
            ..managed(project, "Orv4ObRj", file_name)
        }
    }

    fn library_and_its_dependents() -> PackRecipe {
        let mut recipe = PackRecipe::for_manual_instance(
            "Dependency Aware Example",
            "1.21.1",
            LoaderSelection::exact(LoaderKind::NeoForge, "21.1.77"),
        );
        recipe.files = vec![
            requiring("LibX.jar", "qEH6GYul", &[]),
            requiring(
                "SkyblockBuilder.jar",
                "por2AZc5",
                &[
                    ("qEH6GYul", ModRelationKind::Required),
                    ("3Gdi7Qgq", ModRelationKind::Optional),
                ],
            ),
            requiring(
                "SkyGUIs.jar",
                "3Gdi7Qgq",
                &[
                    ("qEH6GYul", ModRelationKind::Required),
                    ("por2AZc5", ModRelationKind::Required),
                ],
            ),
            requiring(
                "UtilitiX.jar",
                "MMMWYsjm",
                &[("qEH6GYul", ModRelationKind::Required)],
            ),
        ];
        recipe
    }

    #[test]
    fn the_reverse_index_names_every_file_that_requires_a_project() {
        let recipe = library_and_its_dependents();
        let library = recipe.files.first().expect("the library");
        let names: Vec<String> = recipe
            .required_by(library)
            .into_iter()
            .map(PackFileRecipe::display_name)
            .collect();
        assert_eq!(
            names,
            vec![
                "mods/SkyblockBuilder.jar".to_owned(),
                "mods/SkyGUIs.jar".to_owned(),
                "mods/UtilitiX.jar".to_owned(),
            ],
            "forward edges are stored; the direction a warning needs is derived"
        );
    }

    #[test]
    fn an_optional_relation_never_appears_in_the_required_reverse_index() {
        // Skyblock Builder declares Sky GUIs optional. Turning Sky GUIs off
        // costs a feature, so nothing may cascade from it.
        let recipe = library_and_its_dependents();
        let sky_guis = recipe
            .files
            .iter()
            .find(|file| file.file_name() == "SkyGUIs.jar")
            .expect("the addon");
        assert!(
            recipe.required_by(sky_guis).is_empty(),
            "an optional relation is not a requirement"
        );
    }

    #[test]
    fn an_absent_relation_set_and_an_empty_one_are_told_apart_on_disk() {
        // The whole point of the `Option`: "nobody asked the catalog" and "the
        // catalog says it needs nothing" send a warning to opposite answers.
        let unasked = managed("qEH6GYul", "Orv4ObRj", "LibX.jar");
        assert_eq!(unasked.recorded_relations(), None);
        let unasked_json = serde_json::to_string(&unasked).expect("serialize");
        assert!(
            !unasked_json.contains("relations"),
            "an absent value is omitted, not written as null: {unasked_json}"
        );

        let answered = requiring("LibX.jar", "qEH6GYul", &[]);
        assert_eq!(answered.recorded_relations(), Some(&[][..]));
        let answered_json = serde_json::to_string(&answered).expect("serialize");
        assert!(
            answered_json.contains(r#""relations":[]"#),
            "an answered-and-empty set is written, or it reads back as unasked: {answered_json}"
        );

        let back: PackFileRecipe = serde_json::from_str(&answered_json).expect("deserialize");
        assert_eq!(back.recorded_relations(), Some(&[][..]));
        let back: PackFileRecipe = serde_json::from_str(&unasked_json).expect("deserialize");
        assert_eq!(back.recorded_relations(), None);
    }

    #[test]
    fn an_entry_written_before_relations_existed_reads_as_unasked() {
        let json = r#"{
            "logical_path": "mods/library.jar",
            "hashes": [],
            "client": "required",
            "server": "unknown",
            "origin": "pack_declared"
        }"#;
        let file: PackFileRecipe = serde_json::from_str(json).expect("deserialize");
        assert_eq!(file.recorded_relations(), None);
        assert!(
            !file.requires(&managed("qEH6GYul", "Orv4ObRj", "LibX.jar")),
            "an unrecorded entry proves nothing, so it requires nothing provably"
        );
    }

    #[test]
    fn an_absent_original_identity_is_not_written_to_json() {
        let file = managed("u6dRKJwZ", "Orv4ObRj", "library.jar");
        assert_eq!(file.original_identity, None);
        let json = serde_json::to_string(&file).expect("serialize");
        assert!(
            !json.contains("original_identity"),
            "machine-owned JSON stays compact: an absent value is omitted, not written as null: {json}"
        );
    }
}
