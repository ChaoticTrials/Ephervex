//! The third-party license inventory compiled into this build.
//!
//! One service behind both frontends. The desktop application's "About &
//! Licenses" screen and `ephervex licenses` read the same document, so they
//! cannot disagree about what Ephervex ships or under what terms.
//!
//! Nothing here decides anything. The inventory is generated from the real
//! dependency graph in `Cargo.lock` by `tools/licenses/generate.sh` and
//! committed; a dependency change that is not regenerated fails CI. Ephervex
//! does not invent dependency license information, and there is deliberately
//! no code path that would let it: the only source is the constant below.
//!
//! Attribution itself is satisfied by the `THIRD-PARTY-LICENSES.md` every
//! release artifact carries, not by this. A screen a user may never open is
//! not how a license's "distribute this text with the software" condition is
//! met; it is how they find it when they look.

use std::sync::OnceLock;

use serde::Deserialize;

/// The inventory format this build reads.
///
/// A document from a future Ephervex is refused rather than partly understood,
/// for the same reason every other persisted format is
/// (see `docs/internal-format-versions.md`).
pub const LICENSE_INVENTORY_FORMAT_VERSION: u32 = 1;

/// The inventory compiled into the binary. There is no other source: no code
/// path loads one from the network, a cache, or a file.
const BUNDLED_INVENTORY: &str = include_str!("../data/third-party-licenses.json");

/// Why an inventory could not be read.
#[derive(Debug, thiserror::Error)]
pub enum LicenseInventoryError {
    #[error("the third-party license inventory is not valid JSON: {0}")]
    Malformed(#[from] serde_json::Error),
    #[error(
        "the third-party license inventory is version {found}, and this build reads version {expected}"
    )]
    UnsupportedVersion { found: u32, expected: u32 },
    #[error("{crate_name} {version} names license text {index}, which the inventory does not have")]
    DanglingText {
        crate_name: String,
        version: String,
        index: usize,
    },
}

/// One license's full text, as the crate that ships it wrote it.
#[derive(Debug, Clone, Deserialize)]
pub struct LicenseText {
    /// The SPDX identifier, or a `LicenseRef-` name for terms SPDX does not
    /// list.
    pub id: String,
    /// The license's name in prose, which for a `LicenseRef-` is the
    /// identifier again.
    pub name: String,
    pub text: String,
}

/// One dependency and the terms it is under.
#[derive(Debug, Clone, Deserialize)]
pub struct DependencyLicense {
    pub name: String,
    pub version: String,
    /// The crate's own SPDX expression, exactly as its manifest declares it.
    /// It may offer a choice (`MIT OR Apache-2.0`); `texts` is what Ephervex
    /// actually ships for it.
    pub license: String,
    /// Indices into [`LicenseInventory::licenses`].
    pub texts: Vec<usize>,
}

/// Every third-party dependency in this build, and every license text they are
/// covered by.
#[derive(Debug, Clone, Deserialize)]
pub struct LicenseInventory {
    version: u32,
    licenses: Vec<LicenseText>,
    crates: Vec<DependencyLicense>,
}

impl LicenseInventory {
    /// The inventory compiled into this build.
    pub fn bundled() -> Result<Self, LicenseInventoryError> {
        Self::parse(BUNDLED_INVENTORY)
    }

    /// The bundled inventory, read once for the whole process.
    ///
    /// Five hundred crates and a hundred license texts is a document worth
    /// parsing once rather than every time a screen opens. The failure is
    /// still returned rather than swallowed, because a broken build should be
    /// reportable rather than fatal.
    pub fn bundled_shared() -> Result<&'static Self, &'static LicenseInventoryError> {
        static BUNDLED: OnceLock<Result<LicenseInventory, LicenseInventoryError>> = OnceLock::new();
        BUNDLED.get_or_init(Self::bundled).as_ref()
    }

    /// Reads a document and checks that every crate's texts exist.
    ///
    /// The index check is not defensive tidiness: an index into a list is the
    /// one thing in this format that can be wrong without being malformed, and
    /// an inventory that silently dropped a license would show a dependency
    /// with no terms at all.
    pub fn parse(document: &str) -> Result<Self, LicenseInventoryError> {
        let inventory: Self = serde_json::from_str(document)?;
        if inventory.version != LICENSE_INVENTORY_FORMAT_VERSION {
            return Err(LicenseInventoryError::UnsupportedVersion {
                found: inventory.version,
                expected: LICENSE_INVENTORY_FORMAT_VERSION,
            });
        }
        for dependency in &inventory.crates {
            for &index in &dependency.texts {
                if index >= inventory.licenses.len() {
                    return Err(LicenseInventoryError::DanglingText {
                        crate_name: dependency.name.clone(),
                        version: dependency.version.clone(),
                        index,
                    });
                }
            }
        }
        Ok(inventory)
    }

    /// Every dependency, ordered by name and version as the inventory records
    /// them.
    #[must_use]
    pub fn dependencies(&self) -> &[DependencyLicense] {
        &self.crates
    }

    /// Every distinct license text, ordered as the inventory records them.
    #[must_use]
    pub fn licenses(&self) -> &[LicenseText] {
        &self.licenses
    }

    /// The texts one dependency is covered by.
    ///
    /// Empty only for a dependency this inventory does not hold; every one it
    /// does hold has at least one, because generation refuses a crate whose
    /// terms it could not find.
    #[must_use]
    pub fn texts_for(&self, dependency: &DependencyLicense) -> Vec<&LicenseText> {
        dependency
            .texts
            .iter()
            .filter_map(|&index| self.licenses.get(index))
            .collect()
    }

