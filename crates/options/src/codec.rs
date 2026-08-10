//! How one Minecraft release writes one setting into `options.txt`.
//!
//! # Why this is a closed set of variants
//!
//! The compatibility dataset is remote input. It arrives over the network from
//! a feed Ephervex does not serve, and it decides how values are written into a
//! file the game then reads. The one thing it must never be able to say is
//! "compute the stored value like *this*": a formula in JSON is an expression
//! somebody has to evaluate, and an evaluator reachable from network input is a
//! liability no launcher needs.
//!
//! So a codec is an enum. Every variant is implemented and reviewed here, and
//! its parameters are plain validated numbers and tokens. The generator's job
//! is to recognise a release's arithmetic as one of these variants at
//! *generation* time; a shape it cannot recognise becomes
//! [`PreferenceSupport::Unknown`](crate::compatibility::PreferenceSupport::Unknown) and is never written. A feed
//! naming a variant this build does not implement fails to deserialize, which
//! rejects the whole document rather than half-applying it.
//!
//! # Why encoding can fail
//!
//! A release's proven domain is part of its representation. Minecraft 26.2
//! accepts a simulation distance of 2 to 5; a preference of 12 is not something
//! to clamp into range, because a clamp writes a setting the user never chose.
//! It is [`Unrepresentable::OutOfRange`], the preference is skipped, and the
//! caller is told which one and why.

use std::fmt;

use serde::Deserialize;

/// One release's storage form for one setting.
///
/// Deserialized from the compatibility dataset. Unknown variant names are a
/// deserialization failure by construction: `serde` has nowhere to put a tag it
/// does not recognise, and there is deliberately no catch-all variant.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OptionCodec {
    /// Lowercase `true` / `false`.
    Boolean,
    /// Free text written verbatim after the first colon.
    Text,
    /// A Minecraft language code. Writing one additionally requires the target
    /// release's language catalog to contain it, which options.txt itself
    /// cannot express — see [`crate::LanguageAvailability`].
    LanguageCode,
    /// The pre-1.13 numeric key code.
    KeyCodeLwjgl2,
    /// The 1.13-and-later `key.keyboard.*` / `key.mouse.*` name.
    KeyNameGlfw,
    /// A whole number, written without a fractional part.
    Integer { min: Option<i64>, max: Option<i64> },
    /// A decimal number, written with a fractional part as the game writes it.
    Decimal { min: Option<f64>, max: Option<f64> },
    /// A decimal stored through an affine transform: `stored = (value - offset) / scale`.
    ///
    /// This is the pre-1.19 field of view, whose file value is
    /// `(degrees - 70) / 40` rather than the degrees the settings screen shows.
    /// `min` and `max` bound the *value*, not the stored number.
    ScaledDecimal {
        offset: f64,
        scale: f64,
        min: Option<f64>,
        max: Option<f64>,
    },
    /// An enumeration persisted as one of a fixed set of integer ids.
    EnumId { values: Vec<i64> },
    /// An enumeration persisted as one of a fixed set of string tokens.
    EnumToken { tokens: Vec<String> },
}

/// A value on its way into one options file, in the release-neutral form the
/// preference model holds it in.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingValue {
    Boolean(bool),
    Integer(i64),
    /// Held in the unit the settings screen shows, never in a stored form.
    Decimal(f64),
    Text(String),
}

/// Why a value cannot be written through a release's own representation.
///
/// Reported rather than worked around: every variant here describes a case
/// where writing *something* would mean writing a setting nobody chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Unrepresentable {
    /// The release stores a shape the value is not.
    #[error("the value is not the shape this release stores")]
    TypeMismatch,
    /// Outside the domain this release was proven to accept.
    #[error("the value is outside the range this release accepts")]
    OutOfRange,
    /// Not one of the tokens or ids this release's setting enumerates.
    #[error("the value is not one this release's setting enumerates")]
    NotInDomain,
    /// Contains something that would not survive the file's own line format.
    #[error("the value cannot be written as one options file line")]
    Unwritable,
}

