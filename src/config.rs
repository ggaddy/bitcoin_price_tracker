use std::path::PathBuf;

pub(crate) const REFRESH_INTERVAL_SECONDS: i64 = 10;
pub(crate) const VIEWER_TTL_SECONDS: i64 = 15;

pub(crate) fn database_path() -> PathBuf {
    std::env::var("DATABASE_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("data/bitcoin_prices.db"))
}
