use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};

use axum::{
    Router,
    extract::DefaultBodyLimit,
    http::StatusCode,
    routing::{any, delete, get, post},
};
use kwp_lib::VERSION;
use kwp_lib::domain::config::model::AppConfig;
use kwp_lib::domain::config::ports::AppConfigLoader;
use kwp_lib::domain::webhook::model::{ChannelForwardStatus, WebhookChannel};
use kwp_lib::domain::webhook::service::WebhookServiceImpl;
use kwp_lib::outbound::config::EnvConfigLoader;
use kwp_lib::outbound::sqlite::Sqlite;
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use route::{
    config::get_config_route,
    delete_webhook::delete_webhook_route,
    list_webhooks::list_webhooks_route,
    metrics::metrics_route,
    queue::{
        clear_queue_route, get_queue_route, pause_queue_route, resume_queue_route,
        retry_webhook_route,
    },
    read_webhooks::read_webhooks_route,
    receive_webhook::receive_webhook_route,
    sign_webhook::sign_webhook_route,
    test_send::test_send_route,
};
use sentry::SentryFutureExt;
use sentry::integrations::tower::NewSentryLayer;

use crate::route::version::get_version_route;

pub mod background;
pub mod http_client;
pub mod logger;
pub mod middleware;
pub mod observability;
// A handler step that can reject the request returns `Result<T, Response>`, and
// `axum::Response` is 128 bytes — which is exactly what `result_large_err` counts.
// Boxing it here would buy nothing: the response is built once per rejected request
// and returned immediately.
#[allow(clippy::result_large_err)]
pub mod route;
pub mod security_metrics;
pub mod static_files;

#[derive(Clone)]
pub struct AppState {
    pub config: AppConfig,
    pub webhook_service: WebhookServiceImpl<Sqlite>,
    pub metrics_handle: Option<PrometheusHandle>,
    pub http_client: reqwest::Client,
    pub forward_statuses: Arc<RwLock<HashMap<String, ChannelForwardStatus>>>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    // Initialized before anything else so that startup failures are reported too.
    // The guard must outlive the application: dropping it stops event delivery.
    let _sentry_guard = observability::init();

    let result = run().await;

    if let Err(error) = &result {
        observability::capture_fatal_error(error);
    }

    result
}

/// Every startup check, in one place: misconfiguration must stop the process here
/// rather than surface as a failed webhook hours later.
fn validate_config(app_config: &AppConfig) -> anyhow::Result<()> {
    app_config
        .validate_body_limits()
        .map_err(|e| anyhow::anyhow!(e))?;
    app_config
        .validate_allowed_ips()
        .map_err(|e| anyhow::anyhow!(e))?;
    app_config
        .validate_templates()
        .map_err(|e| anyhow::anyhow!(e))?;
    app_config
        .validate_forward_backoff()
        .map_err(|e| anyhow::anyhow!(e))?;
    Ok(())
}

/// Spawns one forwarder task per channel that has a `forward` config, and returns
/// the status map the tasks and the API share.
fn spawn_forwarders(
    app_config: &AppConfig,
    db: &Sqlite,
    http_client: &reqwest::Client,
) -> Arc<RwLock<HashMap<String, ChannelForwardStatus>>> {
    let forward_statuses = Arc::new(RwLock::new(HashMap::new()));

    for channel_cfg in &app_config.channels {
        let Some(forward_cfg) = channel_cfg.forward.clone() else {
            continue;
        };

        // Registered before the task starts: `/queue` must find an entry for the
        // channel even if the forwarder has not run a single cycle yet.
        forward_statuses
            .write()
            .unwrap()
            .insert(channel_cfg.name.clone(), ChannelForwardStatus::new());

        let forward_url = forward_cfg.url.clone();
        let forwarder = background::forward::run_forwarder(
            WebhookChannel::new(channel_cfg.name.clone()),
            forward_cfg.clone(),
            forward_cfg.backoff(&app_config.forward_backoff),
            channel_cfg.webhook_secret.clone(),
            channel_cfg.monitoring_metrics,
            db.clone(),
            http_client.clone(),
            app_config.ignored_headers.clone(),
            forward_statuses.clone(),
        );

        // The forwarder loop never returns on its own: if the task ends, deliveries
        // for the channel have silently stopped and that has to be reported.
        let hub = observability::task_hub("forwarder", &channel_cfg.name);
        let watchdog_hub = hub.clone();
        let channel_name = channel_cfg.name.clone();

        tokio::spawn(
            async move {
                match tokio::spawn(forwarder.bind_hub(hub)).await {
                    Ok(()) => log::error!("[forwarder:{channel_name}] task exited unexpectedly"),
                    Err(e) => log::error!("[forwarder:{channel_name}] task terminated: {e}"),
                }
            }
            .bind_hub(watchdog_hub),
        );

        log::info!(
            "started forwarder for channel={} → {}",
            channel_cfg.name,
            forward_url
        );
    }

    forward_statuses
}

