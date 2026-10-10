//! Parsing and ordering the strings a JVM reports as its version.
//!
//! # The trap this module exists for
//!
//! Java changed its version scheme in Java 9 (JEP 223), and the two schemes
//! disagree about where the major version lives:
//!
//! ```text
//! 1.8.0_492   ->  major 8    the leading `1.` is a fossil; the major is second
//! 17.0.19     ->  major 17   the major is first
//! ```
//!
//! Taking "the first component" gives major `1` for every Java 8 installation
//! in existence, and a launcher that does so will refuse a perfectly good
//! runtime for 1.12.2 while cheerfully offering it for a version that requires
//! Java 1. It is the single easiest thing to get wrong here, so it has its own
//! tests.
//!
//! # Why the third number matters
//!
//! Old Forge does not merely need "a Java 8" - it needs a Java 8 below some
//! update, because later 8u releases changed behavior it depends on. The parsed
//! form therefore keeps the quarterly-update number, which
//! is `_492` in the legacy scheme and the third dotted component in the modern
//! one. Both are "the number that goes up every quarter", which is exactly what
//! a compatibility bound needs to compare.
//!
//! # Emergency patch releases
//!
//! JEP 322 allows a fourth number after the quarterly update, for an
//! emergency fix of that update: `17.0.4.1+1` replaces `17.0.4+8`. The parsed
//! form keeps it as `patch` so the fix orders above every build of the release
//! it patches instead of collapsing onto it.
//!
//! # Ordering
//!
//! [`JavaVersion`] is a plain ordered tuple: major, then minor, then the
//! quarterly number, then the emergency patch, then the build. The raw text is
//! deliberately *not* a field - two spellings of the same version must compare
//! equal, and a `raw` field would make `Ord` disagree with `PartialEq`. Callers
//! that want to show what the JVM actually printed keep the string beside the
//! parsed value.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A JVM version, normalized across both of Java's numbering schemes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct JavaVersion {
    /// The feature release: 8 for `1.8.0_492`, 17 for `17.0.19`.
    pub major: u32,
    pub minor: u32,
    /// The quarterly update: `492` from `1.8.0_492`, `19` from `17.0.19`.
    ///
    /// Named for what it is used for rather than for either scheme's spelling,
    /// because the whole point is that a bound can be written once and hold for
    /// both.
    pub update: u32,
    /// The emergency patch of the update: `1` from `17.0.4.1+1`, `0` when the
    /// string carried none.
    ///
    /// Sits between `update` and `build` so the derived `Ord` ranks a patch
    /// release above every build of the update it fixes.
    #[serde(default)]
    pub patch: u32,
    /// The vendor build number, or `0` when the string did not carry one.
    ///
    /// Folded into the ordering rather than left `Option` so that `Ord` and
    /// `PartialEq` cannot disagree. No JVM publishes build 0, so nothing real
    /// is lost.
    pub build: u32,
}

impl JavaVersion {
    #[must_use]
    pub const fn new(major: u32, minor: u32, update: u32) -> Self {
        Self {
            major,
            minor,
            update,
            patch: 0,
            build: 0,
        }
    }

    /// The lowest version with this major, useful as a bound.
    #[must_use]
    pub const fn major_floor(major: u32) -> Self {
        Self::new(major, 0, 0)
    }

    #[must_use]
    pub const fn with_build(mut self, build: u32) -> Self {
        self.build = build;
        self
    }

    /// Parses a `java.version` or `java.runtime.version` value.
    ///
    /// Returns `None` for anything that does not begin with a number, which is
    /// what a truncated or localized banner looks like. A runtime whose version
    /// cannot be read is skipped rather than guessed at.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim().trim_matches('"');

        // `17.0.19+10-1-24.04.2-Ubuntu` and `1.8.0_492-8u492-...-b09` both carry
        // a vendor tail after the build. Everything from the second `-` onward
        // is packaging detail, and `25-ea` is a tail with no build at all.
        let (head, tail) = match text.split_once('+') {
            Some((head, tail)) => (head, Some(tail)),
            None => (text, None),
        };
        let head = head.split('-').next().unwrap_or(head);
        let build = tail.map_or(0, |tail| leading_number(tail).unwrap_or(0));

        // Java 8 spells the update `_492`; everything else uses a dot.
        let (dotted, legacy_update) = match head.split_once('_') {
            Some((dotted, update)) => (dotted, leading_number(update)),
            None => (head, None),
        };

