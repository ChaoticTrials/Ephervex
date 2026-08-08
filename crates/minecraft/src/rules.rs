//! Mojang's conditional rules, parsed but deliberately **not** evaluated
//!
//! Libraries and arguments in a version JSON carry `rules` arrays that decide
//! whether the entry applies: which operating system, which architecture,
//! which launcher features are active. Ephervex is Linux-only, so it is
//! tempting to evaluate these at parse time and keep only what applies here.
//!
//! That would be wrong twice over. Feature rules (`is_demo_user`,
//! `has_custom_resolution`, the quick-play family) depend on how the *user*
//! launches, which is not known while metadata is being parsed. And a
//! reproduction launcher is expected to explain why an argument is or is not
//! present in a launch it produced — which needs the condition, not just its
//! result. So the conditions survive into Vanilla launch support, which evaluates them against
//! a concrete launch context.
//!
//! # Three shapes of OS-version condition
//!
//! `os.version` changed form between eras and both are still live in the
//! supported range, which is why [`OsVersionCondition`] exists rather than a
//! plain string:
//!
//! | Era | Shape |
//! |---|---|
//! | 1.13 – 1.19.x | `"version": "^10\\."` — a regex against the OS version |
//! | 1.20 – 1.21.x | absent |
//! | 26.x | `"versionRange": { "min": "10.0.17134" }` — an inclusive bound |
//!
//! Neither form is interpreted at parse time. The regex is never compiled (that
//! would drag a regex engine in for a condition that only ever matters on
//! Windows); the range *is* comparable and is compared when a launch is being
//! built. Both are carried verbatim into [`OsVersionCondition`].
//!
//! # Evaluation happens against a launch, not against a document
//!
//! [`RuleContext`] is the concrete answer Vanilla launch support supplies: which platform, and
//! which launcher features the user turned on. [`rules_allow`] applies Mojang's
//! own semantics to it — an empty rule list allows, and otherwise the *last*
//! matching rule decides, which is how `allow` followed by
//! `disallow os=osx` says "everywhere except macOS".

use std::collections::BTreeMap;

use serde::Deserialize;
use tracing::warn;

use crate::platform::Platform;

/// What a matching rule does to the entry that carries it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleAction {
    Allow,
    Disallow,
}

/// An operating system named by Mojang metadata.
///
/// `Other` keeps an unknown name rather than failing: a new platform appearing
/// in a version JSON must not make the whole version unreadable, and on Linux
/// an unknown OS name simply never matches.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum OsName {
    Linux,
    Windows,
    /// macOS, spelled `osx` in every Mojang version JSON.
    Osx,
    Other(String),
}

impl From<String> for OsName {
    fn from(value: String) -> Self {
        match value.as_str() {
            "linux" => Self::Linux,
            "windows" => Self::Windows,
            "osx" => Self::Osx,
            _ => Self::Other(value),
        }
    }
}

impl std::fmt::Display for OsName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.pad(match self {
            Self::Linux => "linux",
            Self::Windows => "windows",
            Self::Osx => "osx",
            Self::Other(name) => name,
        })
    }
}

/// A constraint on the operating system version, in whichever form the version
/// JSON used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OsVersionCondition {
    /// A regular expression matched against the OS version string, e.g.
    /// `^10\.` (1.13 through 1.19.x).
    Pattern(String),
    /// An inclusive range, introduced with the 26.x releases. Either bound may
    /// be absent.
    Range {
        min: Option<String>,
        max: Option<String>,
    },
}

/// The OS half of a rule condition.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OsCondition {
    pub name: Option<OsName>,
    /// `x86`, `x86_64`, `arm64`. Kept as text because Mojang's spelling is not
    /// Rust's `target_arch` spelling and the translation belongs where the
    /// rule is evaluated.
    pub arch: Option<String>,
    pub version: Option<OsVersionCondition>,
}

