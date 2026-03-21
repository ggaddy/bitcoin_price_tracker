use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::get,
};
use reqwest::Client;
use serde::Serialize;
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::info;

#[derive(Clone)]
struct AppState {
    client: Client,
}

#[derive(Serialize)]
struct SourcePrice {
    source: &'static str,
    price_usd: f64,
}

#[derive(Serialize)]
struct PriceResponse {
    symbol: &'static str,
    currency: &'static str,
    sources: Vec<SourcePrice>,
    average_price: Option<f64>,
    spread: Option<f64>,
    fetched_at_unix: u64,
    warnings: Vec<String>,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG")
                .unwrap_or_else(|_| "bitcoin_price_tracker=debug,axum=info".into()),
        )
        .init();

    let client = Client::builder()
        .user_agent("bitcoin-price-tracker/0.1")
        .build()
        .expect("failed to build HTTP client");
    let state = AppState { client };

    let app = Router::new()
        .route("/", get(index))
        .route("/api/price", get(btc_prices))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000")
        .await
        .expect("failed to bind to 0.0.0.0:3000");
    info!("server listening on http://0.0.0.0:3000");

    axum::serve(listener, app)
        .await
        .expect("server exited unexpectedly");
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn btc_prices(State(state): State<AppState>) -> impl IntoResponse {
    let (coingecko, coinbase, kraken) = tokio::join!(
        fetch_coingecko(&state.client),
        fetch_coinbase(&state.client),
        fetch_kraken(&state.client),
    );

    let mut sources = Vec::new();
    let mut warnings = Vec::new();

    for result in [coingecko, coinbase, kraken] {
        match result {
            Ok(price) => sources.push(price),
            Err(err) => warnings.push(err),
        }
    }

    let (average_price, spread) = if sources.is_empty() {
        (None, None)
    } else {
        let total: f64 = sources.iter().map(|p| p.price_usd).sum();
        let average = total / sources.len() as f64;
        let min = sources
            .iter()
            .map(|p| p.price_usd)
            .fold(f64::INFINITY, f64::min);
        let max = sources
            .iter()
            .map(|p| p.price_usd)
            .fold(f64::NEG_INFINITY, f64::max);
        (Some(average), Some(max - min))
    };

    let status = if sources.is_empty() {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    };

    (
        status,
        Json(PriceResponse {
            symbol: "BTC",
            currency: "USD",
            sources,
            average_price,
            spread,
            fetched_at_unix: now_unix(),
            warnings,
        }),
    )
}

async fn fetch_coingecko(client: &Client) -> Result<SourcePrice, String> {
    let value: Value = client
        .get("https://api.coingecko.com/api/v3/simple/price?ids=bitcoin&vs_currencies=usd")
        .send()
        .await
        .map_err(|e| format!("CoinGecko request failed: {e}"))?
        .error_for_status()
        .map_err(|e| format!("CoinGecko HTTP error: {e}"))?
        .json()
        .await
        .map_err(|e| format!("CoinGecko parse failed: {e}"))?;

    let price = value
        .get("bitcoin")
        .and_then(|v| v.get("usd"))
        .and_then(Value::as_f64)
        .ok_or_else(|| "CoinGecko response missing bitcoin.usd".to_string())?;

    Ok(SourcePrice {
        source: "CoinGecko",
        price_usd: price,
    })
}

async fn fetch_coinbase(client: &Client) -> Result<SourcePrice, String> {
    let value: Value = client
        .get("https://api.coinbase.com/v2/prices/spot?currency=USD")
        .send()
        .await
        .map_err(|e| format!("Coinbase request failed: {e}"))?
        .error_for_status()
        .map_err(|e| format!("Coinbase HTTP error: {e}"))?
        .json()
        .await
        .map_err(|e| format!("Coinbase parse failed: {e}"))?;

    let amount = value
        .get("data")
        .and_then(|v| v.get("amount"))
        .and_then(Value::as_str)
        .ok_or_else(|| "Coinbase response missing data.amount".to_string())?;

    let price = amount
        .parse::<f64>()
        .map_err(|e| format!("Coinbase amount parse failed: {e}"))?;

    Ok(SourcePrice {
        source: "Coinbase",
        price_usd: price,
    })
}

