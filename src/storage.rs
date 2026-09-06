use std::{
    fs,
    path::{Path, PathBuf},
};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use tokio::task;

use crate::models::{SnapshotRecord, SourcePrice};

pub(crate) async fn init_db(db_path: PathBuf) -> Result<(), String> {
    run_blocking(move || init_db_sync(&db_path)).await
}

fn init_db_sync(db_path: &Path) -> Result<(), String> {
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create database directory {parent:?}: {error}"))?;
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
    .map_err(|error| format!("failed to initialize schema: {error}"))?;

    ensure_snapshot_column(&conn, "refreshed_source", "TEXT")?;
    prune_snapshots_to_latest(&conn)?;

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

pub(crate) async fn store_snapshot(
    db_path: PathBuf,
    snapshot: SnapshotRecord,
) -> Result<(), String> {
    run_blocking(move || store_snapshot_sync(&db_path, snapshot)).await
}

fn store_snapshot_sync(db_path: &Path, snapshot: SnapshotRecord) -> Result<(), String> {
    let mut conn = open_connection(db_path)?;
    let tx = conn
        .transaction()
        .map_err(|error| format!("failed to start SQLite transaction: {error}"))?;

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
            "INSERT INTO source_prices (snapshot_id, source, price_usd)
             VALUES (?1, ?2, ?3)",
        )
        .map_err(|error| format!("failed to prepare source insert: {error}"))?;

    for source in snapshot.sources {
        stmt.execute(params![snapshot_id, source.source, source.price_usd])
            .map_err(|error| format!("failed to insert source price: {error}"))?;
    }

    drop(stmt);
    tx.commit()
        .map_err(|error| format!("failed to commit SQLite transaction: {error}"))?;

    Ok(())
}

pub(crate) async fn load_latest_snapshot(
    db_path: PathBuf,
) -> Result<Option<SnapshotRecord>, String> {
    run_blocking(move || load_latest_snapshot_sync(&db_path)).await
}

fn load_latest_snapshot_sync(db_path: &Path) -> Result<Option<SnapshotRecord>, String> {
    load_latest_snapshot_sync_with_hook(db_path, || {})
}

// The hook lets tests commit a replacement between the two queries deterministically.
fn load_latest_snapshot_sync_with_hook(
    db_path: &Path,
    after_metadata: impl FnOnce(),
) -> Result<Option<SnapshotRecord>, String> {
    let mut conn = open_connection(db_path)?;
    // Both queries must see the same committed snapshot, even if a writer replaces it.
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Deferred)
        .map_err(|error| format!("failed to start SQLite read transaction: {error}"))?;
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

    after_metadata();

    let warnings = serde_json::from_str::<Vec<String>>(&warnings_json)
        .map_err(|error| format!("failed to decode stored warnings: {error}"))?;

    let mut stmt = tx
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
        .map_err(|error| format!("failed to prepare source lookup: {error}"))?;

    let rows = stmt
        .query_map(params![snapshot_id], |row| {
            Ok(SourcePrice {
                source: row.get(0)?,
                price_usd: row.get(1)?,
            })
        })
        .map_err(|error| format!("failed to query stored source prices: {error}"))?;

    let mut sources = Vec::new();
    for row in rows {
        sources.push(row.map_err(|error| format!("failed to decode stored source row: {error}"))?);
    }

    drop(stmt);
    tx.commit()
        .map_err(|error| format!("failed to finish SQLite read transaction: {error}"))?;

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

    use crate::models::{SnapshotRecord, SourcePrice};

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
        replacement.sources.push(SourcePrice {
            source: "Coinbase".to_string(),
            price_usd: 100_040.0,
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
    fn store_snapshot_replaces_previous_snapshot_and_source_rows() {
        let db_path = temp_db_path("latest-only");
        init_db_sync(&db_path).unwrap();

        store_snapshot_sync(&db_path, snapshot_for_test(10)).unwrap();
        store_snapshot_sync(
            &db_path,
            SnapshotRecord {
                fetched_at_unix: 20,
                sources: vec![
                    SourcePrice {
                        source: "Coinbase".to_string(),
                        price_usd: 200_100.0,
                    },
                    SourcePrice {
                        source: "CoinGecko".to_string(),
                        price_usd: 200_000.0,
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
