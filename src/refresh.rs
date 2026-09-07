use std::time::{Duration, Instant, UNIX_EPOCH};

use crate::{
    config::REFRESH_INTERVAL_SECONDS,
    errors::{ProviderError, ProviderErrorKind, RetryAfter},
    pricing::UpstreamSource,
};

#[derive(Default)]
pub(crate) struct RefreshCoordinator {
    next_source: usize,
    last_attempt: Option<Instant>,
    next_eligible: [Option<Instant>; 4],
    consecutive_failures: [u32; 4],
    completed_generation: u64,
}

pub(crate) struct RefreshPlan {
    pub(crate) sources: Vec<UpstreamSource>,
    pub(crate) full_refresh: bool,
    generation: u64,
}

impl RefreshCoordinator {
    // Called only after inspecting stored data and rechecking presence. Record
    // attempts before I/O so cancellation and storage failures retain the gate.
    pub(crate) fn begin(
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
            let index = UpstreamSource::ALL
                .iter()
                .position(|candidate| candidate == source)
                .unwrap();
            self.next_eligible[index] = Some(now + interval);
            self.next_source = (index + 1) % UpstreamSource::ALL.len();
        }
        Ok(RefreshPlan {
            sources,
            full_refresh,
            generation: requested_generation,
        })
    }

    pub(crate) fn complete(
        &mut self,
        plan: &RefreshPlan,
        failures: &[ProviderError],
        now: Instant,
        now_unix: i64,
    ) {
        for source in &plan.sources {
            let index = UpstreamSource::ALL
                .iter()
                .position(|candidate| candidate == source)
                .unwrap();
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
        if plan.full_refresh {
            // A newer activation lives in a separate atomic and is never cleared.
            self.completed_generation = plan.generation;
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
            assert!(!plan.full_refresh);
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
        assert!(plan.full_refresh);
        assert_eq!(plan.sources, UpstreamSource::ALL);
    }
}
