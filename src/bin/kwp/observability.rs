//! Sentry error reporting.
//!
//! Reporting is optional: without `SENTRY_DSN` no client is initialized and every
//! helper here becomes a no-op.
//!
//! Events reach Sentry through the logger — `log::error!` is captured as an event,
//! `warn`/`info` become breadcrumbs (see [`crate::logger`]). Panics are captured by
//! the panic integration installed by [`init`].

use std::collections::BTreeMap;
use std::env;
use std::net::IpAddr;
use std::sync::OnceLock;
use std::time::Duration;

use axum::http::{HeaderMap, Method, Uri, header, uri};
use kwp_lib::domain::config::model::WebhookChannelConfig;
use sentry::protocol::{IpAddress, Request as SentryRequest, User};
use sentry::{ClientInitGuard, ClientOptions};

/// Release name reported to Sentry. Uses the package version, not `kwp_lib::VERSION`,
/// because the latter carries a build suffix that is not a valid release identifier.
const RELEASE: &str = concat!("kwp@", env!("CARGO_PKG_VERSION"));

/// Envelopes are sent from a background thread, so panics and fatal errors have to
/// wait for the queue to drain — the release profile uses `panic = 'abort'` and the
/// process is gone as soon as the panic hook returns.
const FLUSH_TIMEOUT: Duration = Duration::from_secs(3);

const REDACTED_VALUE: &str = "[redacted]";
const NON_UTF8_VALUE: &str = "[non-utf8]";

/// Substrings that mark a header as sensitive. Matched against the lowercase header
/// name, so `authorization`, `x-hub-signature-256` and `x-api-key` are all covered.
const SENSITIVE_HEADER_MARKERS: &[&str] = &[
    "auth", "cookie", "key", "password", "secret", "sign", "token",
];

/// Header names taken from the channel configuration (`secret-header`, `sign-header`).
/// They are arbitrary — `x-telegram-bot-api-secret-token` carries the plain secret —
/// so they are collected at startup and redacted alongside the well-known ones.
static CONFIGURED_SENSITIVE_HEADERS: OnceLock<Vec<String>> = OnceLock::new();

/// Initializes the Sentry client from `SENTRY_DSN` / `SENTRY_ENVIRONMENT`.
///
/// Returns `None` when reporting is disabled or the DSN is unusable. The returned
/// guard must be kept alive for the whole process lifetime — dropping it flushes
/// pending events and disables the client.
pub fn init() -> Option<ClientInitGuard> {
    let settings = parse_settings(
        env::var("SENTRY_DSN").ok().as_deref(),
        env::var("SENTRY_ENVIRONMENT").ok().as_deref(),
    )?;

    // Sentry builds its own `reqwest` client, and `reqwest` is compiled with
    // `rustls-no-provider` — the provider has to be installed before the transport
    // creates its TLS client.
    crate::http_client::install_crypto_provider();

    let mut options = ClientOptions::default();
    options.dsn = Some(settings.dsn);
    options.release = Some(RELEASE.into());
    options.environment = settings.environment.map(Into::into);
    options.attach_stacktrace = true;
    // Webhook payloads and secrets must never leave the process implicitly;
    // request data is attached explicitly by `set_request_scope`.
    options.send_default_pii = false;

    let guard = sentry::init(options);

    install_panic_hook();

    Some(guard)
}

/// Whether an enabled Sentry client is bound to the current hub.
pub fn is_enabled() -> bool {
    sentry::Hub::current()
        .client()
        .is_some_and(|client| client.is_enabled())
}

/// Blocks until queued events are sent (or [`FLUSH_TIMEOUT`] expires).
pub fn flush() {
    if let Some(client) = sentry::Hub::current().client() {
        client.flush(Some(FLUSH_TIMEOUT));
    }
}

/// Reports an error that terminates the process, then flushes.
pub fn capture_fatal_error(error: &anyhow::Error) {
    sentry::with_scope(
        |scope| scope.set_level(Some(sentry::Level::Fatal)),
        || sentry::integrations::anyhow::capture_anyhow(error),
    );

    flush();
}

