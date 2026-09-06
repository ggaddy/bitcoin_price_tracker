use reqwest::Client;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
};
use tokio::sync::Mutex;

use crate::{
    config::UpstreamEndpoints,
    util::{Clock, SystemClock},
};

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) client: Client,
    pub(crate) db_path: PathBuf,
    pub(crate) endpoints: Arc<UpstreamEndpoints>,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) viewers: Arc<Mutex<HashMap<String, i64>>>,
    pub(crate) refresh_lock: Arc<Mutex<()>>,
    pub(crate) full_refresh_pending: Arc<AtomicBool>,
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

    pub(crate) fn with_dependencies(
        client: Client,
        db_path: PathBuf,
        endpoints: UpstreamEndpoints,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            client,
            db_path,
            endpoints: Arc::new(endpoints),
            clock,
            viewers: Arc::new(Mutex::new(HashMap::new())),
            refresh_lock: Arc::new(Mutex::new(())),
            full_refresh_pending: Arc::new(AtomicBool::new(false)),
        }
    }
}
