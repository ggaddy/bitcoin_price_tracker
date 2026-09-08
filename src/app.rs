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
        .route("/assets/dashboard.css", get(crate::ui::dashboard_css))
        .route("/api/price", get(btc_prices))
        .route("/api/presence", post(update_presence))
        .layer(axum::extract::DefaultBodyLimit::max(1024))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::limits::limit_requests,
        ))
        .layer(axum::middleware::from_fn(crate::ui::security_headers))
        .with_state(state)
}