async fn fetch_kraken(client: &Client) -> Result<SourcePrice, String> {
    let value: Value = client
        .get("https://api.kraken.com/0/public/Ticker?pair=XBTUSD")
        .send()
        .await
        .map_err(|e| format!("Kraken request failed: {e}"))?
        .error_for_status()
        .map_err(|e| format!("Kraken HTTP error: {e}"))?
        .json()
        .await
        .map_err(|e| format!("Kraken parse failed: {e}"))?;

    let ticker = value
        .get("result")
        .and_then(Value::as_object)
        .and_then(|result| result.values().next())
        .ok_or_else(|| "Kraken response missing result entry".to_string())?;

    let close_str = ticker
        .get("c")
        .and_then(Value::as_array)
        .and_then(|arr| arr.first())
        .and_then(Value::as_str)
        .ok_or_else(|| "Kraken response missing close price at result.*.c[0]".to_string())?;

    let price = close_str
        .parse::<f64>()
        .map_err(|e| format!("Kraken close parse failed: {e}"))?;

    Ok(SourcePrice {
        source: "Kraken",
        price_usd: price,
    })
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

const INDEX_HTML: &str = r##"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>BTC Matrix Tracker</title>
    <style>
      :root {
        --bg-0: #000000;
        --neon: #56ff75;
        --neon-soft: #3ccf57;
        --danger: #ff6868;
      }
      * {
        box-sizing: border-box;
      }
      body {
        margin: 0;
        min-height: 100vh;
        color: var(--neon);
        font-family: "Courier New", Courier, monospace;
        background: var(--bg-0);
        overflow-x: hidden;
      }
      canvas#rain {
        position: fixed;
        inset: 0;
        z-index: 0;
        opacity: 0.28;
      }
      main {
        position: relative;
        z-index: 1;
        max-width: 900px;
        margin: 0 auto;
        padding: 2rem 1rem 3rem;
      }
      .panel {
        border: 1px solid rgba(86, 255, 117, 0.45);
        background: linear-gradient(140deg, rgba(3, 16, 3, 0.9), rgba(8, 30, 8, 0.74));
        box-shadow: 0 0 0.8rem rgba(86, 255, 117, 0.25), inset 0 0 0.6rem rgba(86, 255, 117, 0.08);
        padding: 1rem;
        margin-bottom: 1rem;
        border-radius: 0.5rem;
      }
      .title {
        margin: 0 0 0.4rem;
        font-size: clamp(1.4rem, 4vw, 2.4rem);
        letter-spacing: 0.08em;
        text-shadow: 0 0 0.6rem rgba(86, 255, 117, 0.6);
      }
      .sub {
        margin: 0;
        color: var(--neon-soft);
      }
      .hero-price {
        margin: 0;
        font-size: clamp(2rem, 8vw, 4rem);
        text-shadow: 0 0 0.8rem rgba(86, 255, 117, 0.6);
      }
      .grid {
        display: grid;
        grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
        gap: 0.8rem;
      }
      .card {
        border: 1px solid rgba(86, 255, 117, 0.3);
        border-radius: 0.4rem;
        padding: 0.75rem;
        background: rgba(8, 22, 8, 0.65);
      }
      .label {
        font-size: 0.85rem;
        color: #96f3a8;
      }
      .value {
        margin-top: 0.2rem;
        font-size: 1.15rem;
        font-weight: 700;
      }
      .status-bad {
        color: var(--danger);
      }
      .warnings {
        margin: 0;
        padding-left: 1rem;
      }
      @media (max-width: 560px) {
        main {
          padding-top: 1.2rem;
        }
      }
    </style>
  </head>
  <body>
    <canvas id="rain"></canvas>
    <main>
      <section class="panel">
        <h1 class="title">BTC MATRIX TRACKER</h1>
        <p class="sub">Multi-source spot pricing feed</p>
      </section>

      <section class="panel">
        <p class="label">Average BTC/USD</p>
        <h2 class="hero-price" id="avg">Loading...</h2>
        <div class="grid">
          <div class="card">
            <div class="label">Spread</div>
            <div class="value" id="spread">-</div>
          </div>
          <div class="card">
            <div class="label">Updated</div>
            <div class="value" id="updated">-</div>
          </div>
          <div class="card">
            <div class="label">Status</div>
            <div class="value" id="status">Connecting</div>
          </div>
        </div>
      </section>

      <section class="panel">
        <p class="label">Sources</p>
        <div class="grid" id="sources"></div>
      </section>

      <section class="panel">
        <p class="label">Warnings</p>
        <ul class="warnings" id="warnings">
          <li>None</li>
        </ul>
      </section>
    </main>

    <script>
      const fmt = new Intl.NumberFormat("en-US", { style: "currency", currency: "USD", maximumFractionDigits: 2 });
      const avgEl = document.getElementById("avg");
      const spreadEl = document.getElementById("spread");
      const updatedEl = document.getElementById("updated");
      const statusEl = document.getElementById("status");
      const sourcesEl = document.getElementById("sources");
      const warningsEl = document.getElementById("warnings");

      async function refresh() {
        try {
          const res = await fetch("/api/price");
          const data = await res.json();
          const healthy = res.ok && data.sources && data.sources.length > 0;

          statusEl.textContent = healthy ? "LIVE" : "DEGRADED";
          statusEl.classList.toggle("status-bad", !healthy);

          avgEl.textContent = data.average_price ? fmt.format(data.average_price) : "Unavailable";
          spreadEl.textContent = data.spread ? fmt.format(data.spread) : "-";
          updatedEl.textContent = data.fetched_at_unix
            ? new Date(data.fetched_at_unix * 1000).toLocaleTimeString()
            : "-";

          sourcesEl.innerHTML = "";
          for (const source of data.sources || []) {
            const node = document.createElement("article");
            node.className = "card";
            node.innerHTML = `<div class="label">${source.source}</div><div class="value">${fmt.format(source.price_usd)}</div>`;
            sourcesEl.appendChild(node);
          }

          warningsEl.innerHTML = "";
          if ((data.warnings || []).length === 0) {
            warningsEl.innerHTML = "<li>None</li>";
          } else {
            for (const warning of data.warnings) {
              const li = document.createElement("li");
              li.textContent = warning;
              warningsEl.appendChild(li);
            }
          }
        } catch (err) {
          statusEl.textContent = "OFFLINE";
          statusEl.classList.add("status-bad");
          avgEl.textContent = "Unavailable";
          warningsEl.innerHTML = `<li>UI fetch failed: ${String(err)}</li>`;
        }
      }

      function matrixRain() {
        const canvas = document.getElementById("rain");
        const ctx = canvas.getContext("2d");
        const chars = "01BTC$#@";
        const cell = 24;
        let streams = [];
        let columns = 0;

        function resize() {
          canvas.width = window.innerWidth;
          canvas.height = window.innerHeight;
          columns = Math.max(1, Math.floor(canvas.width / cell));
          streams = Array.from({ length: columns }, () => ({
            y: -Math.random() * canvas.height,
            speed: 0.5 + Math.random() * 1.8,
            active: Math.random() < 0.28,
          }));
        }

        function draw() {
          ctx.fillStyle = "rgba(0, 0, 0, 0.1)";
          ctx.fillRect(0, 0, canvas.width, canvas.height);
          ctx.font = "17px monospace";
          for (let i = 0; i < streams.length; i++) {
            const stream = streams[i];

            if (!stream.active && Math.random() < 0.02) {
              stream.active = true;
              stream.y = -Math.random() * canvas.height * 0.9;
              stream.speed = 0.45 + Math.random() * 2.1;
            }
            if (!stream.active) continue;
            if (Math.random() < 0.4) continue;

            const text = chars[Math.floor(Math.random() * chars.length)];
            ctx.fillStyle = Math.random() < 0.14 ? "#b9ffc6" : "#56ff75";
            ctx.fillText(text, i * cell, stream.y);

            if (Math.random() < 0.22) {
              const ghostText = chars[Math.floor(Math.random() * chars.length)];
              ctx.fillStyle = "rgba(86, 255, 117, 0.35)";
              ctx.fillText(ghostText, i * cell, stream.y - cell);
            }

            stream.y += stream.speed * cell;
            if (stream.y > canvas.height + Math.random() * canvas.height * 0.5) {
              stream.active = false;
            }
          }
        }

        window.addEventListener("resize", resize);
        resize();
        setInterval(draw, 55);
      }

      matrixRain();
      refresh();
      setInterval(refresh, 60000);
    </script>
  </body>
</html>
"##;
