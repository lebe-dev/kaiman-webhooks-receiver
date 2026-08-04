//! Lock-contention handling for the SQLite adapter.
//!
//! SQLite serialises writers: at any moment a single connection may hold the
//! write lock, and everyone else gets `SQLITE_BUSY` / `SQLITE_LOCKED`.
//! `sqlite3_busy_timeout` already absorbs most collisions, but it is not a
//! complete answer:
//!
//! * it gives up after a fixed wall-clock budget and then the error reaches the
//!   caller, which for this service means a webhook is answered with an error and
//!   possibly never redelivered;
//! * it is not invoked at all for some conditions — `SQLITE_BUSY_SNAPSHOT` and
//!   `SQLITE_LOCKED_SHAREDCACHE` bypass the busy handler entirely.
//!
//! So every statement is additionally retried here, with exponential backoff and
//! a hard wall-clock budget so a stuck lock can never turn into an unbounded hang.

use std::future::Future;
use std::time::{Duration, Instant};

/// Primary SQLite result codes that mean "another connection holds the lock".
const SQLITE_BUSY: i32 = 5;
const SQLITE_LOCKED: i32 = 6;

/// How aggressively a statement that lost the race for the write lock is retried.
#[derive(Debug, Clone)]
pub struct LockRetryPolicy {
    /// Total number of attempts, including the first one.
    pub max_attempts: u32,
    /// Delay before the second attempt; doubles on every further attempt.
    pub initial_backoff: Duration,
    /// Upper bound for a single backoff delay.
    pub max_backoff: Duration,
    /// Wall-clock ceiling for the whole retry sequence. No further attempt is
    /// started once it would exceed this, so callers keep a predictable latency.
    pub budget: Duration,
}

impl Default for LockRetryPolicy {
    fn default() -> Self {
        // The budget is deliberately smaller than the timeout webhook senders
        // usually apply (GitHub gives up after 10s): failing with 503 in time for
        // the sender to redeliver beats holding the connection until it times out.
        Self {
            max_attempts: 5,
            initial_backoff: Duration::from_millis(25),
            max_backoff: Duration::from_millis(400),
            budget: Duration::from_secs(4),
        }
    }
}

impl LockRetryPolicy {
    /// Backoff before the attempt that follows `attempt` (1-based).
    fn backoff_after(&self, attempt: u32) -> Duration {
        let factor = 1u32
            .checked_shl(attempt.saturating_sub(1).min(16))
            .unwrap_or(u32::MAX);
        let delay = self
            .initial_backoff
            .saturating_mul(factor)
            .min(self.max_backoff);
        delay + jitter(delay)
    }
}

/// Spreads simultaneous waiters out so they do not all wake into the same lock.
/// The quality of the randomness is irrelevant here — only that two waiters
/// starting at different nanoseconds get different delays — so this avoids
/// pulling in an RNG dependency.
fn jitter(base: Duration) -> Duration {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.subsec_nanos())
        .unwrap_or(0);

    base.mul_f64(f64::from(nanos % 1_000) / 2_000.0)
}

/// `true` if `extended_code` denotes lock contention.
///
/// sqlx surfaces `sqlite3_extended_errcode`, so values such as 517
/// (`BUSY_SNAPSHOT`), 261 (`BUSY_RECOVERY`), 773 (`BUSY_TIMEOUT`) and 262
/// (`LOCKED_SHAREDCACHE`) all show up here. The primary code is the low byte.
fn is_lock_code(extended_code: i32) -> bool {
    matches!(extended_code & 0xff, SQLITE_BUSY | SQLITE_LOCKED)
}

/// The extended result code of a database error, if it has one.
fn extended_code(error: &sqlx::Error) -> Option<i32> {
    let sqlx::Error::Database(db_error) = error else {
        return None;
    };

    db_error.code()?.parse::<i32>().ok()
}

/// `true` when retrying the statement has a chance of succeeding: some other
/// connection holds the lock right now.
///
/// Every statement in this adapter is a single self-contained statement, so a
/// lock error means nothing was committed and a retry cannot duplicate a write.
pub fn is_retryable_lock(error: &sqlx::Error) -> bool {
    extended_code(error).is_some_and(is_lock_code)
}

/// `true` when the operation failed because the database (or the pool in front
/// of it) was too contended to serve it. Distinct from [`is_retryable_lock`]:
/// pool exhaustion is not worth retrying at this layer, but it is still a
/// "come back later" condition rather than a genuine failure.
pub fn is_lock_contention(error: &sqlx::Error) -> bool {
    is_retryable_lock(error) || matches!(error, sqlx::Error::PoolTimedOut)
}

