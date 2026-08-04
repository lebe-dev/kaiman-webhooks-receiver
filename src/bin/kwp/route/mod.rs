use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};

pub mod config;
pub mod delete_webhook;
pub mod list_webhooks;
pub mod metrics;
pub mod queue;
pub mod read_webhooks;
pub mod receive_webhook;
pub mod sign_webhook;
pub mod test_send;
pub mod version;

/// How long a client is asked to wait before retrying a request that lost the
/// race for the database write lock.
const RETRY_AFTER_SECONDS: &str = "1";

/// Response for a request that could not be served because storage was too
/// contended.
///
/// This is deliberately 503 rather than 500: webhook senders redeliver on 503,
/// so a lock collision costs a retry instead of the event itself. Nothing was
/// written, so retrying cannot duplicate anything.
pub fn storage_busy_response() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [(header::RETRY_AFTER, RETRY_AFTER_SECONDS)],
        "Storage is busy, retry later",
    )
        .into_response()
}