impl OptionCodec {
    /// Encodes one value into the exact text this release stores.
    pub fn encode(&self, value: &SettingValue) -> Result<String, Unrepresentable> {
        match (self, value) {
            (Self::Boolean, SettingValue::Boolean(on)) => Ok(on.to_string()),
            (Self::Text, SettingValue::Text(text)) => {
                writable(text)?;
                Ok(text.clone())
            }
            (Self::LanguageCode, SettingValue::Text(code)) => {
                if is_language_code(code) {
                    Ok(code.clone())
                } else {
                    Err(Unrepresentable::Unwritable)
                }
            }
            (Self::Integer { min, max }, SettingValue::Integer(number)) => {
                in_range(number, *min, *max)?;
                Ok(number.to_string())
            }
            (Self::Decimal { min, max }, value) => {
                let number = as_decimal(value)?;
                in_range(&number, *min, *max)?;
                Ok(format_decimal(number))
            }
            (
                Self::ScaledDecimal {
                    offset,
                    scale,
                    min,
                    max,
                },
                value,
            ) => {
                let number = as_decimal(value)?;
                in_range(&number, *min, *max)?;
                if *scale == 0.0 || !scale.is_finite() || !offset.is_finite() {
                    return Err(Unrepresentable::Unwritable);
                }
                let stored = (number - offset) / scale;
                if stored.is_finite() {
                    Ok(format_decimal(stored))
                } else {
                    Err(Unrepresentable::Unwritable)
                }
            }
            (Self::EnumId { values }, SettingValue::Integer(id)) => {
                if values.contains(id) {
                    Ok(id.to_string())
                } else {
                    Err(Unrepresentable::NotInDomain)
                }
            }
            (Self::EnumToken { tokens }, SettingValue::Text(token)) => {
                if tokens.iter().any(|candidate| candidate == token) {
                    Ok(token.clone())
                } else {
                    Err(Unrepresentable::NotInDomain)
                }
            }
            // Key bindings have no Ephervex preference behind them, so there is
            // nothing to encode into one. The representation is still carried
            // so the dataset describes the release completely.
            _ => Err(Unrepresentable::TypeMismatch),
        }
    }

    /// Whether writing this setting additionally depends on the target
    /// release's language catalog.
    #[must_use]
    pub const fn needs_language_catalog(&self) -> bool {
        matches!(self, Self::LanguageCode)
    }

    /// Whether the codec's own parameters are usable.
    ///
    /// Checked when a dataset is validated, so a feed carrying an infinite
    /// scale or an empty enumeration is refused as a whole rather than failing
    /// later against one instance.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        match self {
            Self::Integer { min, max } => match (min, max) {
                (Some(low), Some(high)) => low <= high,
                _ => true,
            },
            Self::Decimal { min, max } => bounded(*min, *max),
            Self::ScaledDecimal {
                offset,
                scale,
                min,
                max,
            } => offset.is_finite() && scale.is_finite() && *scale != 0.0 && bounded(*min, *max),
            Self::EnumId { values } => !values.is_empty(),
            Self::EnumToken { tokens } => {
                !tokens.is_empty() && tokens.iter().all(|token| writable(token).is_ok())
            }
            _ => true,
        }
    }
}

impl fmt::Display for OptionCodec {
    /// The variant name, for developer logs and reports.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Boolean => "boolean",
            Self::Text => "text",
            Self::LanguageCode => "language_code",
            Self::KeyCodeLwjgl2 => "key_code_lwjgl2",
            Self::KeyNameGlfw => "key_name_glfw",
            Self::Integer { .. } => "integer",
            Self::Decimal { .. } => "decimal",
            Self::ScaledDecimal { .. } => "scaled_decimal",
            Self::EnumId { .. } => "enum_id",
            Self::EnumToken { .. } => "enum_token",
        };
        f.write_str(name)
    }
}

fn bounded(min: Option<f64>, max: Option<f64>) -> bool {
    let finite = min.is_none_or(f64::is_finite) && max.is_none_or(f64::is_finite);
    finite
        && match (min, max) {
            (Some(low), Some(high)) => low <= high,
            _ => true,
        }
}

