use reqwest::Client;
use serde_json::Value;
use std::collections::HashMap;
use tokio::try_join;

use crate::{
    models::{SnapshotRecord, SourcePrice},
    state::AppState,
    storage::store_snapshot,
    util::now_unix,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UpstreamSource {
    CoinGecko,
    Coinbase,
    Kraken,
    Gemini,
}

impl UpstreamSource {
    const ALL: [Self; 4] = [Self::CoinGecko, Self::Coinbase, Self::Kraken, Self::Gemini];

    fn name(self) -> &'static str {
        match self {
            Self::CoinGecko => "CoinGecko",
            Self::Coinbase => "Coinbase",
            Self::Kraken => "Kraken",
            Self::Gemini => "Gemini",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        match name {
            "CoinGecko" => Some(Self::CoinGecko),
            "Coinbase" => Some(Self::Coinbase),
            "Kraken" => Some(Self::Kraken),
            "Gemini" => Some(Self::Gemini),
            _ => None,
        }
    }

    fn next_source(last_refreshed_source: Option<&str>) -> Self {
        let Some(last_source) = last_refreshed_source.and_then(Self::from_name) else {
            return Self::ALL[0];
        };

        let Some(index) = Self::ALL.iter().position(|source| *source == last_source) else {
            return Self::ALL[0];
        };

        Self::ALL[(index + 1) % Self::ALL.len()]
    }
}

async fn fetch_coingecko(client: &Client) -> Result<SourcePrice, String> {
    let value: Value = client
        .get("https://api.coingecko.com/api/v3/simple/price?ids=bitcoin&vs_currencies=usd")
        .send()
        .await
        .map_err(|error| format!("CoinGecko request failed: {error}"))?
        .error_for_status()
        .map_err(|error| format!("CoinGecko HTTP error: {error}"))?
        .json()
        .await
        .map_err(|error| format!("CoinGecko parse failed: {error}"))?;

    let price = value
        .get("bitcoin")
        .and_then(|entry| entry.get("usd"))
        .and_then(Value::as_f64)
        .ok_or_else(|| "CoinGecko response missing bitcoin.usd".to_string())?;

    Ok(SourcePrice {
        source: "CoinGecko".to_string(),
        price_usd: price,
    })
}

async fn fetch_coinbase(client: &Client) -> Result<SourcePrice, String> {
    let value: Value = client
        .get("https://api.coinbase.com/v2/prices/spot?currency=USD")
        .send()
        .await
        .map_err(|error| format!("Coinbase request failed: {error}"))?
        .error_for_status()
        .map_err(|error| format!("Coinbase HTTP error: {error}"))?
        .json()
        .await
        .map_err(|error| format!("Coinbase parse failed: {error}"))?;

    let amount = value
        .get("data")
        .and_then(|entry| entry.get("amount"))
        .and_then(Value::as_str)
        .ok_or_else(|| "Coinbase response missing data.amount".to_string())?;

    let price = amount
        .parse::<f64>()
        .map_err(|error| format!("Coinbase amount parse failed: {error}"))?;

    Ok(SourcePrice {
        source: "Coinbase".to_string(),
        price_usd: price,
    })
}

async fn fetch_kraken(client: &Client) -> Result<SourcePrice, String> {
    let value: Value = client
        .get("https://api.kraken.com/0/public/Ticker?pair=XBTUSD")
        .send()
        .await
        .map_err(|error| format!("Kraken request failed: {error}"))?
        .error_for_status()
        .map_err(|error| format!("Kraken HTTP error: {error}"))?
        .json()
        .await
        .map_err(|error| format!("Kraken parse failed: {error}"))?;

    let ticker = value
        .get("result")
        .and_then(Value::as_object)
        .and_then(|result| result.values().next())
        .ok_or_else(|| "Kraken response missing result entry".to_string())?;

    let close_str = ticker
        .get("c")
        .and_then(Value::as_array)
        .and_then(|entries| entries.first())
        .and_then(Value::as_str)
        .ok_or_else(|| "Kraken response missing close price at result.*.c[0]".to_string())?;

    let price = close_str
        .parse::<f64>()
        .map_err(|error| format!("Kraken close parse failed: {error}"))?;

    Ok(SourcePrice {
        source: "Kraken".to_string(),
        price_usd: price,
    })
}

async fn fetch_gemini(client: &Client) -> Result<SourcePrice, String> {
    let value: Value = client
        .get("https://api.gemini.com/v2/ticker/btcusd")
        .send()
        .await
        .map_err(|error| format!("Gemini request failed: {error}"))?
        .error_for_status()
        .map_err(|error| format!("Gemini HTTP error: {error}"))?
        .json()
        .await
        .map_err(|error| format!("Gemini parse failed: {error}"))?;

    let bid = value
        .get("bid")
        .and_then(Value::as_str)
        .ok_or_else(|| "Gemini response missing bid".to_string())?;

    let price = bid
        .parse::<f64>()
        .map_err(|error| format!("Gemini bid parse failed: {error}"))?;

    Ok(SourcePrice {
        source: "Gemini".to_string(),
        price_usd: price,
    })
}

pub(crate) async fn refresh_snapshot(
    state: &AppState,
    latest_snapshot: Option<SnapshotRecord>,
    refresh_all_sources: bool,
) -> Result<(), String> {
    let (sources, refreshed_source) = if refresh_all_sources {
        (
            fetch_all_sources(&state.client).await?,
            Some("all".to_string()),
        )
    } else {
        let next_source = UpstreamSource::next_source(
            latest_snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.refreshed_source.as_deref()),
        );
        let refreshed_price = fetch_round_robin_source(&state.client, next_source).await?;
        (
            merge_snapshot_sources(latest_snapshot.as_ref(), refreshed_price),
            Some(next_source.name().to_string()),
        )
    };
    let (average_price, spread) = summarize_prices(&sources);

    let snapshot = SnapshotRecord {
        fetched_at_unix: now_unix(),
        sources,
        average_price,
        spread,
        warnings: Vec::new(),
        refreshed_source,
    };

    store_snapshot(state.db_path.clone(), snapshot).await
}

