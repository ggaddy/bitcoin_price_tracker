use reqwest::{Client, Response, header::RETRY_AFTER};
use serde_json::Value;
use std::collections::HashMap;
use tokio::join;

use crate::{
    config::UpstreamEndpoints,
    errors::{ProviderError, ProviderErrorKind, RefreshError, RetryAfter},
    models::{SnapshotRecord, SourcePrice},
    refresh::RefreshPlan,
    state::AppState,
    storage::store_snapshot,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UpstreamSource {
    CoinGecko,
    Coinbase,
    Kraken,
    Gemini,
}

impl UpstreamSource {
    pub(crate) const ALL: [Self; 4] = [Self::CoinGecko, Self::Coinbase, Self::Kraken, Self::Gemini];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::CoinGecko => "CoinGecko",
            Self::Coinbase => "Coinbase",
            Self::Kraken => "Kraken",
            Self::Gemini => "Gemini",
        }
    }
}

async fn fetch_json(
    client: &Client,
    endpoint: &str,
    provider: UpstreamSource,
) -> Result<Value, ProviderError> {
    let response = client
        .get(endpoint)
        .send()
        .await
        .map_err(|error| ProviderError::from_reqwest(provider, error))?;
    decode_response(provider, response).await
}

async fn decode_response(
    provider: UpstreamSource,
    response: Response,
) -> Result<Value, ProviderError> {
    let status = response.status();
    if !status.is_success() {
        return Err(ProviderError {
            provider,
            kind: ProviderErrorKind::Http {
                status,
                retry_after: response
                    .headers()
                    .get(RETRY_AFTER)
                    .and_then(RetryAfter::parse),
            },
        });
    }
    response
        .json()
        .await
        .map_err(|error| ProviderError::from_reqwest(provider, error))
}

fn checked_price(
    provider: UpstreamSource,
    field: &'static str,
    price: f64,
) -> Result<SourcePrice, ProviderError> {
    if !price.is_finite() || price <= 0.0 {
        return Err(ProviderError::invalid_price(
            provider,
            field,
            "expected a finite positive number",
        ));
    }
    Ok(SourcePrice {
        source: provider.name().to_string(),
        price_usd: price,
    })
}

fn parse_price(
    provider: UpstreamSource,
    field: &'static str,
    amount: &str,
) -> Result<SourcePrice, ProviderError> {
    let price = amount
        .parse::<f64>()
        .map_err(|_| ProviderError::invalid_price(provider, field, "not a number"))?;
    checked_price(provider, field, price)
}

async fn fetch_coingecko(client: &Client, endpoint: &str) -> Result<SourcePrice, ProviderError> {
    let provider = UpstreamSource::CoinGecko;
    let value = fetch_json(client, endpoint, provider).await?;

    let price = value
        .get("bitcoin")
        .and_then(|entry| entry.get("usd"))
        .and_then(Value::as_f64)
        .ok_or_else(|| ProviderError::invalid_payload(provider, "missing numeric bitcoin.usd"))?;

    checked_price(provider, "bitcoin.usd", price)
}

async fn fetch_coinbase(client: &Client, endpoint: &str) -> Result<SourcePrice, ProviderError> {
    let provider = UpstreamSource::Coinbase;
    let value = fetch_json(client, endpoint, provider).await?;

    let amount = value
        .get("data")
        .and_then(|entry| entry.get("amount"))
        .and_then(Value::as_str)
        .ok_or_else(|| ProviderError::invalid_payload(provider, "missing string data.amount"))?;

    parse_price(provider, "data.amount", amount)
}

async fn fetch_kraken(client: &Client, endpoint: &str) -> Result<SourcePrice, ProviderError> {
    let provider = UpstreamSource::Kraken;
    let value = fetch_json(client, endpoint, provider).await?;

    let ticker = value
        .get("result")
        .and_then(Value::as_object)
        .and_then(|result| result.values().next())
        .ok_or_else(|| ProviderError::invalid_payload(provider, "missing result entry"))?;

    let close_str = ticker
        .get("c")
        .and_then(Value::as_array)
        .and_then(|entries| entries.first())
        .and_then(Value::as_str)
        .ok_or_else(|| ProviderError::invalid_payload(provider, "missing string result.*.c[0]"))?;

    parse_price(provider, "result.*.c[0]", close_str)
}

