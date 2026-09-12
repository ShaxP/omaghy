//! Backoff, and the one rule that is not a tuning knob.
//!
//! **A mutation is never retried automatically** (`spec/20-store.md` §6). A
//! duplicated review or a double merge is worse than a visible failure, and
//! GitHub gives us no way to tell a lost response from a lost request. So
//! [`RetryPolicy::attempts_for`] returns 1 for anything that mutates, and the
//! decision lives here rather than at each call site.
//!
//! Everything else backs off exponentially and capped: 5xx, an unreachable
//! host, and a secondary limit short enough to be worth waiting out.

use async_trait::async_trait;
use std::fmt;
use time::Duration;

/// How a retryable failure is spaced out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Total attempts, including the first. 1 means "never retry".
    pub max_attempts: u32,
    /// The delay before the second attempt. Doubles thereafter.
    pub base_delay: Duration,
    /// The ceiling on a computed backoff.
    pub max_delay: Duration,
    /// The longest server-mandated `Retry-After` we will wait out inline.
    /// Anything longer is reported as a rate limit for the UI to render,
    /// because blocking a refresh for minutes is indistinguishable from a hang.
    pub max_retry_after: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_delay: Duration::milliseconds(500),
            max_delay: Duration::seconds(8),
            max_retry_after: Duration::seconds(30),
        }
    }
}

impl RetryPolicy {
    /// A policy that never retries. Useful for `omaghy doctor`, where one
    /// honest failure beats three slow ones.
    pub fn none() -> Self {
        Self {
            max_attempts: 1,
            ..Self::default()
        }
    }

    /// Attempts allowed for a request, given whether it mutates.
    pub fn attempts_for(&self, mutation: bool) -> u32 {
        if mutation {
            1
        } else {
            self.max_attempts.max(1)
        }
    }

    /// The wait before attempt `attempt` (1-based: the delay before attempt 2
    /// is `base_delay`).
    ///
    /// No jitter. omaghy is one process making a handful of requests on behalf
    /// of one human; jitter exists to de-synchronise fleets, and adding it here
    /// would only make the backoff untestable.
    pub fn delay_before(&self, attempt: u32) -> Duration {
        if attempt <= 1 {
            return Duration::ZERO;
        }
        let doublings = (attempt - 2).min(16);
        let scaled = self
            .base_delay
            .checked_mul(1i32 << doublings)
            .unwrap_or(self.max_delay);
        scaled.min(self.max_delay)
    }
}

/// Waiting, injectable.
///
/// A backoff test that actually sleeps is a slow test, and a slow test that
/// asserts on wall-clock time is a flaky one. Tests substitute
/// [`crate::cassette::RecordingSleeper`] and assert on the delays that were
/// *requested*.
#[async_trait]
pub trait Sleeper: fmt::Debug + Send + Sync + 'static {
    async fn sleep(&self, duration: Duration);
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RealSleeper;

#[async_trait]
impl Sleeper for RealSleeper {
    async fn sleep(&self, duration: Duration) {
        if let Ok(d) = duration.try_into() {
            tokio::time::sleep(d).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mutation_is_attempted_exactly_once() {
        let p = RetryPolicy::default();
        assert_eq!(p.attempts_for(true), 1);
        assert!(p.attempts_for(false) > 1);
    }

    #[test]
    fn backoff_doubles_and_then_stops() {
        let p = RetryPolicy {
            max_attempts: 6,
            base_delay: Duration::milliseconds(500),
            max_delay: Duration::seconds(2),
            ..RetryPolicy::default()
        };
        assert_eq!(p.delay_before(1), Duration::ZERO);
        assert_eq!(p.delay_before(2), Duration::milliseconds(500));
        assert_eq!(p.delay_before(3), Duration::seconds(1));
        assert_eq!(p.delay_before(4), Duration::seconds(2));
        assert_eq!(p.delay_before(5), Duration::seconds(2), "capped");
        assert_eq!(p.delay_before(99), Duration::seconds(2), "still capped");
    }

    #[test]
    fn never_retrying_is_expressible() {
        assert_eq!(RetryPolicy::none().attempts_for(false), 1);
    }
}
