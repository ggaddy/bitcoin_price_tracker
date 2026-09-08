use std::{
    fs,
    path::{Path, PathBuf},
};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use tokio::task;

use crate::{
    models::{
        SnapshotRecord, StoredPriceState,
        source_contract::{LastAttempt, ProviderHealth, SourceError, StoredQuote},
    },
    pricing::UpstreamSource,
};

const SCHEMA_VERSION: i64 = 1;

pub(crate) async fn init_db(db_path: PathBuf) -> Result<(), String> {
    run_blocking(move || init_db_sync(&db_path)).await
}

fn init_db_sync(db_path: &Path) -> Result<(), String> {
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create database directory {parent:?}: {error}"))?;
    }

    let mut conn = open_connection(db_path)?;
    // Serialize startup migrations and include schema, retention, health seeding,
    // and version publication in the same rollback boundary.
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| format!("failed to start schema migration: {error}"))?;
    let version: i64 = tx
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|error| format!("failed to read SQLite schema version: {error}"))?;
    match version {
        0 => migrate_unversioned_schema(&tx)?,
        SCHEMA_VERSION => {}
        _ => {
            return Err(format!(
                "unsupported SQLite schema version {version}; supported version is {SCHEMA_VERSION}"
            ));
        }
    }
    prune_snapshots_to_latest(&tx)?;
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(|error| format!("failed to record SQLite schema version: {error}"))?;
    tx.commit()
        .map_err(|error| format!("failed to commit schema migration: {error}"))?;
    Ok(())
}

fn migrate_unversioned_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "
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
    .map_err(|error| format!("failed to initialize schema: {error}"))?;

    ensure_snapshot_column(conn, "refreshed_source", "TEXT")?;
    conn.execute_batch(
        "ALTER TABLE source_prices ADD COLUMN last_success_at_unix INTEGER;
         ALTER TABLE source_prices ADD COLUMN quote_kind TEXT NOT NULL DEFAULT 'unknown'
             CHECK (quote_kind IN ('aggregate', 'spot', 'last_trade', 'bid', 'unknown'));
         UPDATE source_prices SET quote_kind = CASE source
             WHEN 'CoinGecko' THEN 'aggregate'
             WHEN 'Coinbase' THEN 'spot'
             WHEN 'Kraken' THEN 'last_trade'
             WHEN 'Gemini' THEN 'bid'
             ELSE 'unknown' END;
         CREATE TABLE provider_health (
             source TEXT PRIMARY KEY NOT NULL,
             attempt_outcome TEXT NOT NULL DEFAULT 'unknown'
                 CHECK (attempt_outcome IN ('unknown', 'success', 'failure')),
             attempted_at_unix INTEGER,
             error_category TEXT CHECK (error_category IN
                 ('timeout', 'transport', 'http', 'invalid_payload', 'invalid_price')),
             error_message TEXT,
             http_status INTEGER CHECK (http_status BETWEEN 100 AND 599),
             CHECK (
                 (attempt_outcome = 'unknown' AND attempted_at_unix IS NULL
                     AND error_category IS NULL AND error_message IS NULL AND http_status IS NULL)
                 OR (attempt_outcome = 'success' AND attempted_at_unix IS NOT NULL
                     AND error_category IS NULL AND error_message IS NULL AND http_status IS NULL)
                 OR (attempt_outcome = 'failure' AND attempted_at_unix IS NOT NULL
                     AND error_category IS NOT NULL AND error_message IS NOT NULL
                     AND ((error_category = 'http' AND http_status IS NOT NULL)
                         OR (error_category != 'http' AND http_status IS NULL)))
             )
         );",
    )
    .map_err(|error| {
        format!("failed to migrate source observations and provider health: {error}")
    })?;
    // Do not infer successful attempts or observation times from legacy snapshots.
    for source in UpstreamSource::ALL {
        conn.execute(
            "INSERT INTO provider_health (source) VALUES (?1)",
            [source.name()],
        )
        .map_err(|error| format!("failed to initialize provider health: {error}"))?;
    }

    Ok(())
}

fn ensure_snapshot_column(
    conn: &Connection,
    column_name: &str,
    column_type: &str,
) -> Result<(), String> {
    let mut stmt = conn
        .prepare("PRAGMA table_info(price_snapshots)")
        .map_err(|error| format!("failed to inspect price_snapshots schema: {error}"))?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|error| format!("failed to query price_snapshots schema: {error}"))?;

    for column in columns {
        if column.map_err(|error| format!("failed to decode schema row: {error}"))? == column_name {
            return Ok(());
        }
    }

    conn.execute(
        &format!("ALTER TABLE price_snapshots ADD COLUMN {column_name} {column_type}"),
        [],
    )
    .map_err(|error| format!("failed to add {column_name} column to price_snapshots: {error}"))?;

    Ok(())
}

