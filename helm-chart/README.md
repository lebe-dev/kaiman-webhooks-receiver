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

### Persistence (`.persistence`)

SQLite data is stored under `DATA_PATH`. A PersistentVolumeClaim is created automatically.

| Parameter | Description | Default |
|---|---|---|
| `persistence.enabled` | Create a PVC for the data directory | `true` |
| `persistence.size` | PVC storage request | `100Mi` |
| `persistence.storageClassName` | StorageClass name. Leave empty for the cluster default | `""` |
| `persistence.accessMode` | PVC access mode | `ReadWriteOnce` |

> **Note:** `ReadWriteOnce` does not support `replicaCount > 1` with most storage classes.