    /// One license text by its index, as [`DependencyLicense::texts`] names
    /// them.
    #[must_use]
    pub fn text(&self, index: usize) -> Option<&LicenseText> {
        self.licenses.get(index)
    }

    /// Each distinct license identifier and how many dependencies it covers,
    /// ordered by identifier.
    ///
    /// A summary rather than a second listing: several crates under MIT ship
    /// several MIT files that differ in their copyright line, so the texts are
    /// distinct while the identifier is one.
    #[must_use]
    pub fn identifier_counts(&self) -> Vec<(&str, usize)> {
        let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
        for dependency in &self.crates {
            let mut seen: Vec<&str> = self
                .texts_for(dependency)
                .into_iter()
                .map(|text| text.id.as_str())
                .collect();
            seen.sort_unstable();
            seen.dedup();
            for identifier in seen {
                *counts.entry(identifier).or_default() += 1;
            }
        }
        counts.into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    const ONE_CRATE: &str = r#"{"version":1,"licenses":[{"id":"MIT","name":"MIT License","text":"the terms"}],"crates":[{"name":"alpha","version":"1.0.0","license":"MIT","texts":[0]}]}"#;

    #[test]
    fn the_bundled_inventory_parses() {
        let inventory = LicenseInventory::bundled().expect("the bundled inventory must parse");
        assert!(
            !inventory.dependencies().is_empty(),
            "a build with no dependencies is a generation failure, not an empty inventory"
        );
        assert!(!inventory.licenses().is_empty());
    }

    #[test]
    fn every_bundled_dependency_has_terms_to_show() {
        let inventory = LicenseInventory::bundled().expect("the bundled inventory must parse");
        for dependency in inventory.dependencies() {
            assert!(
                !inventory.texts_for(dependency).is_empty(),
                "{} {} is listed with no license text",
                dependency.name,
                dependency.version
            );
            assert!(!dependency.license.is_empty());
        }
    }

    #[test]
    fn the_bundled_inventory_carries_the_slint_terms_it_ships_under() {
        // The desktop application is built on Slint under its royalty-free
        // license, whose attribution condition the About screen satisfies
        // (ADR 0001). Shipping the toolkit without its text would be shipping
        // it outside those terms.
        let inventory = LicenseInventory::bundled().expect("the bundled inventory must parse");
        let slint = inventory
            .dependencies()
            .iter()
            .find(|dependency| dependency.name == "slint")
            .expect("the desktop application depends on slint");
        let texts = inventory.texts_for(slint);
        assert!(
            texts
                .iter()
                .any(|text| text.id.starts_with("LicenseRef-Slint-")
                    && text.text.contains("AboutSlint")),
            "slint is listed without the royalty-free terms Ephervex uses it under"
        );
    }

    #[test]
    fn the_bundled_inventory_lists_the_fonts_with_their_own_license_texts() {
        let inventory = LicenseInventory::bundled().expect("the bundled inventory must parse");
        for (font, notice) in [
            ("Pixelify Sans (font)", "The Pixelify Sans Project Authors"),
            ("Space Grotesk (font)", "The Space Grotesk Project Authors"),
            ("IBM Plex Mono (font)", "Reserved Font Name \"Plex\""),
        ] {
            let dependency = inventory
                .dependencies()
                .iter()
                .find(|dependency| dependency.name == font);
            assert!(dependency.is_some(), "{font} is bundled but not listed");
            let dependency = dependency.expect("checked above");
            assert_eq!(dependency.license, "OFL-1.1");
            assert!(
                inventory
                    .texts_for(dependency)
                    .iter()
                    .any(|text| text.id == "OFL-1.1" && text.text.contains(notice)),
                "{font} is listed without its own notice"
            );
        }
    }

    #[test]
    fn a_dependency_names_the_texts_that_cover_it() {
        let inventory = LicenseInventory::parse(ONE_CRATE).expect("parses");
        let alpha = &inventory.dependencies()[0];
        assert_eq!(alpha.name, "alpha");
        assert_eq!(inventory.texts_for(alpha)[0].text, "the terms");
    }

    #[test]
    fn a_document_from_a_future_build_is_refused() {
        let document = ONE_CRATE.replace("\"version\":1", "\"version\":2");
        assert!(matches!(
            LicenseInventory::parse(&document),
            Err(LicenseInventoryError::UnsupportedVersion {
                found: 2,
                expected: 1
            })
        ));
    }

    #[test]
    fn a_dependency_naming_a_text_that_is_not_there_is_refused() {
        let document = ONE_CRATE.replace("\"texts\":[0]", "\"texts\":[7]");
        assert!(matches!(
            LicenseInventory::parse(&document),
            Err(LicenseInventoryError::DanglingText { index: 7, .. })
        ));
    }

    #[test]
    fn a_damaged_document_is_refused_rather_than_partly_read() {
        assert!(matches!(
            LicenseInventory::parse("{\"version\":1,"),
            Err(LicenseInventoryError::Malformed(_))
        ));
    }

    #[test]
    fn identifiers_are_counted_once_per_dependency() {
        // Two crates under MIT with different copyright lines are two texts
        // and one identifier.
        let document = r#"{"version":1,"licenses":[{"id":"MIT","name":"MIT License","text":"a"},{"id":"MIT","name":"MIT License","text":"b"}],"crates":[{"name":"alpha","version":"1.0.0","license":"MIT","texts":[0]},{"name":"beta","version":"2.0.0","license":"MIT","texts":[1]}]}"#;
        let inventory = LicenseInventory::parse(document).expect("parses");
        assert_eq!(inventory.identifier_counts(), vec![("MIT", 2)]);
        assert_eq!(inventory.licenses().len(), 2);
    }
}
