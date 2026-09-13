//! Choosing a runtime, and the override layers that can overrule the choice
//!
//! # Two questions, deliberately separated
//!
//! **Which runtime?** [`select`] filters the inventory by the required major
//! and every loader rule, then ranks what survives. **Is the choice even
//! ours?** [`JavaPreference`] answers that first: a pinned path short-circuits
//! selection entirely, and the *reason* it was pinned is part of the answer so
//! a user can be told why their setting won.
//!
//! Keeping them apart matters because a pinned runtime still has to be checked
//! against the rules - pinning means "use this one", not "stop caring whether
//! it works". A pinned runtime that fails the rules is a clear failure, never a
//! quiet substitution.
//!
//! # The precedence
//!
//! ```text
//! explicit request  >  per-instance override  >  global preferred path  >  automatic
//! ```
//!
//! The global setting sits at the bottom because it is a *default*. An instance
//! override must take precedence for "Use global settings" to have a meaningful
//! alternative.
//!
//! # "Use global settings" is a state, not an absence
//!
//! An instance with `jvm_override: None` follows the global settings wherever
//! they go. An instance with `Some(JvmSettings { max_memory_mib: Some(4096) })`
//! is pinned to 4096 MiB even if it currently equals the global value and even
//! after the global value changes. [`EffectiveJvmSettings`] records which
//! fields came from where, so the difference is visible rather than inferred.

use std::collections::BTreeSet;
use std::path::PathBuf;

use ephervex_model::{JavaOverride, JvmSettings};
use tracing::debug;

use crate::discovery::{JavaRuntime, RuntimeInventory};
use crate::error::JavaError;
use crate::requirement::RequiredJava;
use crate::rules::{JavaCompatibilityRule, RejectionReason, RuleRejection, RuleVerdict};

/// The global half of global Java settings, independent of how they are stored.
///
/// A mirror of `ephervex-storage`'s `JavaConfig` rather than the type itself:
/// this crate must not depend on the storage crate to decide which JVM to run,
/// and the configuration *file* schema is storage's business. `ephervex-core`
/// bridges the two.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalJavaSettings {
    pub auto_detect: bool,
    pub auto_download: bool,
    pub min_memory_mib: u32,
    pub max_memory_mib: u32,
    pub jvm_args: Vec<String>,
    /// the preferred Java path setting, or `None` for `Auto...`.
    pub preferred_path: Option<PathBuf>,
}

impl Default for GlobalJavaSettings {
    fn default() -> Self {
        Self {
            auto_detect: true,
            auto_download: true,
            min_memory_mib: 512,
            max_memory_mib: 4096,
            jvm_args: Vec::new(),
            preferred_path: None,
        }
    }
}

/// Which layer supplied the pinned path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinSource {
    /// A path passed to this specific operation - a CLI argument, a "test this
    /// runtime" action.
    ExplicitRequest,
    /// The instance's own [`JavaOverride`].
    InstanceOverride,
    /// The global "Preferred Java path" setting.
    GlobalPreference,
}

impl PinSource {
    /// A stable, untranslated identifier for logs and `--json`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ExplicitRequest => "explicit-request",
            Self::InstanceOverride => "instance-override",
            Self::GlobalPreference => "global-preference",
        }
    }
}

/// How the runtime is to be chosen, once the override layers have been read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JavaPathChoice {
    /// Use this exact executable.
    Pinned { path: PathBuf, source: PinSource },
    /// Pick the best compatible runtime from the inventory.
    Automatic,
}

/// The three override layers of unresolved.
#[derive(Debug, Clone, Default)]
pub struct JavaPreference {
    /// A path supplied for this operation only.
    pub explicit: Option<PathBuf>,
    /// The instance's stored override.
    pub instance: Option<JavaOverride>,
    /// The global "Preferred Java path".
    pub global: Option<PathBuf>,
}

impl JavaPreference {
    /// Applies the precedence.
    #[must_use]
    pub fn resolve(&self) -> JavaPathChoice {
        if let Some(path) = &self.explicit {
            return JavaPathChoice::Pinned {
                path: path.clone(),
                source: PinSource::ExplicitRequest,
            };
        }
        if let Some(override_) = &self.instance {
            return JavaPathChoice::Pinned {
                path: override_.path.clone(),
                source: PinSource::InstanceOverride,
            };
        }
        if let Some(path) = &self.global {
            return JavaPathChoice::Pinned {
                path: path.clone(),
                source: PinSource::GlobalPreference,
            };
        }
        JavaPathChoice::Automatic
    }
}

