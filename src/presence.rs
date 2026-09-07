use std::collections::HashMap;
use std::sync::atomic::Ordering;

use crate::{
    config::{REFRESH_INTERVAL_SECONDS, VIEWER_TTL_SECONDS},
    models::{PresencePayload, SnapshotRecord},
    state::AppState,
};

pub(crate) async fn apply_presence_update(state: &AppState, payload: PresencePayload) -> usize {
    let now = state.clock.now_unix();
    let mut viewers = state.viewers.lock().await;
    prune_inactive_viewers(&mut viewers, now);
    let previously_empty = viewers.is_empty();

    if payload.active {
        viewers.insert(payload.session_id, now);
    } else {
        viewers.remove(&payload.session_id);
    }

    if payload.active && previously_empty && !viewers.is_empty() {
        state.full_refresh_generation.fetch_add(1, Ordering::SeqCst);
    }

    viewers.len()
}

pub(crate) async fn active_viewer_count(state: &AppState) -> usize {
    let now = state.clock.now_unix();
    let mut viewers = state.viewers.lock().await;
    prune_inactive_viewers(&mut viewers, now);
    viewers.len()
}

pub(crate) fn prune_inactive_viewers(viewers: &mut HashMap<String, i64>, now: i64) {
    viewers.retain(|_, last_seen| now.saturating_sub(*last_seen) <= VIEWER_TTL_SECONDS);
}

pub(crate) fn snapshot_age_seconds(snapshot: &SnapshotRecord, now: i64) -> Option<i64> {
    Some(now.saturating_sub(snapshot.fetched_at_unix))
}

pub(crate) fn refresh_skip_reason(
    active_viewers: usize,
    snapshot_age: Option<i64>,
) -> Option<String> {
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

pub(crate) fn is_valid_session_id(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id.len() <= 128
        && session_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::Ordering;

    use super::{
        apply_presence_update, is_valid_session_id, prune_inactive_viewers, refresh_skip_reason,
    };
    use crate::config::VIEWER_TTL_SECONDS;
    use crate::models::PresencePayload;
    use crate::state::AppState;
    use std::path::PathBuf;

    #[test]
    fn session_ids_must_be_ascii_and_reasonable_length() {
        assert!(is_valid_session_id("viewer_123-abc"));
        assert!(!is_valid_session_id(""));
        assert!(!is_valid_session_id("contains space"));
        assert!(!is_valid_session_id(&"x".repeat(129)));
    }

    #[test]
    fn refresh_is_skipped_when_snapshot_is_too_recent() {
        let reason = refresh_skip_reason(1, Some(4));
        assert!(reason.is_some());
    }

    #[test]
    fn inactive_viewers_are_pruned() {
        let mut viewers = HashMap::from([
            ("active".to_string(), 100),
            ("stale".to_string(), 100 - VIEWER_TTL_SECONDS - 1),
        ]);

        prune_inactive_viewers(&mut viewers, 100);

        assert!(viewers.contains_key("active"));
        assert!(!viewers.contains_key("stale"));
    }

    #[tokio::test]
    async fn first_active_viewer_marks_full_refresh_pending() {
        let state = AppState::new(reqwest::Client::new(), PathBuf::from("test.db"));

        let count = apply_presence_update(
            &state,
            PresencePayload {
                session_id: "viewer-1".to_string(),
                active: true,
            },
        )
        .await;

        assert_eq!(count, 1);
        assert_eq!(state.full_refresh_generation.load(Ordering::SeqCst), 1);
    }
}
