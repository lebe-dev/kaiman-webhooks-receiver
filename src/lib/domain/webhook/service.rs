use std::collections::HashMap;

use bytes::Bytes;

use super::model::{
    DeleteWebhookError, ListWebhooksError, ReadWebhooksError, ReceiveWebhookError, Webhook,
    WebhookChannel,
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
}
