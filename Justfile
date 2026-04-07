set dotenv-load := true

version := `cat Cargo.toml | grep version | head -1 | cut -d " " -f 3 | tr -d "\""`
image := 'tinyops/kwp'
trivyReportFile := "docs/trivy-scan-report.txt"
chartName := `cat helm-chart/Chart.yaml | yq -r '.name'`
chartVersion := `cat helm-chart/Chart.yaml | yq -r '.version'`

cleanup:
    rm -f {{ chartName }}-*.tgz

format:
    cargo fmt

lint-backend:
    cargo clippy

lint-frontend:
    cd frontend && yarn lint

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

run-backend:
    cargo run

run-frontend:
    cd frontend && yarn && npm run dev -- --port=4200

# FRONTEND

frontend-install:
    cd frontend && npm install

frontend-build:
    jq --arg v "{{ version }}" '.version = $v' frontend/package.json > frontend/package.json.tmp && \
        mv frontend/package.json.tmp frontend/package.json
    cd frontend && npm run build

build-release: frontend-build lint
    cargo build --release

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

build-release-image: test
    docker build --progress=plain --platform=linux/amd64 -t {{ image }}:{{ version }} .

release: build-release-image && trivy-save-reports
    docker push {{ image }}:{{ version }}
