//! A string that cannot be logged by accident.
//!
//! Every credential the launcher holds is a [`SecretString`] - the Microsoft
//! refresh token, the authorization code, the CurseForge API key. That is not
//! decoration: the codebase logs liberally with `tracing`, `#[instrument]`
//! derives fields from function arguments, and `{error:?}` is the normal way to
//! render a failure. Any of those turns a plain `String` refresh token into a
//! line in a file that users routinely paste into bug reports.
//!
//! It lives in the model rather than beside one of its consumers because more
//! than one provider crate holds a credential, and a second wrapper - or a raw
//! `String` threaded through a provider that did not want to depend on
//! authentication - is exactly the leak this type exists to prevent.
//!
//! So the type refuses to render itself. `Debug` prints a redaction marker,
//! there is no `Display`, and reading the value requires calling
//! [`SecretString::expose`] - a name chosen so that the eight or so call sites
//! that legitimately need the bytes are greppable, and so that writing one by
//! accident is difficult.
//!
//! # What this deliberately does not do
//!
//! It does not zero its memory on drop. Doing that properly needs a crate like
//! `zeroize` plus care about every move the optimizer is allowed to make, and a
//! half-measure - overwriting one copy while the allocator holds three others -
//! would buy the appearance of protection rather than the fact of it. The
//! threat this type is built against is *disclosure through logs*, which is the
//! one that actually happens to launchers, and against that it is complete.

use std::fmt;

/// A credential: an OAuth token, an authorization code, or an API key.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretString(String);

/// What a redacted secret renders as. Recognizable in a log, and short enough
/// not to be mistaken for the value.
pub const REDACTED: &str = "<redacted>";

impl SecretString {
    /// Wraps a value that must never be logged.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Reads the secret.
    ///
    /// Deliberately verbose. Every call is a place where a credential leaves
    /// its wrapper, and those places should be easy to enumerate.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Whether the secret is empty, which is how an absent value arrives from
    /// a configuration field or an API response that omitted it.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The secret with surrounding whitespace removed, or `None` when nothing
    /// is left of it.
    ///
    /// Here rather than at each call site so that a settings screen can refuse
    /// a blank value without exposing the bytes to decide it. Trimming is not a
    /// format rule imposed on an opaque credential: a value pasted into a field
    /// routinely arrives with a trailing newline, and HTTP field values have
    /// their surrounding whitespace stripped by definition (RFC 9110 §5.5), so
    /// the untrimmed value could never have been sent verbatim anyway.
    #[must_use]
    pub fn trimmed(&self) -> Option<Self> {
        let trimmed = self.0.trim();
        (!trimmed.is_empty()).then(|| Self(trimmed.to_owned()))
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Not `SecretString("...")`: a bare marker survives being embedded in a
        // larger `{:?}` without looking like a value that happens to be short.
        formatter.write_str(REDACTED)
    }
}

impl From<String> for SecretString {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for SecretString {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl<'de> serde::Deserialize<'de> for SecretString {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    const TOKEN: &str = "M.C523_BAY.0.U.-Cn8Fak3RefreshTokenValue";

    #[test]
    fn debug_output_never_contains_the_value() {
        let secret = SecretString::new(TOKEN);
        assert_eq!(format!("{secret:?}"), REDACTED);
        assert!(!format!("{secret:?}").contains("Fak3"));
    }

    #[test]
    fn a_secret_nested_inside_another_debug_render_is_still_redacted() {
        // This is the case that actually bites: nobody logs the token directly,
        // they log the struct that happens to hold one.
        // Never read directly; the point is what `{:?}` does with them.
        #[derive(Debug)]
        #[allow(dead_code)]
        struct Tokens {
            refresh: SecretString,
            expires_in: u64,
        }
        let rendered = format!(
            "{:?}",
            Tokens {
                refresh: SecretString::new(TOKEN),
                expires_in: 3600,
            }
        );
        assert!(!rendered.contains("Fak3"), "{rendered}");
        assert!(rendered.contains("3600"), "non-secret fields still render");
    }

    #[test]
    fn the_value_is_readable_through_the_deliberately_named_accessor() {
        assert_eq!(SecretString::new(TOKEN).expose(), TOKEN);
        assert!(SecretString::new("").is_empty());
    }

    #[test]
    fn a_value_that_is_only_whitespace_trims_away_to_nothing() {
        // What a settings screen needs to refuse a blank entry without reading
        // the bytes to find out.
        for blank in ["", " ", "\t", "\n", "  \r\n  "] {
            assert!(SecretString::new(blank).trimmed().is_none(), "{blank:?}");
        }
    }

    #[test]
    fn trimming_removes_the_newline_a_paste_carries_and_nothing_else() {
        let trimmed = SecretString::new("  $2a$10$Fak3Key  \n")
            .trimmed()
            .expect("a value remains");
        assert_eq!(trimmed.expose(), "$2a$10$Fak3Key");
        // Whitespace inside the value is part of it, not surrounding it.
        assert_eq!(
            SecretString::new(" a b ")
                .trimmed()
                .expect("a value remains")
                .expose(),
            "a b"
        );
    }
}
