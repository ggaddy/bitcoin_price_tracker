FROM docker.io/library/rust:1.85.1-bookworm@sha256:e51d0265072d2d9d5d320f6a44dde6b9ef13653b035098febd68cce8fa7c0bc4 AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY src ./src
RUN cargo build --locked --release && mkdir /app/runtime-data

FROM gcr.io/distroless/cc-debian13:nonroot@sha256:c31ff9abcb1910f3ab25c7957bdaf0bfe12a01eb546e8df2282f1c8f682b606c
WORKDIR /app
COPY --from=builder --chown=10001:10001 /app/runtime-data /app/data
COPY --from=builder /app/target/release/bitcoin_price_tracker /usr/local/bin/bitcoin_price_tracker
USER 10001:10001
EXPOSE 3000
ENV RUST_LOG=info DATABASE_PATH=/app/data/bitcoin_prices.db BIND_ADDRESS=0.0.0.0:3000
HEALTHCHECK --interval=30s --timeout=6s --start-period=5s --retries=3 CMD ["bitcoin_price_tracker", "--healthcheck"]
STOPSIGNAL SIGTERM
CMD ["bitcoin_price_tracker"]
