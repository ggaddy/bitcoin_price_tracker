use axum::http::StatusCode;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, UNIX_EPOCH},
};
use tokio::sync::Mutex;
use tracing::warn;

use crate::{
    config::{REFRESH_INTERVAL_SECONDS, SOURCE_MAX_AGE_SECONDS},
    errors::{ProviderError, ProviderErrorKind, RetryAfter},
    freshness::evaluate_quotes,
    models::{PriceResponse, PriceStatus},
    presence::active_viewer_count,
    pricing::{UpstreamSource, refresh_snapshot},
    state::AppState,
    storage::{SnapshotRecord, StoredPriceState, load_price_state},
};

#[derive(Default)]
pub(crate) struct PriceRefresh {
    coordinator: Arc<Mutex<RefreshCoordinator>>,
    report: Mutex<PriceRefreshReport>,
    stopping: AtomicBool,
}

#[derive(Clone, Default)]
struct PriceRefreshReport {
    succeeded: bool,
    // Client-safe failures not already saved in the snapshot (e.g. write failure).
    errors: Vec<String>,
}

impl PriceRefresh {
    pub(crate) fn stop(&self) {
        self.stopping.store(true, Ordering::SeqCst);
    }

    pub(crate) fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    pub(crate) async fn drain_until(&self, deadline: tokio::time::Instant) -> Result<(), String> {
        // Ownership spans provider I/O, the SQLite commit and the report update.
        let guard = tokio::time::timeout_at(deadline, self.coordinator.lock())
            .await
            .map_err(|_| "background price refresh shutdown deadline exceeded".to_string())?;
        drop(guard);
        Ok(())
    }
}

