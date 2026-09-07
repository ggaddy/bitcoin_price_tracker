use std::time::Duration;

use axum::http::StatusCode;
use serde_json::json;

use crate::{
    config::UPSTREAM_REQUEST_TIMEOUT,
    presence::active_viewer_count,
    pricing::UpstreamSource,
    storage::load_latest_snapshot,
    test_support::{TEST_NOW, TestApp},
};

#[tokio::test]
async fn cached_callers_finish_while_one_refresh_is_stalled() {
    let app = TestApp::new().await;
    app.presence(true).await;
    assert_eq!(app.price().await.0, StatusCode::OK);
    let provider = app.upstreams.provider(UpstreamSource::CoinGecko);
    provider.wait_for_request().await;
    let release = provider.hold_responses();
    app.clock.advance(Duration::from_secs(10));

    let owner = app.price();
    tokio::pin!(owner);
    tokio::select! {
        _ = provider.wait_for_request() => {}
        _ = &mut owner => panic!("refresh completed while the provider was held"),
    }
    let readers = tokio::time::timeout(Duration::from_secs(1), async {
        tokio::join!(app.price(), app.price(), app.price())
    })
    .await
    .expect("cached readers did not wait for the provider");
    for (status, data) in [readers.0, readers.1, readers.2] {
        assert_eq!(status, StatusCode::OK);
        assert_eq!(data["average_price"], 100_150.0);
        assert_eq!(data["fetched_at_unix"], TEST_NOW);
        assert_eq!(data["refresh_succeeded"], false);
        assert!(
            data["refresh_skipped_reason"]
                .as_str()
                .unwrap()
                .contains("in progress")
        );
    }
    assert_eq!(app.upstreams.request_counts(), [2, 1, 1, 1]);
    // Presence remains responsive, and the owner's response reflects the departure.
    app.presence(false).await;
    release.add_permits(1);
    let (_, completed) = owner.await;
    assert_eq!(completed["refresh_succeeded"], true);
    assert_eq!(completed["active_viewers"], 0);
}

#[tokio::test]
async fn concurrent_cold_start_returns_unavailable_without_waiting() {
    let app = TestApp::new().await;
    let provider = app.upstreams.provider(UpstreamSource::CoinGecko);
    let release = provider.hold_responses();
    app.presence(true).await;
    let owner = app.price();
    tokio::pin!(owner);
    tokio::select! {
        _ = provider.wait_for_request() => {}
        _ = &mut owner => panic!("refresh completed before release"),
    }
    let (status, data) = tokio::time::timeout(Duration::from_secs(1), app.price())
        .await
        .expect("cold reader completed while refresh remained held");
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(data["sources"], json!([]));
    assert!(
        data["refresh_skipped_reason"]
            .as_str()
            .unwrap()
            .contains("in progress")
    );
    release.add_permits(1);
    assert_eq!(owner.await.0, StatusCode::OK);
    assert_eq!(app.upstreams.request_counts(), [1; 4]);
}

#[tokio::test]
async fn activation_during_batch_survives_completion_and_freshness_gate() {
    let app = TestApp::new().await;
    let provider = app.upstreams.provider(UpstreamSource::CoinGecko);
    let release = provider.hold_responses();
    app.presence(true).await;
    let owner = app.price();
    tokio::pin!(owner);
    tokio::select! {
        _ = provider.wait_for_request() => {}
        _ = &mut owner => panic!("refresh completed before release"),
    }
    app.presence(false).await;
    app.presence(true).await;
    release.add_permits(1);
    assert_eq!(owner.await.0, StatusCode::OK);
    assert_eq!(app.price().await.1["refresh_succeeded"], false);
    assert_eq!(app.upstreams.request_counts(), [1; 4]);
    app.clock.advance(Duration::from_secs(10));
    release.add_permits(1);
    assert_eq!(app.price().await.1["refresh_succeeded"], true);
    assert_eq!(app.upstreams.request_counts(), [2; 4]);
    app.presence(true).await;
    app.clock.advance(Duration::from_secs(10));
    release.add_permits(1);
    assert_eq!(app.price().await.1["refresh_succeeded"], true);
    assert_eq!(app.upstreams.request_counts(), [3, 2, 2, 2]);
}

