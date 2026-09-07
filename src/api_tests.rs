use std::time::Duration;

use axum::http::StatusCode;
use serde_json::json;

use crate::{
    config::UPSTREAM_REQUEST_TIMEOUT,
    presence::active_viewer_count,
    pricing::UpstreamSource,
    storage::load_latest_snapshot,
    test_support::{FixtureResponse, TEST_NOW, TestApp},
};

#[tokio::test]
async fn all_rate_limited_providers_wait_without_consuming_pending_activation() {
    use axum::http::{HeaderValue, header::RETRY_AFTER};
    use std::time::UNIX_EPOCH;

    // Exercise header parsing through the actual HTTP adapters and coordinator.
    for hint in [
        "60".to_string(),
        httpdate::fmt_http_date(UNIX_EPOCH + Duration::from_secs((TEST_NOW + 60) as u64)),
    ] {
        let app = TestApp::new().await;
        for source in UpstreamSource::ALL {
            let mut response = FixtureResponse::json(json!({}));
            response.status = StatusCode::TOO_MANY_REQUESTS;
            response
                .headers
                .insert(RETRY_AFTER, HeaderValue::from_str(&hint).unwrap());
            app.upstreams.provider(source).set_response(response);
        }
        app.presence(true).await;
        assert_eq!(app.price().await.0, StatusCode::SERVICE_UNAVAILABLE);
        app.presence(false).await;
        app.presence(true).await;
        for seconds in [10, 10, 10, 10, 10, 9] {
            app.clock.advance(Duration::from_secs(seconds));
            app.presence(true).await;
            for _ in 0..3 {
                let (_, cached) = app.price().await;
                assert!(
                    cached["refresh_skipped_reason"]
                        .as_str()
                        .unwrap()
                        .contains("all providers")
                );
            }
            assert_eq!(app.upstreams.request_counts(), [1; 4]);
        }
        app.clock.advance(Duration::from_secs(1));
        app.upstreams
            .provider(UpstreamSource::CoinGecko)
            .set_response(FixtureResponse::json(json!({"bitcoin": {"usd": 123000}})));
        let (status, recovered) = app.price().await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(recovered["average_price"], 123000.0);
        // A pending activation still attempts every eligible provider at exactly 60s.
        assert_eq!(app.upstreams.request_counts(), [2; 4]);
        app.upstreams
            .provider(UpstreamSource::CoinGecko)
            .set_response(FixtureResponse::json(json!({})));
        app.clock.advance(Duration::from_secs(10));
        app.presence(true).await;
        assert_eq!(app.price().await.1["refresh_succeeded"], false);
        assert_eq!(app.upstreams.request_counts(), [3, 2, 2, 2]);
        app.clock.advance(Duration::from_secs(10));
        app.presence(false).await;
        app.presence(true).await;
        app.price().await;
        // Recovery reset CoinGecko's backoff: its next failure waits 10s, not 20s.
        assert_eq!(
            app.upstreams
                .provider(UpstreamSource::CoinGecko)
                .request_count(),
            4
        );
    }
}

#[tokio::test]
async fn rate_limited_provider_does_not_block_healthy_rotation_or_lose_retry_on_write_failure() {
    use axum::http::{HeaderValue, header::RETRY_AFTER};

    let app = TestApp::new().await;
    app.presence(true).await;
    app.price().await;
    let conn = rusqlite::Connection::open(&app.state.db_path).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER reject_snapshot BEFORE INSERT ON price_snapshots
        BEGIN SELECT RAISE(ABORT, 'test write failure'); END;",
    )
    .unwrap();
    let mut response = FixtureResponse::json(json!({}));
    response.status = StatusCode::TOO_MANY_REQUESTS;
    response
        .headers
        .insert(RETRY_AFTER, HeaderValue::from_static("120"));
    app.upstreams
        .provider(UpstreamSource::CoinGecko)
        .set_response(response);
    app.clock.advance(Duration::from_secs(10));
    app.presence(false).await;
    app.presence(true).await;
    let (_, failed_write) = app.price().await;
    assert_eq!(failed_write["refresh_succeeded"], false);
    assert!(
        failed_write["warnings"]
            .to_string()
            .contains("Failed to store")
    );
    assert_eq!(app.upstreams.request_counts(), [2; 4]);
    conn.execute_batch("DROP TRIGGER reject_snapshot;").unwrap();
    for _ in 0..4 {
        app.clock.advance(Duration::from_secs(10));
        app.presence(true).await;
        assert_eq!(app.price().await.1["refresh_succeeded"], true);
    }
    assert_eq!(app.upstreams.request_counts(), [2, 4, 3, 3]);
}

