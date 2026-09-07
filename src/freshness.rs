use crate::{
    config::SOURCE_MAX_AGE_SECONDS,
    models::source_contract::{
        AggregateCoverage, LastAttempt, PriceStatus, ProviderHealth, SourceFreshness,
        SourceResponse, StoredQuote,
    },
    pricing::UpstreamSource,
};

pub(crate) struct QuoteEvaluation {
    pub(crate) sources: Vec<SourceResponse>,
    pub(crate) average_price: Option<f64>,
    pub(crate) spread: Option<f64>,
    pub(crate) status: PriceStatus,
    pub(crate) coverage: AggregateCoverage,
}

pub(crate) fn evaluate_quotes(
    quotes: Vec<StoredQuote>,
    health: &[ProviderHealth],
    now: i64,
) -> QuoteEvaluation {
    let sources: Vec<_> = quotes
        .into_iter()
        .filter(|quote| quote.price_usd.is_finite() && quote.price_usd > 0.0)
        .map(|quote| {
            let age_seconds = quote
                .last_success_at_unix
                .and_then(|time| now.checked_sub(time))
                .filter(|age| *age >= 0);
            let configured = UpstreamSource::ALL
                .iter()
                .any(|source| source.name() == quote.source);
            let freshness = if !configured {
                SourceFreshness::Unknown
            } else if quote.last_success_at_unix.is_some_and(|time| time > now) {
                SourceFreshness::Future
            } else {
                match age_seconds {
                    Some(age) if age < SOURCE_MAX_AGE_SECONDS as i64 => SourceFreshness::Fresh,
                    Some(_) => SourceFreshness::Stale,
                    None => SourceFreshness::Unknown,
                }
            };
            SourceResponse {
                quote,
                age_seconds,
                freshness,
            }
        })
        .collect();

    // Exactly one contribution per configured provider, in configured order.
    // If legacy duplicates exist, select its newest qualifying observation.
    let contributors: Vec<_> = UpstreamSource::ALL
        .iter()
        .filter_map(|provider| {
            sources
                .iter()
                .filter(|source| {
                    source.quote.source == provider.name()
                        && source.freshness == SourceFreshness::Fresh
                })
                .max_by_key(|source| source.quote.last_success_at_unix)
        })
        .collect();
    let average_price = contributors.first().map(|first| {
        // Positive finite inputs keep each difference bounded; avoid overflowing
        // a sum (or underflowing tiny quotes by dividing them before summing).
        let mut average = first.quote.price_usd;
        for (index, source) in contributors.iter().enumerate().skip(1) {
            average += (source.quote.price_usd - average) / (index + 1) as f64;
        }
        average
    });
    let spread = average_price.map(|_| {
        let min = contributors
            .iter()
            .map(|source| source.quote.price_usd)
            .fold(f64::INFINITY, f64::min);
        let max = contributors
            .iter()
            .map(|source| source.quote.price_usd)
            .fold(f64::NEG_INFINITY, f64::max);
        max - min
    });
    let all_healthy = UpstreamSource::ALL.iter().all(|provider| {
        health.iter().any(|record| {
            record.source == provider.name()
                && matches!(record.last_attempt, LastAttempt::Success { .. })
        })
    });
    let status = if contributors.len() == UpstreamSource::ALL.len() && all_healthy {
        PriceStatus::Live
    } else if !contributors.is_empty() {
        PriceStatus::Degraded
    } else if !sources.is_empty() {
        PriceStatus::Stale
    } else {
        PriceStatus::Unavailable
    };
    let coverage = AggregateCoverage {
        configured_source_count: UpstreamSource::ALL.len(),
        fresh_source_count: contributors.len(),
        contributing_sources: contributors
            .iter()
            .map(|source| source.quote.source.clone())
            .collect(),
    };
    QuoteEvaluation {
        sources,
        average_price,
        spread,
        status,
        coverage,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::source_contract::QuoteKind;

    fn quote(source: &str, price: f64, time: i64) -> StoredQuote {
        StoredQuote {
            source: source.into(),
            price_usd: price,
            last_success_at_unix: Some(time),
            quote_kind: QuoteKind::Unknown,
        }
    }

    #[test]
    fn extreme_positive_prices_keep_finite_nonzero_aggregates() {
        for price in [f64::MAX, f64::from_bits(1)] {
            let result = evaluate_quotes(
                UpstreamSource::ALL
                    .iter()
                    .map(|source| quote(source.name(), price, 100))
                    .collect(),
                &[],
                100,
            );
            assert_eq!(result.average_price, Some(price));
            assert_eq!(result.spread, Some(0.0));
            assert_eq!(result.coverage.fresh_source_count, 4);
            assert_eq!(result.status, PriceStatus::Degraded);
        }
    }

    #[test]
    fn duplicates_and_unknown_providers_cannot_inflate_coverage() {
        let result = evaluate_quotes(
            vec![
                quote("Coinbase", 200.0, 99),
                quote("Legacy", 1000.0, 100),
                quote("Coinbase", 100.0, 98),
                quote("Kraken", f64::NAN, 100),
                quote("Gemini", -1.0, 100),
                quote("CoinGecko", f64::INFINITY, 100),
            ],
            &[],
            100,
        );
        assert_eq!(result.sources.len(), 3);
        assert_eq!(result.sources[1].freshness, SourceFreshness::Unknown);
        assert_eq!(result.coverage.fresh_source_count, 1);
        assert_eq!(result.coverage.contributing_sources, ["Coinbase"]);
        assert_eq!(result.average_price, Some(200.0));
        assert_eq!(result.spread, Some(0.0));
        assert_eq!(result.status, PriceStatus::Degraded);
    }
}