#[tokio::test]
async fn failed_attempt_observes_cadence_and_advances_rotation() {
    let app = TestApp::new().await;
    app.presence(true).await;
    assert_eq!(app.price().await.0, StatusCode::OK);
    app.upstreams
        .provider(UpstreamSource::CoinGecko)
        .set_response(crate::test_support::FixtureResponse::json(json!({})));
    app.clock.advance(Duration::from_secs(10));
    app.presence(true).await;
    assert_eq!(app.price().await.1["refresh_succeeded"], false);
    assert_eq!(app.upstreams.request_counts(), [2, 1, 1, 1]);
    app.clock.advance(Duration::from_secs(9));
    let (_, cached) = app.price().await;
    assert!(
        cached["refresh_skipped_reason"]
            .as_str()
            .unwrap()
            .contains("last attempt")
    );
    assert_eq!(app.upstreams.request_counts(), [2, 1, 1, 1]);
    app.clock.advance(Duration::from_secs(1));
    assert_eq!(app.price().await.1["refresh_succeeded"], true);
    assert_eq!(app.upstreams.request_counts(), [2, 2, 1, 1]);
}

#[tokio::test]
async fn cancelling_refresh_releases_ownership_and_retains_attempt_gate() {
    let app = TestApp::new().await;
    let provider = app.upstreams.provider(UpstreamSource::CoinGecko);
    let release = provider.hold_responses();
    app.presence(true).await;
    {
        let owner = app.price();
        tokio::pin!(owner);
        tokio::select! {
            _ = provider.wait_for_request() => {}
            _ = &mut owner => panic!("refresh completed before release"),
        }
        for source in [
            UpstreamSource::Coinbase,
            UpstreamSource::Kraken,
            UpstreamSource::Gemini,
        ] {
            app.upstreams.provider(source).wait_for_request().await;
        }
    }
    assert!(app.state.refresh.try_lock().is_ok());
    let (_, data) = app.price().await;
    assert!(
        data["refresh_skipped_reason"]
            .as_str()
            .unwrap()
            .contains("last attempt")
    );
    let counts = app.upstreams.request_counts();
    app.clock.advance(Duration::from_secs(10));
    release.add_permits(2);
    assert_eq!(app.price().await.1["refresh_succeeded"], true);
    for (before, after) in counts.into_iter().zip(app.upstreams.request_counts()) {
        assert_eq!(after, before + 1);
    }
}

#[tokio::test]
async fn timed_out_refresh_serves_the_previous_snapshot_and_releases_the_lock() {
    let app = TestApp::new().await;
    app.presence(true).await;
    assert_eq!(app.price().await.0, StatusCode::OK);
    let provider = app.upstreams.provider(UpstreamSource::CoinGecko);
    provider.wait_for_request().await; // Consume the initial successful refresh's signal.
    let release = provider.hold_responses();
    app.clock.advance(Duration::from_secs(10));

    let response = app.price();
    tokio::pin!(response);
    tokio::select! {
        _ = provider.wait_for_request() => {}
        _ = &mut response => panic!("request finished before the held provider timed out"),
    }
    tokio::time::pause();
    tokio::time::advance(UPSTREAM_REQUEST_TIMEOUT).await;
    // Resume before SQLite's blocking work so automatic time advances cannot race the route deadline.
    tokio::time::resume();
    let (status, data) = response.await;
    release.add_permits(1);
    assert_eq!(status, StatusCode::OK);
    assert_eq!(data["fetched_at_unix"], TEST_NOW);
    assert_eq!(data["average_price"], 100_150.0);
    assert_eq!(data["refresh_succeeded"], false);
    assert!(
        data["warnings"]
            .to_string()
            .contains("CoinGecko request timed out")
    );
    assert!(app.state.refresh.try_lock().is_ok());
}

#[tokio::test]
async fn storage_failure_warnings_hide_internal_details() {
    let app = TestApp::new().await;
    app.presence(true).await;
    assert_eq!(app.price().await.0, StatusCode::OK);
    let conn = rusqlite::Connection::open(&app.state.db_path).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER reject_snapshot BEFORE INSERT ON price_snapshots
        BEGIN SELECT RAISE(ABORT, 'private error at /private/fixture.db'); END;",
    )
    .unwrap();
    app.clock.advance(Duration::from_secs(10));
    let (status, data) = app.price().await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(data["fetched_at_unix"], TEST_NOW);
    assert_eq!(data["average_price"], 100_150.0);
    assert_eq!(data["refresh_succeeded"], false);
    assert!(
        data["warnings"]
            .to_string()
            .contains("Failed to store refreshed price data")
    );
    assert!(!data.to_string().contains("/private/fixture.db"));
    assert!(!data.to_string().contains("private error"));
    let counts = app.upstreams.request_counts();
    let (_, cached) = app.price().await;
    assert!(
        cached["refresh_skipped_reason"]
            .as_str()
            .unwrap()
            .contains("last attempt")
    );
    assert_eq!(app.upstreams.request_counts(), counts);
}

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
