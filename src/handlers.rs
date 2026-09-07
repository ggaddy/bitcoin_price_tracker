use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse},
};
use std::sync::atomic::Ordering;
use tracing::warn;

use crate::{
    config::REFRESH_INTERVAL_SECONDS,
    models::{PresencePayload, PresenceResponse, PriceResponse},
    presence::{
        active_viewer_count, apply_presence_update, is_valid_session_id, refresh_skip_reason,
        snapshot_age_seconds,
    },
    pricing::refresh_snapshot,
    state::AppState,
    storage::load_latest_snapshot,
    ui::INDEX_HTML,
};

pub(crate) async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

pub(crate) async fn update_presence(
    State(state): State<AppState>,
    Json(payload): Json<PresencePayload>,
) -> impl IntoResponse {
    if !is_valid_session_id(&payload.session_id) {
        return (
            StatusCode::BAD_REQUEST,
            Json(PresenceResponse { active_viewers: 0 }),
        );
    }

    let active_viewers = apply_presence_update(&state, payload).await;
    (StatusCode::OK, Json(PresenceResponse { active_viewers }))
}

pub(crate) async fn btc_prices(State(state): State<AppState>) -> impl IntoResponse {
    let active_viewers = active_viewer_count(&state).await;
    let mut refresh_succeeded = false;
    let mut refresh_skipped_reason = None;
    let mut refresh_error = None;

    if active_viewers == 0 {
        refresh_skipped_reason = Some(
            "Refresh skipped: no active viewers are currently tracked. Open the dashboard tab to resume updates."
                .to_string(),
        );
    } else if let Ok(mut coordinator) = state.refresh.try_lock() {
        match load_latest_snapshot(state.db_path.clone()).await {
            Ok(snapshot) => {
                let age = snapshot
                    .as_ref()
                    .and_then(|record| snapshot_age_seconds(record, state.clock.now_unix()));

                // The viewer lock is released before any provider I/O. Presence
                // never acquires the coordinator, so it can progress during a refresh.
                let current_viewers = active_viewer_count(&state).await;
                if let Some(reason) = refresh_skip_reason(current_viewers, age) {
                    refresh_skipped_reason = Some(reason);
                } else {
                    let generation = state.full_refresh_generation.load(Ordering::SeqCst);
                    match coordinator.begin(state.clock.now_monotonic(), generation) {
                        Ok(plan) => {
                            let result = refresh_snapshot(&state, snapshot, &plan).await;
                            coordinator.complete(&plan);
                            match result {
                                Ok(()) => refresh_succeeded = true,
                                Err(error) => {
                                    warn!(error = ?error, "price refresh failed");
                                    refresh_error = Some(error.to_string());
                                }
                            }
                        }
                        Err(reason) => refresh_skipped_reason = Some(reason.to_string()),
                    }
                }
            }
            Err(error) => {
                warn!(error = %error, "failed to inspect SQLite before refresh");
                refresh_error =
                    Some("Failed to inspect stored price data before refresh".to_string());
            }
        }
    } else {
        refresh_skipped_reason =
            Some("Refresh skipped: another refresh is in progress.".to_string());
    }

    let stored_snapshot = load_latest_snapshot(state.db_path.clone()).await;
    let active_viewers = active_viewer_count(&state).await;
    let response_now = state.clock.now_unix();
    match stored_snapshot {
        Ok(Some(snapshot)) => {
            let fetched_age_seconds = snapshot_age_seconds(&snapshot, response_now);
            let stale = fetched_age_seconds
                .map(|age| age >= REFRESH_INTERVAL_SECONDS)
                .unwrap_or(true);
            let mut warnings = snapshot.warnings.clone();

            if let Some(error) = refresh_error {
                warnings.insert(
                    0,
                    format!("Refresh failed; serving latest SQLite snapshot: {error}"),
                );
            }

            (
                StatusCode::OK,
                Json(PriceResponse {
                    symbol: "BTC",
                    currency: "USD",
                    sources: snapshot.sources,
                    average_price: snapshot.average_price,
                    spread: snapshot.spread,
                    fetched_at_unix: snapshot.fetched_at_unix,
                    fetched_age_seconds,
                    warnings,
                    refresh_succeeded,
                    stale,
                    active_viewers,
                    refresh_skipped_reason,
                }),
            )
        }
        Ok(None) => {
            let mut warnings = vec!["No stored BTC price snapshot is available yet.".to_string()];
            if let Some(error) = refresh_error {
                warnings.push(error);
            }

            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(PriceResponse {
                    symbol: "BTC",
                    currency: "USD",
                    sources: Vec::new(),
                    average_price: None,
                    spread: None,
                    fetched_at_unix: 0,
                    fetched_age_seconds: None,
                    warnings,
                    refresh_succeeded: false,
                    stale: true,
                    active_viewers,
                    refresh_skipped_reason,
                }),
            )
        }
        Err(error) => {
            warn!("failed to load SQLite snapshot: {error}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(PriceResponse {
                    symbol: "BTC",
                    currency: "USD",
                    sources: Vec::new(),
                    average_price: None,
                    spread: None,
                    fetched_at_unix: 0,
                    fetched_age_seconds: None,
                    warnings: vec!["Failed to load stored price data.".to_string()],
                    refresh_succeeded: false,
                    stale: true,
                    active_viewers,
                    refresh_skipped_reason,
                }),
            )
        }
    }
}
