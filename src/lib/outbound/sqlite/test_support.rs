//! Helpers shared by the SQLite adapter's lock-contention tests.

use std::time::Duration;

use sqlx::{Connection, Executor, SqliteConnection};
use tempfile::TempDir;

use super::init::SqliteTuning;
use super::retry::LockRetryPolicy;

/// A throwaway on-disk database.
///
/// Lock behaviour only exists for on-disk databases: `sqlite::memory:` reports
/// `journal_mode = memory` and never takes a file lock, so contention tests have
/// to run against a real file. The returned [`TempDir`] must stay alive for as
/// long as the database is used.
pub(crate) fn temp_db() -> (TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}/kwp.db?mode=rwc", dir.path().display());
    (dir, url)
}

/// Production tuning with the waits shortened, so contention tests finish in
/// milliseconds instead of seconds.
pub(crate) fn fast_tuning() -> SqliteTuning {
    SqliteTuning {
        busy_timeout: Duration::from_millis(50),
        retry: LockRetryPolicy {
            max_attempts: 8,
            initial_backoff: Duration::from_millis(10),
            max_backoff: Duration::from_millis(40),
            budget: Duration::from_secs(5),
        },
        ..SqliteTuning::default()
    }
}

/// Tuning that gives up almost immediately — used to prove that an operation
/// *would* have failed without retries.
pub(crate) fn no_retry_tuning() -> SqliteTuning {
    SqliteTuning {
        busy_timeout: Duration::from_millis(50),
        retry: LockRetryPolicy {
            max_attempts: 1,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(1),
            budget: Duration::from_millis(100),
        },
        ..SqliteTuning::default()
    }
}

/// Holds SQLite's write lock on `url` until released or dropped, which is what
/// makes every other writer see `SQLITE_BUSY`.
pub(crate) struct WriteLock {
    conn: SqliteConnection,
}

impl WriteLock {
    pub(crate) async fn acquire(url: &str) -> Self {
        let mut conn = SqliteConnection::connect(url).await.unwrap();
        conn.execute("BEGIN EXCLUSIVE").await.unwrap();
        Self { conn }
    }

    pub(crate) async fn release(mut self) {
        self.conn.execute("ROLLBACK").await.unwrap();
    }

    /// Releases the lock after `delay`, so a retrying caller can observe the
    /// transition from locked to available.
    pub(crate) fn release_after(self, delay: Duration) {
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            self.release().await;
        });
    }
}
