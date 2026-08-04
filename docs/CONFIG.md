# Configuration Reference

This document describes the configuration options available for Kaiman Webhooks Proxy.

## Environment Variables

These variables can be set in a `.env` file or directly in your environment.

| Variable | Default Value | Description |
| :--- | :--- | :--- |
| `BIND_ADDRESS` | `0.0.0.0:8080` | The address and port the server listens on. |
| `LOG_LEVEL` | `info` | Logging verbosity (e.g., `debug`, `info`, `warn`, `error`). |
| `LOG_TARGET` | `stdout` | Destination for logs (e.g., `stdout`). |
| `DATA_PATH` | `./data` | Path to the directory where data is stored. |
| `DATABASE_URL` | `sqlite://./data/kwp.db?mode=rwc` | Connection string for the SQLite database. |
| `CONFIG_FILE` | `config.yml` | Path to the YAML configuration file. |
| `SENTRY_DSN` | — | Sentry project DSN. Absent or empty disables error reporting. See [MONITORING.md](MONITORING.md#error-reporting-sentry). |
| `SENTRY_ENVIRONMENT` | `production` | Environment tag attached to Sentry events (`production`, `staging`, …). Only used when `SENTRY_DSN` is set. |

## YAML Configuration (`config.yml`)

The YAML file defines the channels and their security settings.

### Channels

The `channels` key contains a list of channel configurations.

| Field | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `name` | string | required | A unique name for the channel. |
| `api-read-token` | string | required | Bearer token used to authenticate requests for reading webhooks from this channel. |
| `webhook-secret` | string | — | (Optional) The secret key used to verify the authenticity of incoming webhooks. |
| `secret-header` | string | — | (Optional) The HTTP header name that contains the verification token or signature. |
| `secret-type` | enum | `plain` | Verification mode: `plain` (constant-time byte comparison) or `hmac-sha256`. |
| `secret-extract-template` | string | `{{ raw }}` | Tera template to extract the hex signature from the header value. Variable: `raw`. |
| `max-body-size` | integer | — | (Optional) Maximum request body size in bytes for this channel. |
| `allowed-ips` | list | — | (Optional) List of allowed source IPs or CIDR ranges. |
| `forward` | object | — | (Optional) Auto-forward webhooks to a target URL. |

#### `secret-type` values

- **`plain`** (default): incoming header value is compared byte-for-byte (constant-time) against `webhook-secret`. Used by Telegram.
- **`hmac-sha256`**: incoming header value is expected to contain a hex HMAC-SHA256 signature. The proxy computes `HMAC-SHA256(webhook-secret, request_body)` and compares with constant-time equality. Used by GitHub, Stripe, Shopify, etc.

> When using `hmac-sha256`, both `webhook-secret` and `secret-header` are required.

#### `secret-extract-template`

A [Tera](https://keats.github.io/tera/) template used to extract the raw hex signature from the header value before comparison.

Variable available: `raw` — the full header value as a string.

| Example | Input | Output |
| :--- | :--- | :--- |
| `{{ raw }}` (default) | `abc123` | `abc123` |
| `{{ raw \| replace(from="sha256=", to="") }}` | `sha256=abc123` | `abc123` |

Available Tera filters: `replace`, `split`, `last`, `trim`, `lower`, `upper`.

### Forward Configuration

| Field | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `url` | string | required | Target URL to forward webhooks to. |
| `interval-seconds` | integer | required | How often (in seconds) to check for pending webhooks. |
| `expected-status` | integer | `200` | HTTP status code that indicates successful delivery. |
| `timeout-seconds` | integer | `15` | Request timeout in seconds. |
| `sign-header` | string | — | (Optional) Header name to attach the HMAC signature to outgoing requests. |
| `sign-secret` | string | — | (Optional) Secret key for HMAC-SHA256 signing of outgoing requests. |
| `sign-template` | string | `{{ signature }}` | Tera template to format the signature into the header value. Variable: `signature`. |

> `sign-header` and `sign-secret` must be configured together.

#### `sign-template`

A Tera template to format the computed hex HMAC-SHA256 signature into the header value sent to the target.

Variable available: `signature` — the hex HMAC-SHA256 signature.

| Example | Output |
| :--- | :--- |
| `{{ signature }}` (default) | `abc123...` |
| `sha256={{ signature }}` | `sha256=abc123...` |

#### Examples

```yaml
channels:
  # Telegram (plain secret — default)
  - name: telegram
    api-read-token: "token"
    webhook-secret: "secret"
    secret-header: "X-Telegram-Bot-Api-Secret-Token"

  # GitHub (HMAC-SHA256)
  - name: github
    api-read-token: "token"
    webhook-secret: "secret"
    secret-header: "X-Hub-Signature-256"
    secret-type: hmac-sha256
    secret-extract-template: '{{ raw | replace(from="sha256=", to="") }}'
    forward:
      url: "https://target/hook"
      interval-seconds: 30
      sign-header: "X-Hub-Signature-256"
      sign-secret: "forward_secret"
      sign-template: "sha256={{ signature }}"

  # Open channel (no verification)
  - name: open
    api-read-token: "token"
```

## Storage contention (SQLite locking)

SQLite allows exactly one writer at a time. KWP writes on every received webhook,
on every forwarding attempt and on every successful delivery, so writers do collide
under load. The storage adapter is configured to absorb this without operator
intervention:

| Setting | Value | Why |
| :--- | :--- | :--- |
| `journal_mode` | `WAL` | Readers never block on the writer, so polling and the Web UI stay responsive while webhooks are being stored. |
| `synchronous` | `NORMAL` | The recommended companion to WAL. Removes the per-commit `fsync`, which is what keeps the write lock held. An OS crash or power loss can lose the most recent commits; the database itself stays intact, and an application crash loses nothing. |
| `busy_timeout` | 2s | How long SQLite waits for the lock before reporting `SQLITE_BUSY`. |
| Retry budget | 4s | Statements that still hit the lock are retried with exponential backoff and jitter. |
| Pool size | 8 connections | Writes serialise regardless, so a larger pool would only add waiters. |
| Acquire timeout | 3s | sqlx defaults to 30s, which outlives a webhook sender's own timeout. |

The worst case (2s + 3s + 4s) stays below the ~10s a sender such as GitHub waits, so
a request always gets an answer in time for the sender to react.

If contention outlives the retry budget, the request is answered with
**`503 Service Unavailable` and a `Retry-After` header** rather than `500`. Nothing
was written in that case, so a redelivery cannot duplicate anything. The
`kwp_webhook_receive_total{status="storage_busy"}` metric counts these; see
[MONITORING.md](MONITORING.md#kwp_webhook_receive_total).

Recommendations for avoiding contention in the first place:

- **Keep `DATA_PATH` on a local disk.** Network filesystems (NFS, SMB) implement
  file locking unreliably and are the most common source of persistent lock errors.
- **Run a single KWP instance per database file.** Two instances sharing one file
  double the write pressure and serialise against each other. Scale by giving each
  instance its own database, not by adding replicas over a shared volume.
- **Avoid clearing very large queues during traffic peaks.**
  `POST /api/webhook/{channel}/queue/clear` is the longest write the service
  performs.

## What's next

- [Integration guides](INTEGRATIONS.md)
