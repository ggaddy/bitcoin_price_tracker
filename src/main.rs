use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::{get, post},
};
use reqwest::Client;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::{sync::Mutex, task};
use tracing::{info, warn};

const REFRESH_INTERVAL_SECONDS: i64 = 10;
const VIEWER_TTL_SECONDS: i64 = 15;
const MAX_SNAPSHOT_HISTORY: i64 = 1_440;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UpstreamSource {
    CoinGecko,
    Coinbase,
    Kraken,
    Gemini,
}

impl UpstreamSource {
    const ALL: [Self; 4] = [Self::CoinGecko, Self::Coinbase, Self::Kraken, Self::Gemini];

    fn name(self) -> &'static str {
        match self {
            Self::CoinGecko => "CoinGecko",
            Self::Coinbase => "Coinbase",
            Self::Kraken => "Kraken",
            Self::Gemini => "Gemini",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        match name {
            "CoinGecko" => Some(Self::CoinGecko),
            "Coinbase" => Some(Self::Coinbase),
            "Kraken" => Some(Self::Kraken),
            "Gemini" => Some(Self::Gemini),
            _ => None,
        }
    }

    fn next_source(last_refreshed_source: Option<&str>) -> Self {
        let Some(last_source) = last_refreshed_source.and_then(Self::from_name) else {
            return Self::ALL[0];
        };

        let Some(index) = Self::ALL.iter().position(|source| *source == last_source) else {
            return Self::ALL[0];
        };

        Self::ALL[(index + 1) % Self::ALL.len()]
    }
}

#[derive(Clone)]
struct AppState {
    client: Client,
    db_path: PathBuf,
    viewers: Arc<Mutex<HashMap<String, i64>>>,
    refresh_lock: Arc<Mutex<()>>,
}

#[derive(Clone, Serialize)]
struct SourcePrice {
    source: String,
    price_usd: f64,
}

#[derive(Serialize)]
struct PriceResponse {
    symbol: &'static str,
    currency: &'static str,
    sources: Vec<SourcePrice>,
    average_price: Option<f64>,
    spread: Option<f64>,
    fetched_at_unix: i64,
    fetched_age_seconds: Option<i64>,
    warnings: Vec<String>,
    refresh_succeeded: bool,
    stale: bool,
    active_viewers: usize,
    refresh_skipped_reason: Option<String>,
}

#[derive(Clone)]
struct SnapshotRecord {
    fetched_at_unix: i64,
    sources: Vec<SourcePrice>,
    average_price: Option<f64>,
    spread: Option<f64>,
    warnings: Vec<String>,
    refreshed_source: Option<String>,
}

#[derive(Deserialize)]
struct PresencePayload {
    session_id: String,
    active: bool,
}

#[derive(Serialize)]
struct PresenceResponse {
    active_viewers: usize,
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

    let db_path = database_path();
    init_db(db_path.clone())
        .await
        .expect("failed to initialize SQLite database");

