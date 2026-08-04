use std::collections::HashSet;
use std::future::Future;
use std::str::FromStr;
use std::time::Duration;

use anyhow::Context;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Row, SqlitePool};

use super::retry::{LockRetryPolicy, retry_on_locked};

/// Columns added after the initial release. Applied on startup for databases
/// created by an earlier version.
const ADDED_COLUMNS: [(&str, &str); 3] = [
    ("forward_attempts", "INTEGER NOT NULL DEFAULT 0"),
    ("last_attempt_at", "INTEGER"),
    ("last_attempt_error", "TEXT"),
];

/// Connection settings for the SQLite adapter.
///
/// SQLite permits exactly one writer at a time, so all of these exist to keep
/// write transactions short and lock collisions survivable. They are explicit
/// rather than inherited from sqlx so that the behaviour is pinned by tests and
/// cannot drift with a dependency bump.
#[derive(Debug, Clone)]
pub struct SqliteTuning {
    /// How long SQLite itself waits for the write lock before returning
    /// `SQLITE_BUSY`. Kept short on purpose: [`SqliteTuning::retry`] retries on
    /// top of it, and a long timeout would just pin a pool connection.
    pub busy_timeout: Duration,
    /// Writes serialise regardless of pool size, so a large pool only adds
    /// waiters (and WAL readers that hold back checkpoints).
    pub max_connections: u32,
    /// Keeping one connection alive means the `journal_mode = WAL` switch happens
    /// once at startup instead of on every reconnect.
    pub min_connections: u32,
    /// How long a caller waits for a free pool connection. sqlx defaults to 30s,
    /// which outlives any webhook sender's own timeout.
    pub acquire_timeout: Duration,
    /// `NORMAL` is the recommended companion to WAL: it drops the per-commit
    /// fsync, which is what keeps the write lock held. An OS-level crash or power
    /// loss can then lose the most recent commits (the database itself stays
    /// intact); an application crash cannot.
    pub synchronous: SqliteSynchronous,
    /// Retry behaviour for statements that hit the write lock anyway.
    pub retry: LockRetryPolicy,
}

impl Default for SqliteTuning {
    fn default() -> Self {
        // These add up: the retry budget only decides whether *another* attempt is
        // started, so the slowest possible call is roughly
        // `retry.budget + acquire_timeout + busy_timeout`. That sum has to stay
        // under the timeout webhook senders apply — see
        // `worst_case_latency_stays_below_a_typical_sender_timeout`.
        Self {
            busy_timeout: Duration::from_secs(2),
            max_connections: 8,
            min_connections: 1,
            acquire_timeout: Duration::from_secs(3),
            synchronous: SqliteSynchronous::Normal,
            retry: LockRetryPolicy::default(),
        }
    }
}

/// SQLite reports a re-added column as a generic `SQLITE_ERROR`, so the message is
/// the only thing that distinguishes it. Covered by
/// `adding_an_existing_column_is_tolerated`, which fails if the wording changes.
fn is_duplicate_column(error: &sqlx::Error) -> bool {
    error.to_string().contains("duplicate column")
}

#[derive(Debug, Clone)]
pub struct Sqlite {
    pool: SqlitePool,
    retry: LockRetryPolicy,
}

impl Sqlite {
    pub fn get_pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn new(path: &str) -> Result<Sqlite, anyhow::Error> {
        Self::new_with_tuning(path, SqliteTuning::default()).await
    }

    pub async fn new_with_tuning(
        path: &str,
        tuning: SqliteTuning,
    ) -> Result<Sqlite, anyhow::Error> {
        let options = SqliteConnectOptions::from_str(path)
            .with_context(|| format!("invalid database path {}", path))?
            .pragma("foreign_keys", "ON")
            .pragma("journal_mode", "WAL")
            .synchronous(tuning.synchronous)
            .busy_timeout(tuning.busy_timeout);

        // Switching a database into WAL needs a brief exclusive lock that
        // `busy_timeout` cannot wait on, so two processes opening the same fresh
        // file can collide here. Retrying is the only cure.
        let pool = retry_on_locked(&tuning.retry, "connect", || {
            SqlitePoolOptions::new()
                .max_connections(tuning.max_connections)
                .min_connections(tuning.min_connections)
                .acquire_timeout(tuning.acquire_timeout)
                .connect_with(options.clone())
        })
        .await
        .with_context(|| format!("failed to open database at {}", path))?;

        let db = Sqlite {
            pool,
            retry: tuning.retry,
        };

        db.migrate().await?;

        Ok(db)
    }

    /// Retries `attempt` while the database is locked, then hands the error back
    /// unchanged for the caller to classify.
    pub(crate) async fn with_lock_retry<T, F, Fut>(
        &self,
        operation: &str,
        attempt: F,
    ) -> Result<T, sqlx::Error>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, sqlx::Error>>,
    {
        retry_on_locked(&self.retry, operation, attempt).await
    }

