# Load variables from .env (gitignored) into recipe environments — e.g. SONAR_TOKEN.
set dotenv-load

version := `cat Cargo.toml | grep version | head -1 | cut -d " " -f 3 | tr -d "\""`
commitHash := `git rev-parse --short HEAD`
devVersion := version + "-" + commitHash
image := 'tinyops/kwp'
trivyReportFile := "docs/trivy-scan-report.txt"
chartName := `cat helm-chart/Chart.yaml | yq -r '.name'`
chartVersion := `cat helm-chart/Chart.yaml | yq -r '.version'`
coverageDir := "coverage"
lcovReport := coverageDir + "/lcov.info"
clippyReport := coverageDir + "/clippy-report.json"

cleanup:
    rm -f {{ chartName }}-*.tgz
    rm -rf {{ coverageDir }} .scannerwork

bump-frontend-deps:
    cd frontend && rm -rf node_modules yarn.lock && yarn install

bump-backend-deps:
    cargo update

bump-deps: bump-frontend-deps && bump-backend-deps

format:
    cargo fmt

lint-backend:
    cargo clippy

lint-frontend:
    cd frontend && yarn lint

# Type-check svelte components and TS sources
check-frontend:
    cd frontend && yarn typecheck

lint: lint-backend && lint-frontend

build: lint
    cargo build

test-image-build:
    docker build --progress=plain --platform=linux/amd64 .

run:
    cargo run --bin kwp

test:
    cargo test --lib
    cargo test --bin kwp

# Run the tests instrumented and write an LCOV report for SonarQube
coverage:
    #!/usr/bin/env bash
    # Requires `cargo install cargo-llvm-cov` and `rustup component add llvm-tools`.
    set -euo pipefail
    mkdir -p {{ coverageDir }}
    cargo llvm-cov --lib --bin kwp --lcov --output-path {{ lcovReport }}
    # llvm-cov records absolute host paths; the scanner sees the project mounted
    # at /usr/src and would resolve none of them. Make them project-relative.
    sed -i.bak "s|^SF:$PWD/|SF:|" {{ lcovReport }}
    rm -f {{ lcovReport }}.bak
    echo "coverage report: {{ lcovReport }}"

# Clippy findings in SonarQube's expected JSON format
clippy-report:
    #!/usr/bin/env bash
    # The sonar-scanner-cli image has no Rust toolchain, so clippy has to run
    # here rather than inside the scanner (sonar.rust.clippy.enabled=false).
    set -euo pipefail
    mkdir -p {{ coverageDir }}
    cargo clippy --all-targets --message-format=json > {{ clippyReport }}
    echo "clippy report: {{ clippyReport }}"

run-backend:
    cargo run

run-frontend:
    cd frontend && yarn && yarn dev --port=4200

# FRONTEND

frontend-install:
    cd frontend && yarn install

frontend-build:
    jq --arg v "{{ version }}" '.version = $v' frontend/package.json > frontend/package.json.tmp && \
        mv frontend/package.json.tmp frontend/package.json
    cd frontend && yarn build

build-release: frontend-build lint
    cargo build --release

# --- SonarQube (static analysis) ---
sonarHostUrl := env_var_or_default("SONAR_HOST_URL", "http://host.docker.internal:9000")

# Scan the project on SonarQube, with fresh coverage and clippy reports
sonar-scan: coverage clippy-report
    #!/usr/bin/env bash
    # Scope, exclusions and report paths live in sonar-project.properties;
    # only the version is passed here, because it comes from Cargo.toml.
    set -euo pipefail
    if [ -z "${SONAR_TOKEN:-}" ]; then
        echo "error: SONAR_TOKEN is not set." >&2
        echo "  Generate a token at {{ sonarHostUrl }} -> My Account -> Security," >&2
        echo "  then add it to .env:  SONAR_TOKEN=sqp_xxx" >&2
        exit 1
    fi
    docker run --rm \
        --add-host=host.docker.internal:host-gateway \
        -e SONAR_HOST_URL="{{ sonarHostUrl }}" \
        -e SONAR_TOKEN="$SONAR_TOKEN" \
        -v "$PWD:/usr/src" \
        sonarsource/sonar-scanner-cli:latest \
        -Dsonar.projectVersion="{{ version }}"

# HELM CHART

test-chart:
    helm template helm-chart/

lint-chart:
    helm lint helm-chart/

build-chart: test-chart && lint-chart
    helm package helm-chart/ --app-version {{ version }}

# SECURITY

trivy-save-reports:
    trivy -v > {{ trivyReportFile }}
    trivy config Dockerfile >> {{ trivyReportFile }}
    trivy image --severity HIGH,CRITICAL {{ image }}:{{ version }} >> {{ trivyReportFile }}

# DEPLOY

deploy HOSTNAME:
    ssh -t {{ HOSTNAME }} "cd /opt/kwp && KWP_VERSION={{ version }} docker compose pull && KWP_VERSION={{ version }} docker compose down && kwp_VERSION={{ version }} docker compose up -d"

# RELEASE

release-chart: build-chart
    rm -rf helm-repo
    git clone git@github.com:tinyops-ru/tinyops-ru.github.io.git helm-repo
    bash -euo pipefail -c '\
        cd helm-repo && \
        cp ../{{ chartName }}-{{ chartVersion }}.tgz helm-charts/ && \
        helm repo index helm-charts/ && \
        if [ -z "$(git status --porcelain)" ]; then \
            echo "Chart {{ chartName }}-{{ chartVersion }} already published, skipping." && \
            exit 0; \
        fi && \
        git add helm-charts/ && \
        git commit -m "Add helm chart: {{ chartName }}-{{ chartVersion }}" && \
        git push'
    rm -rf helm-repo

build-release-image: test && lint
    docker build --progress=plain --platform=linux/amd64 -t {{ image }}:{{ version }} .

release: build-release-image && trivy-save-reports
    docker push {{ image }}:{{ version }}

build-release-image-dev: test && lint
    docker build --progress=plain --platform=linux/amd64 -t {{ image }}:{{ devVersion }} .

release-dev: build-release-image-dev
    docker push {{ image }}:{{ devVersion }}
