use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use kwp_lib::domain::config::model::WebhookForwardConfig;
use kwp_lib::domain::crypto;
use kwp_lib::domain::webhook::backoff::{self, FailureKind, ForwardBackoff};
use kwp_lib::domain::webhook::model::{ChannelForwardStatus, Webhook, WebhookChannel};
use kwp_lib::domain::webhook::ports::WebhookRepository;

/// After this many consecutive failed attempts a single error-level record is
/// emitted, so a permanently stuck webhook surfaces in Sentry without one event
/// per retry.
const STUCK_ATTEMPTS_THRESHOLD: i64 = 10;

type ForwardStatuses = Arc<RwLock<HashMap<String, ChannelForwardStatus>>>;

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// True only for the attempt that crosses [`STUCK_ATTEMPTS_THRESHOLD`], so a stuck
/// webhook produces one report instead of one per retry.
fn crossed_stuck_threshold(attempts_before: i64) -> bool {
    attempts_before + 1 == STUCK_ATTEMPTS_THRESHOLD
}

/// What a delivery attempt left behind, so the caller knows what to record.
struct Failure {
    kind: FailureKind,
    /// Delay the target asked for via `Retry-After`, if it sent a usable one.
    retry_after: Option<Duration>,
    message: String,
    /// Value of the `status` metric label.
    metric: &'static str,
}

/// Result of one pass over a channel's queue.
enum Progress {
    /// A webhook was delivered and removed, so the next one can be tried at once.
    Drained,
    /// Nothing more to do for now — wait out the channel's interval.
    Idle,
}

#[allow(clippy::too_many_arguments)]
pub async fn run_forwarder<R: WebhookRepository>(
    channel: WebhookChannel,
    forward_cfg: WebhookForwardConfig,
    backoff: ForwardBackoff,
    webhook_secret: Option<String>,
    monitoring_metrics: bool,
    repo: R,
    http: reqwest::Client,
    ignored_headers: Vec<String>,
    forward_statuses: ForwardStatuses,
) {
    Forwarder {
        channel,
        cfg: forward_cfg,
        backoff,
        webhook_secret,
        monitoring_metrics,
        repo,
        http,
        ignored_headers,
        statuses: forward_statuses,
    }
    .run()
    .await
}

struct Forwarder<R: WebhookRepository> {
    channel: WebhookChannel,
    cfg: WebhookForwardConfig,
    backoff: ForwardBackoff,
    webhook_secret: Option<String>,
    monitoring_metrics: bool,
    repo: R,
    http: reqwest::Client,
    ignored_headers: Vec<String>,
    statuses: ForwardStatuses,
}

impl<R: WebhookRepository> Forwarder<R> {
    async fn run(self) {
        let interval = Duration::from_secs(self.cfg.interval_seconds);

        self.refresh_queue_size().await;

        loop {
            if self.is_paused() {
                tokio::time::sleep(interval).await;
                continue;
            }

            match self.forward_next().await {
                // A delivered webhook says the target is healthy, so drain the rest
                // of the queue without waiting out the interval.
                Progress::Drained => continue,
                Progress::Idle => tokio::time::sleep(interval).await,
            }
        }
    }

    fn name(&self) -> &str {
        self.channel.as_str()
    }

    fn is_paused(&self) -> bool {
        self.statuses
            .read()
            .ok()
            .and_then(|map| map.get(self.name()).map(|status| status.paused))
            .unwrap_or(false)
    }

    fn update_status(&self, f: impl FnOnce(&mut ChannelForwardStatus)) {
        if let Ok(mut map) = self.statuses.write()
            && let Some(status) = map.get_mut(self.name())
        {
            f(status);
        }
    }

    fn inc_forward(&self, status: &'static str) {
        if !self.monitoring_metrics {
            return;
        }
        metrics::counter!(
            "kwp_webhook_forward_total",
            "channel" => self.channel.as_str().to_string(),
            "status" => status
        )
        .increment(1);
    }

