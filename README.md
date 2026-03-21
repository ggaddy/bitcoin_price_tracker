# Bitcoin Price Tracker

Rust web app that shows BTC/USD spot prices from multiple free sources with a Matrix-style UI.

## Sources

- CoinGecko
- Coinbase
- Kraken

## Run locally

```bash
cargo run
```

Open `http://localhost:3000`.

## Run in container

```bash
docker build -t btc-matrix .
docker run --rm -p 3000:3000 btc-matrix
```

Open `http://localhost:3000`.
