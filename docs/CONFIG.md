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

## YAML Configuration (`config.yml`)

The YAML file defines the channels and their security settings.

### Channels

The `channels` key contains a list of channel configurations.

| Field | Description |
| :--- | :--- |
| `name` | A unique name for the channel. |
| `token` | The token used to authenticate requests for reading webhooks from this channel. |
| `webhook-secret` | (Optional) The secret key used to verify the authenticity of incoming webhooks. |
| `secret-header` | (Optional) The HTTP header name that contains the `webhook-secret`. |

#### Example

```yaml
channels:
  - name: telegram
    token: your_read_token_here
    webhook-secret: your_webhook_secret_here
    secret-header: X-Telegram-Bot-Api-Secret-Token

  - name: open
    token: your_open_read_token_here
    # no webhook-secret — accepts webhooks without verification
```
