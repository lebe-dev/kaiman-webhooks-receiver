FROM node:25-alpine AS frontend-build

WORKDIR /build/frontend

RUN apk add --no-cache jq

COPY frontend/package.json frontend/yarn.lock ./
RUN yarn

COPY Cargo.toml ./Cargo.toml
COPY frontend/ ./

RUN VERSION=$(grep '^version' Cargo.toml | head -1 | cut -d'"' -f2) && \
    jq --arg v "$VERSION" '.version = $v' package.json > package.json.tmp && \
    mv package.json.tmp package.json && \
    rm Cargo.toml

RUN yarn build

FROM rust:1.97.1-alpine AS app-build

WORKDIR /build

RUN apk --no-cache add musl-dev elfutils xz wget upx mold

COPY Cargo.toml Cargo.lock /build/
COPY .cargo /build/.cargo
COPY src /build/src
COPY --from=frontend-build /build/static /build/static

RUN cargo build --bin kwp --release && \
    eu-elfcompress target/release/kwp && \
    strip target/release/kwp && \
    upx -9 --lzma target/release/kwp && \
    chmod +x target/release/kwp

FROM alpine:3.24

WORKDIR /app

RUN apk --no-cache add ca-certificates && \
    addgroup -g 10001 -S app && \
    adduser -u 10001 -D -S -G app -h /app app && \
    mkdir /app/data && \
    chmod 700 /app && \
    chown -R app:app /app

COPY --from=app-build /build/target/release/kwp /app/kwp

RUN chown -R app:app /app && chmod +x /app/kwp

USER app

CMD ["/app/kwp"]
