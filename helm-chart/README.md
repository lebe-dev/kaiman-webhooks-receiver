# Kaiman Webhooks Proxy Helm Chart

Helm chart for [Kaiman Webhooks Proxy](https://github.com/tinyops-ru/kaiman-webhooks-proxy) — a lightweight webhook forwarding service written in Rust.

## Installing the Chart

```bash
helm repo add tinyops https://tinyops.ru/helm-charts/
helm repo update

helm upgrade --install --create-namespace -n kaiman-webhooks-proxy kaiman-webhooks-proxy tinyops/kwp [-f values.yaml]
```

## Configuration

### Environment Variables (`.envs`)

All application env vars are injected via a ConfigMap. Keys are exact env var names, so you can add or override any variable without changing the chart.

| Parameter | Description | Default |
|---|---|---|
| `envs.BIND_ADDRESS` | Address and port the server listens on | `0.0.0.0:8080` |
| `envs.LOG_LEVEL` | Log verbosity (`debug`, `info`, `warn`, `error`) | `info` |
| `envs.LOG_TARGET` | Log output target (`stdout` or a file path) | `stdout` |
| `envs.DATA_PATH` | Directory for the SQLite database file | `/app/data` |
| `envs.DATABASE_URL` | SQLite connection string | `sqlite:///app/data/kwp.db?mode=rwc` |
| `envs.CONFIG_FILE` | Path to the channels config file inside the container | `/app/config.yml` |
| `envs.IGNORED_HEADERS` | Comma-separated list of headers to strip when receiving/forwarding webhooks | `host,content-length,transfer-encoding,connection,content-type` |
| `envs.DEFAULT_BODY_LIMIT` | Max request body size in bytes. Omit to use the app default (262144 = 256 KB) | _(unset)_ |
| `envs.TRUSTED_PROXIES` | Comma-separated list of trusted reverse proxy IPs/CIDRs. When set, `X-Forwarded-For`/`X-Real-IP` headers are used for client IP resolution. Leave unset to always use the direct connection IP (safe default) | _(unset)_ |
| `envs.SENTRY_ENVIRONMENT` | Environment tag on Sentry events. Only used when `secrets.envs.SENTRY_DSN` is set | _(unset)_ |

### Channels Config (`.config`)

Raw YAML content written to `/app/config.yml` inside the container. Stored in a Kubernetes Secret. See [config.yml-dist](../config.yml-dist) for the full format with examples (Telegram, GitHub, open channels).

```yaml
config: |
  channels:
    - name: telegram
      api-read-token: "your_read_token_here"
      webhook-secret: "your_webhook_secret_here"
      secret-header: "X-Telegram-Bot-Api-Secret-Token"
```

The pod restarts automatically when this value changes (checksum annotation on the pod template).

### Secrets (`.secrets`)

Secret values are stored alongside `config.yml` in the same Kubernetes Secret. Annotations on that Secret (e.g. for external-secrets / Vault Agent) live here too.

| Parameter | Description | Default |
|---|---|---|
| `secrets.annotations` | Annotations applied to the channels Secret | `{}` |
| `secrets.envs` | Map of secret env vars injected into the container via `envFrom.secretRef`. Keys are exact env var names | `{}` |
| `secrets.envs.UI_ACCESS_TOKEN` | Token required to access the embedded web UI | _(unset)_ |
| `secrets.envs.SENTRY_DSN` | Sentry project DSN. Leave unset to disable error reporting — see [MONITORING.md](../docs/MONITORING.md#error-reporting-sentry) | _(unset)_ |

```yaml
secrets:
  annotations:
    vault.security.banzaicloud.io/vault-addr: https://vault.company.com
  envs:
    UI_ACCESS_TOKEN: "change-me"
```

### Persistence (`.persistence`)

SQLite data is stored under `DATA_PATH`. A PersistentVolumeClaim is created automatically.

| Parameter | Description | Default |
|---|---|---|
| `persistence.enabled` | Create a PVC for the data directory | `true` |
| `persistence.size` | PVC storage request | `100Mi` |
| `persistence.storageClassName` | StorageClass name. Leave empty for the cluster default | `""` |
| `persistence.accessMode` | PVC access mode | `ReadWriteOnce` |

> **Note:** `ReadWriteOnce` does not support `replicaCount > 1` with most storage classes.

### Resources (`.resources`)

Sized for the actual workload: one Rust binary forwarding webhooks through SQLite.

| Parameter | Description | Default |
|---|---|---|
| `resources.requests.cpu` | CPU reserved by the scheduler | `50m` |
| `resources.requests.memory` | Memory reserved by the scheduler | `64Mi` |
| `resources.requests.ephemeral-storage` | Local storage reserved by the scheduler | `128Mi` |
| `resources.limits.memory` | Memory ceiling; the container is OOM-killed above it | `128Mi` |
| `resources.limits.ephemeral-storage` | Local storage ceiling; the pod is evicted above it | `512Mi` |

There is deliberately no CPU limit: a forwarding burst should be allowed to use idle CPU rather than be throttled. Raise `limits.ephemeral-storage` if `LOG_TARGET` points at a file instead of `stdout`.

### Service account (`.serviceAccount`)

| Parameter | Description | Default |
|---|---|---|
| `serviceAccount.create` | Create a ServiceAccount for the release | `true` |
| `serviceAccount.automount` | Mount the ServiceAccount token into the pod | `false` |
| `serviceAccount.annotations` | Annotations applied to the ServiceAccount | `{}` |
| `serviceAccount.name` | Use an existing ServiceAccount instead of a generated name | `""` |

KWP never talks to the Kubernetes API, so the token is not mounted by default — it would only be a credential an attacker could reuse. Set `serviceAccount.automount: true` if a sidecar in the pod needs it.
