//! Application identity constants.

/// The single source of truth for the application identifier.
///
/// Used for XDG directory names, the temporary-instance root, the keyring
/// service name, and the desktop entry. Never hardcode this string elsewhere.
pub const APP_ID: &str = "ephervex";

/// Human-readable application name for user-facing surfaces.
pub const APP_NAME: &str = "Ephervex";

/// Version of the running build, taken from the workspace package version.
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Project URL advertised in the HTTP `User-Agent` so upstream services can
/// identify and contact us.
pub const APP_URL: &str = "https://github.com/ChaoticTrials/ephervex";

/// The `User-Agent` sent with every outbound HTTP request.
#[must_use]
pub fn user_agent() -> String {
    format!("{APP_NAME}/{APP_VERSION} (+{APP_URL})")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn user_agent_identifies_the_application_and_a_contact_url() {
        let ua = user_agent();
        assert!(ua.starts_with("Ephervex/"));
        assert!(ua.contains(APP_URL));
    }
}
