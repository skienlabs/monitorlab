//! PostgreSQL monitoring through a read-only role configured in `agent.toml` (`[postgres.connection]`).
//!
//! Every call opens a short-lived connection, runs a handful of catalog queries and closes it, so the agent
//! never holds a session open on the database. Nothing here changes data except the explicit
//! cancel/terminate of a backend that the user asked for.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use monitorlab_common::{PgBackendKind, PgSummary};
use serde_json::{json, Value};
use tokio_postgres::{Client, Config, NoTls};

use crate::config::{PostgresConfig, PostgresConnection};

pub struct Postgres {
    conn: Option<PostgresConnection>,
}

#[derive(Debug)]
struct NoVerify;

impl rustls::client::danger::ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &[rustls::pki_types::CertificateDer<'_>],
        _: &rustls::pki_types::ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(&self, m: &[u8], c: &rustls::pki_types::CertificateDer<'_>, d: &rustls::DigitallySignedStruct) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(m, c, d, &rustls::crypto::ring::default_provider().signature_verification_algorithms)
    }
    fn verify_tls13_signature(&self, m: &[u8], c: &rustls::pki_types::CertificateDer<'_>, d: &rustls::DigitallySignedStruct) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(m, c, d, &rustls::crypto::ring::default_provider().signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider().signature_verification_algorithms.supported_schemes()
    }
}

impl Postgres {
    pub fn new(cfg: PostgresConfig) -> Self {
        Self { conn: cfg.connection }
    }

    pub fn configured(&self) -> bool {
        self.conn.is_some()
    }