impl OsVersionCondition {
    /// Whether an OS version satisfies this condition.
    ///
    /// `None` means "this build cannot decide", which is the honest answer for
    /// [`Self::Pattern`]: it is a regular expression, and compiling one would
    /// add a regex engine to the dependency graph for a condition that has only
    /// ever gated Windows-specific JVM flags. A caller treats an undecidable
    /// condition as not matching and says so in the log, rather than guessing
    #[must_use]
    pub fn matches(&self, version: &str) -> Option<bool> {
        match self {
            Self::Pattern(_) => None,
            Self::Range { min, max } => Some(
                min.as_ref()
                    .is_none_or(|min| compare_versions(version, min).is_ge())
                    && max
                        .as_ref()
                        .is_none_or(|max| compare_versions(version, max).is_le()),
            ),
        }
    }
}

/// Orders two dotted numeric OS versions, e.g. `10.0.17134` against `10.0.19045`.
///
/// Components that are not numbers compare as text, and a missing component
/// counts as zero, so `10.0` and `10.0.0` are equal. That is enough for the
/// only shape Mojang has published and does not pretend to be a general version
/// algebra — Minecraft's own ids get [`crate::MinecraftVersionId`], which is a
/// separate problem with separate rules.
fn compare_versions(left: &str, right: &str) -> std::cmp::Ordering {
    let mut left = left.split('.');
    let mut right = right.split('.');
    loop {
        let (a, b) = (left.next(), right.next());
        if a.is_none() && b.is_none() {
            return std::cmp::Ordering::Equal;
        }
        let (a, b) = (a.unwrap_or("0"), b.unwrap_or("0"));
        let ordering = match (a.parse::<u64>(), b.parse::<u64>()) {
            (Ok(a), Ok(b)) => a.cmp(&b),
            _ => a.cmp(b),
        };
        if ordering != std::cmp::Ordering::Equal {
            return ordering;
        }
    }
}

impl OsCondition {
    /// Whether this condition constrains anything at all.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.name.is_none() && self.arch.is_none() && self.version.is_none()
    }

    /// Whether a platform satisfies every part of this condition.
    #[must_use]
    pub fn matches(&self, platform: &Platform) -> bool {
        if self.name.as_ref().is_some_and(|name| *name != platform.os) {
            return false;
        }
        if self
            .arch
            .as_ref()
            .is_some_and(|arch| *arch != platform.arch)
        {
            return false;
        }
        match self.version.as_ref() {
            None => true,
            Some(condition) => condition.matches(&platform.os_version).unwrap_or_else(|| {
                // Unreachable on Linux: every `os.version` regex Mojang has
                // published also names Windows, so the name check above has
                // already refused. Logged rather than assumed, because "this
                // cannot happen" is how a condition ends up silently ignored.
                warn!(
                    ?condition,
                    platform = %platform,
                    "cannot evaluate an os.version condition; treating the rule as not matching"
                );
                false
            }),
        }
    }
}

/// The concrete launch a rule is evaluated against.
///
/// Features are a map rather than a struct for the same reason [`Rule`]'s are:
/// Mojang adds feature flags without warning — the quick-play family arrived in
/// 1.20 — and a struct would turn each addition into a code change in a place
/// that has no opinion about what the flag means.
#[derive(Debug, Clone, Default)]
pub struct RuleContext {
    pub platform: Platform,
    /// Launcher features that are *on*. A flag absent here is off, which is the
    /// same thing Mojang's own launcher means by omitting it.
    pub features: BTreeMap<String, bool>,
}

impl RuleContext {
    /// A context for one platform with no features enabled.
    #[must_use]
    pub fn new(platform: Platform) -> Self {
        Self {
            platform,
            features: BTreeMap::new(),
        }
    }

    /// Turns a launcher feature on or off.
    #[must_use]
    pub fn with_feature(mut self, name: impl Into<String>, enabled: bool) -> Self {
        self.features.insert(name.into(), enabled);
        self
    }

    /// Whether a feature is on.
    #[must_use]
    pub fn feature(&self, name: &str) -> bool {
        self.features.get(name).copied().unwrap_or(false)
    }
}

impl Rule {
    /// Whether this rule's conditions hold for a launch.
    ///
    /// A rule with neither `os` nor `features` matches everything, which is how
    /// Mojang writes the `allow` half of an allow/disallow pair.
    #[must_use]
    pub fn matches(&self, context: &RuleContext) -> bool {
        if !self
            .features
            .iter()
            .all(|(name, expected)| context.feature(name) == *expected)
        {
            return false;
        }
        self.os
            .as_ref()
            .is_none_or(|condition| condition.matches(&context.platform))
    }
}