/// A JVM setting that an instance can pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum JvmField {
    MinMemory,
    MaxMemory,
    JvmArgs,
}

impl JvmField {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MinMemory => "min_memory_mib",
            Self::MaxMemory => "max_memory_mib",
            Self::JvmArgs => "jvm_args",
        }
    }
}

/// The JVM settings an instance actually runs with, and where each came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveJvmSettings {
    pub min_memory_mib: u32,
    pub max_memory_mib: u32,
    pub jvm_args: Vec<String>,
    /// Whether the instance opted out of global settings at all. This preserves
    /// the checkbox state even when the override pins no individual field.
    pub overrides_global: bool,
    /// The fields the instance pinned. A field absent here follows the global
    /// setting and will move when it does.
    pub overridden: BTreeSet<JvmField>,
}

impl EffectiveJvmSettings {
    /// Layers a per-instance override over the global settings.
    #[must_use]
    pub fn resolve(global: &GlobalJavaSettings, instance: Option<&JvmSettings>) -> Self {
        let mut effective = Self {
            min_memory_mib: global.min_memory_mib,
            max_memory_mib: global.max_memory_mib,
            jvm_args: global.jvm_args.clone(),
            overrides_global: instance.is_some(),
            overridden: BTreeSet::new(),
        };

        let Some(instance) = instance else {
            return effective;
        };
        if let Some(value) = instance.min_memory_mib {
            effective.min_memory_mib = value;
            effective.overridden.insert(JvmField::MinMemory);
        }
        if let Some(value) = instance.max_memory_mib {
            effective.max_memory_mib = value;
            effective.overridden.insert(JvmField::MaxMemory);
        }
        if !instance.jvm_args.is_empty() {
            effective.jvm_args.clone_from(&instance.jvm_args);
            effective.overridden.insert(JvmField::JvmArgs);
        }
        effective
    }

    /// Whether a field came from the instance rather than the global settings.
    #[must_use]
    pub fn is_overridden(&self, field: JvmField) -> bool {
        self.overridden.contains(&field)
    }
}

/// A runtime that was considered and refused, with the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectedRuntime {
    pub executable: PathBuf,
    pub version: crate::version::JavaVersion,
    pub rejection: RuleRejection,
}

/// The chosen runtime, and what it was chosen over.
#[derive(Debug, Clone)]
pub struct Selection {
    pub runtime: JavaRuntime,
    /// Every runtime a rule refused. Kept so "why not that one" has an answer
    /// without re-running the scan.
    pub rejected: Vec<RejectedRuntime>,
}

/// The rule every selection applies before the loader's own.
///
/// The required major is not negotiable and does not depend on a loader having
/// registered anything, so it is applied here rather than left to a caller to
/// remember.
fn major_rule(required: &RequiredJava) -> crate::rules::VersionBound {
    crate::rules::VersionBound::new("minecraft-java-major").major(required.major)
}

/// Picks the best runtime satisfying the requirement and every rule.
///
/// Ranking among the survivors is newest-first, with a managed runtime
/// preferred over a system one at equal version - Ephervex installed the
/// managed one for exactly this purpose, and preferring it makes the launcher's
/// behavior reproducible across machines whose distributions patch differently.
///
/// Fails rather than substituting when nothing qualifies.
pub fn select(
    inventory: &RuntimeInventory,
    required: &RequiredJava,
    rules: &[&dyn JavaCompatibilityRule],
) -> Result<Selection, JavaError> {
    let base = major_rule(required);
    let mut accepted: Vec<&JavaRuntime> = Vec::new();
    let mut rejected: Vec<RejectedRuntime> = Vec::new();

    'candidate: for runtime in &inventory.runtimes {
        for rule in
            std::iter::once(&base as &dyn JavaCompatibilityRule).chain(rules.iter().copied())
        {
            if let RuleVerdict::Reject(rejection) = rule.evaluate(runtime) {
                rejected.push(RejectedRuntime {
                    executable: runtime.executable.clone(),
                    version: runtime.version,
                    rejection,
                });
                continue 'candidate;
            }
        }
        accepted.push(runtime);
    }

    accepted.sort_by(|left, right| {
        right
            .version
            .cmp(&left.version)
            .then_with(|| right.is_managed().cmp(&left.is_managed()))
            .then_with(|| left.executable.cmp(&right.executable))
    });

    match accepted.first() {
        Some(runtime) => {
            debug!(
                path = %runtime.executable.display(),
                version = %runtime.version,
                required = required.major,
                considered = inventory.runtimes.len(),
                "selected a java runtime"
            );
            Ok(Selection {
                runtime: (*runtime).clone(),
                rejected,
            })
        }
        None => Err(JavaError::NoCompatibleRuntime {
            required_major: required.major,
            considered: inventory.runtimes.len(),
            rejections: rejected
                .iter()
                .map(|entry| entry.rejection.clone())
                .collect(),
        }),
    }
}