fn prune_snapshots_to_latest(conn: &Connection) -> Result<(), String> {
    conn.execute(
        "DELETE FROM price_snapshots
         WHERE id NOT IN (
             SELECT id
             FROM price_snapshots
             ORDER BY fetched_at_unix DESC, id DESC
             LIMIT 1
         )",
        [],
    )
    .map_err(|error| {
        format!("failed to prune stored price snapshots to the latest entry: {error}")
    })?;

    Ok(())
}

#[cfg(test)]
fn store_snapshot_sync(db_path: &Path, snapshot: SnapshotRecord) -> Result<(), String> {
    store_refresh_sync(db_path, Some(snapshot), Vec::new())
}

pub(crate) async fn store_refresh(
    db_path: PathBuf,
    snapshot: Option<SnapshotRecord>,
    health: Vec<ProviderHealth>,
) -> Result<(), String> {
    run_blocking(move || store_refresh_sync(&db_path, snapshot, health)).await
}

fn store_refresh_sync(
    db_path: &Path,
    snapshot: Option<SnapshotRecord>,
    health: Vec<ProviderHealth>,
) -> Result<(), String> {
    let mut conn = open_connection(db_path)?;
    let tx = conn
        .transaction()
        .map_err(|error| format!("failed to start SQLite transaction: {error}"))?;

    if let Some(snapshot) = snapshot {
        write_snapshot(&tx, snapshot)?;
    }
    write_provider_health(&tx, health)?;
    tx.commit()
        .map_err(|error| format!("failed to commit refresh transaction: {error}"))?;
    Ok(())
}

fn write_snapshot(tx: &Connection, snapshot: SnapshotRecord) -> Result<(), String> {
    if snapshot
        .sources
        .iter()
        .any(|quote| !quote.price_usd.is_finite() || quote.price_usd <= 0.0)
    {
        return Err("refusing to store an invalid source price".to_string());
    }
    // Persist only the current merged snapshot instead of a refresh history.
    tx.execute("DELETE FROM price_snapshots", [])
        .map_err(|error| format!("failed to clear stored price snapshot: {error}"))?;

    let warnings_json = serde_json::to_string(&snapshot.warnings)
        .map_err(|error| format!("failed to encode warnings as JSON: {error}"))?;

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
    .map_err(|error| format!("failed to insert price snapshot: {error}"))?;

    let snapshot_id = tx.last_insert_rowid();
    let mut stmt = tx
        .prepare(
            "INSERT INTO source_prices (snapshot_id, source, price_usd, last_success_at_unix, quote_kind)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .map_err(|error| format!("failed to prepare source insert: {error}"))?;

    for source in snapshot.sources {
        stmt.execute(params![
            snapshot_id,
            source.source,
            source.price_usd,
            source.last_success_at_unix,
            enum_text(source.quote_kind)?
        ])
        .map_err(|error| format!("failed to insert source price: {error}"))?;
    }

    drop(stmt);

    Ok(())
}

#[cfg(test)]
pub(crate) async fn load_latest_snapshot(
    db_path: PathBuf,
) -> Result<Option<SnapshotRecord>, String> {
    load_price_state(db_path).await.map(|state| state.snapshot)
}

#[cfg(test)]
fn load_latest_snapshot_sync(db_path: &Path) -> Result<Option<SnapshotRecord>, String> {
    load_latest_snapshot_sync_with_hook(db_path, || {})
}

#[cfg(test)]
fn load_latest_snapshot_sync_with_hook(
    db_path: &Path,
    after_metadata: impl FnOnce(),
) -> Result<Option<SnapshotRecord>, String> {
    load_price_state_sync_with_hook(db_path, after_metadata).map(|state| state.snapshot)
}

pub(crate) async fn load_price_state(db_path: PathBuf) -> Result<StoredPriceState, String> {
    run_blocking(move || load_price_state_sync_with_hook(&db_path, || {})).await
}

// Hold one read snapshot across quote metadata, source observations, and health,
// including the case where only failed-attempt health exists on a cold database.
fn load_price_state_sync_with_hook(
    db_path: &Path,
    after_metadata: impl FnOnce(),
) -> Result<StoredPriceState, String> {
    let mut conn = Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| format!("failed to open SQLite for reading: {error}"))?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Deferred)
        .map_err(|error| format!("failed to start SQLite read transaction: {error}"))?;
    let snapshot = read_snapshot(&tx, after_metadata)?;
    let provider_health = read_provider_health(&tx)?;
    tx.commit()
        .map_err(|error| format!("failed to finish SQLite read transaction: {error}"))?;
    Ok(StoredPriceState {
        snapshot,
        provider_health,
    })
}

