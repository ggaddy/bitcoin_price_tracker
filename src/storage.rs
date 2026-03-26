use std::{
    fs,
    path::{Path, PathBuf},
};

use rusqlite::{Connection, OptionalExtension, params};
use tokio::task;

use crate::{
    config::MAX_SNAPSHOT_HISTORY,
    models::{SnapshotRecord, SourcePrice},
};

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
    .map_err(|error| format!("failed to prune old price snapshots: {error}"))?;

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
    prune_snapshot_history(&tx)?;
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

    let warnings = serde_json::from_str::<Vec<String>>(&warnings_json)
        .map_err(|error| format!("failed to decode stored warnings: {error}"))?;

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

    use crate::{
        config::MAX_SNAPSHOT_HISTORY,
        models::{SnapshotRecord, SourcePrice},
    };

    use super::{init_db_sync, load_latest_snapshot_sync, open_connection, store_snapshot_sync};

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
}
