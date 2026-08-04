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
use crate::middleware::request_id::RequestId;
use kwp_lib::domain::webhook::model::WebhookChannel;

#[derive(Serialize)]
pub struct WebhookListItemDto {
    pub id: i64,
    pub headers: HashMap<String, String>,
    pub payload: Value,
    pub received_at: i64,
}

pub async fn list_webhooks_route(
    State(state): State<Arc<AppState>>,
    Extension(request_id): Extension<RequestId>,
    Path(channel_name): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let bearer = headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));

    let bearer = match bearer {
        Some(b) => b,
        None => {
            log::warn!(
                "[req:{}] missing or invalid Authorization header for list on channel: {}",
                request_id,
                channel_name
            );
            return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
        }
    };

    match state.config.find_channel_by_token(bearer) {
        Some(c) => {
            if c.name != channel_name {
                log::warn!(
                    "[req:{}] token for channel '{}' used to list channel '{}'",
                    request_id,
                    c.name,
                    channel_name
                );
                return (StatusCode::FORBIDDEN, "Forbidden").into_response();
            }
        }
        None => {
            if !state.config.is_ui_token(bearer) {
                log::warn!(
                    "[req:{}] invalid token for list request on channel: {}",
                    request_id,
                    channel_name
                );
                return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
            }
            if state.config.find_channel_by_name(&channel_name).is_none() {
                return (StatusCode::NOT_FOUND, "Channel not found").into_response();
            }
        }
    };

    let channel = WebhookChannel::new(channel_name.clone());

    match state.webhook_service.list_webhooks(&channel).await {
        Ok(webhooks) => {
            log::debug!(
                "[req:{}] listed {} webhooks for channel: {}",
                request_id,
                webhooks.len(),
                channel_name
            );
            let dtos: Vec<WebhookListItemDto> = webhooks
                .into_iter()
                .filter_map(|w| {
                    Some(WebhookListItemDto {
                        id: w.id?,
                        headers: w.headers,
                        payload: serde_json::from_slice(&w.payload)
                            .unwrap_or(serde_json::Value::Null),
                        received_at: w.received_at,
                    })
                })
                .collect();
            (StatusCode::OK, Json(dtos)).into_response()
        }
        Err(e) if e.is_busy() => {
            log::warn!(
                "[req:{}] storage busy listing webhooks for {}: {}",
                request_id,
                channel_name,
                e
            );
            crate::route::storage_busy_response()
        }
        Err(e) => {
            log::error!(
                "[req:{}] failed to list webhooks for channel {}: {}",
                request_id,
                channel_name,
                e
            );
            (StatusCode::INTERNAL_SERVER_ERROR, "Error").into_response()
        }
    }
}
