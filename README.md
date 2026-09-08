# Bitcoin Price Tracker

Rust BTC/USD tracker with a responsive Cybercore dashboard.

The UI uses [Cybercore CSS](https://github.com/sebyx07/cybercore-css/) 0.3.0.
Its stylesheet and MIT license are vendored in `src/vendor` and embedded in the
binary, so styling works without a CDN or a frontend build step.

## Behavior

- Uses CoinGecko, Coinbase, Kraken, and Gemini
- Stores prices in local SQLite
- Serves the latest stored snapshot to the UI
- Refreshes only while a visible tab is active
- Tracks viewer heartbeats with monotonic time: viewers remain active through 15 seconds and expire immediately after that, independent of system clock adjustments
- Rotates upstream checks one source at a time, advancing after failed attempts too
- Skips refreshes when the latest snapshot is under 10 seconds old or the previous attempt started less than 10 seconds ago
- Backs off failed providers for 10, 20, 40, 80, 160, then 300 seconds; successful provider recovery resets the delay
- Honors longer valid `Retry-After` delays or HTTP dates, skipping cooling-down providers while others continue
- Serves stored prices while another request refreshes; concurrent cold-start requests receive 503 until a snapshot is available
- Requests a full refresh when the first viewer becomes active, preserving new activations that arrive during a refresh
- Limits upstream connections to 2 seconds and complete requests, including response bodies, to 5 seconds
- Requires finite, positive prices; merges successful provider results with retained quotes when other providers fail
- Explicitly requests Coinbase BTC-USD, checks reported pair identifiers, and rejects Kraken API errors even with HTTP 200
- Leaves the stored snapshot unchanged if every provider fails or the database write fails
- Persists each provider's last successful observation time and quote kind, preserving both when another provider refreshes or that provider fails
- Saves provider outcomes and successful quotes in one transaction; all-provider failure saves health without advancing quote timestamps

The API's `refresh_succeeded` is true when at least one new quote was saved, including partial refreshes. Partial snapshots include a warning for each failed provider. When every provider fails, errors appear in that request's response, the quote snapshot remains intact, and the latest outcomes are saved independently. Any persistence failure rolls back both quotes and health. Snapshot timestamps describe persistence; response aggregates are recomputed from qualifying source observations on every request.

The API now includes `provider_health` for all configured providers, including
those without a quote. Each record contains the provider `source` and a
`last_attempt` outcome (`unknown`, `success`, or `failure`), with the attempt's
Unix start time and a safe error for failures. Health survives restarts and is
read consistently with quotes. Each source exposes its observation time, quote kind, age, and freshness.
Only configured providers with finite positive quotes aged less than 90 seconds
contribute to the average and spread; unknown, future, and expired observations
remain visible but do not contribute. With no contributors, both aggregates are null.
`coverage` reports configured/fresh counts and contributor names.

`status` is `LIVE` when all four providers are fresh with successful health,
`DEGRADED` when some quotes qualify but coverage or health is incomplete,
`STALE` when valid retained quotes exist but none qualify, and `UNAVAILABLE`
when no valid quote exists. A refresh persistence error also degrades LIVE.
The compatibility `stale` flag is true for every status except LIVE. HTTP status
remains 200 for retained valid quotes, 503 without valid quotes, or 500 for a
database read failure.
See [the P3 source contract](docs/source-contract.md) for the complete field definitions.

The displayed value is an indicative average: it combines Coinbase spot,
Gemini bid, Kraken last-trade, and CoinGecko aggregate quotes. Invalid prices
are rejected by provider adapters and the storage writer; invalid retained rows
are excluded when merging a new snapshot.

Startup now migrates unversioned SQLite databases to schema version 1 atomically.
It preserves the latest quotes, adds nullable observation times and quote kinds,
and initializes independent provider health as unknown. Legacy observation times
are never inferred from snapshot time. Repeated startup preserves the metadata;
migration failure rolls back all schema/data changes, and unsupported schema
versions stop startup. Successful refreshes now replace unknown legacy
observation times with actual per-provider completion times.

Retry delays start when the batch completes and use monotonic deadlines. Invalid or
unrepresentably large retry hints fall back to exponential backoff. When every
provider is cooling down, requests serve stored data without consuming another
attempt. Retry state is process-local and resets on restart; the persisted
snapshot still supplies the ten-second success-age gate.

Presence is rechecked after reading the snapshot and before dispatching providers.
Already dispatched work may finish and save prices after the last viewer leaves;
the response reports the current viewer count. Future snapshot timestamps have
unknown age (`fetched_age_seconds: null`) and are treated as stale, while the
monotonic attempt gate still prevents rapid retries.

## Dashboard behavior

Each page creates a fresh in-memory presence ID, so opened or duplicated tabs
cannot inherit another tab's identity. Browser storage is not required. UUID
support falls back to secure random bytes; missing random/request capabilities
produce an explicit Unsupported state.

Visible pages run independent presence and price loops, waiting five seconds
between completed requests. Presence has a three-second deadline and prices an
eight-second deadline, including response bodies. Hidden pages cancel requests,
stop timers, and send inactive presence; returning resumes one set of loops.
Failures retry automatically and retain received quotes with an age warning.

Source cards show quote kind, elapsed seconds, and the latest provider error,
including providers without quotes. Coverage and indicative aggregates update
as quotes expire between responses; unknown/future quotes never become fresh
locally. Last price update uses source observation times. Status transitions
are announced politely, and decorative CSS animation pauses while hidden and
respects reduced-motion preferences.

## Local

Install Rust with rustup, then run:

```bash
cargo run --locked
```

`rust-toolchain.toml` pins Rust 1.85.1 with rustfmt and Clippy; `Cargo.toml`
declares the same minimum version. Tokio's runtime, I/O, signal, synchronization,
and timer features are explicit. Development checks use locked dependencies:

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

Open `http://localhost:3000` in a visible tab.

## Container

The final image runs as UID/GID **10001:10001** and includes no Rust build tools.
A named volume is initialized with a writable data directory:

```bash
docker build -t btc-matrix .
docker volume create btc-data
docker run -d --name btc-tracker -p 3000:3000 -v btc-data:/app/data btc-matrix
docker exec btc-tracker bitcoin_price_tracker --healthcheck
docker stop --time 35 btc-tracker
```

For an existing bind mount, stop the previous instance and back up its database
before migration. The container user must own the directory and database, because
SQLite also creates journal files there. Prepare permissions within the engine's
user namespace (this also works with rootless Podman):

```bash
mkdir -p data
docker run --rm --user 0:0 -v "$PWD/data:/app/data" btc-matrix chown -R 10001:10001 /app/data
docker run -d --name btc-tracker -p 3000:3000 -v "$PWD/data:/app/data" btc-matrix
```

For Podman, build with `podman build --format docker -t btc-matrix .` to preserve
HEALTHCHECK metadata; then use `podman` in place of `docker` in run commands.
On SELinux hosts, use an appropriate
volume label such as `:Z`. Do not run multiple instances against the same SQLite
file: viewer presence, rate limits, refresh ownership, and retry scheduling are
process-local. Use a single application instance behind your reverse proxy.

SIGINT/SIGTERM stops accepting new work and drains active requests for up to
`SHUTDOWN_SECONDS`, followed by up to five seconds for blocking SQLite cleanup.
A drain timeout exits unsuccessfully and is logged. Configure your orchestrator's
termination grace longer than this total; 35 seconds covers the defaults.

`GET /health` reports only local database readability, with 200 `{"status":"ok"}`
or 503 `{"status":"unavailable"}`. It works with an empty initialized database,
never activates viewers or fetches upstream prices, and does not create a missing
database. The image's health probe calls this endpoint using the application
binary, with no curl or shell dependency.

### Runtime verification

```bash
python3 scripts/verify_runtime.py --engine docker --image btc-matrix
```

This creates unique disposable containers and a named volume, verifies non-root
startup, custom bind configuration, empty-data health, seeded quote serving,
pause/resume, restart persistence, unhealthy storage, probe recovery, and graceful
shutdown, and removes only its own resources. A pinned Python helper image drives
HTTP/SQLite checks. The application has no external network during these checks;
no public provider access is possible.

### Container CI and releases

The GitHub Actions container workflow builds each pull request when opened,
updated, or reopened. PR builds do not log in to Docker Hub or push images.

To enable publishing, add a repository Actions secret named `DOCKERHUB_TOKEN`
containing a Docker Hub access token for `agaddy` with write access to
`agaddy/bitcoin_price_tracker`.

Push a stable release tag in the existing `vMAJOR.MINOR.PATCH` format:

```bash
git tag v2.3.1
git push origin v2.3.1
```

This builds and pushes `agaddy/bitcoin_price_tracker:2.3.1` and
`agaddy/bitcoin_price_tracker:latest`. Each published release updates `latest`,
including releases from older branches. Branch pushes and prerelease tags do
not publish images. The workflow must be present in the tagged commit.

## Configuration and limits

| Environment variable | Default | Accepted values |
| --- | --- | --- |
| `DATABASE_PATH` | `data/bitcoin_prices.db` locally; `/app/data/bitcoin_prices.db` in the image | Writable SQLite path |
| `BIND_ADDRESS` | `0.0.0.0:3000` | IP address and port 1–65535; IPv6 example `[::1]:3000` |
| `MAX_VIEWERS` | `1000` | 1–100000 |
| `REQUESTS_PER_SECOND` | `100` | 1–10000 per request group |
| `REQUEST_CONCURRENCY` | `64` | 1–1024 per request group |
| `SHUTDOWN_SECONDS` | `25` | 1–120 |
| `RUST_LOG` | `bitcoin_price_tracker=info` locally; `info` in image | tracing filter |

Invalid numeric/bind settings cause startup to fail with the setting's name.
Expired viewers are pruned before admission. At viewer capacity, existing
heartbeats and removals still work; new viewers receive 429 with a safe error and
`Retry-After: 15`.

Price, presence, and asset requests have independent token buckets and concurrency
limits, so flooding presence does not consume cached-price capacity. Each bucket
can burst up to one second's allowance. Health has reserved capacity (10 requests
per second, four concurrent). Rate exhaustion returns 429; concurrency exhaustion
returns 503, both with `Retry-After: 1`. Requests have a 20-second application
deadline, and presence JSON is limited to 1024 bytes. No limiter stores client/IP
keys or trusts forwarded IP headers. Configure TLS, connection/header limits, and
any per-client policy at your reverse proxy; tune these application-wide budgets
to the expected audience. Ordinary dashboard retries already handle 429/503.

