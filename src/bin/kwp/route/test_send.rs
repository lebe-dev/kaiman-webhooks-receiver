use std::sync::Arc;
use std::time::Duration;

use axum::{
    Extension, Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::middleware::request_id::RequestId;
use kwp_lib::domain::crypto;

#[derive(Deserialize)]
pub struct TestSendRequest {
    pub secret: Option<String>,
    pub payload: serde_json::Value,
}

#[derive(Serialize)]
pub struct TestSendResponse {
    pub status: u16,
    pub body: String,
}

pub async fn test_send_route(
    State(state): State<Arc<AppState>>,
    Extension(request_id): Extension<RequestId>,
    Path(channel_name): Path<String>,
    headers: HeaderMap,
    Json(req): Json<TestSendRequest>,
) -> impl IntoResponse {
    let bearer = headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));

    // Rejections are warnings here as everywhere else: an operator watching INFO has
    // to see that someone is being turned away from this endpoint.
    let bearer = match bearer {
        Some(b) => b,
        None => {
            log::warn!(
                "[req:{}] missing or invalid Authorization header for test-send on channel: {}",
                request_id,
                channel_name
            );
            return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
        }
    };

    let channel_config = match state.config.find_channel_by_token(bearer) {
        Some(c) => {
            if c.name != channel_name {
                log::warn!(
                    "[req:{}] token for channel '{}' used to test-send for channel '{}'",
                    request_id,
                    c.name,
                    channel_name
                );
                return (StatusCode::FORBIDDEN, "Forbidden").into_response();
            }
            c
        }
        None => {
            if !state.config.is_ui_token(bearer) {
                log::warn!(
                    "[req:{}] invalid token for test-send on channel: {}",
                    request_id,
                    channel_name
                );
                return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
            }
            match state.config.find_channel_by_name(&channel_name) {
                Some(c) => c,
                None => {
                    log::warn!(
                        "[req:{}] test-send requested for unknown channel: {}",
                        request_id,
                        channel_name
                    );
                    return (StatusCode::NOT_FOUND, "Channel not found").into_response();
                }
            }
        }
    };

    log::info!(
        "[req:{}] request to test-send webhook for channel: {}",
        request_id,
        channel_name
    );

    let forward_cfg = match &channel_config.forward {
        Some(f) => f,
        None => {
            log::warn!(
                "[req:{}] test-send requested for channel {} which has no forward configuration",
                request_id,
                channel_name
            );
            return (
                StatusCode::BAD_REQUEST,
                "Channel has no forward configuration",
            )
                .into_response();
        }
    };

    let body_bytes = serde_json::to_vec(&req.payload).unwrap_or_default();

    let mut request = state
        .http_client
        .post(&forward_cfg.url)
        .header("Content-Type", "application/json")
        .timeout(Duration::from_secs(forward_cfg.timeout_seconds));

    log::debug!(
        "[req:{}] test-send: {} bytes to {} (timeout {}s, sign-header {})",
        request_id,
        body_bytes.len(),
        forward_cfg.url,
        forward_cfg.timeout_seconds,
        forward_cfg.sign_header.as_deref().unwrap_or("none")
    );

    if let Some(sign_header) = &forward_cfg.sign_header {
        // Which secret was used explains a rejected signature; the secret and the
        // signature it produces are both withheld — the signature authenticates this
        // exact payload, so a log record carrying it is a replayable credential.
        let (secret_str, secret_source) = match req.secret.as_deref().filter(|s| !s.is_empty()) {
            Some(s) => (s.to_string(), "request"),
            None => match forward_cfg
                .sign_secret
                .as_deref()
                .or(channel_config.webhook_secret.as_deref())
            {
                Some(s) => (s.to_string(), "channel configuration"),
                None => {
                    log::warn!(
                        "[req:{}] test-send for channel {} needs a signature but no secret is available",
                        request_id,
                        channel_name
                    );
                    return (
                        StatusCode::BAD_REQUEST,
                        "No secret provided and no sign_secret or webhook_secret configured for this channel",
                    )
                        .into_response();
                }
            },
        };

        let signature = crypto::hmac_sha256_hex(secret_str.as_bytes(), &body_bytes);

        let header_value = match forward_cfg.sign_template.as_deref() {
            None => signature,
            Some(tmpl) => match crypto::render_sign_template(tmpl, &signature) {
                Ok(v) => v,
                Err(e) => {
                    log::error!(
                        "[req:{}] sign-template render failed for channel '{}': {}",
                        request_id,
                        channel_name,
                        e
                    );
                    return (StatusCode::INTERNAL_SERVER_ERROR, "Template error").into_response();
                }
            },
        };

        log::debug!(
            "[req:{}] test-send: signed with the secret from the {}, template {}",
            request_id,
            secret_source,
            if forward_cfg.sign_template.is_some() {
                "applied"
            } else {
                "not configured"
            }
        );

        request = request.header(sign_header.as_str(), &header_value);
    }

    let response = match request.body(body_bytes).send().await {
        Ok(r) => r,
        Err(e) => {
            log::warn!(
                "[req:{}] test-send to {} failed for channel '{}': {}",
                request_id,
                forward_cfg.url,
                channel_name,
                e
            );
            return (
                StatusCode::BAD_GATEWAY,
                format!("Failed to reach target: {e}"),
            )
                .into_response();
        }
    };

    let status = response.status().as_u16();
    let body = response.text().await.unwrap_or_default();

    // The body goes back to the caller in the response, so it does not need to be in
    // the log as well — its size is enough to explain an empty-looking result.
    log::info!(
        "[req:{}] test-send to channel '{}' returned status={} ({} bytes)",
        request_id,
        channel_name,
        status,
        body.len()
    );

    (StatusCode::OK, Json(TestSendResponse { status, body })).into_response()
}
