use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use kwp_lib::domain::config::model::WebhookForwardConfig;
use kwp_lib::domain::crypto;
use kwp_lib::domain::webhook::model::{ChannelForwardStatus, WebhookChannel};
use kwp_lib::domain::webhook::ports::WebhookRepository;

/// After this many consecutive failed attempts a single error-level record is
/// emitted, so a permanently stuck webhook surfaces in Sentry without one event
/// per retry.
const STUCK_ATTEMPTS_THRESHOLD: i64 = 10;

fn inc_forward(channel: &WebhookChannel, status: &'static str, enabled: bool) {
    if !enabled {
        return;
    }
    metrics::counter!(
        "kwp_webhook_forward_total",
        "channel" => channel.as_str().to_string(),
        "status" => status
    )
    .increment(1);
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// Records a failed attempt and reports the webhook once it looks permanently stuck.
async fn record_failed_attempt<R: WebhookRepository>(
    repo: &R,
    channel: &WebhookChannel,
    id: i64,
    attempts_before: i64,
    error_msg: &str,
) {
    if let Err(e) = repo.increment_forward_attempts(id, error_msg).await {
        log::error!(
            "[forwarder:{}] failed to record attempt for id={}: {}",
            channel.as_str(),
            id,
            e
        );
    }

    if crossed_stuck_threshold(attempts_before) {
        log::error!(
            "[forwarder:{}] webhook id={} is still not delivered after {} attempts: {}",
            channel.as_str(),
            id,
            STUCK_ATTEMPTS_THRESHOLD,
            error_msg
        );
    }
}

/// True only for the attempt that crosses [`STUCK_ATTEMPTS_THRESHOLD`], so a stuck
/// webhook produces one report instead of one per retry.
fn crossed_stuck_threshold(attempts_before: i64) -> bool {
    attempts_before + 1 == STUCK_ATTEMPTS_THRESHOLD
}

fn update_status(
    statuses: &Arc<RwLock<HashMap<String, ChannelForwardStatus>>>,
    channel: &str,
    f: impl FnOnce(&mut ChannelForwardStatus),
) {
    if let Ok(mut map) = statuses.write()
        && let Some(status) = map.get_mut(channel)
    {
        f(status);
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn run_forwarder<R: WebhookRepository>(
    channel: WebhookChannel,
    forward_cfg: WebhookForwardConfig,
    webhook_secret: Option<String>,
    monitoring_metrics: bool,
    repo: R,
    http: reqwest::Client,
    ignored_headers: Vec<String>,
    forward_statuses: Arc<RwLock<HashMap<String, ChannelForwardStatus>>>,
) {
    let interval = Duration::from_secs(forward_cfg.interval_seconds);

    if let Ok(count) = repo.count_by_channel(&channel).await {
        update_status(&forward_statuses, channel.as_str(), |s| {
            s.queue_size = count;
        });
    }

    loop {
        let paused = forward_statuses
            .read()
            .ok()
            .and_then(|map| map.get(channel.as_str()).map(|s| s.paused))
            .unwrap_or(false);

        if paused {
            tokio::time::sleep(interval).await;
            continue;
        }

        match repo.peek_oldest_by_channel(&channel).await {
            Err(e) => {
                log::error!("[forwarder:{}] peek failed: {}", channel.as_str(), e);
                inc_forward(&channel, "internal_error", monitoring_metrics);
                tokio::time::sleep(interval).await;
            }
            Ok(None) => {
                log::debug!(
                    "[forwarder:{}] no pending webhooks, sleeping",
                    channel.as_str()
                );
                update_status(&forward_statuses, channel.as_str(), |s| {
                    s.queue_size = 0;
                });
                tokio::time::sleep(interval).await;
            }
            Ok(Some(webhook)) => {
                let id = match webhook.id {
                    Some(id) => id,
                    None => {
                        log::error!("[forwarder:{}] webhook has no id", channel.as_str());
                        inc_forward(&channel, "internal_error", monitoring_metrics);
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

                let body_bytes = webhook.payload.clone();

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
                    // Skip the sign header — it will be recomputed below with the correct secret
                    if forward_cfg
                        .sign_header
                        .as_deref()
                        .is_some_and(|h| h.eq_ignore_ascii_case(key))
                    {
                        continue;
                    }
                    request = request.header(key, value);
                }

                if let Some(sign_header) = &forward_cfg.sign_header {
                    let effective_secret = forward_cfg
                        .sign_secret
                        .as_deref()
                        .or(webhook_secret.as_deref());
                    let Some(sign_secret) = effective_secret else {
                        log::error!(
                            "[forwarder:{}] sign-header configured but no sign_secret or webhook_secret available",
                            channel.as_str()
                        );
                        inc_forward(&channel, "internal_error", monitoring_metrics);
                        tokio::time::sleep(interval).await;
                        continue;
                    };
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
                                inc_forward(&channel, "internal_error", monitoring_metrics);
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
                        inc_forward(&channel, "network_error", monitoring_metrics);

                        let error_msg = format!("network error: {cause}");
                        record_failed_attempt(
                            &repo,
                            &channel,
                            id,
                            webhook.forward_attempts,
                            &error_msg,
                        )
                        .await;
                        update_status(&forward_statuses, channel.as_str(), |s| {
                            s.last_error_at = Some(now_unix());
                            s.last_error_message = Some(error_msg.clone());
                        });

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
                            inc_forward(&channel, "ok", monitoring_metrics);
                            let removed = repo.delete_by_id(id).await;
                            if let Err(e) = &removed {
                                log::error!(
                                    "[forwarder:{}] delete_by_id({}) failed, webhook stays queued and will be delivered again: {}",
                                    channel.as_str(),
                                    id,
                                    e
                                );
                            }
                            update_status(&forward_statuses, channel.as_str(), |s| {
                                s.last_success_at = Some(now_unix());
                                s.queue_size = (s.queue_size - 1).max(0);
                            });

                            // The webhook was delivered but is still queued, so the
                            // next iteration would peek the same row and deliver it
                            // twice in a row. Back off instead and give whatever
                            // held the lock a chance to let go.
                            if removed.is_err() {
                                tokio::time::sleep(interval).await;
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
                            inc_forward(&channel, "unexpected_status", monitoring_metrics);

                            let error_msg = format!("HTTP {}: {}", status.as_u16(), body_preview);
                            record_failed_attempt(
                                &repo,
                                &channel,
                                id,
                                webhook.forward_attempts,
                                &error_msg,
                            )
                            .await;
                            update_status(&forward_statuses, channel.as_str(), |s| {
                                s.last_error_at = Some(now_unix());
                                s.last_error_message = Some(error_msg.clone());
                            });

                            tokio::time::sleep(interval).await;
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stuck_webhook_is_reported_once() {
        let reported: Vec<i64> = (0..20).filter(|a| crossed_stuck_threshold(*a)).collect();

        assert_eq!(
            reported,
            vec![STUCK_ATTEMPTS_THRESHOLD - 1],
            "exactly one attempt out of a long retry run may report"
        );
    }

    #[test]
    fn early_attempts_are_not_reported() {
        assert!(!crossed_stuck_threshold(0));
        assert!(!crossed_stuck_threshold(1));
    }
}
