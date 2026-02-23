# Project Security

This document provides an overview of the security architecture.

## Architecture

- All incoming webhooks may provide secret header. It depends on your configuration.
- REST API:
  - Requires authentication
  - Request size is limited to 256 MB
- Project uses [sqlite](https://sqlite.org/) to store data in file `kwp.db`. You can switch it to in memory mode by setting `DATABASE_URL` environment variable to `sqlite::memory:`.

## Container Image

- We use Alpine Linux as the base image, which is a lightweight and secure Linux distribution. We also use Docker Compose to manage our services, which provides a secure and isolated environment for our application.
- Rootless container image with uid/gid recommended for Kubernetes.
- The latest trivy scan report is [here](trivy-scan-report.txt).
