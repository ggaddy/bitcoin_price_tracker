use serde::{Deserialize, Serialize};

pub(crate) mod source_contract;
pub(crate) use source_contract::PriceResponse;

#[derive(Clone, Serialize)]
pub(crate) struct SourcePrice {
    pub(crate) source: String,
    pub(crate) price_usd: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SnapshotRecord {
    pub(crate) fetched_at_unix: i64,
    pub(crate) sources: Vec<source_contract::StoredQuote>,
    pub(crate) average_price: Option<f64>,
    pub(crate) spread: Option<f64>,
    pub(crate) warnings: Vec<String>,
    pub(crate) refreshed_source: Option<String>,
}

#[derive(Debug, PartialEq)]
pub(crate) struct StoredPriceState {
    pub(crate) snapshot: Option<SnapshotRecord>,
    pub(crate) provider_health: Vec<source_contract::ProviderHealth>,
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
