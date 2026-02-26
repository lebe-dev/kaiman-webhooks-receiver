use std::time::Duration;

use kwp_lib::domain::config::model::WebhookForwardConfig;
use kwp_lib::domain::crypto;
use kwp_lib::domain::webhook::model::WebhookChannel;
use kwp_lib::domain::webhook::ports::WebhookRepository;

fn inc_forward(channel: &WebhookChannel, status: &'static str) {
    metrics::counter!(
        "kwp_webhook_forward_total",
        "channel" => channel.as_str().to_string(),
        "status" => status
    )
    .increment(1);
}

pub async fn run_forwarder<R: WebhookRepository>(
    channel: WebhookChannel,
    forward_cfg: WebhookForwardConfig,
    repo: R,
    http: reqwest::Client,
    ignored_headers: Vec<String>,
) {
    let interval = Duration::from_secs(forward_cfg.interval_seconds);

    loop {
        match repo.peek_oldest_by_channel(&channel).await {
            Err(e) => {
                log::error!("[forwarder:{}] peek failed: {}", channel.as_str(), e);
                inc_forward(&channel, "internal_error");
                tokio::time::sleep(interval).await;
            }
            Ok(None) => {
                log::debug!(
                    "[forwarder:{}] no pending webhooks, sleeping",
                    channel.as_str()
                );
                tokio::time::sleep(interval).await;
            }
            Ok(Some(webhook)) => {
                let id = match webhook.id {
                    Some(id) => id,
                    None => {
                        log::error!("[forwarder:{}] webhook has no id", channel.as_str());
                        inc_forward(&channel, "internal_error");
                        tokio::time::sleep(interval).await;
                        continue;
                    }
                };

                log::debug!(
                    "[forwarder:{}] forwarding webhook id={} to {}",
                    channel.as_str(),
                    id,
                    forward_cfg.url
                );

                let body_bytes = match serde_json::to_vec(&webhook.payload) {
                    Ok(b) => b,
                    Err(e) => {
                        log::error!(
                            "[forwarder:{}] failed to serialize payload: {}",
                            channel.as_str(),
                            e
                        );
                        inc_forward(&channel, "internal_error");
                        tokio::time::sleep(interval).await;
                        continue;
                    }
                };

                let timeout = Duration::from_secs(forward_cfg.timeout_seconds);
                let mut request = http
                    .post(&forward_cfg.url)
                    .timeout(timeout)
                    .header("content-type", "application/json")
                    .body(body_bytes.clone());

                for (key, value) in &webhook.headers {
                    if ignored_headers.contains(key) {
                        continue;
                    }
                    request = request.header(key, value);
                }

                if let (Some(sign_header), Some(sign_secret)) =
                    (&forward_cfg.sign_header, &forward_cfg.sign_secret)
                {
                    let sig = crypto::hmac_sha256_hex(sign_secret.as_bytes(), &body_bytes);
                    let header_value = match forward_cfg.sign_template.as_deref() {
                        Some(tmpl) => match crypto::render_sign_template(tmpl, &sig) {
                            Ok(v) => v,
                            Err(e) => {
                                log::error!(
                                    "[forwarder:{}] sign-template render failed: {}",
                                    channel.as_str(),
                                    e
                                );
                                inc_forward(&channel, "internal_error");
                                tokio::time::sleep(interval).await;
                                continue;
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
                        log::warn!("[forwarder:{}] request failed: {}", channel.as_str(), cause);
                        inc_forward(&channel, "network_error");
                        tokio::time::sleep(interval).await;
                    }
                    Ok(resp) => {
                        let status = resp.status();
                        if status.as_u16() == forward_cfg.expected_status {
                            log::info!(
                                "[forwarder:{}] successfully forwarded webhook id={} → {}",
                                channel.as_str(),
                                id,
                                forward_cfg.url
                            );
                            inc_forward(&channel, "ok");
                            if let Err(e) = repo.delete_by_id(id).await {
                                log::error!(
                                    "[forwarder:{}] delete_by_id({}) failed: {}",
                                    channel.as_str(),
                                    id,
                                    e
                                );
                            }
                        } else {
                            let body = resp
                                .text()
                                .await
                                .unwrap_or_else(|e| format!("<failed to read body: {e}>"));
                            const MAX_BODY: usize = 512;
                            let body_preview = if body.len() > MAX_BODY {
                                format!("{}…({} bytes total)", &body[..MAX_BODY], body.len())
                            } else {
                                body
                            };
                            log::warn!(
                                "[forwarder:{}] unexpected status {} from {}: {}",
                                channel.as_str(),
                                status,
                                forward_cfg.url,
                                body_preview
                            );
                            inc_forward(&channel, "unexpected_status");
                            tokio::time::sleep(interval).await;
                        }
                    }
                }
            }
        }
    }
}
