use reqwest::Client;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, atomic::AtomicU64},
    time::Instant,
};
use tokio::sync::Mutex;

use crate::{
    config::{RuntimeConfig, UpstreamEndpoints},
    limits::RequestLimits,
    refresh::RefreshCoordinator,
    util::{Clock, SystemClock},
};

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) config: RuntimeConfig,
    pub(crate) limits: Arc<RequestLimits>,
    pub(crate) client: Client,
    pub(crate) db_path: PathBuf,
    pub(crate) endpoints: Arc<UpstreamEndpoints>,
    pub(crate) clock: Arc<dyn Clock>,
    // If both locks are needed, acquire refresh before viewers. Presence only
    // acquires viewers, and that guard must never span network or database I/O.
    pub(crate) viewers: Arc<Mutex<HashMap<String, Instant>>>,
    pub(crate) refresh: Arc<Mutex<RefreshCoordinator>>,
    pub(crate) full_refresh_generation: Arc<AtomicU64>,
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
            db_path,
            endpoints: Arc::new(endpoints),
            clock,
            viewers: Arc::new(Mutex::new(HashMap::new())),
            refresh: Arc::new(Mutex::new(RefreshCoordinator::default())),
            full_refresh_generation: Arc::new(AtomicU64::new(0)),
        }
    }
}
