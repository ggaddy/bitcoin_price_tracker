mod app;
mod config;
mod errors;
mod handlers;
mod models;
mod presence;
mod pricing;
mod state;
mod storage;
mod ui;
mod util;

use app::router;
use state::AppState;
use tracing::info;

use crate::{
    config::{database_path, upstream_client_builder},
    storage::init_db,
};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG")
                .unwrap_or_else(|_| "bitcoin_price_tracker=debug,axum=info".into()),
        )
        .init();

    let client = upstream_client_builder()
        .build()
        .expect("failed to build HTTP client");

    let db_path = database_path();
    init_db(db_path.clone())
        .await
        .expect("failed to initialize SQLite database");

    let state = AppState::new(client, db_path);

    let app = router(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000")
        .await
        .expect("failed to bind to 0.0.0.0:3000");
    info!("server listening on http://0.0.0.0:3000");

    axum::serve(listener, app)
        .await
        .expect("server exited unexpectedly");
}

#[cfg(test)]
mod selenium_tests;

#[cfg(test)]
mod test_support;

#[cfg(test)]
mod api_tests;
