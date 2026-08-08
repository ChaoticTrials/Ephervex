//! Which leg of the chain a message or a failure is about.
//!
//! Authentication is five services in a row, and every one of them can fail
//! with a 503 that looks identical from the outside. Naming the leg is what
//! turns "authentication failed" into something a user or a bug report can act
//! on, and it is what the progress surface reports as the flow advances.

use std::fmt;

/// One step of the Microsoft → Xbox → Minecraft chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum AuthStage {
    /// Getting a Microsoft access token: the browser flow, the device code
    /// flow, or a refresh.
    Microsoft,
    /// `POST user.auth.xboxlive.com/user/authenticate`.
    XboxLive,
    /// `POST xsts.auth.xboxlive.com/xsts/authorize`.
    Xsts,
    /// `POST api.minecraftservices.com/authentication/login_with_xbox`.
    MinecraftLogin,
    /// `GET api.minecraftservices.com/entitlements/mcstore`.
    Entitlements,
    /// `GET api.minecraftservices.com/minecraft/profile`.
    Profile,
}

impl AuthStage {
    /// Every stage, in the order the chain runs them.
    pub const ORDER: [Self; 6] = [
        Self::Microsoft,
        Self::XboxLive,
        Self::Xsts,
        Self::MinecraftLogin,
        Self::Entitlements,
        Self::Profile,
    ];

    /// A stable identifier for logs, `--json` output, and message arguments.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Microsoft => "microsoft",
            Self::XboxLive => "xbox-live",
            Self::Xsts => "xsts",
            Self::MinecraftLogin => "minecraft-login",
            Self::Entitlements => "entitlements",
            Self::Profile => "profile",
        }
    }

    /// The `ephervex-i18n` message id naming this stage to a user.
    #[must_use]
    pub const fn message_id(self) -> &'static str {
        match self {
            Self::Microsoft => "auth-stage-microsoft",
            Self::XboxLive => "auth-stage-xbox-live",
            Self::Xsts => "auth-stage-xsts",
            Self::MinecraftLogin => "auth-stage-minecraft-login",
            Self::Entitlements => "auth-stage-entitlements",
            Self::Profile => "auth-stage-profile",
        }
    }
}

impl fmt::Display for AuthStage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn every_stage_has_a_distinct_identifier_and_message_id() {
        let mut names: Vec<_> = AuthStage::ORDER
            .iter()
            .map(|stage| stage.as_str())
            .collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(names.len(), before);

        let mut ids: Vec<_> = AuthStage::ORDER
            .iter()
            .map(|stage| stage.message_id())
            .collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), before);
    }

    #[test]
    fn the_order_is_the_order_the_chain_runs() {
        // Ownership is checked before the profile, and both after a Minecraft
        // token exists. Authentication ordering requires this before any
        // accident of how the enum was written.
        assert!(AuthStage::Microsoft < AuthStage::XboxLive);
        assert!(AuthStage::XboxLive < AuthStage::Xsts);
        assert!(AuthStage::Xsts < AuthStage::MinecraftLogin);
        assert!(AuthStage::MinecraftLogin < AuthStage::Entitlements);
        assert!(AuthStage::Entitlements < AuthStage::Profile);
    }
}
