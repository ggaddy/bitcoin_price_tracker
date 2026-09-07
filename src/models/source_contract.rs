//! P3 source observations and response contract. See docs/source-contract.md.
//!
//! Stored records contain observations only. Response types add freshness and
//! aggregates computed at request time; snapshot persistence time is not a quote's age.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum QuoteKind {
    Aggregate,
    Spot,
    LastTrade,
    Bid,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct StoredQuote {
    pub(crate) source: String,
    pub(crate) price_usd: f64,
    pub(crate) last_success_at_unix: Option<i64>,
    pub(crate) quote_kind: QuoteKind,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ErrorCategory {
    Timeout,
    Transport,
    Http,
    InvalidPayload,
    InvalidPrice,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct SourceError {
    pub(crate) category: ErrorCategory,
    // A client-safe message, never a diagnostic error chain or database path.
    pub(crate) message: String,
    pub(crate) http_status: Option<u16>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub(crate) enum LastAttempt {
    // No trustworthy recorded attempt; does not claim the provider was never tried.
    Unknown,
    Success {
        attempted_at_unix: i64,
    },
    Failure {
        attempted_at_unix: i64,
        error: SourceError,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct ProviderHealth {
    pub(crate) source: String,
    pub(crate) last_attempt: LastAttempt,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SourceFreshness {
    Fresh,
    Stale,
    Unknown,
    Future,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SourceResponse {
    #[serde(flatten)]
    pub(crate) quote: StoredQuote,
    pub(crate) age_seconds: Option<i64>,
    pub(crate) freshness: SourceFreshness,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(crate) enum PriceStatus {
    Live,
    Degraded,
    Stale,
    Unavailable,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct AggregateCoverage {
    pub(crate) configured_source_count: usize,
    pub(crate) fresh_source_count: usize,
    pub(crate) contributing_sources: Vec<String>,
}

#[derive(Serialize)]
pub(crate) struct PriceResponse {
    pub(crate) symbol: &'static str,
    pub(crate) currency: &'static str,
    // Quote-bearing rows only, preserving the existing numeric price_usd contract.
    pub(crate) sources: Vec<SourceResponse>,
    // Includes configured providers without a successful quote.
    pub(crate) provider_health: Vec<ProviderHealth>,
    pub(crate) average_price: Option<f64>,
    pub(crate) spread: Option<f64>,
    pub(crate) status: PriceStatus,
    pub(crate) coverage: AggregateCoverage,
    pub(crate) source_max_age_seconds: u64,
    pub(crate) evaluated_at_unix: i64,
    // Compatibility: these describe snapshot persistence, not source freshness.
    pub(crate) fetched_at_unix: i64,
    pub(crate) fetched_age_seconds: Option<i64>,
    pub(crate) warnings: Vec<String>,
    pub(crate) refresh_succeeded: bool,
    pub(crate) stale: bool,
    pub(crate) active_viewers: usize,
    pub(crate) refresh_skipped_reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn legacy_quote_keeps_unknown_observation_time_without_computed_storage_fields() {
        let record = StoredQuote {
            source: "Coinbase".into(),
            price_usd: 100_000.0,
            last_success_at_unix: None,
            quote_kind: QuoteKind::Spot,
        };
        let value = serde_json::to_value(&record).unwrap();
        assert_eq!(
            value,
            json!({
                "source": "Coinbase", "price_usd": 100000.0,
                "last_success_at_unix": null, "quote_kind": "spot"
            })
        );
        assert_eq!(
            serde_json::from_value::<StoredQuote>(value).unwrap(),
            record
        );
    }

    #[test]
    fn failed_provider_without_a_quote_has_independent_health() {
        let health = ProviderHealth {
            source: "CoinGecko".into(),
            last_attempt: LastAttempt::Failure {
                attempted_at_unix: 1_700_000_010,
                error: SourceError {
                    category: ErrorCategory::Http,
                    message: "CoinGecko HTTP error: 429 Too Many Requests".into(),
                    http_status: Some(429),
                },
            },
        };
        let value = serde_json::to_value(&health).unwrap();
        assert_eq!(
            value,
            json!({
                "source": "CoinGecko",
                "last_attempt": {
                    "outcome": "failure", "attempted_at_unix": 1700000010,
                    "error": {"category": "http", "message": "CoinGecko HTTP error: 429 Too Many Requests", "http_status": 429}
                }
            })
        );
        assert_eq!(
            serde_json::from_value::<ProviderHealth>(value).unwrap(),
            health
        );
        assert_eq!(
            serde_json::to_value(LastAttempt::Unknown).unwrap(),
            json!({"outcome": "unknown"})
        );
        assert_eq!(
            serde_json::to_value(LastAttempt::Success {
                attempted_at_unix: 12
            })
            .unwrap(),
            json!({"outcome": "success", "attempted_at_unix": 12})
        );
        assert!(
            serde_json::from_value::<LastAttempt>(
                json!({"outcome": "failure", "attempted_at_unix": 12})
            )
            .is_err()
        );
    }

    #[test]
    fn response_contract_retains_legacy_keys_and_exposes_freshness_separately() {
        let response = PriceResponse {
            symbol: "BTC",
            currency: "USD",
            sources: vec![SourceResponse {
                quote: StoredQuote {
                    source: "Coinbase".into(),
                    price_usd: 100000.0,
                    last_success_at_unix: None,
                    quote_kind: QuoteKind::Spot,
                },
                age_seconds: None,
                freshness: SourceFreshness::Unknown,
            }],
            provider_health: ["CoinGecko", "Coinbase", "Kraken", "Gemini"]
                .into_iter()
                .map(|source| ProviderHealth {
                    source: source.into(),
                    last_attempt: LastAttempt::Unknown,
                })
                .collect(),
            average_price: None,
            spread: None,
            status: PriceStatus::Stale,
            coverage: AggregateCoverage {
                configured_source_count: 4,
                fresh_source_count: 0,
                contributing_sources: vec![],
            },
            source_max_age_seconds: 90,
            evaluated_at_unix: 1700000010,
            fetched_at_unix: 1700000000,
            fetched_age_seconds: Some(10),
            warnings: vec![],
            refresh_succeeded: false,
            stale: true,
            active_viewers: 1,
            refresh_skipped_reason: None,
        };
        let value = serde_json::to_value(response).unwrap();
        for key in [
            "symbol",
            "currency",
            "sources",
            "average_price",
            "spread",
            "fetched_at_unix",
            "fetched_age_seconds",
            "warnings",
            "refresh_succeeded",
            "stale",
            "active_viewers",
            "refresh_skipped_reason",
        ] {
            assert!(
                value.get(key).is_some(),
                "missing compatibility field: {key}"
            );
        }
        assert_eq!(value["sources"][0]["price_usd"], 100000.0);
        assert_eq!(value["sources"][0]["last_success_at_unix"], Value::Null);
        assert_eq!(value["sources"][0]["age_seconds"], Value::Null);
        assert_eq!(value["sources"][0]["freshness"], "unknown");
        assert_eq!(value["average_price"], Value::Null);
        assert_eq!(value["spread"], Value::Null);
        assert_eq!(value["status"], "STALE");
        assert_eq!(value["provider_health"].as_array().unwrap().len(), 4);
        assert_eq!(
            value["coverage"],
            json!({"configured_source_count": 4, "fresh_source_count": 0, "contributing_sources": []})
        );
    }

    #[test]
    fn enum_wire_values_are_stable() {
        assert_eq!(
            serde_json::to_value([
                PriceStatus::Live,
                PriceStatus::Degraded,
                PriceStatus::Stale,
                PriceStatus::Unavailable
            ])
            .unwrap(),
            json!(["LIVE", "DEGRADED", "STALE", "UNAVAILABLE"])
        );
        assert_eq!(
            serde_json::to_value([
                QuoteKind::Aggregate,
                QuoteKind::Spot,
                QuoteKind::LastTrade,
                QuoteKind::Bid,
                QuoteKind::Unknown
            ])
            .unwrap(),
            json!(["aggregate", "spot", "last_trade", "bid", "unknown"])
        );
        assert_eq!(
            serde_json::to_value([
                SourceFreshness::Fresh,
                SourceFreshness::Stale,
                SourceFreshness::Unknown,
                SourceFreshness::Future
            ])
            .unwrap(),
            json!(["fresh", "stale", "unknown", "future"])
        );
        assert_eq!(
            serde_json::to_value([
                ErrorCategory::Timeout,
                ErrorCategory::Transport,
                ErrorCategory::Http,
                ErrorCategory::InvalidPayload,
                ErrorCategory::InvalidPrice
            ])
            .unwrap(),
            json!([
                "timeout",
                "transport",
                "http",
                "invalid_payload",
                "invalid_price"
            ])
        );
    }
}
