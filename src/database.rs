use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::models::{DownloadRecord, DownloadStatus};

pub type SharedDatabase = Arc<Mutex<Database>>;

pub struct Database {
    connection: Connection,
    path: PathBuf,
}

impl Database {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("cannot create database directory {}", parent.display())
            })?;
        }
        let connection = Connection::open(&path)
            .with_context(|| format!("cannot open database {}", path.display()))?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        let database = Self { connection, path };
        database.migrate()?;
        Ok(database)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn migrate(&self) -> Result<()> {
        self.connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS downloads (
                id TEXT PRIMARY KEY NOT NULL,
                url TEXT NOT NULL,
                file_name TEXT NOT NULL,
                total_bytes INTEGER,
                downloaded_bytes INTEGER NOT NULL DEFAULT 0,
                status TEXT NOT NULL,
                save_path TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                completed_at INTEGER,
                speed_bps REAL NOT NULL DEFAULT 0,
                eta_seconds INTEGER,
                error TEXT,
                priority INTEGER NOT NULL DEFAULT 0,
                connections INTEGER NOT NULL DEFAULT 4,
                expected_sha256 TEXT,
                content_type TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_downloads_created_at ON downloads(created_at DESC);
            CREATE INDEX IF NOT EXISTS idx_downloads_status ON downloads(status);
            CREATE TABLE IF NOT EXISTS app_metadata (
                key TEXT PRIMARY KEY NOT NULL,
                value TEXT NOT NULL
            );",
        )?;
        Ok(())
    }

    pub fn load_downloads(&self) -> Result<Vec<DownloadRecord>> {
        let mut statement = self.connection.prepare(
            "SELECT id, url, file_name, total_bytes, downloaded_bytes, status,
                    save_path, created_at, completed_at, speed_bps, eta_seconds,
                    error, priority, connections, expected_sha256, content_type
             FROM downloads ORDER BY priority DESC, created_at DESC",
        )?;
        let records = statement
            .query_map([], |row| {
                Ok(DownloadRecord {
                    id: row.get(0)?,
                    url: row.get(1)?,
                    file_name: row.get(2)?,
                    total_bytes: row.get(3)?,
                    downloaded_bytes: row.get(4)?,
                    status: DownloadStatus::from_db_value(row.get::<_, String>(5)?.as_str()),
                    save_path: row.get(6)?,
                    created_at: row.get(7)?,
                    completed_at: row.get(8)?,
                    speed_bps: row.get(9)?,
                    eta_seconds: row.get(10)?,
                    error: row.get(11)?,
                    priority: row.get(12)?,
                    connections: row.get(13)?,
                    expected_sha256: row.get(14)?,
                    content_type: row.get(15)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(records)
    }

    pub fn find(&self, id: &str) -> Result<Option<DownloadRecord>> {
        self.connection
            .query_row(
                "SELECT id, url, file_name, total_bytes, downloaded_bytes, status,
                        save_path, created_at, completed_at, speed_bps, eta_seconds,
                        error, priority, connections, expected_sha256, content_type
                 FROM downloads WHERE id = ?1",
                [id],
                |row| {
                    Ok(DownloadRecord {
                        id: row.get(0)?,
                        url: row.get(1)?,
                        file_name: row.get(2)?,
                        total_bytes: row.get(3)?,
                        downloaded_bytes: row.get(4)?,
                        status: DownloadStatus::from_db_value(row.get::<_, String>(5)?.as_str()),
                        save_path: row.get(6)?,
                        created_at: row.get(7)?,
                        completed_at: row.get(8)?,
                        speed_bps: row.get(9)?,
                        eta_seconds: row.get(10)?,
                        error: row.get(11)?,
                        priority: row.get(12)?,
                        connections: row.get(13)?,
                        expected_sha256: row.get(14)?,
                        content_type: row.get(15)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn insert(&self, record: &DownloadRecord) -> Result<()> {
        self.connection.execute(
            "INSERT INTO downloads (
                id, url, file_name, total_bytes, downloaded_bytes, status,
                save_path, created_at, completed_at, speed_bps, eta_seconds,
                error, priority, connections, expected_sha256, content_type
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                record.id,
                record.url,
                record.file_name,
                record.total_bytes,
                record.downloaded_bytes,
                record.status.as_db_value(),
                record.save_path,
                record.created_at,
                record.completed_at,
                record.speed_bps,
                record.eta_seconds,
                record.error,
                record.priority,
                record.connections,
                record.expected_sha256,
                record.content_type,
            ],
        )?;
        Ok(())
    }

    pub fn update(&self, record: &DownloadRecord) -> Result<()> {
        self.connection.execute(
            "UPDATE downloads SET
                url = ?2, file_name = ?3, total_bytes = ?4, downloaded_bytes = ?5,
                status = ?6, save_path = ?7, created_at = ?8, completed_at = ?9,
                speed_bps = ?10, eta_seconds = ?11, error = ?12, priority = ?13,
                connections = ?14, expected_sha256 = ?15, content_type = ?16
             WHERE id = ?1",
            params![
                record.id,
                record.url,
                record.file_name,
                record.total_bytes,
                record.downloaded_bytes,
                record.status.as_db_value(),
                record.save_path,
                record.created_at,
                record.completed_at,
                record.speed_bps,
                record.eta_seconds,
                record.error,
                record.priority,
                record.connections,
                record.expected_sha256,
                record.content_type,
            ],
        )?;
        Ok(())
    }

    pub fn update_progress(
        &self,
        id: &str,
        downloaded_bytes: u64,
        total_bytes: Option<u64>,
        speed_bps: f64,
        eta_seconds: Option<u64>,
    ) -> Result<()> {
        self.connection.execute(
            "UPDATE downloads SET downloaded_bytes = ?2, total_bytes = COALESCE(?3, total_bytes),
             speed_bps = ?4, eta_seconds = ?5 WHERE id = ?1",
            params![id, downloaded_bytes, total_bytes, speed_bps, eta_seconds],
        )?;
        Ok(())
    }

    pub fn update_status(
        &self,
        id: &str,
        status: DownloadStatus,
        error: Option<&str>,
        completed_at: Option<i64>,
    ) -> Result<()> {
        self.connection.execute(
            "UPDATE downloads SET status = ?2, error = ?3, completed_at = ?4 WHERE id = ?1",
            params![id, status.as_db_value(), error, completed_at],
        )?;
        Ok(())
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        self.connection
            .execute("DELETE FROM downloads WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn set_priority(&self, id: &str, priority: i32) -> Result<()> {
        self.connection
            .execute("UPDATE downloads SET priority = ?2 WHERE id = ?1", params![id, priority])?;
        Ok(())
    }
}
