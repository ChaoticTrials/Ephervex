//! Service-supplied token lifetimes.
//!
//! `expires_in` arrives as a bare integer from a network service, so it is
//! untrusted: it can be negative, absent, or far beyond any date the clock can
//! represent. [`Lifetime`] bounds it once, at the response boundary, so the
//! expiry arithmetic after it cannot overflow.

use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Deserializer};

/// Largest magnitude kept, in seconds: a century.
///
/// Far beyond any lifetime a token service issues, and small enough that
/// adding it to the current time stays inside the range `chrono` can
/// represent. A larger value is clamped to this rather than refused.
const MAX_SECONDS: i64 = 100 * 366 * 24 * 60 * 60;

/// How long a service says something stays valid, in seconds from now.
///
/// Zero or negative means already expired. The value is clamped to
/// plus or minus [`MAX_SECONDS`] when it is read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Lifetime(i64);

impl Lifetime {
    pub(crate) fn from_seconds(seconds: i64) -> Self {
        Self(seconds.clamp(-MAX_SECONDS, MAX_SECONDS))
    }

    pub(crate) const fn seconds(self) -> i64 {
        self.0
    }

    /// Whether it leaves any time at all.
    pub(crate) const fn is_positive(self) -> bool {
        self.0 > 0
    }

    /// The remaining time, with an already-expired lifetime reading as zero.
    pub(crate) fn remaining(self) -> Duration {
        Duration::from_secs(u64::try_from(self.0).unwrap_or(0))
    }

    /// The moment it runs out, counted from now.
    pub(crate) fn expires_at(self) -> DateTime<Utc> {
        let now = Utc::now();
        // The clamp keeps the sum representable; `now` is the fallback only
        // for a clock already outside the supported range.
        now.checked_add_signed(TimeDelta::seconds(self.0))
            .unwrap_or(now)
    }
}

impl<'de> Deserialize<'de> for Lifetime {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        i64::deserialize(deserializer).map(Self::from_seconds)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn a_lifetime_far_beyond_the_clock_range_still_yields_an_expiry() {
        for seconds in [9_000_000_000_000, i64::MAX / 1000, i64::MAX, i64::MIN] {
            let lifetime = Lifetime::from_seconds(seconds);
            let expiry = lifetime.expires_at();
            assert_eq!(expiry > Utc::now(), seconds > 0, "{seconds}");
        }
    }

    #[test]
    fn an_ordinary_lifetime_is_kept_exactly() {
        let lifetime = Lifetime::from_seconds(86_400);
        assert_eq!(lifetime.seconds(), 86_400);
        assert_eq!(lifetime.remaining(), Duration::from_hours(24));
        assert!(lifetime.is_positive());
    }

    #[test]
    fn a_zero_or_negative_lifetime_has_nothing_remaining() {
        for seconds in [0, -1, -86_400] {
            let lifetime = Lifetime::from_seconds(seconds);
            assert!(!lifetime.is_positive(), "{seconds}");
            assert_eq!(lifetime.remaining(), Duration::ZERO, "{seconds}");
        }
    }
}