fn as_decimal(value: &SettingValue) -> Result<f64, Unrepresentable> {
    match value {
        SettingValue::Decimal(number) if number.is_finite() => Ok(*number),
        #[expect(
            clippy::cast_precision_loss,
            reason = "options values are settings-screen magnitudes, far inside f64's exact integer range"
        )]
        SettingValue::Integer(number) => Ok(*number as f64),
        _ => Err(Unrepresentable::TypeMismatch),
    }
}

fn in_range<T: PartialOrd>(
    value: &T,
    min: Option<T>,
    max: Option<T>,
) -> Result<(), Unrepresentable> {
    let inside = min.is_none_or(|low| *value >= low) && max.is_none_or(|high| *value <= high);
    if inside {
        Ok(())
    } else {
        Err(Unrepresentable::OutOfRange)
    }
}

/// Rejects anything that would not survive one `key:value` line.
///
/// A newline turns one setting into two, and a control character in a file the
/// game parses by hand is not something to find out about at launch.
fn writable(text: &str) -> Result<(), Unrepresentable> {
    if text.chars().any(char::is_control) {
        Err(Unrepresentable::Unwritable)
    } else {
        Ok(())
    }
}

/// Minecraft's own language-code form: lowercase letters, digits, `_` and `-`.
fn is_language_code(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= 32
        && code.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
}