fn read_snapshot(
    tx: &Connection,
    after_metadata: impl FnOnce(),
) -> Result<Option<SnapshotRecord>, String> {
    let latest = tx
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
        .map_err(|error| format!("failed to query latest snapshot: {error}"))?;

    after_metadata();

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
        .map_err(|error| format!("failed to decode stored warnings: {error}"))?;

    let mut stmt = tx
        .prepare(
            "
            SELECT source, price_usd, last_success_at_unix, quote_kind
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
        .map_err(|error| format!("failed to prepare source lookup: {error}"))?;

    let rows = stmt
        .query_map(params![snapshot_id], |row| {
            Ok(StoredQuote {
                source: row.get(0)?,
                price_usd: row.get(1)?,
                last_success_at_unix: row.get(2)?,
                quote_kind: decode_enum(row, 3)?,
            })
        })
        .map_err(|error| format!("failed to query stored source prices: {error}"))?;

    let mut sources = Vec::new();
    for row in rows {
        sources.push(row.map_err(|error| format!("failed to decode stored source row: {error}"))?);
    }

    drop(stmt);

    Ok(Some(SnapshotRecord {
        fetched_at_unix,
        sources,
        average_price,
        spread,
        warnings,
        refreshed_source,
    }))
}

fn enum_text(value: impl serde::Serialize) -> Result<String, String> {
    serde_json::to_value(value)
        .map_err(|error| error.to_string())?
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| "expected a string enum".to_string())
}

