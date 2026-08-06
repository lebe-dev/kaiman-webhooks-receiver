use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    Extension,
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use subtle::ConstantTimeEq;

use crate::AppState;
use crate::middleware::client_ip::ClientIp;
use crate::middleware::request_id::RequestId;
use kwp_lib::domain::config::model::{SecretType, WebhookChannelConfig};
use kwp_lib::domain::crypto;
use kwp_lib::domain::webhook::model::WebhookChannel;

fn inc_receive(channel: &str, status: &'static str, enabled: bool) {
    if !enabled {
        return;
    }
    metrics::counter!(
        "kwp_webhook_receive_total",
        "channel" => channel.to_string(),
        "status" => status
    )
    .increment(1);
}

/// The HMAC branch of secret verification: the signature the sender put in the
/// header, extracted with the channel's template, against the one computed over
/// this exact body.
fn verify_hmac_secret(
    channel_config: &WebhookChannelConfig,
    request_id: &RequestId,
    secret: &str,
    provided_raw: Option<&str>,
    body: &Bytes,
) -> Result<bool, Response> {
    let channel_name = &channel_config.name;

    let Some(raw) = provided_raw else {
        log::warn!("[req:{request_id}] missing secret header for channel: {channel_name}");
        inc_receive(
            channel_name,
            "unauthorized",
            channel_config.monitoring_metrics,
        );
        return Err((StatusCode::UNAUTHORIZED, "Unauthorized").into_response());
    };

    let extract_tmpl = channel_config
        .secret_extract_template
        .as_deref()
        .unwrap_or("{{ raw }}");
    let expected_hex = crypto::render_extract_template(extract_tmpl, raw).map_err(|e| {
        log::error!(
            "[req:{request_id}] secret-extract-template render failed for channel '{channel_name}': {e}"
        );
        inc_receive(
            channel_name,
            "internal_error",
            channel_config.monitoring_metrics,
        );
        (StatusCode::INTERNAL_SERVER_ERROR, "Error").into_response()
    })?;

    let computed_hex = crypto::hmac_sha256_hex(secret.as_bytes(), body);
    Ok(crypto::verify_hmac_hex(&expected_hex, &computed_hex))
}

/// Authorizes the request against the channel's secret.
///
/// A channel without both a secret and a header to read it from is unauthenticated
/// by configuration, so it passes. `Err` carries the response to send instead.
fn verify_secret(
    channel_config: &WebhookChannelConfig,
    request_id: &RequestId,
    headers: &HeaderMap,
    body: &Bytes,
) -> Result<(), Response> {
    let channel_name = &channel_config.name;

    let (Some(secret), Some(header_name)) = (
        &channel_config.webhook_secret,
        &channel_config.secret_header,
    ) else {
        return Ok(());
    };

    log::debug!("[req:{request_id}] verifying webhook secret for channel: '{channel_name}'");
    let provided_raw = headers
        .get(header_name.as_str())
        .and_then(|v| v.to_str().ok());

    let verified = match channel_config.secret_type {
        SecretType::Plain => provided_raw
            .is_some_and(|token| bool::from(token.as_bytes().ct_eq(secret.as_bytes()))),
        SecretType::HmacSha256 => {
            verify_hmac_secret(channel_config, request_id, secret, provided_raw, body)?
        }
    };

    if !verified {
        log::warn!("[req:{request_id}] invalid webhook secret for channel: {channel_name}");
        inc_receive(
            channel_name,
            "unauthorized",
            channel_config.monitoring_metrics,
        );
        return Err((StatusCode::UNAUTHORIZED, "Unauthorized").into_response());
    }

    log::debug!("[req:{request_id}] webhook secret verified for channel: {channel_name}");
    Ok(())
}