async fn fetch_gemini(client: &Client, endpoint: &str) -> Result<SourcePrice, ProviderError> {
    let provider = UpstreamSource::Gemini;
    let value = fetch_json(client, endpoint, provider).await?;

    let bid = value
        .get("bid")
        .and_then(Value::as_str)
        .ok_or_else(|| ProviderError::invalid_payload(provider, "missing string bid"))?;

    parse_price(provider, "bid", bid)
}

pub(crate) struct RefreshOutcome {
    // Keep every typed provider error even when persistence also fails.
    pub(crate) failures: Vec<ProviderError>,
    // Ok(false) means no provider supplied a quote, so no write was attempted.
    pub(crate) stored: Result<bool, RefreshError>,
}

pub(crate) async fn refresh_snapshot(
    state: &AppState,
    latest_snapshot: Option<SnapshotRecord>,
    plan: &RefreshPlan,
) -> RefreshOutcome {
    let results = fetch_selected_sources(&state.client, &state.endpoints, &plan.sources).await;
    let mut refreshed = Vec::new();
    let mut failures = Vec::new();
    for result in results {
        match result {
            Ok(price) => refreshed.push(price),
            Err(error) => failures.push(error),
        }
    }
    if refreshed.is_empty() {
        return RefreshOutcome {
            failures,
            stored: Ok(false),
        };
    }

    let refreshed_source = match refreshed.as_slice() {
        [price] => price.source.clone(),
        prices if prices.len() == UpstreamSource::ALL.len() => "all".to_string(),
        _ => "partial".to_string(),
    };
    let sources = merge_snapshot_sources(latest_snapshot.as_ref(), refreshed);
    let (average_price, spread) = summarize_prices(&sources);
    let snapshot = SnapshotRecord {
        fetched_at_unix: state.clock.now_unix(),
        sources,
        average_price,
        spread,
        warnings: failures.iter().map(ToString::to_string).collect(),
        refreshed_source: Some(refreshed_source),
    };
    let stored = store_snapshot(state.db_path.clone(), snapshot)
        .await
        .map(|()| true)
        .map_err(RefreshError);
    RefreshOutcome { failures, stored }
}

async fn fetch_round_robin_source(
    client: &Client,
    endpoints: &UpstreamEndpoints,
    source: UpstreamSource,
) -> Result<SourcePrice, ProviderError> {
    match source {
        UpstreamSource::CoinGecko => fetch_coingecko(client, &endpoints.coingecko).await,
        UpstreamSource::Coinbase => fetch_coinbase(client, &endpoints.coinbase).await,
        UpstreamSource::Kraken => fetch_kraken(client, &endpoints.kraken).await,
        UpstreamSource::Gemini => fetch_gemini(client, &endpoints.gemini).await,
    }
}

async fn fetch_selected_sources(
    client: &Client,
    endpoints: &UpstreamEndpoints,
    sources: &[UpstreamSource],
) -> Vec<Result<SourcePrice, ProviderError>> {
    let fetch = |source| async move {
        if sources.contains(&source) {
            Some(fetch_round_robin_source(client, endpoints, source).await)
        } else {
            None
        }
    };
    let (coingecko, coinbase, kraken, gemini) = join!(
        fetch(UpstreamSource::CoinGecko),
        fetch(UpstreamSource::Coinbase),
        fetch(UpstreamSource::Kraken),
        fetch(UpstreamSource::Gemini)
    );
    [coingecko, coinbase, kraken, gemini]
        .into_iter()
        .flatten()
        .collect()
}

fn merge_snapshot_sources(
    latest_snapshot: Option<&SnapshotRecord>,
    refreshed_prices: Vec<SourcePrice>,
) -> Vec<SourcePrice> {
    let mut source_prices = HashMap::new();

    if let Some(snapshot) = latest_snapshot {
        for source in &snapshot.sources {
            source_prices.insert(source.source.clone(), source.price_usd);
        }
    }

    for refreshed_price in refreshed_prices {
        source_prices.insert(refreshed_price.source, refreshed_price.price_usd);
    }
    ordered_sources(source_prices)
}

fn ordered_sources(mut source_prices: HashMap<String, f64>) -> Vec<SourcePrice> {
    let mut sources = Vec::new();

    for source in UpstreamSource::ALL {
        if let Some(price_usd) = source_prices.remove(source.name()) {
            sources.push(SourcePrice {
                source: source.name().to_string(),
                price_usd,
            });
        }
    }

    let mut extras: Vec<_> = source_prices
        .into_iter()
        .map(|(source, price_usd)| SourcePrice { source, price_usd })
        .collect();
    extras.sort_by(|left, right| left.source.cmp(&right.source));
    sources.extend(extras);

    sources
}

