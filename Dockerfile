FROM docker.io/library/rust:1.99.0-bookworm@sha256:59037199c44290f2befcdd58dcc540164763fc296950255aaefeef096a1866b0 AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY src ./src
RUN cargo build --locked --release && mkdir /app/runtime-data

FROM gcr.io/distroless/cc-debian13:nonroot@sha256:54df941ed0d06a1bd95ef5e0ce391fd8d9f94b64782dc9a60062727849ee3f97
WORKDIR /app
COPY --from=builder --chown=10001:10001 /app/runtime-data /app/data
COPY --from=builder /app/target/release/bitcoin_price_tracker /usr/local/bin/bitcoin_price_tracker
USER 10001:10001
EXPOSE 3000
ENV RUST_LOG=info DATABASE_PATH=/app/data/bitcoin_prices.db BIND_ADDRESS=0.0.0.0:3000
HEALTHCHECK --interval=30s --timeout=6s --start-period=5s --retries=3 CMD ["bitcoin_price_tracker", "--healthcheck"]
STOPSIGNAL SIGTERM
CMD ["bitcoin_price_tracker"]
