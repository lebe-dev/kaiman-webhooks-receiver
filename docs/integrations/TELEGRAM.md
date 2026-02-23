# Telegram Bot API Integration

This guide explains how to receive Telegram Bot API webhooks via Kaiman Webhooks Proxy.

## Overview

Telegram delivers bot updates by pushing HTTP POST requests to a URL you register via `setWebhook`. Because your bot may run behind NAT or on a machine without a public HTTPS endpoint, the proxy acts as the public-facing receiver while your bot polls the buffered updates at its own pace.

## Prerequisites

- A running Kaiman Webhooks Proxy instance reachable over HTTPS on one of Telegram's supported ports: **443, 80, 88, or 8443**
- A Telegram bot token obtained from [@BotFather](https://t.me/botfather)
- A valid TLS certificate (CA-signed or self-signed) — plain HTTP is not supported

## Step 1 — Configure a channel

Add a `telegram` channel to your `config.yml`:

```yaml
channels:
  - name: telegram
    token: <your-poll-token>                          # Bearer token your bot uses to fetch updates
    webhook-secret: <your-webhook-secret>             # Secret sent by Telegram in every request
    secret-header: X-Telegram-Bot-Api-Secret-Token   # Header Telegram uses to carry the secret
```

**Field reference:**

| Field | Description |
|---|---|
| `token` | Arbitrary secret your bot uses as a Bearer token to read buffered updates from the proxy |
| `webhook-secret` | A string of 1–256 characters (`A-Z`, `a-z`, `0-9`, `-`, `_`). You choose this value and pass it to `setWebhook`. Telegram includes it in every request so the proxy can verify origin. |
| `secret-header` | Must be exactly `X-Telegram-Bot-Api-Secret-Token` — this is the header name Telegram uses |

Restart the proxy after editing the config.

## Step 2 — Register the webhook URL with Telegram

Call the `setWebhook` method using your bot token. Replace the placeholders:

```bash
curl "https://api.telegram.org/bot<BOT_TOKEN>/setWebhook" \
  -d "url=https://webhooks.example.com/api/webhook/telegram" \
  -d "secret_token=<your-webhook-secret>"
```

A successful response looks like:

```json
{"ok": true, "result": true, "description": "Webhook was set"}
```

> **Port note:** Telegram only accepts webhook URLs on ports 443, 80, 88, or 8443. Make sure your proxy is reachable on one of these ports.

> **IP note:** If you restrict inbound traffic by IP, allow Telegram's subnets: `149.154.160.0/20` and `91.108.4.0/22`. IPv6 is not supported for Telegram webhooks.

## Step 3 — Verify the webhook is set

```bash
curl "https://api.telegram.org/bot<BOT_TOKEN>/getWebhookInfo"
```

The response confirms the URL, pending update count, and last error (if any).

## Step 4 — Poll updates from your bot

Your bot fetches and deletes buffered updates with a single GET request:

```bash
curl -H "Authorization: Bearer <your-poll-token>" \
  https://webhooks.example.com/api/webhook/telegram
```

The response is a JSON array of raw Telegram `Update` objects. Each call removes them from the buffer (pop semantics), so every update is delivered exactly once.

### Example payload

```json
[
  {
    "update_id": 123456789,
    "message": {
      "message_id": 42,
      "from": {
        "id": 987654321,
        "is_bot": false,
        "first_name": "Alice",
        "username": "alice"
      },
      "chat": {
        "id": 987654321,
        "first_name": "Alice",
        "type": "private"
      },
      "date": 1740000000,
      "text": "/start"
    }
  }
]
```

## TLS certificate requirements

Telegram enforces strict TLS requirements for webhook endpoints:

- TLS 1.2 or higher is required; TLS 1.0/1.1 and SSL are not accepted
- The certificate CN or SAN must match the domain in your webhook URL
- CA-signed certificates must chain to a root that Telegram trusts
- Self-signed certificates are supported — pass the certificate file to `setWebhook` via the `certificate` parameter:
  ```bash
  curl "https://api.telegram.org/bot<BOT_TOKEN>/setWebhook" \
    -F "url=https://webhooks.example.com/api/webhook/telegram" \
    -F "secret_token=<your-webhook-secret>" \
    -F "certificate=@/path/to/your/cert.pem"
  ```
- If your CA-signed certificate uses an intermediate CA not in Telegram's trust list, supply the root certificate the same way as a self-signed certificate

## Removing the webhook

To stop receiving push updates and switch back to `getUpdates`:

```bash
curl "https://api.telegram.org/bot<BOT_TOKEN>/deleteWebhook"
```

## Reliability notes

- The proxy responds with HTTP 200 to every valid (verified) incoming webhook, satisfying Telegram's delivery confirmation requirement.
- Telegram retries failed deliveries with increasing delays. Your proxy endpoint must be reachable and return 200; otherwise Telegram will keep retrying and eventually disable the webhook.
- Updates are stored durably in SQLite, so your bot can poll on its own schedule without missing messages.
- Telegram does not send updates for messages older than 24 hours if the webhook was unavailable during that window.
