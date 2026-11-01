FROM docker.io/library/rust:1.99.1-bookworm@sha256:e12566d5abc17a0da741628782cdb504b68b8961f5bfcf677936bf3e8b32de29 AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY src ./src
RUN cargo build --locked --release && mkdir /app/runtime-data

FROM gcr.io/distroless/cc-debian13:nonroot@sha256:e792ab3d241a468a4fd7519ddbbebe66b49b5f365771716ea688ad40b6c6f1c2
WORKDIR /app
COPY --from=builder --chown=10001:10001 /app/runtime-data /app/data
COPY --from=builder /app/target/release/bitcoin_price_tracker /usr/local/bin/bitcoin_price_tracker
USER 10001:10001
EXPOSE 3000
ENV RUST_LOG=info DATABASE_PATH=/app/data/bitcoin_prices.db BIND_ADDRESS=0.0.0.0:3000 LANG=C LC_ALL=C
HEALTHCHECK --interval=30s --timeout=6s --start-period=5s --retries=3 CMD ["bitcoin_price_tracker", "--healthcheck"]
STOPSIGNAL SIGTERM
CMD ["bitcoin_price_tracker"]
