# Bitcoin Price Tracker

A Rust/Axum BTC/USD price tracker with an embedded dark-only dashboard, four
upstream price providers, local SQLite persistence, an in-memory Bitcoin.com
RSS ticker, and a non-root pruned-distroless container image. Dashboard HTML,
CSS, and JavaScript are embedded in the binary with zero external CSS
frameworks, frontend build steps, or runtime CDN dependencies.

## Quick start

Install Rust with `rustup` (which selects the pinned `1.100.0` toolchain in
[rust-toolchain.toml](rust-toolchain.toml)), then run from the repository root:

```bash
cargo run --locked
```

Open `http://localhost:3000` in a visible browser tab.

- Prices persist to `data/bitcoin_prices.db` by default (only the latest merged
  snapshot and per-provider health are retained; this is not a historical
  time-series database).
- An empty database is healthy (`GET /health` returns `200`), while
  `GET /api/price` returns HTTP `503` with status `UNAVAILABLE` until an active
  viewer triggers a background refresh that commits a valid quote.

## How it works

- **Indicative average from four providers:** Combines CoinGecko (`aggregate`),
  Coinbase (`spot`), Kraken (`last_trade`), and Gemini (`bid`) quotes. Only
  configured providers with observation ages under 90 seconds contribute to
  `average_price` and `spread`.
- **Viewer-driven background refreshes:** Visible tabs generate an in-memory
  session ID and send `POST /api/presence` heartbeats every 5 seconds (15s TTL;
  hidden tabs pause and send inactive presence). Reads always return stored
  SQLite data without waiting for upstream I/O; one background refresh runs at a
  time, gated to at most once every 10 seconds with per-provider exponential
  backoff (`10s` → `300s`, honoring longer `Retry-After` hints).
- **Isolated RSS feed:** `GET /api/news` caches Bitcoin.com RSS XML in memory
  for 5 minutes with a single background refresh (12s timeout) and its own
  request concurrency budget so feed stalls never block price reads or health
  probes.
- **Single-instance design:** Run one application process per SQLite database
  behind a TLS-terminating reverse proxy. Presence, request budgets, refresh
  locks, and provider cooldowns are process-local.

## Quick API check

With the server running locally:

```bash
curl http://localhost:3000/health
curl http://localhost:3000/api/price
curl http://localhost:3000/api/news
curl -X POST http://localhost:3000/api/presence -H 'Content-Type: application/json' \
  -d '{"session_id":"example-tab","active":true,"include_price":true}'
curl -X POST http://localhost:3000/api/presence -H 'Content-Type: application/json' \
  -d '{"session_id":"example-tab","active":false}'
```

The active presence request admits a viewer (expiring after 15 seconds) and
returns the evaluated cached `price` object inline while any needed refresh runs
in the background; the inactive request removes that session. See
[docs/source-contract.md](docs/source-contract.md) for the full wire contract.

## Documentation

| Goal | Canonical document |
| --- | --- |
| Run in Docker/Podman, configure env vars/limits, caching, troubleshoot | [docs/operations.md](docs/operations.md) |
| Inspect HTTP endpoints, JSON fields, freshness rules, SQLite migration | [docs/source-contract.md](docs/source-contract.md) |
| Inspect internal state machines, invariants, and transition tables | [docs/state-machines.md](docs/state-machines.md) |
| Run Rust, Playwright, container, or Selenium tests; CI & releases | [docs/development.md](docs/development.md) |
| Run local origin load tests or inspect measured capacity limits | [docs/load-testing.md](docs/load-testing.md) |
| Review active security controls, advisory checks, and `.grype.yaml` policy | [SECURITY.md](SECURITY.md) |
| Coding-agent conventions and workflow | [AGENTS.md](AGENTS.md) |

### Work tracking and historical archives

Active tasks, blockers, and project memory live in **Beads** (`bd ready`,
`bd show <id>`, `bd prime`); `.beads/issues.jsonl` is a passive export, not the
issue database.

Dated records are archived separately so they are not confused with current
policy or active work queues:

- [docs/security/review-history.md](docs/security/review-history.md) and
  [docs/security/](docs/security) — dated dependency/image advisory reviews,
  machine-readable scan evidence, and October 2026 edge observations
- [docs/archive/audit-history.md](docs/archive/audit-history.md) — archived
  September–October 2026 implementation and audit history
- [docs/archive/security-audit-2026-09.md](docs/archive/security-audit-2026-09.md)
  — archived September 2026 security audit and v2.7.0 remediation report
- [docs/archive/project-plan.md](docs/archive/project-plan.md) — original
  project brief