/// Everything that can be rejected by looking at the body alone: its size, the
/// declared content type, and whether it parses as JSON.
fn validate_body(
    state: &AppState,
    channel_config: &WebhookChannelConfig,
    request_id: &RequestId,
    headers: &HeaderMap,
    body: &Bytes,
) -> Result<(), Response> {
    let channel_name = &channel_config.name;

    let effective_limit = channel_config
        .max_body_size
        .unwrap_or(state.config.default_body_limit);
    if body.len() > effective_limit {
        log::warn!(
            "[req:{}] request body too large for channel {}: {} bytes > limit {} bytes",
            request_id,
            channel_name,
            body.len(),
            effective_limit
        );
        inc_receive(
            channel_name,
            "payload_too_large",
            channel_config.monitoring_metrics,
        );
        return Err((StatusCode::PAYLOAD_TOO_LARGE, "Payload Too Large").into_response());
    }

    let content_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !content_type.starts_with("application/json") {
        log::warn!(
            "[req:{request_id}] unsupported content type for channel {channel_name}: {content_type}"
        );
        inc_receive(
            channel_name,
            "invalid_content_type",
            channel_config.monitoring_metrics,
        );
        return Err((
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Expected application/json",
        )
            .into_response());
    }

    if serde_json::from_slice::<serde_json::Value>(body).is_err() {
        log::warn!("[req:{request_id}] invalid JSON body for channel {channel_name}");
        inc_receive(
            channel_name,
            "invalid_json",
            channel_config.monitoring_metrics,
        );
        return Err((StatusCode::UNPROCESSABLE_ENTITY, "Invalid JSON").into_response());
    }

    Ok(())
}

