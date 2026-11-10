use reqwest::Client;
use std::{path::PathBuf, sync::Arc};
use tokio::sync::Mutex;

use crate::{
    config::{RuntimeConfig, UpstreamEndpoints},
    limits::RequestLimits,
    presence::Presence,
    refresh::PriceRefresh,
    util::{Clock, SystemClock},
};

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) config: RuntimeConfig,
    pub(crate) limits: Arc<RequestLimits>,
    pub(crate) client: Client,
    pub(crate) news: Arc<Mutex<crate::news::FeedCache>>,
    pub(crate) db_path: PathBuf,
    pub(crate) endpoints: Arc<UpstreamEndpoints>,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) presence: Arc<Presence>,
    pub(crate) refresh: Arc<PriceRefresh>,
}

impl AppState {
    pub(crate) fn new(client: Client, db_path: PathBuf) -> Self {
        Self::with_dependencies(
            client,
            db_path,
            UpstreamEndpoints::default(),
            Arc::new(SystemClock),
        )
    }

    pub(crate) fn with_config(mut self, config: RuntimeConfig) -> Self {
        self.limits = Arc::new(RequestLimits::new(&config, self.clock.now_monotonic()));
        self.config = config;
        self
    }

    pub(crate) fn with_dependencies(
        client: Client,
        db_path: PathBuf,
        endpoints: UpstreamEndpoints,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let config = RuntimeConfig::default();
        Self {
            limits: Arc::new(RequestLimits::new(&config, clock.now_monotonic())),
            config,
            client,
            news: Arc::new(Mutex::new(crate::news::FeedCache::default())),
            db_path,
            endpoints: Arc::new(endpoints),
            clock,
            presence: Arc::new(Presence::default()),
            refresh: Arc::new(PriceRefresh::default()),
        }
    }
}
