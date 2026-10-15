//! The hook a loader uses to constrain the runtime.
//!
//! # Why "the major matches" is not enough
//!
//! Forge for 1.12.2 needs Java 8 - and so does Forge for 1.16.5 - but they are
//! not interchangeable requirements. Old Forge is known to break on *late* Java
//! 8 updates while later Java-8 Forge is fine on them, and a selector that only
//! compares majors cannot tell those two apart. It would hand 1.12.2 the
//! newest 8u on the machine and produce a crash the user then has to
//! diagnose - which is the silent approximation Ephervex does not allow.
//!
//! So a rule constrains the *whole* version, quarterly update included. That is
//! the entire reason [`crate::version::JavaVersion`] keeps the update number.
//!
//! # Who implements this
//!
//! Nobody here. `ephervex-loaders` owns loader knowledge and depends on this
//! crate; the dependency never runs the other way. This module defines the
//! vocabulary and one ready-made implementation general enough for the loader
//! providers to express known caveats as data rather than as code.
//!
//! # Rules reject; they do not rank
//!
//! A verdict is accept or reject with a structured reason. Ranking among the
//! runtimes that survive is [`crate::selection`]'s job, and keeping the two
//! apart means a loader cannot accidentally express a preference it did not
//! mean to make binding.

use std::fmt;

use crate::adoptium::AdoptiumArchitecture;
use crate::discovery::JavaRuntime;
use crate::version::JavaVersion;

/// What a rule decided about one candidate runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleVerdict {
    Accept,
    Reject(RuleRejection),
}

impl RuleVerdict {
    #[must_use]
    pub const fn is_accepted(&self) -> bool {
        matches!(self, Self::Accept)
    }
}

/// Why a rule refused a runtime.
///
/// Structured rather than a sentence: the frontend renders it through
/// `ephervex-i18n`, and the numbers are what make the message actionable
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleRejection {
    /// The rule's untranslated identifier, e.g. `forge-legacy-java8`.
    pub rule: String,
    pub reason: RejectionReason,
}

impl fmt::Display for RuleRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.reason, self.rule)
    }
}

/// The shapes of incompatibility a rule can report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RejectionReason {
    /// The feature release is wrong outright.
    MajorMismatch { required: u32, found: u32 },
    /// Right major, but the runtime predates what the loader needs.
    TooOld {
        minimum: JavaVersion,
        found: JavaVersion,
    },
    /// Right major, but the runtime is a later update than the loader tolerates
    /// - the Forge 1.12.2 case.
    TooNew {
        maximum: JavaVersion,
        found: JavaVersion,
    },
    /// The runtime is built for a different architecture.
    ArchitectureMismatch { expected: String, found: String },
}

impl fmt::Display for RejectionReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MajorMismatch { required, found } => {
                write!(f, "needs java {required}, found java {found}")
            }
            Self::TooOld { minimum, found } => write!(f, "needs at least {minimum}, found {found}"),
            Self::TooNew { maximum, found } => write!(f, "needs at most {maximum}, found {found}"),
            Self::ArchitectureMismatch { expected, found } => {
                write!(f, "needs {expected}, found {found}")
            }
        }
    }
}

/// A constraint a loader places on the Java runtime.
///
/// Implemented by `ephervex-loaders`. `Send + Sync` because selection runs
/// inside async services shared between frontends.
pub trait JavaCompatibilityRule: fmt::Debug + Send + Sync {
    /// A stable, untranslated identifier, used in logs and in the rejection.
    fn id(&self) -> &str;

    fn evaluate(&self, runtime: &JavaRuntime) -> RuleVerdict;
}

/// A rule expressed entirely as version bounds.
///
/// General enough for the supported compatibility constraints: a loader should
/// be able to state "Java 8, no later than update 320" as data rather than by
/// writing a trait implementation whose logic then has to be reviewed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionBound {
    id: String,
    major: Option<u32>,
    minimum: Option<JavaVersion>,
    maximum: Option<JavaVersion>,
}

