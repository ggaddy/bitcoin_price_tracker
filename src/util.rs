use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub(crate) trait Clock: Send + Sync {
    fn now_unix(&self) -> i64;
    fn now_monotonic(&self) -> Instant;
}

pub(crate) struct SystemClock;

impl Clock for SystemClock {
    fn now_monotonic(&self) -> Instant {
        Instant::now()
    }

    fn now_unix(&self) -> i64 {
        now_unix()
    }
}

pub(crate) fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs() as i64)
}
