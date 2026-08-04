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
    pub forward_attempts: i64,
    pub last_attempt_at: Option<i64>,
    pub last_attempt_error: Option<String>,
    /// When the forwarder may try again, as a unix timestamp. `None` means the
    /// webhook is due now — the state of a webhook that has never failed.
    pub next_attempt_at: Option<i64>,
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
            forward_attempts: 0,
            last_attempt_at: None,
            last_attempt_error: None,
            next_attempt_at: None,
        }
    }
}

#[derive(Debug, Error)]
pub enum WebhookRepositoryError {
    /// The storage was too contended to serve the operation, and retrying inside
    /// the adapter did not help.
    ///
    /// This is transient and carries no information about whether the data is
    /// intact — nothing was written, and asking again later is expected to work.
    /// Callers should translate it into "retry later" (HTTP 503) rather than a
    /// failure, so that a webhook sender redelivers instead of dropping the event.
    #[error("storage is busy: {0}")]
    Busy(#[source] anyhow::Error),

    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

impl WebhookRepositoryError {
    pub fn is_busy(&self) -> bool {
        matches!(self, Self::Busy(_))
    }
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

#[derive(Debug, Error)]
pub enum QueueWebhooksError {
    #[error(transparent)]
    RepositoryError(#[from] WebhookRepositoryError),
}

/// Lets HTTP handlers answer "retry later" without having to know which
/// repository error they are looking at.
macro_rules! delegate_is_busy {
    ($($error:ty),+ $(,)?) => {
        $(
            impl $error {
                pub fn is_busy(&self) -> bool {
                    match self {
                        Self::RepositoryError(e) => e.is_busy(),
                    }
                }
            }
        )+
    };
}

delegate_is_busy!(
    ReceiveWebhookError,
    ReadWebhooksError,
    ListWebhooksError,
    DeleteWebhookError,
    QueueWebhooksError,
);

#[derive(Debug, Clone, Serialize)]
pub struct ChannelForwardStatus {
    pub paused: bool,
    pub queue_size: i64,
    pub last_success_at: Option<i64>,
    pub last_error_at: Option<i64>,
    pub last_error_message: Option<String>,
    /// Earliest moment any queued webhook of this channel becomes due, so the UI
    /// can explain a queue that sits still while the channel is not paused.
    pub next_attempt_at: Option<i64>,
}

impl Default for ChannelForwardStatus {
    fn default() -> Self {
        Self::new()
    }
}

impl ChannelForwardStatus {
    pub fn new() -> Self {
        Self {
            paused: false,
            queue_size: 0,
            last_success_at: None,
            last_error_at: None,
            last_error_message: None,
            next_attempt_at: None,
        }
    }
}
