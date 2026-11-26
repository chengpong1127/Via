//! Pause after TDX failures: short first, doubling while failures repeat, reset on success.

use std::time::{Duration, Instant};

/// Longest pause the breaker or a `Retry-After` header may impose.
pub use crate::tdx::MAX_RETRY_AFTER as MAX_PAUSE;

/// Failures that are not a 429 only pause once they repeat.
const REPEATS_BEFORE_NON_RATE_LIMIT_PAUSE: u32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    RateLimited,
    Upstream,
}

pub struct Breaker {
    first_step: Duration,
    consecutive_failures: u32,
    blocked_until: Option<Instant>,
}

impl Breaker {
    pub fn new(first_step: Duration) -> Self {
        Self {
            first_step,
            consecutive_failures: 0,
            blocked_until: None,
        }
    }

    pub fn is_blocked(&mut self, now: Instant) -> bool {
        match self.blocked_until {
            Some(until) if until > now => true,
            Some(_) => {
                self.blocked_until = None;
                false
            }
            None => false,
        }
    }

    /// Records a failure and returns the pause it started, if any.
    pub fn record_failure(
        &mut self,
        now: Instant,
        failure: Failure,
        retry_after: Option<Duration>,
    ) -> Option<Duration> {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        if failure == Failure::Upstream
            && self.consecutive_failures < REPEATS_BEFORE_NON_RATE_LIMIT_PAUSE
        {
            return None;
        }
        let pause = retry_after
            .map(|header| header.min(MAX_PAUSE))
            .unwrap_or_else(|| self.step());
        if pause.is_zero() {
            return None;
        }
        let until = now + pause;
        if self.blocked_until.is_none_or(|current| current < until) {
            self.blocked_until = Some(until);
        }
        Some(pause)
    }

    pub fn record_success(&mut self) {
        self.consecutive_failures = 0;
    }

    fn step(&self) -> Duration {
        let doublings = self.consecutive_failures.saturating_sub(1).min(16);
        self.first_step
            .checked_mul(1_u32 << doublings)
            .unwrap_or(MAX_PAUSE)
            .min(MAX_PAUSE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn a_429_without_retry_after_pauses_for_the_first_step() {
        let now = Instant::now();
        let mut breaker = Breaker::new(secs(3));
        assert_eq!(
            breaker.record_failure(now, Failure::RateLimited, None),
            Some(secs(3))
        );
        assert!(breaker.is_blocked(now + secs(2)));
        assert!(!breaker.is_blocked(now + secs(3)));
    }

    #[test]
    fn consecutive_failures_double_up_to_sixty_seconds() {
        let now = Instant::now();
        let mut breaker = Breaker::new(secs(3));
        let pauses = (0..7)
            .map(|_| breaker.record_failure(now, Failure::RateLimited, None))
            .collect::<Vec<_>>();
        let expected = [3, 6, 12, 24, 48, 60, 60].map(|s| Some(secs(s)));
        assert_eq!(pauses, expected);
    }

    #[test]
    fn success_resets_the_step() {
        let now = Instant::now();
        let mut breaker = Breaker::new(secs(3));
        breaker.record_failure(now, Failure::RateLimited, None);
        breaker.record_failure(now, Failure::RateLimited, None);
        breaker.record_success();
        assert_eq!(
            breaker.record_failure(now, Failure::RateLimited, None),
            Some(secs(3))
        );
    }

    #[test]
    fn retry_after_is_capped_at_sixty_seconds() {
        let now = Instant::now();
        let mut breaker = Breaker::new(secs(3));
        assert_eq!(
            breaker.record_failure(now, Failure::RateLimited, Some(secs(3600))),
            Some(secs(60))
        );
        assert!(breaker.is_blocked(now + secs(59)));
        assert!(!breaker.is_blocked(now + secs(60)));
    }

    #[test]
    fn a_single_upstream_failure_does_not_pause_but_repeats_do() {
        let now = Instant::now();
        let mut breaker = Breaker::new(secs(3));
        assert_eq!(breaker.record_failure(now, Failure::Upstream, None), None);
        assert!(!breaker.is_blocked(now));
        assert_eq!(
            breaker.record_failure(now, Failure::Upstream, None),
            Some(secs(6))
        );
        assert!(breaker.is_blocked(now + secs(5)));
    }

    #[test]
    fn a_success_between_upstream_failures_keeps_them_unpaused() {
        let now = Instant::now();
        let mut breaker = Breaker::new(secs(3));
        assert_eq!(breaker.record_failure(now, Failure::Upstream, None), None);
        breaker.record_success();
        assert_eq!(breaker.record_failure(now, Failure::Upstream, None), None);
    }
}
