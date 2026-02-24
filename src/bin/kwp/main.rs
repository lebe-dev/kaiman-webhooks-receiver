use std::sync::Arc;

use axum::{
    Router,
    extract::DefaultBodyLimit,
    routing::{get, post},
};
use kwp_lib::VERSION;
use kwp_lib::domain::config::model::AppConfig;
use kwp_lib::domain::config::ports::AppConfigLoader;
use kwp_lib::domain::webhook::model::WebhookChannel;
use kwp_lib::domain::webhook::service::WebhookServiceImpl;
use kwp_lib::outbound::config::EnvConfigLoader;
use kwp_lib::outbound::sqlite::Sqlite;
use logger::get_logging_config;
use route::{read_webhooks::read_webhooks_route, receive_webhook::receive_webhook_route};

use crate::route::version::get_version_route;

pub mod background;
pub mod logger;
pub mod route;

#[derive(Clone)]
pub struct AppState {
    pub config: AppConfig,
    pub webhook_service: WebhookServiceImpl<Sqlite>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    let config_loader = EnvConfigLoader;
    let app_config = config_loader.load()?;
    app_config
        .validate_body_limits()
        .map_err(|e| anyhow::anyhow!(e))?;

    let logging_config = get_logging_config(&app_config.log_level, &app_config.log_target);
    log4rs::init_config(logging_config)?;

    let db = Sqlite::new(&app_config.db_cnn).await?;

    let http_client = reqwest::Client::new();
    for channel_cfg in &app_config.channels {
        if let Some(forward_cfg) = channel_cfg.forward.clone() {
            let channel = WebhookChannel::new(channel_cfg.name.clone());
            let repo = db.clone();
            let client = http_client.clone();

            tokio::spawn(background::forward::run_forwarder(
                channel,
                forward_cfg,
                repo,
                client,
            ));

            log::info!(
                "started forwarder for channel={} → {}",
                channel_cfg.name,
                channel_cfg.forward.as_ref().unwrap().url
            );
        }
    }

    let webhook_service = WebhookServiceImpl::new(db);

    let app_state = Arc::new(AppState {
        config: app_config.clone(),
        webhook_service,
    });

    let app = Router::new()
        .route("/api/version", get(get_version_route))
        .route("/api/webhook/{channel}", post(receive_webhook_route))
        .route("/api/webhook/{channel}", get(read_webhooks_route))
        .layer(DefaultBodyLimit::max(app_config.max_body_limit()))
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

    let server = axum::serve(listener, app);

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
