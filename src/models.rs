pub(crate) mod source_contract;
pub(crate) use source_contract::PriceResponse;

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
