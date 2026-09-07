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
- Rotates upstream checks one source at a time, advancing after failed attempts too
- Skips refreshes when the latest snapshot is under 10 seconds old or the previous attempt started less than 10 seconds ago
- Backs off failed providers for 10, 20, 40, 80, 160, then 300 seconds; successful provider recovery resets the delay
- Honors longer valid `Retry-After` delays or HTTP dates, skipping cooling-down providers while others continue
- Serves stored prices while another request refreshes; concurrent cold-start requests receive 503 until a snapshot is available
- Requests a full refresh when the first viewer becomes active, preserving new activations that arrive during a refresh
- Limits upstream connections to 2 seconds and complete requests, including response bodies, to 5 seconds
- Requires finite, positive prices; merges successful provider results with retained quotes when other providers fail
- Leaves the stored snapshot unchanged if every provider fails or the database write fails

The API's `refresh_succeeded` is true when at least one new quote was saved, including partial refreshes. Partial snapshots include a warning for each failed provider. If nothing can be saved, errors appear in that request's response and the previous snapshot remains intact. Snapshot timestamps and aggregates still describe the merged snapshot; per-source freshness and persistent health tracking are planned in P3 of [AUDIT.md](AUDIT.md).

Retry delays start when the batch completes and use monotonic deadlines. Invalid or
unrepresentably large retry hints fall back to exponential backoff. When every
provider is cooling down, requests serve stored data without consuming another
attempt. Retry state is process-local and resets on restart; the persisted
snapshot still supplies the ten-second success-age gate.

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
