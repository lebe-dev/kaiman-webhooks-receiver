use std::sync::Arc;

use axum::{
    Json,
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use serde::Serialize;

use crate::AppState;
use kwp_lib::domain::config::model::SecretType;
use kwp_lib::domain::crypto;

#[derive(Serialize)]
pub struct SignResponseDto {
    pub signature: String,
    pub header_name: String,
    pub header_value: String,
}

pub async fn sign_webhook_route(
    State(state): State<Arc<AppState>>,
    Path(channel_name): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    log::info!("request to sign payload for channel: {}", channel_name);

    let bearer = headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));

    let bearer = match bearer {
        Some(b) => b,
        None => {
            log::warn!(
                "missing or invalid Authorization header for sign request on channel: {}",
                channel_name
            );
            return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
        }
    };

    let channel_config = match state.config.find_channel_by_token(bearer) {
        Some(c) => {
            if c.name != channel_name {
                log::warn!(
                    "token for channel '{}' used to sign for channel '{}'",
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
                    "invalid token for sign request on channel: {}",
                    channel_name
                );
                return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
            }
            match state.config.find_channel_by_name(&channel_name) {
                Some(c) => c,
                None => return (StatusCode::NOT_FOUND, "Channel not found").into_response(),
            }
        }
    };

    if channel_config.secret_type != SecretType::HmacSha256 {
        log::warn!(
            "sign requested for channel '{}' which does not use hmac-sha256",
            channel_name
        );
        return (
            StatusCode::BAD_REQUEST,
            "Channel does not use HMAC-SHA256 secret type",
        )
            .into_response();
    }

    let secret = channel_config.webhook_secret.as_deref().unwrap_or("");
    let header_name = channel_config.secret_header.clone().unwrap_or_default();

    let signature = crypto::hmac_sha256_hex(secret.as_bytes(), &body);

    let sign_tmpl = channel_config
        .secret_sign_template
        .as_deref()
        .unwrap_or("{{ signature }}");

    let header_value = match crypto::render_sign_template(sign_tmpl, &signature) {
        Ok(v) => v,
        Err(e) => {
            log::error!(
                "secret-sign-template render failed for channel '{}': {}",
                channel_name,
                e
            );
            return (StatusCode::INTERNAL_SERVER_ERROR, "Error").into_response();
        }
    };

    log::info!("computed signature for channel: {}", channel_name);
    (
        StatusCode::OK,
        Json(SignResponseDto {
            signature,
            header_name,
            header_value,
        }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::net::IpAddr;
    use std::sync::{Arc, RwLock};

    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use axum::routing::post;
    use tower::ServiceExt;

    use kwp_lib::domain::config::model::{AppConfig, SecretType, WebhookChannelConfig};
    use kwp_lib::domain::crypto;
    use kwp_lib::domain::webhook::service::WebhookServiceImpl;
    use kwp_lib::outbound::sqlite::Sqlite;

    use crate::AppState;
    use crate::middleware::client_ip::ClientIp;
    use axum::Extension;

    use super::sign_webhook_route;

    fn make_hmac_channel(name: &str, secret: &str, header: &str) -> WebhookChannelConfig {
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
        }
    }

    fn make_plain_channel(name: &str) -> WebhookChannelConfig {
        WebhookChannelConfig {
            name: name.to_string(),
            api_read_token: "read-token".to_string(),
            webhook_secret: Some("sec".to_string()),
            secret_header: Some("X-Secret".to_string()),
            secret_type: SecretType::Plain,
            secret_extract_template: None,
            secret_sign_template: None,
            forward: None,
            max_body_size: None,
            allowed_ips: None,
        }
    }

    async fn build_app(channels: Vec<WebhookChannelConfig>) -> Router {
        let config = AppConfig {
            bind: "0.0.0.0:8080".to_string(),
            log_level: "info".to_string(),
            log_target: "stdout".to_string(),
            data_path: "./data".to_string(),
            db_cnn: "sqlite::memory:".to_string(),
            channels,
            default_body_limit: 262_144,
            ignored_headers: vec![],
            metrics_enabled: false,
            trusted_proxies: vec![],
            ui_access_token: None,
            ui_enabled: true,
            api_enabled: true,
        };
        let db = Sqlite::new("sqlite::memory:").await.unwrap();
        let state = Arc::new(AppState {
            config,
            webhook_service: WebhookServiceImpl::new(db),
            metrics_handle: None,
            http_client: reqwest::Client::new(),
            forward_statuses: Arc::new(RwLock::new(HashMap::new())),
        });
        let client_ip: IpAddr = "127.0.0.1".parse().unwrap();
        Router::new()
            .route("/api/webhook/{channel}/sign", post(sign_webhook_route))
            .layer(Extension(ClientIp(client_ip)))
            .with_state(state)
    }

    async fn post_sign(
        app: Router,
        channel: &str,
        token: Option<&str>,
        body: &[u8],
    ) -> (StatusCode, String) {
        let mut builder = Request::builder()
            .method("POST")
            .uri(format!("/api/webhook/{}/sign", channel));
        if let Some(t) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {}", t));
        }
        let req = builder.body(Body::from(body.to_vec())).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8_lossy(&bytes).to_string();
        (status, body_str)
    }

    #[tokio::test]
    async fn test_sign_returns_200_with_correct_signature() {
        let ch = make_hmac_channel("gh", "my-secret", "X-Hub-Signature-256");
        let app = build_app(vec![ch]).await;
        let payload = b"{\"event\":\"push\"}";

        let (status, body) = post_sign(app, "gh", Some("read-token"), payload).await;

        assert_eq!(status, StatusCode::OK);
        let expected_sig = crypto::hmac_sha256_hex(b"my-secret", payload);
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["signature"], expected_sig);
        assert_eq!(json["header_name"], "X-Hub-Signature-256");
        assert_eq!(json["header_value"], expected_sig);
    }

    #[tokio::test]
    async fn test_sign_missing_auth_returns_401() {
        let ch = make_hmac_channel("gh", "my-secret", "X-Hub-Signature-256");
        let app = build_app(vec![ch]).await;

        let (status, _) = post_sign(app, "gh", None, b"{}").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_sign_invalid_token_returns_401() {
        let ch = make_hmac_channel("gh", "my-secret", "X-Hub-Signature-256");
        let app = build_app(vec![ch]).await;

        let (status, _) = post_sign(app, "gh", Some("wrong-token"), b"{}").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_sign_wrong_channel_returns_403() {
        let ch_a = make_hmac_channel("channel-a", "sec", "X-Sig");
        let ch_b = make_hmac_channel("channel-b", "sec2", "X-Sig");
        let app = build_app(vec![ch_a, ch_b]).await;

        // token belongs to channel-a, but path says channel-b
        let (status, _) = post_sign(app, "channel-b", Some("read-token"), b"{}").await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn test_sign_plain_secret_type_returns_400() {
        let ch = make_plain_channel("plain-ch");
        let app = build_app(vec![ch]).await;

        let (status, _) = post_sign(app, "plain-ch", Some("read-token"), b"{}").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_sign_with_sign_template_github_style() {
        let mut ch = make_hmac_channel("gh", "my-secret", "X-Hub-Signature-256");
        ch.secret_sign_template = Some("sha256={{ signature }}".to_string());
        let app = build_app(vec![ch]).await;
        let payload = b"{\"event\":\"push\"}";

        let (status, body) = post_sign(app, "gh", Some("read-token"), payload).await;

        assert_eq!(status, StatusCode::OK);
        let expected_sig = crypto::hmac_sha256_hex(b"my-secret", payload);
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["signature"], expected_sig);
        assert_eq!(json["header_value"], format!("sha256={}", expected_sig));
    }

    #[tokio::test]
    async fn test_sign_no_sign_template_header_value_equals_signature() {
        let ch = make_hmac_channel("gh", "my-secret", "X-Sig");
        let app = build_app(vec![ch]).await;

        let (status, body) = post_sign(app, "gh", Some("read-token"), b"hello").await;

        assert_eq!(status, StatusCode::OK);
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["signature"], json["header_value"]);
    }
}
