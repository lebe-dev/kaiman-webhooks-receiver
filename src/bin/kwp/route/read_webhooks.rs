use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    Extension, Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use serde::Serialize;
use serde_json::Value;

use crate::AppState;
use crate::middleware::client_ip::ClientIp;
use kwp_lib::domain::webhook::model::WebhookChannel;

#[derive(Serialize)]
pub struct WebhookDto {
    pub headers: HashMap<String, String>,
    pub payload: Value,
    pub received_at: i64,
}

pub async fn read_webhooks_route(
    State(state): State<Arc<AppState>>,
    Extension(client_ip): Extension<ClientIp>,
    headers: HeaderMap,
    Path(channel_name): Path<String>,
) -> impl IntoResponse {
    log::debug!(
        "read webhooks request from {} for channel: {}",
        client_ip.0,
        channel_name
    );
    log::info!("request to read webhooks for channel: {}", channel_name);

    let bearer = headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));

    let bearer = match bearer {
        Some(b) => b,
        None => {
            log::warn!(
                "missing or invalid Authorization header for channel: {}",
                channel_name
            );
            return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
        }
    };

    let channel_config = match state.config.find_channel_by_token(bearer) {
        Some(c) => c,
        None => {
            log::warn!("invalid token provided for channel: {}", channel_name);
            return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
        }
    };

    if channel_config.name != channel_name {
        log::warn!(
            "token for channel: {} was used to attempt access to channel: {}",
            channel_config.name,
            channel_name
        );
        return (StatusCode::FORBIDDEN, "Forbidden").into_response();
    }

    let channel = WebhookChannel::new(channel_name.clone());

    match state
        .webhook_service
        .read_and_delete_webhooks(&channel)
        .await
    {
        Ok(webhooks) => {
            log::info!(
                "successfully read {} webhooks for channel: {}",
                webhooks.len(),
                channel_name
            );
            let dtos: Vec<WebhookDto> = webhooks
                .into_iter()
                .map(|w| WebhookDto {
                    headers: w.headers,
                    payload: w.payload,
                    received_at: w.received_at,
                })
                .collect();
            (StatusCode::OK, Json(dtos)).into_response()
        }
        Err(e) => {
            log::error!("Failed to read webhooks: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "Error").into_response()
        }
    }
}
