use std::collections::HashMap;

use bytes::Bytes;

use crate::domain::webhook::model::{Webhook, WebhookChannel, WebhookRepositoryError};
use crate::domain::webhook::ports::WebhookRepository;

use super::init::Sqlite;
use super::retry::is_lock_contention;
use sqlx::Row;

/// Columns selected wherever a full [`Webhook`] is read back.
const WEBHOOK_COLUMNS: &str = "id, channel, headers, payload, received_at, forward_attempts, last_attempt_at, last_attempt_error";

fn parse_webhook_row(row: &sqlx::sqlite::SqliteRow) -> Option<Webhook> {
    let id: i64 = row.try_get("id").ok()?;
    let channel: String = row.try_get("channel").ok()?;
    let headers_str: String = row.try_get("headers").ok()?;
    let payload: Vec<u8> = row.try_get("payload").ok()?;
    let received_at: i64 = row.try_get("received_at").ok()?;
    let forward_attempts: i64 = row.try_get("forward_attempts").ok()?;
    let last_attempt_at: Option<i64> = row.try_get("last_attempt_at").ok()?;
    let last_attempt_error: Option<String> = row.try_get("last_attempt_error").ok()?;

    let headers: HashMap<String, String> = serde_json::from_str(&headers_str).unwrap_or_default();

    Some(Webhook {
        id: Some(id),
        channel: WebhookChannel::new(channel),
        headers,
        payload: Bytes::from(payload),
        received_at,
        forward_attempts,
        last_attempt_at,
        last_attempt_error,
    })
}

/// Separates "the database was too busy, ask again" from a genuine failure, so
/// callers can answer 503 instead of 500 and the sender redelivers.
fn to_repository_error(error: sqlx::Error) -> WebhookRepositoryError {
    if is_lock_contention(&error) {
        return WebhookRepositoryError::Busy(error.into());
    }

    WebhookRepositoryError::Other(error.into())
}

impl WebhookRepository for Sqlite {
    async fn insert(&self, webhook: &Webhook) -> Result<(), WebhookRepositoryError> {
        let headers_json =
            serde_json::to_string(&webhook.headers).unwrap_or_else(|_| "{}".to_string());

        self.with_lock_retry("insert", || {
            sqlx::query(
                "INSERT INTO webhooks (channel, headers, payload, received_at) VALUES (?, ?, ?, ?)",
            )
            .bind(webhook.channel.as_str())
            .bind(headers_json.as_str())
            .bind(webhook.payload.as_ref())
            .bind(webhook.received_at)
            .execute(self.get_pool())
        })
        .await
        .map_err(to_repository_error)?;

        Ok(())
    }

    async fn read_and_delete_by_channel(
        &self,
        channel: &WebhookChannel,
        limit: i64,
    ) -> Result<Vec<Webhook>, WebhookRepositoryError> {
        let statement = format!(
            "DELETE FROM webhooks WHERE id IN (
                SELECT id FROM webhooks WHERE channel = ?
                ORDER BY received_at ASC LIMIT ?
            ) RETURNING {WEBHOOK_COLUMNS}"
        );

        let rows = self
            .with_lock_retry("read_and_delete_by_channel", || {
                sqlx::query(&statement)
                    .bind(channel.as_str())
                    .bind(limit)
                    .fetch_all(self.get_pool())
            })
            .await
            .map_err(to_repository_error)?;

        let webhooks = rows.iter().filter_map(parse_webhook_row).collect();