/// Checks a pinned runtime against the requirement and the loader rules.
///
/// A pin says *which* runtime, not *whether it works*. Skipping the rules here
/// would make an override a way to silently launch something that crashes.
pub fn check_pinned(
    runtime: &JavaRuntime,
    required: &RequiredJava,
    rules: &[&dyn JavaCompatibilityRule],
    source: PinSource,
) -> Result<(), JavaError> {
    let base = major_rule(required);
    for rule in std::iter::once(&base as &dyn JavaCompatibilityRule).chain(rules.iter().copied()) {
        if let RuleVerdict::Reject(rejection) = rule.evaluate(runtime) {
            return Err(JavaError::PinnedRuntimeIncompatible {
                path: runtime.executable.clone(),
                pinned_by: source,
                rejection: Box::new(rejection),
            });
        }
    }
    Ok(())
}

/// A short, untranslated description of a rejection, for logs.
#[must_use]
pub fn describe(reason: &RejectionReason) -> String {
    reason.to_string()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use chrono::Utc;

    use super::*;
    use crate::discovery::{JavaOrigin, JavaSourceKind};
    use crate::requirement::RequirementBasis;
    use crate::rules::{LEGACY_FORGE_MAX_JAVA_8_UPDATE, VersionBound};
    use crate::version::JavaVersion;

    fn runtime(path: &str, version: JavaVersion, kind: JavaSourceKind) -> JavaRuntime {
        JavaRuntime {
            executable: PathBuf::from(path),
            home: None,
            version,
            version_string: version.to_string(),
            runtime_version: version.to_string(),
            vendor: "Ubuntu".to_owned(),
            arch: "amd64".to_owned(),
            origins: vec![JavaOrigin {
                kind,
                path: PathBuf::from(path),
            }],
        }
    }

    /// A representative runtime inventory covering the Java majors used by
    /// supported Minecraft releases.
    fn representative_inventory() -> RuntimeInventory {
        RuntimeInventory {
            runtimes: vec![
                runtime(
                    "/usr/lib/jvm/java-25-openjdk-amd64/bin/java",
                    JavaVersion::new(25, 0, 3),
                    JavaSourceKind::System,
                ),
                runtime(
                    "/usr/lib/jvm/java-21-openjdk-amd64/bin/java",
                    JavaVersion::new(21, 0, 11),
                    JavaSourceKind::System,
                ),
                runtime(
                    "/usr/lib/jvm/java-17-openjdk-amd64/bin/java",
                    JavaVersion::new(17, 0, 19),
                    JavaSourceKind::System,
                ),
                runtime(
                    "/usr/lib/jvm/java-8-openjdk-amd64/jre/bin/java",
                    JavaVersion::new(8, 0, 492),
                    JavaSourceKind::System,
                ),
            ],
            failures: Vec::new(),
            probed: 4,
            candidates: 21,
            scanned_at: Utc::now(),
        }
    }

    fn required(major: u32) -> RequiredJava {
        RequiredJava {
            major,
            basis: RequirementBasis::FallbackTable,
        }
    }

    #[test]
    fn each_minecraft_era_selects_the_runtime_its_metadata_asks_for() {
        // Each supported release era selects the exact Java major its metadata
        // names.
        for (major, expected) in [
            (8, "/usr/lib/jvm/java-8-openjdk-amd64/jre/bin/java"),
            (17, "/usr/lib/jvm/java-17-openjdk-amd64/bin/java"),
            (21, "/usr/lib/jvm/java-21-openjdk-amd64/bin/java"),
            (25, "/usr/lib/jvm/java-25-openjdk-amd64/bin/java"),
        ] {
            let selection = select(&representative_inventory(), &required(major), &[])
                .expect("a runtime exists");
            assert_eq!(
                selection.runtime.executable,
                PathBuf::from(expected),
                "java {major}"
            );
        }
    }

    #[test]
    fn a_missing_major_fails_rather_than_falling_back_to_a_near_miss() {
        // Java 16 is the 1.17 requirement and is not present in this fixture.
        // Handing over Java 17 "because it is close" is the substitution
        // Ephervex does not allow.
        let error =
            select(&representative_inventory(), &required(16), &[]).expect_err("no java 16");
        assert!(
            matches!(
                error,
                JavaError::NoCompatibleRuntime {
                    required_major: 16,
                    ..
                }
            ),
            "{error:?}"
        );
    }

    #[test]
    fn a_loader_rule_can_veto_a_runtime_the_major_check_accepted() {
        // Java 8 is present in this fixture, but 8u492 is too new for Forge
        // 1.12.2.
        let legacy_forge = VersionBound::new("forge-legacy-java8")
            .major(8)
            .at_most(JavaVersion::new(8, 0, LEGACY_FORGE_MAX_JAVA_8_UPDATE));

        let plain = select(&representative_inventory(), &required(8), &[]).expect("java 8 exists");
        assert_eq!(plain.runtime.version.update, 492);

        let error = select(&representative_inventory(), &required(8), &[&legacy_forge])
            .expect_err("8u492 is too new for old forge");
        let JavaError::NoCompatibleRuntime { rejections, .. } = &error else {
            panic!("{error:?}");
        };
        assert!(
            rejections
                .iter()
                .any(|rejection| rejection.rule == "forge-legacy-java8"),
            "the failure has to name the rule that caused it: {rejections:?}"
        );
    }

    #[test]
    fn the_newest_update_of_the_required_major_wins() {
        let mut inventory = representative_inventory();
        inventory.runtimes.push(runtime(
            "/usr/lib/jvm/java-17-old/bin/java",
            JavaVersion::new(17, 0, 2),
            JavaSourceKind::System,
        ));
        let selection = select(&inventory, &required(17), &[]).expect("java 17");
        assert_eq!(selection.runtime.version.update, 19);
    }

    #[test]
    fn a_managed_runtime_wins_a_tie_against_a_system_one() {
        let mut inventory = representative_inventory();
        inventory.runtimes.push(runtime(
            "/home/dev/.local/share/ephervex/java/21/eclipse-jdk-21.0.11+9/bin/java",
            JavaVersion::new(21, 0, 11),
            JavaSourceKind::Managed,
        ));
        let selection = select(&inventory, &required(21), &[]).expect("java 21");
        assert!(
            selection.runtime.is_managed(),
            "at equal version the runtime Ephervex installed is the reproducible one"
        );
    }

    #[test]
    fn rejected_runtimes_are_reported_alongside_the_winner() {
        let selection = select(&representative_inventory(), &required(21), &[]).expect("java 21");
        assert_eq!(
            selection.rejected.len(),
            3,
            "the other three majors were considered and refused"
        );
    }

    // -- override precedence  -----------------------------------------

    #[test]
    fn the_override_layers_apply_in_the_documented_order() {
        let global = PathBuf::from("/opt/global/bin/java");
        let instance = JavaOverride::new("/opt/instance/bin/java");
        let explicit = PathBuf::from("/opt/explicit/bin/java");

        let all_three = JavaPreference {
            explicit: Some(explicit.clone()),
            instance: Some(instance.clone()),
            global: Some(global.clone()),
        };
        assert_eq!(
            all_three.resolve(),
            JavaPathChoice::Pinned {
                path: explicit,
                source: PinSource::ExplicitRequest
            }
        );

        let without_explicit = JavaPreference {
            explicit: None,
            instance: Some(instance),
            global: Some(global.clone()),
        };
        assert_eq!(
            without_explicit.resolve(),
            JavaPathChoice::Pinned {
                path: PathBuf::from("/opt/instance/bin/java"),
                source: PinSource::InstanceOverride
            }
        );

        let global_only = JavaPreference {
            explicit: None,
            instance: None,
            global: Some(global.clone()),
        };
        assert_eq!(
            global_only.resolve(),
            JavaPathChoice::Pinned {
                path: global,
                source: PinSource::GlobalPreference
            }
        );

        assert_eq!(
            JavaPreference::default().resolve(),
            JavaPathChoice::Automatic
        );
    }

    #[test]
    fn a_pinned_runtime_is_still_checked_against_the_rules() {
        // "Use this Java" is not "stop caring whether it works".
        let pinned = runtime(
            "/usr/lib/jvm/java-8-openjdk-amd64/jre/bin/java",
            JavaVersion::new(8, 0, 492),
            JavaSourceKind::UserSupplied,
        );
        let error = check_pinned(&pinned, &required(21), &[], PinSource::InstanceOverride)
            .expect_err("java 8 cannot run a java 21 version");
        assert!(
            matches!(
                error,
                JavaError::PinnedRuntimeIncompatible {
                    pinned_by: PinSource::InstanceOverride,
                    ..
                }
            ),
            "{error:?}"
        );
    }

    #[test]
    fn a_pinned_runtime_that_satisfies_everything_is_accepted() {
        let pinned = runtime(
            "/usr/lib/jvm/java-21-openjdk-amd64/bin/java",
            JavaVersion::new(21, 0, 11),
            JavaSourceKind::UserSupplied,
        );
        check_pinned(&pinned, &required(21), &[], PinSource::ExplicitRequest).expect("compatible");
    }

    // -- jvm settings layering  ---------------------------------------

    fn global_settings() -> GlobalJavaSettings {
        GlobalJavaSettings {
            min_memory_mib: 512,
            max_memory_mib: 4096,
            jvm_args: vec!["-XX:+UseG1GC".to_owned()],
            ..GlobalJavaSettings::default()
        }
    }

    #[test]
    fn no_override_means_every_field_follows_the_global_settings() {
        let effective = EffectiveJvmSettings::resolve(&global_settings(), None);
        assert_eq!(effective.max_memory_mib, 4096);
        assert_eq!(effective.jvm_args, vec!["-XX:+UseG1GC".to_owned()]);
        assert!(!effective.overrides_global);
        assert!(effective.overridden.is_empty());
    }

    #[test]
    fn overridden_to_the_same_value_is_distinguishable_from_using_the_global_one() {
        // This is the distinction the "Use global settings" option checkbox draws,
        // and it is invisible if only the resulting numbers are compared.
        let global = global_settings();
        let pinned_to_the_same_value = JvmSettings {
            max_memory_mib: Some(4096),
            ..JvmSettings::default()
        };

        let following = EffectiveJvmSettings::resolve(&global, None);
        let pinned = EffectiveJvmSettings::resolve(&global, Some(&pinned_to_the_same_value));

        assert_eq!(following.max_memory_mib, pinned.max_memory_mib);
        assert_ne!(
            following, pinned,
            "the two states must not be equal just because the numbers agree"
        );
        assert!(!following.is_overridden(JvmField::MaxMemory));
        assert!(pinned.is_overridden(JvmField::MaxMemory));

        // And the consequence that makes it matter: raising the global maximum
        // moves one and not the other.
        let raised = GlobalJavaSettings {
            max_memory_mib: 8192,
            ..global
        };
        assert_eq!(
            EffectiveJvmSettings::resolve(&raised, None).max_memory_mib,
            8192
        );
        assert_eq!(
            EffectiveJvmSettings::resolve(&raised, Some(&pinned_to_the_same_value)).max_memory_mib,
            4096
        );
    }

    #[test]
    fn an_override_pins_only_the_fields_it_sets() {
        let effective = EffectiveJvmSettings::resolve(
            &global_settings(),
            Some(&JvmSettings {
                min_memory_mib: None,
                max_memory_mib: Some(8192),
                jvm_args: Vec::new(),
            }),
        );
        assert_eq!(effective.min_memory_mib, 512, "unset fields stay global");
        assert_eq!(effective.max_memory_mib, 8192);
        assert_eq!(effective.jvm_args, vec!["-XX:+UseG1GC".to_owned()]);
        assert_eq!(
            effective.overridden.iter().copied().collect::<Vec<_>>(),
            vec![JvmField::MaxMemory]
        );
    }

    #[test]
    fn an_override_that_pins_nothing_still_counts_as_opting_out() {
        let effective =
            EffectiveJvmSettings::resolve(&global_settings(), Some(&JvmSettings::default()));
        assert!(
            effective.overrides_global,
            "the checkbox is off even though no value was changed yet"
        );
        assert!(effective.overridden.is_empty());
    }
}
