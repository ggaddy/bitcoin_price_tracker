# Source observations and freshness contract

The price response types are defined in
[`src/models/source_contract.rs`](../src/models/source_contract.rs); presence
types are in [`src/models.rs`](../src/models.rs), with HTTP behavior in
[`src/handlers.rs`](../src/handlers.rs).
SQLite schema version 1 stores observations and provider health atomically.
Responses evaluate per-source freshness, status, coverage, and aggregates at
request time in [`src/freshness.rs`](../src/freshness.rs); persisted snapshot
summaries are not reused. Serialization and route tests verify the wire format
and runtime behavior. This is the current reference; historical P1–P3 phase
names and implementation evidence are in [the archived audit](../AUDIT.md).

## Schema migration

Startup migrates unversioned databases (`PRAGMA user_version = 0`) to version 1
inside one immediate transaction. This includes the older schema without
`refreshed_source`, new observation columns, provider health initialization,
latest-only pruning, and the version update. A failure rolls back all these
changes. Unsupported versions are rejected before schema or retention changes.

`source_prices` gains nullable `last_success_at_unix` and non-null `quote_kind`
(default `unknown`). Migrated provider names determine known kinds; all legacy
observation timestamps remain null. `provider_health` is keyed by provider name
and stores `attempt_outcome`, nullable `attempted_at_unix`, `error_category`,
`error_message`, and `http_status`. Constraints enforce the documented outcome
and error combinations. Health is independent of snapshot deletion and starts
as unknown for all four configured providers, even in an empty database.

Repeat startup preserves version-1 metadata and applies the existing latest-only
retention policy. Normal refreshes now persist each successful provider's own
observation time and quote kind, alongside outcomes for all attempted providers.
An all-failure batch updates health without rewriting quote rows or snapshot
metadata. Reading snapshot metadata, quotes, and provider health uses one
transaction, even when only failure health exists in an otherwise empty database.

## Persisted observations

A `StoredQuote` represents the last validated, finite, positive price from one
provider. It has `source` (the existing provider name), `price_usd` (a number),
`last_success_at_unix` (an integer or explicit null), and `quote_kind`.

`last_success_at_unix` records when this application received and validated that
provider's successful result, in Unix seconds. It is an application observation
time, not an exchange trade timestamp. Capture it per provider, not when the
batch finishes or SQLite saves the merged snapshot. Replacing another provider's
quote never changes it. A failed request also never changes it.

Legacy rows migrate with a null last-success timestamp. Never derive that time
from the snapshot timestamp. Known providers can retain their known quote kind;
unrecognized legacy provider names use `unknown` and remain visible but do not
contribute to configured-provider coverage or aggregates.

| Provider | `quote_kind` | Meaning |
| --- | --- | --- |
| CoinGecko | `aggregate` | Aggregated BTC/USD price |
| Coinbase | `spot` | BTC/USD spot price |
| Kraken | `last_trade` | BTC/USD last trade / close field |
| Gemini | `bid` | BTC/USD bid price |
| Unrecognized legacy source | `unknown` | Quote semantics not established |

The combined number is an **indicative average**, since these are different
quote types. The dashboard labels it accordingly. Provider adapters validate
the selected pair and payload:

- [Coinbase spot prices](https://docs.cdp.coinbase.com/coinbase-business/track-apis/prices): request `/v2/prices/BTC-USD/spot`, require USD currency, and validate BTC base whenever that optional field is present. The documented response can omit base.
- [Kraken ticker](https://docs.kraken.com/api-reference/market-data/get-ticker-information): require an empty error array and select the default internal BTC/USD result key `XXBTZUSD` explicitly, regardless of other returned markets.
- [Gemini ticker v2](https://developer.gemini.com/rest/market-data): require the BTCUSD symbol and read its bid. Matching the symbol is case-insensitive.
- CoinGecko continues to select numeric `bitcoin.usd` explicitly. All adapters reject nonfinite/nonpositive prices. Storage also rejects invalid quote values, and new snapshot merges omit invalid retained rows.

`ProviderHealth` is stored independently of the quote. There is one latest
record per configured provider, including providers that have never supplied a
valid quote. Its `last_attempt` is one of:

```json
{"outcome": "unknown"}
```

```json
{"outcome": "success", "attempted_at_unix": 1700000010}
```

```json
{"outcome": "failure", "attempted_at_unix": 1700000010,
 "error": {"category": "http", "message": "CoinGecko HTTP error: 429 Too Many Requests", "http_status": 429}}
```

`attempted_at_unix` is the provider request's start time. Only completed outcomes
are persisted. `unknown` means no trustworthy recorded attempt; it does not claim
that a legacy provider has never been contacted. A successful outcome means the
provider returned a valid quote; the quote and health update are committed in the
same transaction. A persistence failure rolls both back. A failed provider
updates only its health, retaining its last good quote and observation time.

Error categories are `timeout`, `transport`, `http`, `invalid_payload`, and
`invalid_price`. Messages must be client-safe, using the existing provider error
display policy. `http_status` is an integer for HTTP failures and null otherwise.
Raw transport errors, database paths, session IDs, retry counters, and monotonic
deadlines are not part of this persisted/public health contract.

Snapshot `fetched_at_unix`, saved averages/spreads, batch warnings, and refreshed
source metadata remain persistence metadata. They must not be interpreted as
current per-source freshness. A batch with only provider failures updates health
without advancing the quote snapshot timestamp. Quotes and health are read
together in one SQLite transaction.

## Computed response fields

`GET /api/price` returns `symbol: "BTC"`, `currency: "USD"`, and the fields
described below. `sources` contains quote-bearing rows with numeric `price_usd`;
providers without a quote have no synthetic zero/null price row. Each row
includes its stored quote metadata plus computed `age_seconds` (integer or null)
and `freshness`:

| `freshness` | Condition | `age_seconds` | Aggregate eligibility |
| --- | --- | --- | --- |
| `fresh` | Valid quote from a configured provider, known observation time, age below 90 seconds | Nonnegative integer | Included |
| `stale` | Configured provider with known observation time, age at least 90 seconds | Nonnegative integer | Excluded |
| `unknown` | Missing/unrepresentable age or unrecognized provider | Null if age cannot be calculated; otherwise nonnegative age | Excluded |
| `future` | Configured provider with observation time after response evaluation time | Null | Excluded |

Age is evaluated once per response using `evaluated_at_unix`. Age zero is fresh;
age 89 is fresh; age 90 is stale. Use checked arithmetic. A finite positive quote
with an unknown or future time remains visible but is excluded from aggregates.
Invalid numeric quotes must be rejected before persistence and excluded from
response quote rows if encountered in legacy/corrupt data.

`provider_health` is an array of the latest recorded health for all configured
providers, joined by `source`. This exposes failures even when `sources` is empty.
A provider failure may coexist with a still-fresh retained quote; that quote can
contribute to the average, but the response is degraded until recovery.

`coverage` contains `configured_source_count` (currently 4),
`fresh_source_count`, and `contributing_sources`. Contributors are unique names in
configured provider order, and their length equals `fresh_source_count`, which
cannot exceed `configured_source_count`. Retained unrecognized legacy rows do not
inflate either count. If legacy duplicates exist, the newest qualifying observation
for each provider contributes once. The count pair expresses aggregate coverage without a
redundant floating-point percentage.

`average_price` and `spread` are computed only from contributing quotes. One
contributor produces its own price as the average and zero spread. No contributors
produces null for both fields. Never reuse the stored snapshot's summaries for
these response values. `source_max_age_seconds: 90` and `evaluated_at_unix`
let clients explain and update displayed age consistently.

| `status` | Condition | Compatibility `stale` |
| --- | --- | --- |
| `LIVE` | Every configured provider has a fresh quote and latest health outcome `success` | false |
| `DEGRADED` | At least one contributor, but incomplete coverage or any configured provider has a failure/unknown health outcome | true |
| `STALE` | Retained valid quotes exist, but no contributors | true |
| `UNAVAILABLE` | No valid quote is available | true |

A refresh persistence error also changes an otherwise LIVE response
to DEGRADED, even when the previous stored health remains successful.

The price handler returns HTTP 200 when a valid retained quote exists, even if
its aggregate is null and status is STALE, or 503 with UNAVAILABLE when no valid
quote exists.
A local database read failure remains HTTP 500 with UNAVAILABLE and a safe
warning; it cannot claim that the database has never held quotes. In that error
response use empty source/health arrays and zero fresh coverage, because the
persisted records could not be read. Request middleware can instead return
429 (rate limit), 503 (concurrency limit), or 408 (deadline) with a safe `error`
object rather than the price response shape; see [request limits](../README.md#configuration-and-limits).

`fetched_at_unix` is the snapshot persistence time (0 without a snapshot).
`fetched_age_seconds` is its nonnegative age at evaluation, or null without a
snapshot, for future timestamps, or when arithmetic is unrepresentable. This
snapshot age does not describe any provider's observation age.

Price requests serve retained quotes and may start one background refresh
without waiting for provider I/O. `refreshing`
is a boolean reporting that refresh ownership is reserved/in progress.
`refresh_succeeded` describes the latest completed attempt in this process,
not a synchronous refresh performed by the current request. It means at least one
new quote was committed, can be true with DEGRADED status, and remains unchanged
by successful cached reads. A database-read failure reports false regardless of
the previous attempt. It starts false on process restart, and is false after an
all-failure health-only update or persistence failure. Quote/health records remain
durable; safe nonpersisted refresh errors remain visible until the next completed
attempt. `active_viewers` is the current nonnegative viewer count, `warnings` is
an array of safe strings, and `refresh_skipped_reason` is a string or null.
The compatibility `stale` flag is false only for LIVE; it also covers DEGRADED.

## Presence and cached startup prices

`POST /api/presence` accepts `session_id` and boolean `active`, with optional
`include_price` (default false). Session IDs contain 1–128 ASCII alphanumeric,
hyphen, or underscore characters. Expired viewers are pruned before admission;
existing heartbeats/removals work at capacity. New viewers at capacity receive
429 with `Retry-After: 15`; invalid session IDs receive 400. Accepted active
heartbeats remain present through 15 seconds and expire immediately afterward,
using monotonic time. An inactive heartbeat removes that session.

An active heartbeat may add `include_price: true`. Only a
successfully admitted viewer receives the optional `price` response object, using
the same evaluated price contract. Admission still returns HTTP 200 when the
nested price status is UNAVAILABLE, or when the database cannot be read; its
warnings/status report that separately from successful registration. Normal
heartbeats, inactive beacons and rejected/invalid viewers omit the price object.
Requests without the option receive the small `active_viewers` response.
The dashboard falls back to a price GET if an older server omits the object.

Admitted startup reads and ordinary price reads share refresh ownership,
the ten-second gates and provider cooldowns. Dispatched price work survives
client cancellation. Shutdown prevents new dispatch and drains in-progress
price work through its commit within the same configured HTTP shutdown budget.

## Integration checks

Tests cover migration rollback, null legacy observation times, repeat startup,
independent health for providers without quotes, atomic quote/health writes,
and persistence across restart. Route tests cover unknown/future timestamps,
the exact 90-second boundary, partial coverage, fresh retained quotes after
failure, all-failure batches, status, and nullable aggregates. Evaluator tests
cover duplicate/unrecognized providers, invalid values, and extreme finite prices.
The migration/recovery matrix includes JSON contract assertions.
Both legacy schema variants are exercised through migration, unknown-age serving,
partial refresh, restart, expiration, full recovery, and another restart. Empty
and failed cold starts return 503 with independent health; unreadable storage
returns 500 without exposing internal details.
The dashboard displays per-source metadata, health, and aggregate coverage.
It advances reported ages with monotonic elapsed time, removes expired quotes
from displayed aggregates, and can downgrade LIVE locally. Only a new server
response can make an unknown, future, or stale observation fresh. Transport
failures retain quote cards with an OFFLINE warning; failed heartbeats degrade
an otherwise LIVE display. Last price update uses the latest nonfuture source
observation time, rather than snapshot persistence time.
