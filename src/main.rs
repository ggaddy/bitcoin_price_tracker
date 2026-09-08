mod app;
mod config;
mod errors;
mod freshness;
mod handlers;
mod lifecycle;
mod limits;
mod models;
mod presence;
mod pricing;
mod refresh;
mod state;
mod storage;
mod ui;
mod util;

use app::router;
use config::{RuntimeConfig, database_path, upstream_client_builder};
use state::AppState;
use std::{process::ExitCode, time::Duration};
use storage::init_db;

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "bitcoin_price_tracker=info".into()),
        )
        .init();
    let config = match RuntimeConfig::from_env() {
        Ok(config) => config,
        Err(error) => {
            tracing::error!(%error, "invalid configuration");
            return ExitCode::FAILURE;
        }
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("create async runtime");
    let result = runtime.block_on(async {
        if std::env::args().nth(1).as_deref() == Some("--healthcheck") {
            let mut address = config.bind_address;
            if address.ip().is_unspecified() {
                address.set_ip(if address.is_ipv4() { std::net::Ipv4Addr::LOCALHOST.into() } else { std::net::Ipv6Addr::LOCALHOST.into() });
            }
            let response = upstream_client_builder().no_proxy().build().map_err(|error| error.to_string())?
                .get(format!("http://{address}/health")).send().await.map_err(|error| error.to_string())?;
            return if response.status().is_success() { Ok(()) } else { Err("health endpoint is unavailable".into()) };
        }
        let client = upstream_client_builder().build().map_err(|error| format!("HTTP client setup failed: {error}"))?;
        let db_path = database_path();
        init_db(db_path.clone()).await.map_err(|error| format!("SQLite initialization failed: {error}"))?;
        let listener = tokio::net::TcpListener::bind(config.bind_address).await
            .map_err(|error| format!("failed to bind {}: {error}", config.bind_address))?;
        tracing::info!(address = %config.bind_address, max_viewers = config.max_viewers, "server listening");
        let grace = Duration::from_secs(config.shutdown_seconds);
        let state = AppState::new(client, db_path).with_config(config);
        lifecycle::serve_until(listener, router(state), lifecycle::shutdown_signal(), grace).await
    });
    // Bound cleanup of any blocking SQLite jobs after the HTTP drain deadline.
    runtime.shutdown_timeout(Duration::from_secs(5));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(%error, "application stopped");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod selenium_tests;

#[cfg(test)]
mod test_support;

#[cfg(test)]
mod api_tests;

#[cfg(test)]
mod operations_tests;
