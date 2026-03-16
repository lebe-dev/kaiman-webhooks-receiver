use std::sync::Arc;
use std::time::Duration;

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};

use crate::AppState;
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
    Path(channel_name): Path<String>,
    headers: HeaderMap,
    Json(req): Json<TestSendRequest>,
) -> impl IntoResponse {
    log::info!("request to test-send webhook for channel: {}", channel_name);
    log::debug!("test-send request headers: {:?}", headers);

    let bearer = headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));

    let bearer = match bearer {
        Some(b) => b,
        None => {
            log::debug!("test-send: no bearer token found");
            return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
        }
    };

    let channel_config = match state.config.find_channel_by_token(bearer) {
        Some(c) => {
            log::debug!("test-send: found channel by token: {}", c.name);
            if c.name != channel_name {
                log::debug!(
                    "test-send: token channel mismatch - token: {}, requested: {}",
                    c.name,
                    channel_name
                );
                return (StatusCode::FORBIDDEN, "Forbidden").into_response();
            }
            c
        }
        None => {
            log::debug!(
                "test-send: token not found in channels, checking UI token for channel: {}",
                channel_name
            );
            if !state.config.is_ui_token(bearer) {
                log::debug!("test-send: invalid bearer token (not UI token)");
                return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
            }
            match state.config.find_channel_by_name(&channel_name) {
                Some(c) => {
                    log::debug!("test-send: found channel by name: {}", channel_name);
                    c
                }
                None => {
                    log::debug!("test-send: channel not found: {}", channel_name);
                    return (StatusCode::NOT_FOUND, "Channel not found").into_response();
                }
            }
        }
    };

    let forward_cfg = match &channel_config.forward {
        Some(f) => {
            log::debug!(
                "test-send: forward config - url: {}, timeout: {}s, sign_header: {:?}",
                f.url,
                f.timeout_seconds,
                f.sign_header
            );
            f
        }
        None => {
            log::debug!(
                "test-send: channel {} has no forward configuration",
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
    log::debug!(
        "test-send: payload size: {} bytes, payload: {:?}",
        body_bytes.len(),
        req.payload
    );

    let mut request = state
        .http_client
        .post(&forward_cfg.url)
        .header("Content-Type", "application/json")
        .timeout(Duration::from_secs(forward_cfg.timeout_seconds));
    log::debug!("test-send: created base request to {}", forward_cfg.url);

    if let Some(sign_header) = &forward_cfg.sign_header {
        log::debug!("test-send: sign_header configured: {}", sign_header);
        let secret_str = match req.secret.as_deref().filter(|s| !s.is_empty()) {
            Some(s) => {
                log::debug!("test-send: using request-provided secret");
                s.to_string()
            }
            None => match forward_cfg
                .sign_secret
                .as_deref()
                .or(channel_config.webhook_secret.as_deref())
            {
                Some(s) => {
                    log::debug!(
                        "test-send: using configured secret (sign_secret or webhook_secret)"
                    );
                    s.to_string()
                }
                None => {
                    log::debug!("test-send: no secret available for signing");
                    return (
                        StatusCode::BAD_REQUEST,
                        "No secret provided and no sign_secret or webhook_secret configured for this channel",
                    )
                        .into_response();
                }
            },
        };

        let signature = crypto::hmac_sha256_hex(secret_str.as_bytes(), &body_bytes);
        log::debug!(
            "test-send: computed signature: {} (first 16 chars: {})",
            signature,
            &signature[..signature.len().min(16)]
        );

        let header_value = if let Some(tmpl) = &forward_cfg.sign_template {
            log::debug!("test-send: applying sign_template: {}", tmpl);
            match crypto::render_sign_template(tmpl, &signature) {
                Ok(v) => {
                    log::debug!("test-send: rendered header value: {}", v);
                    v
                }
                Err(e) => {
                    log::error!(
                        "sign-template render failed for channel '{}': {}",
                        channel_name,
                        e
                    );
                    return (StatusCode::INTERNAL_SERVER_ERROR, "Template error").into_response();
                }
            }
        } else {
            log::debug!("test-send: no sign_template, using signature directly");
            signature
        };

        request = request.header(sign_header.as_str(), &header_value);
        log::debug!(
            "test-send: added {} header (value length: {})",
            sign_header,
            header_value.len()
        );
    } else {
        log::debug!("test-send: no sign_header configured");
    }

    log::debug!("test-send: sending request to {}", forward_cfg.url);
    let response = match request.body(body_bytes).send().await {
        Ok(r) => {
            log::debug!("test-send: got response from target service");
            r
        }
        Err(e) => {
            log::warn!(
                "test-send to {} failed for channel '{}': {}",
                forward_cfg.url,
                channel_name,
                e
            );
            log::debug!("test-send error details: {:?}", e);
            return (
                StatusCode::BAD_GATEWAY,
                format!("Failed to reach target: {e}"),
            )
                .into_response();
        }
    };

    let status = response.status().as_u16();
    let response_headers = response.headers().clone();
    log::debug!(
        "test-send: response status={}, headers: {:?}",
        status,
        response_headers
    );

    let body = response.text().await.unwrap_or_default();
    log::debug!(
        "test-send: response body size: {} bytes, body: {}",
        body.len(),
        if body.len() > 500 {
            format!("{}...", &body[..500])
        } else {
            body.clone()
        }
    );

    log::info!(
        "test-send to channel '{}' returned status={}",
        channel_name,
        status
    );

    (StatusCode::OK, Json(TestSendResponse { status, body })).into_response()
}
