FROM docker.io/library/rust:1.99.1-bookworm@sha256:e12566d5abc17a0da741628782cdb504b68b8961f5bfcf677936bf3e8b32de29 AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY src ./src
RUN cargo build --locked --release && mkdir /app/runtime-data

FROM gcr.io/distroless/cc-debian13:nonroot@sha256:e792ab3d241a468a4fd7519ddbbebe66b49b5f365771716ea688ad40b6c6f1c2 AS runtime-base

FROM builder AS runtime-rootfs
COPY --from=runtime-base / /runtime-rootfs/
# TLS uses Rustls; unused OpenSSL is the base's only consumer of zlib.
# Remove both packages' actual files, not just scanner metadata.
# A changed base package must be reviewed before this removal recipe is reused.
RUN set -eu; \
    cd /runtime-rootfs; \
    grep -Fx 'Version: 1:1.3.dfsg+really1.3.1-1+b1' var/lib/dpkg/status.d/zlib1g; \
    grep -Fx 'Version: 3.5.7-1~deb13u3' var/lib/dpkg/status.d/libssl3t64; \
    for package in libssl3t64 zlib1g; do \
        md5sum --check --strict "var/lib/dpkg/status.d/$package.md5sums"; \
        while read -r checksum path; do \
            test -n "$path" || continue; \
            rm -f -- "$path"; \
        done < "var/lib/dpkg/status.d/$package.md5sums"; \
        rmdir "usr/share/doc/$package"; \
        rm -f "var/lib/dpkg/status.d/$package" "var/lib/dpkg/status.d/$package.md5sums"; \
    done; \
    find usr/lib -type l -name libz.so.1 -delete; \
    find usr/lib -type f -name '*.so*' -exec readelf --dynamic --wide {} + > /tmp/runtime-needed; \
    if grep -E '\(NEEDED\).*\[lib(z|ssl|crypto)\.so' /tmp/runtime-needed; then \
        echo 'Retained runtime libraries require removed packages; review removal' >&2; exit 1; \
    fi; \
    test -z "$(find usr/lib \( -name 'libz.so*' -o -name 'libssl.so*' -o -name 'libcrypto.so*' \) -print -quit)"

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
