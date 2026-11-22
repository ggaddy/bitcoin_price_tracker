# Operations and configuration

This guide covers running `bitcoin_price_tracker` in containers, tuning runtime
limits, configuring reverse-proxy caching, and troubleshooting common issues.
For API wire formats and schema migration semantics, see
[docs/source-contract.md](source-contract.md). For internal state machines, see
[docs/state-machines.md](state-machines.md). For measured origin throughput,
see [docs/load-testing.md](load-testing.md).

## Container deployment

The [Dockerfile](../Dockerfile) builds a release binary and assembles a
`FROM scratch` runtime image from a pruned, digest-pinned Debian 13 distroless
`cc` filesystem ([scripts/prune_runtime.py](../scripts/prune_runtime.py)). The
final image runs as UID/GID **10001:10001** with no shell, package manager, or
build tools. It retains only the executable's four-library native dependency
chain (`libc`, `libm`, `libgcc_s`, dynamic loader), removing unused OpenSSL,
compression, C++/OpenMP, and libc modules. TLS uses Rustls with
`SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt`, libc supplies built-in
files/DNS resolution, and `LC_ALL=C` is required.

### Named volume (recommended)

A named volume is initialized with writable ownership for `10001:10001`:

```bash
docker build -t btc-matrix .
docker volume create btc-data
docker run -d --name btc-tracker -p 3000:3000 -v btc-data:/app/data \
  --read-only --cap-drop ALL --security-opt no-new-privileges btc-matrix
docker exec btc-tracker bitcoin_price_tracker --healthcheck
```

Open `http://localhost:3000`. Stop gracefully with enough grace to cover the
default 25-second HTTP/refresh drain plus 5-second SQLite cleanup:

```bash
docker stop --time 35 btc-tracker
```

### Bind mount

For an existing host directory, stop any running instance and back up its
database before upgrading. UID/GID `10001:10001` must own both the directory
and database file so SQLite can create journal files. Prepare ownership inside
the container engine's user namespace (also suitable for rootless Podman):

```bash
mkdir -p data
docker run --rm --network none --user 0:0 -v "$PWD/data:/app/data" \
  python:3.15.0-slim-bookworm@sha256:0e019bbdfb5d9874cb2389f1c2884b64aa2ac97c99960b5bc521cef978c0617d \
  chown -R 10001:10001 /app/data
docker run -d --name btc-tracker -p 3000:3000 -v "$PWD/data:/app/data" \
  --read-only --cap-drop ALL --security-opt no-new-privileges btc-matrix
```

### Podman and SELinux notes

- Build with `podman build --format docker -t btc-matrix .` to preserve OCI
  `HEALTHCHECK` metadata, then substitute `podman` for `docker` in run commands.
- On SELinux hosts, append an appropriate volume label (such as `:Z`) to bind
  mounts.

### Single-instance and upgrade rules

Run **one application instance per SQLite database** behind your reverse proxy.
Viewer presence, request budgets, refresh ownership, and provider backoff are
all process-local.

