//! Retry policy for transient failures.
//!
//! Ephervex is used while services are flaky — that is close to its reason for
//! existing — so a single 503 from a CDN must not abort a pack import. What it
//! must *not* do is retry things that cannot improve: a 404, a 403, or a hash
//! mismatch means asking again is pure noise for the upstream service and pure
//! delay for the user. Classification lives in
//! [`crate::DownloadError::is_transient`]; the schedule lives here.

use std::time::Duration;

use reqwest::header::{HeaderMap, RETRY_AFTER};

/// How many times to try, and how long to wait between tries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Total attempts including the first, so `1` means "no retries".
    pub max_attempts: u32,
    /// Delay before the second attempt.
    pub initial_backoff: Duration,
    /// Ceiling for the exponential growth, and for any server-supplied
    /// `Retry-After`. A hostile or misconfigured server must not be able to
    /// park a download for an hour.
    pub max_backoff: Duration,
    /// Growth factor applied per attempt.
    pub multiplier: u32,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 4,
            initial_backoff: Duration::from_millis(250),
            max_backoff: Duration::from_secs(8),
            multiplier: 2,
        }
    }
}

impl RetryPolicy {
    /// Fail on the first error. Useful in tests and for interactive operations
    /// where the user is waiting and would rather see the failure.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            max_attempts: 1,
            initial_backoff: Duration::ZERO,
            max_backoff: Duration::ZERO,
            multiplier: 1,
        }
    }

    /// Delay to wait after `attempt` (1-based) failed.
    ///
    /// The schedule is deterministic: no jitter is applied, because the
    /// workspace has no random-number dependency and the download manager's
    /// bounded concurrency already caps how many requests can collide after a
    /// shared outage. If a downstream consumers fans out widely enough for
    /// synchronized retries to matter, add jitter here.
    #[must_use]
    pub fn backoff_after(&self, attempt: u32) -> Duration {
        let exponent = attempt.saturating_sub(1);
        let factor = self
            .multiplier
            .checked_pow(exponent)
            .unwrap_or(u32::MAX)
            .max(1);
        self.initial_backoff
            .saturating_mul(factor)
            .min(self.max_backoff)
    }

    /// Combines the schedule with a server-supplied hint, clamped to
    /// [`Self::max_backoff`].
    #[must_use]
    pub fn delay_after(&self, attempt: u32, retry_after: Option<Duration>) -> Duration {
        match retry_after {
            Some(hint) => hint.min(self.max_backoff),
            None => self.backoff_after(attempt),
        }
    }
}

/// Whether an HTTP status is worth trying again.
///
/// 408, 425, and 429 are explicit "try later" signals. 5xx is a server-side
/// fault, except for the two that describe a permanent capability gap: 501
/// (unimplemented method) and 505 (unsupported HTTP version) will answer
/// identically forever.
#[must_use]
pub fn status_is_transient(status: u16) -> bool {
    match status {
        // A permanent capability gap: the same request will get the same
        // answer forever.
        501 | 505 => false,
        408 | 425 | 429 | 500..=599 => true,
        _ => false,
    }
}

/// Reads a `Retry-After` header expressed in seconds.
///
/// The HTTP-date form is deliberately unsupported: honoring it requires
/// trusting a remote clock, and falling back to the exponential schedule is
/// always a safe answer.
#[must_use]
pub fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use reqwest::header::HeaderValue;

    #[test]
    fn backoff_grows_exponentially_until_it_hits_the_ceiling() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.backoff_after(1), Duration::from_millis(250));
        assert_eq!(policy.backoff_after(2), Duration::from_millis(500));
        assert_eq!(policy.backoff_after(3), Duration::from_secs(1));
        assert_eq!(policy.backoff_after(50), policy.max_backoff);
    }

    #[test]
    fn a_policy_of_none_makes_the_first_failure_final() {
        assert_eq!(RetryPolicy::none().max_attempts, 1);
    }

    #[test]
    fn a_server_hint_wins_but_is_clamped_to_the_ceiling() {
        let policy = RetryPolicy::default();
        assert_eq!(
            policy.delay_after(1, Some(Duration::from_secs(2))),
            Duration::from_secs(2)
        );
        assert_eq!(
            policy.delay_after(1, Some(Duration::from_secs(3600))),
            policy.max_backoff
        );
    }

    #[test]
    fn rate_limits_and_gateway_faults_are_transient_but_client_errors_are_not() {
        assert!(status_is_transient(429));
        assert!(status_is_transient(503));
        assert!(status_is_transient(500));
        assert!(!status_is_transient(404));
        assert!(!status_is_transient(403));
        assert!(!status_is_transient(501));
        assert!(!status_is_transient(200));
    }

    #[test]
    fn a_numeric_retry_after_is_read_and_a_date_is_ignored() {
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, HeaderValue::from_static("12"));
        assert_eq!(retry_after(&headers), Some(Duration::from_secs(12)));

        let mut dated = HeaderMap::new();
        dated.insert(
            RETRY_AFTER,
            HeaderValue::from_static("Wed, 21 Oct 2015 07:28:00 GMT"),
        );
        assert_eq!(retry_after(&dated), None);
    }
}
