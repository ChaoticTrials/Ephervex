//! The typed form of an instance's optional memory fields.

/// Reads a memory field in MiB: blank follows the global setting (`None`), and
/// anything else must be a plain whole number, never silently dropped.
pub(crate) fn parse_optional_mib(text: &str) -> Result<Option<u32>, NotANumber> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if !trimmed.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(NotANumber);
    }
    trimmed.parse().map(Some).map_err(|_| NotANumber)
}

/// A memory field holds something other than a plain whole number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NotANumber;

impl NotANumber {
    /// The localization message that explains this refusal.
    pub(crate) const MESSAGE_ID: &'static str = "gui-java-memory-digits-only";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blank_field_follows_the_global_setting() {
        assert_eq!(parse_optional_mib(""), Ok(None));
        assert_eq!(parse_optional_mib("  \t "), Ok(None));
    }

    #[test]
    fn digits_are_read_as_mebibytes() {
        assert_eq!(parse_optional_mib("4096"), Ok(Some(4096)));
        assert_eq!(parse_optional_mib(" 512 "), Ok(Some(512)));
    }

    #[test]
    fn text_that_is_not_a_plain_number_is_refused_instead_of_inherited() {
        for text in ["4G", "abc", "-1", "+512", "5 12", "1.5", "4096MiB"] {
            assert_eq!(parse_optional_mib(text), Err(NotANumber), "{text:?}");
        }
    }

    #[test]
    fn a_number_too_large_for_the_field_is_refused() {
        assert_eq!(parse_optional_mib("99999999999"), Err(NotANumber));
    }
}
