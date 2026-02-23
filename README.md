# Kaiman Webhooks Proxy

Receive webhooks from external services and forward them to your target service with retries. Or poll them via REST API.

Self-hosted alternative for [Cloudflare Tunnel](https://developers.cloudflare.com/cloudflare-one/connections/connect-apps/), [ngrok](https://ngrok.com/), etc.

## Getting Started

```bash
mkdir /opt/webhooks-proxy

cp .env-example .env

# edit .env for your needs

# then start
docker compose up -d
```

## Documentation

- [Installation](docs/INSTALL.md)
- [Configuration](docs/CONFIG.md)
- [API](docs/API.md)
- [Security](docs/SECURITY.md)
- [FAQ](docs/FAQ.md)

## Features

- Unlimited webhook sources with [strong security](docs/SECURITY.md)
- Forwarding with retries: receive and store webhooks, then forward them to your target service with automatic retries
- Poll mode: fetch webhook payloads later via [REST API](docs/API.md)
- Blazing fast and lightweight 🦀 (Rust)
  ```bash
  CONTAINER ID   NAME                 CPU %     MEM USAGE / LIMIT
  3c3508ed449b   kwp                  0.00%     5.145MiB / 1.922GiB
  ```

## Motivation

I'm working on a project that receives webhooks from various sources (Todoist, Telegram, etc.). I used to deploy it on a VPS, but the development cycle was too long:

1. Build container image (Rust is notorious for its long build times)
2. Pull container image on VPS
3. Restart container

Webhooks Proxy collects all incoming webhooks and stores them in a database. My app polls them on its own schedule. After fetching the webhook data, it deletes them from the database.

I prefer self-hosted solutions over cloud services.

## Roadmap

- Config: turn on/off API
- Config: ignore-headers
- Exponential backoff
