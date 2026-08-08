//! Launch arguments, in either of the forms Mojang has published
//!
//! # Two forms, one representation
//!
//! 1.12.2 — and everything before it — carries a single
//! `"minecraftArguments"` **string**: a whitespace-separated template with no
//! conditions and no JVM half at all.
//!
//! ```text
//! "--username ${auth_player_name} --version ${version_name} …"
//! ```
//!
//! 1.13 onward carries an `"arguments"` **object** whose members are arrays
//! mixing plain strings with `{ "rules": [...], "value": … }` objects, where
//! `value` is either one string or an array of them.
//!
//! Both become [`Arguments`]: a set of named groups, each a list of
//! [`ArgumentEntry`] carrying its values and its (possibly empty) rules. The
//! form itself is preserved in [`Arguments::form`], because the difference is
//! not merely cosmetic — see the legacy note below.
//!
//! # Groups are open-ended on purpose
//!
//! The 26.x releases introduced a third group, `default-user-jvm`, holding
//! Mojang's own recommended heap and GC flags. the project's 2026 snapshot does not
//! mention it, and an enum closed over `game` and `jvm` would have silently
//! dropped it — which is exactly the kind of quiet approximation Ephervex does not allow.
//! [`ArgumentGroup::Other`] means the next such addition arrives as data rather
//! than as a parse failure or a hole.
//!
//! # The legacy form carries no JVM arguments
//!
//! A pre-1.13 version JSON says nothing about `-cp`, `-Djava.library.path`, or
//! the log4j configuration flag; a launcher is expected to know them. Supplying
//! that default set is a *launch* concern and belongs to Vanilla launch support, not to
//! parsing, so [`Arguments::group`] simply reports no `jvm` group for 1.12.2.
//! [`ArgumentForm::Legacy`] is how a caller knows the absence means "you must
//! supply these" rather than "this version needs none".

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::rules::{RawRule, Rule, parse_rules};

/// Which of Mojang's two argument encodings a version used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArgumentForm {
    /// `minecraftArguments`, a flat string. Up to and including 1.12.2.
    Legacy,
    /// The `arguments` object with rules. 1.13 onward.
    Modern,
}

/// A named argument group.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ArgumentGroup {
    /// Arguments passed to the game's main class.
    Game,
    /// Arguments passed to the JVM.
    Jvm,
    /// JVM arguments Mojang recommends as a *user-editable default*, new in the
    /// 26.x releases. Distinct from [`Self::Jvm`]: these are a starting point a
    /// user's own settings replace, not flags the game requires
    DefaultUserJvm,
    /// A group this build has not been taught. Kept so it can be logged and
    /// surfaced rather than silently lost.
    Other(String),
}

impl ArgumentGroup {
    /// The key as it appears in a version JSON.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Game => "game",
            Self::Jvm => "jvm",
            Self::DefaultUserJvm => "default-user-jvm",
            Self::Other(name) => name,
        }
    }

    /// Whether this group is one this build understands.
    #[must_use]
    pub const fn is_recognized(&self) -> bool {
        !matches!(self, Self::Other(_))
    }
}

impl From<&str> for ArgumentGroup {
    fn from(value: &str) -> Self {
        match value {
            "game" => Self::Game,
            "jvm" => Self::Jvm,
            "default-user-jvm" => Self::DefaultUserJvm,
            other => Self::Other(other.to_owned()),
        }
    }
}

impl std::fmt::Display for ArgumentGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `pad` rather than `write_str`, so a caller aligning a column with
        // `{group:<17}` gets what it asked for.
        f.pad(self.as_str())
    }
}

/// One entry of an argument group: the tokens it contributes, and the
/// conditions under which it contributes them.
///
/// The values are templates — `${auth_player_name}`, `${classpath}` — and are
/// left unsubstituted. Substitution needs an account, a classpath, and a game
/// directory, none of which exist while metadata is being read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArgumentEntry {
    pub values: Vec<String>,
    /// Empty for an unconditional entry. Never evaluated here; see
    /// [`crate::rules`].
    pub rules: Vec<Rule>,
}

impl ArgumentEntry {
    /// Whether this entry applies regardless of platform and launch options.
    #[must_use]
    pub fn is_unconditional(&self) -> bool {
        self.rules.is_empty()
    }
}

