use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    Extension, Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use serde_json::Value;

use crate::AppState;
use crate::middleware::request_id::RequestId;
use kwp_lib::domain::config::model::{WebhookChannelConfig, WebhookForwardConfig};
use kwp_lib::domain::crypto;
use kwp_lib::domain::webhook::backoff::{self, FailureKind};
use kwp_lib::domain::webhook::model::{ChannelForwardStatus, Webhook, WebhookChannel};

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
    /// When the forwarder will try this webhook again; `null` means "due now".
    pub next_attempt_at: Option<i64>,
}

#[derive(Serialize)]
pub struct QueueResponse {
    pub status: ChannelForwardStatus,
    pub items: Vec<QueueItemDto>,
}

pub async fn get_queue_route(
    State(state): State<Arc<AppState>>,
    Extension(request_id): Extension<RequestId>,
    Path(channel_name): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let bearer = match extract_bearer(&headers) {
        Some(b) => b,
        None => return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response(),
    };

    if let Err((status, msg)) = authorize_channel(&state, bearer, &channel_name) {
        return (status, msg).into_response();
    }

    // The UI polls this endpoint, and reading the queue changes nothing, so it stays
    // out of the INFO narrative.
    log::debug!(
        "[req:{}] request to get queue for channel: {}",
        request_id,
        channel_name
    );

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
        Err(e) if e.is_busy() => {
            log::warn!(
                "[req:{}] storage busy listing queue for {}: {}",
                request_id,
                channel_name,
                e
            );
            return crate::route::storage_busy_response();
        }
        Err(e) => {
            log::error!(
                "[req:{}] failed to list queue for channel {}: {}",
                request_id,
                channel_name,
                e
            );
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
                next_attempt_at: w.next_attempt_at,
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
    // The in-memory status only tracks the forwarder's last attempt, so the soonest
    // due time comes from the queue itself. It is only meaningful while every queued
    // webhook waits: one webhook that is due now means the channel is not idle.
    status.next_attempt_at = items
        .iter()
        .map(|item| item.next_attempt_at)
        .collect::<Option<Vec<_>>>()
        .and_then(|due_times| due_times.into_iter().min());

    (StatusCode::OK, Json(QueueResponse { status, items })).into_response()
}

pub async fn pause_queue_route(
    State(state): State<Arc<AppState>>,
    Extension(request_id): Extension<RequestId>,
    Path(channel_name): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let bearer = match extract_bearer(&headers) {
        Some(b) => b,
        None => return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response(),
    };

    if let Err((status, msg)) = authorize_channel(&state, bearer, &channel_name) {
        return (status, msg).into_response();
    }

    let mut map = match state.forward_statuses.write() {
        Ok(m) => m,
        Err(e) => {
            log::error!(
                "[req:{}] forward statuses lock is poisoned: {}",
                request_id,
                e
            );
            return (StatusCode::INTERNAL_SERVER_ERROR, "Error").into_response();
        }
    };

    match map.get_mut(&channel_name) {
        Some(status) => {
            status.paused = true;
            log::info!(
                "[req:{}] paused queue for channel: {}",
                request_id,
                channel_name
            );
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
    Extension(request_id): Extension<RequestId>,
    Path(channel_name): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let bearer = match extract_bearer(&headers) {
        Some(b) => b,
        None => return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response(),
    };

    if let Err((status, msg)) = authorize_channel(&state, bearer, &channel_name) {
        return (status, msg).into_response();
    }

    let mut map = match state.forward_statuses.write() {
        Ok(m) => m,
        Err(e) => {
            log::error!(
                "[req:{}] forward statuses lock is poisoned: {}",
                request_id,
                e
            );
            return (StatusCode::INTERNAL_SERVER_ERROR, "Error").into_response();
        }
    };

    match map.get_mut(&channel_name) {
        Some(status) => {
            status.paused = false;
            log::info!(
                "[req:{}] resumed queue for channel: {}",
                request_id,
                channel_name
            );
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
    Extension(request_id): Extension<RequestId>,
    Path(channel_name): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
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
                "[req:{}] cleared {} webhooks from queue for channel: {}",
                request_id,
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
        Err(e) if e.is_busy() => {
            log::warn!(
                "[req:{}] storage busy clearing queue for {}: {}",
                request_id,
                channel_name,
                e
            );
            crate::route::storage_busy_response()
        }
        Err(e) => {
            log::error!(
                "[req:{}] failed to clear queue for channel {}: {}",
                request_id,
                channel_name,
                e
            );
            (StatusCode::INTERNAL_SERVER_ERROR, "Error").into_response()
        }
    }
}

/// One manual retry of one webhook: everything the steps below share.
///
/// Bundled into a struct because the alternative is half a dozen parameters
/// repeated on every helper — the same reason `record_failure` used to carry an
/// `allow(too_many_arguments)`.
struct RetryContext<'a> {
    state: &'a AppState,
    request_id: &'a RequestId,
    channel_name: &'a str,
    webhook_id: i64,
    webhook: &'a Webhook,
    channel_cfg: &'a WebhookChannelConfig,
    forward_cfg: &'a WebhookForwardConfig,
}

impl RetryContext<'_> {
    /// Rebuilds the forward request the way the background forwarder would: the
    /// stored headers minus the ignored ones and minus the signature header,
    /// which is re-signed here rather than replayed.
    fn build_request(&self) -> Result<reqwest::RequestBuilder, Response> {
        let mut request = self
            .state
            .http_client
            .post(&self.forward_cfg.url)
            .timeout(Duration::from_secs(self.forward_cfg.timeout_seconds))
            .header("content-type", "application/json")
            .body(self.webhook.payload.clone());

        for (key, value) in &self.webhook.headers {
            if self.state.config.ignored_headers.contains(key) {
                continue;
            }
            if self
                .forward_cfg
                .sign_header
                .as_deref()
                .is_some_and(|h| h.eq_ignore_ascii_case(key))
            {
                continue;
            }
            request = request.header(key, value);
        }

        let Some(sign_header) = &self.forward_cfg.sign_header else {
            return Ok(request);
        };
        let header_value = self.sign(&self.webhook.payload)?;
        Ok(request.header(sign_header.as_str(), header_value))
    }

    /// Computes the value of the signature header for `payload`.
    ///
    /// Startup validation guarantees a secret is reachable here, so a missing one
    /// is a bug rather than bad input — hence 500 and `error!`.
    fn sign(&self, payload: &[u8]) -> Result<String, Response> {
        let effective_secret = self
            .forward_cfg
            .sign_secret
            .as_deref()
            .or(self.channel_cfg.webhook_secret.as_deref());
        let Some(sign_secret) = effective_secret else {
            log::error!(
                "[req:{}] channel '{}': sign-header configured but no sign-secret or webhook-secret available",
                self.request_id,
                self.channel_name
            );
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                "sign-header configured but no secret available",
            )
                .into_response());
        };

        let sig = crypto::hmac_sha256_hex(sign_secret.as_bytes(), payload);
        let Some(tmpl) = self.forward_cfg.sign_template.as_deref() else {
            return Ok(sig);
        };
        crypto::render_sign_template(tmpl, &sig).map_err(|e| {
            log::error!(
                "[req:{}] sign-template render failed for channel '{}': {}",
                self.request_id,
                self.channel_name,
                e
            );
            (StatusCode::INTERNAL_SERVER_ERROR, "Template error").into_response()
        })
    }

    /// The request never reached the target: no status code, so the whole error
    /// chain is all the operator gets.
    async fn on_transport_error(&self, error: reqwest::Error) -> Response {
        let mut cause = format!("{error}");
        let mut src: &dyn std::error::Error = &error;
        while let Some(next) = src.source() {
            cause.push_str(&format!(": {next}"));
            src = next;
        }
        log::warn!(
            "[req:{}] retry webhook {} for channel '{}' failed: {}",
            self.request_id,
            self.webhook_id,
            self.channel_name,
            cause
        );

        let error_msg = format!("network error: {cause}");
        self.record_failure(FailureKind::Transient, None, &error_msg)
            .await;

        Self::result(false, None, Some(error_msg), None)
    }

    /// The target answered. Whether that counts as delivery is the channel's
    /// `expected_status` to decide.
    async fn on_response(&self, resp: reqwest::Response) -> Response {
        let status_code = resp.status().as_u16();
        // Read before the body: consuming the response drops the headers.
        let retry_after = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(backoff::parse_retry_after);
        let body = resp
            .text()
            .await
            .unwrap_or_else(|e| format!("<failed to read body: {e}>"));

        if status_code != self.forward_cfg.expected_status {
            log::warn!(
                "[req:{}] retry webhook {} for channel '{}' got unexpected status {}: {}",
                self.request_id,
                self.webhook_id,
                self.channel_name,
                status_code,
                body
            );
            self.record_failure(
                backoff::classify_status(status_code),
                retry_after,
                &format!("HTTP {status_code}: {body}"),
            )
            .await;
            return Self::result(false, Some(status_code), None, Some(body));
        }

        log::info!(
            "[req:{}] retry webhook {} for channel '{}' succeeded (status={})",
            self.request_id,
            self.webhook_id,
            self.channel_name,
            status_code
        );
        self.forget_delivered().await;
        Self::result(true, Some(status_code), None, Some(body))
    }

    /// Removes a webhook the target has accepted, and decrements the queue counter.
    async fn forget_delivered(&self) {
        // A webhook that was delivered but not removed is still queued, so
        // the forwarder will deliver it a second time. The operator has to
        // learn that from here — the duplicate itself looks like a success.
        if let Err(e) = self
            .state
            .webhook_service
            .delete_webhook(
                &WebhookChannel::new(self.channel_name.to_string()),
                self.webhook_id,
            )
            .await
        {
            log::error!(
                "[req:{}] webhook {} for channel '{}' was delivered by manual retry but could not be removed, it will be delivered again: {}",
                self.request_id,
                self.webhook_id,
                self.channel_name,
                e
            );
        }

        if let Ok(mut map) = self.state.forward_statuses.write()
            && let Some(status) = map.get_mut(self.channel_name)
        {
            status.last_success_at = Some(now_unix());
            status.queue_size = (status.queue_size - 1).max(0);
        }
    }

    /// Applies the channel's backoff to a webhook whose manual retry failed.
    ///
    /// Without this a failed manual retry would leave the webhook due immediately,
    /// undoing the delay the forwarder had already assigned to it.
    async fn record_failure(
        &self,
        kind: FailureKind,
        retry_after: Option<Duration>,
        error_msg: &str,
    ) {
        let delay = backoff::next_delay(
            &self.forward_cfg.backoff(&self.state.config.forward_backoff),
            kind,
            self.webhook.forward_attempts + 1,
            retry_after,
            backoff::clock_jitter_unit(),
        );
        let next_attempt_at = now_unix() + delay.as_secs() as i64;

        if let Err(e) = self
            .state
            .webhook_service
            .record_forward_failure(self.webhook_id, error_msg, next_attempt_at)
            .await
        {
            log::error!(
                "[req:{}] failed to record attempt for webhook {}: {}",
                self.request_id,
                self.webhook_id,
                e
            );
        }

        if let Ok(mut map) = self.state.forward_statuses.write()
            && let Some(status) = map.get_mut(self.channel_name)
        {
            status.last_error_at = Some(now_unix());
            status.last_error_message = Some(error_msg.to_string());
        }
    }

    /// A retry always answers 200 — the payload says how the forward itself went.
    fn result(
        success: bool,
        status_code: Option<u16>,
        error: Option<String>,
        body: Option<String>,
    ) -> Response {
        (
            StatusCode::OK,
            Json(RetryResponse {
                success,
                status_code,
                body,
                error,
            }),
        )
            .into_response()
    }
}