    async fn migrate(&self) -> Result<(), anyhow::Error> {
        self.with_lock_retry("create schema", || {
            sqlx::query(
                r#"
            CREATE TABLE IF NOT EXISTS webhooks (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                channel     TEXT NOT NULL,
                headers     TEXT NOT NULL DEFAULT '{}',
                payload     BLOB NOT NULL,
                received_at INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_webhooks_channel ON webhooks(channel);
            CREATE INDEX IF NOT EXISTS idx_webhooks_received_at ON webhooks(received_at);
            "#,
            )
            .execute(self.get_pool())
        })
        .await
        .context("failed to create webhooks schema")?;

        // Checking first means a restart of an already-migrated database issues no
        // write at all, so it cannot fail on a lock held by the instance being
        // replaced during a rolling deploy.
        let existing = self.existing_columns().await?;

        for (name, definition) in ADDED_COLUMNS {
            if existing.contains(name) {
                continue;
            }

            self.add_column(name, definition).await?;
        }

        Ok(())
    }

    /// Adds a column, treating "it is already there" as success.
    ///
    /// The caller skips columns it already saw in `PRAGMA table_info`, so this only
    /// matters when another instance added the same column in between — which is
    /// exactly the rolling-deploy case, and must not stop startup.
    async fn add_column(&self, name: &str, definition: &str) -> Result<(), anyhow::Error> {
        // Built outside the closure: `sqlx::query` borrows the statement, so a
        // temporary created inside would not outlive the returned future.
        let statement = format!("ALTER TABLE webhooks ADD COLUMN {name} {definition}");

        let result = self
            .with_lock_retry("add column", || {
                sqlx::query(&statement).execute(self.get_pool())
            })
            .await;

        match result {
            Ok(_) => {
                log::info!("migrated webhooks table: added column {name}");
                Ok(())
            }
            Err(e) if is_duplicate_column(&e) => {
                log::info!("column {name} was already added by another instance");
                Ok(())
            }
            Err(e) => {
                Err(anyhow::Error::from(e)
                    .context(format!("failed to add column {name} to webhooks")))
            }
        }
    }

    async fn existing_columns(&self) -> Result<HashSet<String>, anyhow::Error> {
        let rows = self
            .with_lock_retry("read table info", || {
                sqlx::query("PRAGMA table_info(webhooks)").fetch_all(self.get_pool())
            })
            .await
            .context("failed to read webhooks table info")?;

        let mut columns = HashSet::with_capacity(rows.len());

        for row in &rows {
            let name: String = row
                .try_get("name")
                .context("unexpected PRAGMA table_info layout")?;
            columns.insert(name);
        }

        Ok(columns)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::outbound::sqlite::test_support::{WriteLock, fast_tuning, temp_db};
    use sqlx::{Connection, Executor, SqliteConnection};
    use std::time::Instant;

    #[tokio::test]
    async fn connection_settings_are_pinned() {
        let (_dir, url) = temp_db();
        let db = Sqlite::new(&url).await.unwrap();
        let pool = db.get_pool();

        let journal_mode: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(journal_mode, "wal", "readers must not block on the writer");

        // 1 == NORMAL. FULL (2) fsyncs on every commit and lengthens the window
        // during which the write lock is held.
        let synchronous: i64 = sqlx::query_scalar("PRAGMA synchronous")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(synchronous, 1);

        let busy_timeout: i64 = sqlx::query_scalar("PRAGMA busy_timeout")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(busy_timeout, 2_000);

        assert_eq!(pool.options().get_max_connections(), 8);
        assert_eq!(pool.options().get_min_connections(), 1);
        assert_eq!(
            pool.options().get_acquire_timeout(),
            Duration::from_secs(3),
            "sqlx defaults to 30s, which outlives every webhook sender's timeout"
        );
    }

    /// The retry budget only gates whether a *further* attempt is started, so the
    /// attempt already in flight can still spend `acquire_timeout + busy_timeout`
    /// on top of it. All three have to fit inside a sender's patience.
    #[test]
    fn worst_case_latency_stays_below_a_typical_sender_timeout() {
        let tuning = SqliteTuning::default();
        let worst_case = tuning.retry.budget + tuning.acquire_timeout + tuning.busy_timeout;

        assert!(
            worst_case < Duration::from_secs(10),
            "a receive must fail in time for the sender to redeliver, got {worst_case:?}"
        );
    }

    #[tokio::test]
    async fn schema_is_created_with_all_columns() {
        let (_dir, url) = temp_db();
        let db = Sqlite::new(&url).await.unwrap();

        let columns = db.existing_columns().await.unwrap();

        for (name, _) in ADDED_COLUMNS {
            assert!(columns.contains(name), "missing column {name}");
        }
        assert!(columns.contains("id"));
        assert!(columns.contains("payload"));
    }

    #[tokio::test]
    async fn reopening_an_already_migrated_database_issues_no_writes() {
        let (_dir, url) = temp_db();
        Sqlite::new(&url).await.unwrap();

        // Every ALTER TABLE is skipped because the columns already exist, so a
        // startup that races another writer must not need the write lock at all.
        let lock = WriteLock::acquire(&url).await;

        let started = Instant::now();
        let reopened = Sqlite::new_with_tuning(&url, fast_tuning()).await;
        let elapsed = started.elapsed();

        assert!(
            reopened.is_ok(),
            "startup must survive a concurrent writer: {reopened:?}"
        );
        assert!(
            elapsed < Duration::from_secs(1),
            "startup must not have waited on the write lock, took {elapsed:?}"
        );

        lock.release().await;
    }

    /// A database written by an earlier version: the table exists but the columns
    /// added later do not, so startup genuinely has to write.
    async fn create_legacy_database(url: &str) {
        let mut conn = SqliteConnection::connect(url).await.unwrap();
        conn.execute(
            "CREATE TABLE webhooks (
                    id          INTEGER PRIMARY KEY AUTOINCREMENT,
                    channel     TEXT NOT NULL,
                    headers     TEXT NOT NULL DEFAULT '{}',
                    payload     BLOB NOT NULL,
                    received_at INTEGER NOT NULL
                )",
        )
        .await
        .unwrap();
        conn.close().await.unwrap();
    }

    #[tokio::test]
    async fn migration_recovers_when_the_lock_is_released() {
        let (_dir, url) = temp_db();
        create_legacy_database(&url).await;

        WriteLock::acquire(&url)
            .await
            .release_after(Duration::from_millis(150));

        let db = Sqlite::new_with_tuning(&url, fast_tuning()).await.unwrap();
        let columns = db.existing_columns().await.unwrap();

        for (name, _) in ADDED_COLUMNS {
            assert!(columns.contains(name), "missing column {name}");
        }
    }

    #[tokio::test]
    async fn migration_is_idempotent_across_restarts() {
        let (_dir, url) = temp_db();

        for _ in 0..3 {
            Sqlite::new(&url).await.unwrap();
        }

        let db = Sqlite::new(&url).await.unwrap();
        let columns = db.existing_columns().await.unwrap();

        assert_eq!(
            columns.len(),
            5 + ADDED_COLUMNS.len(),
            "restarts must not duplicate or add columns"
        );
    }

    /// The rolling-deploy case, made deterministic: a second instance decided to
    /// add a column, then lost the race. It must carry on rather than refuse to
    /// start. Also pins the error wording `is_duplicate_column` relies on.
    #[tokio::test]
    async fn adding_an_existing_column_is_tolerated() {
        let (_dir, url) = temp_db();
        let db = Sqlite::new(&url).await.unwrap();

        let (name, definition) = ADDED_COLUMNS[0];

        // The column is already there after startup, so this is the exact
        // statement a racing instance would issue.
        db.add_column(name, definition)
            .await
            .expect("a column added by another instance must not fail startup");

        assert_eq!(
            db.existing_columns().await.unwrap().len(),
            5 + ADDED_COLUMNS.len(),
            "tolerating the error must not change the schema"
        );
    }

    /// Several instances upgrading the same legacy database at once must all come
    /// up with a correct schema.
    ///
    /// Uses a multi-threaded runtime so the startups genuinely overlap rather than
    /// interleaving only at await points.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_migration_of_a_legacy_database_succeeds() {
        let (_dir, url) = temp_db();
        create_legacy_database(&url).await;

        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..8 {
            let url = url.clone();
            tasks.spawn(async move { Sqlite::new(&url).await.map(|_| ()) });
        }

        let mut failures = vec![];
        while let Some(result) = tasks.join_next().await {
            if let Err(e) = result.unwrap() {
                failures.push(format!("{e:#}"));
            }
        }

        assert!(
            failures.is_empty(),
            "concurrent migration failed: {failures:?}"
        );

        let db = Sqlite::new(&url).await.unwrap();
        let columns = db.existing_columns().await.unwrap();
        assert_eq!(
            columns.len(),
            5 + ADDED_COLUMNS.len(),
            "a racing migration must not add a column twice"
        );
    }

    /// Two processes opening the same brand-new database both try to switch it
    /// into WAL, which needs an exclusive lock `busy_timeout` cannot wait on.
    #[tokio::test]
    async fn concurrent_first_startup_on_a_fresh_database_succeeds() {
        let (_dir, url) = temp_db();

        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..16 {
            let url = url.clone();
            tasks.spawn(async move { Sqlite::new(&url).await.map(|_| ()) });
        }

        let mut failures = vec![];
        while let Some(result) = tasks.join_next().await {
            if let Err(e) = result.unwrap() {
                failures.push(format!("{e:#}"));
            }
        }

        assert!(
            failures.is_empty(),
            "concurrent startup on a fresh database failed: {failures:?}"
        );
    }

    #[tokio::test]
    async fn invalid_path_is_reported_with_context() {
        let error = Sqlite::new("not-a-sqlite-url://x").await.unwrap_err();

        assert!(
            format!("{error:#}").contains("not-a-sqlite-url://x"),
            "error must name the offending path: {error:#}"
        );
    }
}