/// Every argument a version declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arguments {
    form: ArgumentForm,
    groups: BTreeMap<ArgumentGroup, Vec<ArgumentEntry>>,
}

impl Arguments {
    /// Which encoding the version used.
    #[must_use]
    pub const fn form(&self) -> ArgumentForm {
        self.form
    }

    /// The entries of one group, or an empty slice when the version declares
    /// none.
    #[must_use]
    pub fn group(&self, group: &ArgumentGroup) -> &[ArgumentEntry] {
        self.groups.get(group).map_or(&[], Vec::as_slice)
    }

    /// Every group the version declared, in a stable order.
    pub fn groups(&self) -> impl Iterator<Item = (&ArgumentGroup, &[ArgumentEntry])> {
        self.groups
            .iter()
            .map(|(group, entries)| (group, entries.as_slice()))
    }

    /// Groups this build does not recognize, so a caller can report them
    /// instead of quietly ignoring them.
    pub fn unrecognized_groups(&self) -> impl Iterator<Item = &ArgumentGroup> {
        self.groups.keys().filter(|group| !group.is_recognized())
    }

    /// Builds the legacy form from a `minecraftArguments` string.
    ///
    /// Splitting on ASCII whitespace matches how the value is consumed: it is a
    /// pre-tokenized command tail, not a shell line. No Mojang release in the
    /// supported range puts a quoted value in it, and treating it as shell
    /// syntax would invent quoting rules nothing on the other side implements.
    #[must_use]
    pub fn from_legacy(arguments: &str) -> Self {
        let values: Vec<String> = arguments
            .split_ascii_whitespace()
            .map(str::to_owned)
            .collect();
        let mut groups = BTreeMap::new();
        if !values.is_empty() {
            groups.insert(
                ArgumentGroup::Game,
                vec![ArgumentEntry {
                    values,
                    rules: Vec::new(),
                }],
            );
        }
        Self {
            form: ArgumentForm::Legacy,
            groups,
        }
    }

    /// Builds the modern form from the parsed `arguments` object.
    pub(crate) fn from_modern(raw: BTreeMap<String, Vec<RawArgument>>) -> Self {
        let groups = raw
            .into_iter()
            .map(|(name, entries)| {
                (
                    ArgumentGroup::from(name.as_str()),
                    entries.into_iter().map(Into::into).collect(),
                )
            })
            .collect();
        Self {
            form: ArgumentForm::Modern,
            groups,
        }
    }

    /// Merges a loader child onto its Minecraft parent's arguments.
    ///
    /// Legacy loader metadata publishes a complete replacement game string.
    /// Modern metadata instead extends every named group, with the parent's
    /// entries first and the child's entries second.
    #[must_use]
    pub fn inherit(parent: &Self, child: &Self) -> Self {
        if child.form == ArgumentForm::Legacy {
            return child.clone();
        }

        let mut groups = parent.groups.clone();
        for (group, entries) in &child.groups {
            groups
                .entry(group.clone())
                .or_default()
                .extend(entries.iter().cloned());
        }
        Self {
            form: ArgumentForm::Modern,
            groups,
        }
    }
}

/// The wire shape of one entry: either a bare string or a conditional object.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum RawArgument {
    Plain(String),
    Conditional {
        #[serde(default)]
        rules: Vec<RawRule>,
        value: RawArgumentValue,
    },
}

/// `value` is one string or an array of them, interchangeably, in the same
/// array of the same version JSON.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum RawArgumentValue {
    One(String),
    Many(Vec<String>),
}

