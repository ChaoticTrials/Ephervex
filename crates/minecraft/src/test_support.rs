//! Fixture access for this crate's tests.
//!
//! The documents are embedded with [`include_str!`] rather than read at run
//! time, which is the cheapest possible guarantee that `cargo test --offline`
//! cannot reach a network *or* depend on a working directory. Their provenance
//! and the reductions applied to them are recorded in
//! `fixtures/minecraft/SOURCES.md`.

/// One captured Mojang document, by filename.
pub(crate) fn fixture(name: &str) -> &'static str {
    match name {
        "version_manifest_v2.json" => {
            include_str!("../../../fixtures/minecraft/version_manifest_v2.json")
        }
        "1.12.2.json" => include_str!("../../../fixtures/minecraft/1.12.2.json"),
        "1.20.1.json" => include_str!("../../../fixtures/minecraft/1.20.1.json"),
        "1.21.1.json" => include_str!("../../../fixtures/minecraft/1.21.1.json"),
        "26.1.json" => include_str!("../../../fixtures/minecraft/26.1.json"),
        other => unimplemented!("no fixture named `{other}`"),
    }
}
