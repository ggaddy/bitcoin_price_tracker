use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) trait Clock: Send + Sync {
    fn now_unix(&self) -> i64;
}

pub(crate) struct SystemClock;

impl Clock for SystemClock {
    fn now_unix(&self) -> i64 {
        now_unix()
    }
}

pub(crate) fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs() as i64)
}