Info logs report provider outcome/duration and startup/shutdown. Debug logs add
refresh decisions, coverage, and overload decisions. Raw viewer IDs are not logged;
database/transport diagnostic details remain server-side.

### API examples

```bash
curl http://localhost:3000/health
curl http://localhost:3000/api/price
curl -X POST http://localhost:3000/api/presence -H 'Content-Type: application/json' \
  -d '{"session_id":"example-tab","active":true}'
```

An empty database is healthy but returns HTTP 503/UNAVAILABLE for prices until a
viewer activates a successful refresh. Source timestamps remain unchanged on
provider failure, and expired quotes remain visible with nullable aggregates;
see [the source contract](docs/source-contract.md).

### Troubleshooting

- Startup SQLite permission errors: check ownership of both the mounted directory
  and existing database as UID/GID 10001:10001. Stop the old process before changing
  ownership or restoring a backup.
- Unhealthy container: inspect application logs and run `bitcoin_price_tracker
  --healthcheck` inside it; check database readability and the configured bind port.
- 429 or busy 503 responses: review request/viewer budgets and proxy traffic;
  capacity limits do not change the upstream refresh cooldown.
- No upstream updates: a visible dashboard must send presence. Check provider
  health/error fields and client connectivity before treating this as a database
  failure. STALE prices can coexist with a healthy local database.
