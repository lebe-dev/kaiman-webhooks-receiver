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

    let bearer = headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));

    let bearer = match bearer {
        Some(b) => b,
        None => {
            return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
        }
    };

    let channel_config = match state.config.find_channel_by_token(bearer) {
        Some(c) => {
            if c.name != channel_name {
                return (StatusCode::FORBIDDEN, "Forbidden").into_response();
            }
            c
        }
        None => {
            if !state.config.is_ui_token(bearer) {
                return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
            }
            match state.config.find_channel_by_name(&channel_name) {
                Some(c) => c,
                None => return (StatusCode::NOT_FOUND, "Channel not found").into_response(),
            }
        }
    };

    let forward_cfg = match &channel_config.forward {
        Some(f) => f,
        None => {
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

    if let Some(sign_header) = &forward_cfg.sign_header {
        let secret_str = match req.secret.as_deref().filter(|s| !s.is_empty()) {
            Some(s) => s.to_string(),
            None => match forward_cfg
                .sign_secret
                .as_deref()
                .or(channel_config.webhook_secret.as_deref())
            {
                Some(s) => s.to_string(),
                None => {
                    return (
                        StatusCode::BAD_REQUEST,
                        "No secret provided and no sign_secret or webhook_secret configured for this channel",
                    )
                        .into_response();
                }
            },
        };

        let signature = crypto::hmac_sha256_hex(secret_str.as_bytes(), &body_bytes);

        let header_value = if let Some(tmpl) = &forward_cfg.sign_template {
            match crypto::render_sign_template(tmpl, &signature) {
                Ok(v) => v,
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
            signature
        };

        request = request.header(sign_header.as_str(), header_value);
    }

    let response = match request.body(body_bytes).send().await {
        Ok(r) => r,
        Err(e) => {
            log::warn!(
                "test-send to {} failed for channel '{}': {}",
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

    log::info!(
        "test-send to channel '{}' returned status={}",
        channel_name,
        status
    );

    (StatusCode::OK, Json(TestSendResponse { status, body })).into_response()
}