fn summarize_prices(sources: &[SourcePrice]) -> (Option<f64>, Option<f64>) {
    if sources.is_empty() {
        return (None, None);
    }

    let total: f64 = sources.iter().map(|price| price.price_usd).sum();
    let average = total / sources.len() as f64;
    let min = sources
        .iter()
        .map(|price| price.price_usd)
        .fold(f64::INFINITY, f64::min);
    let max = sources
        .iter()
        .map(|price| price.price_usd)
        .fold(f64::NEG_INFINITY, f64::max);

    (Some(average), Some(max - min))
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderValue, StatusCode, header};
    use reqwest::{Client, Response};
    use serde_json::json;
    use std::{error::Error, time::Duration};
    use tokio::{
        io::AsyncWriteExt,
        net::{TcpListener, TcpStream},
        time,
    };

    use super::{UpstreamSource, decode_response, fetch_json, fetch_round_robin_source};
    use crate::{
        config::{UPSTREAM_CONNECT_TIMEOUT, UPSTREAM_REQUEST_TIMEOUT, upstream_client_builder},
        errors::{ProviderErrorKind, RetryAfter},
        test_support::{FixtureResponse, TestApp},
    };

    #[tokio::test]
    async fn provider_fixtures_support_http_errors_malformed_json_and_recovery() {
        let app = TestApp::new().await;
        let provider = app.upstreams.provider(UpstreamSource::CoinGecko);
        let fetch = || {
            fetch_round_robin_source(
                &app.state.client,
                &app.state.endpoints,
                UpstreamSource::CoinGecko,
            )
        };
        let mut limited = FixtureResponse::json(json!({"error": "rate limited"}));
        limited.status = StatusCode::TOO_MANY_REQUESTS;
        limited
            .headers
            .insert(header::RETRY_AFTER, HeaderValue::from_static("20"));
        provider.set_response(limited);

        let error = fetch().await.err().expect("expected an HTTP error");
        assert_eq!(error.provider, UpstreamSource::CoinGecko);
        assert!(
            matches!(&error.kind, ProviderErrorKind::Http {
            status: StatusCode::TOO_MANY_REQUESTS,
            retry_after: Some(RetryAfter::Delay(delay))
        } if *delay == Duration::from_secs(20)),
            "{error:?}"
        );
        assert_eq!(provider.request_count(), 1);

        let mut malformed = FixtureResponse::json(json!(null));
        malformed.body = "not json".to_string();
        provider.set_response(malformed);
        let error = fetch().await.err().expect("expected a JSON error");
        assert!(
            matches!(
                error.kind,
                ProviderErrorKind::InvalidPayload { cause: Some(_), .. }
            ),
            "{error:?}"
        );
        assert!(error.source().is_some());
        assert_eq!(provider.request_count(), 2);

        provider.set_response(FixtureResponse::json(
            json!({"bitcoin": {"usd": 101_000.0}}),
        ));
        let recovered = fetch().await.unwrap();
        assert_eq!(recovered.source, "CoinGecko");
        assert_eq!(recovered.price_usd, 101_000.0);
        assert_eq!(provider.request_count(), 3);
    }

    #[tokio::test]
    async fn total_deadline_limits_a_provider_that_never_sends_headers() {
        let app = TestApp::new().await;
        let provider = app.upstreams.provider(UpstreamSource::CoinGecko);
        let release = provider.hold_responses();
        let fetch = fetch_round_robin_source(
            &app.state.client,
            &app.state.endpoints,
            UpstreamSource::CoinGecko,
        );
        tokio::pin!(fetch);
        tokio::select! {
            _ = provider.wait_for_request() => {}
            _ = &mut fetch => panic!("provider completed before release"),
        }
        time::pause();
        time::advance(UPSTREAM_REQUEST_TIMEOUT).await;
        let error = time::timeout(Duration::from_millis(10), fetch)
            .await
            .expect("total deadline fired")
            .err()
            .expect("expected a timeout");
        assert!(
            matches!(error.kind, ProviderErrorKind::Timeout(_)),
            "{error:?}"
        );
        assert!(error.source().is_some());
        release.add_permits(1);
    }

    #[tokio::test]
    async fn connect_deadline_limits_a_stalled_tls_handshake() {
        let client = upstream_client_builder().no_proxy().build().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("https://{}", listener.local_addr().unwrap());
        let fetch = fetch_json(&client, &endpoint, UpstreamSource::CoinGecko);
        tokio::pin!(fetch);
        let (_socket, _) = tokio::select! {
            accepted = listener.accept() => accepted.unwrap(),
            _ = &mut fetch => panic!("request completed before TLS handshake"),
        };
        // Keep the accepted socket open without replying to the TLS handshake.
        time::pause();
        time::advance(UPSTREAM_CONNECT_TIMEOUT).await;
        let error = time::timeout(Duration::from_millis(10), fetch)
            .await
            .expect("connect deadline fired before total deadline")
            .expect_err("expected a timeout");
        assert!(
            matches!(error.kind, ProviderErrorKind::Timeout(_)),
            "{error:?}"
        );
    }

    async fn partial_http_response(client: &Client) -> (Response, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/private-fixture", listener.local_addr().unwrap());
        let (response, socket) = tokio::join!(client.get(endpoint).send(), async {
            let (mut socket, _) = listener.accept().await.unwrap();
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{")
                .await
                .unwrap();
            socket
        });
        (response.unwrap(), socket)
    }

    #[tokio::test]
    async fn total_deadline_also_covers_the_response_body() {
        let client = upstream_client_builder().no_proxy().build().unwrap();
        let (response, _socket) = partial_http_response(&client).await;
        assert_eq!(response.status(), StatusCode::OK);
        time::pause();
        time::advance(UPSTREAM_REQUEST_TIMEOUT).await;
        let error = time::timeout(
            Duration::from_millis(10),
            decode_response(UpstreamSource::CoinGecko, response),
        )
        .await
        .expect("body deadline fired")
        .unwrap_err();
        assert!(
            matches!(error.kind, ProviderErrorKind::Timeout(_)),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn truncated_body_is_a_transport_error_with_a_safe_public_message() {
        let client = upstream_client_builder().no_proxy().build().unwrap();
        let (response, socket) = partial_http_response(&client).await;
        drop(socket);
        let error = decode_response(UpstreamSource::CoinGecko, response)
            .await
            .unwrap_err();
        assert!(
            matches!(error.kind, ProviderErrorKind::Transport(_)),
            "{error:?}"
        );
        assert!(error.source().is_some());
        assert_eq!(error.to_string(), "CoinGecko request failed");
        assert!(!error.to_string().contains("private-fixture"));
    }

    #[tokio::test]
    async fn missing_provider_fields_are_invalid_payloads() {
        let app = TestApp::new().await;
        for source in UpstreamSource::ALL {
            app.upstreams
                .provider(source)
                .set_response(FixtureResponse::json(json!({})));
            let error = fetch_round_robin_source(&app.state.client, &app.state.endpoints, source)
                .await
                .err()
                .expect("expected a schema error");
            assert_eq!(error.provider, source);
            assert!(
                matches!(
                    error.kind,
                    ProviderErrorKind::InvalidPayload { cause: None, .. }
                ),
                "{error:?}"
            );
        }
    }

    #[tokio::test]
    async fn invalid_numeric_prices_are_rejected_by_each_provider() {
        let app = TestApp::new().await;
        for amount in ["0", "-1", "NaN", "inf", "-inf", "not-a-price"] {
            for (source, body) in [
                (
                    UpstreamSource::Coinbase,
                    json!({"data": {"amount": amount}}),
                ),
                (
                    UpstreamSource::Kraken,
                    json!({"result": {"XXBTZUSD": {"c": [amount]}}}),
                ),
                (UpstreamSource::Gemini, json!({"bid": amount})),
            ] {
                app.upstreams
                    .provider(source)
                    .set_response(FixtureResponse::json(body));
                let error =
                    fetch_round_robin_source(&app.state.client, &app.state.endpoints, source)
                        .await
                        .err()
                        .expect("expected an invalid price");
                assert_eq!(error.provider, source);
                assert!(
                    matches!(error.kind, ProviderErrorKind::InvalidPrice { .. }),
                    "{error:?}"
                );
            }
        }
        for amount in [0.0, -1.0] {
            app.upstreams
                .provider(UpstreamSource::CoinGecko)
                .set_response(FixtureResponse::json(json!({"bitcoin": {"usd": amount}})));
            let error = fetch_round_robin_source(
                &app.state.client,
                &app.state.endpoints,
                UpstreamSource::CoinGecko,
            )
            .await
            .err()
            .expect("expected an invalid price");
            assert!(
                matches!(error.kind, ProviderErrorKind::InvalidPrice { .. }),
                "{error:?}"
            );
        }
    }
}
