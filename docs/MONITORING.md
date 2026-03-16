# Monitoring

Service exposes Prometheus-compatible metrics at `GET /api/metrics`.

## Enabling

Metrics are **disabled by default**. Set the environment variable to enable them:

```
METRICS_ENABLED=1
```

Accepted values: `1` or `true`. Any other value (or the variable being absent) keeps metrics disabled.

When disabled, the `/api/metrics` endpoint returns `404`. Counter calls in the code are no-ops — there is no performance cost.

## Architecture

Counters are kept **in process memory** using the [`metrics`](https://docs.rs/metrics) crate as a facade and [`metrics-exporter-prometheus`](https://docs.rs/metrics-exporter-prometheus) as the recorder.

When `METRICS_ENABLED=1` is set, a `PrometheusHandle` is installed at startup and stored in `AppState`. On each `GET /api/metrics` request, the handle renders the current counter values from memory into Prometheus text format and returns them in the response body.

There is **no external storage** — counters live only in RAM and reset to zero every time the process restarts. This means:

- No additional infrastructure is required (no StatsD, no push gateway).
- Historical data is not preserved across restarts.
- The intended use is to let Prometheus scrape the endpoint at regular intervals and store the time series itself.

When `METRICS_ENABLED` is absent or set to any other value, no recorder is installed. The `inc_receive` / `inc_forward` calls in the route and background task code still execute, but the `metrics` crate silently discards them — there is no measurable overhead.

## Endpoint

```
GET /api/metrics
Content-Type: text/plain; version=0.0.4; charset=utf-8
```

The response is in the [Prometheus text exposition format](https://prometheus.io/docs/instrumenting/exposition_formats/).

## Available Metrics

### `kwp_webhook_receive_total`

Counts incoming webhook requests, labeled by channel and outcome.

| Label | Description |
|---|---|
| `channel` | Channel name from the URL path |
| `status` | Outcome of the request (see table below) |

**Status values:**

| `status` | HTTP code | Meaning |
|---|---|---|
| `ok` | 200 | Webhook accepted and stored |
| `channel_not_found` | 404 | No channel with this name exists |
| `ip_blocked` | 403 | Sender IP is not in the channel's `allowed-ips` list |
| `unauthorized` | 401 | Secret header is missing or does not match |
| `payload_too_large` | 413 | Request body exceeds the configured size limit |
| `invalid_content_type` | 415 | `Content-Type` is not `application/json` |
| `invalid_json` | 422 | Request body is not valid JSON |
| `internal_error` | 500 | Database error or template rendering failure |

### `kwp_webhook_forward_total`

Counts outgoing forwarding attempts, labeled by channel and outcome.

| Label | Description |
|---|---|
| `channel` | Channel name |
| `status` | Outcome of the forwarding attempt (see table below) |

**Status values:**

| `status` | Meaning |
|---|---|
| `ok` | Webhook forwarded successfully (target returned the expected HTTP status) |
| `network_error` | HTTP request failed — timeout, DNS failure, or connection error |
| `unexpected_status` | Target responded with a status code different from `expected-status` in config |
| `internal_error` | Could not read from DB, serialize payload, or render sign template |

### `kwp_channel_security_config`

Gauge exposing the security posture of each channel. Set once at startup.

| Label | Description |
|---|---|
| `channel` | Channel name |
| `feature` | Security feature being checked (see table below) |

**Feature values:**

| `feature` | Value `1` | Value `0` |
|---|---|---|
| `ip_allowlist` | `allowed-ips` is configured | No IP restriction — any source accepted |
| `secret` | `webhook-secret` is configured | No secret validation — requests not authenticated |

## Example Output

```
# HELP kwp_webhook_receive_total
# TYPE kwp_webhook_receive_total counter
kwp_webhook_receive_total{channel="telegram",status="ok"} 42
kwp_webhook_receive_total{channel="telegram",status="unauthorized"} 3
kwp_webhook_receive_total{channel="github",status="ok"} 17

# HELP kwp_webhook_forward_total
# TYPE kwp_webhook_forward_total counter
kwp_webhook_forward_total{channel="telegram",status="ok"} 41
kwp_webhook_forward_total{channel="telegram",status="network_error"} 1
```

```
# HELP kwp_channel_security_config
# TYPE kwp_channel_security_config gauge
kwp_channel_security_config{channel="telegram",feature="ip_allowlist"} 1
kwp_channel_security_config{channel="telegram",feature="secret"} 1
kwp_channel_security_config{channel="open",feature="ip_allowlist"} 0
kwp_channel_security_config{channel="open",feature="secret"} 0
```

## Scrape Configuration

Add this to your `prometheus.yml`:

```yaml
scrape_configs:
  - job_name: kwp
    static_configs:
      - targets: ["localhost:3000"]
    metrics_path: /api/metrics
    scrape_interval: 15s
    scrape_timeout: 5s
```

## Alerting Rules

### Prometheus Alert Rules

Save as `kwp-alerts.yml` and include in your Prometheus configuration:

```yaml
groups:
  - name: kwp
    interval: 30s
    rules:
      # High rate of rejected webhooks (unauthorized, IP blocked, invalid JSON, etc.)
      - alert: KWPHighRejectionRate
        expr: |
          (
            rate(kwp_webhook_receive_total{status=~"unauthorized|ip_blocked|invalid_.*|payload_too_large"}[5m])
            /
            rate(kwp_webhook_receive_total[5m]) > 0.1
          )
        for: 5m
        labels:
          severity: warning
        annotations:
          summary: "High webhook rejection rate on channel {{ $labels.channel }}"
          description: "Channel {{ $labels.channel }} is rejecting more than 10% of incoming webhooks over the last 5 minutes."

      # High error rate in webhook forwarding
      - alert: KWPForwardingErrors
        expr: |
          (
            rate(kwp_webhook_forward_total{status=~"network_error|unexpected_status|internal_error"}[5m])
            /
            rate(kwp_webhook_forward_total[5m]) > 0.2
          )
        for: 5m
        labels:
          severity: critical
        annotations:
          summary: "High webhook forwarding error rate on channel {{ $labels.channel }}"
          description: "Channel {{ $labels.channel }} is failing to forward more than 20% of webhooks. Check target service health and network connectivity."

      # No webhooks received in a long time (configured channel should receive webhooks)
      - alert: KWPNoIncomingWebhooks
        expr: |
          rate(kwp_webhook_receive_total{status="ok"}[10m]) == 0
          and
          kwp_webhook_receive_total > 0
        for: 30m
        labels:
          severity: warning
        annotations:
          summary: "No webhooks received on channel {{ $labels.channel }} for 30 minutes"
          description: "Channel {{ $labels.channel }} previously received webhooks but has not received any in the last 30 minutes. Check upstream service integration."

      # No successful webhook forwards in a long time
      - alert: KWPForwardingStalled
        expr: |
          rate(kwp_webhook_forward_total{status="ok"}[10m]) == 0
          and
          kwp_webhook_forward_total{status="ok"} > 0
        for: 15m
        labels:
          severity: critical
        annotations:
          summary: "No successful webhook forwards on channel {{ $labels.channel }} for 15 minutes"
          description: "Channel {{ $labels.channel }} has not successfully forwarded any webhooks in 15 minutes. Check target service and network."

      # Discrepancy between received and forwarded webhooks
      - alert: KWPQueueBacklog
        expr: |
          (
            increase(kwp_webhook_receive_total{status="ok"}[5m])
            -
            increase(kwp_webhook_forward_total{status="ok"}[5m])
          ) > 100
        for: 10m
        labels:
          severity: warning
        annotations:
          summary: "Large webhook backlog on channel {{ $labels.channel }}"
          description: "Channel {{ $labels.channel }} has more than 100 unprocessed webhooks. Forwarding service may be slow or stuck."

      # All webhooks are being rejected due to authentication
      - alert: KWPAuthenticationFailure
        expr: |
          rate(kwp_webhook_receive_total{status="unauthorized"}[5m]) > 0
          and
          rate(kwp_webhook_receive_total{status="ok"}[5m]) == 0
        for: 5m
        labels:
          severity: critical
        annotations:
          summary: "All webhooks rejected (authentication) on channel {{ $labels.channel }}"
          description: "Channel {{ $labels.channel }} is rejecting all webhooks due to authentication failures. Check if upstream secret has changed."

      # Channel has no webhook secret configured
      - alert: KwpChannelNoSecret
        expr: kwp_channel_security_config{feature="secret"} == 0
        labels:
          severity: warning
        annotations:
          summary: "Channel {{ $labels.channel }} has no webhook secret"
          description: "Channel {{ $labels.channel }} does not have webhook-secret configured. Incoming webhooks are not authenticated."

      # Channel has no IP allowlist configured
      - alert: KwpChannelNoIpAllowlist
        expr: kwp_channel_security_config{feature="ip_allowlist"} == 0
        labels:
          severity: warning
        annotations:
          summary: "Channel {{ $labels.channel }} has no IP allowlist"
          description: "Channel {{ $labels.channel }} does not have allowed-ips configured. Any source IP can send webhooks."

      # Internal server errors
      - alert: KWPInternalErrors
        expr: rate(kwp_webhook_receive_total{status="internal_error"}[5m]) > 0.01
        for: 5m
        labels:
          severity: critical
        annotations:
          summary: "Internal server errors on channel {{ $labels.channel }}"
          description: "Channel {{ $labels.channel }} is experiencing internal errors (database, template rendering). Check service logs."
```

### VictoriaMetrics Alert Rules

VictoriaMetrics uses the same Prometheus alert rule syntax. Save the rules above and include them in your VictoriaMetrics config:

```yaml
rule_files:
  - "/etc/victoriametrics/kwp-alerts.yml"

scrape_configs:
  - job_name: kwp
    static_configs:
      - targets: ["localhost:3000"]
    metrics_path: /api/metrics
```

### Alert Manager Configuration

Example `alertmanager.yml` for routing KWP alerts to Slack:

```yaml
global:
  resolve_timeout: 5m

route:
  receiver: default
  group_by: ['alertname', 'cluster', 'service']
  group_wait: 10s
  group_interval: 10s
  repeat_interval: 12h

  routes:
    - match:
        job: kwp
      receiver: kwp_team
      group_wait: 5s
      repeat_interval: 1h

receivers:
  - name: default
    slack_configs:
      - api_url: ${SLACK_WEBHOOK_URL}

  - name: kwp_team
    slack_configs:
      - api_url: ${SLACK_WEBHOOK_URL}
        channel: '#webhooks'
        title: 'KWP Alert: {{ .GroupLabels.alertname }}'
        text: '{{ range .Alerts }}{{ .Annotations.description }}{{ end }}'
        color: '{{ if eq .Status "firing" }}danger{{ else }}good{{ end }}'
```

### Grafana Dashboard Query Examples

Useful queries to include in your Grafana dashboard:

```promql
# Webhooks received per second (last 5m)
rate(kwp_webhook_receive_total{status="ok"}[5m])

# Webhook forwarding success rate
rate(kwp_webhook_forward_total{status="ok"}[5m])
/
rate(kwp_webhook_forward_total[5m])

# Rejection reasons breakdown
rate(kwp_webhook_receive_total{status!="ok"}[5m])

# Forwarding errors by type
rate(kwp_webhook_forward_total{status=~"network_error|unexpected_status|internal_error"}[5m])

# Total pending webhooks (estimate)
increase(kwp_webhook_receive_total{status="ok"}[1h])
-
increase(kwp_webhook_forward_total{status="ok"}[1h])

# P95 webhook processing latency (if latency histograms are added in future)
histogram_quantile(0.95, kwp_webhook_forward_duration_seconds_bucket)
```