pub(crate) async fn cached_prices(state: &AppState) -> (StatusCode, PriceResponse) {
    // Reserve refresh ownership before inspecting SQLite so concurrent readers
    // cannot dispatch overlapping attempts. Readers never wait for ownership
    // or for provider network requests; storage owns merging each result.
    let owner = state.refresh.coordinator.clone().try_lock_owned().ok();
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
    let report = state.refresh.report.lock().await.clone();
    let mut refreshing = owner.is_none();
    let refresh_skipped_reason;
    if read_failed {
        refresh_skipped_reason =
            Some("Refresh skipped: stored price data could not be read.".into());
    } else if state.refresh.is_stopping() {
        refresh_skipped_reason = Some("Refresh skipped: server is shutting down.".into());
    } else if let Some(reason) = refresh_skip_reason(active_viewers, fetched_age_seconds) {
        refresh_skipped_reason = Some(reason);
    } else if let Some(mut coordinator) = owner {
        let generation = state.presence.generation();
        match coordinator.begin(state.clock.now_monotonic(), generation) {
            Ok(plan) => {
                refreshing = true;
                refresh_skipped_reason = Some("Refresh scheduled in the background.".into());
                let state = state.clone();
                tokio::spawn(async move {
                    // A client leaving/cancelling never cancels dispatched work.
                    // Recheck admission immediately before provider I/O begins.
                    if state.refresh.is_stopping() || active_viewer_count(&state).await == 0 {
                        return;
                    }
                    let outcome = refresh_snapshot(&state, &plan.sources).await;
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
                    *state.refresh.report.lock().await = report;
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

fn snapshot_age_seconds(snapshot: &SnapshotRecord, now: i64) -> Option<i64> {
    // Future timestamps have unknown age; they must not suppress refresh forever
    // after the system clock moves backward or a database is copied from elsewhere.
    now.checked_sub(snapshot.fetched_at_unix)
        .filter(|age| *age >= 0)
}

fn refresh_skip_reason(active_viewers: usize, snapshot_age: Option<i64>) -> Option<String> {
    if active_viewers == 0 {
        return Some(
            "Refresh skipped: no active viewers are currently tracked. Open the dashboard tab to resume updates."
                .to_string(),
        );
    }

    if let Some(age) = snapshot_age {
        if age < REFRESH_INTERVAL_SECONDS {
            return Some(format!(
                "Refresh skipped: latest snapshot is {age}s old; minimum refresh interval is {REFRESH_INTERVAL_SECONDS}s."
            ));
        }
    }

    None
}

#[derive(Default)]
struct RefreshCoordinator {
    next_source: usize,
    last_attempt: Option<Instant>,
    next_eligible: [Option<Instant>; 4],
    consecutive_failures: [u32; 4],
    completed_generation: u64,
}

struct RefreshPlan {
    sources: Vec<UpstreamSource>,
    generation: Option<u64>,
}

impl RefreshCoordinator {
    // Called only after inspecting stored data and rechecking presence. Record
    // attempts before I/O so cancellation and storage failures retain the gate.
    fn begin(
        &mut self,
        now: Instant,
        requested_generation: u64,
    ) -> Result<RefreshPlan, &'static str> {
        let interval = Duration::from_secs(REFRESH_INTERVAL_SECONDS as u64);
        if self
            .last_attempt
            .is_some_and(|last| now.saturating_duration_since(last) < interval)
        {
            return Err(
                "Refresh skipped: minimum interval since the last attempt has not elapsed.",
            );
        }

        let full_refresh = requested_generation != self.completed_generation;
        let mut sources = Vec::new();
        for offset in 0..UpstreamSource::ALL.len() {
            let index = if full_refresh {
                offset
            } else {
                (self.next_source + offset) % UpstreamSource::ALL.len()
            };
            if self.next_eligible[index].is_none_or(|eligible| now >= eligible) {
                sources.push(UpstreamSource::ALL[index]);
                if !full_refresh {
                    break;
                }
            }
        }
        if sources.is_empty() {
            return Err("Refresh skipped: all providers are waiting before another attempt.");
        }

        self.last_attempt = Some(now);
        for source in &sources {
            let index = source.index();
            self.next_eligible[index] = Some(now + interval);
            self.next_source = (index + 1) % UpstreamSource::ALL.len();
        }
        Ok(RefreshPlan {
            sources,
            generation: full_refresh.then_some(requested_generation),
        })
    }

    fn complete(
        &mut self,
        plan: &RefreshPlan,
        failures: &[ProviderError],
        now: Instant,
        now_unix: i64,
    ) {
        for source in &plan.sources {
            let index = source.index();
            if let Some(error) = failures.iter().find(|error| error.provider == *source) {
                let count = self.consecutive_failures[index].saturating_add(1).min(6);
                self.consecutive_failures[index] = count;
                let backoff = Duration::from_secs((10_u64 << (count - 1)).min(300));
                let retry_delay = match &error.kind {
                    ProviderErrorKind::Http {
                        retry_after: Some(RetryAfter::Delay(delay)),
                        ..
                    } => Some(*delay),
                    ProviderErrorKind::Http {
                        retry_after: Some(RetryAfter::At(time)),
                        ..
                    } => time.duration_since(UNIX_EPOCH).ok().and_then(|time| {
                        let seconds = (i128::from(time.as_secs()) - i128::from(now_unix)).max(0);
                        u64::try_from(seconds).ok().map(Duration::from_secs)
                    }),
                    _ => None,
                };
                // Convert a wall-clock hint once; subsequent eligibility is monotonic.
                // Ignore hints too large for Instant instead of panicking or retrying now.
                self.next_eligible[index] = Some(
                    retry_delay
                        .and_then(|delay| now.checked_add(delay.max(backoff)))
                        .unwrap_or(now + backoff),
                );
            } else {
                // Provider recovery is independent of whether SQLite saved its quote.
                // Keep the minimum deadline installed before dispatch even on write failure.
                self.consecutive_failures[index] = 0;
            }
        }
        if let Some(generation) = plan.generation {
            // A newer activation lives in a separate atomic and is never cleared.
            self.completed_generation = generation;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn http_failure(retry_after: Option<RetryAfter>) -> ProviderError {
        ProviderError {
            provider: UpstreamSource::CoinGecko,
            kind: ProviderErrorKind::Http {
                status: reqwest::StatusCode::TOO_MANY_REQUESTS,
                retry_after,
            },
        }
    }

    #[test]
    fn repeated_failures_back_off_to_cap_and_recovery_resets_delay() {
        let mut coordinator = RefreshCoordinator::default();
        let mut now = Instant::now();
        for (attempt, seconds) in [10, 20, 40, 80, 160, 300, 300].into_iter().enumerate() {
            // New activations cannot bypass an individual provider's cooldown.
            let generation = attempt as u64 + 1;
            let plan = coordinator.begin(now, generation).unwrap();
            assert!(plan.sources.contains(&UpstreamSource::CoinGecko));
            coordinator.complete(&plan, &[http_failure(None)], now, 0);
            assert_eq!(
                coordinator.next_eligible[0],
                Some(now + Duration::from_secs(seconds))
            );
            now += Duration::from_secs(seconds);
        }
        let plan = coordinator.begin(now, 8).unwrap();
        coordinator.complete(&plan, &[], now, 0);
        now += Duration::from_secs(10);
        let plan = coordinator.begin(now, 9).unwrap();
        coordinator.complete(&plan, &[http_failure(None)], now, 0);
        assert_eq!(
            coordinator.next_eligible[0],
            Some(now + Duration::from_secs(10))
        );
    }

    #[test]
    fn retry_hints_extend_backoff_and_ignore_unrepresentable_deadlines() {
        for (hint, seconds) in [
            (None, 10),
            (Some(RetryAfter::Delay(Duration::ZERO)), 10),
            (Some(RetryAfter::Delay(Duration::from_secs(600))), 600),
            (
                Some(RetryAfter::At(UNIX_EPOCH + Duration::from_secs(1600))),
                600,
            ),
            (
                Some(RetryAfter::At(UNIX_EPOCH + Duration::from_secs(900))),
                10,
            ),
            (Some(RetryAfter::Delay(Duration::from_secs(u64::MAX))), 10),
        ] {
            let mut coordinator = RefreshCoordinator::default();
            let now = Instant::now();
            let plan = coordinator.begin(now, 1).unwrap();
            coordinator.complete(&plan, &[http_failure(hint)], now, 1000);
            assert_eq!(
                coordinator.next_eligible[0],
                Some(now + Duration::from_secs(seconds))
            );
        }
    }

    #[test]
    fn cooling_provider_is_skipped_until_exact_deadline() {
        let mut coordinator = RefreshCoordinator::default();
        let now = Instant::now();
        let plan = coordinator.begin(now, 1).unwrap();
        coordinator.complete(
            &plan,
            &[http_failure(Some(RetryAfter::Delay(Duration::from_secs(
                60,
            ))))],
            now,
            0,
        );
        assert_eq!(
            coordinator
                .begin(now + Duration::from_secs(10), 1)
                .unwrap()
                .sources,
            [UpstreamSource::Coinbase]
        );
        let plan = coordinator.begin(now + Duration::from_secs(59), 2).unwrap();
        assert!(!plan.sources.contains(&UpstreamSource::CoinGecko));
        coordinator.complete(&plan, &[], now + Duration::from_secs(59), 59);
        // Even a provider whose retry expires must respect the global attempt gate.
        assert!(coordinator.begin(now + Duration::from_secs(60), 2).is_err());
        assert_eq!(
            coordinator
                .begin(now + Duration::from_secs(69), 2)
                .unwrap()
                .sources,
            [UpstreamSource::CoinGecko]
        );
    }

    #[test]
    fn attempts_rotate_without_requiring_a_successful_snapshot() {
        let mut coordinator = RefreshCoordinator::default();
        let now = Instant::now();
        for (index, source) in UpstreamSource::ALL
            .into_iter()
            .chain([UpstreamSource::CoinGecko])
            .enumerate()
        {
            let plan = coordinator
                .begin(now + Duration::from_secs(index as u64 * 10), 0)
                .unwrap();
            assert_eq!(plan.sources, [source]);
            assert!(plan.generation.is_none());
        }
    }

    #[test]
    fn no_eligible_provider_does_not_consume_an_attempt_or_activation() {
        let now = Instant::now();
        let mut coordinator = RefreshCoordinator {
            next_eligible: [Some(now + Duration::from_secs(20)); 4],
            ..Default::default()
        };
        assert!(coordinator.begin(now, 1).is_err());
        assert!(coordinator.last_attempt.is_none());
        let plan = coordinator.begin(now + Duration::from_secs(20), 1).unwrap();
        assert!(plan.generation.is_some());
        assert_eq!(plan.sources, UpstreamSource::ALL);
    }

    #[tokio::test]
    async fn tracker_background_drain_uses_the_existing_shutdown_deadline() {
        let mut app = crate::test_support::TestApp::new().await;
        app.configure(crate::config::RuntimeConfig {
            shutdown_seconds: 1,
            ..Default::default()
        });
        let guard = app.state.refresh.coordinator.clone().lock_owned().await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server = tokio::spawn(crate::lifecycle::serve_tracker_until(
            listener,
            app.state.clone(),
            async {},
        ));
        while !app.state.refresh.is_stopping() {
            tokio::task::yield_now().await;
        }
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::time::resume();
        assert_eq!(
            server.await.unwrap(),
            Err("background price refresh shutdown deadline exceeded".into())
        );
        drop(guard);
    }

    #[test]
    fn refresh_is_skipped_when_snapshot_is_too_recent() {
        assert!(refresh_skip_reason(1, Some(4)).is_some());
    }
}
