//! What an instance knows about its own mods, and how much of it survives.
//!
//! Everything here is offline. A `.mrpack` index is a document, so parsing one
//! contacts nothing, and the recipe it produces is written to and read back
//! from a temporary directory — which is exactly what a restart does.
//!
//! The invariant these tests exist to hold is that a pack's mods are ordinary
//! members of the instance's mod list, told apart from later installations by
//! their recorded ownership rather than by being left out.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use ephervex_download::DomainAllowlist;
use ephervex_model::{Hash, HashAlgorithm, ImportSourceKind, ModOrigin, ModProvider};
use ephervex_pack::{
    ModIdentity, PackFileRecipe, PackRecipe, ProviderFileIdentity, SideRequirement, SourceStore,
};
use tempfile::TempDir;

const SHA1: &str = "da39a3ee5e6b4b0d3255bfef95601890afd80709";
const SHA512: &str = "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e";

/// One file entry as Modrinth's pack tooling writes it: no project or version
/// fields, and a CDN address that carries both.
fn cdn_file(path: &str, project: &str, version: &str, env: &str) -> String {
    let name = path.rsplit('/').next().expect("a file name");
    format!(
        r#"{{
            "path": "{path}",
            "hashes": {{"sha1": "{SHA1}", "sha512": "{SHA512}"}},
            "env": {env},
            "downloads": ["https://cdn.modrinth.com/data/{project}/versions/{version}/{name}"],
            "fileSize": 4096
        }}"#
    )
}

const BOTH_SIDES: &str = r#"{"client":"required","server":"required"}"#;

fn index(files: &[String]) -> String {
    format!(
        r#"{{
            "formatVersion": 1,
            "game": "minecraft",
            "versionId": "3.2.1",
            "name": "Reproduction Pack",
            "files": [{}],
            "dependencies": {{"minecraft": "1.20.1", "neoforge": "47.1.106"}}
        }}"#,
        files.join(", ")
    )
}

fn parse(files: &[String]) -> PackRecipe {
    ephervex_pack::modrinth::parse(
        index(files).as_bytes(),
        &DomainAllowlist::modrinth_pack_defaults(),
        Some("reproduction.mrpack".to_owned()),
    )
    .expect("a valid pack index")
    .recipe
}

/// A pack with two mods, a config file, and an optional mod nobody selected.
fn pack() -> PackRecipe {
    parse(&[
        cdn_file("mods/journeymap.jar", "lfHFW1mp", "GKQEbfSt", BOTH_SIDES),
        cdn_file("mods/architectury.jar", "lhGA9TYQ", "Va9L5Nqe", BOTH_SIDES),
        cdn_file("config/journeymap.toml", "lfHFW1mp", "GKQEbfSt", BOTH_SIDES),
        cdn_file(
            "mods/extra-sounds.jar",
            "MubyTbnA",
            "PnLuDMzB",
            r#"{"client":"optional","server":"optional"}"#,
        ),
    ])
}

/// A mod as the browser installs one, recorded with full identity.
fn browser_install(project: &str, version: &str, name: &str, origin: ModOrigin) -> PackFileRecipe {
    PackFileRecipe {
        logical_path: PathBuf::from("mods").join(name),
        hashes: vec![Hash::new(HashAlgorithm::Sha1, SHA1).expect("valid sha1")],
        downloads: vec![format!(
            "https://cdn.modrinth.com/data/{project}/versions/{version}/{name}"
        )],
        size: Some(2048),
        client: SideRequirement::Required,
        server: SideRequirement::Unknown,
        selected: None,
        origin,
        identity: Some(ProviderFileIdentity {
            provider: Some(ModProvider::Modrinth),
            project_id: Some(project.to_owned()),
            version_id: Some(version.to_owned()),
            file_id: None,
        }),
    }
}

struct Retained {
    _base: TempDir,
    store: SourceStore,
}

impl Retained {
    fn holding(recipe: &PackRecipe) -> Self {
        let base = tempfile::tempdir().expect("temp dir");
        let store = SourceStore::new(base.path().join(".launcher/source"));
        store.write_recipe(recipe).expect("write the recipe");
        Self { _base: base, store }
    }

