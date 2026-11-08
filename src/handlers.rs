use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::{get, post},
};
use tracing::warn;

use crate::{
    presence::{PresenceError, PresencePayload, PresenceResponse},
    refresh::cached_prices,
    state::AppState,
    storage::load_price_state,
    ui::INDEX_HTML,
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
        .route("/api/news", get(crate::news::bitcoin_news))
        .route("/api/presence", post(update_presence))
        .layer(axum::extract::DefaultBodyLimit::max(1024))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::limits::limit_requests,
        ))
        .layer(axum::middleware::from_fn(crate::ui::security_headers))
        .with_state(state)
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn update_presence(
    State(state): State<AppState>,
    Json(payload): Json<PresencePayload>,
) -> impl IntoResponse {
    match crate::presence::update_presence(&state, payload).await {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(PresenceError::InvalidSession) => (
            StatusCode::BAD_REQUEST,
            Json(PresenceResponse {
                active_viewers: 0,
                price: None,
            }),
        )
            .into_response(),
        Err(PresenceError::CapacityReached { active_viewers }) => {
            tracing::debug!(active_viewers, "viewer capacity reached");
            (StatusCode::TOO_MANY_REQUESTS, [("retry-after", "15")], Json(serde_json::json!({
                "active_viewers": active_viewers, "error": "Viewer capacity reached; retry later."
            }))).into_response()
        }
    }
}

async fn btc_prices(State(state): State<AppState>) -> impl IntoResponse {
    let (status, price) = cached_prices(&state).await;
    (status, Json(price))
}

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    match load_price_state(state.db_path.clone()).await {
        Ok(_) => (StatusCode::OK, Json(serde_json::json!({"status": "ok"}))),
        Err(error) => {
            warn!(error = %error, "health database read failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"status": "unavailable"})),
            )
        }
    }
}
