use std::time::Duration;

use thirtyfour::prelude::*;

use crate::{app::router, test_support::TestApp};

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
async fn selenium_dashboard_smoke_test() -> Result<(), Box<dyn std::error::Error>> {
    // Local providers and a manual clock prevent accidental public upstream calls,
    // even when browser startup takes longer than the normal freshness window.
    let fixture = TestApp::new().await;
    fixture.presence(true).await;
    fixture.price().await;
    let app = router(fixture.state.clone());
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

    let driver = match build_test_driver().await {
        Ok(driver) => driver,
        Err(error) => {
            server.abort();
            let _ = server.await;
            return Err(error.into());
        }
    };
    // Assertions return errors so browser/server cleanup also runs on a mismatch.
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let app_url = format!("http://{}:{}/", browser_test_host(), address.port());
        driver.goto(&app_url).await?;
        let title = driver.title().await?;
        let heading = driver.find(By::Css("h1.title")).await?.text().await?;
        let average = wait_for_text(&driver, "avg").await?;
        let status = wait_for_text(&driver, "status").await?;
        let source = wait_for_text(&driver, "sources").await?;
        if title != "BTC Tracker" || heading != "BTC TRACKER"
            || !average.replace(['\u{2009}', ','], "").contains("100150.00")
            || status != "LIVE" || !source.to_ascii_lowercase().contains("coingecko") {
            return Err(format!("Unexpected dashboard: title={title}, heading={heading}, average={average}, status={status}").into());
        }
        Ok(())
    }.await;

    let quit_result = driver.quit().await;
    server.abort();
    let _ = server.await;
    result?;
    quit_result?;
    Ok(())
}
