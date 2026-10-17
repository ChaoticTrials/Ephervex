//! The `server.properties` keys Ephervex edits through typed controls.
//!
//! Each key's meaning, accepted values and default follow the vanilla dedicated
//! server. Keys outside this catalog are never interpreted and keep exactly what
//! the file holds. The typed network keys and `eula` have settings of their own
//! and are not listed here. Remote administration (RCON, query, JMX and the
//! management server) is outside what Ephervex configures.

use crate::{ServerError, ServerResult};

/// Where a key is shown, in the order the groups are listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PropertyGroup {
    World,
    Gameplay,
    Players,
    Performance,
    Advanced,
}

/// What a key's value is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropertyValueKind {
    /// `true` or `false`.
    Toggle,
    /// A whole number within the bounds the server reads it with: the Java
    /// type it is read as, or the range a server clamps it to, outside of
    /// which no value has an effect of its own.
    Integer { minimum: i64, maximum: i64 },
    /// One of the names, or its position: servers before Minecraft 1.14 read
    /// these keys only as numbers.
    Choice(&'static [&'static str]),
    /// Any single-line text, stored as written.
    Text,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PropertyDefinition {
    pub key: &'static str,
    pub group: PropertyGroup,
    pub kind: PropertyValueKind,
    /// What a current vanilla server writes when the file does not hold the
    /// key. Shown, never written.
    pub vanilla_default: &'static str,
}

/// The largest value a Java `int` holds, which most numeric keys are read as.
const INT_MAX: i64 = 2_147_483_647;
const INT_MIN: i64 = -2_147_483_648;

const fn toggle(key: &'static str, group: PropertyGroup, default: bool) -> PropertyDefinition {
    PropertyDefinition {
        key,
        group,
        kind: PropertyValueKind::Toggle,
        vanilla_default: if default { "true" } else { "false" },
    }
}

const fn integer(
    key: &'static str,
    group: PropertyGroup,
    (minimum, maximum): (i64, i64),
    vanilla_default: &'static str,
) -> PropertyDefinition {
    PropertyDefinition {
        key,
        group,
        kind: PropertyValueKind::Integer { minimum, maximum },
        vanilla_default,
    }
}

const fn text(
    key: &'static str,
    group: PropertyGroup,
    vanilla_default: &'static str,
) -> PropertyDefinition {
    PropertyDefinition {
        key,
        group,
        kind: PropertyValueKind::Text,
        vanilla_default,
    }
}

pub const DIFFICULTIES: &[&str] = &["peaceful", "easy", "normal", "hard"];
pub const GAME_MODES: &[&str] = &["survival", "creative", "adventure", "spectator"];

/// Every catalog key, grouped and in the order each group lists them.
pub const SERVER_PROPERTY_CATALOG: &[PropertyDefinition] = {
    use PropertyGroup::{Advanced, Gameplay, Performance, Players, World};
    &[
        text("level-name", World, "world"),
        text("level-seed", World, ""),
        text("level-type", World, "minecraft:normal"),
        toggle("generate-structures", World, true),
        toggle("allow-nether", World, true),
        toggle("hardcore", World, false),
        integer("spawn-protection", World, (INT_MIN, INT_MAX), "16"),
        integer("max-world-size", World, (1, 29_999_984), "29999984"),
        PropertyDefinition {
            key: "gamemode",
            group: Gameplay,
            kind: PropertyValueKind::Choice(GAME_MODES),
            vanilla_default: "survival",
        },
        toggle("force-gamemode", Gameplay, false),
        PropertyDefinition {
            key: "difficulty",
            group: Gameplay,
            kind: PropertyValueKind::Choice(DIFFICULTIES),
            vanilla_default: "easy",
        },
        toggle("pvp", Gameplay, true),
        toggle("allow-flight", Gameplay, false),
        toggle("enable-command-block", Gameplay, false),
        toggle("spawn-monsters", Gameplay, true),
        toggle("spawn-animals", Gameplay, true),
        toggle("spawn-npcs", Gameplay, true),
        text("motd", Players, "A Minecraft Server"),
        integer("max-players", Players, (INT_MIN, INT_MAX), "20"),
        toggle("white-list", Players, false),
        toggle("enforce-whitelist", Players, false),
        integer("player-idle-timeout", Players, (INT_MIN, INT_MAX), "0"),
        integer("op-permission-level", Players, (INT_MIN, INT_MAX), "4"),
        integer(
            "function-permission-level",
            Players,
            (INT_MIN, INT_MAX),
            "2",
        ),
        integer("view-distance", Performance, (2, 32), "10"),
        integer("simulation-distance", Performance, (2, 32), "10"),
        integer("max-tick-time", Performance, (i64::MIN, i64::MAX), "60000"),
        integer(
            "pause-when-empty-seconds",
            Performance,
            (INT_MIN, INT_MAX),
            "60",
        ),
        integer(
            "entity-broadcast-range-percentage",
            Performance,
            (10, 1000),
            "100",
        ),
        toggle("sync-chunk-writes", Performance, true),
        text("generator-settings", Advanced, "{}"),
        text("initial-enabled-packs", Advanced, "vanilla"),
        text("initial-disabled-packs", Advanced, ""),
        toggle("enable-status", Advanced, true),
        toggle("hide-online-players", Advanced, false),
        toggle("enforce-secure-profile", Advanced, true),
        toggle("accepts-transfers", Advanced, false),
        toggle("broadcast-console-to-ops", Advanced, true),
        toggle("log-ips", Advanced, true),
        integer(
            "network-compression-threshold",
            Advanced,
            (INT_MIN, INT_MAX),
            "256",
        ),
        integer("rate-limit", Advanced, (INT_MIN, INT_MAX), "0"),
        integer(
            "max-chained-neighbor-updates",
            Advanced,
            (INT_MIN, INT_MAX),
            "1000000",
        ),
    ]
};