#[tokio::test]
async fn partial_cold_start_keeps_the_only_successful_provider() {
    let app = TestApp::new().await;
    for source in [
        UpstreamSource::CoinGecko,
        UpstreamSource::Coinbase,
        UpstreamSource::Kraken,
    ] {
        app.upstreams
            .provider(source)
            .set_response(FixtureResponse::json(json!({})));
    }
    let release = app
        .upstreams
        .provider(UpstreamSource::Gemini)
        .hold_responses();
    app.presence(true).await;
    let response = app.price();
    tokio::pin!(response);
    tokio::select! {
        _ = async {
            for source in UpstreamSource::ALL {
                app.upstreams.provider(source).wait_for_request().await;
            }
        } => {}
        _ = &mut response => panic!("a failed provider cancelled the held successful provider"),
    }
    release.add_permits(1);
    let (status, data) = response.await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(data["refresh_succeeded"], true);
    assert_eq!(
        data["sources"],
        json!([{"source": "Gemini", "price_usd": 100_300.0}])
    );
    assert_eq!(data["average_price"], 100_300.0);
    assert_eq!(data["spread"], 0.0);
    let stored = load_latest_snapshot(app.state.db_path.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.refreshed_source.as_deref(), Some("Gemini"));
    assert_eq!(data["warnings"].as_array().unwrap().len(), 3);
    for source in ["CoinGecko", "Coinbase", "Kraken"] {
        assert!(
            data["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|warning| warning.as_str().unwrap().contains(source))
        );
    }
    assert_eq!(app.upstreams.request_counts(), [1; 4]);
    let (_, cached) = app.price().await;
    assert_eq!(cached["warnings"], data["warnings"]);
    assert_eq!(cached["sources"], data["sources"]);
    assert_eq!(cached["refresh_succeeded"], false);
    assert_eq!(app.upstreams.request_counts(), [1; 4]);
}

#[tokio::test]
async fn total_provider_failure_preserves_the_database_and_reports_every_error() {
    for seeded in [false, true] {
        let app = TestApp::new().await;
        app.presence(true).await;
        if seeded {
            // Include a warning in the stored snapshot so preservation covers metadata too.
            app.upstreams
                .provider(UpstreamSource::CoinGecko)
                .set_response(FixtureResponse::json(json!({})));
            assert_eq!(app.price().await.0, StatusCode::OK);
            app.presence(false).await;
            app.clock.advance(Duration::from_secs(10));
            app.presence(true).await;
        }
        for source in UpstreamSource::ALL {
            app.upstreams
                .provider(source)
                .set_response(FixtureResponse::json(json!({})));
        }
        let before = std::fs::read(&app.state.db_path).unwrap();
        let (status, data) = app.price().await;
        assert_eq!(
            status,
            if seeded {
                StatusCode::OK
            } else {
                StatusCode::SERVICE_UNAVAILABLE
            }
        );
        assert_eq!(data["refresh_succeeded"], false);
        assert_eq!(std::fs::read(&app.state.db_path).unwrap(), before);
        for source in UpstreamSource::ALL {
            assert!(
                data["warnings"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|warning| warning.as_str().unwrap().contains(source.name()))
            );
        }
        assert_eq!(
            app.upstreams.request_counts(),
            [if seeded { 2 } else { 1 }; 4]
        );
        let counts = app.upstreams.request_counts();
        assert_eq!(app.price().await.1["refresh_succeeded"], false);
        assert_eq!(app.upstreams.request_counts(), counts);
    }
}

#[tokio::test]
async fn partial_refresh_write_failure_preserves_quotes_and_both_error_types() {
    let app = TestApp::new().await;
    app.presence(true).await;
    let (_, original) = app.price().await;
    let conn = rusqlite::Connection::open(&app.state.db_path).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER reject_snapshot BEFORE INSERT ON price_snapshots
        BEGIN SELECT RAISE(ABORT, 'private error at /private/fixture.db'); END;",
    )
    .unwrap();
    app.upstreams
        .provider(UpstreamSource::CoinGecko)
        .set_response(FixtureResponse::json(json!({})));
    app.upstreams
        .provider(UpstreamSource::Coinbase)
        .set_response(FixtureResponse::json(json!({"data": {"amount": "120000"}})));
    app.presence(false).await;
    app.clock.advance(Duration::from_secs(10));
    app.presence(true).await;
    let before = std::fs::read(&app.state.db_path).unwrap();
    let (status, data) = app.price().await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(data["refresh_succeeded"], false);
    assert_eq!(data["sources"], original["sources"]);
    assert_eq!(data["fetched_at_unix"], TEST_NOW);
    assert_eq!(std::fs::read(&app.state.db_path).unwrap(), before);
    assert_eq!(data["warnings"].as_array().unwrap().len(), 2);
    assert!(
        data["warnings"]
            .to_string()
            .contains("CoinGecko response invalid")
    );
    assert!(
        data["warnings"]
            .to_string()
            .contains("Failed to store refreshed price data")
    );
    assert!(!data.to_string().contains("private error"));
    assert!(!data.to_string().contains("/private/fixture.db"));
    assert_eq!(app.upstreams.request_counts(), [2; 4]);
    let (_, cached) = app.price().await;
    assert!(
        cached["refresh_skipped_reason"]
            .as_str()
            .unwrap()
            .contains("last attempt")
    );
    assert_eq!(app.upstreams.request_counts(), [2; 4]);

    conn.execute_batch("DROP TRIGGER reject_snapshot;").unwrap();
    app.upstreams
        .provider(UpstreamSource::CoinGecko)
        .set_response(FixtureResponse::json(json!({"bitcoin": {"usd": 120000}})));
    app.clock.advance(Duration::from_secs(10));
    let (_, recovered) = app.price().await;
    assert_eq!(recovered["refresh_succeeded"], true);
    assert_eq!(recovered["sources"][0]["price_usd"], 120_000.0);
    assert_eq!(recovered["sources"][1]["price_usd"], 100_100.0);
    assert_eq!(app.upstreams.request_counts(), [3, 2, 2, 2]);
}