Startup atomically migrates unversioned databases (`PRAGMA user_version = 0`)
to schema version `1` and refuses unsupported versions. Always back up the
database before upgrading; see [schema migration](source-contract.md#schema-migration).

Kubernetes manifests and rollout evidence belong in the separate
`ggaddy/kube-lan-rke2` repository.

## Health check and graceful shutdown

- **Health probe:** `GET /health` verifies local SQLite readability without
  activating viewers, calling providers, or creating a missing database. It
  returns HTTP `200` `{"status":"ok"}` (even when the database has no quotes
  yet) or `503` `{"status":"unavailable"}` on read failure.
- **Container healthcheck:** The image runs `bitcoin_price_tracker --healthcheck`
  every 30s (6s timeout, 5s start period, 3 retries), probing `/health` over
  loopback with no `curl` or shell dependency.
- **Shutdown:** On `SIGINT`/`SIGTERM`, the server stops accepting new
  connections and drains in-flight HTTP requests and any dispatched background
  price refresh within `SHUTDOWN_SECONDS`, followed by up to 5 seconds for
  blocking SQLite cleanup. Exceeding the drain deadline logs an error and exits
  non-zero. Configure your orchestrator's termination grace period longer than
  `SHUTDOWN_SECONDS + 5` (35 seconds for defaults).

## Configuration and limits

All runtime settings are parsed in [src/config.rs](../src/config.rs) and
[src/main.rs](../src/main.rs). Invalid numeric or bind values fail startup with
the offending variable name.

| Environment variable | Default | Accepted values |
| --- | --- | --- |
| `DATABASE_PATH` | `data/bitcoin_prices.db` locally; `/app/data/bitcoin_prices.db` in image | Writable SQLite file path |
| `BIND_ADDRESS` | `0.0.0.0:3000` | IP address and port `1`–`65535` (IPv6 example: `[::1]:3000`) |
| `MAX_VIEWERS` | `1000` | `1`–`100000` |
| `REQUESTS_PER_SECOND` | `250` | `1`–`10000` per request group |
| `REQUEST_CONCURRENCY` | `64` | `1`–`1024` per request group |
| `MAX_CONNECTIONS` | `128` | `1`–`4096` remote TCP connections (plus `min(MAX_CONNECTIONS, 4)` loopback slots) |
| `SHUTDOWN_SECONDS` | `25` | `1`–`120` |
| `RUST_LOG` | `bitcoin_price_tracker=info` locally; `info` in image | `tracing-subscriber` env filter |

### Request budgets and connection bounds

- **Viewer admission:** Expired viewers (TTL 15s) are pruned before admission.
  At `MAX_VIEWERS`, existing heartbeats and removals still succeed; new viewers
  receive HTTP `429` with `Retry-After: 15`.
- **Route groups ([src/limits.rs](../src/limits.rs)):** `/api/price`,
  `/api/presence`, `/api/news`, `/health`, and UI/assets (`/`, `/assets/*`) each
  have independent token buckets (burst up to 1 second's allowance) and
  concurrency semaphores:
  - `/api/price`, `/api/presence`, assets: `REQUESTS_PER_SECOND`, `REQUEST_CONCURRENCY`
  - `/api/news`: `REQUESTS_PER_SECOND`, concurrency capped at `min(REQUEST_CONCURRENCY, 8)`
  - `/health`: fixed `10` req/s and `4` concurrent handlers
  - Rate exhaustion returns `429` with `Retry-After: 1`; concurrency exhaustion
    returns `503` with `Retry-After: 1`; per-request deadline is `20s` (`408`).
- **HTTP/1 connection limits ([src/lifecycle.rs](../src/lifecycle.rs), [src/handlers.rs](../src/handlers.rs)):**
  HTTP/1 only, one request per connection (`keep_alive(false)`), 5s header read
  timeout, max 64 headers, 16 KiB read buffer, 25s total connection lifetime,
  and a 1,024-byte HTTP request body limit (`DefaultBodyLimit`). Loopback health
  probes use a separate connection pool so remote load cannot starve them.
- **Reverse-proxy responsibility:** No limiter reads forwarded IP headers or
  maintains per-client maps. Enforce TLS and per-client abuse/fairness controls
  at the edge proxy.
- **Capacity context:** At 1,000 tabs, 5-second polling nominally generates ~200
  req/s each on `/api/price` and `/api/presence` (25% headroom under the 250/s
  default). Arithmetic alone is not a throughput guarantee; see
  [docs/load-testing.md](load-testing.md) for measured staggered vs. burst limits.

## Asset caching and reverse proxies

- `/` (`index.html`), `/assets/dashboard.css`, and `/assets/dashboard.js` return
  `Cache-Control: no-cache` so deployments never mix UI versions.
- Embedded CSS and JS include content-derived weak ETags (`W/"..."`); matching
  `If-None-Match` requests return `304 Not Modified`.
- `/api/*` and `/health` return `Cache-Control: no-store`.
- Configure your reverse proxy/CDN to honor origin `Cache-Control` and
  revalidation headers. Immutable content-hashed asset URLs remain deferred in
  Beads issue `bitcoin_price_tracker-f14`; current query-version URLs must
  continue to revalidate.

## Logging and clock synchronization

- `info` logs report startup/shutdown and provider fetch outcomes/durations;
  `debug` logs add refresh, coverage, and overload decisions. Raw viewer session
  IDs are never logged.
- Observation timestamps use the host clock; keep node time synchronization
  working. The container itself needs no NTP egress or clock-setting capability.

## Troubleshooting

- **SQLite permission errors:** Stop the container, then verify UID/GID
  `10001:10001` owns both the mounted data directory and `bitcoin_prices.db`
  (plus WAL/journal files).
- **Unhealthy container:** Inspect container logs, run
  `bitcoin_price_tracker --healthcheck` inside the container, and check database
  readability and `BIND_ADDRESS`.
- **HTTP `429` or busy `503`:** Check viewer/request budgets and proxy traffic.
  These limits do not bypass upstream provider cooldowns; normal dashboard
  retries handle them.
- **No price updates (`STALE` or `UNAVAILABLE`):** At least one visible
  dashboard tab must send `POST /api/presence` heartbeats. Check `provider_health`
  in `GET /api/price` and outbound HTTPS connectivity; `STALE` prices can
  coexist with a `200` `/health` response.
- **Unsupported schema version:** Startup refuses newer database schemas instead
  of rewriting them; run a matching application version or restore a backup.