#[derive(Serialize)]
pub struct RetryResponse {
    pub success: bool,
    pub status_code: Option<u16>,
    pub body: Option<String>,
    pub error: Option<String>,
}

/// Resolves the channel and the forward target to retry against.
fn find_forward_target<'a>(
    state: &'a AppState,
    channel_name: &str,
) -> Result<(&'a WebhookChannelConfig, &'a WebhookForwardConfig), Response> {
    let Some(channel_cfg) = state.config.find_channel_by_name(channel_name) else {
        return Err((StatusCode::NOT_FOUND, "Channel not found").into_response());
    };
    let Some(forward_cfg) = &channel_cfg.forward else {
        return Err((
            StatusCode::BAD_REQUEST,
            "Channel has no forward configuration",
        )
            .into_response());
    };
    Ok((channel_cfg, forward_cfg))
}

/// Loads the webhook and checks it really belongs to `channel_name` — an id from
/// another channel must not be re-sent through this channel's forward config.
async fn load_webhook_to_retry(
    state: &AppState,
    request_id: &RequestId,
    channel_name: &str,
    webhook_id: i64,
) -> Result<Webhook, Response> {
    let webhook = match state.webhook_service.get_webhook(webhook_id).await {
        Ok(Some(w)) => w,
        Ok(None) => return Err((StatusCode::NOT_FOUND, "Webhook not found").into_response()),
        Err(e) if e.is_busy() => {
            log::warn!("[req:{request_id}] storage busy fetching webhook {webhook_id}: {e}");
            return Err(crate::route::storage_busy_response());
        }
        Err(e) => {
            log::error!("[req:{request_id}] failed to get webhook {webhook_id}: {e}");
            return Err((StatusCode::INTERNAL_SERVER_ERROR, "Error").into_response());
        }
    };

    if webhook.channel.as_str() != channel_name {
        return Err((StatusCode::NOT_FOUND, "Webhook not found in this channel").into_response());
    }
    Ok(webhook)
}