    let state = AppState {
        client,
        db_path,
        viewers: Arc::new(Mutex::new(HashMap::new())),
        refresh_lock: Arc::new(Mutex::new(())),
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/api/price", get(btc_prices))
        .route("/api/presence", post(update_presence))
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

async fn update_presence(
    State(state): State<AppState>,
    Json(payload): Json<PresencePayload>,
) -> impl IntoResponse {
    if !is_valid_session_id(&payload.session_id) {
        return (
            StatusCode::BAD_REQUEST,
            Json(PresenceResponse { active_viewers: 0 }),
        );
    }

    let active_viewers = apply_presence_update(&state, payload).await;
    (StatusCode::OK, Json(PresenceResponse { active_viewers }))
}

async fn btc_prices(State(state): State<AppState>) -> impl IntoResponse {
    let active_viewers = active_viewer_count(&state).await;
    let mut refresh_succeeded = false;
    let mut refresh_skipped_reason = None;
    let mut refresh_error = None;

    if active_viewers == 0 {
        refresh_skipped_reason = Some(
            "Refresh skipped: no active viewers are currently tracked. Open the dashboard tab to resume updates."
                .to_string(),
        );
    } else {
        let _guard = state.refresh_lock.lock().await;
        let now = now_unix();

        match load_latest_snapshot(state.db_path.clone()).await {
            Ok(snapshot) => {
                let age = snapshot
                    .as_ref()
                    .and_then(|record| snapshot_age_seconds(record, now));

                if let Some(reason) = refresh_skip_reason(active_viewers, age) {
                    refresh_skipped_reason = Some(reason);
                } else if let Err(err) = refresh_snapshot(&state, snapshot.clone()).await {
                    refresh_error = Some(err);
                } else {
                    refresh_succeeded = true;
                }
            }
            Err(err) => {
                refresh_error = Some(format!("Failed to inspect SQLite before refresh: {err}"));
            }
        }
    }

    let response_now = now_unix();
    match load_latest_snapshot(state.db_path.clone()).await {
        Ok(Some(snapshot)) => {
            let fetched_age_seconds = snapshot_age_seconds(&snapshot, response_now);
            let stale = fetched_age_seconds
                .map(|age| age >= REFRESH_INTERVAL_SECONDS)
                .unwrap_or(true);
            let mut warnings = snapshot.warnings.clone();

            if let Some(err) = refresh_error {
                warnings.insert(
                    0,
                    format!("Refresh failed; serving latest SQLite snapshot: {err}"),
                );
            }

            (
                StatusCode::OK,
                Json(PriceResponse {
                    symbol: "BTC",
                    currency: "USD",
                    sources: snapshot.sources,
                    average_price: snapshot.average_price,
                    spread: snapshot.spread,
                    fetched_at_unix: snapshot.fetched_at_unix,
                    fetched_age_seconds,
                    warnings,
                    refresh_succeeded,
                    stale,
                    active_viewers,
                    refresh_skipped_reason,
                }),
            )
        }
        Ok(None) => {
            let mut warnings = vec!["No stored BTC price snapshot is available yet.".to_string()];
            if let Some(err) = refresh_error {
                warnings.push(err);
            }

            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(PriceResponse {
                    symbol: "BTC",
                    currency: "USD",
                    sources: Vec::new(),
                    average_price: None,
                    spread: None,
                    fetched_at_unix: 0,
                    fetched_age_seconds: None,
                    warnings,
                    refresh_succeeded: false,
                    stale: true,
                    active_viewers,
                    refresh_skipped_reason,
                }),
            )
        }
        Err(err) => {
            warn!("failed to load SQLite snapshot: {err}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(PriceResponse {
                    symbol: "BTC",
                    currency: "USD",
                    sources: Vec::new(),
                    average_price: None,
                    spread: None,
                    fetched_at_unix: 0,
                    fetched_age_seconds: None,
                    warnings: vec!["Failed to load stored price data.".to_string()],
                    refresh_succeeded: false,
                    stale: true,
                    active_viewers,
                    refresh_skipped_reason,
                }),
            )
        }
    }
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
        source: "CoinGecko".to_string(),
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
        source: "Coinbase".to_string(),
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
        source: "Kraken".to_string(),
        price_usd: price,
    })
}

async fn fetch_gemini(client: &Client) -> Result<SourcePrice, String> {
    let value: Value = client
        .get("https://api.gemini.com/v2/ticker/btcusd")
        .send()
        .await
        .map_err(|e| format!("Gemini request failed: {e}"))?
        .error_for_status()
        .map_err(|e| format!("Gemini HTTP error: {e}"))?
        .json()
        .await
        .map_err(|e| format!("Gemini parse failed: {e}"))?;

    let bid = value
        .get("bid")
        .and_then(Value::as_str)
        .ok_or_else(|| "Gemini response missing bid".to_string())?;

    let price = bid
        .parse::<f64>()
        .map_err(|e| format!("Gemini bid parse failed: {e}"))?;

    Ok(SourcePrice {
        source: "Gemini".to_string(),
        price_usd: price,
    })
}

