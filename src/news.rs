use std::{
    future::Future,
    time::{Duration, Instant},
};

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode},
};
use serde::Serialize;

use crate::state::AppState;

const FEED_URL: &str = "https://news.bitcoin.com/feed/";
const REFRESH_INTERVAL: Duration = Duration::from_secs(300);
const MAX_FEED_BYTES: usize = 2 * 1024 * 1024;
const REFRESH_TIMEOUT: Duration = Duration::from_secs(12);

#[derive(Default)]
pub(crate) struct FeedCache {
    checked_at: Option<Instant>,
    xml: Option<String>,
    stale: bool,
    refreshing: bool,
}

#[derive(Serialize)]
pub(crate) struct FeedResponse {
    xml: Option<String>,
    stale: bool,
    refreshing: bool,
}

async fn fetch_feed(client: &reqwest::Client) -> Result<String, String> {
    let mut response = client
        .get(FEED_URL)
        .timeout(REFRESH_TIMEOUT)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| error.to_string())? {
        if bytes.len() + chunk.len() > MAX_FEED_BYTES {
            return Err("RSS response exceeded size limit".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let xml = String::from_utf8(bytes).map_err(|error| error.to_string())?;
    if !xml.contains("<rss") || !xml.contains("<channel") {
        return Err("Upstream did not return an RSS feed".into());
    }
    Ok(xml)
}

pub(crate) async fn bitcoin_news(
    State(state): State<AppState>,
) -> (StatusCode, HeaderMap, Json<FeedResponse>) {
    let client = state.client.clone();
    news_response(&state, async move { fetch_feed(&client).await }).await
}

async fn news_response(
    state: &AppState,
    fetch: impl Future<Output = Result<String, String>> + Send + 'static,
) -> (StatusCode, HeaderMap, Json<FeedResponse>) {
    // Reserve one refresh under the lock, but never hold it across upstream I/O.
    // Readers return immediately, including during a cold start or slow refresh.
    let mut cache = state.news.lock().await;
    let expired = cache.checked_at.is_none_or(|at| {
        state.clock.now_monotonic().saturating_duration_since(at) >= REFRESH_INTERVAL
    });
    if expired && !cache.refreshing {
        cache.refreshing = true;
        let shared = state.news.clone();
        let clock = state.clock.clone();
        tokio::spawn(async move {
            let result = tokio::time::timeout(REFRESH_TIMEOUT, fetch)
                .await
                .unwrap_or_else(|_| Err("RSS refresh deadline exceeded".into()));
            let mut cache = shared.lock().await;
            match result {
                Ok(xml) => {
                    cache.xml = Some(xml);
                    cache.stale = false;
                }
                Err(error) => {
                    tracing::warn!("Bitcoin RSS refresh failed: {error}");
                    cache.stale = true;
                }
            }
            // Throttle failures as well as successes, measured from completion.
            cache.checked_at = Some(clock.now_monotonic());
            cache.refreshing = false;
        });
    }
    let mut headers = HeaderMap::new();
    if cache.refreshing {
        headers.insert("retry-after", HeaderValue::from_static("2"));
    }
    (
        if cache.xml.is_some() {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        headers,
        Json(FeedResponse {
            xml: cache.xml.clone(),
            stale: cache.stale || expired,
            refreshing: cache.refreshing,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cached_feed_and_failures_are_served_without_refetching() {
        for (xml, stale, expected_status) in [
            (
                Some("<rss><channel/></rss>".to_string()),
                false,
                StatusCode::OK,
            ),
            (
                Some("<rss><channel/></rss>".to_string()),
                true,
                StatusCode::OK,
            ),
            (None, true, StatusCode::SERVICE_UNAVAILABLE),
        ] {
            let state = AppState::new(reqwest::Client::new(), std::path::PathBuf::new());
            *state.news.lock().await = FeedCache {
                checked_at: Some(Instant::now()),
                xml: xml.clone(),
                stale,
                refreshing: false,
            };
            let (status, _, Json(body)) = bitcoin_news(State(state)).await;
            assert_eq!(status, expected_status);
            assert_eq!(body.xml, xml);
            assert_eq!(body.stale, stale);
            assert!(!body.refreshing);
        }
    }

    async fn settled(state: &AppState) {
        tokio::time::timeout(Duration::from_secs(1), async {
            while state.news.lock().await.refreshing {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn cold_start_and_expired_readers_return_while_one_refresh_is_held() {
        use tokio::sync::oneshot;
        for previous in [None, Some("<rss><channel>old</channel></rss>".to_string())] {
            let state = AppState::new(reqwest::Client::new(), std::path::PathBuf::new());
            state.news.lock().await.xml = previous.clone();
            let (entered, started) = oneshot::channel();
            let (release, held) = oneshot::channel();
            let (status, headers, Json(body)) = news_response(&state, async move {
                entered.send(()).unwrap();
                held.await.unwrap();
                Ok("<rss><channel>new</channel></rss>".into())
            })
            .await;
            started.await.unwrap();
            assert_eq!(
                status,
                if previous.is_some() {
                    StatusCode::OK
                } else {
                    StatusCode::SERVICE_UNAVAILABLE
                }
            );
            assert_eq!(headers["retry-after"], "2");
            assert_eq!(body.xml, previous);
            assert!(body.stale && body.refreshing);
            for _ in 0..64 {
                let (_, _, Json(body)) = tokio::time::timeout(
                    Duration::from_millis(100),
                    news_response(&state, std::future::pending()),
                )
                .await
                .unwrap();
                assert_eq!(body.xml, previous);
                assert!(body.refreshing);
            }
            release.send(()).unwrap();
            settled(&state).await;
            let (_, _, Json(body)) = news_response(&state, std::future::pending()).await;
            assert_eq!(
                body.xml.as_deref(),
                Some("<rss><channel>new</channel></rss>")
            );
            assert!(!body.stale && !body.refreshing);
        }
    }

    #[tokio::test]
    async fn failure_preserves_cached_feed_and_retries_only_after_backoff() {
        let app = crate::test_support::TestApp::new().await;
        app.state.news.lock().await.xml = Some("old feed".into());
        let _ = news_response(&app.state, async { Err("fixture failure".into()) }).await;
        settled(&app.state).await;
        app.clock
            .advance_monotonic(REFRESH_INTERVAL - Duration::from_secs(1));
        let (_, _, Json(body)) = news_response(&app.state, std::future::pending()).await;
        assert_eq!(body.xml.as_deref(), Some("old feed"));
        assert!(body.stale && !body.refreshing);
        app.clock.advance_monotonic(Duration::from_secs(1));
        let _ = news_response(&app.state, async { Ok("recovered feed".into()) }).await;
        settled(&app.state).await;
        let (_, _, Json(body)) = news_response(&app.state, std::future::pending()).await;
        assert_eq!(body.xml.as_deref(), Some("recovered feed"));
        assert!(!body.stale && !body.refreshing);
    }

    #[tokio::test(start_paused = true)]
    async fn refresh_deadline_releases_single_flight_and_starts_failure_backoff() {
        let state = AppState::new(reqwest::Client::new(), std::path::PathBuf::new());
        let _ = news_response(&state, std::future::pending()).await;
        tokio::task::yield_now().await;
        tokio::time::advance(REFRESH_TIMEOUT).await;
        settled(&state).await;
        let (_, headers, Json(body)) = news_response(&state, std::future::pending()).await;
        assert!(body.stale && !body.refreshing);
        assert!(headers.is_empty());
    }
}
