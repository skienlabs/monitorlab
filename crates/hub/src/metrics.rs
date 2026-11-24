//! Time-series storage: raw samples (default 3 days) + 5-minute rollups (default 90 days).

use std::time::Duration;

use anyhow::Result;
use rusqlite::params;
use serde_json::{json, Value};
use monitorlab_common::MetricsSample;

use crate::db::Db;
use crate::util::now_ms;

const COLS: &str = "cpu, mem_pct, mem_used, swap_pct, net_rx, net_tx, disk_read, disk_write, disk_pct, load1, mssql_cpu, mssql_mem, iis_cpu, iis_mem, containers_running";

pub async fn insert(db: &Db, agent_id: String, m: MetricsSample) -> Result<()> {
    db.run(move |c| {
        let pct = |used: u64, total: u64| if total > 0 { used as f64 * 100.0 / total as f64 } else { 0.0 };
        let disk_pct = m.disks.iter().map(|d| pct(d.used, d.total)).fold(0.0f64, f64::max);
        let (mssql_cpu, mssql_mem) = m
            .mssql
            .as_ref()
            .map(|s| s.instances.iter().fold((0.0f64, 0i64), |a, i| (a.0 + i.cpu as f64, a.1 + i.mem as i64)))
            .map(|(c, m)| (Some(c), Some(m)))
            .unwrap_or((None, None));
        let (iis_cpu, iis_mem) = m.iis.as_ref().map(|i| (Some(i.worker_cpu as f64), Some(i.worker_mem as i64))).unwrap_or((None, None));
        c.execute(
            &format!("INSERT OR REPLACE INTO metrics (agent_id, ts, {COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)"),
            params![
                agent_id,
                m.ts,
                m.cpu as f64,
                pct(m.mem_used, m.mem_total),
                m.mem_used as i64,
                pct(m.swap_used, m.swap_total),
                m.net_rx_bps,
                m.net_tx_bps,
                m.disk_read_bps,
                m.disk_write_bps,
                disk_pct,
                m.load.map(|l| l[0]),
                mssql_cpu,
                mssql_mem,
                iis_cpu,
                iis_mem,
                m.docker.as_ref().map(|d| d.running as i64),
            ],
        )?;
        Ok(())
    })
    .await
}

/// (lookback ms, source table, bucket ms)
fn plan(range: &str) -> (i64, &'static str, i64) {
    const M: i64 = 60_000;
    match range {
        "15m" => (15 * M, "metrics", 1_000),
        "1h" => (60 * M, "metrics", 1_000),
        "6h" => (6 * 60 * M, "metrics", M),
        "24h" => (24 * 60 * M, "metrics", 4 * M),
        "7d" => (7 * 24 * 60 * M, "metrics_5m", 30 * M),
        "30d" => (30 * 24 * 60 * M, "metrics_5m", 120 * M),
        _ => (60 * M, "metrics", 1_000),
    }
}