        let mut components = Vec::new();
        for part in dotted.split('.') {
            match leading_number(part) {
                Some(value) => components.push(value),
                // A trailing empty component (`17.`) ends the number; a
                // non-numeric one (`1.8.0-internal`) has already been stripped.
                None => break,
            }
        }

        let first = *components.first()?;
        // The fossil `1.` prefix. `1` alone is not a Java version anyone ships,
        // and treating it as major 1 would be no more useful than rejecting it.
        let (major, minor, dotted_update, patch) = if first == 1 && components.len() >= 2 {
            (
                components[1],
                components.get(2).copied().unwrap_or(0),
                components.get(3).copied().unwrap_or(0),
                components.get(4).copied().unwrap_or(0),
            )
        } else {
            (
                first,
                components.get(1).copied().unwrap_or(0),
                components.get(2).copied().unwrap_or(0),
                components.get(3).copied().unwrap_or(0),
            )
        };

        Some(Self {
            major,
            minor,
            // The legacy `_492` wins where present: in `1.8.0_492` the dotted
            // third component is always 0 and carries nothing.
            update: legacy_update.unwrap_or(dotted_update),
            patch,
            build,
        })
    }

    /// Extracts the version from a `java -version` banner.
    ///
    /// The fallback for a JVM too old to support `-XshowSettings:properties`.
    /// The first quoted token of the first line is the version in every banner
    /// either scheme has published:
    ///
    /// ```text
    /// openjdk version "1.8.0_492"
    /// openjdk version "17.0.19" 2026-04-21
    /// ```
    #[must_use]
    pub fn parse_banner(banner: &str) -> Option<Self> {
        let quoted = banner.lines().find_map(|line| {
            line.split_once('"')
                .and_then(|(_, rest)| rest.split_once('"'))
        })?;
        Self::parse(quoted.0)
    }

    /// Whether this runtime uses the pre-Java-9 numbering scheme.
    #[must_use]
    pub const fn is_legacy_scheme(&self) -> bool {
        self.major <= 8
    }
}

impl fmt::Display for JavaVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The `1.x.y_z` spelling has no slot for a patch, so a patched legacy
        // major is written in the dotted form, which parses back to the same
        // value.
        if self.is_legacy_scheme() && self.patch == 0 {
            write!(f, "1.{}.{}_{}", self.major, self.minor, self.update)?;
        } else {
            write!(f, "{}.{}.{}", self.major, self.minor, self.update)?;
            if self.patch != 0 {
                write!(f, ".{}", self.patch)?;
            }
        }
        if self.build != 0 {
            write!(f, "+{}", self.build)?;
        }
        Ok(())
    }
}

