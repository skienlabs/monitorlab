//! Per-container and per-pod CPU / memory history.
//! Raw samples (every 30–60 s) are kept 3 days; 15-minute averages / maxima 30 days.

use anyhow::Result;
use axum::extract::{Path, Query, State};
use axum::Json;
use rusqlite::params;
use serde::Deserialize;
use serde_json::{json, Value};
use monitorlab_common::MetricsSample;

use crate::auth::AuthUser;
use crate::db::Db;
use crate::rbac;
use crate::state::{ApiError, ApiResult, S};
use crate::util::now_ms;

pub const KIND_CONTAINER: i64 = 0;
pub const KIND_POD: i64 = 1;
const RAW_DAYS: i64 = 3;
const ROLLUP_DAYS: i64 = 30;
const B15: i64 = 15 * 60_000;

pub async fn insert(db: &Db, agent_id: &str, m: &MetricsSample) -> Result<()> {
    if m.containers.is_empty() && m.pods.is_empty() {
        return Ok(());
    }
    let id = agent_id.to_string();
    let ts = m.ts;
    let rows: Vec<(i64, String, String, f64, f64)> = m
        .containers
        .iter()
        .map(|c| (KIND_CONTAINER, c.name.clone(), c.project.clone().unwrap_or_default(), c.cpu, c.mem as f64))
        .chain(m.pods.iter().map(|p| (KIND_POD, format!("{}/{}", p.ns, p.name), format!("{}/{}", p.ns, p.owner), p.cpu, p.mem as f64)))
        .collect();
    db.run(move |c| {
        let tx = c.transaction()?;
        {
            let mut st = tx.prepare_cached("INSERT OR REPLACE INTO usage_raw (agent_id, kind, name, grp, ts, cpu, mem) VALUES (?1,?2,?3,?4,?5,?6,?7)")?;
            for (k, name, grp, cpu, mem) in &rows {
                st.execute(params![id, k, name, grp, ts, (cpu * 100.0).round() / 100.0, mem])?;
            }
        }
        tx.commit()?;
        Ok(())
    })
    .await
}

/// Roll complete 15-minute buckets into `usage_15m` and apply retention (called every 5 min).
pub async fn maintenance(db: &Db) -> Result<()> {
    let wm: i64 = db.get_setting("usage_rollup_watermark").await?.and_then(|v| v.parse().ok()).unwrap_or(0);
    let until = (now_ms() / B15) * B15;
    let from = if wm == 0 { until - RAW_DAYS * 86_400_000 } else { wm };
    if until > from {
        db.run(move |c| {
            c.execute(
                &format!(
                    "INSERT OR REPLACE INTO usage_15m (agent_id, kind, name, grp, ts, cpu, cpu_max, mem, mem_max) \
                     SELECT agent_id, kind, name, MAX(grp), (ts / {B15}) * {B15}, AVG(cpu), MAX(cpu), AVG(mem), MAX(mem) \
                     FROM usage_raw WHERE ts >= ?1 AND ts < ?2 GROUP BY agent_id, kind, name, (ts / {B15})"
                ),
                params![from, until],
            )?;
            Ok(())
        })
        .await?;
        db.set_setting("usage_rollup_watermark", until.to_string()).await?;
    }
    let raw_cut = now_ms() - RAW_DAYS * 86_400_000;
    let cut = now_ms() - ROLLUP_DAYS * 86_400_000;
    db.run(move |c| {
        c.execute("DELETE FROM usage_raw WHERE ts < ?1", [raw_cut])?;
        c.execute("DELETE FROM usage_15m WHERE ts < ?1", [cut])?;
        Ok(())
    })
    .await
}

/// (lookback ms, use rollup table, bucket ms)
fn plan(range: &str) -> (i64, bool, i64) {
    const M: i64 = 60_000;
    match range {
        "1h" => (60 * M, false, 30_000),
        "6h" => (6 * 60 * M, false, 2 * M),
        "24h" => (24 * 60 * M, false, 5 * M),
        "3d" => (3 * 24 * 60 * M, false, 15 * M),
        "7d" => (7 * 24 * 60 * M, true, 60 * M),
        "30d" => (30 * 24 * 60 * M, true, 4 * 60 * M),
        _ => (6 * 60 * M, false, 2 * M),
    }
}

#[derive(Deserialize)]
pub struct HistQuery {
    /// container | pod
    kind: String,
    /// Exact container name / `namespace/pod`.
    #[serde(default)]
    name: Option<String>,
    /// Sum over a group: compose project / `namespace/workload` / `namespace/` (whole namespace).
    #[serde(default)]
    group: Option<String>,
    #[serde(default)]
    range: Option<String>,
}

fn kind_of(k: &str) -> ApiResult<i64> {
    match k {
        "container" => Ok(KIND_CONTAINER),
        "pod" => Ok(KIND_POD),
        _ => Err(ApiError::bad("kind must be container or pod")),
    }
}

fn need(user: &AuthUser, id: &str, kind: i64) -> ApiResult<()> {
    user.require(if kind == KIND_POD { rbac::K8S_VIEW } else { rbac::DOCKER_VIEW })?;
    if !user.can_server(id) {
        return Err(ApiError::not_found("Server not found"));
    }
    Ok(())
}