async fn refresh_snapshot(
    state: &AppState,
    latest_snapshot: Option<SnapshotRecord>,
) -> Result<(), String> {
    let next_source = UpstreamSource::next_source(
        latest_snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.refreshed_source.as_deref()),
    );
    let refreshed_price = fetch_round_robin_source(&state.client, next_source).await?;
    let sources = merge_snapshot_sources(latest_snapshot.as_ref(), refreshed_price);
    let (average_price, spread) = summarize_prices(&sources);

    let snapshot = SnapshotRecord {
        fetched_at_unix: now_unix(),
        sources,
        average_price,
        spread,
        warnings: Vec::new(),
        refreshed_source: Some(next_source.name().to_string()),
    };

    store_snapshot(state.db_path.clone(), snapshot).await
}

async fn fetch_round_robin_source(
    client: &Client,
    source: UpstreamSource,
) -> Result<SourcePrice, String> {
    match source {
        UpstreamSource::CoinGecko => fetch_coingecko(client).await,
        UpstreamSource::Coinbase => fetch_coinbase(client).await,
        UpstreamSource::Kraken => fetch_kraken(client).await,
        UpstreamSource::Gemini => fetch_gemini(client).await,
    }
}

fn merge_snapshot_sources(
    latest_snapshot: Option<&SnapshotRecord>,
    refreshed_price: SourcePrice,
) -> Vec<SourcePrice> {
    let mut source_prices = HashMap::new();

    if let Some(snapshot) = latest_snapshot {
        for source in &snapshot.sources {
            source_prices.insert(source.source.clone(), source.price_usd);
        }
    }

    source_prices.insert(refreshed_price.source.clone(), refreshed_price.price_usd);
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
}

async fn apply_presence_update(state: &AppState, payload: PresencePayload) -> usize {
    let now = now_unix();
    let mut viewers = state.viewers.lock().await;
    prune_inactive_viewers(&mut viewers, now);

    if payload.active {
        viewers.insert(payload.session_id, now);
    } else {
        viewers.remove(&payload.session_id);
    }

    viewers.len()
}

async fn active_viewer_count(state: &AppState) -> usize {
    let now = now_unix();
    let mut viewers = state.viewers.lock().await;
    prune_inactive_viewers(&mut viewers, now);
    viewers.len()
}

fn prune_inactive_viewers(viewers: &mut HashMap<String, i64>, now: i64) {
    viewers.retain(|_, last_seen| now.saturating_sub(*last_seen) <= VIEWER_TTL_SECONDS);
}

fn snapshot_age_seconds(snapshot: &SnapshotRecord, now: i64) -> Option<i64> {
    Some(now.saturating_sub(snapshot.fetched_at_unix))
}

fn refresh_skip_reason(active_viewers: usize, snapshot_age: Option<i64>) -> Option<String> {
    if active_viewers == 0 {
        return Some(
            "Refresh skipped: no active viewers are currently tracked. Open the dashboard tab to resume updates."
                .to_string(),
        );
    }

    if let Some(age) = snapshot_age {
        if age < REFRESH_INTERVAL_SECONDS {
            return Some(format!(
                "Refresh skipped: latest snapshot is {age}s old; minimum refresh interval is {REFRESH_INTERVAL_SECONDS}s."
            ));
        }
    }

    None
}

fn is_valid_session_id(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id.len() <= 128
        && session_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
}

