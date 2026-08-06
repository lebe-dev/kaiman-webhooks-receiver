# Development

```bash
just test

just lint

just build
```

## Coverage

```bash
just coverage
```

Runs the same tests as `just test` under instrumentation (`cargo-llvm-cov`) and
writes `coverage/lcov.info`. One-time setup:

```bash
cargo install cargo-llvm-cov
rustup component add llvm-tools
```

## SonarQube

```bash
just sonar-scan
```

`SONAR_HOST_URL` and `SONAR_TOKEN` come from `.env` (gitignored, see `.env-dev`).

The recipe first runs `just coverage` and `just clippy-report`, so the scanner
finds `coverage/lcov.info` and `coverage/clippy-report.json` already on disk —
the `sonarsource/sonar-scanner-cli` image has no Rust toolchain and cannot
produce either by itself.

Analysis scope lives in `sonar-project.properties`: only `src`, `frontend/src`,
`helm-chart`, `Dockerfile` and `docker-compose.yml` are scanned; generated
shadcn-svelte components are excluded.
