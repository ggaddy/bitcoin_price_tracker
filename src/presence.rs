use std::collections::HashMap;

use crate::{
    config::{REFRESH_INTERVAL_SECONDS, VIEWER_TTL_SECONDS},
    models::{PresencePayload, SnapshotRecord},
    state::AppState,
    util::now_unix,
};

pub(crate) async fn apply_presence_update(state: &AppState, payload: PresencePayload) -> usize {
    let now = now_unix();
    let mut viewers = state.viewers.lock().await;
    prune_inactive_viewers(&mut viewers, now);

    if payload.active {
        viewers.insert(payload.session_id, now);
    } else {
        viewers.remove(&payload.session_id);
    }

    viewers.len()
}

pub(crate) async fn active_viewer_count(state: &AppState) -> usize {
    let now = now_unix();
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

    use super::{is_valid_session_id, prune_inactive_viewers, refresh_skip_reason};
    use crate::config::VIEWER_TTL_SECONDS;

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
}