    /// Keeps the queue size the UI shows in step with the database.
    ///
    /// Needed because a queue can be non-empty while nothing in it is due, so the
    /// forwarder cannot infer the size from "no webhook to send".
    async fn refresh_queue_size(&self) {
        match self.repo.count_by_channel(&self.channel).await {
            Ok(count) => self.update_status(|status| status.queue_size = count),
            // Forwarding is unaffected — only the number the UI shows goes stale —
            // so this is not worth an error, but it has to be visible: otherwise a
            // wrong queue size in the UI has no explanation anywhere.
            Err(e) => log::warn!(
                "[forwarder:{}] queue size refresh failed, the UI count stays stale: {}",
                self.name(),
                e
            ),
        }
    }

    async fn forward_next(&self) -> Progress {
        let webhook = match self
            .repo
            .peek_oldest_due_by_channel(&self.channel, now_unix())
            .await
        {
            Err(e) => {
                log::error!("[forwarder:{}] peek failed: {}", self.name(), e);
                self.inc_forward("internal_error");
                return Progress::Idle;
            }
            Ok(None) => {
                log::debug!("[forwarder:{}] no webhook is due, sleeping", self.name());
                self.refresh_queue_size().await;
                return Progress::Idle;
            }
            Ok(Some(webhook)) => webhook,
        };

        let Some(id) = webhook.id else {
            log::error!("[forwarder:{}] webhook has no id", self.name());
            self.inc_forward("internal_error");
            return Progress::Idle;
        };

        log::debug!(
            "[forwarder:{}] forwarding webhook id={} to {}",
            self.name(),
            id,
            self.cfg.url
        );

        let Some(request) = self.build_request(&webhook) else {
            self.inc_forward("internal_error");
            return Progress::Idle;
        };

        match request.send().await {
            Err(e) => {
                let cause = describe_error(&e);
                log::warn!("[forwarder:{}] request failed: {}", self.name(), cause);

                self.record_failure(
                    id,
                    webhook.forward_attempts,
                    Failure {
                        kind: FailureKind::Transient,
                        retry_after: None,
                        message: format!("network error: {cause}"),
                        metric: "network_error",
                    },
                )
                .await;

                Progress::Idle
            }
            Ok(resp) => {
                let status = resp.status();

                if status.as_u16() == self.cfg.expected_status {
                    return self.on_delivered(id).await;
                }

                // Read before the body: consuming the response drops the headers.
                let retry_after = resp
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(backoff::parse_retry_after);

                let body_preview = read_body_preview(resp).await;
                log::warn!(
                    "[forwarder:{}] unexpected status {} from {}: {}",
                    self.name(),
                    status,
                    self.cfg.url,
                    body_preview
                );

                let kind = backoff::classify_status(status.as_u16());
                self.record_failure(
                    id,
                    webhook.forward_attempts,
                    Failure {
                        kind,
                        retry_after,
                        message: format!("HTTP {}: {}", status.as_u16(), body_preview),
                        metric: match kind {
                            FailureKind::Rejected => "client_error",
                            FailureKind::Transient => "unexpected_status",
                        },
                    },
                )
                .await;

                Progress::Idle
            }
        }
    }

    /// Builds the forward request, or returns `None` when the channel is
    /// misconfigured in a way that no retry can fix.
    fn build_request(&self, webhook: &Webhook) -> Option<reqwest::RequestBuilder> {
        let body_bytes = webhook.payload.clone();

        let mut request = self
            .http
            .post(&self.cfg.url)
            .timeout(Duration::from_secs(self.cfg.timeout_seconds))
            .header("content-type", "application/json")
            .body(body_bytes.clone());

        for (key, value) in &webhook.headers {
            if self.ignored_headers.contains(key) {
                continue;
            }
            // Skip the sign header — it will be recomputed below with the correct secret
            if self
                .cfg
                .sign_header
                .as_deref()
                .is_some_and(|h| h.eq_ignore_ascii_case(key))
            {
                continue;
            }
            request = request.header(key, value);
        }

        let Some(sign_header) = &self.cfg.sign_header else {
            return Some(request);
        };

        let effective_secret = self
            .cfg
            .sign_secret
            .as_deref()
            .or(self.webhook_secret.as_deref());
        let Some(sign_secret) = effective_secret else {
            log::error!(
                "[forwarder:{}] sign-header configured but no sign_secret or webhook_secret available",
                self.name()
            );
            return None;
        };

        let signature = crypto::hmac_sha256_hex(sign_secret.as_bytes(), &body_bytes);
        let header_value = match self.cfg.sign_template.as_deref() {
            None => signature,
            Some(tmpl) => match crypto::render_sign_template(tmpl, &signature) {
                Ok(value) => value,
                Err(e) => {
                    log::error!(
                        "[forwarder:{}] sign-template render failed: {}",
                        self.name(),
                        e
                    );
                    return None;
                }
            },
        };

        Some(request.header(sign_header.as_str(), header_value))
    }