/// Whether a rule-gated entry applies to a launch.
///
/// Mojang's semantics, and they are not "any allow wins":
///
/// * No rules at all means allowed. Most libraries carry none.
/// * Otherwise the entry starts disallowed and every *matching* rule sets the
///   verdict, so the last one that matches decides. `[allow, disallow os=osx]`
///   therefore allows everywhere except macOS, and `[allow os=linux]` allows on
///   Linux alone.
#[must_use]
pub fn rules_allow(rules: &[Rule], context: &RuleContext) -> bool {
    if rules.is_empty() {
        return true;
    }
    let mut allowed = false;
    for rule in rules {
        if rule.matches(context) {
            allowed = rule.action == RuleAction::Allow;
        }
    }
    allowed
}

/// One `rules` entry.
///
/// A rule with no `os` and no `features` is unconditional, which is how
/// Mojang writes "allow by default, then disallow on macOS" as two rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pub action: RuleAction,
    pub os: Option<OsCondition>,
    /// Launcher features the rule depends on, e.g. `is_demo_user`,
    /// `has_custom_resolution`, `is_quick_play_multiplayer`.
    ///
    /// Kept as a string-keyed map rather than an enum: Mojang adds feature
    /// flags without warning (the quick-play family arrived in 1.20), and an
    /// exhaustive enum would turn each addition into a parse failure for a
    /// version that is otherwise perfectly readable.
    pub features: BTreeMap<String, bool>,
}

impl Rule {
    /// Whether the rule applies unconditionally.
    #[must_use]
    pub fn is_unconditional(&self) -> bool {
        self.features.is_empty() && self.os.as_ref().is_none_or(OsCondition::is_empty)
    }

    /// Every feature flag named anywhere in a set of rules.
    ///
    /// Vanilla launch support needs this to know which launch-context flags it has to be able
    /// to answer for.
    pub fn feature_names<'a>(rules: impl IntoIterator<Item = &'a Self>) -> Vec<&'a str> {
        let mut names: Vec<&str> = rules
            .into_iter()
            .flat_map(|rule| rule.features.keys().map(String::as_str))
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    }
}

/// The wire shape of a rule. Kept separate from [`Rule`] so the parsed type can
/// have a total `features` map instead of an `Option`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawRule {
    action: RuleAction,
    #[serde(default)]
    os: Option<RawOsCondition>,
    #[serde(default)]
    features: BTreeMap<String, bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawOsCondition {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arch: Option<String>,
    /// 1.13 – 1.19.x: a regex.
    #[serde(default)]
    version: Option<String>,
    /// 26.x onward: an inclusive range.
    #[serde(default)]
    version_range: Option<RawVersionRange>,
}

#[derive(Debug, Deserialize)]
struct RawVersionRange {
    #[serde(default)]
    min: Option<String>,
    #[serde(default)]
    max: Option<String>,
}

impl From<RawRule> for Rule {
    fn from(raw: RawRule) -> Self {
        Self {
            action: raw.action,
            os: raw.os.map(Into::into),
            features: raw.features,
        }
    }
}

impl From<RawOsCondition> for OsCondition {
    fn from(raw: RawOsCondition) -> Self {
        // Both spellings have never appeared together, and if they ever do the
        // newer one wins: it is the form the release that emitted it uses.
        let version = raw.version_range.map_or_else(
            || raw.version.map(OsVersionCondition::Pattern),
            |range| {
                Some(OsVersionCondition::Range {
                    min: range.min,
                    max: range.max,
                })
            },
        );
        Self {
            name: raw.name.map(Into::into),
            arch: raw.arch,
            version,
        }
    }
}

