FROM rust:1.93.1-alpine3.23 AS app-build

WORKDIR /build

RUN apk add musl-dev elfutils xz wget pkgconfig libressl-dev perl make upx mold

COPY . /build

RUN cargo build --bin kwp --release && \
    eu-elfcompress target/release/kwp && \
    strip target/release/kwp && \
    upx -9 --lzma target/release/kwp && \
    chmod +x target/release/kwp

FROM alpine:3.23

WORKDIR /app

RUN apk add libressl-dev && \
    addgroup -g 10001 -S app && \
    adduser -u 10001 -D -S -G app -h /app app && \
    mkdir /app/data && \
    chmod 700 /app && \
    chown -R app:app /app

COPY --from=app-build /build/target/release/kwp /app/kwp

RUN chown -R app:app /app && chmod +x /app/kwp

USER app

CMD ["/app/kwp"]
