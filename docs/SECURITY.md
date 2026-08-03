# Project Security

This document provides an overview of the security architecture.

## Architecture

- All incoming webhooks may provide secret header. It depends on your configuration.
- REST API:
  - Requires authentication
  - Request size is limited to 256 KB
- Project uses [sqlite](https://sqlite.org/) to store data in file `kwp.db`. You can switch it to in memory mode by setting `DATABASE_URL` environment variable to `sqlite::memory:`.

## Error Reporting

Sentry reporting is off unless `SENTRY_DSN` is set. When it is on, events leave the
process and carry:

- the client IP address (`user.ip_address`) and the request URL, method and headers,
- the log message of the error, the release version and the host name.

Webhook payloads are never sent. Header values are redacted when the header name looks
sensitive (`auth`, `cookie`, `key`, `password`, `secret`, `sign`, `token`) and for every
`secret-header` / `sign-header` configured in `config.yml`, since those carry the channel
secret verbatim. Rejected requests (401/403/404) are not reported as events, only as
breadcrumbs. Details: [MONITORING.md](MONITORING.md#error-reporting-sentry).

## Container Image

- We use Alpine Linux as the base image, which is a lightweight and secure Linux distribution. We also use Docker Compose to manage our services, which provides a secure and isolated environment for our application.
- Rootless container image with uid/gid recommended for Kubernetes.
- The latest trivy scan report is [here](trivy-scan-report.txt).

## Recommendations

- Use TLS
- Use [secrets for webhooks](CONFIG.md)
