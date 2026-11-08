use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

use crate::{config::VIEWER_TTL_SECONDS, models::PriceResponse, state::AppState};

#[derive(Default)]
pub(crate) struct Presence {
    viewers: Mutex<HashMap<String, Instant>>,
    generation: AtomicU64,
}

impl Presence {
    pub(crate) fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }
}

#[derive(Debug)]
pub(crate) enum PresenceError {
    InvalidSession,
    CapacityReached { active_viewers: usize },
}

#[derive(Deserialize)]
pub(crate) struct PresencePayload {
    pub(crate) session_id: String,
    pub(crate) active: bool,
    #[serde(default)]
    pub(crate) include_price: bool,
}

#[derive(Serialize)]
pub(crate) struct PresenceResponse {
    pub(crate) active_viewers: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) price: Option<PriceResponse>,
}

pub(crate) async fn update_presence(
    state: &AppState,
    payload: PresencePayload,
) -> Result<PresenceResponse, PresenceError> {
    if !is_valid_session_id(&payload.session_id) {
        return Err(PresenceError::InvalidSession);
    }
    let include_price = payload.active && payload.include_price;
    let active_viewers = {
        let mut viewers = state.presence.viewers.lock().await;
        // Sample time after admission to the critical section.
        let now = state.clock.now_monotonic();
        prune_inactive_viewers(&mut viewers, now);
        let previously_empty = viewers.is_empty();
        if payload.active
            && !viewers.contains_key(&payload.session_id)
            && viewers.len() >= state.config.max_viewers
        {
            return Err(PresenceError::CapacityReached {
                active_viewers: viewers.len(),
            });
        }
        if payload.active {
            viewers.insert(payload.session_id, now);
        } else {
            viewers.remove(&payload.session_id);
        }
        if payload.active && previously_empty && !viewers.is_empty() {
            state.presence.generation.fetch_add(1, Ordering::SeqCst);
        }
        viewers.len()
    };
    // Only admitted active viewers may request a startup price. Release the
    // viewer lock before taking refresh ownership or doing any database I/O.
    let price = if include_price {
        Some(crate::refresh::cached_prices(state).await.1)
    } else {
        None
    };
    Ok(PresenceResponse {
        active_viewers,
        price,
    })
}

pub(crate) async fn active_viewer_count(state: &AppState) -> usize {
    let mut viewers = state.presence.viewers.lock().await;
    let now = state.clock.now_monotonic();
    prune_inactive_viewers(&mut viewers, now);
    viewers.len()
}

fn prune_inactive_viewers(viewers: &mut HashMap<String, Instant>, now: Instant) {
    let ttl = Duration::from_secs(VIEWER_TTL_SECONDS as u64);
    viewers.retain(|_, last_seen| now.saturating_duration_since(*last_seen) <= ttl);
}

fn is_valid_session_id(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id.len() <= 128
        && session_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{
        PresenceError, PresencePayload, active_viewer_count, is_valid_session_id,
        prune_inactive_viewers, update_presence,
    };
    use crate::config::VIEWER_TTL_SECONDS;
    use crate::state::AppState;
    use std::path::PathBuf;
    use std::time::Duration;

    #[test]
    fn session_ids_must_be_ascii_and_reasonable_length() {
        assert!(is_valid_session_id("viewer_123-abc"));
        assert!(!is_valid_session_id(""));
        assert!(!is_valid_session_id("contains space"));
        assert!(!is_valid_session_id(&"x".repeat(129)));
    }

    #[tokio::test]
    async fn invalid_presence_cannot_mutate_admission_or_request_a_startup_price() {
        let app = crate::test_support::TestApp::new().await;
        for session_id in [
            "".to_string(),
            "contains space".to_string(),
            "x".repeat(129),
        ] {
            for active in [true, false] {
                let result = update_presence(
                    &app.state,
                    PresencePayload {
                        session_id: session_id.clone(),
                        active,
                        include_price: true,
                    },
                )
                .await;
                assert!(matches!(result, Err(PresenceError::InvalidSession)));
                assert_eq!(active_viewer_count(&app.state).await, 0);
                assert_eq!(app.state.presence.generation(), 0);
                assert_eq!(app.upstreams.request_counts(), [0; 4]);
            }
        }
    }

    #[test]
    fn inactive_viewers_are_pruned() {
        let now = std::time::Instant::now();
        let mut viewers = HashMap::from([
            ("active".to_string(), now),
            (
                "stale".to_string(),
                now - std::time::Duration::from_secs(VIEWER_TTL_SECONDS as u64 + 1),
            ),
        ]);

        prune_inactive_viewers(&mut viewers, now);

        assert!(viewers.contains_key("active"));
        assert!(!viewers.contains_key("stale"));
    }

    #[tokio::test]
    async fn first_active_viewer_marks_full_refresh_pending() {
        let state = AppState::new(reqwest::Client::new(), PathBuf::from("test.db"));

        let count = update_presence(
            &state,
            PresencePayload {
                session_id: "viewer-1".to_string(),
                active: true,
                include_price: false,
            },
        )
        .await;

        assert_eq!(count.unwrap().active_viewers, 1);
        assert_eq!(state.presence.generation(), 1);
    }

    #[tokio::test]
    async fn presence_samples_time_after_acquiring_the_viewer_lock() {
        use std::{
            future::{Future, poll_fn},
            task::Poll,
        };

        let app = crate::test_support::TestApp::new().await;
        app.presence(true).await;
        let guard = app.state.presence.viewers.lock().await;
        let count = active_viewer_count(&app.state);
        tokio::pin!(count);
        poll_fn(|cx| {
            assert!(count.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        app.clock.advance_monotonic(Duration::from_secs(16));
        drop(guard);
        assert_eq!(count.await, 0);

        let guard = app.state.presence.viewers.lock().await;
        let heartbeat = app.presence(true);
        tokio::pin!(heartbeat);
        poll_fn(|cx| {
            assert!(heartbeat.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        app.clock.advance_monotonic(Duration::from_secs(16));
        drop(guard);
        heartbeat.await;
        assert_eq!(active_viewer_count(&app.state).await, 1);
        app.clock.advance_monotonic(Duration::from_secs(15));
        assert_eq!(active_viewer_count(&app.state).await, 1);
        app.clock.advance_monotonic(Duration::from_nanos(1));
        assert_eq!(active_viewer_count(&app.state).await, 0);
    }
}
