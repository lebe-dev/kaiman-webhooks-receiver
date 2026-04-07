use std::collections::HashMap;

use bytes::Bytes;

use crate::domain::webhook::model::{Webhook, WebhookChannel, WebhookRepositoryError};
use crate::domain::webhook::ports::WebhookRepository;

use super::init::Sqlite;
use sqlx::Row;

fn parse_webhook_row(row: &sqlx::sqlite::SqliteRow) -> Option<Webhook> {
    let id: i64 = row.try_get("id").ok()?;
    let channel: String = row.try_get("channel").ok()?;
    let headers_str: String = row.try_get("headers").ok()?;
    let payload: Vec<u8> = row.try_get("payload").ok()?;
    let received_at: i64 = row.try_get("received_at").ok()?;
    let forward_attempts: i64 = row.try_get("forward_attempts").ok()?;
    let last_attempt_at: Option<i64> = row.try_get("last_attempt_at").ok()?;
    let last_attempt_error: Option<String> = row.try_get("last_attempt_error").ok()?;

    let headers: HashMap<String, String> =
        serde_json::from_str(&headers_str).unwrap_or_default();

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

impl WebhookRepository for Sqlite {
    async fn insert(&self, webhook: &Webhook) -> Result<(), WebhookRepositoryError> {
        let headers_json =
            serde_json::to_string(&webhook.headers).unwrap_or_else(|_| "{}".to_string());

        sqlx::query(
            "INSERT INTO webhooks (channel, headers, payload, received_at) VALUES (?, ?, ?, ?)",
        )
        .bind(webhook.channel.as_str())
        .bind(headers_json)
        .bind(webhook.payload.as_ref())
        .bind(webhook.received_at)
        .execute(self.get_pool())
        .await
        .map_err(|e| WebhookRepositoryError::Other(e.into()))?;

        Ok(())
    }

    async fn read_and_delete_by_channel(
        &self,
        channel: &WebhookChannel,
        limit: i64,
    ) -> Result<Vec<Webhook>, WebhookRepositoryError> {
        let rows = sqlx::query(
            "DELETE FROM webhooks WHERE id IN (
                SELECT id FROM webhooks WHERE channel = ?
                ORDER BY received_at ASC LIMIT ?
            ) RETURNING id, channel, headers, payload, received_at, forward_attempts, last_attempt_at, last_attempt_error",
        )
        .bind(channel.as_str())
        .bind(limit)
        .fetch_all(self.get_pool())
        .await
        .map_err(|e| WebhookRepositoryError::Other(e.into()))?;

        let webhooks = rows.iter().filter_map(parse_webhook_row).collect();

        Ok(webhooks)
    }

    async fn peek_oldest_by_channel(
        &self,
        channel: &WebhookChannel,
    ) -> Result<Option<Webhook>, WebhookRepositoryError> {
        let row = sqlx::query(
            "SELECT id, channel, headers, payload, received_at, forward_attempts, last_attempt_at, last_attempt_error FROM webhooks
             WHERE channel = ? ORDER BY received_at ASC LIMIT 1",
        )
        .bind(channel.as_str())
        .fetch_optional(self.get_pool())
        .await
        .map_err(|e| WebhookRepositoryError::Other(e.into()))?;

        let webhook = row.as_ref().and_then(parse_webhook_row);

        Ok(webhook)
    }

    async fn list_by_channel(
        &self,
        channel: &WebhookChannel,
    ) -> Result<Vec<Webhook>, WebhookRepositoryError> {
        let rows = sqlx::query(
            "SELECT id, channel, headers, payload, received_at, forward_attempts, last_attempt_at, last_attempt_error FROM webhooks
             WHERE channel = ? ORDER BY received_at DESC",
        )
        .bind(channel.as_str())
        .fetch_all(self.get_pool())
        .await
        .map_err(|e| WebhookRepositoryError::Other(e.into()))?;

        let webhooks = rows.iter().filter_map(parse_webhook_row).collect();

        Ok(webhooks)
    }

    async fn delete_by_id(&self, id: i64) -> Result<(), WebhookRepositoryError> {
        sqlx::query("DELETE FROM webhooks WHERE id = ?")
            .bind(id)
            .execute(self.get_pool())
            .await
            .map_err(|e| WebhookRepositoryError::Other(e.into()))?;

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

        sqlx::query(
            "UPDATE webhooks SET forward_attempts = forward_attempts + 1, last_attempt_at = ?, last_attempt_error = ? WHERE id = ?",
        )
        .bind(now)
        .bind(error_message)
        .bind(id)
        .execute(self.get_pool())
        .await
        .map_err(|e| WebhookRepositoryError::Other(e.into()))?;

        Ok(())
    }

    async fn count_by_channel(
        &self,
        channel: &WebhookChannel,
    ) -> Result<i64, WebhookRepositoryError> {
        let row = sqlx::query("SELECT COUNT(*) as cnt FROM webhooks WHERE channel = ?")
            .bind(channel.as_str())
            .fetch_one(self.get_pool())
            .await
            .map_err(|e| WebhookRepositoryError::Other(e.into()))?;

        let count: i64 = row
            .try_get("cnt")
            .map_err(|e| WebhookRepositoryError::Other(e.into()))?;

        Ok(count)
    }

    async fn list_queue_by_channel(
        &self,
        channel: &WebhookChannel,
    ) -> Result<Vec<Webhook>, WebhookRepositoryError> {
        let rows = sqlx::query(
            "SELECT id, channel, headers, payload, received_at, forward_attempts, last_attempt_at, last_attempt_error FROM webhooks
             WHERE channel = ? ORDER BY received_at ASC",
        )
        .bind(channel.as_str())
        .fetch_all(self.get_pool())
        .await
        .map_err(|e| WebhookRepositoryError::Other(e.into()))?;

        let webhooks = rows.iter().filter_map(parse_webhook_row).collect();

        Ok(webhooks)
    }

    async fn clear_by_channel(
        &self,
        channel: &WebhookChannel,
    ) -> Result<i64, WebhookRepositoryError> {
        let result = sqlx::query("DELETE FROM webhooks WHERE channel = ?")
            .bind(channel.as_str())
            .execute(self.get_pool())
            .await
            .map_err(|e| WebhookRepositoryError::Other(e.into()))?;

        Ok(result.rows_affected() as i64)
    }

    async fn get_by_id(
        &self,
        id: i64,
    ) -> Result<Option<Webhook>, WebhookRepositoryError> {
        let row = sqlx::query(
            "SELECT id, channel, headers, payload, received_at, forward_attempts, last_attempt_at, last_attempt_error FROM webhooks
             WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(self.get_pool())
        .await
        .map_err(|e| WebhookRepositoryError::Other(e.into()))?;

        let webhook = row.as_ref().and_then(parse_webhook_row);

        Ok(webhook)
    }

    async fn reset_forward_attempts(
        &self,
        id: i64,
    ) -> Result<(), WebhookRepositoryError> {
        sqlx::query(
            "UPDATE webhooks SET forward_attempts = 0, last_attempt_at = NULL, last_attempt_error = NULL WHERE id = ?",
        )
        .bind(id)
        .execute(self.get_pool())
        .await
        .map_err(|e| WebhookRepositoryError::Other(e.into()))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::webhook::ports::WebhookRepository;

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

        db.increment_forward_attempts(id, "timeout")
            .await
            .unwrap();

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

        db.insert(&make_webhook("demo", b"{}", 1000))
            .await
            .unwrap();

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
}
