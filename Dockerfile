FROM docker.io/library/rust:1.99.1-bookworm@sha256:e6f2cff35d0f309acf8433b4f05878ed77092602c5065cc81801177da0bb3a9c AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY src ./src
RUN cargo build --locked --release && mkdir /app/runtime-data

FROM gcr.io/distroless/cc-debian13:nonroot@sha256:cc66d222f0a7ec996b78a99fe76a969bc80b539a5016ad866c14d1949bc6d0ab AS runtime-base

FROM builder AS runtime-rootfs
COPY --from=runtime-base / /runtime-rootfs/
COPY scripts/prune_runtime.py /usr/local/bin/prune_runtime.py
# Keep the release ELF dependency closure and reviewed built-in DNS/files NSS.
RUN python3 /usr/local/bin/prune_runtime.py --root /runtime-rootfs \
    --executable /app/target/release/bitcoin_price_tracker --report /runtime-library-audit.json

FROM scratch
COPY --from=runtime-rootfs /runtime-rootfs/ /
WORKDIR /app
COPY --from=builder --chown=10001:10001 /app/runtime-data /app/data
COPY --from=builder /app/target/release/bitcoin_price_tracker /usr/local/bin/bitcoin_price_tracker
USER 10001:10001
EXPOSE 3000
ENV PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt \
    RUST_LOG=info DATABASE_PATH=/app/data/bitcoin_prices.db BIND_ADDRESS=0.0.0.0:3000 LANG=C LC_ALL=C
HEALTHCHECK --interval=30s --timeout=6s --start-period=5s --retries=3 CMD ["bitcoin_price_tracker", "--healthcheck"]
STOPSIGNAL SIGTERM
CMD ["bitcoin_price_tracker"]
