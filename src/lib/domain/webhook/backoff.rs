//! Exponential backoff for webhook forwarding.
//!
//! A target that is down is retried on a growing delay instead of every
//! `interval-seconds` forever: the first retry still happens after one interval
//! (so a healthy channel behaves exactly as it did before this module existed),
//! and each further failure multiplies the wait up to a ceiling.
//!
//! The delay is stored per webhook (`next_attempt_at`), not per channel, so a
//! webhook the target keeps rejecting cannot hold up the ones queued behind it.
//!
//! Everything here is a pure function: the caller supplies the attempt count,
//! any `Retry-After` the target sent, and the randomness used for jitter, which
//! keeps the arithmetic testable without a clock or an RNG.

use std::time::Duration;

/// Backoff parameters for one channel, with per-channel overrides already
/// resolved over the global defaults.
#[derive(Debug, Clone, PartialEq)]
pub struct ForwardBackoff {
    /// Delay before the first retry and the base the exponent grows from. This
    /// is the channel's `interval-seconds`, which is why the first retry is
    /// unchanged by backoff.
    pub base: Duration,
    /// Growth factor per failed attempt. `1.0` disables growth, which is the
    /// pre-backoff behaviour.
    pub multiplier: f64,
    /// Ceiling for a single delay. Retries never stop — they only become rare,
    /// so a target that comes back hours later still drains its queue.
    pub max: Duration,
    /// Fraction of the delay that jitter may add or subtract, `0.0..=1.0`.
    /// Keeps several channels (or replicas) from retrying in lockstep.
    pub jitter: f64,
}

/// Why a delivery attempt failed, in terms of what retrying can achieve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// The same payload may well be accepted later: a network error, a 5xx, a
    /// 408/429, or any other status that is not a client error.
    Transient,
    /// The target rejected the payload itself (4xx). Retrying quickly cannot
    /// help, so the webhook waits [`ForwardBackoff::max`] between attempts
    /// rather than being dropped — fixing the target still drains it.
    Rejected,
}

/// Classifies an unexpected response status. Only called when the status differs
/// from the channel's `expected-status`.
pub fn classify_status(status: u16) -> FailureKind {
    // 408 and 429 are 4xx by number only: both explicitly invite a later retry.
    if status == 408 || status == 429 {
        return FailureKind::Transient;
    }

    if (400..500).contains(&status) {
        return FailureKind::Rejected;
    }

    FailureKind::Transient
}

/// Parses a `Retry-After` header in its delta-seconds form.
///
/// The HTTP-date form is intentionally not supported: it needs a date parser and
/// a trustworthy local clock, and targets that ask webhook senders to slow down
/// (429/503 from proxies and rate limiters) send seconds. An unparsable value
/// falls back to the computed backoff, which is never worse than ignoring the
/// response entirely.
pub fn parse_retry_after(value: &str) -> Option<Duration> {
    let seconds: u64 = value.trim().parse().ok()?;

    Some(Duration::from_secs(seconds))
}

/// Delay after `attempts` consecutive failures, before jitter.
///
/// `attempts` counts the failure that just happened, so the first failure gets
/// [`ForwardBackoff::base`].
fn delay_for_attempt(backoff: &ForwardBackoff, attempts: i64) -> Duration {
    // Clamped because `multiplier.powf` on a large exponent reaches `inf`, and an
    // infinite `Duration::from_secs_f64` panics.
    let exponent = attempts.saturating_sub(1).clamp(0, 64) as f64;
    let seconds = backoff.base.as_secs_f64() * backoff.multiplier.powf(exponent);

    if !seconds.is_finite() || seconds >= backoff.max.as_secs_f64() {
        return backoff.max;
    }

    Duration::from_secs_f64(seconds)
}

