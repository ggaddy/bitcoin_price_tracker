use axum::{
    Router,
    routing::{get, post},
};

use crate::{
    handlers::{btc_prices, health, index, update_presence},
    state::AppState,
};

pub(crate) fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/health", get(health))
        .route(
            "/assets/cybercore-0.3.0.min.css",
            get(crate::ui::cybercore_css),
        )
        .route("/assets/dashboard.js", get(crate::ui::dashboard_js))
        .route("/api/price", get(btc_prices))
        .route("/api/presence", post(update_presence))
        .layer(axum::extract::DefaultBodyLimit::max(1024))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::limits::limit_requests,
        ))
        .with_state(state)
}
