//! SQLite storage. One connection behind a mutex, used from the blocking pool.
//! WAL mode keeps writes cheap; the workload is small (a row per agent every 10s).

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use rusqlite::Connection;

#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

const MIGRATIONS: &[&str] = &[
    // v1
    r#"
    CREATE TABLE roles (
        id INTEGER PRIMARY KEY,
        name TEXT NOT NULL UNIQUE COLLATE NOCASE,
        description TEXT NOT NULL DEFAULT '',
        permissions TEXT NOT NULL DEFAULT '[]',
        builtin INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE users (
        id INTEGER PRIMARY KEY,
        email TEXT NOT NULL UNIQUE COLLATE NOCASE,
        name TEXT NOT NULL DEFAULT '',
        password_hash TEXT NOT NULL,
        role_id INTEGER NOT NULL REFERENCES roles(id),
        server_scope TEXT NULL,
        disabled INTEGER NOT NULL DEFAULT 0,
        must_change_password INTEGER NOT NULL DEFAULT 0,
        created_at INTEGER NOT NULL,
        last_login_at INTEGER NULL
    );
    CREATE TABLE sessions (
        token_hash TEXT PRIMARY KEY,
        user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
        created_at INTEGER NOT NULL,
        last_seen_at INTEGER NOT NULL,
        expires_at INTEGER NOT NULL,
        ip TEXT NOT NULL DEFAULT '',
        user_agent TEXT NOT NULL DEFAULT ''
    );
    CREATE INDEX sessions_user ON sessions(user_id);
    CREATE TABLE agents (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL DEFAULT '',
        secret_hash TEXT NULL,
        enroll_token_hash TEXT NULL,
        enroll_expires_at INTEGER NULL,
        hello TEXT NULL,
        created_at INTEGER NOT NULL,
        enrolled_at INTEGER NULL,
        last_seen_at INTEGER NULL,
        is_local INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE metrics (
        agent_id TEXT NOT NULL,
        ts INTEGER NOT NULL,
        cpu REAL, mem_pct REAL, mem_used INTEGER, swap_pct REAL,
        net_rx REAL, net_tx REAL, disk_read REAL, disk_write REAL, disk_pct REAL,
        load1 REAL, mssql_cpu REAL, mssql_mem INTEGER, iis_cpu REAL, iis_mem INTEGER,
        containers_running INTEGER,
        PRIMARY KEY (agent_id, ts)
    ) WITHOUT ROWID;
    CREATE TABLE metrics_5m (
        agent_id TEXT NOT NULL,
        ts INTEGER NOT NULL,
        cpu REAL, mem_pct REAL, mem_used INTEGER, swap_pct REAL,
        net_rx REAL, net_tx REAL, disk_read REAL, disk_write REAL, disk_pct REAL,
        load1 REAL, mssql_cpu REAL, mssql_mem INTEGER, iis_cpu REAL, iis_mem INTEGER,
        containers_running INTEGER,
        PRIMARY KEY (agent_id, ts)
    ) WITHOUT ROWID;
    CREATE TABLE audit (
        id INTEGER PRIMARY KEY,
        ts INTEGER NOT NULL,
        user_id INTEGER NULL,
        user_email TEXT NOT NULL DEFAULT '',
        ip TEXT NOT NULL DEFAULT '',
        action TEXT NOT NULL,
        server_id TEXT NULL,
        server_name TEXT NULL,
        target TEXT NULL,
        detail TEXT NULL,
        ok INTEGER NOT NULL DEFAULT 1
    );
    CREATE INDEX audit_ts ON audit(ts);
    CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
    "#,
    // v2: alerting
    r#"
    CREATE TABLE alerts (
        id INTEGER PRIMARY KEY,
        agent_id TEXT NOT NULL,
        rule TEXT NOT NULL,
        severity TEXT NOT NULL,
        message TEXT NOT NULL,
        started_at INTEGER NOT NULL,
        resolved_at INTEGER NULL,
        acked_by TEXT NULL,
        acked_at INTEGER NULL
    );
    CREATE INDEX alerts_open ON alerts(agent_id, resolved_at);
    CREATE TABLE channels (
        id INTEGER PRIMARY KEY,
        name TEXT NOT NULL,
        kind TEXT NOT NULL,
        url TEXT NOT NULL,
        enabled INTEGER NOT NULL DEFAULT 1,
        min_severity TEXT NOT NULL DEFAULT 'warning',
        last_error TEXT NULL,
        last_sent_at INTEGER NULL,
        created_at INTEGER NOT NULL
    );
    "#,
];

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        // The database holds password hashes and the SMTP password: owner-only.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for p in [path.to_path_buf(), path.with_extension("db-wal"), path.with_extension("db-shm")] {
                if p.exists() {
                    let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
                }
            }
        }
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        for (i, sql) in MIGRATIONS.iter().enumerate() {
            let v = (i + 1) as i64;
            if v > version {
                conn.execute_batch(&format!("BEGIN; {sql}; PRAGMA user_version = {v}; COMMIT;"))?;
                tracing::info!("database migrated to v{v}");
            }
        }
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    /// Run `f` with the connection on the blocking thread pool.
    pub async fn run<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let mut c = conn.lock().unwrap_or_else(|e| e.into_inner());
            f(&mut c)
        })
        .await?
    }

    pub async fn get_setting(&self, key: &'static str) -> Result<Option<String>> {
        self.run(move |c| {
            use rusqlite::OptionalExtension;
            Ok(c.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0)).optional()?)
        })
        .await
    }

    pub async fn set_setting(&self, key: &'static str, value: String) -> Result<()> {
        self.run(move |c| {
            c.execute(
                "INSERT INTO settings(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                rusqlite::params![key, value],
            )?;
            Ok(())
        })
        .await
    }
}