pub(crate) fn parse_rules(raw: Vec<RawRule>) -> Vec<Rule> {
    raw.into_iter().map(Into::into).collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn parse(json: &str) -> Rule {
        let raw: RawRule = serde_json::from_str(json).expect("well-formed rule");
        raw.into()
    }

    #[test]
    fn an_unconditional_allow_parses_as_unconditional() {
        let rule = parse(r#"{"action":"allow"}"#);
        assert_eq!(rule.action, RuleAction::Allow);
        assert!(rule.is_unconditional());
    }

    #[test]
    fn a_disallow_on_one_os_keeps_its_condition() {
        let rule = parse(r#"{"action":"disallow","os":{"name":"osx"}}"#);
        assert_eq!(rule.action, RuleAction::Disallow);
        assert!(!rule.is_unconditional());
        assert_eq!(
            rule.os.expect("os condition").name,
            Some(OsName::Osx),
            "the condition must survive parsing, not be evaluated away"
        );
    }

    #[test]
    fn an_architecture_rule_keeps_mojangs_spelling() {
        let rule = parse(r#"{"action":"allow","os":{"arch":"x86"}}"#);
        assert_eq!(rule.os.expect("os condition").arch.as_deref(), Some("x86"));
    }

    #[test]
    fn the_pre_1_20_os_version_regex_is_carried_without_being_compiled() {
        let rule = parse(r#"{"action":"allow","os":{"name":"windows","version":"^10\\."}}"#);
        assert_eq!(
            rule.os.expect("os condition").version,
            Some(OsVersionCondition::Pattern("^10\\.".to_owned()))
        );
    }

    #[test]
    fn the_26_x_os_version_range_parses_as_a_range() {
        let rule = parse(
            r#"{"action":"allow","os":{"name":"windows","versionRange":{"min":"10.0.17134"}}}"#,
        );
        assert_eq!(
            rule.os.expect("os condition").version,
            Some(OsVersionCondition::Range {
                min: Some("10.0.17134".to_owned()),
                max: None,
            })
        );
    }

    #[test]
    fn feature_rules_survive_including_ones_this_build_has_never_heard_of() {
        let rule = parse(
            r#"{"action":"allow","features":{"is_quick_play_realms":true,"some_future_flag":false}}"#,
        );
        assert_eq!(rule.features.get("is_quick_play_realms"), Some(&true));
        assert_eq!(rule.features.get("some_future_flag"), Some(&false));
        assert_eq!(
            Rule::feature_names([&rule]),
            ["is_quick_play_realms", "some_future_flag"]
        );
    }

    #[test]
    fn an_unknown_operating_system_is_kept_rather_than_rejected() {
        let rule = parse(r#"{"action":"allow","os":{"name":"haiku"}}"#);
        assert_eq!(
            rule.os.expect("os condition").name,
            Some(OsName::Other("haiku".to_owned()))
        );
    }

    // ------------------------------------------------------------ evaluation

    fn rules(json: &str) -> Vec<Rule> {
        let raw: Vec<RawRule> = serde_json::from_str(json).expect("well-formed rules");
        parse_rules(raw)
    }

    fn linux() -> RuleContext {
        RuleContext::new(Platform::new(OsName::Linux, "x86_64", "6.8.0"))
    }

    fn windows(version: &str) -> RuleContext {
        RuleContext::new(Platform::new(OsName::Windows, "x86_64", version))
    }

    #[test]
    fn an_entry_with_no_rules_applies_everywhere() {
        assert!(rules_allow(&[], &linux()));
        assert!(rules_allow(&[], &windows("10.0.19045")));
    }

    #[test]
    fn allow_then_disallow_means_everywhere_except_the_named_platform() {
        // The pair form 1.12.2 uses for its LWJGL natives.
        let pair = rules(r#"[{"action":"allow"},{"action":"disallow","os":{"name":"osx"}}]"#);
        assert!(rules_allow(&pair, &linux()));
        assert!(rules_allow(&pair, &windows("10.0.19045")));
        assert!(!rules_allow(
            &pair,
            &RuleContext::new(Platform::new(OsName::Osx, "arm64", "14.5"))
        ));
    }

    #[test]
    fn a_single_allow_for_one_os_excludes_every_other_one() {
        // The form 1.19-and-later natives use: one rule, and everything not
        // named by it is refused. Getting this backwards is how a Linux launch
        // ends up with the Windows natives on its classpath.
        let only_linux = rules(r#"[{"action":"allow","os":{"name":"linux"}}]"#);
        assert!(rules_allow(&only_linux, &linux()));
        assert!(!rules_allow(&only_linux, &windows("10.0.19045")));
    }

    #[test]
    fn an_architecture_rule_is_compared_against_mojangs_spelling() {
        let x86_only = rules(r#"[{"action":"allow","os":{"arch":"x86"}}]"#);
        assert!(!rules_allow(&x86_only, &linux()));
        assert!(rules_allow(
            &x86_only,
            &RuleContext::new(Platform::new(OsName::Linux, "x86", "6.8.0"))
        ));
    }

    #[test]
    fn the_26_x_version_range_is_compared_rather_than_carried() {
        let modern = rules(
            r#"[{"action":"allow","os":{"name":"windows","versionRange":{"min":"10.0.17134"}}}]"#,
        );
        assert!(rules_allow(&modern, &windows("10.0.19045")));
        assert!(rules_allow(&modern, &windows("10.0.17134")), "inclusive");
        assert!(!rules_allow(&modern, &windows("6.1.7601")));

        let ancient = rules(
            r#"[{"action":"allow","os":{"name":"windows","versionRange":{"max":"10.0.17134"}}}]"#,
        );
        assert!(
            ancient
                .first()
                .is_some_and(|rule| rule.matches(&windows("6.1.7601")))
        );
        assert!(!rules_allow(&ancient, &windows("10.0.19045")));
    }

    #[test]
    fn the_pre_1_20_regex_spelling_is_undecidable_and_never_silently_treated_as_a_match() {
        // Compiling it would add a regex engine for a condition that has only
        // ever gated a Windows JVM flag; guessing at it would be worse.
        let condition = OsVersionCondition::Pattern("^10\\.".to_owned());
        assert_eq!(condition.matches("10.0.19045"), None);

        let regex_rule =
            rules(r#"[{"action":"allow","os":{"name":"windows","version":"^10\\."}}]"#);
        assert!(
            !rules_allow(&regex_rule, &windows("10.0.19045")),
            "an undecidable condition must not be assumed to hold"
        );
        assert!(
            !rules_allow(&regex_rule, &linux()),
            "and the os name already refuses it on the platform Ephervex targets"
        );
    }

    #[test]
    fn a_feature_rule_follows_the_launch_and_not_the_platform() {
        let demo = rules(r#"[{"action":"allow","features":{"is_demo_user":true}}]"#);
        assert!(!rules_allow(&demo, &linux()));
        assert!(rules_allow(
            &demo,
            &linux().with_feature("is_demo_user", true)
        ));
        assert!(!rules_allow(
            &demo,
            &linux().with_feature("is_demo_user", false)
        ));
    }

    #[test]
    fn a_feature_rule_asking_for_a_flag_to_be_off_is_honored_as_written() {
        let rule = rules(r#"[{"action":"allow","features":{"is_demo_user":false}}]"#);
        assert!(rules_allow(&rule, &linux()), "absent means off");
        assert!(!rules_allow(
            &rule,
            &linux().with_feature("is_demo_user", true)
        ));
    }

    #[test]
    fn every_condition_of_one_rule_has_to_hold_at_once() {
        let both = rules(
            r#"[{"action":"allow","os":{"name":"linux","arch":"x86_64"},"features":{"has_custom_resolution":true}}]"#,
        );
        assert!(!rules_allow(&both, &linux()));
        assert!(rules_allow(
            &both,
            &linux().with_feature("has_custom_resolution", true)
        ));
        assert!(!rules_allow(
            &both,
            &windows("10.0.19045").with_feature("has_custom_resolution", true)
        ));
    }

    #[test]
    fn dotted_os_versions_compare_numerically_rather_than_as_text() {
        // `10.0.9` is *older* than `10.0.17134`; a string comparison says the
        // opposite and would enable a GC flag on an OS that cannot run it.
        assert_eq!(
            compare_versions("10.0.9", "10.0.17134"),
            std::cmp::Ordering::Less
        );
        assert_eq!(
            compare_versions("10.0", "10.0.0"),
            std::cmp::Ordering::Equal
        );
        assert_eq!(
            compare_versions("11.0.0", "10.9.9"),
            std::cmp::Ordering::Greater
        );
    }
}