/// Runs `attempt` until it succeeds, fails with something other than a lock
/// error, or runs out of attempts / budget.
///
/// `attempt` is a closure rather than a future because a future cannot be polled
/// twice — each retry has to build a fresh query.
pub async fn retry_on_locked<T, F, Fut>(
    policy: &LockRetryPolicy,
    operation: &str,
    mut attempt: F,
) -> Result<T, sqlx::Error>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, sqlx::Error>>,
{
    let started = Instant::now();
    let mut attempt_number = 1;

    loop {
        let error = match attempt().await {
            Ok(value) => {
                if attempt_number > 1 {
                    log::info!(
                        "sqlite {operation} succeeded on attempt {attempt_number} after lock contention"
                    );
                }
                return Ok(value);
            }
            Err(error) => error,
        };

        if !is_retryable_lock(&error) {
            return Err(error);
        }

        if attempt_number >= policy.max_attempts {
            log::error!(
                "sqlite {operation} gave up after {attempt_number} attempts, database still locked: {error}"
            );
            return Err(error);
        }

        let backoff = policy.backoff_after(attempt_number);

        if started.elapsed() + backoff >= policy.budget {
            log::error!(
                "sqlite {operation} gave up after {:?} (retry budget {:?}), database still locked: {error}",
                started.elapsed(),
                policy.budget
            );
            return Err(error);
        }

        log::warn!(
            "sqlite {operation} is locked (attempt {attempt_number}/{}), retrying in {backoff:?}: {error}",
            policy.max_attempts
        );

        tokio::time::sleep(backoff).await;
        attempt_number += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fast_policy() -> LockRetryPolicy {
        LockRetryPolicy {
            max_attempts: 4,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(4),
            budget: Duration::from_secs(5),
        }
    }

    /// A genuine `sqlx::Error::Database` that is *not* lock contention, so the
    /// classifier is exercised against real sqlx/SQLite plumbing rather than a
    /// hand-rolled stub. Lock errors cannot be raised from SQL on demand; they are
    /// covered end-to-end in the adapter tests, which lock an actual database file.
    async fn non_lock_database_error() -> sqlx::Error {
        use sqlx::{Connection, Executor, SqliteConnection};

        let mut conn = SqliteConnection::connect("sqlite::memory:").await.unwrap();

        conn.execute("SELECT this_function_does_not_exist()")
            .await
            .unwrap_err()
    }

    #[test]
    fn every_busy_and_locked_extended_code_is_recognised() {
        // Extended codes SQLite can report for contention, from sqlite3.h.
        for code in [
            5,   // SQLITE_BUSY
            6,   // SQLITE_LOCKED
            261, // SQLITE_BUSY_RECOVERY
            517, // SQLITE_BUSY_SNAPSHOT
            773, // SQLITE_BUSY_TIMEOUT
            262, // SQLITE_LOCKED_SHAREDCACHE
            518, // SQLITE_LOCKED_VTAB
        ] {
            assert!(is_lock_code(code), "code {code} must count as contention");
        }
    }

    #[test]
    fn unrelated_extended_codes_are_not_mistaken_for_locks_by_masking() {
        for code in [
            1,    // SQLITE_ERROR
            8,    // SQLITE_READONLY
            11,   // SQLITE_CORRUPT
            19,   // SQLITE_CONSTRAINT
            1299, // SQLITE_CONSTRAINT_NOTNULL
            2579, // SQLITE_CONSTRAINT_UNIQUE
            1288, // SQLITE_READONLY_ROLLBACK — low byte 8, must not match
        ] {
            assert!(
                !is_lock_code(code),
                "code {code} must not count as contention"
            );
        }
    }

    #[test]
    fn backoff_grows_and_is_capped() {
        let policy = LockRetryPolicy {
            max_attempts: 10,
            initial_backoff: Duration::from_millis(10),
            max_backoff: Duration::from_millis(50),
            budget: Duration::from_secs(30),
        };

        // Jitter adds at most 50%, so compare against the un-jittered bounds.
        assert!(policy.backoff_after(1) >= Duration::from_millis(10));
        assert!(policy.backoff_after(1) < Duration::from_millis(16));
        assert!(policy.backoff_after(2) >= Duration::from_millis(20));
        assert!(policy.backoff_after(3) >= Duration::from_millis(40));
        // Capped, plus jitter on top of the cap.
        assert!(policy.backoff_after(9) < Duration::from_millis(76));
    }

    #[test]
    fn backoff_does_not_overflow_on_large_attempt_numbers() {
        let policy = LockRetryPolicy::default();

        assert!(policy.backoff_after(u32::MAX) <= policy.max_backoff + policy.max_backoff);
    }

    #[tokio::test]
    async fn non_lock_errors_are_returned_on_the_first_attempt() {
        let mut calls = 0;

        let result: Result<(), sqlx::Error> = retry_on_locked(&fast_policy(), "test", || {
            calls += 1;
            async { Err(non_lock_database_error().await) }
        })
        .await;

        assert!(result.is_err());
        assert_eq!(calls, 1, "a syntax/constraint error must not be retried");
    }

    #[tokio::test]
    async fn success_is_returned_without_retrying() {
        let mut calls = 0;

        let result = retry_on_locked(&fast_policy(), "test", || {
            calls += 1;
            async { Ok::<_, sqlx::Error>(42) }
        })
        .await;

        assert_eq!(result.unwrap(), 42);
        assert_eq!(calls, 1);
    }

    #[test]
    fn pool_timeout_is_contention_but_not_retryable() {
        assert!(is_lock_contention(&sqlx::Error::PoolTimedOut));
        assert!(!is_retryable_lock(&sqlx::Error::PoolTimedOut));
    }

    #[tokio::test]
    async fn unrelated_errors_are_neither_locks_nor_contention() {
        let error = non_lock_database_error().await;

        assert!(!is_retryable_lock(&error));
        assert!(!is_lock_contention(&error));
    }

    #[test]
    fn row_not_found_is_not_treated_as_contention() {
        assert!(!is_lock_contention(&sqlx::Error::RowNotFound));
    }
}