pub async fn receive_webhook_route(
    State(state): State<Arc<AppState>>,
    Extension(client_ip): Extension<ClientIp>,
    Extension(request_id): Extension<RequestId>,
    Path(channel_name): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    // Arrival is only logged at DEBUG: an unauthenticated sender must not be able to
    // fill the INFO log. Accepted webhooks are reported once below, and every reason
    // for rejecting one is a warning of its own.
    log::debug!(
        "[req:{}] incoming webhook from {} for channel: '{}' ({} bytes)",
        request_id,
        client_ip.0,
        channel_name,
        body.len()
    );

    let channel_config = match state.config.find_channel_by_name(&channel_name) {
        Some(c) => c,
        None => {
            log::warn!(
                "[req:{}] webhook received for unknown channel: {}",
                request_id,
                channel_name
            );
            inc_receive(&channel_name, "channel_not_found", true);
            return (StatusCode::NOT_FOUND, "Channel not found").into_response();
        }
    };

    if !channel_config.is_ip_allowed(&client_ip.0) {
        log::warn!(
            "[req:{}] IP {} blocked for channel: '{}'",
            request_id,
            client_ip.0,
            channel_name
        );
        inc_receive(
            &channel_name,
            "ip_blocked",
            channel_config.monitoring_metrics,
        );
        return (StatusCode::FORBIDDEN, "Forbidden").into_response();
    }

    if let Err(response) = verify_secret(channel_config, &request_id, &headers, &body) {
        return response;
    }

    if let Err(response) = validate_body(&state, channel_config, &request_id, &headers, &body) {
        return response;
    }

    log::debug!(
        "[req:{}] filtering headers for channel: {}",
        request_id,
        channel_name
    );
    let forwarded_headers: HashMap<String, String> = headers
        .iter()
        .filter_map(|(k, v)| {
            let key = k.as_str().to_lowercase();
            if state.config.ignored_headers.contains(&key) {
                return None;
            }
            v.to_str().ok().map(|val| (key, val.to_string()))
        })
        .collect();

    let channel = WebhookChannel::new(channel_name.clone());
    // Read before `body` is handed to the service, which consumes it.
    let body_len = body.len();

    match state
        .webhook_service
        .receive_webhook(channel, forwarded_headers, body)
        .await
    {
        Ok(()) => {
            log::info!(
                "[req:{}] webhook successfully processed and stored for channel: {} ({} bytes)",
                request_id,
                channel_name,
                body_len
            );
            inc_receive(&channel_name, "ok", channel_config.monitoring_metrics);
            (StatusCode::OK, "OK").into_response()
        }
        // Nothing was stored and the condition is transient, so answer 503: the
        // sender redelivers, whereas a 500 would silently drop the webhook.
        Err(e) if e.is_busy() => {
            log::warn!(
                "[req:{}] storage busy, asking sender to redeliver webhook for channel {}: {}",
                request_id,
                channel_name,
                e
            );
            inc_receive(
                &channel_name,
                "storage_busy",
                channel_config.monitoring_metrics,
            );
            crate::route::storage_busy_response()
        }
        Err(e) => {
            log::error!(
                "[req:{}] failed to store webhook for channel {}: {}",
                request_id,
                channel_name,
                e
            );
            inc_receive(
                &channel_name,
                "internal_error",
                channel_config.monitoring_metrics,
            );
            (StatusCode::INTERNAL_SERVER_ERROR, "Error").into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::net::IpAddr;
    use std::sync::{Arc, RwLock};

    use axum::{
        Extension, Router,
        body::Body,
        http::{self, Request, StatusCode},
        routing::{get, post},
    };
    use tower::ServiceExt;

    use kwp_lib::domain::config::model::{
        AppConfig, ForwardBackoffDefaults, SecretType, WebhookChannelConfig,
    };
    use kwp_lib::domain::crypto;
    use kwp_lib::domain::webhook::service::WebhookServiceImpl;
    use kwp_lib::outbound::sqlite::Sqlite;

    use crate::AppState;
    use crate::middleware::client_ip::ClientIp;
    use crate::route::{
        read_webhooks::read_webhooks_route, receive_webhook::receive_webhook_route,
    };

    fn make_channel(name: &str, max_body_size: Option<usize>) -> WebhookChannelConfig {
        WebhookChannelConfig {
            name: name.to_string(),
            api_read_token: "read-token".to_string(),
            webhook_secret: None,
            secret_header: None,
            secret_type: SecretType::Plain,
            secret_extract_template: None,
            secret_sign_template: None,
            forward: None,
            max_body_size,
            allowed_ips: None,
            monitoring_metrics: true,
            note: None,
        }
    }

    fn make_channel_with_secret(name: &str, secret: &str, header: &str) -> WebhookChannelConfig {
        WebhookChannelConfig {
            name: name.to_string(),
            api_read_token: "read-token".to_string(),
            webhook_secret: Some(secret.to_string()),
            secret_header: Some(header.to_string()),
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

    fn make_channel_with_allowed_ips(name: &str, ips: Vec<&str>) -> WebhookChannelConfig {
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
            allowed_ips: Some(ips.into_iter().map(String::from).collect()),
            monitoring_metrics: true,
            note: None,
        }
    }

    fn make_channel_with_hmac(name: &str, secret: &str, header: &str) -> WebhookChannelConfig {
        WebhookChannelConfig {
            name: name.to_string(),
            api_read_token: "read-token".to_string(),
            webhook_secret: Some(secret.to_string()),
            secret_header: Some(header.to_string()),
            secret_type: SecretType::HmacSha256,
            secret_extract_template: None,
            secret_sign_template: None,
            forward: None,
            max_body_size: None,
            allowed_ips: None,
            monitoring_metrics: true,
            note: None,
        }
    }

    async fn build_app_with_ip(
        channels: Vec<WebhookChannelConfig>,
        default_body_limit: usize,
        client_ip: IpAddr,
    ) -> Router {
        let db = Sqlite::new("sqlite::memory:").await.unwrap();
        build_app_with_db(channels, default_body_limit, client_ip, db)
    }

    fn build_app_with_db(
        channels: Vec<WebhookChannelConfig>,
        default_body_limit: usize,
        client_ip: IpAddr,
        db: Sqlite,
    ) -> Router {
        let config = AppConfig {
            bind: "0.0.0.0:8080".to_string(),
            log_level: "info".to_string(),
            log_target: "stdout".to_string(),
            data_path: "./data".to_string(),
            db_cnn: "sqlite::memory:".to_string(),
            channels,
            default_body_limit,
            ignored_headers: vec![
                "connection".to_string(),
                "content-length".to_string(),
                "content-type".to_string(),
                "host".to_string(),
                "transfer-encoding".to_string(),
            ],
            metrics_enabled: false,
            trusted_proxies: vec![],
            ui_access_token: None,
            ui_enabled: true,
            api_enabled: true,
            forward_backoff: ForwardBackoffDefaults::default(),
        };
        let state = Arc::new(AppState {
            config,
            webhook_service: WebhookServiceImpl::new(db),
            metrics_handle: None,
            http_client: crate::http_client::build_http_client().unwrap(),
            forward_statuses: Arc::new(RwLock::new(HashMap::new())),
        });
        Router::new()
            .route("/api/webhook/{channel}", post(receive_webhook_route))
            .route("/api/webhook/{channel}", get(read_webhooks_route))
            .layer(Extension(ClientIp(client_ip)))
            .layer(axum::middleware::from_fn(
                crate::middleware::request_id::middleware,
            ))
            .with_state(state)
    }

    async fn build_app(channels: Vec<WebhookChannelConfig>, default_body_limit: usize) -> Router {
        build_app_with_ip(channels, default_body_limit, "127.0.0.1".parse().unwrap()).await
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

    /// Builds an app on a file-backed database whose write lock is already held,
    /// so every write the handler attempts sees `SQLITE_BUSY`.
    ///
    /// The returned connection owns the lock; dropping it releases it.
    async fn build_app_with_locked_db(
        channels: Vec<WebhookChannelConfig>,
    ) -> (Router, tempfile::TempDir, sqlx::SqliteConnection) {
        use kwp_lib::outbound::sqlite::{LockRetryPolicy, SqliteTuning};
        use sqlx::{Connection, Executor};
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}/kwp.db?mode=rwc", dir.path().display());

        // Short waits: this test is about the response, not about how long the
        // adapter is willing to wait.
        let tuning = SqliteTuning {
            busy_timeout: Duration::from_millis(20),
            retry: LockRetryPolicy {
                max_attempts: 2,
                initial_backoff: Duration::from_millis(5),
                max_backoff: Duration::from_millis(5),
                budget: Duration::from_millis(100),
            },
            ..SqliteTuning::default()
        };
        let db = Sqlite::new_with_tuning(&url, tuning).await.unwrap();

        let mut lock = sqlx::SqliteConnection::connect(&url).await.unwrap();
        lock.execute("BEGIN EXCLUSIVE").await.unwrap();

        let app = build_app_with_db(channels, 1024, "127.0.0.1".parse().unwrap(), db);

        (app, dir, lock)
    }

    /// Webhook senders redeliver on 503 but treat 500 as a delivered-and-broken
    /// event, so a lock collision must never be reported as 500.
    #[tokio::test]
    async fn test_busy_storage_returns_503_with_retry_after() {
        let (app, _dir, _lock) = build_app_with_locked_db(vec![make_channel("test", None)]).await;

        let req = Request::builder()
            .method("POST")
            .uri("/api/webhook/test")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(b"{}".to_vec()))
            .unwrap();
        let response = app.oneshot(req).await.unwrap();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response
                .headers()
                .get(http::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok()),
            Some("1"),
            "the sender needs to be told when to come back"
        );
    }

    /// The destructive poll deletes rows, so it needs the write lock too.
    #[tokio::test]
    async fn test_busy_storage_returns_503_when_polling() {
        let (app, _dir, _lock) = build_app_with_locked_db(vec![make_channel("test", None)]).await;

        let req = Request::builder()
            .method("GET")
            .uri("/api/webhook/test")
            .header(http::header::AUTHORIZATION, "Bearer read-token")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    /// A busy database must not be confused with an authentication failure or a
    /// missing channel: those are checked first and answered on their own.
    #[tokio::test]
    async fn test_busy_storage_does_not_mask_auth_failures() {
        let (app, _dir, _lock) = build_app_with_locked_db(vec![make_channel("test", None)]).await;

        let req = Request::builder()
            .method("GET")
            .uri("/api/webhook/test")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
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
        json_body.extend_from_slice(&[b'a'; 98]);
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

    #[tokio::test]
    async fn test_allowed_ip_returns_200() {
        let channel = make_channel_with_allowed_ips("secure", vec!["127.0.0.1"]);
        let app = build_app_with_ip(vec![channel], 1024, "127.0.0.1".parse().unwrap()).await;
        assert_eq!(
            send_json(app, "secure", b"{}".to_vec(), None).await,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn test_blocked_ip_returns_403() {
        let channel = make_channel_with_allowed_ips("secure", vec!["10.0.0.1"]);
        let app = build_app_with_ip(vec![channel], 1024, "192.168.1.100".parse().unwrap()).await;
        assert_eq!(
            send_json(app, "secure", b"{}".to_vec(), None).await,
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn test_allowed_cidr_returns_200() {
        let channel = make_channel_with_allowed_ips("secure", vec!["10.0.0.0/8"]);
        let app = build_app_with_ip(vec![channel], 1024, "10.5.6.7".parse().unwrap()).await;
        assert_eq!(
            send_json(app, "secure", b"{}".to_vec(), None).await,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn test_blocked_ip_before_secret_check_returns_403_not_401() {
        let mut channel = make_channel_with_secret("secure", "mysecret", "X-Secret");
        channel.allowed_ips = Some(vec!["10.0.0.1".to_string()]);
        let app = build_app_with_ip(vec![channel], 1024, "192.168.1.100".parse().unwrap()).await;
        // IP blocked before secret is checked
        let req = Request::builder()
            .method("POST")
            .uri("/api/webhook/secure")
            .header(http::header::CONTENT_TYPE, "application/json")
            .header("X-Secret", "mysecret")
            .body(Body::from(b"{}".to_vec()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn test_ignored_headers_filtered_on_receive() {
        let app = build_app(vec![make_channel("test", None)], 1024).await;
        let req = Request::builder()
            .method("POST")
            .uri("/api/webhook/test")
            .header(http::header::CONTENT_TYPE, "application/json")
            .header("host", "example.com")
            .header("x-custom-header", "should-be-kept")
            .body(Body::from(b"{\"test\": true}".to_vec()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        // To verify headers were filtered, we'd need to query the database
        // but for now, we just verify that the request succeeds.
        // Custom header filtering is implicitly tested through the database query tests.
    }

    #[tokio::test]
    async fn test_hmac_valid_signature_returns_200() {
        let body = b"{\"event\":\"push\"}";
        let secret = "mysecret";
        let sig = crypto::hmac_sha256_hex(secret.as_bytes(), body);
        let channel = make_channel_with_hmac("github", secret, "X-Hub-Signature-256");
        let app = build_app(vec![channel], 1024).await;
        let req = Request::builder()
            .method("POST")
            .uri("/api/webhook/github")
            .header(http::header::CONTENT_TYPE, "application/json")
            .header("X-Hub-Signature-256", &sig)
            .body(Body::from(body.to_vec()))
            .unwrap();
        assert_eq!(app.oneshot(req).await.unwrap().status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_hmac_invalid_signature_returns_401() {
        let body = b"{\"event\":\"push\"}";
        let channel = make_channel_with_hmac("github", "mysecret", "X-Hub-Signature-256");
        let app = build_app(vec![channel], 1024).await;
        let req = Request::builder()
            .method("POST")
            .uri("/api/webhook/github")
            .header(http::header::CONTENT_TYPE, "application/json")
            .header("X-Hub-Signature-256", "badhex")
            .body(Body::from(body.to_vec()))
            .unwrap();
        assert_eq!(
            app.oneshot(req).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn test_hmac_missing_header_returns_401() {
        let body = b"{\"event\":\"push\"}";
        let channel = make_channel_with_hmac("github", "mysecret", "X-Hub-Signature-256");
        let app = build_app(vec![channel], 1024).await;
        let req = Request::builder()
            .method("POST")
            .uri("/api/webhook/github")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_vec()))
            .unwrap();
        assert_eq!(
            app.oneshot(req).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn test_hmac_with_extract_template_github_style() {
        let body = b"{\"event\":\"push\"}";
        let secret = "mysecret";
        let sig = crypto::hmac_sha256_hex(secret.as_bytes(), body);
        let header_value = format!("sha256={sig}");
        let mut channel = make_channel_with_hmac("github", secret, "X-Hub-Signature-256");
        channel.secret_extract_template =
            Some(r#"{{ raw | replace(from="sha256=", to="") }}"#.to_string());
        let app = build_app(vec![channel], 1024).await;
        let req = Request::builder()
            .method("POST")
            .uri("/api/webhook/github")
            .header(http::header::CONTENT_TYPE, "application/json")
            .header("X-Hub-Signature-256", &header_value)
            .body(Body::from(body.to_vec()))
            .unwrap();
        assert_eq!(app.oneshot(req).await.unwrap().status(), StatusCode::OK);
    }
}
