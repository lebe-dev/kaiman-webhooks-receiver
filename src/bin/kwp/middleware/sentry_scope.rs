use axum::{extract::Request, middleware::Next, response::Response};

use crate::middleware::client_ip::ClientIp;
use crate::middleware::request_id::RequestId;
use crate::observability;

/// Attaches request context (method, URL, redacted headers, client IP, request id,
/// channel) to the Sentry scope, so any error reported while handling the request
/// carries it.
///
/// Must run inside [`crate::middleware::client_ip::ClientIpExtractor::middleware`]
/// and [`crate::middleware::request_id::middleware`] — both values are read from
/// the request extensions those insert. The request id is what ties a Sentry event
/// back to the log records of the same request.
pub async fn middleware(request: Request, next: Next) -> Response {
    if !observability::is_enabled() {
        return next.run(request).await;
    }

    if let Some(ClientIp(client_ip)) = request.extensions().get::<ClientIp>().cloned() {
        let request_id = request.extensions().get::<RequestId>().cloned();

        observability::set_request_scope(
            request.method(),
            request.uri(),
            client_ip,
            request.headers(),
            request_id.as_ref().map(RequestId::as_str),
        );
    }

    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        body::Body,
        http::StatusCode,
        routing::{get, post},
    };
    use sentry::integrations::tower::NewSentryLayer;
    use sentry::protocol::IpAddress;
    use sentry::test::with_captured_events;
    use std::net::IpAddr;
    use tower::ServiceExt;

    /// Same layer order as `main.rs`: the per-request hub is bound outside, the id is
    /// assigned next, and the scope middleware runs inside both.
    fn router() -> Router {
        Router::new()
            .route("/api/webhook/{channel}", post(failing_handler))
            .route("/api/config", get(failing_handler))
            .layer(axum::middleware::from_fn(middleware))
            .layer(axum::middleware::from_fn(
                crate::middleware::request_id::middleware,
            ))
            .layer(NewSentryLayer::<Request>::new_from_top())
    }

    /// Stands in for a `log::error!` inside a handler — the logger is not installed in
    /// tests, so the event is captured directly.
    async fn failing_handler() -> StatusCode {
        sentry::capture_message("failed to store webhook", sentry::Level::Error);
        StatusCode::INTERNAL_SERVER_ERROR
    }

    fn webhook_request(client_ip: Option<&str>) -> axum::http::Request<Body> {
        let mut builder = axum::http::Request::builder()
            .method("POST")
            .uri("/api/webhook/telegram")
            .header("host", "kwp.example.com")
            .header("content-type", "application/json")
            .header("authorization", "Bearer must-not-leak")
            .header("x-telegram-bot-api-secret-token", "must-not-leak");

        if let Some(ip) = client_ip {
            builder = builder.extension(ClientIp(ip.parse::<IpAddr>().unwrap()));
        }

        builder.body(Body::empty()).unwrap()
    }

    fn config_request() -> axum::http::Request<Body> {
        axum::http::Request::builder()
            .uri("/api/config")
            .header("host", "kwp.example.com")
            .extension(ClientIp("127.0.0.1".parse::<IpAddr>().unwrap()))
            .body(Body::empty())
            .unwrap()
    }

    fn send_all(requests: Vec<axum::http::Request<Body>>) -> Vec<StatusCode> {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let mut statuses = Vec::new();

            for request in requests {
                statuses.push(router().oneshot(request).await.unwrap().status());
            }

            statuses
        })
    }

    #[test]
    fn attaches_request_context_to_errors() {
        let events = with_captured_events(|| {
            assert_eq!(
                send_all(vec![webhook_request(Some("203.0.113.7"))]),
                vec![StatusCode::INTERNAL_SERVER_ERROR]
            );
        });

        assert_eq!(events.len(), 1);
        let event = &events[0];

        assert_eq!(
            event.transaction.as_deref(),
            Some("POST /api/webhook/telegram")
        );
        assert_eq!(event.tags.get("channel").unwrap(), "telegram");
        assert_eq!(
            event.user.as_ref().unwrap().ip_address,
            Some(IpAddress::Exact("203.0.113.7".parse().unwrap()))
        );

        // The value is random; what matters is that the event can be joined to the
        // log records of the same request at all.
        let request_id = event
            .tags
            .get("request_id")
            .expect("an event must be traceable back to the request log");
        assert!(request_id.chars().all(|c| c.is_ascii_hexdigit()));

        let request = event.request.as_ref().expect("request context is attached");
        assert_eq!(request.method.as_deref(), Some("POST"));
        assert_eq!(
            request.url.as_ref().map(ToString::to_string).as_deref(),
            Some("http://kwp.example.com/api/webhook/telegram")
        );
        assert_eq!(
            request.headers.get("content-type").unwrap(),
            "application/json"
        );
        assert_eq!(request.headers.get("authorization").unwrap(), "[redacted]");
        assert_eq!(
            request
                .headers
                .get("x-telegram-bot-api-secret-token")
                .unwrap(),
            "[redacted]",
            "webhook secrets must never be sent to Sentry"
        );
    }

    #[test]
    fn does_not_leak_channel_tag_between_requests() {
        let events = with_captured_events(|| {
            send_all(vec![webhook_request(Some("203.0.113.7")), config_request()]);
        });

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].tags.get("channel").unwrap(), "telegram");
        assert!(
            events[1].tags.get("channel").is_none(),
            "each request must get its own hub, otherwise tags bleed across requests"
        );
    }

    #[test]
    fn serves_requests_without_client_ip_extension() {
        let events = with_captured_events(|| {
            assert_eq!(
                send_all(vec![webhook_request(None)]),
                vec![StatusCode::INTERNAL_SERVER_ERROR],
                "a missing client IP must not change the response"
            );
        });

        assert_eq!(events.len(), 1);
        assert!(events[0].user.is_none());
    }
}
