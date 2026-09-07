use axum::{
    Router,
    routing::{get, post},
};

use crate::{
    handlers::{btc_prices, index, update_presence},
    state::AppState,
};

pub(crate) fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route(
            "/assets/cybercore-0.3.0.min.css",
            get(crate::ui::cybercore_css),
        )
        .route("/api/price", get(btc_prices))
        .route("/api/presence", post(update_presence))
        .with_state(state)
}
