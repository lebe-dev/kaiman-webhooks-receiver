# Kaiman Webhooks Proxy

Receive webhooks from external services and forward them to your target service with retries. Or poll them via REST API.

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

- **Forwarding with retries**: receive and store webhooks, then forward them to your target service with automatic retries
- **Poll mode**: fetch webhooks later via REST API
- Blazing fast and lightweight

## Motivation

I'm working on a project that receives webhooks from various sources (Todoist, Telegram, etc.). I used to deploy it on a VPS, but the development cycle was too long:

1. Build container image (Rust is notorious for its long build times)
2. Pull container image on VPS
3. Restart container

Webhooks Proxy collects all incoming webhooks and stores them in a database. My app polls them on its own schedule. After fetching the webhook data, it deletes them from the database.

## Roadmap

- Config: turn on/off API
- Config: ignore-headers
- Exponential backoff
