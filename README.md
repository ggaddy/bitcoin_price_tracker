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

## Local

```bash
cargo run
```

Open `http://localhost:3000` in a visible tab.

## Container

```bash
docker build -t btc-matrix .
docker run --rm -p 3000:3000 -v "$(pwd)/data:/app/data" btc-matrix
```

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

## Config

- Default DB: `data/bitcoin_prices.db`
- Override DB: `DATABASE_PATH=/custom/path/bitcoin_prices.db`

## Browser Test

- Selenium smoke test uses `thirtyfour` and is ignored by default
- End-to-end runner: `scripts/run_selenium_smoke_test.sh`
- Useful overrides: `CONTAINER_ENGINE`, `WEBDRIVER_URL`, `SELENIUM_APP_HOST`, `SELENIUM_WAIT_SECONDS`