    async fn connect(&self) -> Result<Client> {
        let c = self.conn.as_ref().ok_or_else(|| anyhow!("PostgreSQL is not configured on this agent"))?;
        let mut cfg = Config::new();
        cfg.host(&c.host).port(c.port).user(&c.user).dbname(&c.database).application_name("monitorlab").connect_timeout(Duration::from_secs(5));
        if !c.password.is_empty() {
            cfg.password(&c.password);
        }
        let client = if c.sslmode.eq_ignore_ascii_case("disable") {
            let (client, conn) = tokio::time::timeout(Duration::from_secs(8), cfg.connect(NoTls)).await.map_err(|_| anyhow!("connection timed out"))??;
            tokio::spawn(async move {
                let _ = conn.await;
            });
            client
        } else {
            let tls = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()?
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(NoVerify))
                .with_no_client_auth();
            let (client, conn) = tokio::time::timeout(Duration::from_secs(8), cfg.connect(tokio_postgres_rustls::MakeRustlsConnect::new(tls)))
                .await
                .map_err(|_| anyhow!("connection timed out"))??;
            tokio::spawn(async move {
                let _ = conn.await;
            });
            client
        };
        client.batch_execute("SET statement_timeout = '8s'; SET default_transaction_read_only = on").await.ok();
        Ok(client)
    }

    /// Short, readable reason for a failed connection.
    fn describe(e: &anyhow::Error) -> String {
        if let Some(pe) = e.downcast_ref::<tokio_postgres::Error>() {
            if let Some(db) = pe.as_db_error() {
                return format!("{} ({})", db.message(), db.code().code());
            }
            let mut s = pe.to_string();
            let mut src = std::error::Error::source(pe);
            while let Some(x) = src {
                s.push_str(": ");
                s.push_str(&x.to_string());
                src = x.source();
            }
            return s;
        }
        e.to_string()
    }

    /// Light health sample for the periodic metrics.
    pub async fn summary(&self) -> Option<PgSummary> {
        self.conn.as_ref()?;
        let r = tokio::time::timeout(Duration::from_secs(12), self.summary_inner()).await;
        Some(match r {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => PgSummary { up: false, error: Some(Self::describe(&e)), ..Default::default() },
            Err(_) => PgSummary { up: false, error: Some("timed out".into()), ..Default::default() },
        })
    }

    async fn summary_inner(&self) -> Result<PgSummary> {
        let c = self.connect().await?;
        let row = c
            .query_one(
                "SELECT version(), current_setting('max_connections')::bigint, pg_is_in_recovery(), \
                 (SELECT count(*) FROM pg_stat_activity WHERE backend_type = 'client backend')::bigint, \
                 (SELECT count(*) FROM pg_stat_activity WHERE backend_type = 'client backend' AND wait_event_type = 'Lock')::bigint, \
                 COALESCE((SELECT sum(pg_database_size(datname)) FROM pg_database WHERE NOT datistemplate), 0)::bigint, \
                 COALESCE(EXTRACT(EPOCH FROM now() - pg_last_xact_replay_timestamp()), 0)::float8",
                &[],
            )
            .await?;
        let replica: bool = row.get(2);
        Ok(PgSummary {
            up: true,
            error: None,
            version: short_version(&row.get::<_, String>(0)),
            max_connections: row.get(1),
            replica,
            connections: row.get(3),
            blocked: row.get(4),
            db_bytes: row.get(5),
            replay_lag_s: if replica { row.get(6) } else { 0.0 },
        })
    }

    pub async fn overview(&self) -> Result<Value> {
        let c = match self.connect().await {
            Ok(c) => c,
            Err(e) => bail!("PostgreSQL unreachable: {}", Self::describe(&e)),
        };
        let head = c
            .query_one(
                "SELECT version(), EXTRACT(EPOCH FROM now() - pg_postmaster_start_time())::float8, \
                 current_setting('max_connections')::bigint, pg_is_in_recovery(), current_user::text, \
                 COALESCE((SELECT max(EXTRACT(EPOCH FROM now() - xact_start)) FROM pg_stat_activity \
                           WHERE xact_start IS NOT NULL AND state <> 'idle' AND pid <> pg_backend_pid()), 0)::float8, \
                 COALESCE(EXTRACT(EPOCH FROM now() - pg_last_xact_replay_timestamp()), 0)::float8",
                &[],
            )
            .await?;
        let replica: bool = head.get(3);
        let states = c
            .query(
                "SELECT COALESCE(state, 'other')::text, count(*)::bigint, \
                 count(*) FILTER (WHERE wait_event_type = 'Lock')::bigint \
                 FROM pg_stat_activity WHERE backend_type = 'client backend' GROUP BY 1",
                &[],
            )
            .await?;
        let (mut active, mut idle, mut idle_tx, mut total, mut blocked) = (0i64, 0i64, 0i64, 0i64, 0i64);
        for r in &states {
            let (s, n, b): (String, i64, i64) = (r.get(0), r.get(1), r.get(2));
            total += n;
            blocked += b;
            match s.as_str() {
                "active" => active += n,
                "idle" => idle += n,
                x if x.starts_with("idle in transaction") => idle_tx += n,
                _ => {}
            }
        }
        let dbs = c
            .query(
                "SELECT d.datname::text, pg_database_size(d.datname)::bigint, COALESCE(s.numbackends, 0)::bigint, \
                 COALESCE(s.xact_commit, 0)::bigint, COALESCE(s.xact_rollback, 0)::bigint, COALESCE(s.blks_hit, 0)::bigint, \
                 COALESCE(s.blks_read, 0)::bigint, COALESCE(s.deadlocks, 0)::bigint, COALESCE(s.temp_bytes, 0)::bigint \
                 FROM pg_database d LEFT JOIN pg_stat_database s ON s.datid = d.oid \
                 WHERE NOT d.datistemplate AND d.datallowconn ORDER BY 2 DESC",
                &[],
            )
            .await?;
        let databases: Vec<Value> = dbs
            .iter()
            .map(|r| {
                let (hit, read): (i64, i64) = (r.get(5), r.get(6));
                json!({
                    "name": r.get::<_, String>(0), "size": r.get::<_, i64>(1), "connections": r.get::<_, i64>(2),
                    "commits": r.get::<_, i64>(3), "rollbacks": r.get::<_, i64>(4), "cache_hit": if hit + read > 0 { Some(hit as f64 / (hit + read) as f64 * 100.0) } else { None },
                    "deadlocks": r.get::<_, i64>(7), "temp_bytes": r.get::<_, i64>(8),
                })
            })
            .collect();
        let mut replication = vec![];
        if !replica {
            if let Ok(rows) = c
                .query(
                    "SELECT COALESCE(client_addr::text, ''), COALESCE(application_name, '')::text, COALESCE(state, '')::text, \
                     COALESCE(sync_state, '')::text, COALESCE(pg_wal_lsn_diff(pg_current_wal_lsn(), replay_lsn), 0)::float8, \
                     COALESCE(EXTRACT(EPOCH FROM replay_lag), 0)::float8 FROM pg_stat_replication",
                    &[],
                )
                .await
            {
                for r in rows {
                    replication.push(json!({
                        "client": r.get::<_, String>(0), "name": r.get::<_, String>(1), "state": r.get::<_, String>(2),
                        "sync": r.get::<_, String>(3), "lag_bytes": r.get::<_, f64>(4), "lag_s": r.get::<_, f64>(5),
                    }));
                }
            }
        }
        Ok(json!({
            "version": head.get::<_, String>(0), "short_version": short_version(&head.get::<_, String>(0)),
            "uptime_s": head.get::<_, f64>(1), "max_connections": head.get::<_, i64>(2), "replica": replica,
            "user": head.get::<_, String>(4), "longest_xact_s": head.get::<_, f64>(5),
            "replay_lag_s": if replica { head.get::<_, f64>(6) } else { 0.0 },
            "connections": { "total": total, "active": active, "idle": idle, "idle_in_transaction": idle_tx, "blocked": blocked },
            "databases": databases, "replication": replication,
        }))
    }

    pub async fn activity(&self) -> Result<Value> {
        let c = match self.connect().await {
            Ok(c) => c,
            Err(e) => bail!("PostgreSQL unreachable: {}", Self::describe(&e)),
        };
        let rows = c
            .query(
                "SELECT pid::bigint, COALESCE(datname, '')::text, COALESCE(usename, '')::text, COALESCE(application_name, '')::text, \
                 COALESCE(client_addr::text, ''), COALESCE(state, '')::text, COALESCE(wait_event_type, '')::text, COALESCE(wait_event, '')::text, \
                 COALESCE(EXTRACT(EPOCH FROM now() - query_start), 0)::float8, COALESCE(EXTRACT(EPOCH FROM now() - xact_start), 0)::float8, \
                 left(COALESCE(query, ''), 1000), COALESCE(array_length(pg_blocking_pids(pid), 1), 0)::bigint \
                 FROM pg_stat_activity WHERE backend_type = 'client backend' AND pid <> pg_backend_pid() \
                 ORDER BY (state = 'active') DESC NULLS LAST, 9 DESC LIMIT 300",
                &[],
            )
            .await?;
        let list: Vec<Value> = rows
            .iter()
            .map(|r| {
                json!({
                    "pid": r.get::<_, i64>(0), "database": r.get::<_, String>(1), "user": r.get::<_, String>(2), "app": r.get::<_, String>(3),
                    "client": r.get::<_, String>(4), "state": r.get::<_, String>(5), "wait_type": r.get::<_, String>(6), "wait": r.get::<_, String>(7),
                    "query_s": r.get::<_, f64>(8), "xact_s": r.get::<_, f64>(9), "query": r.get::<_, String>(10), "blocked_by": r.get::<_, i64>(11),
                })
            })
            .collect();
        Ok(json!({ "sessions": list }))
    }

    pub async fn backend(&self, pid: i32, kind: PgBackendKind) -> Result<Value> {
        let c = self.connect().await.map_err(|e| anyhow!("PostgreSQL unreachable: {}", Self::describe(&e)))?;
        // This is the one write-capable call: allow it even though the session is read-only by default.
        let sql = match kind {
            PgBackendKind::Cancel => "SELECT pg_cancel_backend($1::int)",
            PgBackendKind::Terminate => "SELECT pg_terminate_backend($1::int)",
        };
        let row = c.query_one(sql, &[&pid]).await.map_err(|e| anyhow!("{}", Self::describe(&e.into())))?;
        let ok: bool = row.get(0);
        if !ok {
            bail!("PostgreSQL did not signal that session (it may have finished already, or the role lacks permission)");
        }
        Ok(json!({ "ok": true, "pid": pid }))
    }
}

fn short_version(v: &str) -> String {
    // "PostgreSQL 16.4 (Debian 16.4-1) on x86_64-..." -> "PostgreSQL 16.4"
    v.split(" on ").next().unwrap_or(v).split(" (").next().unwrap_or(v).to_string()
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_is_shortened() {
        assert_eq!(super::short_version("PostgreSQL 16.4 (Debian 16.4-1.pgdg120+1) on x86_64-pc-linux-gnu, compiled by gcc"), "PostgreSQL 16.4");
    }
}
