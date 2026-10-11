use crate::{
    config::RuntimeConfig, lifecycle::serve_until, pricing::UpstreamSource, test_support::TestApp,
};
use axum::{
    Router,
    http::{Method, StatusCode},
    routing::get,
};
use serde_json::json;
use std::{sync::atomic::Ordering, time::Duration};
use tokio::{net::TcpListener, sync::oneshot};

#[tokio::test]
async fn health_is_read_only_and_does_not_activate_providers() {
    let app = TestApp::new().await;
    let (status, data) = app.request(Method::GET, "/health", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(data, json!({"status": "ok"}));
    assert_eq!(app.upstreams.request_counts(), [0; 4]);
    assert_eq!(app.state.full_refresh_generation.load(Ordering::SeqCst), 0);
    assert!(app.state.viewers.lock().await.is_empty());
    let moved = app.state.db_path.with_extension("saved");
    std::fs::rename(&app.state.db_path, &moved).unwrap();
    let (status, data) = app.request(Method::GET, "/health", None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(data, json!({"status": "unavailable"}));
    assert!(
        !app.state.db_path.exists(),
        "health must not create a missing database"
    );
}

#[tokio::test]
async fn viewer_cap_preserves_existing_heartbeats_removal_and_expiry() {
    let mut app = TestApp::new().await;
    app.configure(RuntimeConfig {
        max_viewers: 2,
        ..RuntimeConfig::default()
    });
    app.presence_for("one", true).await;
    app.presence_for("two", true).await;
    let (status, data) = app
        .request(
            Method::POST,
            "/api/presence",
            Some(json!({"session_id": "three", "active": true})),
        )
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(data["active_viewers"], 2);
    assert!(data["error"].as_str().unwrap().contains("capacity"));
    app.presence_for("one", true).await;
    app.presence_for("two", false).await;
    app.presence_for("three", true).await;
    app.clock.advance(Duration::from_secs(16));
    app.presence_for("four", true).await;
    let viewers = app.state.viewers.lock().await;
    assert_eq!(viewers.len(), 1);
    assert!(viewers.contains_key("four"));
    assert_eq!(app.state.full_refresh_generation.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn rate_limits_have_independent_health_and_presence_capacity_and_do_not_bypass_cadence() {
    let mut app = TestApp::new().await;
    app.configure(RuntimeConfig {
        requests_per_second: 2,
        ..RuntimeConfig::default()
    });
    app.presence(true).await;
    assert_eq!(app.price().await.0, StatusCode::SERVICE_UNAVAILABLE);
    app.wait_for_refresh().await;
    assert_eq!(app.price().await.0, StatusCode::OK);
    let (status, data) = app.price().await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(data["error"].as_str().unwrap().contains("rate"));
    assert_eq!(
        app.request(Method::GET, "/health", None).await.0,
        StatusCode::OK
    );
    app.presence(true).await;
    app.clock.advance(Duration::from_millis(500));
    assert_eq!(app.price().await.0, StatusCode::OK);
    assert_eq!(app.upstreams.request_counts(), [1; 4]);
}

#[tokio::test]
async fn concurrent_refresh_allows_cached_reads_while_overload_is_bounded() {
    let mut app = TestApp::new().await;
    app.configure(RuntimeConfig {
        concurrency: 2,
        ..RuntimeConfig::default()
    });
    app.presence(true).await;
    app.refresh_and_price().await;
    app.clock.advance(Duration::from_secs(10));
    let provider = app.upstreams.provider(UpstreamSource::CoinGecko);
    let gate = provider.hold_responses();
    let request = app.refresh_and_price();
    tokio::pin!(request);
    // Drain permits from the successful initial request, then observe refresh dispatch.
    provider.wait_for_request().await;
    tokio::select! {
        _ = provider.wait_for_request() => {},
        _ = &mut request => panic!("held refresh completed"),
    }
    for _ in 0..5 {
        let (status, data) = app.price().await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(data["average_price"], 100150.0);
    }
    assert_eq!(
        app.request(Method::GET, "/health", None).await.0,
        StatusCode::OK
    );
    gate.add_permits(1);
    assert_eq!(request.await.0, StatusCode::OK);
    assert_eq!(app.upstreams.request_counts(), [2, 1, 1, 1]);
}

#[tokio::test]
async fn background_refresh_does_not_occupy_the_only_request_slot() {
    let mut app = TestApp::new().await;
    app.configure(RuntimeConfig {
        concurrency: 1,
        ..RuntimeConfig::default()
    });
    app.presence(true).await;
    let provider = app.upstreams.provider(UpstreamSource::CoinGecko);
    let gate = provider.hold_responses();
    let (status, first) = app.price().await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(first["status"], "UNAVAILABLE");
    provider.wait_for_request().await;
    for _ in 0..5 {
        let (status, data) = tokio::time::timeout(Duration::from_secs(1), app.price())
            .await
            .expect("request slots are released before upstream work ends");
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(data["status"], "UNAVAILABLE");
        assert_eq!(data["refreshing"], true);
        assert!(
            data["error"].is_null(),
            "cold state is not a concurrency rejection"
        );
    }
    assert_eq!(
        app.request(Method::GET, "/health", None).await.0,
        StatusCode::OK
    );
    app.presence(true).await;
    gate.add_permits(1);
    app.wait_for_refresh().await;
    assert_eq!(app.price().await.0, StatusCode::OK);
    assert_eq!(app.upstreams.request_counts(), [1; 4]);
}

#[tokio::test]
async fn tracker_shutdown_drains_a_background_refresh_after_its_http_response_finishes() {
    let app = TestApp::new().await;
    app.presence(true).await;
    let provider = app.upstreams.provider(UpstreamSource::CoinGecko);
    let release = provider.hold_responses();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop_send, stop_receive) = oneshot::channel();
    let state = app.state.clone();
    let server = tokio::spawn(crate::lifecycle::serve_tracker_until(
        listener,
        state,
        async {
            stop_receive.await.unwrap();
        },
        Duration::from_secs(10),
        crate::lifecycle::ConnectionLimits::default(),
    ));
    let response = reqwest::Client::new()
        .get(format!("http://{address}/api/price"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response.json::<serde_json::Value>().await.unwrap()["refreshing"],
        true
    );
    provider.wait_for_request().await;
    stop_send.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while !app.state.stopping.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        !server.is_finished(),
        "HTTP drain must also wait for the background commit"
    );
    release.add_permits(1);
    assert_eq!(server.await.unwrap(), Ok(()));
    let stored = crate::storage::load_price_state(app.state.db_path.clone())
        .await
        .unwrap();
    assert_eq!(stored.snapshot.unwrap().sources.len(), 4);
    assert_eq!(app.upstreams.request_counts(), [1; 4]);
    app.clock.advance(Duration::from_secs(10));
    let (_, stopped) = app.price().await;
    assert_eq!(stopped["refreshing"], false);
    assert!(
        stopped["refresh_skipped_reason"]
            .as_str()
            .unwrap()
            .contains("shutting down")
    );
    assert_eq!(app.upstreams.request_counts(), [1; 4]);
}

#[tokio::test]
async fn tracker_background_drain_uses_the_existing_shutdown_deadline() {
    let app = TestApp::new().await;
    let guard = app.state.refresh.clone().lock_owned().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server = tokio::spawn(crate::lifecycle::serve_tracker_until(
        listener,
        app.state.clone(),
        async {},
        Duration::from_secs(1),
        crate::lifecycle::ConnectionLimits::default(),
    ));
    while !app.state.stopping.load(Ordering::SeqCst) {
        tokio::task::yield_now().await;
    }
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(1)).await;
    tokio::time::resume();
    assert_eq!(
        server.await.unwrap(),
        Err("background price refresh shutdown deadline exceeded".into())
    );
    drop(guard);
}

#[tokio::test]
async fn shutdown_drains_an_in_flight_request() {
    let (entered_send, entered_receive) = oneshot::channel();
    let entered = std::sync::Arc::new(tokio::sync::Mutex::new(Some(entered_send)));
    let release = std::sync::Arc::new(tokio::sync::Semaphore::new(0));
    let handler_release = release.clone();
    let app = Router::new().route(
        "/",
        get(move || {
            let entered = entered.clone();
            let release = handler_release.clone();
            async move {
                entered.lock().await.take().unwrap().send(()).unwrap();
                release.acquire().await.unwrap().forget();
                "saved"
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = oneshot::channel();
    let server = tokio::spawn(serve_until(
        listener,
        app,
        async {
            let _ = stopped.await;
        },
        Duration::from_secs(2),
        Default::default(),
    ));
    let request = tokio::spawn(async move {
        reqwest::get(format!("http://{address}/"))
            .await
            .unwrap()
            .text()
            .await
            .unwrap()
    });
    entered_receive.await.unwrap();
    stop.send(()).unwrap();
    tokio::task::yield_now().await;
    assert!(!server.is_finished());
    release.add_permits(1);
    assert_eq!(request.await.unwrap(), "saved");
    assert!(server.await.unwrap().is_ok());
}

#[tokio::test]
async fn shutdown_has_a_deadline_for_stalled_requests() {
    let (entered_send, entered_receive) = oneshot::channel();
    let entered = std::sync::Arc::new(tokio::sync::Mutex::new(Some(entered_send)));
    let app = Router::new().route(
        "/",
        get(move || {
            let entered = entered.clone();
            async move {
                entered.lock().await.take().unwrap().send(()).unwrap();
                std::future::pending::<&'static str>().await
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = oneshot::channel();
    let server = tokio::spawn(serve_until(
        listener,
        app,
        async {
            let _ = stopped.await;
        },
        Duration::from_millis(20),
        Default::default(),
    ));
    let request = tokio::spawn(async move { reqwest::get(format!("http://{address}/")).await });
    entered_receive.await.unwrap();
    stop.send(()).unwrap();
    assert!(server.await.unwrap().unwrap_err().contains("deadline"));
    request.abort();
}

#[tokio::test(start_paused = true)]
async fn stalled_request_has_a_deadline_and_releases_its_slot() {
    use tower::ServiceExt;
    let app = TestApp::new().await;
    let router = Router::new()
        .route(
            "/stall",
            get(|| async { std::future::pending::<&'static str>().await }),
        )
        .route("/ready", get(|| async { "ready" }))
        .layer(axum::middleware::from_fn_with_state(
            app.state.clone(),
            crate::limits::limit_requests,
        ));
    let started = tokio::time::Instant::now();
    let response = router
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .uri("/stall")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
    assert_eq!(started.elapsed(), Duration::from_secs(20));
    let response = router
        .oneshot(
            axum::http::Request::builder()
                .uri("/ready")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn oversized_presence_body_is_rejected_without_registering_a_viewer() {
    use tower::ServiceExt;
    let app = TestApp::new().await;
    let response = crate::app::router(app.state.clone())
        .oneshot(
            axum::http::Request::builder()
                .method(Method::POST)
                .uri("/api/presence")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    json!({"session_id": "x".repeat(2000), "active": true}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert!(app.state.viewers.lock().await.is_empty());
    assert_eq!(app.upstreams.request_counts(), [0; 4]);
}

#[tokio::test]
async fn security_headers_cover_assets_errors_and_overload() {
    use tower::ServiceExt;
    let mut app = TestApp::new().await;
    app.configure(RuntimeConfig {
        requests_per_second: 1,
        ..Default::default()
    });
    let router = crate::app::router(app.state.clone());
    for path in [
        "/",
        "/assets/dashboard.css",
        "/assets/dashboard.js?v=2.9.1",
        "/missing",
        "/api/price",
        "/api/price",
    ] {
        let response = router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri(path)
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let headers = response.headers();
        let policy = headers["content-security-policy"].to_str().unwrap();
        assert!(policy.contains("script-src 'self'"));
        assert!(policy.contains("frame-ancestors 'none'"));
        assert!(!policy.contains("unsafe-inline"));
        assert_eq!(headers["x-content-type-options"], "nosniff");
        assert_eq!(
            headers["cache-control"],
            if path.starts_with("/api/") {
                "no-store"
            } else {
                "no-cache"
            }
        );
    }
}

#[tokio::test]
async fn news_overload_cannot_consume_asset_capacity() {
    use tower::ServiceExt;
    let mut app = TestApp::new().await;
    app.configure(RuntimeConfig {
        requests_per_second: 2,
        concurrency: 1,
        ..Default::default()
    });
    let release = std::sync::Arc::new(tokio::sync::Semaphore::new(0));
    let entered = std::sync::Arc::new(tokio::sync::Semaphore::new(0));
    let handler_release = release.clone();
    let handler_entered = entered.clone();
    let router = Router::new()
        .route(
            "/api/news",
            get(move || {
                let release = handler_release.clone();
                let entered = handler_entered.clone();
                async move {
                    entered.add_permits(1);
                    release.acquire().await.unwrap().forget();
                    "news"
                }
            }),
        )
        .route("/", get(|| async { "dashboard" }))
        .route("/assets/dashboard.js", get(crate::ui::dashboard_js))
        .layer(axum::middleware::from_fn_with_state(
            app.state.clone(),
            crate::limits::limit_requests,
        ));
    let request = |path| {
        axum::http::Request::builder()
            .uri(path)
            .body(axum::body::Body::empty())
            .unwrap()
    };
    let held = tokio::spawn(router.clone().oneshot(request("/api/news")));
    entered.acquire().await.unwrap().forget();
    assert_eq!(
        router
            .clone()
            .oneshot(request("/api/news"))
            .await
            .unwrap()
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        router
            .clone()
            .oneshot(request("/api/news"))
            .await
            .unwrap()
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    for path in ["/", "/assets/dashboard.js"] {
        assert_eq!(
            router
                .clone()
                .oneshot(request(path))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
    }
    release.add_permits(1);
    assert_eq!(held.await.unwrap().unwrap().status(), StatusCode::OK);
}

#[tokio::test]
async fn embedded_assets_revalidate_get_and_head_without_losing_security_headers() {
    use tower::ServiceExt;
    let app = TestApp::new().await;
    let router = crate::app::router(app.state.clone());
    let mut tags = Vec::new();
    for path in [
        "/assets/dashboard.js",
        "/assets/dashboard.css",
        "/assets/cybercore-0.3.0.min.css",
    ] {
        let request = |method, tag: Option<&str>| {
            let mut request = axum::http::Request::builder().method(method).uri(path);
            if let Some(tag) = tag {
                request = request.header("if-none-match", tag);
            }
            request.body(axum::body::Body::empty()).unwrap()
        };
        let response = router
            .clone()
            .oneshot(request(Method::GET, None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let tag = response.headers()["etag"].to_str().unwrap().to_string();
        let original = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(!original.is_empty());
        assert!(
            !tags.contains(&tag),
            "different embedded content needs a different validator"
        );
        tags.push(tag.clone());
        for matching in [
            tag.clone(),
            tag.trim_start_matches("W/").to_string(),
            format!("\"other\", {tag}"),
            "*".into(),
        ] {
            for method in [Method::GET, Method::HEAD] {
                let response = router
                    .clone()
                    .oneshot(request(method, Some(&matching)))
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
                assert_eq!(response.headers()["etag"], tag);
                assert_eq!(response.headers()["cache-control"], "no-cache");
                assert_eq!(response.headers()["x-content-type-options"], "nosniff");
                assert!(response.headers().contains_key("content-security-policy"));
                assert!(
                    axum::body::to_bytes(response.into_body(), usize::MAX)
                        .await
                        .unwrap()
                        .is_empty()
                );
            }
        }
        let response = router
            .clone()
            .oneshot(request(Method::GET, Some("W/\"previous-deployment\"")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["etag"], tag);
        assert_eq!(
            axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap(),
            original
        );
    }
}

#[tokio::test]
async fn incomplete_headers_release_connection_capacity_before_application_deadline() {
    use crate::lifecycle::ConnectionLimits;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpStream,
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = oneshot::channel();
    let server = tokio::spawn(serve_until(
        listener,
        Router::new().route("/health", get(|| async { "ok" })),
        async {
            let _ = stopped.await;
        },
        Duration::from_secs(1),
        ConnectionLimits {
            max_connections: 1,
            header_timeout: Duration::from_millis(200),
            lifetime: Duration::from_secs(2),
        },
    ));
    let mut slow = TcpStream::connect(address).await.unwrap();
    slow.write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nX-Slow: ")
        .await
        .unwrap();
    // Give the first accept task a chance to own the sole slot; the timeout
    // assertions below use real sockets rather than a router-only test seam.
    tokio::time::sleep(Duration::from_millis(20)).await;
    let mut refused = TcpStream::connect(address).await.unwrap();
    let mut byte = [0; 1];
    let result = tokio::time::timeout(Duration::from_secs(1), refused.read(&mut byte))
        .await
        .unwrap();
    assert!(
        matches!(result, Ok(0)) || result.is_err(),
        "excess connection must be closed"
    );
    let result = tokio::time::timeout(Duration::from_secs(1), slow.read(&mut byte))
        .await
        .unwrap();
    assert!(
        matches!(result, Ok(0)) || result.is_err(),
        "header timeout must close the socket"
    );
    let response = reqwest::get(format!("http://{address}/health"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["connection"], "close");
    assert_eq!(response.text().await.unwrap(), "ok");
    stop.send(()).unwrap();
    assert!(server.await.unwrap().is_ok());
}

#[tokio::test]
async fn connection_lifetime_bounds_a_stalled_handler_and_frees_its_slot() {
    use crate::lifecycle::ConnectionLimits;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = oneshot::channel();
    let router = Router::new()
        .route(
            "/stall",
            get(|| async { std::future::pending::<&'static str>().await }),
        )
        .route("/health", get(|| async { "ok" }));
    let server = tokio::spawn(serve_until(
        listener,
        router,
        async {
            let _ = stopped.await;
        },
        Duration::from_secs(1),
        ConnectionLimits {
            max_connections: 1,
            header_timeout: Duration::from_millis(50),
            lifetime: Duration::from_millis(200),
        },
    ));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    assert!(
        client
            .get(format!("http://{address}/stall"))
            .send()
            .await
            .is_err()
    );
    assert_eq!(
        client
            .get(format!("http://{address}/health"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    stop.send(()).unwrap();
    assert!(server.await.unwrap().is_ok());
}