async fn fetch_round_robin_source(
    client: &Client,
    source: UpstreamSource,
) -> Result<SourcePrice, String> {
    match source {
        UpstreamSource::CoinGecko => fetch_coingecko(client).await,
        UpstreamSource::Coinbase => fetch_coinbase(client).await,
        UpstreamSource::Kraken => fetch_kraken(client).await,
        UpstreamSource::Gemini => fetch_gemini(client).await,
    }
}

async fn fetch_all_sources(client: &Client) -> Result<Vec<SourcePrice>, String> {
    let (coingecko, coinbase, kraken, gemini) = try_join!(
        fetch_coingecko(client),
        fetch_coinbase(client),
        fetch_kraken(client),
        fetch_gemini(client)
    )?;

    Ok(vec![coingecko, coinbase, kraken, gemini])
}

fn merge_snapshot_sources(
    latest_snapshot: Option<&SnapshotRecord>,
    refreshed_price: SourcePrice,
) -> Vec<SourcePrice> {
    let mut source_prices = HashMap::new();

    if let Some(snapshot) = latest_snapshot {
        for source in &snapshot.sources {
            source_prices.insert(source.source.clone(), source.price_usd);
        }
    }

    source_prices.insert(refreshed_price.source.clone(), refreshed_price.price_usd);
    ordered_sources(source_prices)
}

fn ordered_sources(mut source_prices: HashMap<String, f64>) -> Vec<SourcePrice> {
    let mut sources = Vec::new();

    for source in UpstreamSource::ALL {
        if let Some(price_usd) = source_prices.remove(source.name()) {
            sources.push(SourcePrice {
                source: source.name().to_string(),
                price_usd,
            });
        }
    }

    let mut extras: Vec<_> = source_prices
        .into_iter()
        .map(|(source, price_usd)| SourcePrice { source, price_usd })
        .collect();
    extras.sort_by(|left, right| left.source.cmp(&right.source));
    sources.extend(extras);

    sources
}

fn summarize_prices(sources: &[SourcePrice]) -> (Option<f64>, Option<f64>) {
    if sources.is_empty() {
        return (None, None);
    }

    let total: f64 = sources.iter().map(|price| price.price_usd).sum();
    let average = total / sources.len() as f64;
    let min = sources
        .iter()
        .map(|price| price.price_usd)
        .fold(f64::INFINITY, f64::min);
    let max = sources
        .iter()
        .map(|price| price.price_usd)
        .fold(f64::NEG_INFINITY, f64::max);

    (Some(average), Some(max - min))
}

#[cfg(test)]
mod tests {
    use super::UpstreamSource;

    #[test]
    fn upstream_sources_rotate_in_round_robin_order() {
        assert_eq!(UpstreamSource::next_source(None), UpstreamSource::CoinGecko);
        assert_eq!(
            UpstreamSource::next_source(Some("CoinGecko")),
            UpstreamSource::Coinbase
        );
        assert_eq!(
            UpstreamSource::next_source(Some("Coinbase")),
            UpstreamSource::Kraken
        );
        assert_eq!(
            UpstreamSource::next_source(Some("Kraken")),
            UpstreamSource::Gemini
        );
        assert_eq!(
            UpstreamSource::next_source(Some("Gemini")),
            UpstreamSource::CoinGecko
        );
    }
}