fn database_path() -> PathBuf {
    std::env::var("DATABASE_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("data/bitcoin_prices.db"))
}

async fn init_db(db_path: PathBuf) -> Result<(), String> {
    run_blocking(move || init_db_sync(&db_path)).await
}

fn init_db_sync(db_path: &Path) -> Result<(), String> {
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create database directory {parent:?}: {e}"))?;
    }

    let conn = open_connection(db_path)?;
    conn.execute_batch(
        "
        PRAGMA foreign_keys = ON;
        CREATE TABLE IF NOT EXISTS price_snapshots (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            fetched_at_unix INTEGER NOT NULL,
            average_price REAL,
            spread REAL,
            warnings_json TEXT NOT NULL,
            refreshed_source TEXT
        );
        CREATE TABLE IF NOT EXISTS source_prices (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            snapshot_id INTEGER NOT NULL,
            source TEXT NOT NULL,
            price_usd REAL NOT NULL,
            FOREIGN KEY(snapshot_id) REFERENCES price_snapshots(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_source_prices_snapshot_id
            ON source_prices(snapshot_id);
        ",
    )
    .map_err(|e| format!("failed to initialize schema: {e}"))?;

    ensure_snapshot_column(&conn, "refreshed_source", "TEXT")?;

    Ok(())
}

fn ensure_snapshot_column(
    conn: &Connection,
    column_name: &str,
    column_type: &str,
) -> Result<(), String> {
    let mut stmt = conn
        .prepare("PRAGMA table_info(price_snapshots)")
        .map_err(|e| format!("failed to inspect price_snapshots schema: {e}"))?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| format!("failed to query price_snapshots schema: {e}"))?;

    for column in columns {
        if column.map_err(|e| format!("failed to decode schema row: {e}"))? == column_name {
            return Ok(());
        }
    }

    conn.execute(
        &format!("ALTER TABLE price_snapshots ADD COLUMN {column_name} {column_type}"),
        [],
    )
    .map_err(|e| format!("failed to add {column_name} column to price_snapshots: {e}"))?;

    Ok(())
}

fn prune_snapshot_history(tx: &rusqlite::Transaction<'_>) -> Result<(), String> {
    tx.execute(
        "DELETE FROM price_snapshots
         WHERE id IN (
             SELECT id
             FROM price_snapshots
             ORDER BY fetched_at_unix DESC, id DESC
             LIMIT -1 OFFSET ?1
         )",
        params![MAX_SNAPSHOT_HISTORY],
    )
    .map_err(|e| format!("failed to prune old price snapshots: {e}"))?;

    Ok(())
}

async fn store_snapshot(db_path: PathBuf, snapshot: SnapshotRecord) -> Result<(), String> {
    run_blocking(move || store_snapshot_sync(&db_path, snapshot)).await
}

fn store_snapshot_sync(db_path: &Path, snapshot: SnapshotRecord) -> Result<(), String> {
    let mut conn = open_connection(db_path)?;
    let tx = conn
        .transaction()
        .map_err(|e| format!("failed to start SQLite transaction: {e}"))?;

    let warnings_json = serde_json::to_string(&snapshot.warnings)
        .map_err(|e| format!("failed to encode warnings as JSON: {e}"))?;

    tx.execute(
        "INSERT INTO price_snapshots (
             fetched_at_unix,
             average_price,
             spread,
             warnings_json,
             refreshed_source
         )
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            snapshot.fetched_at_unix,
            snapshot.average_price,
            snapshot.spread,
            warnings_json,
            snapshot.refreshed_source
        ],
    )
    .map_err(|e| format!("failed to insert price snapshot: {e}"))?;

    let snapshot_id = tx.last_insert_rowid();
    let mut stmt = tx
        .prepare(
            "INSERT INTO source_prices (snapshot_id, source, price_usd)
             VALUES (?1, ?2, ?3)",
        )
        .map_err(|e| format!("failed to prepare source insert: {e}"))?;

    for source in snapshot.sources {
        stmt.execute(params![snapshot_id, source.source, source.price_usd])
            .map_err(|e| format!("failed to insert source price: {e}"))?;
    }

    drop(stmt);
    prune_snapshot_history(&tx)?;
    tx.commit()
        .map_err(|e| format!("failed to commit SQLite transaction: {e}"))?;

    Ok(())
}

