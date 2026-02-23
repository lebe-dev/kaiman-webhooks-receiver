# API Documentation

Kaiman Webhooks Proxy provides two primary endpoints for each channel, both located at the same path but using different HTTP methods.

## Base URL

By default, the server runs on `http://0.0.0.0:8080`.

## Endpoints

### 1. Receive Webhook

Used by external services (GitHub, Telegram, etc.) to send webhooks to KWP.

*   **URL:** `/api/webhook/{channel_name}`
*   **Method:** `POST`
*   **Path Parameters:**
    *   `channel_name`: The name of the channel as defined in `config.yml`.
*   **Body:** Any valid JSON payload.
*   **Headers:**
    *   If `secret-header` is configured for the channel, that header must be present and match the `webhook-secret`.
*   **Authentication:** Verified via the configured `secret-header`.
*   **Response:**
    *   `200 OK`: Webhook received and stored.
    *   `401 Unauthorized`: Secret header missing or incorrect.
    *   `404 Not Found`: Channel name not found in configuration.
    *   `500 Internal Server Error`: Failed to store the webhook.

### 2. Read and Delete Webhooks

Used by your local application or scripts to retrieve pending webhooks for a specific channel.

*   **URL:** `/api/webhook/{channel_name}`
*   **Method:** `GET`
*   **Path Parameters:**
    *   `channel_name`: The name of the channel.
*   **Headers:**
    *   `Authorization: Bearer <token>`: The `token` configured for this channel in `config.yml`.
*   **Authentication:** Bearer Token.
*   **Response:**
    *   `200 OK`: Returns a JSON array of webhooks. **Note:** Webhooks are deleted from the proxy immediately after being successfully returned in this call.
    *   `401 Unauthorized`: Missing or invalid Bearer token.
    *   `403 Forbidden`: Token is valid but belongs to a different channel.
    *   `500 Internal Server Error`: Database error.

#### Response Schema

```json
[
  {
    "headers": {
      "x-github-event": "push",
      "user-agent": "GitHub-Hookshot/f311f42"
    },
    "payload": {
      "ref": "refs/heads/main",
      "before": "0000000000000000000000000000000000000000",
      "after": "6113728f27ae82c7b1a177c8ef03f99b72d0ad35"
    },
    "received_at": 1708684800
  }
]
```

*   `headers`: Map of HTTP headers received with the original webhook (excluding hop-by-hop headers).
*   `payload`: The original JSON body.
*   `received_at`: Unix timestamp (seconds) when the webhook was received.

## Examples

### Send a Webhook (POST)

```bash
curl -X POST http://localhost:8080/api/webhook/github \
  -H "Content-Type: application/json" \
  -H "X-Hub-Signature-256: mysecret" \
  -d '{"action":"opened","pull_request":{"id":1}}'
```

### Read and Delete Webhooks (GET)

```bash
curl -X GET http://localhost:8080/api/webhook/github \
  -H "Authorization: Bearer ghi789"
```

The response will be a JSON array of all pending webhooks for the channel, and they will be deleted immediately after retrieval.

Example response:
```json
[
  {
    "headers": {
      "content-type": "application/json",
      "x-hub-signature-256": "mysecret"
    },
    "payload": {
      "action": "opened",
      "pull_request": {
        "id": 1
      }
    },
    "received_at": 1708684800
  }
]
```
