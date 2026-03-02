use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};

use crate::AppState;
use kwp_lib::domain::webhook::model::WebhookChannel;

pub async fn delete_webhook_route(
    State(state): State<Arc<AppState>>,
    Path((channel_name, webhook_id)): Path<(String, i64)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    log::info!(
        "request to delete webhook {} for channel: {}",
        webhook_id,
        channel_name
    );

    let bearer = headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));

    let bearer = match bearer {
        Some(b) => b,
        None => {
            log::warn!(
                "missing or invalid Authorization header for delete on channel: {}",
                channel_name
            );
            return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
        }
    };

    match state.config.find_channel_by_token(bearer) {
        Some(c) => {
            if c.name != channel_name {
                log::warn!(
                    "token for channel '{}' used to delete webhook in channel '{}'",
                    c.name,
                    channel_name
                );
                return (StatusCode::FORBIDDEN, "Forbidden").into_response();
            }
        }
        None => {
            if !state.config.is_ui_token(bearer) {
                log::warn!(
                    "invalid token for delete request on channel: {}",
                    channel_name
                );
                return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
            }
            if state.config.find_channel_by_name(&channel_name).is_none() {
                return (StatusCode::NOT_FOUND, "Channel not found").into_response();
            }
        }
    };

    match state
        .webhook_service
        .delete_webhook(&WebhookChannel::new(channel_name.clone()), webhook_id)
        .await
    {
        Ok(_) => {
            log::info!(
                "successfully deleted webhook {} for channel: {}",
                webhook_id,
                channel_name
            );
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => {
            log::error!("Failed to delete webhook: {}", e);
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
        routing::delete,
    };
    use tower::ServiceExt;

    use kwp_lib::domain::config::model::{AppConfig, SecretType, WebhookChannelConfig};
    use kwp_lib::domain::webhook::service::WebhookServiceImpl;
    use kwp_lib::outbound::sqlite::Sqlite;

    use crate::AppState;
    use crate::route::delete_webhook::delete_webhook_route;

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

    async fn build_app(channels: Vec<WebhookChannelConfig>) -> Router {
        let db = Sqlite::new("sqlite::memory:").await.unwrap();
        let webhook_service = WebhookServiceImpl::new(db);
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
        });

        Router::new()
            .route("/api/webhook/{channel}/{id}", delete(delete_webhook_route))
            .with_state(app_state)
    }

    #[tokio::test]
    async fn test_delete_without_auth_returns_401() {
        let app = build_app(vec![make_channel("test")]).await;
        let req = Request::builder()
            .method("DELETE")
            .uri("/api/webhook/test/1")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_delete_with_ui_token_returns_204() {
        let app = build_app(vec![make_channel("test")]).await;
        let req = Request::builder()
            .method("DELETE")
            .uri("/api/webhook/test/1")
            .header(http::header::AUTHORIZATION, "Bearer ui-token")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn test_delete_wrong_channel_token_returns_403() {
        let mut ch1 = make_channel("ch1");
        ch1.api_read_token = "ch1-token".to_string();
        let mut ch2 = make_channel("ch2");
        ch2.api_read_token = "ch2-token".to_string();

        let app = build_app(vec![ch1, ch2]).await;
        let req = Request::builder()
            .method("DELETE")
            .uri("/api/webhook/ch2/1")
            .header(http::header::AUTHORIZATION, "Bearer ch1-token")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn test_delete_with_channel_token_returns_204() {
        let mut ch = make_channel("test");
        ch.api_read_token = "test-token".to_string();

        let app = build_app(vec![ch]).await;
        let req = Request::builder()
            .method("DELETE")
            .uri("/api/webhook/test/1")
            .header(http::header::AUTHORIZATION, "Bearer test-token")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn test_delete_nonexistent_channel_returns_404() {
        let app = build_app(vec![make_channel("test")]).await;
        let req = Request::builder()
            .method("DELETE")
            .uri("/api/webhook/nonexistent/1")
            .header(http::header::AUTHORIZATION, "Bearer ui-token")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