/// Formats a number the way the game writes a decimal option: never in
/// exponent form, and a whole number keeps its `.0`.
fn format_decimal(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{value:.1}")
    } else {
        format!("{value}")
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn parse(json: &str) -> Result<OptionCodec, serde_json::Error> {
        serde_json::from_str(json)
    }

    #[test]
    fn a_codec_variant_this_build_does_not_implement_refuses_to_deserialize() {
        // The whole point of a closed set: remote metadata cannot introduce a
        // way of computing a stored value, only select one of these.
        for hostile in [
            r#"{"kind":"eval","expression":"value * 2"}"#,
            r#"{"kind":"javascript","source":"(v)=>v"}"#,
            r#"{"kind":"formula","text":"((value - 70) / 40)"}"#,
            r#"{"kind":"Boolean"}"#,
        ] {
            assert!(parse(hostile).is_err(), "`{hostile}` must not deserialize");
        }
    }

    #[test]
    fn every_implemented_variant_deserializes_from_the_generated_shape() {
        let cases = [
            (r#"{"kind":"boolean"}"#, OptionCodec::Boolean),
            (r#"{"kind":"text"}"#, OptionCodec::Text),
            (r#"{"kind":"language_code"}"#, OptionCodec::LanguageCode),
            (r#"{"kind":"key_code_lwjgl2"}"#, OptionCodec::KeyCodeLwjgl2),
            (r#"{"kind":"key_name_glfw"}"#, OptionCodec::KeyNameGlfw),
            (
                r#"{"kind":"integer","min":2,"max":32}"#,
                OptionCodec::Integer {
                    min: Some(2),
                    max: Some(32),
                },
            ),
            (
                r#"{"kind":"decimal","min":30,"max":110}"#,
                OptionCodec::Decimal {
                    min: Some(30.0),
                    max: Some(110.0),
                },
            ),
            (
                r#"{"kind":"scaled_decimal","offset":70,"scale":40,"min":null,"max":null}"#,
                OptionCodec::ScaledDecimal {
                    offset: 70.0,
                    scale: 40.0,
                    min: None,
                    max: None,
                },
            ),
            (
                r#"{"kind":"enum_id","values":[0,1,2]}"#,
                OptionCodec::EnumId {
                    values: vec![0, 1, 2],
                },
            ),
            (
                r#"{"kind":"enum_token","tokens":["left","right"]}"#,
                OptionCodec::EnumToken {
                    tokens: vec!["left".to_owned(), "right".to_owned()],
                },
            ),
        ];
        for (json, expected) in cases {
            assert_eq!(parse(json).expect("valid codec"), expected, "{json}");
        }
    }

    #[test]
    fn the_legacy_field_of_view_transform_reproduces_the_games_own_arithmetic() {
        // 1.12.2 through 1.18.2 store `(degrees - 70) / 40`, so 70 degrees is
        // `0.0` and Quake Pro is `1.0`.
        let legacy = OptionCodec::ScaledDecimal {
            offset: 70.0,
            scale: 40.0,
            min: None,
            max: None,
        };
        assert_eq!(legacy.encode(&SettingValue::Decimal(70.0)).unwrap(), "0.0");
        assert_eq!(legacy.encode(&SettingValue::Decimal(90.0)).unwrap(), "0.5");
        assert_eq!(legacy.encode(&SettingValue::Decimal(110.0)).unwrap(), "1.0");
        assert_eq!(legacy.encode(&SettingValue::Decimal(30.0)).unwrap(), "-1.0");
    }

    #[test]
    fn the_modern_field_of_view_is_written_in_degrees_as_the_game_writes_it() {
        let modern = OptionCodec::Decimal {
            min: Some(30.0),
            max: Some(110.0),
        };
        assert_eq!(modern.encode(&SettingValue::Decimal(70.0)).unwrap(), "70.0");
        assert_eq!(modern.encode(&SettingValue::Decimal(82.5)).unwrap(), "82.5");
        assert_eq!(
            modern.encode(&SettingValue::Decimal(120.0)),
            Err(Unrepresentable::OutOfRange)
        );
    }

    #[test]
    fn a_value_outside_a_releases_proven_domain_is_refused_rather_than_clamped() {
        // Clamping writes a setting nobody chose into every instance created
        // afterwards.
        let narrow = OptionCodec::Integer {
            min: Some(2),
            max: Some(5),
        };
        assert_eq!(narrow.encode(&SettingValue::Integer(4)).unwrap(), "4");
        assert_eq!(
            narrow.encode(&SettingValue::Integer(12)),
            Err(Unrepresentable::OutOfRange)
        );
    }

    #[test]
    fn an_enumeration_accepts_only_what_the_release_enumerates() {
        let ids = OptionCodec::EnumId {
            values: vec![0, 1, 2],
        };
        let tokens = OptionCodec::EnumToken {
            tokens: vec!["left".to_owned(), "right".to_owned()],
        };
        assert_eq!(ids.encode(&SettingValue::Integer(2)).unwrap(), "2");
        assert_eq!(
            ids.encode(&SettingValue::Integer(3)),
            Err(Unrepresentable::NotInDomain)
        );
        assert_eq!(
            tokens
                .encode(&SettingValue::Text("left".to_owned()))
                .unwrap(),
            "left"
        );
        assert_eq!(
            tokens.encode(&SettingValue::Text("middle".to_owned())),
            Err(Unrepresentable::NotInDomain)
        );
    }

    #[test]
    fn a_value_cannot_smuggle_a_second_setting_into_the_file() {
        for hostile in ["en_us\nfullscreen:true", "a\rb"] {
            assert_eq!(
                OptionCodec::Text.encode(&SettingValue::Text(hostile.to_owned())),
                Err(Unrepresentable::Unwritable),
                "{hostile:?}"
            );
        }
        assert_eq!(
            OptionCodec::LanguageCode.encode(&SettingValue::Text("EN_US".to_owned())),
            Err(Unrepresentable::Unwritable),
            "Minecraft's own codes are lowercase"
        );
    }

    #[test]
    fn a_codec_whose_parameters_are_unusable_is_not_well_formed() {
        assert!(
            !OptionCodec::ScaledDecimal {
                offset: 70.0,
                scale: 0.0,
                min: None,
                max: None,
            }
            .is_well_formed()
        );
        assert!(
            !OptionCodec::Integer {
                min: Some(10),
                max: Some(2),
            }
            .is_well_formed()
        );
        assert!(!OptionCodec::EnumId { values: vec![] }.is_well_formed());
        assert!(
            OptionCodec::Decimal {
                min: Some(30.0),
                max: Some(110.0),
            }
            .is_well_formed()
        );
    }

    #[test]
    fn a_key_binding_representation_has_no_preference_to_encode_into_it() {
        for codec in [OptionCodec::KeyCodeLwjgl2, OptionCodec::KeyNameGlfw] {
            assert_eq!(
                codec.encode(&SettingValue::Integer(17)),
                Err(Unrepresentable::TypeMismatch)
            );
        }
    }
}
