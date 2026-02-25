set dotenv-load := true

version := `cat Cargo.toml | grep version | head -1 | cut -d " " -f 3 | tr -d "\""`
image := 'tinyops/kwp'
trivyReportFile := "docs/trivy-scan-report.txt"
chartVersion := `cat helm-chart/Chart.yaml | yq -r '.version'`

format:
    cargo fmt

lint: format
    cargo clippy

build: lint
    cargo build

test-image-build:
    docker build --progress=plain --platform=linux/amd64 .

run:
    cargo run --bin kwp

test:
    cargo test --lib
    cargo test --bin kwp

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
        cp ../pw-{{ chartVersion }}.tgz helm-charts/ && \
        helm repo index helm-charts/ && \
        if [ -z "$(git status --porcelain)" ]; then \
            echo "Chart pw-{{ chartVersion }} already published, skipping." && \
            exit 0; \
        fi && \
        git add helm-charts/ && \
        git commit -m "Add helm chart: pw-{{ chartVersion }}" && \
        git push'
    rm -rf helm-repo

build-release-image: test
    docker build --progress=plain --platform=linux/amd64 -t {{ image }}:{{ version }} .

release: build-release-image
    docker push {{ image }}:{{ version }}