#[must_use]
pub fn property_definition(key: &str) -> Option<&'static PropertyDefinition> {
    SERVER_PROPERTY_CATALOG
        .iter()
        .find(|definition| definition.key == key)
}

/// Why a value does not fit its key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropertyValueProblem {
    NotToggle,
    NotWholeNumber,
    OutOfRange { minimum: i64, maximum: i64 },
    UnknownChoice,
}

impl PropertyValueProblem {
    const fn reason(self) -> &'static str {
        match self {
            Self::NotToggle => "the value must be true or false",
            Self::NotWholeNumber => "the value must be a whole number",
            Self::OutOfRange { .. } => "the value is outside the range the server accepts",
            Self::UnknownChoice => "the value is not one the server accepts",
        }
    }
}

impl PropertyValueKind {
    /// Whether `value` is one this kind of key accepts.
    pub fn check(self, value: &str) -> Result<(), PropertyValueProblem> {
        match self {
            Self::Toggle => {
                if value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("false") {
                    Ok(())
                } else {
                    Err(PropertyValueProblem::NotToggle)
                }
            }
            Self::Integer { minimum, maximum } => {
                let number = value
                    .parse::<i64>()
                    .map_err(|_| PropertyValueProblem::NotWholeNumber)?;
                if (minimum..=maximum).contains(&number) {
                    Ok(())
                } else {
                    Err(PropertyValueProblem::OutOfRange { minimum, maximum })
                }
            }
            Self::Choice(names) => {
                let position = value.parse::<usize>().ok().filter(|_| {
                    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
                });
                if names.contains(&value) || position.is_some_and(|position| position < names.len())
                {
                    Ok(())
                } else {
                    Err(PropertyValueProblem::UnknownChoice)
                }
            }
            Self::Text => Ok(()),
        }
    }
}

/// Refuses a value a catalog key does not accept. Other keys are not checked.
pub fn check_property_value(key: &str, value: &str) -> ServerResult<()> {
    match property_definition(key) {
        Some(definition) => {
            definition
                .kind
                .check(value)
                .map_err(|problem| ServerError::InvalidProperty {
                    key: key.to_owned(),
                    reason: problem.reason(),
                })
        }
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::properties::{EULA_KEY, ONLINE_MODE_KEY, SERVER_IP_KEY, SERVER_PORT_KEY};

    #[test]
    fn every_key_is_listed_once_and_groups_stay_together_in_order() {
        let mut seen = std::collections::BTreeSet::new();
        for definition in SERVER_PROPERTY_CATALOG {
            assert!(seen.insert(definition.key), "{}", definition.key);
        }
        assert!(
            SERVER_PROPERTY_CATALOG
                .windows(2)
                .all(|pair| pair[0].group <= pair[1].group)
        );
    }

    #[test]
    fn typed_settings_and_remote_administration_are_not_in_the_catalog() {
        for key in [
            ONLINE_MODE_KEY,
            SERVER_IP_KEY,
            SERVER_PORT_KEY,
            EULA_KEY,
            "enable-rcon",
            "rcon.password",
            "enable-query",
            "enable-jmx-monitoring",
            "broadcast-rcon-to-ops",
        ] {
            assert!(property_definition(key).is_none(), "{key}");
        }
    }

    #[test]
    fn every_vanilla_default_is_a_value_its_key_accepts() {
        for definition in SERVER_PROPERTY_CATALOG {
            assert_eq!(
                definition.kind.check(definition.vanilla_default),
                Ok(()),
                "{}",
                definition.key
            );
        }
    }

    #[test]
    fn values_are_checked_against_their_key() {
        assert!(check_property_value("pvp", "TRUE").is_ok());
        assert!(check_property_value("pvp", "yes").is_err());
        assert!(check_property_value("view-distance", "32").is_ok());
        assert!(check_property_value("view-distance", "33").is_err());
        assert!(check_property_value("max-tick-time", "-5").is_ok());
        assert!(check_property_value("max-tick-time", "9223372036854775807").is_ok());
        assert!(check_property_value("max-players", "0").is_ok());
        assert!(check_property_value("spawn-protection", "-1").is_ok());
        assert!(check_property_value("max-players", "2147483648").is_err());
        assert!(check_property_value("max-players", "many").is_err());
        assert!(check_property_value("difficulty", "hard").is_ok());
        assert!(check_property_value("difficulty", "2").is_ok());
        assert!(check_property_value("difficulty", "4").is_err());
        assert!(check_property_value("difficulty", "+1").is_err());
        assert!(check_property_value("level-type", "minecraft:flat").is_ok());
        assert!(check_property_value("some-mod-key", "anything").is_ok());
    }
}
