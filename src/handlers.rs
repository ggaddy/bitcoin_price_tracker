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
    state::{AppState, PriceRefreshReport},
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
            Json(PresenceResponse {
                active_viewers: 0,
                price: None,
            }),
        )
            .into_response();
    }

    // Only an admitted active viewer can ask for the startup snapshot/refresh.
    // Ordinary heartbeats and inactive beacons remain lightweight.
    let include_price = payload.active && payload.include_price;
    match apply_presence_update(&state, payload).await {
        Ok(active_viewers) => {
            let price = if include_price {
                Some(cached_prices(&state).await.1)
            } else {
                None
            };
            (
                StatusCode::OK,
                Json(PresenceResponse {
                    active_viewers,
                    price,
                }),
            )
                .into_response()
        }
        Err(active_viewers) => {
            tracing::debug!(active_viewers, "viewer capacity reached");
            (StatusCode::TOO_MANY_REQUESTS, [("retry-after", "15")], Json(serde_json::json!({
                "active_viewers": active_viewers, "error": "Viewer capacity reached; retry later."
            }))).into_response()
        }
    }
}

pub(crate) async fn btc_prices(State(state): State<AppState>) -> impl IntoResponse {
    let (status, price) = cached_prices(&state).await;
    (status, Json(price))
}

async fn cached_prices(state: &AppState) -> (StatusCode, PriceResponse) {
    // Reserve refresh ownership before inspecting SQLite. Holding this guard
    // ensures the snapshot handed to a refresh cannot race another writer.
    // Readers never wait for ownership, nor for any provider network request.
    let owner = state.refresh.clone().try_lock_owned().ok();
    let stored_snapshot = load_price_state(state.db_path.clone()).await;
    let active_viewers = active_viewer_count(state).await;
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
    let report = state.price_refresh_report.lock().await.clone();
    let mut refreshing = owner.is_none();
    let refresh_skipped_reason;
    if read_failed {
        refresh_skipped_reason =
            Some("Refresh skipped: stored price data could not be read.".into());
    } else if state.stopping.load(Ordering::SeqCst) {
        refresh_skipped_reason = Some("Refresh skipped: server is shutting down.".into());
    } else if let Some(reason) = refresh_skip_reason(active_viewers, fetched_age_seconds) {
        refresh_skipped_reason = Some(reason);
    } else if let Some(mut coordinator) = owner {
        let generation = state.full_refresh_generation.load(Ordering::SeqCst);
        match coordinator.begin(state.clock.now_monotonic(), generation) {
            Ok(plan) => {
                refreshing = true;
                refresh_skipped_reason = Some("Refresh scheduled in the background.".into());
                let state = state.clone();
                let snapshot = stored.snapshot.clone();
                tokio::spawn(async move {
                    // A client leaving/cancelling never cancels dispatched work.
                    // Recheck admission immediately before provider I/O begins.
                    if state.stopping.load(Ordering::SeqCst)
                        || active_viewer_count(&state).await == 0
                    {
                        return;
                    }
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
                    let mut report = PriceRefreshReport::default();
                    match outcome.stored {
                        Ok(saved) => report.succeeded = saved,
                        Err(error) => {
                            warn!(error = ?error, "price refresh failed");
                            report.errors.push(error.to_string());
                        }
                    }
                    // Partial-success warnings are already in the saved snapshot.
                    if !report.succeeded {
                        report
                            .errors
                            .extend(outcome.failures.iter().map(ToString::to_string));
                    }
                    *state.price_refresh_report.lock().await = report;
                    // Keep ownership through the SQLite write and report update.
                    drop(coordinator);
                });
            }
            Err(reason) => refresh_skipped_reason = Some(reason.into()),
        }
    } else {
        refresh_skipped_reason = Some("Refresh skipped: another refresh is in progress.".into());
    }
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
    if !report.errors.is_empty() && evaluated.status == PriceStatus::Live {
        evaluated.status = PriceStatus::Degraded;
    }
    warnings.splice(
        0..0,
        report.errors.into_iter().map(|error| {
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
    tracing::debug!(http_status = status.as_u16(), refreshing,
        skip_reason = ?refresh_skipped_reason, price_status = ?evaluated.status,
        fresh_sources = evaluated.coverage.fresh_source_count, active_viewers, "price response");
    (
        status,
        PriceResponse {
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
            // Latest completed attempt in this process, independent of this read.
            refresh_succeeded: report.succeeded && !read_failed,
            refreshing,
            stale: evaluated.status != PriceStatus::Live,
            active_viewers,
            refresh_skipped_reason,
        },
    )
}

pub(crate) async fn health(State(state): State<AppState>) -> impl IntoResponse {
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