#[tokio::test]
async fn full_refresh_timeout_keeps_three_successful_quotes() {
    let app = TestApp::new().await;
    let provider = app.upstreams.provider(UpstreamSource::CoinGecko);
    let release = provider.hold_responses();
    app.presence(true).await;
    // Exercise the real five-second deadline: the other local responses complete
    // while this provider remains held, without advancing their timers prematurely.
    let (status, data) = app.price().await;
    release.add_permits(1);
    assert_eq!(status, StatusCode::OK);
    assert_eq!(data["refresh_succeeded"], true);
    assert_eq!(
        data["sources"],
        json!([
            {"source": "Coinbase", "price_usd": 100_100.0},
            {"source": "Kraken", "price_usd": 100_200.0},
            {"source": "Gemini", "price_usd": 100_300.0},
        ])
    );
    assert_eq!(data["average_price"], 100_200.0);
    assert_eq!(data["warnings"], json!(["CoinGecko request timed out"]));
    assert_eq!(app.upstreams.request_counts(), [1; 4]);
}

#[tokio::test]
async fn partial_full_refresh_merges_successes_with_retained_quotes() {
    let app = TestApp::new().await;
    app.presence(true).await;
    assert_eq!(app.price().await.0, StatusCode::OK);
    app.upstreams
        .provider(UpstreamSource::CoinGecko)
        .set_response(FixtureResponse::json(
            json!({"bitcoin": {"usd": 110_000.0}}),
        ));
    app.upstreams
        .provider(UpstreamSource::Kraken)
        .set_response(FixtureResponse::json(
            json!({"result": {"XXBTZUSD": {"c": ["110200"]}}}),
        ));
    for source in [UpstreamSource::Coinbase, UpstreamSource::Gemini] {
        app.upstreams
            .provider(source)
            .set_response(FixtureResponse::json(json!({})));
    }
    app.presence(false).await;
    app.clock.advance(Duration::from_secs(10));
    app.presence(true).await;
    let (status, data) = app.price().await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(data["refresh_succeeded"], true);
    assert_eq!(
        data["sources"],
        json!([
            {"source": "CoinGecko", "price_usd": 110_000.0},
            {"source": "Coinbase", "price_usd": 100_100.0},
            {"source": "Kraken", "price_usd": 110_200.0},
            {"source": "Gemini", "price_usd": 100_300.0},
        ])
    );
    assert_eq!(data["average_price"], 105_150.0);
    assert_eq!(data["spread"], 10_100.0);
    let stored = load_latest_snapshot(app.state.db_path.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.refreshed_source.as_deref(), Some("partial"));
    assert_eq!(data["fetched_at_unix"], TEST_NOW + 10);
    assert_eq!(data["warnings"].as_array().unwrap().len(), 2);
    assert_eq!(app.upstreams.request_counts(), [2; 4]);
}

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
async fn fresh_snapshot_after_restart_defers_pending_full_refresh_until_ten_seconds() {
    let mut app = TestApp::new().await;
    app.presence(true).await;
    let (_, original) = app.price().await;
    app.restart().await;
    let (_, dormant) = app.price().await;
    assert_eq!(dormant["active_viewers"], 0);
    assert_eq!(dormant["sources"], original["sources"]);
    app.presence(true).await;
    for elapsed in [0, 9] {
        app.clock.advance(Duration::from_secs(elapsed));
        let (_, cached) = app.price().await;
        assert_eq!(cached["refresh_succeeded"], false);
        assert_eq!(cached["fetched_at_unix"], TEST_NOW);
        assert_eq!(cached["sources"], original["sources"]);
        assert_eq!(app.upstreams.request_counts(), [1; 4]);
    }
    app.clock.advance(Duration::from_secs(1));
    let (_, refreshed) = app.price().await;
    assert_eq!(refreshed["refresh_succeeded"], true);
    assert_eq!(refreshed["fetched_at_unix"], TEST_NOW + 10);
    assert_eq!(app.upstreams.request_counts(), [2; 4]);
}