/// Spreads `delay` by ±[`ForwardBackoff::jitter`], where `unit` is a random value
/// in `0.0..1.0` supplied by the caller.
fn apply_jitter(backoff: &ForwardBackoff, delay: Duration, unit: f64) -> Duration {
    if backoff.jitter <= 0.0 {
        return delay;
    }

    let offset = backoff.jitter * (2.0 * unit.clamp(0.0, 1.0) - 1.0);
    let seconds = delay.as_secs_f64() * (1.0 + offset);

    if !seconds.is_finite() {
        return delay;
    }

    Duration::from_secs_f64(seconds.max(0.0)).min(backoff.max)
}

/// Jitter input taken from the sub-second part of the system clock.
///
/// Jitter only has to stop channels and replicas from retrying in lockstep, so a
/// random number generator dependency would buy nothing. Callers that need a
/// deterministic value pass their own into [`next_delay`] instead.
pub fn clock_jitter_unit() -> f64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.subsec_nanos())
        .unwrap_or(0);

    f64::from(nanos) / f64::from(1_000_000_000u32)
}

/// How long a failed webhook waits before the next delivery attempt.
///
/// `attempts` includes the failure that just happened, `retry_after` is what the
/// target asked for (if anything), and `jitter_unit` is a random value in
/// `0.0..1.0`.
pub fn next_delay(
    backoff: &ForwardBackoff,
    kind: FailureKind,
    attempts: i64,
    retry_after: Option<Duration>,
    jitter_unit: f64,
) -> Duration {
    // The target named a time, so honour it rather than guessing — but never wait
    // longer than the ceiling, otherwise a hostile or buggy header could park a
    // webhook indefinitely. No jitter: the point is to obey the request.
    if let Some(requested) = retry_after {
        return requested.min(backoff.max);
    }

    // Growing from the base would mean hammering a target that already said no.
    // Jitter is pointless here — everything is pinned to the same ceiling anyway.
    if kind == FailureKind::Rejected {
        return backoff.max;
    }

    apply_jitter(backoff, delay_for_attempt(backoff, attempts), jitter_unit)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backoff() -> ForwardBackoff {
        ForwardBackoff {
            base: Duration::from_secs(30),
            multiplier: 2.0,
            max: Duration::from_secs(3600),
            jitter: 0.0,
        }
    }

    #[test]
    fn first_failure_waits_one_interval() {
        // Anyone upgrading gets the old cadence for the first retry; only a
        // repeatedly failing target sees longer waits.
        assert_eq!(delay_for_attempt(&backoff(), 1), Duration::from_secs(30));
    }

    #[test]
    fn delay_doubles_per_attempt() {
        let cfg = backoff();

        assert_eq!(delay_for_attempt(&cfg, 2), Duration::from_secs(60));
        assert_eq!(delay_for_attempt(&cfg, 3), Duration::from_secs(120));
        assert_eq!(delay_for_attempt(&cfg, 4), Duration::from_secs(240));
    }

    #[test]
    fn delay_is_capped_at_max() {
        let cfg = backoff();

        assert_eq!(delay_for_attempt(&cfg, 100), cfg.max);
    }

    /// `2.0f64.powf(large)` is `inf`, and `Duration::from_secs_f64(inf)` panics.
    #[test]
    fn a_huge_attempt_count_stays_at_max() {
        let cfg = backoff();

        assert_eq!(delay_for_attempt(&cfg, i64::MAX), cfg.max);
    }

    #[test]
    fn a_zero_attempt_count_is_treated_as_the_first_failure() {
        // The counter is read back from the database, so a row written by an
        // older version (or a lost increment) must not produce a nonsense delay.
        assert_eq!(delay_for_attempt(&backoff(), 0), Duration::from_secs(30));
    }

    #[test]
    fn multiplier_of_one_keeps_a_fixed_interval() {
        let cfg = ForwardBackoff {
            multiplier: 1.0,
            ..backoff()
        };

        assert_eq!(delay_for_attempt(&cfg, 1), Duration::from_secs(30));
        assert_eq!(delay_for_attempt(&cfg, 9), Duration::from_secs(30));
    }

    #[test]
    fn jitter_spans_the_configured_fraction() {
        let cfg = ForwardBackoff {
            jitter: 0.2,
            ..backoff()
        };
        let delay = Duration::from_secs(100);

        assert_eq!(apply_jitter(&cfg, delay, 0.0), Duration::from_secs(80));
        assert_eq!(apply_jitter(&cfg, delay, 0.5), Duration::from_secs(100));
        assert_eq!(apply_jitter(&cfg, delay, 1.0), Duration::from_secs(120));
    }

    #[test]
    fn jitter_never_pushes_past_max() {
        let cfg = ForwardBackoff {
            jitter: 0.5,
            ..backoff()
        };

        assert_eq!(apply_jitter(&cfg, cfg.max, 1.0), cfg.max);
    }

    #[test]
    fn jitter_of_zero_leaves_the_delay_alone() {
        let cfg = backoff();

        assert_eq!(apply_jitter(&cfg, cfg.base, 0.0), cfg.base);
        assert_eq!(apply_jitter(&cfg, cfg.base, 1.0), cfg.base);
    }

    #[test]
    fn client_errors_are_rejections() {
        for status in [400, 403, 404, 422] {
            assert_eq!(classify_status(status), FailureKind::Rejected, "{status}");
        }
    }

    #[test]
    fn retry_inviting_statuses_are_transient() {
        // Both are 4xx but explicitly mean "later", so they must not be parked.
        for status in [408, 429] {
            assert_eq!(classify_status(status), FailureKind::Transient, "{status}");
        }
    }

    #[test]
    fn server_errors_are_transient() {
        for status in [500, 502, 503, 504] {
            assert_eq!(classify_status(status), FailureKind::Transient, "{status}");
        }
    }

    /// A target answering 200 where 204 was configured is a mismatch, not a
    /// rejection: it is usually the proxy's own config that is wrong.
    #[test]
    fn unexpected_success_statuses_are_transient() {
        for status in [200, 201, 302] {
            assert_eq!(classify_status(status), FailureKind::Transient, "{status}");
        }
    }

    #[test]
    fn rejections_wait_the_maximum_delay() {
        let cfg = backoff();

        assert_eq!(
            next_delay(&cfg, FailureKind::Rejected, 1, None, 0.5),
            cfg.max,
            "a rejected payload must not be retried at full speed"
        );
    }

    #[test]
    fn retry_after_wins_over_the_computed_delay() {
        let cfg = backoff();

        assert_eq!(
            next_delay(
                &cfg,
                FailureKind::Transient,
                5,
                Some(Duration::from_secs(90)),
                0.5
            ),
            Duration::from_secs(90)
        );
    }

    #[test]
    fn retry_after_is_capped_at_max() {
        let cfg = backoff();

        assert_eq!(
            next_delay(
                &cfg,
                FailureKind::Transient,
                1,
                Some(Duration::from_secs(86_400)),
                0.5
            ),
            cfg.max,
            "a target must not be able to park a webhook for a day"
        );
    }

    /// `next_delay` reads its jitter input as a fraction, so a value outside
    /// `0.0..1.0` would silently skew every delay.
    #[test]
    fn clock_jitter_unit_stays_within_its_range() {
        for _ in 0..1_000 {
            let unit = clock_jitter_unit();
            assert!(
                (0.0..1.0).contains(&unit),
                "jitter unit out of range: {unit}"
            );
        }
    }

    #[test]
    fn parses_delta_seconds() {
        assert_eq!(parse_retry_after("120"), Some(Duration::from_secs(120)));
        assert_eq!(parse_retry_after("  5 "), Some(Duration::from_secs(5)));
        assert_eq!(parse_retry_after("0"), Some(Duration::ZERO));
    }

    #[test]
    fn rejects_unsupported_retry_after_forms() {
        assert_eq!(parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT"), None);
        assert_eq!(parse_retry_after(""), None);
        assert_eq!(parse_retry_after("-5"), None);
        assert_eq!(parse_retry_after("1.5"), None);
    }
}