        Ok(webhooks)
    }

    async fn peek_oldest_by_channel(
        &self,
        channel: &WebhookChannel,
    ) -> Result<Option<Webhook>, WebhookRepositoryError> {
        let statement = format!(
            "SELECT {WEBHOOK_COLUMNS} FROM webhooks
             WHERE channel = ? ORDER BY received_at ASC LIMIT 1"
        );

        let row = self
            .with_lock_retry("peek_oldest_by_channel", || {
                sqlx::query(&statement)
                    .bind(channel.as_str())
                    .fetch_optional(self.get_pool())
            })
            .await
            .map_err(to_repository_error)?;

        let webhook = row.as_ref().and_then(parse_webhook_row);

        Ok(webhook)
    }

    async fn list_by_channel(
        &self,
        channel: &WebhookChannel,
    ) -> Result<Vec<Webhook>, WebhookRepositoryError> {
        let statement = format!(
            "SELECT {WEBHOOK_COLUMNS} FROM webhooks
             WHERE channel = ? ORDER BY received_at DESC"
        );

        let rows = self
            .with_lock_retry("list_by_channel", || {
                sqlx::query(&statement)
                    .bind(channel.as_str())
                    .fetch_all(self.get_pool())
            })
            .await
            .map_err(to_repository_error)?;

        let webhooks = rows.iter().filter_map(parse_webhook_row).collect();

        Ok(webhooks)
    }

    async fn delete_by_id(&self, id: i64) -> Result<(), WebhookRepositoryError> {
        self.with_lock_retry("delete_by_id", || {
            sqlx::query("DELETE FROM webhooks WHERE id = ?")
                .bind(id)
                .execute(self.get_pool())
        })
        .await
        .map_err(to_repository_error)?;

        Ok(())
    }

    async fn increment_forward_attempts(
        &self,
        id: i64,
        error_message: &str,
    ) -> Result<(), WebhookRepositoryError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        self.with_lock_retry("increment_forward_attempts", || {
            sqlx::query(
                "UPDATE webhooks SET forward_attempts = forward_attempts + 1, last_attempt_at = ?, last_attempt_error = ? WHERE id = ?",
            )
            .bind(now)
            .bind(error_message)
            .bind(id)
            .execute(self.get_pool())
        })
        .await
        .map_err(to_repository_error)?;

        Ok(())
    }

    async fn count_by_channel(
        &self,
        channel: &WebhookChannel,
    ) -> Result<i64, WebhookRepositoryError> {
        let count = self
            .with_lock_retry("count_by_channel", || {
                sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM webhooks WHERE channel = ?")
                    .bind(channel.as_str())
                    .fetch_one(self.get_pool())
            })
            .await
            .map_err(to_repository_error)?;

        Ok(count)
    }

    async fn list_queue_by_channel(
        &self,
        channel: &WebhookChannel,
    ) -> Result<Vec<Webhook>, WebhookRepositoryError> {
        let statement = format!(
            "SELECT {WEBHOOK_COLUMNS} FROM webhooks
             WHERE channel = ? ORDER BY received_at ASC"
        );

        let rows = self
            .with_lock_retry("list_queue_by_channel", || {
                sqlx::query(&statement)
                    .bind(channel.as_str())
                    .fetch_all(self.get_pool())
            })
            .await
            .map_err(to_repository_error)?;

        let webhooks = rows.iter().filter_map(parse_webhook_row).collect();

        Ok(webhooks)
    }

    async fn clear_by_channel(
        &self,
        channel: &WebhookChannel,
    ) -> Result<i64, WebhookRepositoryError> {
        let result = self
            .with_lock_retry("clear_by_channel", || {
                sqlx::query("DELETE FROM webhooks WHERE channel = ?")
                    .bind(channel.as_str())
                    .execute(self.get_pool())
            })
            .await
            .map_err(to_repository_error)?;

        Ok(result.rows_affected() as i64)
    }

    async fn get_by_id(&self, id: i64) -> Result<Option<Webhook>, WebhookRepositoryError> {
        let statement = format!("SELECT {WEBHOOK_COLUMNS} FROM webhooks WHERE id = ?");

        let row = self
            .with_lock_retry("get_by_id", || {
                sqlx::query(&statement)
                    .bind(id)
                    .fetch_optional(self.get_pool())
            })
            .await
            .map_err(to_repository_error)?;

        let webhook = row.as_ref().and_then(parse_webhook_row);

        Ok(webhook)
    }

    async fn reset_forward_attempts(&self, id: i64) -> Result<(), WebhookRepositoryError> {
        self.with_lock_retry("reset_forward_attempts", || {
            sqlx::query(
                "UPDATE webhooks SET forward_attempts = 0, last_attempt_at = NULL, last_attempt_error = NULL WHERE id = ?",
            )
            .bind(id)
            .execute(self.get_pool())
        })
        .await
        .map_err(to_repository_error)?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::webhook::ports::WebhookRepository;
    use crate::outbound::sqlite::SqliteTuning;
    use crate::outbound::sqlite::test_support::{WriteLock, fast_tuning, no_retry_tuning, temp_db};
    use std::collections::HashSet;
    use std::time::{Duration, Instant};

    async fn get_in_memory_db() -> Sqlite {
        Sqlite::new("sqlite::memory:").await.unwrap()
    }

    fn make_webhook(channel: &str, payload: &[u8], received_at: i64) -> Webhook {
        Webhook::new(
            WebhookChannel::new(channel),
            HashMap::new(),
            Bytes::copy_from_slice(payload),
            received_at,
        )
    }

    #[tokio::test]
    async fn test_insert_webhook() {
        let db = get_in_memory_db().await;

        let webhook = make_webhook("demo", b"{\"event\":\"push\"}", 1000);

        let result = db.insert(&webhook).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_insert_and_peek_with_headers() {
        let db = get_in_memory_db().await;

        let mut headers = HashMap::new();
        headers.insert("x-custom-header".to_string(), "value123".to_string());

        let webhook = Webhook::new(
            WebhookChannel::new("demo"),
            headers.clone(),
            Bytes::from_static(b"{\"event\":\"push\"}"),
            1000,
        );
        db.insert(&webhook).await.unwrap();

        let peeked = db
            .peek_oldest_by_channel(&WebhookChannel::new("demo"))
            .await
            .unwrap()
            .unwrap();

        assert_eq!(peeked.headers, headers);
        assert_eq!(peeked.payload, &b"{\"event\":\"push\"}"[..]);
    }

    #[tokio::test]
    async fn test_peek_oldest_fifo() {
        let db = get_in_memory_db().await;

        for i in 1i64..=3 {
            db.insert(&make_webhook(
                "demo",
                format!("{{\"seq\":{i}}}").as_bytes(),
                1000 + i,
            ))
            .await
            .unwrap();
        }

        let peeked = db
            .peek_oldest_by_channel(&WebhookChannel::new("demo"))
            .await
            .unwrap()
            .unwrap();

        assert_eq!(peeked.payload, &b"{\"seq\":1}"[..]);
    }

    #[tokio::test]
    async fn test_peek_empty() {
        let db = get_in_memory_db().await;

        let result = db
            .peek_oldest_by_channel(&WebhookChannel::new("demo"))
            .await
            .unwrap();

        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_delete_by_id() {
        let db = get_in_memory_db().await;

        db.insert(&make_webhook("demo", b"{\"seq\":1}", 1000))
            .await
            .unwrap();

        let peeked = db
            .peek_oldest_by_channel(&WebhookChannel::new("demo"))
            .await
            .unwrap()
            .unwrap();

        let id = peeked.id.unwrap();
        db.delete_by_id(id).await.unwrap();

        let after = db
            .peek_oldest_by_channel(&WebhookChannel::new("demo"))
            .await
            .unwrap();

        assert!(after.is_none());
    }

    #[tokio::test]
    async fn test_read_and_delete_by_channel() {
        let db = get_in_memory_db().await;

        for i in 1i64..=3 {
            db.insert(&make_webhook(
                "demo",
                format!("{{\"event\":{i}}}").as_bytes(),
                1000 + i,
            ))
            .await
            .unwrap();
        }

        let webhooks = db
            .read_and_delete_by_channel(&WebhookChannel::new("demo"), 10)
            .await
            .unwrap();
        assert_eq!(webhooks.len(), 3);

        // Verify pop semantics — second read returns empty
        let webhooks2 = db
            .read_and_delete_by_channel(&WebhookChannel::new("demo"), 10)
            .await
            .unwrap();
        assert_eq!(webhooks2.len(), 0);
    }

    #[tokio::test]
    async fn test_list_by_channel_returns_all_newest_first() {
        let db = get_in_memory_db().await;

        for i in 1i64..=3 {
            db.insert(&make_webhook(
                "demo",
                format!("{{\"seq\":{i}}}").as_bytes(),
                1000 + i,
            ))
            .await
            .unwrap();
        }

        let webhooks = db
            .list_by_channel(&WebhookChannel::new("demo"))
            .await
            .unwrap();

        assert_eq!(webhooks.len(), 3);
        assert_eq!(webhooks[0].received_at, 1003); // newest first
        assert_eq!(webhooks[1].received_at, 1002);
        assert_eq!(webhooks[2].received_at, 1001);
    }

    #[tokio::test]
    async fn test_list_by_channel_empty() {
        let db = get_in_memory_db().await;

        let webhooks = db
            .list_by_channel(&WebhookChannel::new("nonexistent"))
            .await
            .unwrap();

        assert!(webhooks.is_empty());
    }

    #[tokio::test]
    async fn test_list_does_not_delete() {
        let db = get_in_memory_db().await;

        db.insert(&make_webhook("demo", b"{\"event\":\"push\"}", 1000))
            .await
            .unwrap();

        let first = db
            .list_by_channel(&WebhookChannel::new("demo"))
            .await
            .unwrap();
        assert_eq!(first.len(), 1);

        let second = db
            .list_by_channel(&WebhookChannel::new("demo"))
            .await
            .unwrap();
        assert_eq!(second.len(), 1);
    }

    #[tokio::test]
    async fn test_cross_channel_isolation() {
        let db = get_in_memory_db().await;

        db.insert(&make_webhook("a", b"{\"ch\":\"a\"}", 1000))
            .await
            .unwrap();
        db.insert(&make_webhook("b", b"{\"ch\":\"b\"}", 1000))
            .await
            .unwrap();

        let a = db
            .read_and_delete_by_channel(&WebhookChannel::new("a"), 10)
            .await
            .unwrap();
        assert_eq!(a.len(), 1);

        // Channel b still intact
        let b = db
            .read_and_delete_by_channel(&WebhookChannel::new("b"), 10)
            .await
            .unwrap();
        assert_eq!(b.len(), 1);
    }

    #[tokio::test]
    async fn test_increment_forward_attempts() {
        let db = get_in_memory_db().await;

        db.insert(&make_webhook("demo", b"{\"event\":\"push\"}", 1000))
            .await
            .unwrap();

        let webhook = db
            .peek_oldest_by_channel(&WebhookChannel::new("demo"))
            .await
            .unwrap()
            .unwrap();
        let id = webhook.id.unwrap();

        assert_eq!(webhook.forward_attempts, 0);
        assert!(webhook.last_attempt_at.is_none());
        assert!(webhook.last_attempt_error.is_none());

        db.increment_forward_attempts(id, "connection refused")
            .await
            .unwrap();

        let updated = db.get_by_id(id).await.unwrap().unwrap();
        assert_eq!(updated.forward_attempts, 1);
        assert!(updated.last_attempt_at.is_some());
        assert_eq!(
            updated.last_attempt_error.as_deref(),
            Some("connection refused")
        );

        db.increment_forward_attempts(id, "timeout").await.unwrap();

        let updated2 = db.get_by_id(id).await.unwrap().unwrap();
        assert_eq!(updated2.forward_attempts, 2);
        assert_eq!(updated2.last_attempt_error.as_deref(), Some("timeout"));
    }

    #[tokio::test]
    async fn test_count_by_channel() {
        let db = get_in_memory_db().await;

        let count = db
            .count_by_channel(&WebhookChannel::new("demo"))
            .await
            .unwrap();
        assert_eq!(count, 0);

        for i in 1i64..=3 {
            db.insert(&make_webhook("demo", b"{}", 1000 + i))
                .await
                .unwrap();
        }
        db.insert(&make_webhook("other", b"{}", 1000))
            .await
            .unwrap();

        let count = db
            .count_by_channel(&WebhookChannel::new("demo"))
            .await
            .unwrap();
        assert_eq!(count, 3);

        let count_other = db
            .count_by_channel(&WebhookChannel::new("other"))
            .await
            .unwrap();
        assert_eq!(count_other, 1);
    }

    #[tokio::test]
    async fn test_list_queue_by_channel_fifo_order() {
        let db = get_in_memory_db().await;

        for i in 1i64..=3 {
            db.insert(&make_webhook(
                "demo",
                format!("{{\"seq\":{i}}}").as_bytes(),
                1000 + i,
            ))
            .await
            .unwrap();
        }

        let webhooks = db
            .list_queue_by_channel(&WebhookChannel::new("demo"))
            .await
            .unwrap();

        assert_eq!(webhooks.len(), 3);
        // FIFO: oldest first
        assert_eq!(webhooks[0].received_at, 1001);
        assert_eq!(webhooks[1].received_at, 1002);
        assert_eq!(webhooks[2].received_at, 1003);
    }

    #[tokio::test]
    async fn test_clear_by_channel() {
        let db = get_in_memory_db().await;

        for i in 1i64..=3 {
            db.insert(&make_webhook("demo", b"{}", 1000 + i))
                .await
                .unwrap();
        }
        db.insert(&make_webhook("other", b"{}", 1000))
            .await
            .unwrap();

        let deleted = db
            .clear_by_channel(&WebhookChannel::new("demo"))
            .await
            .unwrap();
        assert_eq!(deleted, 3);

        let count = db
            .count_by_channel(&WebhookChannel::new("demo"))
            .await
            .unwrap();
        assert_eq!(count, 0);

        // Other channel not affected
        let count_other = db
            .count_by_channel(&WebhookChannel::new("other"))
            .await
            .unwrap();
        assert_eq!(count_other, 1);
    }

    #[tokio::test]
    async fn test_get_by_id() {
        let db = get_in_memory_db().await;

        db.insert(&make_webhook("demo", b"{\"event\":\"push\"}", 1000))
            .await
            .unwrap();

        let webhook = db
            .peek_oldest_by_channel(&WebhookChannel::new("demo"))
            .await
            .unwrap()
            .unwrap();
        let id = webhook.id.unwrap();

        let fetched = db.get_by_id(id).await.unwrap();
        assert!(fetched.is_some());
        let fetched = fetched.unwrap();
        assert_eq!(fetched.id, Some(id));
        assert_eq!(fetched.channel.as_str(), "demo");
        assert_eq!(fetched.payload, &b"{\"event\":\"push\"}"[..]);

        // Non-existent id
        let missing = db.get_by_id(9999).await.unwrap();
        assert!(missing.is_none());
    }

    #[tokio::test]
    async fn test_reset_forward_attempts() {
        let db = get_in_memory_db().await;

        db.insert(&make_webhook("demo", b"{}", 1000)).await.unwrap();

        let webhook = db
            .peek_oldest_by_channel(&WebhookChannel::new("demo"))
            .await
            .unwrap()
            .unwrap();
        let id = webhook.id.unwrap();

        // Increment first
        db.increment_forward_attempts(id, "some error")
            .await
            .unwrap();
        db.increment_forward_attempts(id, "another error")
            .await
            .unwrap();

        let updated = db.get_by_id(id).await.unwrap().unwrap();
        assert_eq!(updated.forward_attempts, 2);
        assert!(updated.last_attempt_at.is_some());
        assert!(updated.last_attempt_error.is_some());

        // Reset
        db.reset_forward_attempts(id).await.unwrap();

        let reset = db.get_by_id(id).await.unwrap().unwrap();
        assert_eq!(reset.forward_attempts, 0);
        assert!(reset.last_attempt_at.is_none());
        assert!(reset.last_attempt_error.is_none());
    }

    // --- Lock contention -----------------------------------------------------
    //
    // These run against an on-disk database on purpose: `sqlite::memory:` reports
    // `journal_mode = memory` and never takes a file lock, so none of the
    // behaviour below is observable there.

    #[tokio::test]
    async fn in_memory_databases_cannot_exercise_locking() {
        let db = get_in_memory_db().await;

        let journal_mode: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(db.get_pool())
            .await
            .unwrap();

        assert_eq!(
            journal_mode, "memory",
            "documents why the contention tests below use a file-backed database"
        );
    }

    #[tokio::test]
    async fn write_lock_held_past_the_budget_is_reported_as_busy() {
        let (_dir, url) = temp_db();
        let db = Sqlite::new_with_tuning(&url, fast_tuning()).await.unwrap();

        let lock = WriteLock::acquire(&url).await;

        let error = db
            .insert(&make_webhook("demo", b"{}", 1000))
            .await
            .expect_err("a permanently held write lock must surface as an error");

        assert!(
            error.is_busy(),
            "lock contention must be distinguishable from a real failure, got: {error}"
        );

        lock.release().await;
    }

    #[tokio::test]
    async fn insert_recovers_once_the_lock_is_released() {
        let (_dir, url) = temp_db();
        let db = Sqlite::new_with_tuning(&url, fast_tuning()).await.unwrap();

        // Held for longer than a single `busy_timeout` (50ms), so the insert can
        // only succeed if it is retried.
        WriteLock::acquire(&url)
            .await
            .release_after(Duration::from_millis(200));

        db.insert(&make_webhook("demo", b"{}", 1000))
            .await
            .expect("insert must survive a transient lock");

        assert_eq!(
            db.count_by_channel(&WebhookChannel::new("demo"))
                .await
                .unwrap(),
            1,
            "the webhook must be stored exactly once, not duplicated by retries"
        );
    }

    /// The counterpart to the test above: with retries disabled the very same
    /// scenario fails, which is what proves the retry is doing the work.
    #[tokio::test]
    async fn insert_fails_on_a_transient_lock_without_retries() {
        let (_dir, url) = temp_db();
        let db = Sqlite::new_with_tuning(&url, no_retry_tuning())
            .await
            .unwrap();

        WriteLock::acquire(&url)
            .await
            .release_after(Duration::from_millis(200));

        let error = db
            .insert(&make_webhook("demo", b"{}", 1000))
            .await
            .expect_err("without retries a transient lock must fail");

        assert!(error.is_busy());
    }

    #[tokio::test]
    async fn every_write_operation_recovers_from_a_transient_lock() {
        let (_dir, url) = temp_db();
        let db = Sqlite::new_with_tuning(&url, fast_tuning()).await.unwrap();

        db.insert(&make_webhook("demo", b"{}", 1000)).await.unwrap();
        let id = db
            .peek_oldest_by_channel(&WebhookChannel::new("demo"))
            .await
            .unwrap()
            .unwrap()
            .id
            .unwrap();

        let channel = WebhookChannel::new("demo");
        let hold = Duration::from_millis(150);

        WriteLock::acquire(&url).await.release_after(hold);
        db.increment_forward_attempts(id, "boom")
            .await
            .expect("increment_forward_attempts must retry");

        WriteLock::acquire(&url).await.release_after(hold);
        db.reset_forward_attempts(id)
            .await
            .expect("reset_forward_attempts must retry");

        WriteLock::acquire(&url).await.release_after(hold);
        db.read_and_delete_by_channel(&channel, 10)
            .await
            .expect("read_and_delete_by_channel must retry");

        db.insert(&make_webhook("demo", b"{}", 1001)).await.unwrap();

        WriteLock::acquire(&url).await.release_after(hold);
        db.clear_by_channel(&channel)
            .await
            .expect("clear_by_channel must retry");

        db.insert(&make_webhook("demo", b"{}", 1002)).await.unwrap();
        let id = db
            .peek_oldest_by_channel(&channel)
            .await
            .unwrap()
            .unwrap()
            .id
            .unwrap();

        WriteLock::acquire(&url).await.release_after(hold);
        db.delete_by_id(id).await.expect("delete_by_id must retry");

        assert_eq!(db.count_by_channel(&channel).await.unwrap(), 0);
    }

    /// A stuck lock must not turn into an unbounded hang: the caller has to get an
    /// answer within the retry budget so the HTTP handler can respond in time.
    #[tokio::test]
    async fn giving_up_happens_within_the_retry_budget() {
        let (_dir, url) = temp_db();
        let tuning = SqliteTuning {
            busy_timeout: Duration::from_millis(50),
            retry: crate::outbound::sqlite::LockRetryPolicy {
                max_attempts: 10,
                initial_backoff: Duration::from_millis(20),
                max_backoff: Duration::from_millis(50),
                budget: Duration::from_millis(500),
            },
            ..SqliteTuning::default()
        };
        let db = Sqlite::new_with_tuning(&url, tuning).await.unwrap();

        let lock = WriteLock::acquire(&url).await;

        let started = Instant::now();
        let error = db
            .insert(&make_webhook("demo", b"{}", 1000))
            .await
            .unwrap_err();
        let elapsed = started.elapsed();

        assert!(error.is_busy());
        assert!(
            elapsed < Duration::from_millis(1_500),
            "gave up after {elapsed:?}, which exceeds the 500ms budget by too much"
        );

        lock.release().await;
    }

    /// WAL exists so that polling and the UI never wait on the forwarder's writes.
    #[tokio::test]
    async fn reads_are_not_blocked_by_a_held_write_lock() {
        let (_dir, url) = temp_db();
        let db = Sqlite::new_with_tuning(&url, fast_tuning()).await.unwrap();

        for i in 0..5 {
            db.insert(&make_webhook("demo", b"{}", 1000 + i))
                .await
                .unwrap();
        }

        let lock = WriteLock::acquire(&url).await;
        let channel = WebhookChannel::new("demo");

        let started = Instant::now();
        assert_eq!(db.count_by_channel(&channel).await.unwrap(), 5);
        assert_eq!(db.list_by_channel(&channel).await.unwrap().len(), 5);
        assert_eq!(db.list_queue_by_channel(&channel).await.unwrap().len(), 5);
        assert!(db.peek_oldest_by_channel(&channel).await.unwrap().is_some());
        let elapsed = started.elapsed();

        assert!(
            elapsed < Duration::from_millis(200),
            "reads waited {elapsed:?} on a writer; WAL should make them lock-free"
        );

        lock.release().await;
    }

    #[tokio::test]
    async fn concurrent_writers_do_not_produce_lock_errors() {
        let (_dir, url) = temp_db();
        let db = Sqlite::new(&url).await.unwrap();

        const WRITERS: i64 = 16;
        const PER_WRITER: i64 = 25;

        let mut tasks = tokio::task::JoinSet::new();
        for writer in 0..WRITERS {
            let db = db.clone();
            tasks.spawn(async move {
                let mut errors = vec![];
                for i in 0..PER_WRITER {
                    let webhook = make_webhook(&format!("ch{}", writer % 4), b"{}", i);
                    if let Err(e) = db.insert(&webhook).await {
                        errors.push(e.to_string());
                    }
                }
                errors
            });
        }

        let mut errors = vec![];
        while let Some(result) = tasks.join_next().await {
            errors.extend(result.unwrap());
        }

        assert!(errors.is_empty(), "concurrent inserts failed: {errors:?}");

        let mut total = 0;
        for channel in 0..4 {
            total += db
                .count_by_channel(&WebhookChannel::new(format!("ch{channel}")))
                .await
                .unwrap();
        }
        assert_eq!(total, WRITERS * PER_WRITER, "no write may be lost");
    }

    #[tokio::test]
    async fn writers_readers_and_deleters_coexist_without_lock_errors() {
        let (_dir, url) = temp_db();
        let db = Sqlite::new(&url).await.unwrap();

        for i in 0..200 {
            db.insert(&make_webhook("demo", b"{}", i)).await.unwrap();
        }

        let mut tasks = tokio::task::JoinSet::new();

        for worker in 0..12 {
            let db = db.clone();
            tasks.spawn(async move {
                let channel = WebhookChannel::new("demo");
                let mut errors = vec![];

                for i in 0..25 {
                    let result = match worker % 4 {
                        0 => db.insert(&make_webhook("demo", b"{}", 1_000 + i)).await,
                        1 => db.list_by_channel(&channel).await.map(|_| ()),
                        2 => db.count_by_channel(&channel).await.map(|_| ()),
                        _ => db.read_and_delete_by_channel(&channel, 3).await.map(|_| ()),
                    };

                    if let Err(e) = result {
                        errors.push(format!("worker {worker}: {e}"));
                    }
                }

                errors
            });
        }

        let mut errors = vec![];
        while let Some(result) = tasks.join_next().await {
            errors.extend(result.unwrap());
        }

        assert!(errors.is_empty(), "mixed workload failed: {errors:?}");
    }

    /// `read_and_delete_by_channel` is the destructive poll endpoint. Two clients
    /// polling at once must never both receive the same webhook.
    #[tokio::test]
    async fn concurrent_polls_never_return_the_same_webhook_twice() {
        let (_dir, url) = temp_db();
        let db = Sqlite::new(&url).await.unwrap();

        const TOTAL: i64 = 300;
        for i in 0..TOTAL {
            db.insert(&make_webhook("demo", b"{}", i)).await.unwrap();
        }

        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..8 {
            let db = db.clone();
            tasks.spawn(async move {
                let channel = WebhookChannel::new("demo");
                let mut claimed = vec![];

                loop {
                    let batch = db.read_and_delete_by_channel(&channel, 7).await.unwrap();
                    if batch.is_empty() {
                        break;
                    }
                    claimed.extend(batch.into_iter().filter_map(|w| w.id));
                }

                claimed
            });
        }

        let mut all = vec![];
        while let Some(result) = tasks.join_next().await {
            all.extend(result.unwrap());
        }

        let unique: HashSet<i64> = all.iter().copied().collect();
        assert_eq!(
            unique.len(),
            all.len(),
            "a webhook was handed to two pollers at once"
        );
        assert_eq!(
            all.len() as i64,
            TOTAL,
            "every webhook must be claimed exactly once"
        );
    }

    /// Clearing a large queue is the longest write this service performs, so it is
    /// the most likely thing to lock out concurrent receives.
    #[tokio::test]
    async fn clearing_a_large_queue_does_not_lock_out_receives() {
        let (_dir, url) = temp_db();
        let db = Sqlite::new(&url).await.unwrap();

        for i in 0..2_000 {
            db.insert(&make_webhook(
                "bulk",
                b"{\"padding\":\"xxxxxxxxxxxxxxxx\"}",
                i,
            ))
            .await
            .unwrap();
        }

        let clearing = {
            let db = db.clone();
            tokio::spawn(async move { db.clear_by_channel(&WebhookChannel::new("bulk")).await })
        };

        let mut errors = vec![];
        for i in 0..100 {
            if let Err(e) = db.insert(&make_webhook("live", b"{}", i)).await {
                errors.push(e.to_string());
            }
        }

        let cleared = clearing.await.unwrap().unwrap();

        assert_eq!(cleared, 2_000);
        assert!(
            errors.is_empty(),
            "receives failed while the queue was cleared: {errors:?}"
        );
        assert_eq!(
            db.count_by_channel(&WebhookChannel::new("live"))
                .await
                .unwrap(),
            100
        );
    }

    /// A failure that is not lock contention must stay a hard error, so real bugs
    /// are not hidden behind a "retry later" response.
    #[tokio::test]
    async fn non_lock_failures_are_not_classified_as_busy() {
        let (_dir, url) = temp_db();
        let db = Sqlite::new(&url).await.unwrap();

        sqlx::query("DROP TABLE webhooks")
            .execute(db.get_pool())
            .await
            .unwrap();

        let started = Instant::now();
        let error = db
            .insert(&make_webhook("demo", b"{}", 1000))
            .await
            .unwrap_err();
        let elapsed = started.elapsed();

        assert!(
            !error.is_busy(),
            "a missing table is a real failure, not contention: {error}"
        );
        assert!(
            elapsed < Duration::from_millis(500),
            "a non-retryable error must fail fast, took {elapsed:?}"
        );
    }
}
