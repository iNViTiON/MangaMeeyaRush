//! SQLite-backed persistence for mmce: books, bookmarks, history,
//! profiles, and settings.
//!
//! Schema versioning is handled via a `meta` table; `Store::open` runs any
//! outstanding migrations on open and is idempotent on re-open.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};

const CURRENT_VERSION: i64 = 1;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("unsupported schema version {0}")]
    UnsupportedVersion(i64),
    #[error("{0}")]
    Other(String),
}

pub type Result<T, E = StoreError> = std::result::Result<T, E>;

pub type BookId = i64;

#[derive(Debug, Clone)]
pub struct BookRow {
    pub id: BookId,
    pub path: String,
    pub kind: String,
    pub page_count: Option<i64>,
    pub last_opened_at: Option<i64>,
    pub last_page: i64,
}

#[derive(Debug, Clone)]
pub struct BookmarkRow {
    pub id: i64,
    pub book_id: BookId,
    pub page: i64,
    pub label: Option<String>,
    pub created_at: i64,
}

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| StoreError::Other(format!("mkdir {}: {e}", parent.display())))?;
            }
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    /// Open an in-memory DB — handy for tests and for "no persistence"
    /// runs.
    pub fn open_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    fn schema_version(&self) -> Result<i64> {
        // Make sure the meta table exists, then read.
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )?;
        let v: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        Ok(v.and_then(|s| s.parse().ok()).unwrap_or(0))
    }

    fn migrate(&self) -> Result<()> {
        let v = self.schema_version()?;
        if v > CURRENT_VERSION {
            return Err(StoreError::UnsupportedVersion(v));
        }
        if v < 1 {
            self.conn.execute_batch(
                "BEGIN;
                 CREATE TABLE book (
                     id             INTEGER PRIMARY KEY,
                     path           TEXT UNIQUE NOT NULL,
                     kind           TEXT NOT NULL,
                     page_count     INTEGER,
                     last_opened_at INTEGER,
                     last_page      INTEGER NOT NULL DEFAULT 0
                 );
                 CREATE INDEX book_last_opened_idx ON book(last_opened_at DESC);

                 CREATE TABLE bookmark (
                     id         INTEGER PRIMARY KEY,
                     book_id    INTEGER NOT NULL REFERENCES book(id) ON DELETE CASCADE,
                     page       INTEGER NOT NULL,
                     label      TEXT,
                     created_at INTEGER NOT NULL,
                     UNIQUE (book_id, page)
                 );

                 CREATE TABLE profile (
                     name       TEXT PRIMARY KEY,
                     toml_blob  TEXT NOT NULL,
                     updated_at INTEGER NOT NULL
                 );

                 CREATE TABLE setting (
                     key   TEXT PRIMARY KEY,
                     value TEXT NOT NULL
                 );

                 INSERT OR REPLACE INTO meta(key, value) VALUES('schema_version', '1');
                 COMMIT;",
            )?;
        }
        Ok(())
    }

    // ---- books ---------------------------------------------------------

    pub fn upsert_book(&self, path: &str, kind: &str) -> Result<BookId> {
        self.conn.execute(
            "INSERT INTO book(path, kind, last_opened_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(path) DO UPDATE
               SET kind = excluded.kind,
                   last_opened_at = excluded.last_opened_at",
            params![path, kind, now_secs()],
        )?;
        let id: BookId = self
            .conn
            .query_row("SELECT id FROM book WHERE path = ?1", params![path], |r| {
                r.get(0)
            })?;
        Ok(id)
    }

    pub fn touch_book(&self, id: BookId, last_page: usize, page_count: usize) -> Result<()> {
        self.conn.execute(
            "UPDATE book
               SET last_opened_at = ?1,
                   last_page = ?2,
                   page_count = ?3
             WHERE id = ?4",
            params![now_secs(), last_page as i64, page_count as i64, id],
        )?;
        Ok(())
    }

    pub fn get_book_by_path(&self, path: &str) -> Result<Option<BookRow>> {
        let row = self
            .conn
            .query_row(
                "SELECT id, path, kind, page_count, last_opened_at, last_page
                 FROM book WHERE path = ?1",
                params![path],
                row_to_book,
            )
            .optional()?;
        Ok(row)
    }

    pub fn recent_books(&self, limit: u32) -> Result<Vec<BookRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, path, kind, page_count, last_opened_at, last_page
             FROM book
             WHERE last_opened_at IS NOT NULL
             ORDER BY last_opened_at DESC
             LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(params![limit as i64], row_to_book)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Delete books whose `last_opened_at` falls below the `keep_last`
    /// most-recent entries. Used to enforce an N-entry history cap.
    pub fn prune_history(&self, keep_last: u32) -> Result<usize> {
        let count = self.conn.execute(
            "DELETE FROM book WHERE id NOT IN (
                SELECT id FROM book
                WHERE last_opened_at IS NOT NULL
                ORDER BY last_opened_at DESC
                LIMIT ?1
            )",
            params![keep_last as i64],
        )?;
        Ok(count)
    }

    // ---- bookmarks -----------------------------------------------------

    /// Insert a bookmark at `page` for `book_id`. If one already exists at
    /// that page, updates the label.
    pub fn add_bookmark(
        &self,
        book_id: BookId,
        page: usize,
        label: Option<&str>,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO bookmark(book_id, page, label, created_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(book_id, page) DO UPDATE SET label = excluded.label",
            params![book_id, page as i64, label, now_secs()],
        )?;
        let id: i64 = self.conn.query_row(
            "SELECT id FROM bookmark WHERE book_id = ?1 AND page = ?2",
            params![book_id, page as i64],
            |r| r.get(0),
        )?;
        Ok(id)
    }

    pub fn remove_bookmark(&self, book_id: BookId, page: usize) -> Result<bool> {
        let n = self.conn.execute(
            "DELETE FROM bookmark WHERE book_id = ?1 AND page = ?2",
            params![book_id, page as i64],
        )?;
        Ok(n > 0)
    }

    pub fn list_bookmarks(&self, book_id: BookId) -> Result<Vec<BookmarkRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, book_id, page, label, created_at
             FROM bookmark WHERE book_id = ?1
             ORDER BY page ASC",
        )?;
        let rows = stmt
            .query_map(params![book_id], |r| {
                Ok(BookmarkRow {
                    id: r.get(0)?,
                    book_id: r.get(1)?,
                    page: r.get(2)?,
                    label: r.get(3)?,
                    created_at: r.get(4)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // ---- settings ------------------------------------------------------

    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT value FROM setting WHERE key = ?1",
                params![key],
                |r| r.get::<_, String>(0),
            )
            .optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO setting(key, value) VALUES(?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    // ---- profiles ------------------------------------------------------

    pub fn save_profile(&self, name: &str, toml_blob: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO profile(name, toml_blob, updated_at) VALUES(?1, ?2, ?3)
             ON CONFLICT(name) DO UPDATE
               SET toml_blob = excluded.toml_blob, updated_at = excluded.updated_at",
            params![name, toml_blob, now_secs()],
        )?;
        Ok(())
    }

    pub fn get_profile(&self, name: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT toml_blob FROM profile WHERE name = ?1",
                params![name],
                |r| r.get::<_, String>(0),
            )
            .optional()?)
    }

    pub fn list_profiles(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT name FROM profile ORDER BY name")?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

fn row_to_book(r: &rusqlite::Row<'_>) -> rusqlite::Result<BookRow> {
    Ok(BookRow {
        id: r.get(0)?,
        path: r.get(1)?,
        kind: r.get(2)?,
        page_count: r.get(3)?,
        last_opened_at: r.get(4)?,
        last_page: r.get(5)?,
    })
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
