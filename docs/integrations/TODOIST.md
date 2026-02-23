# Todoist Integration

This guide explains how to receive Todoist webhooks via Kaiman Webhooks Proxy.

## Overview

Todoist sends real-time HTTP POST notifications for task, project, section, label, filter, comment, and reminder events. Because Todoist requires a public HTTPS endpoint (no custom ports), the proxy acts as the public-facing receiver while your app polls the buffered payloads at its own pace.

## Prerequisites

- A running Kaiman Webhooks Proxy instance accessible over HTTPS (e.g. `https://webhooks.example.com`)
- A Todoist app created in the [App Management Console](https://app.todoist.com/app/settings/integrations/app-management)

## Step 1 — Configure a channel

Add a `todoist` channel to your `config.yml`:

```yaml
channels:
  - name: todoist
    token: <your-poll-token>          # Bearer token used by your app to fetch events
    webhook-secret: <client_secret>   # Your Todoist app's client_secret
    secret-header: X-Todoist-Hmac-SHA256
```

> **Note:** The `webhook-secret` value must be your Todoist app's **client_secret**. Todoist signs every request with an HMAC-SHA256 of the raw payload using this secret and sends the result (base64-encoded) in the `X-Todoist-Hmac-SHA256` header. The proxy verifies this header before storing the event.

Restart the proxy after editing the config.

## Step 2 — Register the webhook URL in Todoist

1. Open [App Management Console → your app → Webhooks](https://app.todoist.com/app/settings/integrations/app-management).
2. Set the **Callback URL** to:
   ```
   https://webhooks.example.com/api/webhook/todoist
   ```
3. Select the events you want to receive (e.g. `item:added`, `item:completed`).
4. Save.

Todoist only accepts HTTPS URLs with no explicit port — make sure your proxy is reachable that way.

## Step 3 — Activate webhooks for personal use

Todoist does **not** fire webhooks for the account that created the app by default. To enable them for your own account, complete the OAuth flow manually:

1. Open this URL in a browser (replace `<client_id>` and `<redirect_uri>`):
   ```
   https://todoist.com/oauth/authorize?client_id=<client_id>&scope=data:read&state=random&redirect_uri=<redirect_uri>
   ```
2. Approve the permissions. Your browser will redirect and include a `code` query parameter — copy it from the address bar.

3. Exchange the code for an access token (use curl or Postman — it must be a POST, not a browser request):
   ```bash
   curl -X POST https://todoist.com/oauth/access_token \
     -d "client_id=<client_id>" \
     -d "client_secret=<client_secret>" \
     -d "code=<code>" \
     -d "redirect_uri=<redirect_uri>"
   ```
   The response contains an `access_token`. Webhooks are now active for your account.

## Step 4 — Poll events from your app

Your application fetches and deletes buffered events with a single GET request:

```bash
curl -H "Authorization: Bearer <your-poll-token>" \
  https://webhooks.example.com/api/webhook/todoist
```

The response is a JSON array of raw Todoist webhook payloads. Each call removes them from the buffer (pop semantics), so every event is delivered exactly once.

### Example payload

```json
{
  "event_name": "item:added",
  "user_id": "2671355",
  "triggered_at": "2025-02-10T10:39:38.000000Z",
  "version": "10",
  "event_data": {
    "id": "6XR4GqQQCW6Gv9h4",
    "content": "Buy Milk",
    "project_id": "6XR4H993xv8H5qCR",
    "priority": 1,
    "checked": false,
    "added_at": "2025-02-10T10:33:38.000000Z"
  },
  "initiator": {
    "id": "2671355",
    "full_name": "Alice",
    "email": "alice@example.com"
  }
}
```

## Supported events

| Event | Description |
|---|---|
| `item:added` | Task created |
| `item:updated` | Task updated |
| `item:deleted` | Task deleted |
| `item:completed` | Task completed |
| `item:uncompleted` | Task uncompleted |
| `note:added` | Comment added |
| `note:updated` | Comment updated |
| `note:deleted` | Comment deleted |
| `project:added` | Project created |
| `project:updated` | Project updated |
| `project:deleted` | Project deleted |
| `project:archived` / `project:unarchived` | Project archived/unarchived |
| `section:added` / `section:updated` / `section:deleted` | Section changes |
| `section:archived` / `section:unarchived` | Section archived/unarchived |
| `label:added` / `label:updated` / `label:deleted` | Label changes |
| `filter:added` / `filter:updated` / `filter:deleted` | Filter changes |
| `reminder:fired` | Reminder triggered |

## Reliability notes

- Todoist retries failed deliveries up to **3 times**, with a 15-minute delay between attempts. Your endpoint (the proxy) must respond with HTTP 200.
- The proxy stores all events durably in SQLite, so your app can poll on its own schedule without missing events.
- Events may arrive delayed or out of order. Do not use webhooks as the sole data source — treat them as notifications and verify state via the Todoist REST API when needed.
- Each delivery has a unique `X-Todoist-Delivery-ID` header. Retried deliveries reuse the same ID, which you can use for deduplication.