/// Registers the channel-specific header names that must be redacted.
///
/// Called once at startup; later calls are ignored.
pub fn init_sensitive_headers(channels: &[WebhookChannelConfig]) {
    let _ = CONFIGURED_SENSITIVE_HEADERS.set(collect_sensitive_headers(channels));
}

/// Returns a hub for a background task, tagged with its component and channel.
///
/// Background tasks do not go through the per-request Sentry layer, so they need
/// their own hub — otherwise their tags would leak into unrelated events.
pub fn task_hub(component: &str, channel: &str) -> std::sync::Arc<sentry::Hub> {
    let hub = std::sync::Arc::new(sentry::Hub::new_from_top(sentry::Hub::current()));

    hub.configure_scope(|scope| {
        scope.set_tag("component", component);
        scope.set_tag("channel", channel);
    });

    hub
}

/// Attaches request metadata to the scope of the current request.
///
/// Header values are redacted (see [`SENSITIVE_HEADER_MARKERS`] and
/// [`init_sensitive_headers`]) because webhook secrets and signatures travel in
/// headers whose names come from user configuration.
pub fn set_request_scope(method: &Method, uri: &Uri, client_ip: IpAddr, headers: &HeaderMap) {
    let request = SentryRequest {
        method: Some(method.to_string()),
        url: absolute_url(uri, headers).and_then(|url| url.parse().ok()),
        headers: redact_headers(headers, configured_sensitive_headers()),
        ..Default::default()
    };

    let transaction = format!("{} {}", method, uri.path());
    let channel = channel_from_path(uri.path()).map(str::to_string);

    sentry::configure_scope(|scope| {
        scope.set_transaction(Some(&transaction));
        scope.set_user(Some(User {
            ip_address: Some(IpAddress::Exact(client_ip)),
            ..Default::default()
        }));

        if let Some(channel) = &channel {
            scope.set_tag("channel", channel);
        }

        scope.add_event_processor(move |mut event| {
            if event.request.is_none() {
                event.request = Some(request.clone());
            }
            Some(event)
        });
    });
}

struct Settings {
    dsn: sentry::types::Dsn,
    environment: Option<String>,
}

/// Turns the raw environment values into client settings.
///
/// Returns `None` — reporting stays disabled — when the DSN is absent, blank or
/// unparseable. A broken DSN must never keep the service from starting.
fn parse_settings(raw_dsn: Option<&str>, raw_environment: Option<&str>) -> Option<Settings> {
    let dsn = raw_dsn?.trim();

    if dsn.is_empty() {
        return None;
    }

    let dsn = match dsn.parse::<sentry::types::Dsn>() {
        Ok(dsn) => dsn,
        Err(e) => {
            // The logger is not initialized this early, hence stderr.
            eprintln!("invalid SENTRY_DSN, error reporting is disabled: {e}");
            return None;
        }
    };

    let environment = raw_environment
        .map(str::trim)
        .filter(|environment| !environment.is_empty())
        .map(str::to_string);

    Some(Settings { dsn, environment })
}

fn install_panic_hook() {
    // `sentry::init` has already installed a hook that captures the panic event;
    // wrapping it keeps that behaviour and adds the flush.
    let previous = std::panic::take_hook();

    std::panic::set_hook(Box::new(move |info| {
        previous(info);
        flush();
    }));
}

fn collect_sensitive_headers(channels: &[WebhookChannelConfig]) -> Vec<String> {
    let mut headers: Vec<String> = channels
        .iter()
        .flat_map(|channel| {
            let sign_header = channel
                .forward
                .as_ref()
                .and_then(|forward| forward.sign_header.clone());
            [channel.secret_header.clone(), sign_header]
        })
        .flatten()
        .map(|header| header.trim().to_lowercase())
        .filter(|header| !header.is_empty())
        .collect();

    headers.sort();
    headers.dedup();

    headers
}