async fn load_latest_snapshot(db_path: PathBuf) -> Result<Option<SnapshotRecord>, String> {
    run_blocking(move || load_latest_snapshot_sync(&db_path)).await
}

fn load_latest_snapshot_sync(db_path: &Path) -> Result<Option<SnapshotRecord>, String> {
    let conn = open_connection(db_path)?;
    let latest = conn
        .query_row(
            "
            SELECT id, fetched_at_unix, average_price, spread, warnings_json, refreshed_source
            FROM price_snapshots
            ORDER BY fetched_at_unix DESC, id DESC
            LIMIT 1
            ",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<f64>>(2)?,
                    row.get::<_, Option<f64>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            },
        )
        .optional()
        .map_err(|e| format!("failed to query latest snapshot: {e}"))?;

    let Some((
        snapshot_id,
        fetched_at_unix,
        average_price,
        spread,
        warnings_json,
        refreshed_source,
    )) = latest
    else {
        return Ok(None);
    };

    let warnings = serde_json::from_str::<Vec<String>>(&warnings_json)
        .map_err(|e| format!("failed to decode stored warnings: {e}"))?;

    let mut stmt = conn
        .prepare(
            "
            SELECT source, price_usd
            FROM source_prices
            WHERE snapshot_id = ?1
            ORDER BY
                CASE source
                    WHEN 'CoinGecko' THEN 0
                    WHEN 'Coinbase' THEN 1
                    WHEN 'Kraken' THEN 2
                    WHEN 'Gemini' THEN 3
                    ELSE 99
                END,
                source ASC
            ",
        )
        .map_err(|e| format!("failed to prepare source lookup: {e}"))?;

    let rows = stmt
        .query_map(params![snapshot_id], |row| {
            Ok(SourcePrice {
                source: row.get(0)?,
                price_usd: row.get(1)?,
            })
        })
        .map_err(|e| format!("failed to query stored source prices: {e}"))?;

    let mut sources = Vec::new();
    for row in rows {
        sources.push(row.map_err(|e| format!("failed to decode stored source row: {e}"))?);
    }

    Ok(Some(SnapshotRecord {
        fetched_at_unix,
        sources,
        average_price,
        spread,
        warnings,
        refreshed_source,
    }))
}

fn open_connection(db_path: &Path) -> Result<Connection, String> {
    let conn = Connection::open(db_path)
        .map_err(|e| format!("failed to open SQLite database at {db_path:?}: {e}"))?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")
        .map_err(|e| format!("failed to enable SQLite foreign keys for {db_path:?}: {e}"))?;

    Ok(conn)
}

