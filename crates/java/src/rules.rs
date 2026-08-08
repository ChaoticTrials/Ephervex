//! The hook a loader uses to constrain the runtime.
//!
//! # Why "the major matches" is not enough
//!
//! Forge for 1.12.2 needs Java 8 — and so does Forge for 1.16.5 — but they are
//! not interchangeable requirements. Old Forge is known to break on *late* Java
//! 8 updates while later Java-8 Forge is fine on them, and a selector that only
//! compares majors cannot tell those two apart. It would hand 1.12.2 the
//! newest 8u on the machine and produce a crash the user then has to
//! diagnose — which is the silent approximation Ephervex does not allow.
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
    /// — the Forge 1.12.2 case.
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
/// General enough for every caveat the compatibility matrix coverss, which is the point: a loader
/// should be able to state "Java 8, no later than update 320" as data rather
/// than by writing a trait implementation whose logic then has to be reviewed.
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
            && found > maximum
        {
            return reject(RejectionReason::TooNew { maximum, found });
        }
        RuleVerdict::Accept
    }
}

/// The Java 8 update from which old Forge is reported to stop working.
///
/// **This number is policy, not a contract.** No vendor documents it: it is the
/// most widely reported boundary in the modding community for Forge builds
/// predating 1.13, whose launch path does things later 8u releases changed. It
/// lives here as a named constant precisely so it can be corrected without
/// touching the rule engine, and so that a reader can see it is a judgement
/// call rather than something the code derived.
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

    /// What it will register for a later Java-8 Forge release — 1.16.5, the
    /// last Minecraft version on Java 8. Same major, no update ceiling.
    fn forge_1_16_5() -> VersionBound {
        VersionBound::new("forge-java8").major(8)
    }

    #[test]
    fn forge_1_12_2_refuses_a_late_java_8_that_forge_1_16_5_accepts() {
        // A current Java 8 update must still satisfy an exact Java 8 requirement.
        // This machine's Java 8 is 8u492 —
        // fine for 1.16.5, and the exact runtime a major-only check would
        // silently hand to 1.12.2.
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
}
