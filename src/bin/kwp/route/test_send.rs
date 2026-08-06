use std::sync::Arc;
use std::time::Duration;

use axum::{
    Extension, Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::middleware::request_id::RequestId;
use kwp_lib::domain::config::model::{WebhookChannelConfig, WebhookForwardConfig};
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

/// Resolves the channel this request is allowed to test-send to.
///
/// A channel token may only reach its own channel; the UI token may reach any
/// configured one. Rejections are warnings here as everywhere else: an operator
/// watching INFO has to see that someone is being turned away from this endpoint.
fn authorize_test_send<'a>(
    state: &'a AppState,
    request_id: &RequestId,
    channel_name: &str,
    headers: &HeaderMap,
) -> Result<&'a WebhookChannelConfig, Response> {
    let bearer = headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));

    let Some(bearer) = bearer else {
        log::warn!(
            "[req:{request_id}] missing or invalid Authorization header for test-send on channel: {channel_name}"
        );
        return Err((StatusCode::UNAUTHORIZED, "Unauthorized").into_response());
    };

    if let Some(channel_config) = state.config.find_channel_by_token(bearer) {
        if channel_config.name != channel_name {
            log::warn!(
                "[req:{}] token for channel '{}' used to test-send for channel '{}'",
                request_id,
                channel_config.name,
                channel_name
            );
            return Err((StatusCode::FORBIDDEN, "Forbidden").into_response());
        }
        return Ok(channel_config);
    }

    if !state.config.is_ui_token(bearer) {
        log::warn!("[req:{request_id}] invalid token for test-send on channel: {channel_name}");
        return Err((StatusCode::UNAUTHORIZED, "Unauthorized").into_response());
    }

    state
        .config
        .find_channel_by_name(channel_name)
        .ok_or_else(|| {
            log::warn!("[req:{request_id}] test-send requested for unknown channel: {channel_name}");
            (StatusCode::NOT_FOUND, "Channel not found").into_response()
        })
}

/// Builds the value of the signature header for a test send.
///
/// The secret in the request wins over the configured one — that is the point of
/// the endpoint: trying a secret before committing it to the config. Which source
/// was used is logged; the secret and the signature it produces are both withheld,
/// because a signature authenticates one exact payload and a log record carrying it
/// is a replayable credential.
fn sign_test_payload(
    channel_config: &WebhookChannelConfig,
    forward_cfg: &WebhookForwardConfig,
    request_id: &RequestId,
    request_secret: Option<&str>,
    body_bytes: &[u8],
) -> Result<String, Response> {
    let channel_name = &channel_config.name;

    let configured_secret = || {
        forward_cfg
            .sign_secret
            .as_deref()
            .or(channel_config.webhook_secret.as_deref())
            .map(|s| (s, "channel configuration"))
    };
    let secret = request_secret
        .filter(|s| !s.is_empty())
        .map(|s| (s, "request"))
        .or_else(configured_secret);

    let Some((secret_str, secret_source)) = secret else {
        log::warn!(
            "[req:{request_id}] test-send for channel {channel_name} needs a signature but no secret is available"
        );
        return Err((
            StatusCode::BAD_REQUEST,
            "No secret provided and no sign_secret or webhook_secret configured for this channel",
        )
            .into_response());
    };

    let signature = crypto::hmac_sha256_hex(secret_str.as_bytes(), body_bytes);

    let Some(tmpl) = forward_cfg.sign_template.as_deref() else {
        log::debug!(
            "[req:{request_id}] test-send: signed with the secret from the {secret_source}, template not configured"
        );
        return Ok(signature);
    };

    let header_value = crypto::render_sign_template(tmpl, &signature).map_err(|e| {
        log::error!("[req:{request_id}] sign-template render failed for channel '{channel_name}': {e}");
        (StatusCode::INTERNAL_SERVER_ERROR, "Template error").into_response()
    })?;

    log::debug!(
        "[req:{request_id}] test-send: signed with the secret from the {secret_source}, template applied"
    );
    Ok(header_value)
}

pub async fn test_send_route(
    State(state): State<Arc<AppState>>,
    Extension(request_id): Extension<RequestId>,
    Path(channel_name): Path<String>,
    headers: HeaderMap,
    Json(req): Json<TestSendRequest>,
) -> impl IntoResponse {
    let channel_config = match authorize_test_send(&state, &request_id, &channel_name, &headers) {
        Ok(channel_config) => channel_config,
        Err(response) => return response,
    };

    log::info!(
        "[req:{}] request to test-send webhook for channel: {}",
        request_id,
        channel_name
    );

    let Some(forward_cfg) = &channel_config.forward else {
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
        let header_value = match sign_test_payload(
            channel_config,
            forward_cfg,
            &request_id,
            req.secret.as_deref(),
            &body_bytes,
        ) {
            Ok(header_value) => header_value,
            Err(response) => return response,
        };
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