    /// What the launcher sees on a later run, having kept nothing in memory.
    fn reopened(&self) -> PackRecipe {
        self.store.read_recipe().expect("read the recipe back")
    }
}

#[test]
fn an_imported_packs_mods_are_mods_of_the_instance_with_the_pack_as_their_owner() {
    let mods = pack().recorded_mods();

    let names: Vec<&str> = mods.iter().map(|item| item.file_name.as_str()).collect();
    assert_eq!(
        names,
        vec!["journeymap.jar", "architectury.jar"],
        "a pack's mods belong to the instance's mod list"
    );
    assert!(
        mods.iter()
            .all(|item| item.origin == ModOrigin::PackDeclared),
        "the pack, not a later decision, is what put these here"
    );
}

#[test]
fn a_packs_own_download_addresses_identify_its_mods_exactly() {
    let mods = pack().recorded_mods();

    assert_eq!(
        mods[0].identity,
        ModIdentity::Version {
            provider: ModProvider::Modrinth,
            project_id: "lfHFW1mp".to_owned(),
            version_id: "GKQEbfSt".to_owned(),
            file_id: None,
        }
    );
    assert_ne!(
        mods[0].identity, mods[1].identity,
        "two pack mods stay two distinct projects"
    );
}

#[test]
fn a_packs_configs_and_unselected_optional_mods_are_not_mods_of_the_instance() {
    let names: Vec<String> = pack()
        .recorded_mods()
        .into_iter()
        .map(|item| item.file_name)
        .collect();

    assert!(
        !names.contains(&"journeymap.toml".to_owned()),
        "a config file the pack ships is not a mod: {names:?}"
    );
    assert!(
        !names.contains(&"extra-sounds.jar".to_owned()),
        "an optional mod nobody selected was never installed: {names:?}"
    );
}

#[test]
fn a_download_address_that_proves_no_catalog_version_leaves_the_version_unknown() {
    // Packs built by older tooling put a human version *number* in the same
    // position. Recording "1.4.2" as a version id would name a release the
    // catalog has never heard of.
    let recipe = parse(&[cdn_file("mods/older.jar", "lfHFW1mp", "1.4.2", BOTH_SIDES)]);
    let mods = recipe.recorded_mods();

    assert_eq!(
        mods[0].identity,
        ModIdentity::Project {
            provider: ModProvider::Modrinth,
            project_id: "lfHFW1mp".to_owned(),
        },
        "the project is proven, the exact release is not"
    );
    assert_eq!(mods[0].identity.version_id(), None);
}

#[test]
fn a_mod_hosted_somewhere_other_than_the_catalog_claims_no_catalog_identity() {
    // `github.com` is a permitted download host, and a file served from it is
    // not evidence of a Modrinth project. Neither is its file name.
    let recipe = parse(&[format!(
        r#"{{
            "path": "mods/lfHFW1mp.jar",
            "hashes": {{"sha1": "{SHA1}"}},
            "downloads": ["https://github.com/example/mod/releases/download/v1/lfHFW1mp.jar"]
        }}"#
    )]);
    let mods = recipe.recorded_mods();

    assert_eq!(mods[0].identity, ModIdentity::Unresolved);
    assert_eq!(mods[0].identity.provider(), None);
    assert_eq!(
        mods[0].file_name, "lfHFW1mp.jar",
        "the file name is a label and stays one"
    );
}

#[test]
fn pack_ownership_and_identity_survive_a_restart() {
    let retained = Retained::holding(&pack());
    let before = pack().recorded_mods();
    let after = retained.reopened().recorded_mods();

    assert_eq!(
        before, after,
        "reopening the instance must not change what it knows about its mods"
    );
    assert!(after[0].identity.is_fully_resolved());
    assert_eq!(after[0].origin, ModOrigin::PackDeclared);
}

