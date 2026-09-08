use std::{path::PathBuf, time::Duration};

use reqwest::{Client, ClientBuilder};

pub(crate) const REFRESH_INTERVAL_SECONDS: i64 = 10;
pub(crate) const SOURCE_MAX_AGE_SECONDS: u64 = 90;
pub(crate) const VIEWER_TTL_SECONDS: i64 = 15;
pub(crate) const UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
pub(crate) const UPSTREAM_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) fn upstream_client_builder() -> ClientBuilder {
    Client::builder()
        .user_agent("bitcoin-price-tracker/0.1")
        .connect_timeout(UPSTREAM_CONNECT_TIMEOUT)
        .timeout(UPSTREAM_REQUEST_TIMEOUT)
}

pub(crate) struct UpstreamEndpoints {
    pub(crate) coingecko: String,
    pub(crate) coinbase: String,
    pub(crate) kraken: String,
    pub(crate) gemini: String,
}

impl Default for UpstreamEndpoints {
    fn default() -> Self {
        Self {
            coingecko:
                "https://api.coingecko.com/api/v3/simple/price?ids=bitcoin&vs_currencies=usd"
                    .to_string(),
            coinbase: "https://api.coinbase.com/v2/prices/BTC-USD/spot".to_string(),
            kraken: "https://api.kraken.com/0/public/Ticker?pair=XBTUSD".to_string(),
            gemini: "https://api.gemini.com/v2/ticker/btcusd".to_string(),
        }
    }
}

pub(crate) fn database_path() -> PathBuf {
    std::env::var("DATABASE_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("data/bitcoin_prices.db"))
}

#[derive(Clone, Debug)]
pub(crate) struct RuntimeConfig {
    pub(crate) bind_address: std::net::SocketAddr,
    pub(crate) max_viewers: usize,
    pub(crate) requests_per_second: usize,
    pub(crate) concurrency: usize,
    pub(crate) shutdown_seconds: u64,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            bind_address: "0.0.0.0:3000".parse().unwrap(),
            max_viewers: 1000,
            requests_per_second: 100,
            concurrency: 64,
            shutdown_seconds: 25,
        }
    }
}

impl RuntimeConfig {
    pub(crate) fn from_env() -> Result<Self, String> {
        Self::parse(|key| std::env::var(key).ok())
    }

    fn parse(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        fn positive(
            get: &impl Fn(&str) -> Option<String>,
            key: &str,
            default: usize,
            max: usize,
        ) -> Result<usize, String> {
            get(key).map_or(Ok(default), |value| {
                value
                    .parse::<usize>()
                    .ok()
                    .filter(|value| (1..=max).contains(value))
                    .ok_or_else(|| format!("{key} must be an integer from 1 to {max}"))
            })
        }
        let defaults = Self::default();
        let bind_address = get("BIND_ADDRESS").map_or(Ok(defaults.bind_address), |value| {
            value
                .parse::<std::net::SocketAddr>()
                .ok()
                .filter(|address| address.port() != 0)
                .ok_or_else(|| {
                    "BIND_ADDRESS must be an IP address and nonzero port, for example 0.0.0.0:3000"
                        .to_string()
                })
        })?;
        Ok(Self {
            bind_address,
            max_viewers: positive(&get, "MAX_VIEWERS", defaults.max_viewers, 100_000)?,
            requests_per_second: positive(
                &get,
                "REQUESTS_PER_SECOND",
                defaults.requests_per_second,
                10_000,
            )?,
            concurrency: positive(&get, "REQUEST_CONCURRENCY", defaults.concurrency, 1024)?,
            shutdown_seconds: positive(
                &get,
                "SHUTDOWN_SECONDS",
                defaults.shutdown_seconds as usize,
                120,
            )? as u64,
        })
    }
}

#[cfg(test)]
mod runtime_tests {
    use super::RuntimeConfig;

    #[test]
    fn operational_config_validates_bounds_and_addresses() {
        let defaults = RuntimeConfig::parse(|_| None).unwrap();
        assert_eq!(defaults.max_viewers, 1000);
        for key in [
            "MAX_VIEWERS",
            "REQUESTS_PER_SECOND",
            "REQUEST_CONCURRENCY",
            "SHUTDOWN_SECONDS",
        ] {
            for value in ["0", "-1", "bad", "999999999999999999999999"] {
                assert!(RuntimeConfig::parse(|name| (name == key).then(|| value.into())).is_err());
            }
        }
        for address in ["localhost:3000", "0.0.0.0:0", "bad"] {
            assert!(
                RuntimeConfig::parse(|key| (key == "BIND_ADDRESS").then(|| address.into()))
                    .is_err()
            );
        }
        let ipv6 = RuntimeConfig::parse(|key| (key == "BIND_ADDRESS").then(|| "[::1]:4000".into()))
            .unwrap();
        assert_eq!(ipv6.bind_address.port(), 4000);
    }
}