async fn run_blocking<T>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String>
where
    T: Send + 'static,
{
    task::spawn_blocking(work)
        .await
        .map_err(|e| format!("blocking task failed: {e}"))?
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

const INDEX_HTML: &str = r##"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>BTC Tracker</title>
    <style>
      :root {
        --bg-0: #000000;
        --neon: #56ff75;
        --neon-soft: #3ccf57;
        --warn: #ffe66d;
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
        max-width: 980px;
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
        grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
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
      .note {
        margin-top: 0.9rem;
        color: #b9ffc6;
      }
      .status-bad {
        color: var(--danger);
      }
      .status-warn {
        color: var(--warn);
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
        <h1 class="title">BTC TRACKER</h1>
        <p class="sub">spot pricing feed</p>
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
            <div class="label">SQLite Snapshot</div>
            <div class="value" id="updated">-</div>
          </div>
          <div class="card">
            <div class="label">Active Viewers</div>
            <div class="value" id="viewers">0</div>
          </div>
          <div class="card">
            <div class="label">Status</div>
            <div class="value" id="status">Connecting</div>
          </div>
        </div>
        <p class="note" id="note">Tracking tab presence, rotating upstream checks, and enforcing a 10-second refresh gate.</p>
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
      const viewersEl = document.getElementById("viewers");
      const statusEl = document.getElementById("status");
      const noteEl = document.getElementById("note");
      const sourcesEl = document.getElementById("sources");
      const warningsEl = document.getElementById("warnings");
      const sessionKey = "btc-matrix-session-id";
      const sessionId = sessionStorage.getItem(sessionKey) || crypto.randomUUID().replace(/[^A-Za-z0-9_-]/g, "_");
      sessionStorage.setItem(sessionKey, sessionId);
      let refreshTimer = null;

      function setStatus(label, className) {
        statusEl.textContent = label;
        statusEl.classList.remove("status-bad", "status-warn");
        if (className) {
          statusEl.classList.add(className);
        }
      }

      async function sendPresence(active) {
        const payload = JSON.stringify({ session_id: sessionId, active });

        if (!active && navigator.sendBeacon) {
          const blob = new Blob([payload], { type: "application/json" });
          navigator.sendBeacon("/api/presence", blob);
          return;
        }

        await fetch("/api/presence", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: payload,
          keepalive: !active,
        });
      }

      async function refresh() {
        try {
          const res = await fetch("/api/price");
          const data = await res.json();
          const healthy = res.ok && data.sources && data.sources.length > 0;

          viewersEl.textContent = String(data.active_viewers ?? 0);

          if (!healthy) {
            setStatus("OFFLINE", "status-bad");
          } else if (data.stale) {
            setStatus("STALE", "status-warn");
          } else {
            setStatus("LIVE");
          }

          avgEl.textContent = data.average_price != null ? fmt.format(data.average_price) : "Unavailable";
          spreadEl.textContent = data.spread != null ? fmt.format(data.spread) : "-";
          updatedEl.textContent = data.fetched_at_unix
            ? new Date(data.fetched_at_unix * 1000).toLocaleString()
            : "-";

          if (data.refresh_succeeded) {
            noteEl.textContent = "One upstream source price was refreshed and written to SQLite for this cycle.";
          } else if (data.refresh_skipped_reason) {
            noteEl.textContent = data.refresh_skipped_reason;
          } else if (data.stale) {
            noteEl.textContent = "Serving an older SQLite snapshot because the upstream refresh did not complete.";
          } else {
            noteEl.textContent = "Serving the latest SQLite snapshot.";
          }

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
          setStatus("OFFLINE", "status-bad");
          avgEl.textContent = "Unavailable";
          noteEl.textContent = "Dashboard refresh failed before a SQLite snapshot could be loaded.";
          warningsEl.innerHTML = `<li>UI fetch failed: ${String(err)}</li>`;
        }
      }

      async function cycle() {
        if (document.visibilityState !== "visible") {
          return;
        }

        await sendPresence(true);
        await refresh();
      }

      function startTracking() {
        if (refreshTimer !== null || document.visibilityState !== "visible") {
          return;
        }

        cycle();
        refreshTimer = setInterval(cycle, 5000);
      }

      function stopTracking() {
        if (refreshTimer !== null) {
          clearInterval(refreshTimer);
          refreshTimer = null;
        }

        sendPresence(false).catch(() => {});
        setStatus("IDLE", "status-warn");
        noteEl.textContent = "Tracking paused while this tab is hidden.";
      }

      document.addEventListener("visibilitychange", () => {
        if (document.visibilityState === "visible") {
          startTracking();
        } else {
          stopTracking();
        }
      });

      window.addEventListener("pagehide", () => {
        sendPresence(false).catch(() => {});
      });

      if (document.visibilityState === "visible") {
        startTracking();
      } else {
        setStatus("IDLE", "status-warn");
        noteEl.textContent = "Open this tab to start upstream refresh tracking.";
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
    </script>
  </body>
</html>
"##;

#[cfg(test)]
mod tests {
    use super::{
        MAX_SNAPSHOT_HISTORY, SnapshotRecord, SourcePrice, UpstreamSource, VIEWER_TTL_SECONDS,
        init_db_sync, is_valid_session_id, load_latest_snapshot_sync, open_connection,
        prune_inactive_viewers, refresh_skip_reason, store_snapshot_sync,
    };
    use std::{
        collections::HashMap,
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn session_ids_must_be_ascii_and_reasonable_length() {
        assert!(is_valid_session_id("viewer_123-abc"));
        assert!(!is_valid_session_id(""));
        assert!(!is_valid_session_id("contains space"));
        assert!(!is_valid_session_id(&"x".repeat(129)));
    }

    #[test]
    fn refresh_is_skipped_when_snapshot_is_too_recent() {
        let reason = refresh_skip_reason(1, Some(4));
        assert!(reason.is_some());
    }

    #[test]
    fn inactive_viewers_are_pruned() {
        let mut viewers = HashMap::from([
            ("active".to_string(), 100),
            ("stale".to_string(), 100 - VIEWER_TTL_SECONDS - 1),
        ]);

        prune_inactive_viewers(&mut viewers, 100);

        assert!(viewers.contains_key("active"));
        assert!(!viewers.contains_key("stale"));
    }

    fn temp_db_path(test_name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());

        std::env::temp_dir().join(format!("bitcoin-price-tracker-{test_name}-{unique}.db"))
    }

    fn snapshot_for_test(index: i64) -> SnapshotRecord {
        SnapshotRecord {
            fetched_at_unix: index,
            sources: vec![SourcePrice {
                source: "CoinGecko".to_string(),
                price_usd: 100_000.0 + index as f64,
            }],
            average_price: Some(100_000.0 + index as f64),
            spread: Some(0.0),
            warnings: Vec::new(),
            refreshed_source: Some("CoinGecko".to_string()),
        }
    }

    #[test]
    fn snapshot_history_is_capped_and_prunes_source_rows() {
        let db_path = temp_db_path("retention");
        init_db_sync(&db_path).unwrap();

        let total_snapshots = MAX_SNAPSHOT_HISTORY as usize + 5;
        for index in 0..total_snapshots {
            store_snapshot_sync(&db_path, snapshot_for_test(index as i64)).unwrap();
        }

        let conn = open_connection(&db_path).unwrap();
        let snapshot_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM price_snapshots", [], |row| row.get(0))
            .unwrap();
        let source_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM source_prices", [], |row| row.get(0))
            .unwrap();
        let oldest_snapshot: i64 = conn
            .query_row(
                "SELECT MIN(fetched_at_unix) FROM price_snapshots",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let latest_snapshot = load_latest_snapshot_sync(&db_path).unwrap().unwrap();

        assert_eq!(snapshot_count, MAX_SNAPSHOT_HISTORY);
        assert_eq!(source_count, MAX_SNAPSHOT_HISTORY);
        assert_eq!(
            oldest_snapshot,
            (total_snapshots - MAX_SNAPSHOT_HISTORY as usize) as i64
        );
        assert_eq!(
            latest_snapshot.fetched_at_unix,
            (total_snapshots - 1) as i64
        );

        drop(conn);
        fs::remove_file(&db_path).unwrap();
    }

    #[test]
    fn upstream_sources_rotate_in_round_robin_order() {
        assert_eq!(UpstreamSource::next_source(None), UpstreamSource::CoinGecko);
        assert_eq!(
            UpstreamSource::next_source(Some("CoinGecko")),
            UpstreamSource::Coinbase
        );
        assert_eq!(
            UpstreamSource::next_source(Some("Coinbase")),
            UpstreamSource::Kraken
        );
        assert_eq!(
            UpstreamSource::next_source(Some("Kraken")),
            UpstreamSource::Gemini
        );
        assert_eq!(
            UpstreamSource::next_source(Some("Gemini")),
            UpstreamSource::CoinGecko
        );
    }
}