impl VersionBound {
    /// A rule that constrains nothing until a bound is added.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            major: None,
            minimum: None,
            maximum: None,
        }
    }

    /// Requires an exact feature release.
    #[must_use]
    pub const fn major(mut self, major: u32) -> Self {
        self.major = Some(major);
        self
    }

    /// Requires at least this version, inclusive.
    #[must_use]
    pub const fn at_least(mut self, minimum: JavaVersion) -> Self {
        self.minimum = Some(minimum);
        self
    }

    /// Requires no more than this version, inclusive.
    ///
    /// Every vendor build of the bound's own update counts as at the bound:
    /// `21.0.3+9` is at most `21.0.3`. A build on the bound itself is ignored.
    /// An emergency patch is not a build: `21.0.3.1` is above `21.0.3`.
    ///
    /// This is the bound old Forge needs, and the one a major-only check cannot
    /// express.
    #[must_use]
    pub const fn at_most(mut self, maximum: JavaVersion) -> Self {
        self.maximum = Some(maximum);
        self
    }

    /// The inclusive upper bound, when this rule declares one.
    #[must_use]
    pub const fn maximum(&self) -> Option<JavaVersion> {
        self.maximum
    }

    /// The inclusive lower bound, when this rule declares one.
    #[must_use]
    pub const fn minimum(&self) -> Option<JavaVersion> {
        self.minimum
    }
}

/// The release a runtime is on, without its vendor build.
///
/// An emergency patch is part of the release: `21.0.3.1` is newer than
/// `21.0.3`, so a ceiling at `21.0.3` does not admit it. Write the patch into
/// the bound to admit the patched release.
const fn release(version: JavaVersion) -> JavaVersion {
    version.with_build(0)
}

impl JavaCompatibilityRule for VersionBound {
    fn id(&self) -> &str {
        &self.id
    }

    fn evaluate(&self, runtime: &JavaRuntime) -> RuleVerdict {
        let found = runtime.version;
        let reject = |reason| {
            RuleVerdict::Reject(RuleRejection {
                rule: self.id.clone(),
                reason,
            })
        };

        if let Some(required) = self.major
            && found.major != required
        {
            return reject(RejectionReason::MajorMismatch {
                required,
                found: found.major,
            });
        }
        if let Some(minimum) = self.minimum
            && found < minimum
        {
            return reject(RejectionReason::TooOld { minimum, found });
        }
        if let Some(maximum) = self.maximum
            && release(found) > release(maximum)
        {
            return reject(RejectionReason::TooNew { maximum, found });
        }
        RuleVerdict::Accept
    }
}

/// The spelling both sides of an architecture comparison are reduced to.
///
/// A JVM says `amd64`, Rust says `x86_64` and Adoptium says `x64`, so a raw
/// string comparison would call the same processor a mismatch. An architecture
/// Adoptium does not publish is compared by its own lowercased spelling.
fn canonical_architecture(arch: &str) -> String {
    AdoptiumArchitecture::from_java_arch(arch).map_or_else(
        || arch.trim().to_ascii_lowercase(),
        |known| known.as_str().to_owned(),
    )
}

/// Requires a runtime to be built for the architecture the launcher runs on.
///
/// A runtime for another architecture is not a slower choice, it is a broken
/// one: natives are resolved for the host, and a 32-bit JVM on a 64-bit host
/// cannot reserve the default heap. The plan would succeed and the launch fail
/// late, so the runtime is refused up front.
///
/// A runtime that did not report `os.arch` is refused too. Its architecture
/// cannot be verified, and accepting it would be the silent approximation a
/// selection must not make. The rejection names it as `unknown`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostArchitecture {
    expected: String,
}

impl HostArchitecture {
    /// The id used in logs and in the rejection.
    pub const RULE_ID: &'static str = "host-architecture";

    /// The architecture this build of Ephervex is running on.
    #[must_use]
    pub fn current() -> Self {
        Self::new(std::env::consts::ARCH)
    }

    /// A host architecture in any of the JVM, Rust or Adoptium spellings.
    #[must_use]
    pub fn new(host: &str) -> Self {
        Self {
            expected: canonical_architecture(host),
        }
    }
}

impl JavaCompatibilityRule for HostArchitecture {
    fn id(&self) -> &str {
        Self::RULE_ID
    }

