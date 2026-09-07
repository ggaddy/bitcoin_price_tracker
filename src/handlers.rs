use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse},
};
use std::sync::atomic::Ordering;
use tracing::warn;

use crate::{
    config::SOURCE_MAX_AGE_SECONDS,
    freshness::evaluate_quotes,
    models::{PresencePayload, PresenceResponse, PriceResponse},
    models::{StoredPriceState, source_contract::PriceStatus},
    presence::{
        active_viewer_count, apply_presence_update, is_valid_session_id, refresh_skip_reason,
        snapshot_age_seconds,
    },
    pricing::refresh_snapshot,
    state::AppState,
    storage::load_price_state,
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
    let mut refresh_errors = Vec::new();

    if active_viewers == 0 {
        refresh_skipped_reason = Some(
            "Refresh skipped: no active viewers are currently tracked. Open the dashboard tab to resume updates."
                .to_string(),
        );
    } else if let Ok(mut coordinator) = state.refresh.try_lock() {
        match load_price_state(state.db_path.clone()).await {
            Ok(stored) => {
                let snapshot = stored.snapshot;
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
                            let outcome = refresh_snapshot(&state, snapshot, &plan).await;
                            coordinator.complete(
                                &plan,
                                &outcome.failures,
                                state.clock.now_monotonic(),
                                state.clock.now_unix(),
                            );
                            for error in &outcome.failures {
                                warn!(error = ?error, "provider refresh failed");
                            }
                            match outcome.stored {
                                Ok(stored) => refresh_succeeded = stored,
                                Err(error) => {
                                    warn!(error = ?error, "price refresh failed");
                                    refresh_errors.push(error.to_string());
                                }
                            }
                            // A saved snapshot already contains these warnings. Failed or
                            // skipped writes report them on this response without rewriting data.
                            if !refresh_succeeded {
                                refresh_errors
                                    .extend(outcome.failures.iter().map(ToString::to_string));
                            }
                        }
                        Err(reason) => refresh_skipped_reason = Some(reason.to_string()),
                    }
                }
            }
            Err(error) => {
                warn!(error = %error, "failed to inspect SQLite before refresh");
                refresh_errors
                    .push("Failed to inspect stored price data before refresh".to_string());
            }
        }
    } else {
        refresh_skipped_reason =
            Some("Refresh skipped: another refresh is in progress.".to_string());
    }

    let stored_snapshot = load_price_state(state.db_path.clone()).await;
    let active_viewers = active_viewer_count(&state).await;
    let response_now = state.clock.now_unix();
    let (stored, read_failed) = match stored_snapshot {
        Ok(stored) => (stored, false),
        Err(error) => {
            warn!(error = %error, "failed to load SQLite price state");
            (
                StoredPriceState {
                    snapshot: None,
                    provider_health: Vec::new(),
                },
                true,
            )
        }
    };
    let fetched_at_unix = stored
        .snapshot
        .as_ref()
        .map_or(0, |snapshot| snapshot.fetched_at_unix);
    let fetched_age_seconds = stored
        .snapshot
        .as_ref()
        .and_then(|snapshot| snapshot_age_seconds(snapshot, response_now));
    let had_snapshot = stored.snapshot.is_some();
    let (quotes, mut warnings) = stored.snapshot.map_or_else(
        || {
            (
                Vec::new(),
                vec!["No stored BTC price snapshot is available yet.".to_string()],
            )
        },
        |snapshot| (snapshot.sources, snapshot.warnings),
    );
    let mut evaluated = evaluate_quotes(quotes, &stored.provider_health, response_now);
    if !refresh_errors.is_empty() && evaluated.status == PriceStatus::Live {
        evaluated.status = PriceStatus::Degraded;
    }
    warnings.splice(
        0..0,
        refresh_errors.into_iter().map(|error| {
            if had_snapshot {
                format!("Refresh failed; serving latest SQLite snapshot: {error}")
            } else {
                error
            }
        }),
    );
    let status = if read_failed {
        warnings = vec!["Failed to load stored price data.".to_string()];
        StatusCode::INTERNAL_SERVER_ERROR
    } else if evaluated.sources.is_empty() {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    };
    (
        status,
        Json(PriceResponse {
            symbol: "BTC",
            currency: "USD",
            sources: evaluated.sources,
            provider_health: stored.provider_health,
            average_price: evaluated.average_price,
            spread: evaluated.spread,
            status: evaluated.status,
            coverage: evaluated.coverage,
            source_max_age_seconds: SOURCE_MAX_AGE_SECONDS,
            evaluated_at_unix: response_now,
            fetched_at_unix,
            fetched_age_seconds,
            warnings,
            refresh_succeeded: refresh_succeeded && !read_failed,
            stale: evaluated.status != PriceStatus::Live,
            active_viewers,
            refresh_skipped_reason,
        }),
    )
}
