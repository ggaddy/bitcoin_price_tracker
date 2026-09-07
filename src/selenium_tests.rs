use std::{
    fs,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use thirtyfour::prelude::*;

use crate::{
    app::router,
    config::upstream_client_builder,
    models::{
        SnapshotRecord,
        source_contract::{QuoteKind, StoredQuote},
    },
    state::AppState,
    storage::{init_db, store_snapshot},
    util::now_unix,
};

fn temp_db_path(test_name: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());

    std::env::temp_dir().join(format!("bitcoin-price-tracker-{test_name}-{unique}.db"))
}

fn browser_test_host() -> String {
    std::env::var("SELENIUM_APP_HOST").unwrap_or_else(|_| "host.containers.internal".to_string())
}

async fn build_test_driver() -> WebDriverResult<WebDriver> {
    let webdriver_url =
        std::env::var("WEBDRIVER_URL").unwrap_or_else(|_| "http://127.0.0.1:4444".to_string());
    let mut caps = DesiredCapabilities::chrome();
    caps.set_headless()?;
    caps.set_no_sandbox()?;
    caps.set_disable_gpu()?;

    WebDriver::new(webdriver_url, caps).await
}

async fn wait_for_text(driver: &WebDriver, element_id: &str) -> WebDriverResult<String> {
    for _ in 0..30 {
        let text = driver.find(By::Id(element_id)).await?.text().await?;
        if text != "Loading..." && text != "Connecting" && text != "-" {
            return Ok(text);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    driver.find(By::Id(element_id)).await?.text().await
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires a WebDriver server such as Selenium or chromedriver"]
async fn selenium_dashboard_smoke_test() -> WebDriverResult<()> {
    let db_path = temp_db_path("selenium");
    init_db(db_path.clone())
        .await
        .expect("init db for selenium test");
    store_snapshot(
        db_path.clone(),
        SnapshotRecord {
            fetched_at_unix: now_unix(),
            sources: vec![StoredQuote {
                source: "CoinGecko".to_string(),
                price_usd: 70_800.53,
                last_success_at_unix: None,
                quote_kind: QuoteKind::Unknown,
            }],
            average_price: Some(70_800.53),
            spread: Some(0.0),
            warnings: Vec::new(),
            refreshed_source: Some("CoinGecko".to_string()),
        },
    )
    .await
    .expect("seed sqlite snapshot for selenium test");

    let client = upstream_client_builder()
        .user_agent("bitcoin-price-tracker/selenium-test")
        .build()
        .expect("build http client for selenium test");
    let state = AppState::new(client, db_path.clone());
    let app = router(state);
    let listener = tokio::net::TcpListener::bind("0.0.0.0:0")
        .await
        .expect("bind ephemeral test listener");
    let address = listener
        .local_addr()
        .expect("resolve ephemeral test listener address");
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("run selenium smoke test server");
    });

    let driver = build_test_driver().await?;
    let result = async {
        let app_url = format!("http://{}:{}/", browser_test_host(), address.port());
        driver.goto(&app_url).await?;

        assert_eq!(driver.title().await?, "BTC Tracker");
        assert_eq!(
            driver.find(By::Css("h1.title")).await?.text().await?,
            "BTC TRACKER"
        );
        assert_eq!(
            driver.find(By::Css("p.sub")).await?.text().await?,
            "spot pricing feed"
        );

        let average = wait_for_text(&driver, "avg").await?;
        let status = wait_for_text(&driver, "status").await?;
        let source = wait_for_text(&driver, "sources").await?;

        assert!(
            average.contains("70,800.53"),
            "unexpected average text: {average}"
        );
        assert_eq!(status, "LIVE");
        assert!(
            source.contains("CoinGecko"),
            "unexpected source text: {source}"
        );

        WebDriverResult::Ok(())
    }
    .await;

    let quit_result = driver.quit().await;
    server.abort();
    let _ = fs::remove_file(&db_path);

    result?;
    quit_result
}