    fn evaluate(&self, runtime: &JavaRuntime) -> RuleVerdict {
        let found = canonical_architecture(&runtime.arch);
        if !found.is_empty() && found == self.expected {
            return RuleVerdict::Accept;
        }
        RuleVerdict::Reject(RuleRejection {
            rule: Self::RULE_ID.to_owned(),
            reason: RejectionReason::ArchitectureMismatch {
                expected: self.expected.clone(),
                found: if found.is_empty() {
                    "unknown".to_owned()
                } else {
                    found
                },
            },
        })
    }
}

/// The Java 8 update from which Forge is reported to stop working.
///
/// **This number is policy, not a contract.** No vendor documents it, and it
/// lives here as a named constant precisely so it can be corrected without
/// touching the rule engine, and so a reader can see it is a judgement call
/// rather than something the code derived. It is deliberately conservative:
/// being a few updates below the real boundary costs nothing, while being one
/// above it costs a crash the user has to diagnose.
///
/// Two separate breakages sit above this line, which is why it applies to
/// every Java 8 Forge rather than to one era. Up to 1.12.2 Forge launches
/// through LaunchWrapper; from 1.13 through ModLauncher, whose
/// `SecureJarHandler` links `sun.security.util.ManifestEntryVerifier`'s
/// one-argument constructor. Later Java 8 updates gave that constructor a
/// second parameter, so loading any class out of a jar whose manifest carries
/// entry digests -- which the Minecraft client jar does -- raises
/// `NoSuchMethodError` before a window ever opens.
///
/// The Forge provider decides which builds it applies to. This crate only has
/// to be able to express it.
pub const LEGACY_FORGE_MAX_JAVA_8_UPDATE: u32 = 320;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use std::path::PathBuf;

    use super::*;
    use crate::discovery::{JavaOrigin, JavaSourceKind};

    fn runtime(version: JavaVersion) -> JavaRuntime {
        JavaRuntime {
            executable: PathBuf::from("/usr/lib/jvm/x/bin/java"),
            home: None,
            version,
            version_string: version.to_string(),
            runtime_version: version.to_string(),
            vendor: "Private Build".to_owned(),
            arch: "amd64".to_owned(),
            origins: vec![JavaOrigin {
                kind: JavaSourceKind::System,
                path: PathBuf::from("/usr/lib/jvm/x/bin/java"),
            }],
        }
    }

    /// What the Forge provider registers for 1.12.2: Java 8, and not a late
    /// one.
    fn forge_1_12_2() -> VersionBound {
        VersionBound::new("forge-legacy-java8")
            .major(8)
            .at_most(JavaVersion::new(8, 0, LEGACY_FORGE_MAX_JAVA_8_UPDATE))
    }

    /// What it will register for a later Java-8 Forge release - 1.16.5, the
    /// last Minecraft version on Java 8. Same major, no update ceiling.
    fn forge_1_16_5() -> VersionBound {
        VersionBound::new("forge-java8").major(8)
    }

    #[test]
    fn forge_1_12_2_refuses_a_late_java_8_that_forge_1_16_5_accepts() {
        // A current Java 8 update must still satisfy an exact Java 8 requirement.
        // The fixture's Java 8 is 8u492: fine for 1.16.5, and the exact runtime
        // a major-only check would silently hand to 1.12.2.
        let late_java_8 = runtime(JavaVersion::new(8, 0, 492));

        let legacy = forge_1_12_2().evaluate(&late_java_8);
        assert!(
            matches!(
                &legacy,
                RuleVerdict::Reject(RuleRejection {
                    reason: RejectionReason::TooNew { .. },
                    ..
                })
            ),
            "{legacy:?}"
        );

        assert!(
            forge_1_16_5().evaluate(&late_java_8).is_accepted(),
            "the same runtime is fine for a later Java-8 Forge"
        );
    }

    #[test]
    fn forge_1_12_2_accepts_an_early_java_8() {
        assert!(
            forge_1_12_2()
                .evaluate(&runtime(JavaVersion::new(8, 0, 202)))
                .is_accepted()
        );
        assert!(
            forge_1_12_2()
                .evaluate(&runtime(JavaVersion::new(
                    8,
                    0,
                    LEGACY_FORGE_MAX_JAVA_8_UPDATE
                )))
                .is_accepted(),
            "the bound is inclusive"
        );
    }

    #[test]
    fn both_forge_rules_refuse_a_runtime_of_the_wrong_major() {
        for rule in [forge_1_12_2(), forge_1_16_5()] {
            let verdict = rule.evaluate(&runtime(JavaVersion::new(17, 0, 19)));
            assert!(
                matches!(
                    &verdict,
                    RuleVerdict::Reject(RuleRejection {
                        reason: RejectionReason::MajorMismatch {
                            required: 8,
                            found: 17
                        },
                        ..
                    })
                ),
                "{verdict:?}"
            );
        }
    }

    #[test]
    fn a_minimum_bound_rejects_a_runtime_that_is_too_old() {
        let rule = VersionBound::new("needs-a-recent-21").at_least(JavaVersion::new(21, 0, 3));
        let verdict = rule.evaluate(&runtime(JavaVersion::new(21, 0, 1)));
        assert!(
            matches!(
                &verdict,
                RuleVerdict::Reject(RuleRejection {
                    reason: RejectionReason::TooOld { .. },
                    ..
                })
            ),
            "{verdict:?}"
        );
        assert!(
            rule.evaluate(&runtime(JavaVersion::new(21, 0, 11)))
                .is_accepted()
        );
    }

    #[test]
    fn a_rejection_names_the_rule_that_produced_it() {
        // With several loader rules in play, "rejected" is only actionable if
        // the user can see which one said so.
        let RuleVerdict::Reject(rejection) =
            forge_1_12_2().evaluate(&runtime(JavaVersion::new(8, 0, 492)))
        else {
            panic!("expected a rejection");
        };
        assert_eq!(rejection.rule, "forge-legacy-java8");
    }

    #[test]
    fn an_unconstrained_rule_accepts_anything() {
        let rule = VersionBound::new("no-opinion");
        assert!(
            rule.evaluate(&runtime(JavaVersion::new(8, 0, 492)))
                .is_accepted()
        );
        assert!(
            rule.evaluate(&runtime(JavaVersion::new(25, 0, 3)))
                .is_accepted()
        );
    }

    #[test]
    fn an_at_most_bound_accepts_a_build_on_its_own_update() {
        let rule = VersionBound::new("x")
            .major(21)
            .at_most(JavaVersion::new(21, 0, 3));
        assert!(
            rule.evaluate(&runtime(JavaVersion::new(21, 0, 3).with_build(9)))
                .is_accepted(),
            "a build on the bound's own update is at the bound, not past it"
        );
    }

    #[test]
    fn an_at_most_bound_rejects_an_emergency_patch_of_its_own_update() {
        let rule = VersionBound::new("x")
            .major(21)
            .at_most(JavaVersion::new(21, 0, 3));
        let patched = JavaVersion::parse("21.0.3.1+1").expect("parses");
        assert!(
            !rule.evaluate(&runtime(patched)).is_accepted(),
            "21.0.3.1 is newer than every 21.0.3 build"
        );
        assert!(
            VersionBound::new("x")
                .major(21)
                .at_most(JavaVersion::parse("21.0.3.1").expect("parses"))
                .evaluate(&runtime(patched))
                .is_accepted(),
            "a bound that names the patch admits it"
        );
    }

    #[test]
    fn an_at_least_bound_counts_an_emergency_patch_above_its_release() {
        let rule = VersionBound::new("x").at_least(JavaVersion::parse("17.0.4.1").expect("parses"));
        assert!(
            !rule
                .evaluate(&runtime(JavaVersion::new(17, 0, 4).with_build(8)))
                .is_accepted()
        );
        assert!(
            rule.evaluate(&runtime(JavaVersion::parse("17.0.4.1+1").expect("parses")))
                .is_accepted()
        );
    }

    #[test]
    fn an_at_most_bound_still_rejects_a_build_on_the_next_update() {
        let rule = VersionBound::new("x")
            .major(21)
            .at_most(JavaVersion::new(21, 0, 3));
        assert!(
            !rule
                .evaluate(&runtime(JavaVersion::new(21, 0, 4).with_build(1)))
                .is_accepted()
        );
        assert!(
            !forge_1_12_2()
                .evaluate(&runtime(
                    JavaVersion::new(8, 0, LEGACY_FORGE_MAX_JAVA_8_UPDATE + 1).with_build(7)
                ))
                .is_accepted()
        );
    }
}