/// The leading run of ASCII digits, as a number.
fn leading_number(text: &str) -> Option<u32> {
    let digits: String = text
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn java_8_reports_its_major_in_the_second_component() {
        // The bug this whole module exists to prevent. `1.8.0_492` is what
        // every Java 8 on Earth prints, and "the first component" reads it as
        // major 1.
        let version = JavaVersion::parse("1.8.0_492").expect("parses");
        assert_eq!(version.major, 8, "1.8.0_492 is Java 8, not Java 1");
        assert_eq!(version.minor, 0);
        assert_eq!(version.update, 492);
    }

    #[test]
    fn the_modern_scheme_puts_the_major_first() {
        for (text, expected) in [
            ("17.0.19", JavaVersion::new(17, 0, 19)),
            ("21.0.11", JavaVersion::new(21, 0, 11)),
            ("25.0.3", JavaVersion::new(25, 0, 3)),
            ("26", JavaVersion::new(26, 0, 0)),
        ] {
            assert_eq!(JavaVersion::parse(text).expect(text), expected, "{text}");
        }
    }

    #[test]
    fn a_runtime_version_string_carries_the_build_and_drops_the_packaging_tail() {
        // These four are verbatim `java.runtime.version` values from a Debian
        // family machine; the tail after the build is distribution packaging.
        let ubuntu17 = JavaVersion::parse("17.0.19+10-1-24.04.2-Ubuntu").expect("parses");
        assert_eq!(ubuntu17, JavaVersion::new(17, 0, 19).with_build(10));

        let ubuntu8 =
            JavaVersion::parse("1.8.0_492-8u492-ga~us2-0ubuntu1~24.04.1-b09").expect("parses");
        assert_eq!(ubuntu8.major, 8);
        assert_eq!(ubuntu8.update, 492);

        let ubuntu25 = JavaVersion::parse("25.0.3+9-2-24.04.2-Ubuntu").expect("parses");
        assert_eq!(ubuntu25, JavaVersion::new(25, 0, 3).with_build(9));
    }

    #[test]
    fn an_early_access_suffix_does_not_defeat_parsing() {
        assert_eq!(
            JavaVersion::parse("26-ea").expect("parses"),
            JavaVersion::new(26, 0, 0)
        );
        assert_eq!(
            JavaVersion::parse("21.0.1-internal").expect("parses"),
            JavaVersion::new(21, 0, 1)
        );
    }

    #[test]
    fn nothing_numeric_means_no_version_rather_than_a_guess() {
        for text in ["", "unknown", "not a version", "\"\""] {
            assert!(JavaVersion::parse(text).is_none(), "{text:?}");
        }
    }

    #[test]
    fn banners_from_both_schemes_parse() {
        assert_eq!(
            JavaVersion::parse_banner(
                "openjdk version \"1.8.0_492\"\nOpenJDK Runtime Environment (build 1.8.0_492-b09)"
            )
            .expect("parses")
            .major,
            8
        );
        assert_eq!(
            JavaVersion::parse_banner("openjdk version \"17.0.19\" 2026-04-21")
                .expect("parses")
                .major,
            17
        );
    }

    #[test]
    fn ordering_runs_major_then_minor_then_the_quarterly_update() {
        assert!(JavaVersion::new(8, 0, 51) < JavaVersion::new(8, 0, 492));
        assert!(JavaVersion::new(8, 0, 492) < JavaVersion::new(17, 0, 0));
        // A bound like "Java 8 up to update 320" has to be expressible, which
        // is only true if the update participates in the ordering.
        assert!(JavaVersion::new(8, 0, 320) < JavaVersion::new(8, 0, 321));
        assert!(
            JavaVersion::new(21, 0, 11).with_build(9) < JavaVersion::new(21, 0, 11).with_build(10)
        );
    }

    #[test]
    fn a_fourth_component_is_an_emergency_patch_that_orders_above_its_release() {
        let patched = JavaVersion::parse("17.0.4.1+1").expect("parses");
        assert_eq!(patched.update, 4);
        assert_eq!(patched.patch, 1);
        assert_eq!(patched.build, 1);

        let release = JavaVersion::parse("17.0.4+8").expect("parses");
        assert_eq!(release.patch, 0);
        assert!(patched > release, "the patch replaces the release it fixes");
        assert_ne!(patched.with_build(0), release.with_build(0));
        assert!(
            JavaVersion::parse("17.0.4.2").unwrap() > JavaVersion::parse("17.0.4.1+9").unwrap()
        );
        assert!(JavaVersion::parse("17.0.5").unwrap() > JavaVersion::parse("17.0.4.9").unwrap());
    }

    #[test]
    fn a_packaged_runtime_string_keeps_its_emergency_patch() {
        let ubuntu25 = JavaVersion::parse("25.0.4.1+1-1-24.04.4-Ubuntu").expect("parses");
        assert_eq!(
            ubuntu25,
            JavaVersion {
                patch: 1,
                ..JavaVersion::new(25, 0, 4).with_build(1)
            }
        );
    }

    #[test]
    fn the_legacy_prefix_layout_is_not_mistaken_for_a_patch() {
        // `1.8.0_492` has no fourth dotted component, and `1.8.0.292` keeps
        // meaning "update 292" rather than "patch 292".
        let underscore = JavaVersion::parse("1.8.0_492").expect("parses");
        assert_eq!((underscore.update, underscore.patch), (492, 0));
        let dotted = JavaVersion::parse("1.8.0.292").expect("parses");
        assert_eq!((dotted.update, dotted.patch), (292, 0));
    }

    #[test]
    fn display_round_trips_through_the_parser() {
        for version in [
            JavaVersion::new(8, 0, 492),
            JavaVersion::new(17, 0, 19),
            JavaVersion::new(25, 0, 3).with_build(9),
            JavaVersion::parse("17.0.4.1+1").unwrap(),
            JavaVersion::parse("25.0.4.1").unwrap(),
            JavaVersion::parse("8.0.6.26").unwrap(),
        ] {
            let rendered = version.to_string();
            assert_eq!(
                JavaVersion::parse(&rendered).expect(&rendered),
                version,
                "{rendered}"
            );
        }
    }
}