#[tokio::test]
async fn restart_resets_retry_state_but_preserves_partial_snapshot_success_gate() {
    use axum::http::{HeaderValue, header::RETRY_AFTER};

    let mut app = TestApp::new().await;
    let mut limited = FixtureResponse::json(json!({}));
    limited.status = StatusCode::TOO_MANY_REQUESTS;
    limited
        .headers
        .insert(RETRY_AFTER, HeaderValue::from_static("300"));
    app.upstreams
        .provider(UpstreamSource::CoinGecko)
        .set_response(limited);
    app.presence(true).await;
    let (_, partial) = app.price().await;
    assert_eq!(partial["sources"].as_array().unwrap().len(), 3);
    app.restart().await;
    app.presence(true).await;
    let (_, cached) = app.price().await;
    assert_eq!(cached["sources"], partial["sources"]);
    assert_eq!(cached["warnings"], partial["warnings"]);
    assert_eq!(cached["refresh_succeeded"], false);
    assert_eq!(app.upstreams.request_counts(), [1; 4]);
    app.upstreams
        .provider(UpstreamSource::CoinGecko)
        .set_response(FixtureResponse::json(json!({"bitcoin": {"usd": 100000}})));
    app.clock.advance(Duration::from_secs(10));
    let (_, recovered) = app.price().await;
    assert_eq!(recovered["refresh_succeeded"], true);
    assert_eq!(recovered["sources"].as_array().unwrap().len(), 4);
    assert_eq!(recovered["warnings"], json!([]));
    assert_eq!(app.upstreams.request_counts(), [2; 4]);
}