/// Columnar series ready for uPlot. Gaps (agent offline) are emitted as nulls.
pub async fn query(db: &Db, agent_id: String, range: String, interval_secs: u32) -> Result<Value> {
    let (lookback, table, bucket) = plan(&range);
    let since = now_ms() - lookback;
    db.run(move |c| {
        let sql = format!(
            "SELECT (ts / ?3) * ?3 AS t, AVG(cpu), AVG(mem_pct), AVG(mem_used), AVG(swap_pct), AVG(net_rx), AVG(net_tx), \
             AVG(disk_read), AVG(disk_write), MAX(disk_pct), AVG(load1), AVG(mssql_cpu), AVG(mssql_mem), AVG(iis_cpu), AVG(iis_mem), \
             AVG(containers_running), MAX(cpu) \
             FROM {table} WHERE agent_id = ?1 AND ts >= ?2 GROUP BY t ORDER BY t"
        );
        let mut stmt = c.prepare(&sql)?;
        let names = [
            "cpu", "mem_pct", "mem_used", "swap_pct", "net_rx", "net_tx", "disk_read", "disk_write", "disk_pct", "load1",
            "mssql_cpu", "mssql_mem", "iis_cpu", "iis_mem", "containers_running", "cpu_max",
        ];
        let mut ts: Vec<Value> = Vec::new();
        let mut cols: Vec<Vec<Value>> = vec![Vec::new(); names.len()];
        let step = if table == "metrics" { (interval_secs as i64 * 1000).max(bucket) } else { bucket.max(300_000) };
        let gap = step * 5 / 2;
        let mut last_t: Option<i64> = None;
        let mut rows = stmt.query(params![agent_id, since, bucket])?;
        while let Some(r) = rows.next()? {
            let t: i64 = r.get(0)?;
            if let Some(lt) = last_t {
                if t - lt > gap {
                    ts.push(json!((lt + step) / 1000));
                    for col in cols.iter_mut() {
                        col.push(Value::Null);
                    }
                }
            }
            last_t = Some(t);
            ts.push(json!(t / 1000));
            for (i, col) in cols.iter_mut().enumerate() {
                let v: Option<f64> = r.get(i + 1)?;
                col.push(v.map(|x| json!((x * 100.0).round() / 100.0)).unwrap_or(Value::Null));
            }
        }
        let mut series = serde_json::Map::new();
        for (i, n) in names.iter().enumerate() {
            series.insert(n.to_string(), Value::Array(std::mem::take(&mut cols[i])));
        }
        Ok(json!({ "range": range, "bucket_ms": bucket, "ts": ts, "series": series }))
    })
    .await
}

/// Roll raw samples into 5-minute buckets and enforce retention. Runs every 5 minutes.
pub async fn maintenance_loop(db: Db, raw_days: u32, days: u32) {
    let mut tick = tokio::time::interval(Duration::from_secs(300));
    loop {
        tick.tick().await;
        if let Err(e) = maintenance(&db, raw_days, days).await {
            tracing::warn!("metrics maintenance failed: {e:#}");
        }
    }
}

async fn maintenance(db: &Db, raw_days: u32, days: u32) -> Result<()> {
    const B: i64 = 300_000;
    let wm: i64 = db.get_setting("rollup_watermark").await?.and_then(|v| v.parse().ok()).unwrap_or(0);
    let until = (now_ms() / B) * B; // only complete buckets
    let from = if wm == 0 { until - raw_days as i64 * 86_400_000 } else { wm };
    if until > from {
        db.run(move |c| {
            c.execute(
                &format!(
                    "INSERT OR REPLACE INTO metrics_5m (agent_id, ts, {COLS}) \
                     SELECT agent_id, (ts / {B}) * {B}, AVG(cpu), AVG(mem_pct), AVG(mem_used), AVG(swap_pct), AVG(net_rx), AVG(net_tx), \
                     AVG(disk_read), AVG(disk_write), MAX(disk_pct), AVG(load1), AVG(mssql_cpu), AVG(mssql_mem), AVG(iis_cpu), AVG(iis_mem), \
                     AVG(containers_running) FROM metrics WHERE ts >= ?1 AND ts < ?2 GROUP BY agent_id, (ts / {B})"
                ),
                params![from, until],
            )?;
            Ok(())
        })
        .await?;
        db.set_setting("rollup_watermark", until.to_string()).await?;
    }
    let raw_cut = now_ms() - raw_days.max(1) as i64 * 86_400_000;
    let cut = now_ms() - days.max(7) as i64 * 86_400_000;
    let audit_cut = now_ms() - 365 * 86_400_000;
    db.run(move |c| {
        c.execute("DELETE FROM metrics WHERE ts < ?1", [raw_cut])?;
        c.execute("DELETE FROM metrics_5m WHERE ts < ?1", [cut])?;
        c.execute("DELETE FROM audit WHERE ts < ?1", [audit_cut])?;
        c.execute("DELETE FROM sessions WHERE expires_at < ?1", [now_ms()])?;
        c.execute("UPDATE agents SET enroll_token_hash = NULL WHERE enroll_expires_at < ?1", [now_ms()])?;
        Ok(())
    })
    .await
}