#[test]
fn an_instance_imported_before_identities_were_recorded_gains_them_on_reopen() {
    // The migration case. An older build wrote the same authoritative download
    // addresses and no identity; the reconstruction is deterministic, so the
    // instance is completed on the way in rather than re-imported.
    let mut legacy = pack();
    for file in &mut legacy.files {
        file.identity = None;
    }
    let retained = Retained::holding(&legacy);

    let recovered = retained.reopened().recorded_mods();
    assert_eq!(
        recovered[0].identity,
        ModIdentity::Version {
            provider: ModProvider::Modrinth,
            project_id: "lfHFW1mp".to_owned(),
            version_id: "GKQEbfSt".to_owned(),
            file_id: None,
        }
    );
    assert_eq!(
        recovered,
        pack().recorded_mods(),
        "a reconstructed instance and a freshly imported one agree"
    );
}

#[test]
fn an_identity_a_later_build_could_not_reconstruct_is_never_overwritten() {
    // Reconstruction fills gaps. An identity recorded by an installation is a
    // stronger statement than anything derived from an address, so it wins.
    let mut recipe = pack();
    recipe.files[0].identity = Some(ProviderFileIdentity {
        provider: Some(ModProvider::Modrinth),
        project_id: Some("lfHFW1mp".to_owned()),
        version_id: Some("ZZZZZZZZ".to_owned()),
        file_id: None,
    });
    let retained = Retained::holding(&recipe);

    assert_eq!(
        retained.reopened().recorded_mods()[0].identity.version_id(),
        Some("ZZZZZZZZ")
    );
}

#[test]
fn a_mod_installed_after_the_import_joins_the_list_without_rewriting_the_pack() {
    let mut recipe = pack();
    let pack_files_before = recipe.files.clone();

    recipe
        .record_managed(browser_install(
            "u6dRKJwZ",
            "Orv4ObRj",
            "jei.jar",
            ModOrigin::UserSelected,
        ))
        .expect("record the chosen mod");
    recipe
        .record_managed(browser_install(
            "9s6osm5g",
            "Mv7uRFVx",
            "cloth-config.jar",
            ModOrigin::RequiredDependency,
        ))
        .expect("record its dependency");

    let retained = Retained::holding(&recipe);
    let mods = retained.reopened().recorded_mods();

    let ownership: Vec<(&str, ModOrigin)> = mods
        .iter()
        .map(|item| (item.file_name.as_str(), item.origin))
        .collect();
    assert_eq!(
        ownership,
        vec![
            ("journeymap.jar", ModOrigin::PackDeclared),
            ("architectury.jar", ModOrigin::PackDeclared),
            ("jei.jar", ModOrigin::UserSelected),
            ("cloth-config.jar", ModOrigin::RequiredDependency),
        ],
        "one list, four mods, three kinds of ownership"
    );

    let pack_files_after: Vec<PackFileRecipe> = retained
        .reopened()
        .files
        .into_iter()
        .filter(|file| file.origin == ModOrigin::PackDeclared)
        .collect();
    assert_eq!(
        pack_files_after, pack_files_before,
        "what the pack declared is a record of the pack and is not edited by later installs"
    );
    assert_eq!(
        retained.reopened().source_kind,
        ImportSourceKind::ModrinthPack
    );
}

#[test]
fn a_dependency_is_never_presented_as_something_the_user_chose() {
    let mut recipe = PackRecipe::for_manual_instance(
        "By hand",
        "1.20.1",
        ephervex_model::LoaderSelection::exact(ephervex_model::LoaderKind::NeoForge, "47.1.106"),
    );
    recipe
        .record_managed(browser_install(
            "u6dRKJwZ",
            "Orv4ObRj",
            "jei.jar",
            ModOrigin::UserSelected,
        ))
        .expect("record");
    recipe
        .record_managed(browser_install(
            "9s6osm5g",
            "Mv7uRFVx",
            "cloth-config.jar",
            ModOrigin::RequiredDependency,
        ))
        .expect("record");

    let mods = Retained::holding(&recipe).reopened().recorded_mods();
    assert!(mods[0].origin.is_explicit_choice());
    assert!(!mods[1].origin.is_explicit_choice());
    assert!(mods[1].origin.is_dependency());
    assert!(
        !mods.iter().any(|item| item.origin.is_pack_owned()),
        "a hand-made instance has no pack to own anything"
    );
    assert_eq!(mods[1].identity.version_id(), Some("Mv7uRFVx"));
}
