//! What a host expects of its Java runtime, stated as data.
//!
//! The required major comes from Minecraft's own metadata and the loader adds
//! version bounds on top. [`ExpectedJava`] folds both into one description a
//! frontend can print next to the runtime a host would use ("Java 17", "Java 8,
//! update 320 or older"), built from the same [`VersionBound`] rules selection
//! evaluates rather than from a second table.

use crate::requirement::RequiredJava;
use crate::rules::VersionBound;
use crate::version::JavaVersion;

/// The Java a Minecraft and loader combination expects.
///
/// Assumes each bound constrains the required major only, which is how loader
/// providers build them. A bound written for another major is left out of the
/// description, although selection still evaluates it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpectedJava {
    /// The feature release every runtime must have.
    pub major: u32,
    /// The earliest update of that release the loader accepts, inclusive.
    pub minimum: Option<JavaVersion>,
    /// The latest update of that release the loader accepts, inclusive.
    pub maximum: Option<JavaVersion>,
    maximum_update: Option<u32>,
}

impl ExpectedJava {
    /// Combines the required major with the loader's bounds, keeping the
    /// tightest bound on each side.
    #[must_use]
    pub fn new(required: &RequiredJava, bounds: &[VersionBound]) -> Self {
        let minimum = bounds
            .iter()
            .filter_map(VersionBound::minimum)
            .filter(|minimum| minimum.major == required.major)
            .max();
        let maximums = || {
            bounds
                .iter()
                .filter_map(VersionBound::maximum)
                .filter(|maximum| maximum.major == required.major)
        };
        Self {
            major: required.major,
            minimum,
            maximum: maximums().min(),
            maximum_update: maximums().map(|maximum| maximum.update).min(),
        }
    }

    /// The newest update of the required major a managed install may fetch.
    ///
    /// The ceiling both automatic installs at launch and an explicit install
    /// request apply, so neither fetches a build the loader would then refuse.
    #[must_use]
    pub const fn maximum_update(&self) -> Option<u32> {
        self.maximum_update
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::requirement::RequirementBasis;
    use crate::rules::LEGACY_FORGE_MAX_JAVA_8_UPDATE;

    fn required(major: u32) -> RequiredJava {
        RequiredJava {
            major,
            basis: RequirementBasis::FallbackTable,
        }
    }

    #[test]
    fn a_requirement_without_loader_bounds_is_only_a_major() {
        let expected = ExpectedJava::new(&required(17), &[]);
        assert_eq!(expected.major, 17);
        assert_eq!(expected.minimum, None);
        assert_eq!(expected.maximum, None);
        assert_eq!(expected.maximum_update(), None);
    }

    #[test]
    fn a_legacy_forge_ceiling_is_the_update_an_install_must_not_exceed() {
        let bounds = [VersionBound::new("forge-legacy-java8")
            .major(8)
            .at_most(JavaVersion::new(8, 0, LEGACY_FORGE_MAX_JAVA_8_UPDATE))];
        let expected = ExpectedJava::new(&required(8), &bounds);
        assert_eq!(
            expected.maximum_update(),
            Some(LEGACY_FORGE_MAX_JAVA_8_UPDATE)
        );
    }

    #[test]
    fn the_tightest_bound_on_each_side_wins() {
        let bounds = [
            VersionBound::new("a")
                .at_least(JavaVersion::new(21, 0, 2))
                .at_most(JavaVersion::new(21, 0, 9)),
            VersionBound::new("b")
                .at_least(JavaVersion::new(21, 0, 4))
                .at_most(JavaVersion::new(21, 0, 7)),
        ];
        let expected = ExpectedJava::new(&required(21), &bounds);
        assert_eq!(expected.minimum, Some(JavaVersion::new(21, 0, 4)));
        assert_eq!(expected.maximum, Some(JavaVersion::new(21, 0, 7)));
    }

    #[test]
    fn the_displayed_ceiling_is_the_one_evaluation_enforces() {
        let patched = JavaVersion::parse("21.0.3.1").expect("parses");
        let bounds = [
            VersionBound::new("a").at_most(patched),
            VersionBound::new("b").at_most(JavaVersion::new(21, 0, 3)),
        ];
        let expected = ExpectedJava::new(&required(21), &bounds);
        assert_eq!(expected.maximum, Some(JavaVersion::new(21, 0, 3)));
        assert_eq!(expected.maximum_update(), Some(3));
    }

    #[test]
    fn a_bound_on_another_major_is_not_part_of_the_expectation() {
        let bounds = [VersionBound::new("old")
            .at_least(JavaVersion::new(8, 0, 100))
            .at_most(JavaVersion::new(8, 0, 200))];
        let expected = ExpectedJava::new(&required(17), &bounds);
        assert_eq!(expected.minimum, None);
        assert_eq!(expected.maximum, None);
    }
}