pub async fn retry_webhook_route(
    State(state): State<Arc<AppState>>,
    Extension(request_id): Extension<RequestId>,
    Path((channel_name, webhook_id)): Path<(String, i64)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let Some(bearer) = extract_bearer(&headers) else {
        return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    };

    if let Err((status, msg)) = authorize_channel(&state, bearer, &channel_name) {
        return (status, msg).into_response();
    }

    log::info!(
        "[req:{request_id}] request to retry webhook {webhook_id} for channel: {channel_name}"
    );

    let (channel_cfg, forward_cfg) = match find_forward_target(&state, &channel_name) {
        Ok(target) => target,
        Err(response) => return response,
    };

    let webhook = match load_webhook_to_retry(&state, &request_id, &channel_name, webhook_id).await {
        Ok(webhook) => webhook,
        Err(response) => return response,
    };

    let ctx = RetryContext {
        state: &state,
        request_id: &request_id,
        channel_name: &channel_name,
        webhook_id,
        webhook: &webhook,
        channel_cfg,
        forward_cfg,
    };

    let request = match ctx.build_request() {
        Ok(request) => request,
        Err(response) => return response,
    };

    match request.send().await {
        Err(e) => ctx.on_transport_error(e).await,
        Ok(resp) => ctx.on_response(resp).await,
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
        AppConfig, ForwardBackoffDefaults, SecretType, WebhookChannelConfig, WebhookForwardConfig,
    };
    use kwp_lib::domain::webhook::model::{ChannelForwardStatus, WebhookChannel};
    use kwp_lib::domain::webhook::service::WebhookServiceImpl;
    use kwp_lib::outbound::sqlite::Sqlite;
    use serde_json::Value;

    use crate::AppState;
    use crate::route::queue::{
        clear_queue_route, get_queue_route, now_unix, pause_queue_route, resume_queue_route,
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
            monitoring_metrics: true,
            note: None,
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
            backoff: None,
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
            forward_backoff: ForwardBackoffDefaults::default(),
            ui_access_token: Some("ui-token".to_string()),
            ignored_headers: vec![],
            trusted_proxies: vec![],
            metrics_enabled: false,
        };

        let app_state = Arc::new(AppState {
            config,
            webhook_service,
            metrics_handle: None,
            http_client: crate::http_client::build_http_client().unwrap(),
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
            .layer(axum::middleware::from_fn(
                crate::middleware::request_id::middleware,
            ))
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

    async fn fetch_queue(app: Router) -> Value {
        let req = Request::builder()
            .method("GET")
            .uri("/api/webhook/test/queue")
            .header(http::header::AUTHORIZATION, "Bearer ui-token")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();

        serde_json::from_slice(&body).unwrap()
    }

    /// The UI has to be able to explain a queue that sits still: every item carries
    /// its own due time, and the channel status carries the soonest one.
    #[tokio::test]
    async fn test_queue_reports_the_pending_retry_delay() {
        let (app, state) = build_app(vec![make_channel_with_forward("test")]).await;
        let channel = WebhookChannel::new("test");

        state
            .webhook_service
            .receive_webhook(channel.clone(), HashMap::new(), "{}".into())
            .await
            .unwrap();

        let queued = state.webhook_service.list_queue(&channel).await.unwrap();
        let id = queued[0].id.unwrap();
        let due_at = now_unix() + 600;
        state
            .webhook_service
            .record_forward_failure(id, "HTTP 500: boom", due_at)
            .await
            .unwrap();

        let body = fetch_queue(app).await;

        assert_eq!(body["items"][0]["next_attempt_at"], due_at);
        assert_eq!(body["items"][0]["forward_attempts"], 1);
        assert_eq!(
            body["status"]["next_attempt_at"], due_at,
            "a queue where everything waits must report when it resumes"
        );
    }

    /// One webhook that is due now means the channel is not waiting, however long
    /// the others have been parked for.
    #[tokio::test]
    async fn test_queue_status_has_no_delay_while_a_webhook_is_due() {
        let (app, state) = build_app(vec![make_channel_with_forward("test")]).await;
        let channel = WebhookChannel::new("test");

        for _ in 0..2 {
            state
                .webhook_service
                .receive_webhook(channel.clone(), HashMap::new(), "{}".into())
                .await
                .unwrap();
        }

        let queued = state.webhook_service.list_queue(&channel).await.unwrap();
        state
            .webhook_service
            .record_forward_failure(queued[0].id.unwrap(), "HTTP 500: boom", now_unix() + 600)
            .await
            .unwrap();

        let body = fetch_queue(app).await;

        assert!(body["status"]["next_attempt_at"].is_null());
        assert!(body["items"][1]["next_attempt_at"].is_null());
    }
}
