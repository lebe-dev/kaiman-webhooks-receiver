use std::future::Future;

use super::model::{Webhook, WebhookChannel, WebhookRepositoryError};

pub trait WebhookRepository: Send + Sync + Clone + 'static {
    fn insert(
        &self,
        webhook: &Webhook,
    ) -> impl Future<Output = Result<(), WebhookRepositoryError>> + Send;

    /// Atomically read and delete all webhooks for channel (up to limit), oldest first.
    fn read_and_delete_by_channel(
        &self,
        channel: &WebhookChannel,
        limit: i64,
    ) -> impl Future<Output = Result<Vec<Webhook>, WebhookRepositoryError>> + Send;

    /// Peek at the oldest webhook that is due for delivery, without deleting it.
    ///
    /// Webhooks whose `next_attempt_at` is still in the future are skipped, so one
    /// undeliverable webhook cannot hold up the rest of the channel's queue.
    /// `now` is a unix timestamp, passed in so the decision stays testable.
    fn peek_oldest_due_by_channel(
        &self,
        channel: &WebhookChannel,
        now: i64,
    ) -> impl Future<Output = Result<Option<Webhook>, WebhookRepositoryError>> + Send;

    /// Delete a webhook by its ID.
    fn delete_by_id(
        &self,
        id: i64,
    ) -> impl Future<Output = Result<(), WebhookRepositoryError>> + Send;

    /// Non-destructive read of all stored webhooks for a channel, newest first.
    fn list_by_channel(
        &self,
        channel: &WebhookChannel,
    ) -> impl Future<Output = Result<Vec<Webhook>, WebhookRepositoryError>> + Send;

    /// Records a failed delivery: bumps the attempt counter, stores the error, and
    /// parks the webhook until `next_attempt_at` (a unix timestamp).
    fn record_forward_failure(
        &self,
        id: i64,
        error_message: &str,
        next_attempt_at: i64,
    ) -> impl Future<Output = Result<(), WebhookRepositoryError>> + Send;

    fn count_by_channel(
        &self,
        channel: &WebhookChannel,
    ) -> impl Future<Output = Result<i64, WebhookRepositoryError>> + Send;

    fn list_queue_by_channel(
        &self,
        channel: &WebhookChannel,
    ) -> impl Future<Output = Result<Vec<Webhook>, WebhookRepositoryError>> + Send;

    fn clear_by_channel(
        &self,
        channel: &WebhookChannel,
    ) -> impl Future<Output = Result<i64, WebhookRepositoryError>> + Send;

    fn get_by_id(
        &self,
        id: i64,
    ) -> impl Future<Output = Result<Option<Webhook>, WebhookRepositoryError>> + Send;

    fn reset_forward_attempts(
        &self,
        id: i64,
    ) -> impl Future<Output = Result<(), WebhookRepositoryError>> + Send;
}
