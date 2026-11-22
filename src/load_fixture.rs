//! Opt-in process fixture for scripts/load_test.py; absent from production builds.
use std::{path::PathBuf, sync::Arc};

use crate::{
    config::{RuntimeConfig, UpstreamEndpoints, upstream_client_builder},
    lifecycle::{serve_tracker_until, shutdown_signal},
    state::AppState,
    storage::init_db,
    util::SystemClock,
};

#[tokio::test(flavor = "multi_thread")]
#[ignore = "disposable local server; run through python3 scripts/load_test.py"]
async fn serve() {
    // No production endpoint or persistent database can be supplied to this fixture.
    let directory = PathBuf::from(std::env::var("BTC_LOAD_DIRECTORY").expect("harness directory"));
    assert!(directory.is_dir());
    let upstream: std::net::SocketAddr = std::env::var("BTC_LOAD_UPSTREAM")
        .expect("local mock address")
        .parse()
        .unwrap();
    assert!(upstream.ip().is_loopback());
    let endpoint = |path| format!("http://{upstream}/{path}");
    let endpoints = UpstreamEndpoints {
        coingecko: endpoint("coingecko"),
        coinbase: endpoint("coinbase"),
        kraken: endpoint("kraken"),
        gemini: endpoint("gemini"),
        news: endpoint("news"),
    };
    let client = upstream_client_builder()
        .https_only(false)
        .no_proxy()
        .build()
        .unwrap();
    let database = directory.join("prices.db");
    assert!(!database.exists(), "fixture requires a new database");
    init_db(database.clone()).await.unwrap();
    let state = AppState::with_dependencies(client, database, endpoints, Arc::new(SystemClock));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = RuntimeConfig::default();
    let ready = serde_json::json!({
        "address": listener.local_addr().unwrap().to_string(),
        "max_viewers": config.max_viewers,
        "requests_per_second": config.requests_per_second,
        "request_concurrency": config.concurrency,
        "max_connections": config.max_connections,
    });
    std::fs::write(directory.join("ready.json"), ready.to_string()).unwrap();
    serve_tracker_until(listener, state, shutdown_signal())
        .await
        .unwrap();
}
