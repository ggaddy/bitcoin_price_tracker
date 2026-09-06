use std::time::Duration;

use axum::http::StatusCode;
use serde_json::json;

use crate::{
    presence::active_viewer_count,
    pricing::UpstreamSource,
    storage::load_latest_snapshot,
    test_support::{TEST_NOW, TestApp},
};

#[tokio::test]
async fn no_viewers_never_request_upstreams() {
    let app = TestApp::new().await;
    let (status, data) = app.price().await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(data["sources"], json!([]));
    assert_eq!(data["active_viewers"], 0);
    assert_eq!(app.upstreams.request_counts(), [0; 4]);

    app.presence(true).await;
    assert_eq!(app.price().await.0, StatusCode::OK);
    app.presence(false).await;
    app.clock.advance(Duration::from_secs(60));

    let (status, data) = app.price().await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(data["active_viewers"], 0);
    assert_eq!(data["stale"], true);
    assert_eq!(data["average_price"], 100_150.0);
    assert_eq!(app.upstreams.request_counts(), [1; 4]);
}

#[tokio::test]
async fn local_providers_and_manual_time_drive_refresh_and_cache_age() {
    let app = TestApp::new().await;
    app.presence(true).await;

    let (status, data) = app.price().await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(data["refresh_succeeded"], true);
    assert_eq!(data["fetched_at_unix"], TEST_NOW);
    assert_eq!(data["fetched_age_seconds"], 0);
    assert_eq!(data["average_price"], 100_150.0);
    assert_eq!(data["spread"], 300.0);
    assert_eq!(
        data["sources"],
        json!([
            {"source": "CoinGecko", "price_usd": 100_000.0},
            {"source": "Coinbase", "price_usd": 100_100.0},
            {"source": "Kraken", "price_usd": 100_200.0},
            {"source": "Gemini", "price_usd": 100_300.0},
        ])
    );
    assert_eq!(app.upstreams.request_counts(), [1; 4]);

    app.clock.advance(Duration::from_secs(9));
    let (status, cached) = app.price().await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cached["refresh_succeeded"], false);
    assert_eq!(cached["fetched_at_unix"], TEST_NOW);
    assert_eq!(cached["fetched_age_seconds"], 9);
    assert_eq!(app.upstreams.request_counts(), [1; 4]);

    app.clock.advance(Duration::from_secs(1));
    let (status, refreshed) = app.price().await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(refreshed["refresh_succeeded"], true);
    assert_eq!(refreshed["fetched_at_unix"], TEST_NOW + 10);
    assert_eq!(refreshed["fetched_age_seconds"], 0);
    assert_eq!(app.upstreams.request_counts(), [2, 1, 1, 1]);
}

#[tokio::test]
async fn manual_time_expires_presence_without_sleeping() {
    let app = TestApp::new().await;
    app.presence(true).await;
    assert_eq!(app.price().await.0, StatusCode::OK);

    app.clock.advance(Duration::from_secs(15));
    assert_eq!(active_viewer_count(&app.state).await, 1);
    app.clock.advance(Duration::from_secs(1));
    let (status, data) = app.price().await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(data["active_viewers"], 0);
    assert_eq!(data["fetched_age_seconds"], 16);
    assert_eq!(data["refresh_succeeded"], false);
    assert_eq!(app.upstreams.request_counts(), [1; 4]);
}

#[tokio::test]
async fn held_provider_response_uses_time_at_completion() {
    let app = TestApp::new().await;
    let provider = app.upstreams.provider(UpstreamSource::CoinGecko);
    let release = provider.hold_responses();
    app.presence(true).await;

    let response = app.price();
    tokio::pin!(response);
    tokio::select! {
        _ = provider.wait_for_request() => {}
        _ = &mut response => panic!("price request finished before releasing the provider"),
    }
    assert_eq!(provider.request_count(), 1);
    app.clock.advance(Duration::from_secs(7));
    release.add_permits(1);

    let (status, data) = response.await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(data["fetched_at_unix"], TEST_NOW + 7);
    assert_eq!(data["fetched_age_seconds"], 0);
    assert_eq!(data["refresh_succeeded"], true);
    assert_eq!(app.upstreams.request_counts(), [1; 4]);
    let snapshot = load_latest_snapshot(app.state.db_path.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(snapshot.fetched_at_unix, TEST_NOW + 7);
}
