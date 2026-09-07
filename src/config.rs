use std::{path::PathBuf, time::Duration};

use reqwest::{Client, ClientBuilder};

pub(crate) const REFRESH_INTERVAL_SECONDS: i64 = 10;
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
            coinbase: "https://api.coinbase.com/v2/prices/spot?currency=USD".to_string(),
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
