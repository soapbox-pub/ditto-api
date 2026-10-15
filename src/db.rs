//! The SQLite cache database.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};

/// A cached link preview.
#[derive(Debug, Clone)]
pub struct LinkPreviewEntry {
    /// The OEmbed JSON (or error JSON) response body.
    pub json: String,
    /// HTTP status of the original response.
    pub status: u16,
    /// Unix time in milliseconds when it was fetched.
    pub fetched_at: i64,
}

/// Handle to the cache database, cheap to clone.
///
/// Every statement is a single-row lookup or write on a primary key, so a
/// mutex around one connection is plenty and keeps writes serialized.
#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

impl Db {
    /// Open (creating if needed) the database at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        Self::init(Connection::open(path)?)
    }

    /// Open a private in-memory database.
    #[cfg(test)]
    pub fn memory() -> Self {
        Self::init(Connection::open_in_memory().unwrap()).unwrap()
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "wal")?;
        conn.pragma_update(None, "synchronous", "normal")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS link_preview (
                key TEXT PRIMARY KEY,
                json TEXT NOT NULL,
                status INTEGER NOT NULL,
                fetched_at INTEGER NOT NULL
            );",
        )?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn get_link_preview(&self, key: &str) -> Result<Option<LinkPreviewEntry>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT json, status, fetched_at FROM link_preview WHERE key = ?",
                [key],
                |row| {
                    Ok(LinkPreviewEntry {
                        json: row.get(0)?,
                        status: row.get(1)?,
                        fetched_at: row.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn put_link_preview(&self, key: &str, entry: &LinkPreviewEntry) -> Result<()> {
        self.conn().execute(
            "INSERT OR REPLACE INTO link_preview (key, json, status, fetched_at) VALUES (?, ?, ?, ?)",
            params![key, entry.json, entry.status, entry.fetched_at],
        )?;
        Ok(())
    }

    /// Delete a link preview, returning whether one existed.
    pub fn remove_link_preview(&self, key: &str) -> Result<bool> {
        Ok(self
            .conn()
            .execute("DELETE FROM link_preview WHERE key = ?", [key])?
            > 0)
    }
}

/// The current Unix time in milliseconds.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}
