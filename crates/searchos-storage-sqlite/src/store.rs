//! SQLite connection wrapper.

use rusqlite::Connection;
use searchos_domain::SearchOsError;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// SQLite store: one connection guarded by a mutex (single-process WAL).
pub struct SqliteStore {
    conn: Mutex<Connection>,
    path: PathBuf,
}

impl SqliteStore {
    /// Opens (creating) the DB at `path` and runs pending migrations.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, rusqlite::Error> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    rusqlite::Error::SqliteFailure(
                        rusqlite::ffi::Error::new(1),
                        Some(format!("create_dir_all: {e}")),
                    )
                })?;
            }
        }
        let conn = Connection::open(&path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "busy_timeout", 5000)?;
        crate::migrations::migrate(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            path,
        })
    }

    /// Opens an in-memory DB (tests, dry runs).
    pub fn open_in_memory() -> Result<Self, rusqlite::Error> {
        let conn = Connection::open_in_memory()?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        crate::migrations::migrate(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            path: ":memory:".into(),
        })
    }

    /// Runs a closure with exclusive access to the connection. Closures
    /// return the unified [`SearchOsError`]; rusqlite errors are mapped by
    /// the store itself.
    pub fn with_conn<T>(
        &self,
        f: impl FnOnce(&Connection) -> Result<T, SearchOsError>,
    ) -> Result<T, SearchOsError> {
        let guard = self
            .conn
            .lock()
            .map_err(|_| SearchOsError::Storage("sqlite mutex poisoned".into()))?;
        f(&guard)
    }

    /// DB file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Schema version after migrations.
    pub fn schema_version(&self) -> Result<i64, SearchOsError> {
        self.with_conn(|c| {
            c.query_row("PRAGMA user_version", [], |r| r.get(0))
                .map_err(|e| SearchOsError::Storage(e.to_string()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_store_ready() {
        let s = SqliteStore::open_in_memory().unwrap();
        assert_eq!(
            s.schema_version().unwrap(),
            crate::migrations::MIGRATIONS.last().unwrap().version
        );
        s.with_conn(|c| {
            c.execute(
                "INSERT INTO workspaces (id,name,config_version,created_at,updated_at) VALUES ('ws1','default',1,'t','t')",
                [],
            )
            .map_err(|e| SearchOsError::Storage(e.to_string()))
        })
        .unwrap();
        let n: i64 = s
            .with_conn(|c| {
                c.query_row("SELECT count(*) FROM workspaces", [], |r| r.get(0))
                    .map_err(|e| SearchOsError::Storage(e.to_string()))
            })
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn file_store_persists() {
        let dir = std::env::temp_dir().join(format!("searchos-sqlite-{}", std::process::id()));
        let db = dir.join("data/searchos.db");
        {
            let s = SqliteStore::open(&db).unwrap();
            s.with_conn(|c| c.execute("INSERT INTO workspaces (id,name,config_version,created_at,updated_at) VALUES ('wsX','x',1,'t','t')", []).map_err(|e| SearchOsError::Storage(e.to_string()))).unwrap();
        }
        {
            let s = SqliteStore::open(&db).unwrap();
            let n: i64 = s
                .with_conn(|c| {
                    c.query_row("SELECT count(*) FROM workspaces WHERE id='wsX'", [], |r| {
                        r.get(0)
                    })
                    .map_err(|e| SearchOsError::Storage(e.to_string()))
                })
                .unwrap();
            assert_eq!(n, 1);
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}