/// Series for one container / pod, or the sum over a group, ready for uPlot.
pub async fn history(State(state): State<S>, user: AuthUser, Path(id): Path<String>, Query(q): Query<HistQuery>) -> ApiResult<Json<Value>> {
    let kind = kind_of(&q.kind)?;
    need(&user, &id, kind)?;
    let range = q.range.clone().unwrap_or_else(|| "6h".into());
    let (lookback, rollup, bucket) = plan(&range);
    let since = now_ms() - lookback;
    let (filter, key) = match (&q.name, &q.group) {
        (Some(n), _) => ("name = ?3", n.clone()),
        (None, Some(g)) if g.ends_with('/') => ("grp LIKE ?3 ESCAPE '\\'", format!("{}%", g.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"))),
        (None, Some(g)) => ("grp = ?3", g.clone()),
        _ => return Err(ApiError::bad("name or group is required")),
    };
    let r2 = range.clone();
    let out = state
        .db
        .run(move |c| {
            // Sum the members at each sample time first, then average per bucket.
            let sql = if rollup {
                format!(
                    "SELECT (ts / ?4) * ?4 AS t, AVG(sc), MAX(scm), AVG(sm), MAX(smm) FROM \
                     (SELECT ts, SUM(cpu) sc, SUM(cpu_max) scm, SUM(mem) sm, SUM(mem_max) smm FROM usage_15m \
                      WHERE agent_id = ?1 AND kind = ?2 AND {filter} AND ts >= ?5 GROUP BY ts) GROUP BY t ORDER BY t"
                )
            } else {
                format!(
                    "SELECT (ts / ?4) * ?4 AS t, AVG(sc), MAX(sc), AVG(sm), MAX(sm) FROM \
                     (SELECT ts, SUM(cpu) sc, SUM(mem) sm FROM usage_raw \
                      WHERE agent_id = ?1 AND kind = ?2 AND {filter} AND ts >= ?5 GROUP BY ts) GROUP BY t ORDER BY t"
                )
            };
            let mut st = c.prepare(&sql)?;
            let mut ts = Vec::new();
            let (mut cpu, mut cpu_max, mut mem, mut mem_max) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
            let gap = bucket.max(if rollup { B15 } else { 60_000 }) * 5 / 2;
            let mut last: Option<i64> = None;
            let mut rows = st.query(params![id, kind, key, bucket, since])?;
            while let Some(r) = rows.next()? {
                let t: i64 = r.get(0)?;
                if let Some(l) = last {
                    if t - l > gap {
                        ts.push(json!((l + bucket) / 1000));
                        for v in [&mut cpu, &mut cpu_max, &mut mem, &mut mem_max] {
                            v.push(Value::Null);
                        }
                    }
                }
                last = Some(t);
                ts.push(json!(t / 1000));
                let round = |x: Option<f64>| x.map(|v| json!((v * 100.0).round() / 100.0)).unwrap_or(Value::Null);
                cpu.push(round(r.get(1)?));
                cpu_max.push(round(r.get(2)?));
                mem.push(round(r.get(3)?));
                mem_max.push(round(r.get(4)?));
            }
            Ok(json!({ "range": r2, "bucket_ms": bucket, "ts": ts, "series": { "cpu": cpu, "cpu_max": cpu_max, "mem": mem, "mem_max": mem_max } }))
        })
        .await?;
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct TopQuery {
    kind: String,
    #[serde(default)]
    range: Option<String>,
    /// Aggregate by `name` (default) or `group`.
    #[serde(default)]
    by: Option<String>,
}

/// Average / peak usage per container (or pod, project, workload) over a range: "who used the most".
pub async fn top(State(state): State<S>, user: AuthUser, Path(id): Path<String>, Query(q): Query<TopQuery>) -> ApiResult<Json<Value>> {
    let kind = kind_of(&q.kind)?;
    need(&user, &id, kind)?;
    let range = q.range.clone().unwrap_or_else(|| "24h".into());
    let (lookback, rollup, _) = plan(&range);
    let since = now_ms() - lookback;
    let by_group = q.by.as_deref() == Some("group");
    let rows = state
        .db
        .run(move |c| {
            let (table, cmax, mmax) = if rollup { ("usage_15m", "cpu_max", "mem_max") } else { ("usage_raw", "cpu", "mem") };
            let sql = if by_group {
                format!(
                    "SELECT grp, AVG(sc), MAX(scm), AVG(sm), MAX(smm), COUNT(DISTINCT n) FROM \
                     (SELECT grp, ts, SUM(cpu) sc, SUM({cmax}) scm, SUM(mem) sm, SUM({mmax}) smm, MAX(name) n FROM {table} \
                      WHERE agent_id = ?1 AND kind = ?2 AND ts >= ?3 GROUP BY grp, ts) GROUP BY grp ORDER BY AVG(sc) DESC LIMIT 500"
                )
            } else {
                format!(
                    "SELECT name, AVG(cpu), MAX({cmax}), AVG(mem), MAX({mmax}), MAX(grp) FROM {table} \
                     WHERE agent_id = ?1 AND kind = ?2 AND ts >= ?3 GROUP BY name ORDER BY AVG(cpu) DESC LIMIT 500"
                )
            };
            let mut st = c.prepare(&sql)?;
            let v = st
                .query_map(params![id, kind, since], |r| {
                    Ok(json!({
                        "name": r.get::<_, String>(0)?,
                        "cpu_avg": r.get::<_, Option<f64>>(1)?,
                        "cpu_max": r.get::<_, Option<f64>>(2)?,
                        "mem_avg": r.get::<_, Option<f64>>(3)?,
                        "mem_max": r.get::<_, Option<f64>>(4)?,
                        "group": if by_group { Value::Null } else { json!(r.get::<_, String>(5)?) },
                    }))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(v)
        })
        .await?;
    Ok(Json(json!({ "range": range, "items": rows })))
}
