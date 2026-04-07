use std::collections::HashMap;

use bytes::Bytes;

use super::model::{
    DeleteWebhookError, ListWebhooksError, QueueWebhooksError, ReadWebhooksError,
    ReceiveWebhookError, Webhook, WebhookChannel,
};
use super::ports::WebhookRepository;

#[derive(Clone)]
pub struct WebhookServiceImpl<R: WebhookRepository> {
    repository: R,
    read_limit: i64,
}

impl<R: WebhookRepository> WebhookServiceImpl<R> {
    pub fn new(repository: R) -> Self {
        Self {
            repository,
            read_limit: 1000,
        }
    }

    pub async fn receive_webhook(
        &self,
        channel: WebhookChannel,
        headers: HashMap<String, String>,
        payload: Bytes,
    ) -> Result<(), ReceiveWebhookError> {
        let current_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        let webhook = Webhook::new(channel, headers, payload, current_time);

        self.repository.insert(&webhook).await?;

        log::debug!("Stored webhook for channel={}", webhook.channel.as_str());

        Ok(())
    }

    pub async fn read_and_delete_webhooks(
        &self,
        channel: &WebhookChannel,
    ) -> Result<Vec<Webhook>, ReadWebhooksError> {
        let webhooks = self
            .repository
            .read_and_delete_by_channel(channel, self.read_limit)
            .await?;

        log::debug!(
            "Read and deleted {} webhooks for channel={}",
            webhooks.len(),
            channel.as_str()
        );

        Ok(webhooks)
    }

    pub async fn list_webhooks(
        &self,
        channel: &WebhookChannel,
    ) -> Result<Vec<Webhook>, ListWebhooksError> {
        let webhooks = self.repository.list_by_channel(channel).await?;

        log::debug!(
            "Listed {} webhooks for channel={}",
            webhooks.len(),
            channel.as_str()
        );

        Ok(webhooks)
    }

    pub async fn delete_webhook(
        &self,
        channel: &WebhookChannel,
        webhook_id: i64,
    ) -> Result<(), DeleteWebhookError> {
        self.repository.delete_by_id(webhook_id).await?;

        log::debug!(
            "Deleted webhook id={} for channel={}",
            webhook_id,
            channel.as_str()
        );

        Ok(())
    }

    pub async fn list_queue(
        &self,
        channel: &WebhookChannel,
    ) -> Result<Vec<Webhook>, QueueWebhooksError> {
        let webhooks = self.repository.list_queue_by_channel(channel).await?;

        log::debug!(
            "Listed {} queued webhooks for channel={}",
            webhooks.len(),
            channel.as_str()
        );

        Ok(webhooks)
    }

    pub async fn count_queue(
        &self,
        channel: &WebhookChannel,
    ) -> Result<i64, QueueWebhooksError> {
        let count = self.repository.count_by_channel(channel).await?;

        log::debug!(
            "Counted {} queued webhooks for channel={}",
            count,
            channel.as_str()
        );

        Ok(count)
    }

    pub async fn clear_queue(
        &self,
        channel: &WebhookChannel,
    ) -> Result<i64, QueueWebhooksError> {
        let deleted = self.repository.clear_by_channel(channel).await?;

        log::debug!(
            "Cleared {} webhooks from queue for channel={}",
            deleted,
            channel.as_str()
        );

        Ok(deleted)
    }

    pub async fn get_webhook(
        &self,
        id: i64,
    ) -> Result<Option<Webhook>, QueueWebhooksError> {
        let webhook = self.repository.get_by_id(id).await?;

        log::debug!("Fetched webhook id={}, found={}", id, webhook.is_some());

        Ok(webhook)
    }

    pub async fn increment_forward_attempts(
        &self,
        id: i64,
        error_message: &str,
    ) -> Result<(), QueueWebhooksError> {
        self.repository
            .increment_forward_attempts(id, error_message)
            .await?;

        log::debug!(
            "Incremented forward attempts for webhook id={}, error={}",
            id,
            error_message
        );

        Ok(())
    }
}
