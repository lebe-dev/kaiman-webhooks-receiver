# Kaiman Webhooks Proxy

Receive webhooks from external services and forward them to your target service with retries. Or poll them via REST API.

Suitable for Enterprise environments.

## Getting Started

```bash
mkdir /opt/kwp

cp .env-example .env

# edit .env for your needs

# then start
docker compose up -d
```

## Documentation

- [Installation](docs/INSTALL.md)
- [Configuration](docs/CONFIG.md)
- [API](docs/API.md)
- [Monitoring](docs/MONITORING.md)
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

In Enterprise environments, there are specific challenges:

1. **Attack surface reduction**: Exposing multiple API endpoints across microservices to receive webhooks creates a wide attack vector. Each service may use a different web framework with its own set of dependencies. More diversity means a larger attack surface.

2. **Security constraints**: Enterprise networks cannot simply use Cloudflare Tunnel, ngrok, or similar solutions that bypass firewalls and network security policies.

Webhooks Proxy provides a single entry point for webhooks following security best practices. The project is built on [axum](https://github.com/tokio-rs/axum), which has had no reported vulnerabilities since 2022 (see [CVE Details](https://www.cvedetails.com/vendor/28264/)).

## Roadmap

- Support self-signed certificates for target services
- Config: turn on/off API
- Exponential backoff
