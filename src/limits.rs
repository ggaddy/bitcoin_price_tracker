use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use crate::{config::RuntimeConfig, state::AppState};
use axum::{
    Json,
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use tokio::sync::Semaphore;

struct Bucket {
    tokens: f64,
    updated: Instant,
}
struct Gate {
    bucket: Mutex<Bucket>,
    rate: usize,
    slots: Arc<Semaphore>,
}
impl Gate {
    fn new(rate: usize, concurrency: usize, now: Instant) -> Self {
        Self {
            bucket: Mutex::new(Bucket {
                tokens: rate as f64,
                updated: now,
            }),
            rate,
            slots: Arc::new(Semaphore::new(concurrency)),
        }
    }
    fn allow(&self, now: Instant) -> bool {
        let mut bucket = self
            .bucket
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        bucket.tokens = (bucket.tokens
            + now.saturating_duration_since(bucket.updated).as_secs_f64() * self.rate as f64)
            .min(self.rate as f64);
        bucket.updated = now;
        if bucket.tokens < 1.0 {
            return false;
        }
        bucket.tokens -= 1.0;
        true
    }
}

pub(crate) struct RequestLimits {
    price: Gate,
    presence: Gate,
    health: Gate,
    assets: Gate,
}
impl RequestLimits {
    pub(crate) fn new(config: &RuntimeConfig, now: Instant) -> Self {
        Self {
            price: Gate::new(config.requests_per_second, config.concurrency, now),
            presence: Gate::new(config.requests_per_second, config.concurrency, now),
            health: Gate::new(10, 4, now),
            assets: Gate::new(config.requests_per_second, config.concurrency, now),
        }
    }
}

pub(crate) async fn limit_requests(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    // Fixed route groups reserve independent capacity for health and cached price
    // reads. No client/IP keys, forwarded headers, or unbounded limiter maps.
    let gate = match request.uri().path() {
        "/api/price" => &state.limits.price,
        "/api/presence" => &state.limits.presence,
        "/health" => &state.limits.health,
        _ => &state.limits.assets,
    };
    if !gate.allow(state.clock.now_monotonic()) {
        tracing::debug!("request rate limit reached");
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [("retry-after", "1")],
            Json(serde_json::json!({"error": "Request rate limit reached; retry later."})),
        )
            .into_response();
    }
    let Ok(_permit) = gate.slots.clone().try_acquire_owned() else {
        tracing::debug!("request concurrency limit reached");
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [("retry-after", "1")],
            Json(serde_json::json!({"error": "Server busy; retry later."})),
        )
            .into_response();
    };
    match tokio::time::timeout(Duration::from_secs(20), next.run(request)).await {
        Ok(response) => response,
        Err(_) => (
            StatusCode::REQUEST_TIMEOUT,
            Json(serde_json::json!({"error": "Request deadline exceeded."})),
        )
            .into_response(),
    }
}
