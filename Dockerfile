FROM docker.io/library/rust:1.85.1-bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY src ./src
RUN cargo build --locked --release

FROM docker.io/library/debian:bookworm-slim
WORKDIR /app
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 tracker \
    && useradd --uid 10001 --gid 10001 --no-create-home --shell /usr/sbin/nologin tracker \
    && mkdir -p /app/data \
    && chown 10001:10001 /app/data
COPY --from=builder /app/target/release/bitcoin_price_tracker /usr/local/bin/bitcoin_price_tracker
USER 10001:10001
EXPOSE 3000
ENV RUST_LOG=info DATABASE_PATH=/app/data/bitcoin_prices.db BIND_ADDRESS=0.0.0.0:3000
HEALTHCHECK --interval=30s --timeout=6s --start-period=5s --retries=3 CMD ["bitcoin_price_tracker", "--healthcheck"]
STOPSIGNAL SIGTERM
CMD ["bitcoin_price_tracker"]
