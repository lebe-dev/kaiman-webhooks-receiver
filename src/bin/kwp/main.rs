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
use logger::get_logging_config;
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use route::{
    config::get_config_route, delete_webhook::delete_webhook_route,
    list_webhooks::list_webhooks_route, metrics::metrics_route,
    queue::{
        clear_queue_route, get_queue_route, pause_queue_route, resume_queue_route,
        retry_webhook_route,
    },
    read_webhooks::read_webhooks_route, receive_webhook::receive_webhook_route,
    sign_webhook::sign_webhook_route, test_send::test_send_route,
};

use crate::route::version::get_version_route;

pub mod background;
pub mod logger;
pub mod middleware;
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

    let config_loader = EnvConfigLoader;
    let app_config = config_loader.load()?;
    app_config
        .validate_body_limits()
        .map_err(|e| anyhow::anyhow!(e))?;
    app_config
        .validate_allowed_ips()
        .map_err(|e| anyhow::anyhow!(e))?;
    app_config
        .validate_templates()
        .map_err(|e| anyhow::anyhow!(e))?;

    let logging_config = get_logging_config(&app_config.log_level, &app_config.log_target);
    log4rs::init_config(logging_config)?;

    let db = Sqlite::new(&app_config.db_cnn).await?;

    let http_client = reqwest::Client::new();
    let forward_statuses = Arc::new(RwLock::new(HashMap::new()));
    for channel_cfg in &app_config.channels {
        if channel_cfg.forward.is_some() {
            forward_statuses.write().unwrap().insert(
                channel_cfg.name.clone(),
                ChannelForwardStatus::new(),
            );
        }
    }
    for channel_cfg in &app_config.channels {
        if let Some(forward_cfg) = channel_cfg.forward.clone() {
            let channel = WebhookChannel::new(channel_cfg.name.clone());
            let repo = db.clone();
            let client = http_client.clone();
            let ignored_headers = app_config.ignored_headers.clone();
            let statuses = forward_statuses.clone();

            tokio::spawn(background::forward::run_forwarder(
                channel,
                forward_cfg,
                channel_cfg.webhook_secret.clone(),
                repo,
                client,
                ignored_headers,
                statuses,
            ));

            log::info!(
                "started forwarder for channel={} → {}",
                channel_cfg.name,
                channel_cfg.forward.as_ref().unwrap().url
            );
        }
    }

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

    let mut app = Router::new();

    if app_config.api_enabled {
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
            .route("/api/webhook/{channel}/queue/pause", post(pause_queue_route))
            .route("/api/webhook/{channel}/queue/resume", post(resume_queue_route))
            .route("/api/webhook/{channel}/queue/clear", post(clear_queue_route))
            .route("/api/webhook/{channel}/queue/retry/{id}", post(retry_webhook_route));

        if app_config.metrics_enabled {
            app = app.route("/api/metrics", get(metrics_route));
        }
    } else {
        app = app.route("/api/{*path}", any(|| async { StatusCode::NOT_FOUND }));
        log::info!("REST API is disabled (API_ENABLED=0)");
    }

    if app_config.ui_enabled {
        app = app.fallback(static_files::static_file_handler);
    } else {
        log::info!("Web UI is disabled (UI_ENABLED=0)");
    }

    let app = app
        .layer(DefaultBodyLimit::max(app_config.max_body_limit()))
        .layer(axum::middleware::from_fn(
            middleware::client_ip::ClientIpExtractor::middleware,
        ))
        .layer(axum::Extension(app_config.trusted_proxies.clone()))
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
