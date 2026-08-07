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
//! Neither form is interpreted here. The regex is not compiled (that would drag
//! a regex engine in for a condition that only ever matters on Windows), and
//! the range is not compared; both are carried verbatim.

use std::collections::BTreeMap;

use serde::Deserialize;

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

impl OsCondition {
    /// Whether this condition constrains anything at all.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.name.is_none() && self.arch.is_none() && self.version.is_none()
    }
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
}
