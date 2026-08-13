//! The normalized source recipe.
//!
//! # The one rule this module exists to enforce
//!
//! > Do not infer server state only from the mutated client directory.
//!
//! An imported client is a directory a user then edits: they delete a mod to
//! bisect a crash, drop a debug build in, and change a config. Reconstructing
//! the server from that directory later reconstructs their debugging session
//! rather than the pack. So an import records what the *pack* said — every
//! file, its hashes, its download addresses, and its client **and** server
//! environment — and a server materialized months later is built from that
//! record without the original archive being present.
//!
//! # Why this model has no Modrinth in it
//!
//! Provider-specific formats are not the shape of the problem. Modrinth names
//! files by logical path and supplies download addresses, while CurseForge
//! names files by project and file id and resolves their addresses through an
//! API. Both have to land in the same normalized structure, because
//! everything downstream — the installer, "Create Server", the instance UI —
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

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use ephervex_model::{Hash, ImportSourceKind, LoaderSelection, ModOrigin, ModProvider};
use serde::{Deserialize, Serialize};

use crate::error::{PackError, PackResult};

/// The recipe format version, stamped into `recipe.json`.
///
/// Separate from the instance metadata version: a recipe is copied into a
/// server when one is created from a client, so it travels independently of the
/// record that referenced it.
///
/// Version 2 added managed-mod entries — [`PackFileRecipe::origin`],
/// [`ProviderFileIdentity::provider`], and [`SideRequirement::Unknown`]. The
/// bump is deliberate rather than a silent additive change: a version 1 build
/// reading one of these files would drop every browser-installed mod without
/// noticing, and build a server missing them. Reading a version 1 recipe still
/// works, because the new fields default.
pub const RECIPE_VERSION: u32 = 2;

/// Whether one side of the game needs a file.
///
/// Four states rather than a boolean. "Optional" is a genuine third answer that
/// the UI has to offer and the recipe has to remember — collapsing it either
/// way silently changes what gets installed.
///
/// [`Unknown`](Self::Unknown) is the fourth, and it is not a synonym for any of
/// the others. It means the source declared nothing about this side. A
/// `.mrpack` cannot produce it: that format has three environment values, and
/// an *omitted* `env` object is defined to mean required on both sides. A
/// Modrinth project can, because its `client_side`/`server_side` fields have an
/// `unknown` value an author may leave in place. Reading that as `Required`
/// would assert something the author declined to assert — and would put the mod
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
    /// catalog — a Modrinth pack index gives a path and hashes and nothing to
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
}

impl PackFileRecipe {
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
    /// and `unknown` install only if something explicitly selected them — the
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

    /// The managed mod this entry records, if it is one.
    ///
    /// A file qualifies only when it names a provider, a project, *and* a
    /// version: those are what make it re-resolvable. A pack-declared file has
    /// none of them and is correctly not a managed mod — it is still installed
    /// and still part of the server plan, just not something the mod browser
    /// owns. A jar someone dropped into `mods/` has no entry here at all.
    #[must_use]
    pub fn managed_mod(&self) -> Option<ManagedMod> {
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
    /// choice. `Unknown` is deliberately stricter: a choice made for the
    /// client is not proof that an undeclared server side is safe, so it is
    /// included only when this server-creation request explicitly confirms the
    /// path. Unconfirmed entries are returned for actionable diagnostics.
    pub fn server_reconstruction_plan(
        &self,
        confirmed_unknown: &OptionalSelection,
    ) -> PackResult<ServerReconstructionPlan<'_>> {
        let unknown_paths: BTreeSet<&Path> = self
            .files
            .iter()
            .filter(|file| file.server == SideRequirement::Unknown)
            .map(|file| file.logical_path.as_path())
            .collect();
        for path in confirmed_unknown.paths() {
            if !unknown_paths.contains(path) {
                return Err(PackError::UnknownServerSelection {
                    path: path.to_path_buf(),
                });
            }
        }

        let mut files = Vec::new();
        let mut unconfirmed_unknown = Vec::new();
        for file in &self.files {
            match file.server {
                SideRequirement::Required => files.push(file),
                SideRequirement::Optional => {
                    if file.selected == Some(true) {
                        files.push(file);
                    }
                }
                SideRequirement::Unsupported => {}
                SideRequirement::Unknown
                    if confirmed_unknown.includes(&file.logical_path, None) =>
                {
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
            summary: None,
            archive_name: None,
            minecraft_version: minecraft_version.into(),
            loader,
            files: Vec::new(),
            override_layers: Vec::new(),
            provider_metadata: ProviderRecipeMetadata::new(ImportSourceKind::Manual, None),
        }
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

    /// Adds a managed mod to the desired state.
    ///
    /// **Insert-only, on purpose.** If the project is already recorded at a
    /// different version this refuses rather than replacing. An upsert would
    /// have to decide what happens to the old, differently-named jar, and the
    /// honest answer — delete it — is a mod *update*, which this build does not
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
    /// Nothing selected — the product default.
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
    /// and this creation request did not explicitly confirm them.
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
    /// layers would be missing — which is something to say out loud rather than
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
        }
    }

    fn recipe() -> PackRecipe {
        PackRecipe {
            recipe_version: RECIPE_VERSION,
            source_kind: ImportSourceKind::ModrinthPack,
            original_name: "Test Pack".to_owned(),
            original_version: Some("1.0.0".to_owned()),
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
        let json = serde_json::to_string_pretty(&recipe).expect("serialize");
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
        }
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
}