#[tokio::test]
async fn failed_attempt_frequency_does_not_increase_with_viewer_count_or_request_bursts() {
    for viewer_count in [1, 16] {
        let app = TestApp::new().await;
        for source in UpstreamSource::ALL {
            app.upstreams
                .provider(source)
                .set_response(FixtureResponse::json(json!({})));
        }
        for tick in 0..=8 {
            if tick > 0 {
                app.clock.advance(Duration::from_secs(5));
            }
            for viewer in 0..viewer_count {
                app.presence_for(&format!("viewer-{viewer}"), true).await;
            }
            let (a, b, c, d) = tokio::join!(app.price(), app.price(), app.price(), app.price());
            for (status, data) in [a, b, c, d] {
                assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
                assert_eq!(data["active_viewers"], viewer_count);
                assert_eq!(data["refresh_succeeded"], false);
            }
            let mut expected = [1; 4];
            // One full batch at t=0, then exactly one provider at t=10/20/30/40.
            for count in expected.iter_mut().take(tick / 2) {
                *count += 1;
            }
            assert_eq!(app.upstreams.request_counts(), expected);
        }
    }
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
async fn presence_ignores_wall_clock_jumps_and_expires_just_after_ttl() {
    use std::sync::atomic::Ordering;

    let app = TestApp::new().await;
    app.presence(true).await;
    app.price().await;
    app.clock.set_unix(TEST_NOW + 86_400);
    assert_eq!(active_viewer_count(&app.state).await, 1);
    app.clock.set_unix(TEST_NOW - 86_400);
    assert_eq!(active_viewer_count(&app.state).await, 1);
    app.clock.advance_monotonic(Duration::from_secs(15));
    assert_eq!(active_viewer_count(&app.state).await, 1);
    app.clock.advance_monotonic(Duration::from_nanos(1));
    let (_, expired) = app.price().await;
    assert_eq!(expired["active_viewers"], 0);
    assert_eq!(expired["refresh_succeeded"], false);
    assert_eq!(app.upstreams.request_counts(), [1; 4]);

    app.presence(true).await;
    assert_eq!(app.state.full_refresh_generation.load(Ordering::SeqCst), 2);
    assert_eq!(app.price().await.1["refresh_succeeded"], true);
    assert_eq!(app.upstreams.request_counts(), [2; 4]);
    app.presence(false).await;
    assert_eq!(active_viewer_count(&app.state).await, 0);
    app.presence(true).await;
    app.presence(true).await;
    assert_eq!(app.state.full_refresh_generation.load(Ordering::SeqCst), 3);
    // Hide/show requests a batch but still respects both cadence gates.
    assert_eq!(app.price().await.1["refresh_succeeded"], false);
    app.clock.advance(Duration::from_secs(10));
    assert_eq!(app.price().await.1["refresh_succeeded"], true);
    assert_eq!(app.upstreams.request_counts(), [3; 4]);
}

#[tokio::test]
async fn presence_samples_time_after_acquiring_the_viewer_lock() {
    use std::{
        future::{Future, poll_fn},
        task::Poll,
    };

    let app = TestApp::new().await;
    app.presence(true).await;
    let guard = app.state.viewers.lock().await;
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

    let guard = app.state.viewers.lock().await;
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

#[tokio::test]
async fn presence_expiring_during_database_inspection_prevents_dispatch() {
    use std::{
        future::{Future, poll_fn},
        task::Poll,
    };

    let app = TestApp::new().await;
    app.presence(true).await;
    app.price().await;
    app.clock.advance(Duration::from_secs(10));
    // Block snapshot inspection after the initial viewer check, before dispatch.
    let conn = rusqlite::Connection::open(&app.state.db_path).unwrap();
    conn.execute_batch("BEGIN EXCLUSIVE;").unwrap();
    let request = app.price();
    tokio::pin!(request);
    poll_fn(|cx| {
        assert!(request.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    assert!(app.state.refresh.try_lock().is_err());
    app.clock.advance_monotonic(Duration::from_secs(6));
    conn.execute_batch("COMMIT;").unwrap();
    let (_, data) = request.await;
    assert_eq!(data["active_viewers"], 0);
    assert!(
        data["refresh_skipped_reason"]
            .as_str()
            .unwrap()
            .contains("no active viewers")
    );
    assert_eq!(app.upstreams.request_counts(), [1; 4]);
}

#[tokio::test]
async fn dispatched_batch_can_finish_after_viewers_expire() {
    let app = TestApp::new().await;
    let provider = app.upstreams.provider(UpstreamSource::CoinGecko);
    let release = provider.hold_responses();
    app.presence(true).await;
    let request = app.price();
    tokio::pin!(request);
    tokio::select! {
        _ = provider.wait_for_request() => {}
        _ = &mut request => panic!("batch completed before provider release"),
    }
    app.clock.advance_monotonic(Duration::from_secs(16));
    assert_eq!(active_viewer_count(&app.state).await, 0);
    release.add_permits(1);
    let (_, completed) = request.await;
    assert_eq!(completed["active_viewers"], 0);
    assert_eq!(completed["refresh_succeeded"], true);
    assert_eq!(app.price().await.1["refresh_succeeded"], false);
    assert_eq!(app.upstreams.request_counts(), [1; 4]);
}

#[tokio::test]
async fn future_snapshot_has_unknown_age_and_does_not_bypass_attempt_gate() {
    let app = TestApp::new().await;
    app.presence(true).await;
    app.price().await;
    app.clock.set_unix(TEST_NOW - 3600);
    let (_, cached) = app.price().await;
    assert_eq!(cached["fetched_age_seconds"], serde_json::Value::Null);
    assert_eq!(cached["stale"], true);
    assert_eq!(cached["refresh_succeeded"], false);
    assert!(
        cached["refresh_skipped_reason"]
            .as_str()
            .unwrap()
            .contains("last attempt")
    );
    app.clock.advance_monotonic(Duration::from_secs(10));
    let (_, recovered) = app.price().await;
    assert_eq!(recovered["refresh_succeeded"], true);
    assert_eq!(recovered["fetched_at_unix"], TEST_NOW - 3600);
    assert_eq!(app.upstreams.request_counts(), [2, 1, 1, 1]);
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