/// Builds the route table. Both the API and the UI can be switched off, and each
/// says so in the log — a 404 alone would look like a routing bug.
fn build_router(app_config: &AppConfig) -> Router<Arc<AppState>> {
    let mut app = Router::new();

    if !app_config.api_enabled {
        log::info!("REST API is disabled (API_ENABLED=0)");
        app = app.route("/api/{*path}", any(|| async { StatusCode::NOT_FOUND }));
    } else {
        app = app
            .route("/api/version", get(get_version_route))
            .route("/api/config", get(get_config_route))
            .route("/api/webhook/{channel}", post(receive_webhook_route))
            .route("/api/webhook/{channel}", get(read_webhooks_route))
            .route("/api/webhook/{channel}/list", get(list_webhooks_route))
            .route("/api/webhook/{channel}/{id}", delete(delete_webhook_route))
            .route("/api/webhook/{channel}/sign", post(sign_webhook_route))
            .route("/api/webhook/{channel}/test-send", post(test_send_route))
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
            .route(
                "/api/webhook/{channel}/queue/retry/{id}",
                post(retry_webhook_route),
            );

        if app_config.metrics_enabled {
            app = app.route("/api/metrics", get(metrics_route));
        }
    }

    if !app_config.ui_enabled {
        log::info!("Web UI is disabled (UI_ENABLED=0)");
        return app;
    }
    app.fallback(static_files::static_file_handler)
}

async fn run() -> anyhow::Result<()> {
    // Installed before the configuration is read: `EnvConfigLoader` warns about
    // values it cannot use, and `log` macros are no-ops until a logger exists, so
    // installing it afterwards would drop exactly those warnings.
    let (log_level, log_target) = kwp_lib::outbound::config::log_settings_from_env();
    logger::init(&log_level, &log_target, observability::is_enabled())?;

    if observability::is_enabled() {
        log::info!("Sentry error reporting is enabled");
    }

    let config_loader = EnvConfigLoader;
    let app_config = config_loader.load()?;
    validate_config(&app_config)?;

    // Header names carrying secrets are channel-specific, so Sentry has to learn
    // them before any request is served.
    observability::init_sensitive_headers(&app_config.channels);

    // Whether proxy headers are honoured at all is a constant of this process, so
    // it is stated once here instead of on every request.
    if app_config.trusted_proxies.is_empty() {
        log::info!(
            "TRUSTED_PROXIES is empty, proxy headers are ignored and the connection IP is used"
        );
    } else {
        log::info!(
            "trusting proxy headers from {} configured proxies",
            app_config.trusted_proxies.len()
        );
    }

    let db = Sqlite::new(&app_config.db_cnn).await?;
    let http_client = http_client::build_http_client()?;
    let forward_statuses = spawn_forwarders(&app_config, &db, &http_client);

    let webhook_service = WebhookServiceImpl::new(db);

    let metrics_handle = if app_config.metrics_enabled {
        let handle = PrometheusBuilder::new()
            .install_recorder()
            .map_err(|e| anyhow::anyhow!("failed to install prometheus recorder: {}", e))?;
        security_metrics::record_channel_security_gauges(&app_config.channels);
        Some(handle)
    } else {
        None
    };

    let app_state = Arc::new(AppState {
        config: app_config.clone(),
        webhook_service,
        metrics_handle,
        http_client: http_client.clone(),
        forward_statuses: forward_statuses.clone(),
    });

    // Layers are applied outside-in in reverse order: the Sentry hub is bound first,
    // then the request gets its id, then the client IP is resolved, and only then
    // the scope middleware reads both.
    let app = build_router(&app_config)
        .layer(DefaultBodyLimit::max(app_config.max_body_limit()))
        .layer(axum::middleware::from_fn(
            middleware::sentry_scope::middleware,
        ))
        .layer(axum::middleware::from_fn(
            middleware::client_ip::ClientIpExtractor::middleware,
        ))
        .layer(axum::Extension(app_config.trusted_proxies.clone()))
        .layer(axum::middleware::from_fn(
            middleware::request_id::middleware,
        ))
        .layer(NewSentryLayer::<axum::extract::Request>::new_from_top())
        .with_state(app_state);

    let listener = tokio::net::TcpListener::bind(&app_config.bind).await?;

    log::info!(
        r#"
           __ ___       ______
          / //_/ |     / / __ \
         / ,<  | | /| / / /_/ /
        / /| | | |/ |/ / ____/
       /_/ |_| |__/|__/_/

       Kaiman Webhooks Proxy v{}"#,
        VERSION
    );
    log::info!("Listening on '{}'", app_config.bind);

    let server = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    );

    tokio::select! {
        result = server => {
            result?;
        }
        _ = tokio::signal::ctrl_c() => {
            log::info!("shutting down gracefully...");
        }
    }

    Ok(())
}