fn configured_sensitive_headers() -> &'static [String] {
    CONFIGURED_SENSITIVE_HEADERS
        .get()
        .map(Vec::as_slice)
        .unwrap_or_default()
}

/// Rebuilds the absolute request URL. Requests carry an origin-form URI
/// (`/api/webhook/x`), so the authority is taken from the `Host` header.
fn absolute_url(uri: &Uri, headers: &HeaderMap) -> Option<String> {
    let mut parts = uri.clone().into_parts();

    parts.scheme.get_or_insert(uri::Scheme::HTTP);

    if parts.authority.is_none() {
        parts.authority = headers
            .get(header::HOST)
            .and_then(|host| uri::Authority::try_from(host.as_bytes()).ok());
    }

    Some(uri::Uri::from_parts(parts).ok()?.to_string())
}

fn redact_headers(
    headers: &HeaderMap,
    configured_sensitive: &[String],
) -> BTreeMap<String, String> {
    headers
        .iter()
        .map(|(name, value)| {
            let name = name.as_str().to_lowercase();

            if value.is_sensitive() || is_sensitive_header(&name, configured_sensitive) {
                return (name, REDACTED_VALUE.to_string());
            }

            let value = value.to_str().unwrap_or(NON_UTF8_VALUE).to_string();
            (name, value)
        })
        .collect()
}

fn is_sensitive_header(lowercase_name: &str, configured_sensitive: &[String]) -> bool {
    if configured_sensitive
        .iter()
        .any(|configured| configured == lowercase_name)
    {
        return true;
    }

    SENSITIVE_HEADER_MARKERS
        .iter()
        .any(|marker| lowercase_name.contains(marker))
}