fn decode_enum<T: serde::de::DeserializeOwned>(
    row: &rusqlite::Row<'_>,
    index: usize,
) -> rusqlite::Result<T> {
    let text: String = row.get(index)?;
    serde_json::from_value(serde_json::Value::String(text)).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

fn write_provider_health(conn: &Connection, health: Vec<ProviderHealth>) -> Result<(), String> {
    for record in health {
        let (outcome, attempted_at, category, message, status) = match record.last_attempt {
            LastAttempt::Unknown => ("unknown", None, None, None, None),
            LastAttempt::Success { attempted_at_unix } => {
                ("success", Some(attempted_at_unix), None, None, None)
            }
            LastAttempt::Failure {
                attempted_at_unix,
                error,
            } => (
                "failure",
                Some(attempted_at_unix),
                Some(enum_text(error.category)?),
                Some(error.message),
                error.http_status,
            ),
        };
        conn.execute(
            "INSERT INTO provider_health (source, attempt_outcome, attempted_at_unix, error_category, error_message, http_status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(source) DO UPDATE SET attempt_outcome = excluded.attempt_outcome,
                 attempted_at_unix = excluded.attempted_at_unix, error_category = excluded.error_category,
                 error_message = excluded.error_message, http_status = excluded.http_status",
            params![record.source, outcome, attempted_at, category, message, status],
        ).map_err(|error| format!("failed to store provider health: {error}"))?;
    }
    Ok(())
}

fn read_provider_health(conn: &Connection) -> Result<Vec<ProviderHealth>, String> {
    let mut stmt = conn.prepare(
        "SELECT source, attempt_outcome, attempted_at_unix, error_category, error_message, http_status
         FROM provider_health ORDER BY CASE source
             WHEN 'CoinGecko' THEN 0 WHEN 'Coinbase' THEN 1 WHEN 'Kraken' THEN 2 WHEN 'Gemini' THEN 3 ELSE 99 END, source"
    ).map_err(|error| format!("failed to prepare provider health lookup: {error}"))?;
    let rows = stmt
        .query_map([], |row| {
            let outcome: String = row.get(1)?;
            let last_attempt = match outcome.as_str() {
                "unknown" => LastAttempt::Unknown,
                "success" => LastAttempt::Success {
                    attempted_at_unix: row.get(2)?,
                },
                "failure" => LastAttempt::Failure {
                    attempted_at_unix: row.get(2)?,
                    error: SourceError {
                        category: decode_enum(row, 3)?,
                        message: row.get(4)?,
                        http_status: row.get(5)?,
                    },
                },
                _ => return Err(rusqlite::Error::InvalidQuery),
            };
            Ok(ProviderHealth {
                source: row.get(0)?,
                last_attempt,
            })
        })
        .map_err(|error| format!("failed to query provider health: {error}"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("failed to decode provider health: {error}"))
}

fn open_connection(db_path: &Path) -> Result<Connection, String> {
    let conn = Connection::open(db_path)
        .map_err(|error| format!("failed to open SQLite database at {db_path:?}: {error}"))?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")
        .map_err(|error| {
            format!("failed to enable SQLite foreign keys for {db_path:?}: {error}")
        })?;

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
        .map_err(|error| format!("blocking task failed: {error}"))?
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    use rusqlite::params;

    use crate::models::{
        SnapshotRecord,
        source_contract::{QuoteKind, StoredQuote},
    };

    use super::{
        init_db_sync, load_latest_snapshot_sync, load_latest_snapshot_sync_with_hook,
        open_connection, store_snapshot_sync,
    };

    fn temp_db_path(test_name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());

        std::env::temp_dir().join(format!("bitcoin-price-tracker-{test_name}-{unique}.db"))
    }

    fn snapshot_for_test(index: i64) -> SnapshotRecord {
        SnapshotRecord {
            fetched_at_unix: index,
            sources: vec![StoredQuote {
                source: "CoinGecko".to_string(),
                price_usd: 100_000.0 + index as f64,
                last_success_at_unix: None,
                quote_kind: QuoteKind::Unknown,
            }],
            average_price: Some(100_000.0 + index as f64),
            spread: Some(0.0),
            warnings: Vec::new(),
            refreshed_source: Some("CoinGecko".to_string()),
        }
    }

    fn create_legacy_database(db_path: &std::path::Path, with_refreshed_source: bool) {
        let conn = open_connection(db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE price_snapshots (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                fetched_at_unix INTEGER NOT NULL,
                average_price REAL, spread REAL, warnings_json TEXT NOT NULL
             );
             CREATE TABLE source_prices (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                snapshot_id INTEGER NOT NULL,
                source TEXT NOT NULL, price_usd REAL NOT NULL,
                FOREIGN KEY(snapshot_id) REFERENCES price_snapshots(id) ON DELETE CASCADE
             );
             INSERT INTO price_snapshots VALUES (1, 10, 90000, 0, '[]');
             INSERT INTO price_snapshots VALUES (2, 20, 100002, 4, '[\"legacy warning\"]');
             INSERT INTO source_prices (snapshot_id, source, price_usd) VALUES (1, 'CoinGecko', 90000);"
        ).unwrap();
        if with_refreshed_source {
            conn.execute_batch(
                "ALTER TABLE price_snapshots ADD COLUMN refreshed_source TEXT;
                 UPDATE price_snapshots SET refreshed_source = 'all' WHERE id = 2;",
            )
            .unwrap();
        }
        for (index, source) in ["CoinGecko", "Coinbase", "Kraken", "Gemini", "Legacy"]
            .into_iter()
            .enumerate()
        {
            conn.execute(
                "INSERT INTO source_prices (snapshot_id, source, price_usd) VALUES (2, ?1, ?2)",
                params![source, 100000.0 + index as f64],
            )
            .unwrap();
        }
    }

    #[test]
    fn fresh_database_has_versioned_observation_schema_and_unknown_provider_health() {
        let db_path = temp_db_path("fresh-schema");
        init_db_sync(&db_path).unwrap();
        let conn = open_connection(&db_path).unwrap();
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            super::SCHEMA_VERSION
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM source_prices", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        let health: Vec<(String, String, Option<i64>)> = conn.prepare(
            "SELECT source, attempt_outcome, attempted_at_unix FROM provider_health ORDER BY source"
        ).unwrap().query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap()
            .collect::<Result<_, _>>().unwrap();
        assert_eq!(
            health,
            ["CoinGecko", "Coinbase", "Gemini", "Kraken"].map(|source| (
                source.to_string(),
                "unknown".to_string(),
                None
            ))
        );
        drop(conn);
        fs::remove_file(db_path).unwrap();
    }

    #[test]
    fn legacy_migration_preserves_latest_quotes_and_leaves_observation_times_unknown() {
        for with_refreshed_source in [false, true] {
            let db_path = temp_db_path("legacy-migration");
            create_legacy_database(&db_path, with_refreshed_source);
            init_db_sync(&db_path).unwrap();
            let snapshot = load_latest_snapshot_sync(&db_path).unwrap().unwrap();
            assert_eq!(snapshot.fetched_at_unix, 20);
            assert_eq!(snapshot.average_price, Some(100002.0));
            assert_eq!(snapshot.spread, Some(4.0));
            assert_eq!(snapshot.warnings, ["legacy warning"]);
            assert_eq!(
                snapshot.refreshed_source.as_deref(),
                with_refreshed_source.then_some("all")
            );
            assert_eq!(snapshot.sources.len(), 5);
            let conn = open_connection(&db_path).unwrap();
            let rows: Vec<(String, f64, Option<i64>, String)> = conn.prepare(
                "SELECT source, price_usd, last_success_at_unix, quote_kind FROM source_prices ORDER BY id"
            ).unwrap().query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))).unwrap()
                .collect::<Result<_, _>>().unwrap();
            assert_eq!(
                rows,
                [
                    ("CoinGecko", "aggregate"),
                    ("Coinbase", "spot"),
                    ("Kraken", "last_trade"),
                    ("Gemini", "bid"),
                    ("Legacy", "unknown")
                ]
                .into_iter()
                .enumerate()
                .map(|(index, (source, kind))| (
                    source.to_string(),
                    100000.0 + index as f64,
                    None,
                    kind.to_string()
                ))
                .collect::<Vec<_>>()
            );
            assert_eq!(
                conn.query_row("SELECT COUNT(*) FROM price_snapshots", [], |row| row
                    .get::<_, i64>(0))
                    .unwrap(),
                1
            );
            assert_eq!(
                conn.query_row(
                    "SELECT COUNT(*) FROM provider_health WHERE attempt_outcome = 'unknown'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
                4
            );
            assert!(
                conn.prepare("PRAGMA foreign_key_check")
                    .unwrap()
                    .query([])
                    .unwrap()
                    .next()
                    .unwrap()
                    .is_none()
            );
            drop(conn);
            fs::remove_file(db_path).unwrap();
        }
    }

    #[test]
    fn repeated_startup_preserves_recorded_observations_and_health() {
        let db_path = temp_db_path("repeat-migration");
        create_legacy_database(&db_path, true);
        init_db_sync(&db_path).unwrap();
        let conn = open_connection(&db_path).unwrap();
        conn.execute_batch(
            "UPDATE source_prices SET last_success_at_unix = 18 WHERE source = 'CoinGecko';
             UPDATE provider_health SET attempt_outcome = 'failure', attempted_at_unix = 21,
                 error_category = 'http', error_message = 'rate limited', http_status = 429
                 WHERE source = 'CoinGecko';",
        )
        .unwrap();
        for _ in 0..2 {
            init_db_sync(&db_path).unwrap();
        }
        assert_eq!(
            conn.query_row(
                "SELECT last_success_at_unix FROM source_prices WHERE source = 'CoinGecko'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            18
        );
        let health: (String, i64, String, i64) = conn.query_row(
            "SELECT attempt_outcome, attempted_at_unix, error_message, http_status FROM provider_health WHERE source = 'CoinGecko'",
            [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        ).unwrap();
        assert_eq!(health, ("failure".into(), 21, "rate limited".into(), 429));
        // Health is provider-owned, not deleted when a snapshot is replaced.
        store_snapshot_sync(&db_path, snapshot_for_test(30)).unwrap();
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM provider_health", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            4
        );
        drop(conn);
        fs::remove_file(db_path).unwrap();
    }

    #[test]
    fn failed_migration_rolls_back_schema_health_version_and_retention() {
        let db_path = temp_db_path("migration-rollback");
        create_legacy_database(&db_path, false);
        let conn = open_connection(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER reject_pruning BEFORE DELETE ON price_snapshots
             BEGIN SELECT RAISE(ABORT, 'forced migration failure'); END;",
        )
        .unwrap();
        let before = fs::read(&db_path).unwrap();
        let error = init_db_sync(&db_path).unwrap_err();
        assert!(error.contains("forced migration failure"), "{error}");
        assert_eq!(fs::read(&db_path).unwrap(), before);
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM price_snapshots", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM source_prices", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            6
        );
        assert!(
            conn.prepare("SELECT last_success_at_unix FROM source_prices")
                .is_err()
        );
        assert!(
            conn.prepare("SELECT refreshed_source FROM price_snapshots")
                .is_err()
        );
        assert!(conn.prepare("SELECT * FROM provider_health").is_err());
        conn.execute_batch("DROP TRIGGER reject_pruning;").unwrap();
        init_db_sync(&db_path).unwrap();
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            super::SCHEMA_VERSION
        );
        drop(conn);
        fs::remove_file(db_path).unwrap();
    }

    #[test]
    fn unsupported_schema_is_rejected_without_modifying_the_database() {
        for version in [-1, 2] {
            let db_path = temp_db_path("unsupported-schema");
            create_legacy_database(&db_path, true);
            let conn = open_connection(&db_path).unwrap();
            conn.pragma_update(None, "user_version", version).unwrap();
            let before = fs::read(&db_path).unwrap();
            let error = init_db_sync(&db_path).unwrap_err();
            assert!(
                error.contains(&format!("unsupported SQLite schema version {version}")),
                "{error}"
            );
            assert_eq!(fs::read(&db_path).unwrap(), before);
            drop(conn);
            fs::remove_file(db_path).unwrap();
        }
    }

    #[test]
    fn provider_health_rejects_inconsistent_outcomes() {
        let db_path = temp_db_path("health-constraints");
        init_db_sync(&db_path).unwrap();
        let conn = open_connection(&db_path).unwrap();
        for update in [
            "attempt_outcome = 'success'",
            "attempt_outcome = 'failure', attempted_at_unix = 10",
            "attempted_at_unix = 10",
            "attempt_outcome = 'failure', attempted_at_unix = 10, error_category = 'http', error_message = 'failed'",
            "attempt_outcome = 'failure', attempted_at_unix = 10, error_category = 'timeout', error_message = 'failed', http_status = 429",
        ] {
            assert!(
                conn.execute(
                    &format!("UPDATE provider_health SET {update} WHERE source = 'CoinGecko'"),
                    []
                )
                .is_err(),
                "accepted {update}"
            );
        }
        drop(conn);
        fs::remove_file(db_path).unwrap();
    }

    #[test]
    fn load_snapshot_stays_consistent_when_replaced_between_queries() {
        let db_path = temp_db_path("consistent-read");
        init_db_sync(&db_path).unwrap();
        let conn = open_connection(&db_path).unwrap();
        // Allow the writer to commit while a reader holds its snapshot open.
        let journal_mode: String = conn
            .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
            .unwrap();
        assert_eq!(journal_mode, "wal");

        store_snapshot_sync(&db_path, snapshot_for_test(10)).unwrap();

        let snapshot = load_latest_snapshot_sync_with_hook(&db_path, || {
            store_snapshot_sync(&db_path, snapshot_for_test(20)).unwrap();
            let committed = load_latest_snapshot_sync(&db_path).unwrap().unwrap();
            assert_eq!(committed.fetched_at_unix, 20);
            assert_eq!(committed.sources[0].price_usd, 100_020.0);
        })
        .unwrap()
        .unwrap();

        assert_eq!(snapshot.fetched_at_unix, 10);
        assert_eq!(snapshot.average_price, Some(100_010.0));
        assert_eq!(snapshot.spread, Some(0.0));
        assert!(snapshot.warnings.is_empty());
        assert_eq!(snapshot.refreshed_source.as_deref(), Some("CoinGecko"));
        assert_eq!(snapshot.sources.len(), 1);
        assert_eq!(snapshot.sources[0].source, "CoinGecko");
        assert_eq!(snapshot.sources[0].price_usd, 100_010.0);

        drop(conn);
        fs::remove_file(&db_path).unwrap();
    }

    #[test]
    fn quote_and_health_reads_share_one_snapshot_even_without_previous_quotes() {
        use crate::models::source_contract::{LastAttempt, ProviderHealth};

        for seeded in [false, true] {
            let db_path = temp_db_path("quote-health-consistency");
            init_db_sync(&db_path).unwrap();
            let conn = open_connection(&db_path).unwrap();
            conn.execute_batch("PRAGMA journal_mode = WAL;").unwrap();
            if seeded {
                super::store_refresh_sync(
                    &db_path,
                    Some(snapshot_for_test(10)),
                    vec![ProviderHealth {
                        source: "CoinGecko".into(),
                        last_attempt: LastAttempt::Success {
                            attempted_at_unix: 10,
                        },
                    }],
                )
                .unwrap();
            }
            let before = super::load_price_state_sync_with_hook(&db_path, || {}).unwrap();
            let observed = super::load_price_state_sync_with_hook(&db_path, || {
                let mut snapshot = snapshot_for_test(20);
                snapshot.sources[0].last_success_at_unix = Some(20);
                super::store_refresh_sync(
                    &db_path,
                    Some(snapshot),
                    vec![ProviderHealth {
                        source: "CoinGecko".into(),
                        last_attempt: LastAttempt::Success {
                            attempted_at_unix: 20,
                        },
                    }],
                )
                .unwrap();
                let committed = super::load_price_state_sync_with_hook(&db_path, || {}).unwrap();
                assert_eq!(
                    committed.snapshot.unwrap().sources[0].last_success_at_unix,
                    Some(20)
                );
                assert_eq!(
                    committed.provider_health[0].last_attempt,
                    LastAttempt::Success {
                        attempted_at_unix: 20
                    }
                );
            })
            .unwrap();
            assert_eq!(observed, before);
            drop(conn);
            fs::remove_file(db_path).unwrap();
        }
    }

    #[test]
    fn load_empty_database_returns_none_and_allows_subsequent_write() {
        let db_path = temp_db_path("empty-read");
        init_db_sync(&db_path).unwrap();

        assert!(load_latest_snapshot_sync(&db_path).unwrap().is_none());
        store_snapshot_sync(&db_path, snapshot_for_test(10)).unwrap();
        let snapshot = load_latest_snapshot_sync(&db_path).unwrap().unwrap();
        assert_eq!(snapshot.fetched_at_unix, 10);
        assert_eq!(snapshot.sources.len(), 1);
        assert_eq!(snapshot.sources[0].price_usd, 100_010.0);

        fs::remove_file(&db_path).unwrap();
    }

    #[test]
    fn failed_source_insert_preserves_previous_snapshot_and_source_rows() {
        let db_path = temp_db_path("rollback");
        init_db_sync(&db_path).unwrap();
        let mut original = snapshot_for_test(10);
        original.warnings = vec!["retained warning".to_string()];
        store_snapshot_sync(&db_path, original).unwrap();

        let conn = open_connection(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER fail_coinbase_insert
             BEFORE INSERT ON source_prices
             WHEN NEW.source = 'Coinbase'
             BEGIN
                 SELECT RAISE(ABORT, 'forced source insert failure');
             END;",
        )
        .unwrap();

        let mut replacement = snapshot_for_test(20);
        replacement.sources.push(StoredQuote {
            source: "Coinbase".to_string(),
            price_usd: 100_040.0,
            last_success_at_unix: None,
            quote_kind: QuoteKind::Unknown,
        });
        replacement.average_price = Some(100_030.0);
        replacement.spread = Some(20.0);
        replacement.refreshed_source = Some("Coinbase".to_string());
        // The first source inserts successfully; the second forces the entire write to roll back.
        let error = store_snapshot_sync(&db_path, replacement.clone()).unwrap_err();
        assert!(error.contains("forced source insert failure"), "{error}");

        let snapshot = load_latest_snapshot_sync(&db_path).unwrap().unwrap();
        assert_eq!(snapshot.fetched_at_unix, 10);
        assert_eq!(snapshot.average_price, Some(100_010.0));
        assert_eq!(snapshot.spread, Some(0.0));
        assert_eq!(snapshot.warnings, vec!["retained warning".to_string()]);
        assert_eq!(snapshot.refreshed_source.as_deref(), Some("CoinGecko"));
        assert_eq!(snapshot.sources.len(), 1);
        assert_eq!(snapshot.sources[0].source, "CoinGecko");
        assert_eq!(snapshot.sources[0].price_usd, 100_010.0);
        let counts: (i64, i64) = conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM price_snapshots),
                        (SELECT COUNT(*) FROM source_prices)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(counts, (1, 1));

        conn.execute_batch("DROP TRIGGER fail_coinbase_insert;")
            .unwrap();
        store_snapshot_sync(&db_path, replacement).unwrap();
        let recovered = load_latest_snapshot_sync(&db_path).unwrap().unwrap();
        assert_eq!(recovered.fetched_at_unix, 20);
        assert_eq!(recovered.sources.len(), 2);

        drop(conn);
        fs::remove_file(&db_path).unwrap();
    }

    #[test]
    fn invalid_numeric_quote_cannot_replace_stored_quotes_or_health() {
        use crate::models::source_contract::{LastAttempt, ProviderHealth};

        let db_path = temp_db_path("invalid-price-write");
        init_db_sync(&db_path).unwrap();
        store_snapshot_sync(&db_path, snapshot_for_test(10)).unwrap();
        let before = super::load_price_state_sync_with_hook(&db_path, || {}).unwrap();
        for price in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut replacement = snapshot_for_test(20);
            replacement.sources[0].price_usd = price;
            let error = super::store_refresh_sync(
                &db_path,
                Some(replacement),
                vec![ProviderHealth {
                    source: "CoinGecko".into(),
                    last_attempt: LastAttempt::Success {
                        attempted_at_unix: 20,
                    },
                }],
            )
            .unwrap_err();
            assert!(error.contains("invalid source price"));
            assert_eq!(
                super::load_price_state_sync_with_hook(&db_path, || {}).unwrap(),
                before
            );
        }
        fs::remove_file(db_path).unwrap();
    }

    #[test]
    fn store_snapshot_replaces_previous_snapshot_and_source_rows() {
        let db_path = temp_db_path("latest-only");
        init_db_sync(&db_path).unwrap();

        store_snapshot_sync(&db_path, snapshot_for_test(10)).unwrap();
        store_snapshot_sync(
            &db_path,
            SnapshotRecord {
                fetched_at_unix: 20,
                sources: vec![
                    StoredQuote {
                        source: "Coinbase".to_string(),
                        price_usd: 200_100.0,
                        last_success_at_unix: None,
                        quote_kind: QuoteKind::Unknown,
                    },
                    StoredQuote {
                        source: "CoinGecko".to_string(),
                        price_usd: 200_000.0,
                        last_success_at_unix: None,
                        quote_kind: QuoteKind::Unknown,
                    },
                ],
                average_price: Some(200_050.0),
                spread: Some(100.0),
                warnings: vec!["rate limited".to_string()],
                refreshed_source: Some("Coinbase".to_string()),
            },
        )
        .unwrap();

        let conn = open_connection(&db_path).unwrap();
        let snapshot_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM price_snapshots", [], |row| row.get(0))
            .unwrap();
        let source_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM source_prices", [], |row| row.get(0))
            .unwrap();
        let latest_snapshot = load_latest_snapshot_sync(&db_path).unwrap().unwrap();

        assert_eq!(snapshot_count, 1);
        assert_eq!(source_count, 2);
        assert_eq!(latest_snapshot.fetched_at_unix, 20);
        assert_eq!(latest_snapshot.sources.len(), 2);
        assert_eq!(latest_snapshot.sources[0].source, "CoinGecko");
        assert_eq!(latest_snapshot.sources[0].price_usd, 200_000.0);
        assert_eq!(latest_snapshot.sources[1].source, "Coinbase");
        assert_eq!(latest_snapshot.sources[1].price_usd, 200_100.0);
        assert_eq!(latest_snapshot.average_price, Some(200_050.0));
        assert_eq!(latest_snapshot.spread, Some(100.0));
        assert_eq!(latest_snapshot.warnings, vec!["rate limited".to_string()]);
        assert_eq!(
            latest_snapshot.refreshed_source.as_deref(),
            Some("Coinbase")
        );

        drop(conn);
        fs::remove_file(&db_path).unwrap();
    }

    #[test]
    fn init_db_prunes_existing_history_to_latest_snapshot() {
        let db_path = temp_db_path("init-prune");
        init_db_sync(&db_path).unwrap();

        let conn = open_connection(&db_path).unwrap();
        conn.execute("DELETE FROM price_snapshots", []).unwrap();

        conn.execute(
            "INSERT INTO price_snapshots (
                 fetched_at_unix,
                 average_price,
                 spread,
                 warnings_json,
                 refreshed_source
             )
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                10_i64,
                Some(100_000.0_f64),
                Some(0.0_f64),
                "[]",
                "CoinGecko"
            ],
        )
        .unwrap();
        let first_snapshot_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO source_prices (snapshot_id, source, price_usd)
             VALUES (?1, ?2, ?3)",
            params![first_snapshot_id, "CoinGecko", 100_000.0_f64],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO price_snapshots (
                 fetched_at_unix,
                 average_price,
                 spread,
                 warnings_json,
                 refreshed_source
             )
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                20_i64,
                Some(200_050.0_f64),
                Some(100.0_f64),
                "[\"warning\"]",
                "Coinbase"
            ],
        )
        .unwrap();
        let second_snapshot_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO source_prices (snapshot_id, source, price_usd)
             VALUES (?1, ?2, ?3)",
            params![second_snapshot_id, "CoinGecko", 200_000.0_f64],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO source_prices (snapshot_id, source, price_usd)
             VALUES (?1, ?2, ?3)",
            params![second_snapshot_id, "Coinbase", 200_100.0_f64],
        )
        .unwrap();

        drop(conn);

        init_db_sync(&db_path).unwrap();

        let conn = open_connection(&db_path).unwrap();
        let snapshot_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM price_snapshots", [], |row| row.get(0))
            .unwrap();
        let source_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM source_prices", [], |row| row.get(0))
            .unwrap();
        let latest_snapshot = load_latest_snapshot_sync(&db_path).unwrap().unwrap();

        assert_eq!(snapshot_count, 1);
        assert_eq!(source_count, 2);
        assert_eq!(latest_snapshot.fetched_at_unix, 20);
        assert_eq!(latest_snapshot.sources.len(), 2);
        assert_eq!(latest_snapshot.average_price, Some(200_050.0));
        assert_eq!(latest_snapshot.spread, Some(100.0));
        assert_eq!(latest_snapshot.warnings, vec!["warning".to_string()]);
        assert_eq!(
            latest_snapshot.refreshed_source.as_deref(),
            Some("Coinbase")
        );

        drop(conn);
        fs::remove_file(&db_path).unwrap();
    }
}
