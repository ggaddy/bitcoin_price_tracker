use std::time::{Duration, Instant};

use axum::{Json, extract::State, http::StatusCode};
use serde::Serialize;

use crate::state::AppState;

const FEED_URL: &str = "https://news.bitcoin.com/feed/";
const REFRESH_INTERVAL: Duration = Duration::from_secs(300);
const MAX_FEED_BYTES: usize = 2 * 1024 * 1024;

#[derive(Default)]
pub(crate) struct FeedCache {
    checked_at: Option<Instant>,
    xml: Option<String>,
    stale: bool,
}

#[derive(Serialize)]
pub(crate) struct FeedResponse {
    xml: Option<String>,
    stale: bool,
}

async fn fetch_feed(client: &reqwest::Client) -> Result<String, String> {
    let mut response = client
        .get(FEED_URL)
        .timeout(Duration::from_secs(12))
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
) -> (StatusCode, Json<FeedResponse>) {
    // Serialize refreshes across viewers and throttle failures as well as successes.
    let mut cache = state.news.lock().await;
    if cache
        .checked_at
        .is_none_or(|at| at.elapsed() >= REFRESH_INTERVAL)
    {
        match fetch_feed(&state.client).await {
            Ok(xml) => {
                cache.xml = Some(xml);
                cache.stale = false;
            }
            Err(error) => {
                tracing::warn!("Bitcoin RSS refresh failed: {error}");
                cache.stale = true;
            }
        }
        cache.checked_at = Some(Instant::now());
    }
    (
        if cache.xml.is_some() {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        Json(FeedResponse {
            xml: cache.xml.clone(),
            stale: cache.stale,
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
            };
            let (status, Json(body)) = bitcoin_news(State(state)).await;
            assert_eq!(status, expected_status);
            assert_eq!(body.xml, xml);
            assert_eq!(body.stale, stale);
        }
    }
}
