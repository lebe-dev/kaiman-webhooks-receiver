use std::collections::HashMap;

use bytes::Bytes;
use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct WebhookChannel(String);

impl WebhookChannel {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone)]
pub struct Webhook {
    pub id: Option<i64>,
    pub channel: WebhookChannel,
    pub headers: HashMap<String, String>,
    pub payload: Bytes,
    pub received_at: i64,
}

impl Webhook {
    pub fn new(
        channel: WebhookChannel,
        headers: HashMap<String, String>,
        payload: Bytes,
        received_at: i64,
    ) -> Self {
        Self {
            id: None,
            channel,
            headers,
            payload,
            received_at,
        }
    }
}

#[derive(Debug, Error)]
pub enum WebhookRepositoryError {
    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

#[derive(Debug, Error)]
pub enum ReceiveWebhookError {
    #[error(transparent)]
    RepositoryError(#[from] WebhookRepositoryError),
}

#[derive(Debug, Error)]
pub enum ReadWebhooksError {
    #[error(transparent)]
    RepositoryError(#[from] WebhookRepositoryError),
}

#[derive(Debug, Error)]
pub enum ListWebhooksError {
    #[error(transparent)]
    RepositoryError(#[from] WebhookRepositoryError),
}

#[derive(Debug, Error)]
pub enum DeleteWebhookError {
    #[error(transparent)]
    RepositoryError(#[from] WebhookRepositoryError),
}