- Unsupported schema version: use a compatible application version; startup does
  not silently rewrite a newer database. Keep backups when upgrading.

## Continuous integration

`.github/workflows/ci.yml` runs Rust formatting, Clippy, locked tests, deterministic
Chromium tests, a final-image build/runtime smoke check, and Rust/npm advisory
scans on pull requests and master pushes. It also runs weekly and supports manual
dispatch. Actions are pinned to commit SHAs with read-only repository permissions.
Release container publishing remains in `container.yml`.

See [SECURITY.md](SECURITY.md) for scanner versions and the required process for
any advisory exception. There are no accepted advisories.

## Browser tests

The deterministic dashboard suite uses pinned Playwright/Chromium and Node.js
20 or newer. Install once, then run from the repository root:

```bash
npm ci --prefix tests/browser
npm exec --prefix tests/browser -- playwright install chromium
npm test --prefix tests/browser
```

On a Linux host missing browser system libraries, use Playwright's
`install --with-deps chromium` command. Tests serve the actual dashboard assets
locally, intercept every API request with fixtures, and reject external requests.
The suite covers freshness, recovery, request deadlines, visibility changes,
identity fallbacks, safe rendering, reduced motion, and narrow layouts.

To verify the embedded assets from a running local Rust app instead of the test
asset server, use `BTC_APP_URL=http://127.0.0.1:3000 npm test --prefix tests/browser`.
API requests remain mocked. Optional `BTC_SCREENSHOT_DIR=/existing/directory`
writes desktop and narrow-layout screenshots.

The separate Rust Selenium smoke test uses local providers and a manual clock:

```bash
scripts/run_selenium_smoke_test.sh
```

The runner needs Docker or Podman and Python 3, but no host Cargo. It starts
`selenium/standalone-chrome:4.48.0-20260905`, waits for readiness on an ephemeral
loopback port, and runs the test in the pinned Rust container sharing Selenium's
network namespace. This keeps app and WebDriver traffic on loopback without host
networking or host-name assumptions. The engine must support
`--network container:<id>` and bind mounts (Linux engines, including those in WSL).
The workspace is mounted for Cargo build artifacts; no test database is retained.

Overrides: `CONTAINER_ENGINE`, `SELENIUM_IMAGE`, `SELENIUM_CONTAINER_NAME`,
`SELENIUM_WAIT_SECONDS` (1–300), and `RUST_TEST_IMAGE`. The default container name
is unique. A supplied conflicting name is rejected; existing containers are never
removed. Cleanup uses only IDs created by the runner, including failure paths.
For a manually managed WebDriver with host Cargo, set `WEBDRIVER_URL` and
`SELENIUM_APP_HOST` and run `cargo test --locked selenium_dashboard_smoke_test -- --ignored`.
