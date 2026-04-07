use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use serde::Serialize;
use serde_json::Value;

use crate::AppState;
use kwp_lib::domain::crypto;
use kwp_lib::domain::webhook::model::{ChannelForwardStatus, WebhookChannel};

fn extract_bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
}

fn authorize_channel(
    state: &AppState,
    bearer: &str,
    channel_name: &str,
) -> Result<(), (StatusCode, &'static str)> {
    match state.config.find_channel_by_token(bearer) {
        Some(c) => {
            if c.name != channel_name {
                return Err((StatusCode::FORBIDDEN, "Forbidden"));
            }
            Ok(())
        }
        None => {
            if !state.config.is_ui_token(bearer) {
                return Err((StatusCode::UNAUTHORIZED, "Unauthorized"));
            }
            if state.config.find_channel_by_name(channel_name).is_none() {
                return Err((StatusCode::NOT_FOUND, "Channel not found"));
            }
            Ok(())
        }
    }
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

#[derive(Serialize)]
pub struct QueueItemDto {
    pub id: i64,
    pub headers: HashMap<String, String>,
    pub payload: Value,
    pub received_at: i64,
    pub forward_attempts: i64,
    pub last_attempt_at: Option<i64>,
    pub last_attempt_error: Option<String>,
}

#[derive(Serialize)]
pub struct QueueResponse {
    pub status: ChannelForwardStatus,
    pub items: Vec<QueueItemDto>,
}

pub async fn get_queue_route(
    State(state): State<Arc<AppState>>,
    Path(channel_name): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    log::info!("request to get queue for channel: {}", channel_name);

    let bearer = match extract_bearer(&headers) {
        Some(b) => b,
        None => return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response(),
    };

    if let Err((status, msg)) = authorize_channel(&state, bearer, &channel_name) {
        return (status, msg).into_response();
    }

    let channel_cfg = match state.config.find_channel_by_name(&channel_name) {
        Some(c) => c,
        None => return (StatusCode::NOT_FOUND, "Channel not found").into_response(),
    };

    if channel_cfg.forward.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            "Channel has no forward configuration",
        )
            .into_response();
    }

    let channel = WebhookChannel::new(channel_name.clone());

    let webhooks = match state.webhook_service.list_queue(&channel).await {
        Ok(w) => w,
        Err(e) => {
            log::error!("Failed to list queue for channel {}: {}", channel_name, e);
            return (StatusCode::INTERNAL_SERVER_ERROR, "Error").into_response();
        }
    };

    let items: Vec<QueueItemDto> = webhooks
        .into_iter()
        .filter_map(|w| {
            Some(QueueItemDto {
                id: w.id?,
                headers: w.headers,
                payload: serde_json::from_slice(&w.payload).unwrap_or(Value::Null),
                received_at: w.received_at,
                forward_attempts: w.forward_attempts,
                last_attempt_at: w.last_attempt_at,
                last_attempt_error: w.last_attempt_error,
            })
        })
        .collect();

    let mut status = state
        .forward_statuses
        .read()
        .ok()
        .and_then(|map| map.get(&channel_name).cloned())
        .unwrap_or_else(ChannelForwardStatus::new);

    status.queue_size = items.len() as i64;

    (StatusCode::OK, Json(QueueResponse { status, items })).into_response()
}

pub async fn pause_queue_route(
    State(state): State<Arc<AppState>>,
    Path(channel_name): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    log::info!("request to pause queue for channel: {}", channel_name);

    let bearer = match extract_bearer(&headers) {
        Some(b) => b,
        None => return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response(),
    };

    if let Err((status, msg)) = authorize_channel(&state, bearer, &channel_name) {
        return (status, msg).into_response();
    }

    let mut map = match state.forward_statuses.write() {
        Ok(m) => m,
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "Error").into_response(),
    };

    match map.get_mut(&channel_name) {
        Some(status) => {
            status.paused = true;
            log::info!("paused queue for channel: {}", channel_name);
            StatusCode::NO_CONTENT.into_response()
        }
        None => (
            StatusCode::BAD_REQUEST,
            "Channel has no forward configuration",
        )
            .into_response(),
    }
}

pub async fn resume_queue_route(
    State(state): State<Arc<AppState>>,
    Path(channel_name): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    log::info!("request to resume queue for channel: {}", channel_name);

    let bearer = match extract_bearer(&headers) {
        Some(b) => b,
        None => return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response(),
    };

    if let Err((status, msg)) = authorize_channel(&state, bearer, &channel_name) {
        return (status, msg).into_response();
    }

    let mut map = match state.forward_statuses.write() {
        Ok(m) => m,
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "Error").into_response(),
    };

    match map.get_mut(&channel_name) {
        Some(status) => {
            status.paused = false;
            log::info!("resumed queue for channel: {}", channel_name);
            StatusCode::NO_CONTENT.into_response()
        }
        None => (
            StatusCode::BAD_REQUEST,
            "Channel has no forward configuration",
        )
            .into_response(),
    }
}