/// Channel name from `/api/webhook/{channel}[/...]`, used as an event tag.
fn channel_from_path(path: &str) -> Option<&str> {
    let channel = path.strip_prefix("/api/webhook/")?.split('/').next()?;

    if channel.is_empty() {
        return None;
    }

    Some(channel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderName, HeaderValue};
    use sentry::test::with_captured_events;

    fn header_map(headers: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();

        for (name, value) in headers {
            map.insert(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }

        map
    }

    #[test]
    fn redacts_well_known_secret_headers() {
        let headers = header_map(&[
            ("authorization", "Bearer secret-token"),
            ("cookie", "session=abc"),
            ("x-api-key", "abc123"),
            ("x-hub-signature-256", "sha256=deadbeef"),
            ("x-telegram-bot-api-secret-token", "plain-secret"),
        ]);

        let redacted = redact_headers(&headers, &[]);

        for (name, value) in &redacted {
            assert_eq!(value, REDACTED_VALUE, "header '{name}' must be redacted");
        }
    }

    #[test]
    fn keeps_non_sensitive_headers() {
        let headers = header_map(&[
            ("content-type", "application/json"),
            ("user-agent", "curl/8.0"),
            ("host", "kwp.example.com"),
        ]);

        let redacted = redact_headers(&headers, &[]);

        assert_eq!(redacted.get("content-type").unwrap(), "application/json");
        assert_eq!(redacted.get("user-agent").unwrap(), "curl/8.0");
        assert_eq!(redacted.get("host").unwrap(), "kwp.example.com");
    }

    #[test]
    fn redacts_configured_header_without_sensitive_name() {
        let headers = header_map(&[("x-my-webhook", "plain-secret")]);
        let configured = vec!["x-my-webhook".to_string()];

        let redacted = redact_headers(&headers, &configured);

        assert_eq!(redacted.get("x-my-webhook").unwrap(), REDACTED_VALUE);
    }

    #[test]
    fn lowercases_header_names() {
        let headers = header_map(&[("Content-Type", "application/json")]);

        let redacted = redact_headers(&headers, &[]);

        assert!(redacted.contains_key("content-type"));
    }

    #[test]
    fn sensitive_marker_matching_is_case_insensitive_by_name() {
        assert!(is_sensitive_header("x-gitlab-token", &[]));
        assert!(is_sensitive_header("proxy-authorization", &[]));
        assert!(!is_sensitive_header("content-length", &[]));
    }

    #[test]
    fn extracts_channel_from_webhook_paths() {
        assert_eq!(channel_from_path("/api/webhook/telegram"), Some("telegram"));
        assert_eq!(
            channel_from_path("/api/webhook/telegram/list"),
            Some("telegram")
        );
        assert_eq!(
            channel_from_path("/api/webhook/telegram/queue/retry/7"),
            Some("telegram")
        );
    }

    #[test]
    fn ignores_paths_without_channel() {
        assert_eq!(channel_from_path("/api/webhook/"), None);
        assert_eq!(channel_from_path("/api/config"), None);
        assert_eq!(channel_from_path("/api/metrics"), None);
        assert_eq!(channel_from_path("/"), None);
    }

    #[test]
    fn builds_absolute_url_from_host_header() {
        let uri: Uri = "/api/webhook/telegram?debug=1".parse().unwrap();
        let headers = header_map(&[("host", "kwp.example.com")]);

        let url = absolute_url(&uri, &headers).unwrap();

        assert_eq!(url, "http://kwp.example.com/api/webhook/telegram?debug=1");
    }

    #[test]
    fn skips_url_without_host() {
        let uri: Uri = "/api/webhook/telegram".parse().unwrap();

        assert!(absolute_url(&uri, &HeaderMap::new()).is_none());
    }

    #[test]
    fn reporting_is_disabled_without_dsn() {
        assert!(parse_settings(None, None).is_none());
        assert!(parse_settings(Some(""), None).is_none());
        assert!(parse_settings(Some("   "), None).is_none());
    }

    #[test]
    fn reporting_is_disabled_for_invalid_dsn() {
        assert!(parse_settings(Some("not-a-dsn"), Some("production")).is_none());
    }

    #[test]
    fn parses_dsn_and_environment() {
        let settings = parse_settings(
            Some("  https://key@sentry.example.com/7  "),
            Some(" staging "),
        )
        .expect("valid DSN must be accepted");

        assert_eq!(
            settings.dsn.to_string(),
            "https://key:@sentry.example.com/7",
            "surrounding whitespace must be trimmed"
        );
        assert_eq!(settings.environment.as_deref(), Some("staging"));
    }

    #[test]
    fn blank_environment_falls_back_to_sdk_default() {
        let settings = parse_settings(Some("https://key@sentry.example.com/1"), Some("  "))
            .expect("valid DSN must be accepted");

        assert!(settings.environment.is_none());
    }

    #[test]
    fn fatal_errors_are_reported_with_fatal_level() {
        let events = with_captured_events(|| {
            capture_fatal_error(&anyhow::anyhow!("DATABASE_URL is required"));
        });

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].level, sentry::Level::Fatal);
    }

    #[test]
    fn task_hub_tags_events_with_component_and_channel() {
        let events = with_captured_events(|| {
            let hub = task_hub("forwarder", "telegram");

            sentry::Hub::run(hub, || {
                sentry::capture_message("peek failed", sentry::Level::Error);
            });
        });

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].tags.get("component").unwrap(), "forwarder");
        assert_eq!(events[0].tags.get("channel").unwrap(), "telegram");
    }

    #[test]
    fn collects_configured_sensitive_headers_from_channels() {
        let yaml = r#"
channels:
  - name: telegram
    api-read-token: token
    secret-header: X-Telegram-Header
    forward:
      url: http://localhost:9000/hook
      interval-seconds: 5
      sign-header: X-Signature-Custom
  - name: github
    api-read-token: token
    secret-header: x-hub-signature-256
"#;

        #[derive(serde::Deserialize)]
        struct Wrapper {
            channels: Vec<WebhookChannelConfig>,
        }

        let wrapper: Wrapper = serde_yaml::from_str(yaml).unwrap();

        assert_eq!(
            collect_sensitive_headers(&wrapper.channels),
            vec![
                "x-hub-signature-256",
                "x-signature-custom",
                "x-telegram-header"
            ]
        );
    }
}
