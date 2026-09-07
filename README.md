# Bitcoin Price Tracker

Rust BTC/USD tracker with a terminal-style UI.

## Behavior

- Uses CoinGecko, Coinbase, Kraken, and Gemini
- Stores prices in local SQLite
- Serves the latest stored snapshot to the UI
- Refreshes only while a visible tab is active
- Rotates upstream checks one source at a time
- Skips refreshes when the latest snapshot is under 10 seconds old
- Limits upstream connections to 2 seconds and complete requests, including response bodies, to 5 seconds
- Requires finite, positive prices; serves the previous snapshot when a refresh fails and stored data is available

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

## Config

- Default DB: `data/bitcoin_prices.db`
- Override DB: `DATABASE_PATH=/custom/path/bitcoin_prices.db`

## Browser Test

- Selenium smoke test uses `thirtyfour` and is ignored by default
- End-to-end runner: `scripts/run_selenium_smoke_test.sh`
- Useful overrides: `CONTAINER_ENGINE`, `WEBDRIVER_URL`, `SELENIUM_APP_HOST`, `SELENIUM_WAIT_SECONDS`