pub async fn clear_queue_route(
    State(state): State<Arc<AppState>>,
    Path(channel_name): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    log::info!("request to clear queue for channel: {}", channel_name);

    let bearer = match extract_bearer(&headers) {
        Some(b) => b,
        None => return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response(),
    };

    if let Err((status, msg)) = authorize_channel(&state, bearer, &channel_name) {
        return (status, msg).into_response();
    }

    let channel = WebhookChannel::new(channel_name.clone());

    match state.webhook_service.clear_queue(&channel).await {
        Ok(deleted) => {
            log::info!(
                "cleared {} webhooks from queue for channel: {}",
                deleted,
                channel_name
            );

            if let Ok(mut map) = state.forward_statuses.write()
                && let Some(status) = map.get_mut(&channel_name)
            {
                status.queue_size = 0;
            }

            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => {
            log::error!(
                "Failed to clear queue for channel {}: {}",
                channel_name,
                e
            );
            (StatusCode::INTERNAL_SERVER_ERROR, "Error").into_response()
        }
    }
}

#[derive(Serialize)]
pub struct RetryResponse {
    pub success: bool,
    pub status_code: Option<u16>,
    pub body: Option<String>,
    pub error: Option<String>,
}

pub async fn retry_webhook_route(
    State(state): State<Arc<AppState>>,
    Path((channel_name, webhook_id)): Path<(String, i64)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    log::info!(
        "request to retry webhook {} for channel: {}",
        webhook_id,
        channel_name
    );

    let bearer = match extract_bearer(&headers) {
        Some(b) => b,
        None => return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response(),
    };

    if let Err((status, msg)) = authorize_channel(&state, bearer, &channel_name) {
        return (status, msg).into_response();
    }

    let channel_cfg = match state.config.find_channel_by_name(&channel_name) {
        Some(c) => c,
        None => return (StatusCode::NOT_FOUND, "Channel not found").into_response(),
    };

    let forward_cfg = match &channel_cfg.forward {
        Some(f) => f,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                "Channel has no forward configuration",
            )
                .into_response()
        }
    };

    let webhook = match state.webhook_service.get_webhook(webhook_id).await {
        Ok(Some(w)) => w,
        Ok(None) => return (StatusCode::NOT_FOUND, "Webhook not found").into_response(),
        Err(e) => {
            log::error!("Failed to get webhook {}: {}", webhook_id, e);
            return (StatusCode::INTERNAL_SERVER_ERROR, "Error").into_response();
        }
    };

    if webhook.channel.as_str() != channel_name {
        return (StatusCode::NOT_FOUND, "Webhook not found in this channel").into_response();
    }

    let body_bytes = webhook.payload.clone();

    let timeout = Duration::from_secs(forward_cfg.timeout_seconds);
    let mut request = state
        .http_client
        .post(&forward_cfg.url)
        .timeout(timeout)
        .header("content-type", "application/json")
        .body(body_bytes.clone());

    for (key, value) in &webhook.headers {
        if state.config.ignored_headers.contains(key) {
            continue;
        }
        if forward_cfg
            .sign_header
            .as_deref()
            .is_some_and(|h| h.eq_ignore_ascii_case(key))
        {
            continue;
        }
        request = request.header(key, value);
    }

    if let Some(sign_header) = &forward_cfg.sign_header {
        let effective_secret = forward_cfg
            .sign_secret
            .as_deref()
            .or(channel_cfg.webhook_secret.as_deref());
        let Some(sign_secret) = effective_secret else {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "sign-header configured but no secret available",
            )
                .into_response();
        };
        let sig = crypto::hmac_sha256_hex(sign_secret.as_bytes(), &body_bytes);
        let header_value = match forward_cfg.sign_template.as_deref() {
            Some(tmpl) => match crypto::render_sign_template(tmpl, &sig) {
                Ok(v) => v,
                Err(e) => {
                    log::error!(
                        "sign-template render failed for channel '{}': {}",
                        channel_name,
                        e
                    );
                    return (StatusCode::INTERNAL_SERVER_ERROR, "Template error").into_response();
                }
            },
            None => sig,
        };
        request = request.header(sign_header.as_str(), header_value);
    }

    match request.send().await {
        Err(e) => {
            let mut cause = format!("{e}");
            let mut src: &dyn std::error::Error = &e;
            while let Some(next) = src.source() {
                cause.push_str(&format!(": {next}"));
                src = next;
            }
            log::warn!(
                "retry webhook {} for channel '{}' failed: {}",
                webhook_id,
                channel_name,
                cause
            );

            let error_msg = format!("network error: {cause}");
            let _ = state
                .webhook_service
                .increment_forward_attempts(webhook_id, &error_msg)
                .await;

            if let Ok(mut map) = state.forward_statuses.write()
                && let Some(status) = map.get_mut(&channel_name)
            {
                status.last_error_at = Some(now_unix());
                status.last_error_message = Some(error_msg.clone());
            }

            (
                StatusCode::OK,
                Json(RetryResponse {
                    success: false,
                    status_code: None,
                    body: None,
                    error: Some(error_msg),
                }),
            )
                .into_response()
        }
        Ok(resp) => {
            let status_code = resp.status().as_u16();
            let body = resp
                .text()
                .await
                .unwrap_or_else(|e| format!("<failed to read body: {e}>"));

            if status_code == forward_cfg.expected_status {
                log::info!(
                    "retry webhook {} for channel '{}' succeeded (status={})",
                    webhook_id,
                    channel_name,
                    status_code
                );

                let _ = state
                    .webhook_service
                    .delete_webhook(&WebhookChannel::new(channel_name.clone()), webhook_id)
                    .await;

                if let Ok(mut map) = state.forward_statuses.write()
                    && let Some(status) = map.get_mut(&channel_name)
                {
                    status.last_success_at = Some(now_unix());
                    status.queue_size = (status.queue_size - 1).max(0);
                }

                (
                    StatusCode::OK,
                    Json(RetryResponse {
                        success: true,
                        status_code: Some(status_code),
                        body: Some(body),
                        error: None,
                    }),
                )
                    .into_response()
            } else {
                let error_msg = format!("HTTP {}: {}", status_code, body);
                log::warn!(
                    "retry webhook {} for channel '{}' got unexpected status {}: {}",
                    webhook_id,
                    channel_name,
                    status_code,
                    body
                );

                let _ = state
                    .webhook_service
                    .increment_forward_attempts(webhook_id, &error_msg)
                    .await;

                if let Ok(mut map) = state.forward_statuses.write()
                    && let Some(status) = map.get_mut(&channel_name)
                {
                    status.last_error_at = Some(now_unix());
                    status.last_error_message = Some(error_msg);
                }

                (
                    StatusCode::OK,
                    Json(RetryResponse {
                        success: false,
                        status_code: Some(status_code),
                        body: Some(body),
                        error: None,
                    }),
                )
                    .into_response()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, RwLock};

    use axum::{
        Router,
        body::Body,
        http::{self, Request, StatusCode},
        routing::{get, post},
    };
    use tower::ServiceExt;

    use kwp_lib::domain::config::model::{
        AppConfig, SecretType, WebhookChannelConfig, WebhookForwardConfig,
    };
    use kwp_lib::domain::webhook::model::ChannelForwardStatus;
    use kwp_lib::domain::webhook::service::WebhookServiceImpl;
    use kwp_lib::outbound::sqlite::Sqlite;

    use crate::AppState;
    use crate::route::queue::{
        clear_queue_route, get_queue_route, pause_queue_route, resume_queue_route,
    };

    fn make_channel(name: &str) -> WebhookChannelConfig {
        WebhookChannelConfig {
            name: name.to_string(),
            api_read_token: "read-token".to_string(),
            webhook_secret: None,
            secret_header: None,
            secret_type: SecretType::Plain,
            secret_extract_template: None,
            secret_sign_template: None,
            forward: None,
            max_body_size: None,
            allowed_ips: None,
        }
    }

    fn make_channel_with_forward(name: &str) -> WebhookChannelConfig {
        let mut ch = make_channel(name);
        ch.forward = Some(WebhookForwardConfig {
            url: "https://example.com/hook".to_string(),
            interval_seconds: 10,
            expected_status: 200,
            timeout_seconds: 15,
            sign_header: None,
            sign_secret: None,
            sign_template: None,
        });
        ch
    }

    async fn build_app(channels: Vec<WebhookChannelConfig>) -> (Router, Arc<AppState>) {
        let db = Sqlite::new("sqlite::memory:").await.unwrap();
        let webhook_service = WebhookServiceImpl::new(db);

        let mut forward_statuses_map = HashMap::new();
        for ch in &channels {
            if ch.forward.is_some() {
                forward_statuses_map.insert(ch.name.clone(), ChannelForwardStatus::new());
            }
        }

        let config = AppConfig {
            bind: "127.0.0.1:3000".to_string(),
            log_level: "debug".to_string(),
            log_target: "stdout".to_string(),
            data_path: "./data".to_string(),
            db_cnn: "sqlite::memory:".to_string(),
            channels,
            default_body_limit: 1024,
            ui_enabled: true,
            api_enabled: true,
            ui_access_token: Some("ui-token".to_string()),
            ignored_headers: vec![],
            trusted_proxies: vec![],
            metrics_enabled: false,
        };

        let app_state = Arc::new(AppState {
            config,
            webhook_service,
            metrics_handle: None,
            http_client: reqwest::Client::new(),
            forward_statuses: Arc::new(RwLock::new(forward_statuses_map)),
        });

        let router = Router::new()
            .route("/api/webhook/{channel}/queue", get(get_queue_route))
            .route(
                "/api/webhook/{channel}/queue/pause",
                post(pause_queue_route),
            )
            .route(
                "/api/webhook/{channel}/queue/resume",
                post(resume_queue_route),
            )
            .route(
                "/api/webhook/{channel}/queue/clear",
                post(clear_queue_route),
            )
            .with_state(app_state.clone());

        (router, app_state)
    }

    #[tokio::test]
    async fn test_get_queue_without_auth_returns_401() {
        let (app, _) = build_app(vec![make_channel_with_forward("test")]).await;
        let req = Request::builder()
            .method("GET")
            .uri("/api/webhook/test/queue")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_get_queue_with_ui_token_returns_200() {
        let (app, _) = build_app(vec![make_channel_with_forward("test")]).await;
        let req = Request::builder()
            .method("GET")
            .uri("/api/webhook/test/queue")
            .header(http::header::AUTHORIZATION, "Bearer ui-token")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_get_queue_no_forward_returns_400() {
        let (app, _) = build_app(vec![make_channel("test")]).await;
        let req = Request::builder()
            .method("GET")
            .uri("/api/webhook/test/queue")
            .header(http::header::AUTHORIZATION, "Bearer ui-token")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_get_queue_wrong_channel_token_returns_403() {
        let mut ch1 = make_channel_with_forward("ch1");
        ch1.api_read_token = "ch1-token".to_string();
        let mut ch2 = make_channel_with_forward("ch2");
        ch2.api_read_token = "ch2-token".to_string();

        let (app, _) = build_app(vec![ch1, ch2]).await;
        let req = Request::builder()
            .method("GET")
            .uri("/api/webhook/ch2/queue")
            .header(http::header::AUTHORIZATION, "Bearer ch1-token")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn test_pause_queue_returns_204() {
        let (app, state) = build_app(vec![make_channel_with_forward("test")]).await;
        let req = Request::builder()
            .method("POST")
            .uri("/api/webhook/test/queue/pause")
            .header(http::header::AUTHORIZATION, "Bearer ui-token")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);

        let map = state.forward_statuses.read().unwrap();
        assert!(map.get("test").unwrap().paused);
    }

    #[tokio::test]
    async fn test_resume_queue_returns_204() {
        let (app, state) = build_app(vec![make_channel_with_forward("test")]).await;

        // First pause
        {
            let mut map = state.forward_statuses.write().unwrap();
            map.get_mut("test").unwrap().paused = true;
        }

        let req = Request::builder()
            .method("POST")
            .uri("/api/webhook/test/queue/resume")
            .header(http::header::AUTHORIZATION, "Bearer ui-token")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);

        let map = state.forward_statuses.read().unwrap();
        assert!(!map.get("test").unwrap().paused);
    }

    #[tokio::test]
    async fn test_pause_no_forward_returns_400() {
        let (app, _) = build_app(vec![make_channel("test")]).await;
        let req = Request::builder()
            .method("POST")
            .uri("/api/webhook/test/queue/pause")
            .header(http::header::AUTHORIZATION, "Bearer ui-token")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_clear_queue_returns_204() {
        let (app, _) = build_app(vec![make_channel_with_forward("test")]).await;
        let req = Request::builder()
            .method("POST")
            .uri("/api/webhook/test/queue/clear")
            .header(http::header::AUTHORIZATION, "Bearer ui-token")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn test_get_queue_nonexistent_channel_returns_404() {
        let (app, _) = build_app(vec![make_channel_with_forward("test")]).await;
        let req = Request::builder()
            .method("GET")
            .uri("/api/webhook/nonexistent/queue")
            .header(http::header::AUTHORIZATION, "Bearer ui-token")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_get_queue_with_channel_token_returns_200() {
        let mut ch = make_channel_with_forward("test");
        ch.api_read_token = "test-token".to_string();

        let (app, _) = build_app(vec![ch]).await;
        let req = Request::builder()
            .method("GET")
            .uri("/api/webhook/test/queue")
            .header(http::header::AUTHORIZATION, "Bearer test-token")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
