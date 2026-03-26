use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize)]
pub(crate) struct SourcePrice {
    pub(crate) source: String,
    pub(crate) price_usd: f64,
}

#[derive(Serialize)]
pub(crate) struct PriceResponse {
    pub(crate) symbol: &'static str,
    pub(crate) currency: &'static str,
    pub(crate) sources: Vec<SourcePrice>,
    pub(crate) average_price: Option<f64>,
    pub(crate) spread: Option<f64>,
    pub(crate) fetched_at_unix: i64,
    pub(crate) fetched_age_seconds: Option<i64>,
    pub(crate) warnings: Vec<String>,
    pub(crate) refresh_succeeded: bool,
    pub(crate) stale: bool,
    pub(crate) active_viewers: usize,
    pub(crate) refresh_skipped_reason: Option<String>,
}

#[derive(Clone)]
pub(crate) struct SnapshotRecord {
    pub(crate) fetched_at_unix: i64,
    pub(crate) sources: Vec<SourcePrice>,
    pub(crate) average_price: Option<f64>,
    pub(crate) spread: Option<f64>,
    pub(crate) warnings: Vec<String>,
    pub(crate) refreshed_source: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct PresencePayload {
    pub(crate) session_id: String,
    pub(crate) active: bool,
}

#[derive(Serialize)]
pub(crate) struct PresenceResponse {
    pub(crate) active_viewers: usize,
}
