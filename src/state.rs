use reqwest::Client;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
};
use tokio::sync::Mutex;

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) client: Client,
    pub(crate) db_path: PathBuf,
    pub(crate) viewers: Arc<Mutex<HashMap<String, i64>>>,
    pub(crate) refresh_lock: Arc<Mutex<()>>,
    pub(crate) full_refresh_pending: Arc<AtomicBool>,
}

impl AppState {
    pub(crate) fn new(client: Client, db_path: PathBuf) -> Self {
        Self {
            client,
            db_path,
            viewers: Arc::new(Mutex::new(HashMap::new())),
            refresh_lock: Arc::new(Mutex::new(())),
            full_refresh_pending: Arc::new(AtomicBool::new(false)),
        }
    }
}