impl From<RawArgument> for ArgumentEntry {
    fn from(raw: RawArgument) -> Self {
        match raw {
            RawArgument::Plain(value) => Self {
                values: vec![value],
                rules: Vec::new(),
            },
            RawArgument::Conditional { rules, value } => Self {
                values: match value {
                    RawArgumentValue::One(value) => vec![value],
                    RawArgumentValue::Many(values) => values,
                },
                rules: parse_rules(rules),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn modern(json: &str) -> Arguments {
        let raw: BTreeMap<String, Vec<RawArgument>> =
            serde_json::from_str(json).expect("well-formed arguments object");
        Arguments::from_modern(raw)
    }

    #[test]
    fn the_legacy_string_becomes_one_unconditional_game_entry() {
        let arguments =
            Arguments::from_legacy("--username ${auth_player_name} --version ${version_name}");
        assert_eq!(arguments.form(), ArgumentForm::Legacy);

        let game = arguments.group(&ArgumentGroup::Game);
        assert_eq!(game.len(), 1);
        assert!(game[0].is_unconditional());
        assert_eq!(
            game[0].values,
            [
                "--username",
                "${auth_player_name}",
                "--version",
                "${version_name}"
            ]
        );
    }

    #[test]
    fn the_legacy_form_declares_no_jvm_arguments_at_all() {
        let arguments = Arguments::from_legacy("--username ${auth_player_name}");
        assert!(
            arguments.group(&ArgumentGroup::Jvm).is_empty(),
            "1.12.2 supplies none; Vanilla launch support must know to add its own"
        );
        assert_eq!(arguments.form(), ArgumentForm::Legacy);
    }

    #[test]
    fn a_placeholder_is_never_substituted_while_parsing() {
        let arguments = Arguments::from_legacy("--accessToken ${auth_access_token}");
        assert_eq!(
            arguments.group(&ArgumentGroup::Game)[0].values[1],
            "${auth_access_token}"
        );
    }

    #[test]
    fn a_plain_string_and_a_conditional_object_coexist_in_one_group() {
        let arguments = modern(
            r#"{"game":["--username","${auth_player_name}",{"rules":[{"action":"allow","features":{"is_demo_user":true}}],"value":"--demo"}]}"#,
        );
        let game = arguments.group(&ArgumentGroup::Game);
        assert_eq!(game.len(), 3);
        assert!(game[0].is_unconditional());
        assert_eq!(game[2].values, ["--demo"]);
        assert_eq!(game[2].rules.len(), 1);
        assert_eq!(
            game[2].rules[0].features.get("is_demo_user"),
            Some(&true),
            "the feature condition must reach Vanilla launch support, not be resolved here"
        );
    }

    #[test]
    fn a_conditional_value_may_be_a_single_string_or_a_list() {
        let arguments = modern(
            r#"{"game":[{"rules":[{"action":"allow","features":{"has_custom_resolution":true}}],"value":["--width","${resolution_width}"]}],"jvm":[{"rules":[{"action":"allow","os":{"arch":"x86"}}],"value":"-Xss1M"}]}"#,
        );
        assert_eq!(
            arguments.group(&ArgumentGroup::Game)[0].values,
            ["--width", "${resolution_width}"]
        );
        assert_eq!(arguments.group(&ArgumentGroup::Jvm)[0].values, ["-Xss1M"]);
    }

    #[test]
    fn the_26_x_default_user_jvm_group_is_kept_as_its_own_group() {
        let arguments = modern(
            r#"{"jvm":["-Djava.library.path=${natives_directory}"],"default-user-jvm":[{"value":["-Xms2G","-Xmx4G"]},{"rules":[{"action":"allow","os":{"name":"linux"}}],"value":["-XX:+UseZGC"]}]}"#,
        );

        let defaults = arguments.group(&ArgumentGroup::DefaultUserJvm);
        assert_eq!(defaults.len(), 2);
        assert_eq!(defaults[0].values, ["-Xms2G", "-Xmx4G"]);
        assert!(
            defaults[0].is_unconditional(),
            "an object entry with no rules array is still unconditional"
        );
        assert_eq!(defaults[1].values, ["-XX:+UseZGC"]);
        assert!(
            arguments
                .group(&ArgumentGroup::Jvm)
                .iter()
                .all(ArgumentEntry::is_unconditional),
            "the recommended defaults must not be mixed into the required jvm flags"
        );
    }

    #[test]
    fn an_argument_group_from_the_future_is_reported_rather_than_dropped() {
        let arguments = modern(r#"{"game":["--x"],"holodeck":["--y"]}"#);
        let unknown: Vec<_> = arguments
            .unrecognized_groups()
            .map(ToString::to_string)
            .collect();
        assert_eq!(unknown, ["holodeck"]);
        assert_eq!(
            arguments.group(&ArgumentGroup::Other("holodeck".to_owned()))[0].values,
            ["--y"]
        );
    }
}
