use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use subtle::ConstantTimeEq;

use crate::AppState;
use kwp_lib::domain::webhook::model::WebhookChannel;

pub async fn receive_webhook_route(
    State(state): State<Arc<AppState>>,
    Path(channel_name): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    log::info!("received webhook for channel: {}", channel_name);

    let channel_config = match state.config.find_channel_by_name(&channel_name) {
        Some(c) => c,
        None => {
            log::warn!("webhook received for unknown channel: {}", channel_name);
            return (StatusCode::NOT_FOUND, "Channel not found").into_response();
        }
    };

    if let (Some(secret), Some(header_name)) = (
        &channel_config.webhook_secret,
        &channel_config.secret_header,
    ) {
        log::debug!("verifying webhook secret for channel: {}", channel_name);
        let provided = headers
            .get(header_name.as_str())
            .and_then(|v| v.to_str().ok());

        match provided {
            Some(token) if token.as_bytes().ct_eq(secret.as_bytes()).into() => {
                log::debug!("webhook secret verified for channel: {}", channel_name);
            }
            _ => {
                log::warn!("invalid webhook secret for channel: {}", channel_name);
                return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
            }
        }
    }

    let effective_limit = channel_config
        .max_body_size
        .unwrap_or(state.config.default_body_limit);

    if body.len() > effective_limit {
        log::warn!(
            "request body too large for channel {}: {} bytes > limit {} bytes",
            channel_name,
            body.len(),
            effective_limit
        );
        return (StatusCode::PAYLOAD_TOO_LARGE, "Payload Too Large").into_response();
    }

    let content_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !content_type.starts_with("application/json") {
        log::warn!(
            "unsupported content type for channel {}: {}",
            channel_name,
            content_type
        );
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Expected application/json",
        )
            .into_response();
    }

    let payload: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            log::warn!("invalid JSON body for channel {}: {}", channel_name, e);
            return (StatusCode::UNPROCESSABLE_ENTITY, "Invalid JSON").into_response();
        }
    };

    log::debug!("filtering headers for channel: {}", channel_name);
    let hop_by_hop = [
        "host",
        "content-length",
        "transfer-encoding",
        "connection",
        "content-type",
    ];
    let forwarded_headers: HashMap<String, String> = headers
        .iter()
        .filter_map(|(k, v)| {
            let key = k.as_str().to_lowercase();
            if hop_by_hop.contains(&key.as_str()) {
                return None;
            }
            v.to_str().ok().map(|val| (key, val.to_string()))
        })
        .collect();

    let channel = WebhookChannel::new(channel_name.clone());

    match state
        .webhook_service
        .receive_webhook(channel, forwarded_headers, payload)
        .await
    {
        Ok(()) => {
            log::info!(
                "webhook successfully processed and stored for channel: {}",
                channel_name
            );
            (StatusCode::OK, "OK").into_response()
        }
        Err(e) => {
            log::error!(
                "failed to store webhook for channel {}: {}",
                channel_name,
                e
            );
            (StatusCode::INTERNAL_SERVER_ERROR, "Error").into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::{
        Router,
        body::Body,
        http::{self, Request, StatusCode},
        routing::{get, post},
    };
    use tower::ServiceExt;

    use kwp_lib::domain::config::model::{AppConfig, WebhookChannelConfig};
    use kwp_lib::domain::webhook::service::WebhookServiceImpl;
    use kwp_lib::outbound::sqlite::Sqlite;

    use crate::AppState;
    use crate::route::{
        read_webhooks::read_webhooks_route, receive_webhook::receive_webhook_route,
    };

    fn make_channel(name: &str, max_body_size: Option<usize>) -> WebhookChannelConfig {
        WebhookChannelConfig {
            name: name.to_string(),
            api_read_token: "read-token".to_string(),
            webhook_secret: None,
            secret_header: None,
            forward: None,
            max_body_size,
        }
    }

    fn make_channel_with_secret(name: &str, secret: &str, header: &str) -> WebhookChannelConfig {
        WebhookChannelConfig {
            name: name.to_string(),
            api_read_token: "read-token".to_string(),
            webhook_secret: Some(secret.to_string()),
            secret_header: Some(header.to_string()),
            forward: None,
            max_body_size: None,
        }
    }

    async fn build_app(channels: Vec<WebhookChannelConfig>, default_body_limit: usize) -> Router {
        let config = AppConfig {
            bind: "0.0.0.0:8080".to_string(),
            log_level: "info".to_string(),
            log_target: "stdout".to_string(),
            data_path: "./data".to_string(),
            db_cnn: "sqlite::memory:".to_string(),
            channels,
            default_body_limit,
        };
        let db = Sqlite::new("sqlite::memory:").await.unwrap();
        let state = Arc::new(AppState {
            config,
            webhook_service: WebhookServiceImpl::new(db),
        });
        Router::new()
            .route("/api/webhook/{channel}", post(receive_webhook_route))
            .route("/api/webhook/{channel}", get(read_webhooks_route))
            .with_state(state)
    }

    async fn send_json(
        app: Router,
        channel: &str,
        body: Vec<u8>,
        content_type: Option<&str>,
    ) -> StatusCode {
        let ct = content_type.unwrap_or("application/json");
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/webhook/{}", channel))
            .header(http::header::CONTENT_TYPE, ct)
            .body(Body::from(body))
            .unwrap();
        app.oneshot(req).await.unwrap().status()
    }

    #[tokio::test]
    async fn test_body_within_default_limit_returns_200() {
        let app = build_app(vec![make_channel("test", None)], 1024).await;
        assert_eq!(
            send_json(app, "test", b"{}".to_vec(), None).await,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn test_body_exceeds_default_limit_returns_413() {
        let app = build_app(vec![make_channel("test", None)], 10).await;
        let body = b"\"hello wow\"".to_vec(); // 11 bytes
        assert_eq!(
            send_json(app, "test", body, None).await,
            StatusCode::PAYLOAD_TOO_LARGE
        );
    }

    #[tokio::test]
    async fn test_body_within_channel_override_larger_than_default_returns_200() {
        // channel override = 500, default = 10 — 100-byte body fits in channel override
        let app = build_app(vec![make_channel("test", Some(500))], 10).await;
        let mut json_body = b"\"".to_vec();
        json_body.extend_from_slice(&vec![b'a'; 98]);
        json_body.push(b'"'); // 100 bytes total
        assert_eq!(
            send_json(app, "test", json_body, None).await,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn test_body_exceeds_channel_override_smaller_than_default_returns_413() {
        // channel override = 5, default = 1024 — 7-byte body exceeds channel override
        let app = build_app(vec![make_channel("test", Some(5))], 1024).await;
        let body = b"\"hello\"".to_vec(); // 7 bytes
        assert_eq!(
            send_json(app, "test", body, None).await,
            StatusCode::PAYLOAD_TOO_LARGE
        );
    }

    #[tokio::test]
    async fn test_null_json_within_limit_returns_200() {
        let app = build_app(vec![make_channel("test", None)], 1024).await;
        assert_eq!(
            send_json(app, "test", b"null".to_vec(), None).await,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn test_invalid_json_within_limit_returns_422() {
        let app = build_app(vec![make_channel("test", None)], 1024).await;
        assert_eq!(
            send_json(app, "test", b"not json".to_vec(), None).await,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }

    #[tokio::test]
    async fn test_wrong_content_type_returns_415() {
        let app = build_app(vec![make_channel("test", None)], 1024).await;
        assert_eq!(
            send_json(app, "test", b"{}".to_vec(), Some("text/plain")).await,
            StatusCode::UNSUPPORTED_MEDIA_TYPE
        );
    }

    #[tokio::test]
    async fn test_missing_content_type_returns_415() {
        let app = build_app(vec![make_channel("test", None)], 1024).await;
        let req = Request::builder()
            .method("POST")
            .uri("/api/webhook/test")
            .body(Body::from(b"{}".to_vec()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[tokio::test]
    async fn test_unauthenticated_oversized_body_returns_401_not_413() {
        let channel = make_channel_with_secret("secure", "mysecret", "X-Secret");
        let app = build_app(vec![channel], 10).await;
        // body > 10 bytes but no valid secret — expect 401, not 413
        let req = Request::builder()
            .method("POST")
            .uri("/api/webhook/secure")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(b"\"hello world this is big\"".to_vec()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}
