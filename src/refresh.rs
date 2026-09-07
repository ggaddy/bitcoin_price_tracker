use std::time::{Duration, Instant};

use crate::{config::REFRESH_INTERVAL_SECONDS, pricing::UpstreamSource};

#[derive(Default)]
pub(crate) struct RefreshCoordinator {
    next_source: usize,
    last_attempt: Option<Instant>,
    next_eligible: [Option<Instant>; 4],
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

    pub(crate) fn complete(&mut self, plan: &RefreshPlan) {
        if plan.full_refresh {
            // A newer activation lives in a separate atomic and is never cleared.
            self.completed_generation = plan.generation;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
