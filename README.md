# Bitcoin Price Tracker

A Rust/Axum BTC/USD tracker with a Cybercore dashboard, four price providers,
local SQLite persistence, and a non-root container image. Dashboard assets are
embedded in the binary; there is no frontend build step or runtime CDN dependency.
[Cybercore CSS](https://github.com/sebyx07/cybercore-css/) 0.3.0 and its MIT license
are vendored in `src/vendor`.

## Quick start

Install Rust with rustup, then run from the repository root:

```bash
cargo run --locked
```

Open `http://localhost:3000` in a visible tab. The repository pins Rust 1.85.1 in
[rust-toolchain.toml](rust-toolchain.toml); rustup selects that toolchain.
Prices are stored at `data/bitcoin_prices.db` by default.

A visible dashboard sends viewer heartbeats and activates price refreshes.
An empty database is healthy, but `/api/price` returns HTTP 503 with
`UNAVAILABLE` until a refresh saves a valid quote. Reads return stored data
without waiting for upstream providers.

See [container setup](#container), [configuration](#configuration-and-limits),
[API examples](#api-examples), [development checks](#development-and-tests), and
[release publishing](#releases).

## Behavior

- CoinGecko aggregate, Coinbase spot, Kraken last-trade, and Gemini bid quotes
  contribute to an **indicative average**, rather than a single exchange price.
- SQLite retains the latest merged snapshot and each provider's latest health.
  Partial refreshes keep other providers' last valid quotes; failed writes roll
  back both quote and health changes. This is not a historical price database.
- Admitted viewer heartbeats drive price refreshes. Viewers expire just after
  15 seconds without a heartbeat. Price reads recheck presence before dispatch;
  already dispatched work may finish after the last viewer leaves.
- One background price refresh runs at a time. The latest snapshot's age and
  time since the previous attempt each enforce a ten-second gate. Initial viewer
  activation requests a full refresh; ordinary refreshes rotate providers,
  advancing after failed attempts too.
- Failed providers back off for 10, 20, 40, 80, 160, then 300 seconds. Longer valid
  `Retry-After` hints extend the delay. Healthy providers continue while others
  cool down. These retry deadlines and viewer presence are process-local.
- Price-provider connections have a two-second deadline; complete requests,
  including bodies, have a five-second deadline. HTTPS is required, redirects
  are disabled, and responses are limited to 64 KiB. Prices must be finite and
  positive.
- Only configured providers' observations aged less than 90 seconds contribute
  to aggregates. Unknown, future, and expired observations remain visible with
  their ages. No contributors means a null average and spread.

The dashboard reports `LIVE`, `DEGRADED`, `STALE`, or `UNAVAILABLE`, shows provider
errors, and updates quote ages between responses. Each tab creates its own
in-memory presence ID without browser storage. Visible tabs run independent
presence and price loops five seconds after each request finishes, with
three-second and eight-second request deadlines respectively. Hiding the page
cancels those loops and sends inactive presence; returning resumes them.
The initial heartbeat requests a cached price with `include_price: true`,
avoiding an extra initial price GET when supported. Transient errors retain
received quotes and retry automatically. Decorative motion pauses while hidden
and respects reduced-motion preferences.

See the [source and price API contract](docs/source-contract.md) for field
semantics, freshness boundaries, migration, and background-refresh behavior.

### News and asset caching

Bitcoin.com RSS headlines are cached in memory for five minutes. One background
refresh runs with a 12-second deadline while readers receive cached headlines.
Cold starts return 503 with `Retry-After: 2`; failed refreshes retain cached
headlines and use a five-minute backoff. News has its own request budget.
Headline scrolling pauses on hover, focus, or the pause control; reduced motion
disables automatic scrolling by default.

HTML and embedded assets use `no-cache` revalidation. CSS and JavaScript have
content-derived weak ETags; matching conditional GET/HEAD requests return an
empty 304. API and health responses use `no-store`. The reverse proxy must
respect these directives. Immutable, content-hashed asset URLs remain deferred
in Beads issue `bitcoin_price_tracker-f14`; current query-version URLs must
continue to revalidate.

## Container

The final image runs as UID/GID **10001:10001** on digest-pinned distroless Debian
13, without a shell, package manager, or Rust build tools. A named volume is
initialized with a writable data directory:

```bash
docker build -t btc-matrix .
docker volume create btc-data
docker run -d --name btc-tracker -p 3000:3000 -v btc-data:/app/data \
  --read-only --cap-drop ALL --security-opt no-new-privileges btc-matrix
docker exec btc-tracker bitcoin_price_tracker --healthcheck
```

Open `http://localhost:3000`. To stop the container gracefully:

```bash
docker stop --time 35 btc-tracker
```

For an existing bind mount, stop the previous instance and back up its database
before migration. The container user must own the directory and database because
SQLite also creates journal files there. Prepare permissions within the engine's
user namespace (also suitable for rootless Podman):

```bash
mkdir -p data
docker run --rm --network none --user 0:0 -v "$PWD/data:/app/data" \
  python:3.12.3-slim-bookworm@sha256:afc139a0a640942491ec481ad8dda10f2c5b753f5c969393b12480155fe15a63 \
  chown -R 10001:10001 /app/data
docker run -d --name btc-tracker -p 3000:3000 -v "$PWD/data:/app/data" \
  --read-only --cap-drop ALL --security-opt no-new-privileges btc-matrix
```

Use either volume example with an unused container name. For Podman, build with
`podman build --format docker -t btc-matrix .` to preserve HEALTHCHECK metadata,
then use `podman` in place of `docker` in the run commands. On SELinux hosts,
use an appropriate volume label such as `:Z`.

Run **one application instance per SQLite database** behind your reverse proxy.
Viewer presence, request budgets, refresh ownership, and retry scheduling are
process-local. Startup atomically migrates unversioned databases to schema
version 1; unsupported versions stop startup. Back up data before upgrades;
see the [migration contract](docs/source-contract.md#schema-migration).

SIGINT/SIGTERM stops accepting new work and drains HTTP requests and dispatched
price refreshes within `SHUTDOWN_SECONDS`, followed by up to five seconds for
blocking SQLite cleanup. A drain timeout exits unsuccessfully and is logged.
Set the orchestrator's termination grace longer than this total; 35 seconds
covers the defaults.

`GET /health` checks local database readability without activating viewers or
calling providers. It returns 200 `{"status":"ok"}` for an initialized empty
database or 503 `{"status":"unavailable"}` on a read failure; it does not create
a missing database. The image probes this endpoint using its own application
binary, with no curl or shell dependency.

## Configuration and limits

| Environment variable | Default | Accepted values |
| --- | --- | --- |
| `DATABASE_PATH` | `data/bitcoin_prices.db` locally; `/app/data/bitcoin_prices.db` in the image | Writable SQLite path |
| `BIND_ADDRESS` | `0.0.0.0:3000` | IP address and port 1–65535; IPv6 example `[::1]:3000` |
| `MAX_VIEWERS` | `1000` | 1–100000 |
| `REQUESTS_PER_SECOND` | `250` | 1–10000 per request group |
| `REQUEST_CONCURRENCY` | `64` | 1–1024 per request group |
| `MAX_CONNECTIONS` | `128` | 1–4096 remote TCP connections; up to four separate loopback connections |
| `SHUTDOWN_SECONDS` | `25` | 1–120 |
| `RUST_LOG` | `bitcoin_price_tracker=info` locally; `info` in image | tracing filter |

Invalid numeric/bind settings fail startup with the setting's name. Expired
viewers are pruned before admission. At capacity, existing heartbeats and removals
still work; new viewers receive 429 with `Retry-After: 15`.

Price, presence, news, and assets have independent token buckets and concurrency
limits. News concurrency is capped at eight; health reserves 10 requests per
second and four concurrent handlers. Buckets can burst up to one second's
allowance. Rate exhaustion returns 429; concurrency exhaustion returns 503,
both with `Retry-After: 1`. Application requests have a 20-second deadline and
presence JSON is limited to 1024 bytes.

The origin accepts HTTP/1 only, with a five-second header deadline, 64 headers,
a 16 KiB read buffer, a 25-second total connection lifetime, and one request per
connection. Loopback probes use separate connection slots. No limiter trusts
forwarded IP headers or maintains per-client keys. Configure TLS and per-client
abuse controls at your reverse proxy.

At 1,000 tabs, five-second polling nominally needs about 200 requests/second in
each of the price and presence groups; 250/second leaves 25% rate headroom.
This is arithmetic, not measured sustainable capacity. Beads issue
`bitcoin_price_tracker-4mr` tracks load testing. Shared origin budgets alone do
not guarantee client fairness.

Info logs report provider outcome/duration and startup/shutdown; debug logs add
refresh, coverage, and overload decisions. Raw viewer IDs are not logged.
Timestamps use the host clock; nodes need working time synchronization, but the
container needs no NTP egress or clock-setting capability. Kubernetes definitions
and rollout evidence belong in the separate `ggaddy/kube-lan-rke2` repository.

## API examples

With the app running locally:

```bash
curl http://localhost:3000/health
curl http://localhost:3000/api/price
curl -X POST http://localhost:3000/api/presence -H 'Content-Type: application/json' \
  -d '{"session_id":"example-tab","active":true,"include_price":true}'
curl -X POST http://localhost:3000/api/presence -H 'Content-Type: application/json' \
  -d '{"session_id":"example-tab","active":false}'
```

The active example admits a viewer and returns an evaluated cached `price` object
while a refresh may run in the background. A single heartbeat expires after
15 seconds; it is not a persistent subscription. The inactive example removes it.
See the [API contract](docs/source-contract.md) for response fields and errors.

### Troubleshooting

- SQLite permission errors: check ownership of both the mounted directory and
  database as UID/GID 10001:10001. Stop the process before changing ownership or
  restoring a backup.
- Unhealthy container: inspect logs and run `bitcoin_price_tracker --healthcheck`
  inside it; check database readability and the configured bind port.
- 429 or busy 503: review viewer/request budgets and proxy traffic. These limits
  do not bypass upstream cooldowns; normal dashboard retries handle them.
- No price updates: a visible dashboard must send presence. Check provider health
  and client connectivity. STALE prices can coexist with a healthy database.
- Unsupported schema version: use a compatible application version; startup does
  not silently rewrite a newer database.

## Development and tests

Run from the repository root. Rust 1.85.1, rustfmt, and Clippy are pinned by
[rust-toolchain.toml](rust-toolchain.toml); Cargo declares the same minimum Rust
version. Use locked dependencies:

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

### Browser tests

The deterministic suite requires Node.js 20 or newer and pinned Playwright:

```bash
npm ci --prefix tests/browser
npm exec --prefix tests/browser -- playwright install chromium
npm test --prefix tests/browser
```

On Linux hosts missing browser system libraries, install Chromium with
`npm exec --prefix tests/browser -- playwright install --with-deps chromium`.
Tests serve actual dashboard assets locally, mock API requests, and reject
external requests. Coverage includes freshness, recovery, deadlines, visibility,
identity fallbacks, safe rendering, reduced motion, RSS, and narrow layouts.

To test embedded assets from a running Rust app, use
`BTC_APP_URL=http://127.0.0.1:3000 npm test --prefix tests/browser`; APIs remain
mocked. Optional `BTC_SCREENSHOT_DIR=/existing/directory` writes desktop and
narrow-layout screenshots.

### Container verification and Selenium

With Docker or Podman, Python 3, and host `readelf` (from binutils), verify an
already built image:

```bash
python3 scripts/verify_runtime.py --engine docker --image btc-matrix
```

The script uses disposable containers/volumes and a version-tagged Python helper
image. It checks exact-image native imports, locale guards, non-root startup,
health, seeded quotes, restart persistence, storage failure/recovery, bind mounts,
and shutdown. The application has no external network during these checks; the
helper image is pulled before readiness checks. Cleanup removes its own resources.

The separate Rust Selenium smoke test needs Docker or Podman and Python 3,
but no host Cargo:

```bash
scripts/run_selenium_smoke_test.sh
```

The runner uses version-tagged Selenium and Rust images, an ephemeral loopback
WebDriver port, and a shared container network namespace. It requires bind mounts
and `--network container:<id>` support (Linux engines, including WSL). The
workspace is mounted for build artifacts; no test database is retained.
Overrides: `CONTAINER_ENGINE`, `SELENIUM_IMAGE`, `SELENIUM_CONTAINER_NAME`,
`SELENIUM_WAIT_SECONDS` (1–300), and `RUST_TEST_IMAGE`. A supplied conflicting
container name is rejected; cleanup uses only created container IDs.

For a manually managed WebDriver and host Cargo, set `WEBDRIVER_URL` and
`SELENIUM_APP_HOST`, then run:

```bash
cargo test --locked selenium_dashboard_smoke_test -- --ignored
```

### Optional compiler cache

[Kache](https://github.com/kunobi-ninja/kache) is optional. Install a prebuilt
release, or use Rust 1.95 or newer for
`cargo install kache --locked --version 1.0.0`, then run `kache init`.
The application still uses Rust 1.85.1. Use `kache doctor` and `kache stats` to
inspect setup and cache hits.

### CI and advisories

[CI](.github/workflows/ci.yml) runs formatting, Clippy, Rust and Chromium tests,
Rust/npm advisory scans, and final-image build/runtime/security checks on PRs and
master pushes. It also runs weekly and supports manual dispatch. Actions use
pinned SHAs and read-only repository permissions. Rust and RustSec builds have
separate Kache caches; container publishing uses a Buildx layer cache.

There are no accepted **Rust or npm** advisories in the checked-in policy.
Runtime-image applicability exceptions are separately scoped and expire;
see [SECURITY.md](SECURITY.md) and [.grype.yaml](.grype.yaml).

## Releases

Publishing requires a `DOCKERHUB_TOKEN` Actions secret with write access to
`agaddy/bitcoin_price_tracker`. Merge through a passing PR and use a clean,
up-to-date `master` checkout. Check existing tags and choose an **unused** stable
`vMAJOR.MINOR.PATCH` version. Set `release_tag` to that value before running:

```bash
: "${release_tag:?Set release_tag to the new stable release tag}"
git tag "$release_tag"
git push origin "$release_tag"
```

The [container workflow](.github/workflows/container.yml) checks that the tagged
commit matches current `master`, reruns exact-commit Rust/browser/advisory checks,
and verifies/scans a candidate image before publishing that same image without
rebuilding. It publishes the version without the leading `v` and `latest`;
branch pushes and prerelease tags do not publish images. Weekly CI also resolves
and scans the published `latest` digest.

The checked-in [ruleset snapshots](.github/rulesets) require passing PR checks
and restrict release-tag creation to admins while prohibiting tag moves/deletion.
Live repository settings are external; see [security policy](SECURITY.md#runtime-images-and-release-checks).
A git tag, successful image publication, a GitHub Release record, and a production
rollout are separate events. This workflow publishes images; it does not create
a GitHub Release or update the infrastructure deployment.

## Documentation and work tracking

- [Source observations and price API contract](docs/source-contract.md): current
  wire format, SQLite migration, freshness, and persistence semantics.
- [Security policy and review history](SECURITY.md): current checks/exception
  policy, dated applicability reviews, and historical edge observations.
- [Archived implementation audit](AUDIT.md) and [September security audit](SECURITY_AUDIT.md):
  historical findings and evidence, rather than today's work or deployment status.
- [Original project brief](project_plan): the initial requirements, preserved as history.
- [Agent instructions](AGENTS.md): architecture, conventions, validation, and Beads workflow.

Current tasks, blockers, and persistent project memory live in Beads. Run
`bd ready`, `bd show <id>`, and `bd prime` for the work queue and workflow;
`.beads/issues.jsonl` is a passive export, not the issue database or sync protocol.