    async fn on_delivered(&self, id: i64) -> Progress {
        log::info!(
            "[forwarder:{}] successfully forwarded webhook id={} → {}",
            self.name(),
            id,
            self.cfg.url
        );
        self.inc_forward("ok");

        let removed = self.repo.delete_by_id(id).await;
        if let Err(e) = &removed {
            log::error!(
                "[forwarder:{}] delete_by_id({}) failed, webhook stays queued and will be delivered again: {}",
                self.name(),
                id,
                e
            );
        }

        self.update_status(|status| {
            status.last_success_at = Some(now_unix());
            status.queue_size = (status.queue_size - 1).max(0);
        });

        // The webhook was delivered but is still queued, so the next iteration
        // would peek the same row and deliver it twice in a row. Back off instead
        // and give whatever held the lock a chance to let go.
        if removed.is_err() {
            return Progress::Idle;
        }

        Progress::Drained
    }

    /// Stores the failure, parks the webhook for its backoff delay, and reports it
    /// once it looks permanently stuck.
    async fn record_failure(&self, id: i64, attempts_before: i64, failure: Failure) {
        let attempts = attempts_before + 1;
        let delay = backoff::next_delay(
            &self.backoff,
            failure.kind,
            attempts,
            failure.retry_after,
            backoff::clock_jitter_unit(),
        );
        let next_attempt_at = now_unix() + delay.as_secs() as i64;

        self.inc_forward(failure.metric);

        if let Err(e) = self
            .repo
            .record_forward_failure(id, &failure.message, next_attempt_at)
            .await
        {
            // The delay could not be stored, so the next pass will retry this
            // webhook immediately. That is the safe direction — at worst the target
            // sees the old, fixed cadence.
            log::error!(
                "[forwarder:{}] failed to record attempt for id={}: {}",
                self.name(),
                id,
                e
            );
        }

        log::info!(
            "[forwarder:{}] webhook id={} failed {} time(s), next attempt in {}s",
            self.name(),
            id,
            attempts,
            delay.as_secs()
        );

        if crossed_stuck_threshold(attempts_before) {
            log::error!(
                "[forwarder:{}] webhook id={} is still not delivered after {} attempts: {}",
                self.name(),
                id,
                STUCK_ATTEMPTS_THRESHOLD,
                failure.message
            );
        }

        self.update_status(|status| {
            status.last_error_at = Some(now_unix());
            status.last_error_message = Some(failure.message);
        });
    }
}

/// Flattens an error chain into one line, so the cause is not lost in the log.
fn describe_error(error: &reqwest::Error) -> String {
    let mut description = format!("{error}");
    let mut source: &dyn std::error::Error = error;

    while let Some(next) = source.source() {
        description.push_str(&format!(": {next}"));
        source = next;
    }

    description
}

async fn read_body_preview(resp: reqwest::Response) -> String {
    const MAX_BODY: usize = 512;

    let body = resp
        .text()
        .await
        .unwrap_or_else(|e| format!("<failed to read body: {e}>"));

    if body.len() > MAX_BODY {
        return format!("{}…({} bytes total)", &body[..MAX_BODY], body.len());
    }

    body
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
