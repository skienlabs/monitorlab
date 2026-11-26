import { useState } from "react";
import { Ban, Database, PlugZap, Unplug } from "lucide-react";
import { rpc } from "../api";
import { useAuth } from "../auth";
import type { Server } from "../types";
import { Empty, ErrorBanner, Loading, Meter, SearchBox, Seg, Sorted, useConfirm, usePoll } from "../components/ui";
import { Toolbar } from "./DockerTabs";
import { bytes, duration, plural } from "../format";

interface Db {
  name: string;
  size: number;
  connections: number;
  commits: number;
  rollbacks: number;
  cache_hit: number | null;
  deadlocks: number;
  temp_bytes: number;
}
interface Repl {
  client: string;
  name: string;
  state: string;
  sync: string;
  lag_bytes: number;
  lag_s: number;
}
interface Overview {
  version: string;
  short_version: string;
  uptime_s: number;
  max_connections: number;
  replica: boolean;
  user: string;
  longest_xact_s: number;
  replay_lag_s: number;
  connections: { total: number; active: number; idle: number; idle_in_transaction: number; blocked: number };
  databases: Db[];
  replication: Repl[];
}
interface Session {
  pid: number;
  database: string;
  user: string;
  app: string;
  client: string;
  state: string;
  wait_type: string;
  wait: string;
  query_s: number;
  xact_s: number;
  query: string;
  blocked_by: number;
}

export const PG_CONFIG_SNIPPET = `# agent.toml
[postgres.connection]
host = "127.0.0.1"
port = 5432
user = "monitorlab"
password = "…"
database = "postgres"
sslmode = "disable"   # or "require"

-- on PostgreSQL (read-only monitoring role):
-- CREATE ROLE monitorlab LOGIN PASSWORD '…';
-- GRANT pg_monitor TO monitorlab;`;

export function PostgresTab({ s }: { s: Server }) {
  const [view, setView] = useState<"overview" | "activity">("overview");
  if (!s.online) return <Empty icon={<Database size={26} />} title="Server is offline" />;
  return (
    <div>
      <div style={{ marginBottom: 16 }}>
        <Seg value={view} onChange={setView} options={[{ value: "overview", label: "Overview" }, { value: "activity", label: "Sessions" }]} />
      </div>
      {view === "overview" ? <PgOverview s={s} /> : <PgActivity s={s} />}
    </div>
  );
}

function Stat({ label, value, sub, warn }: { label: string; value: React.ReactNode; sub?: React.ReactNode; warn?: boolean }) {
  return (
    <div className="card card-pad col" style={{ gap: 4 }}>
      <div className="small muted">{label}</div>
      <div style={{ fontSize: 24, fontWeight: 600, color: warn ? "var(--crit-text)" : undefined }}>{value}</div>
      {sub && <div className="small faint">{sub}</div>}
    </div>
  );
}

function PgOverview({ s }: { s: Server }) {
  const { data, error, loading, reload } = usePoll(() => rpc<Overview>(s.id, { op: "pg_overview" }), [s.id], 10_000);
  if (!data && !error) return <Loading />;
  if (!data) return <ErrorBanner error={error!} onRetry={reload} />;
  const c = data.connections;
  const usedPct = data.max_connections ? (c.total / data.max_connections) * 100 : 0;
  return (
    <div className="col" style={{ gap: 16 }}>
      <Toolbar onRefresh={reload} loading={loading}>
        <span className="chip good">{data.replica ? "replica" : "primary"}</span>
        <span className="small muted">{data.version.split(" on ")[0]} · up {duration(data.uptime_s)} · role {data.user}</span>
      </Toolbar>
      {error && <ErrorBanner error={error} onRetry={reload} />}
      <div className="grid grid-4">
        <Stat label="Connections" value={`${c.total} / ${data.max_connections}`} sub={<Meter value={usedPct} />} warn={usedPct >= 90} />
        <Stat label="Active queries" value={c.active} sub={`${c.idle} idle · ${c.idle_in_transaction} idle in transaction`} />
        <Stat label="Waiting on locks" value={c.blocked} sub={c.blocked ? "see Sessions" : "no lock waits"} warn={c.blocked > 0} />
        <Stat label="Longest transaction" value={duration(data.longest_xact_s)} sub={data.replica ? `replica lag ${duration(data.replay_lag_s)}` : "running now"} warn={data.longest_xact_s > 600} />
      </div>
      <div className="card">
        <div className="card-head"><h3>Databases</h3></div>
        <div className="table-wrap">
          <Sorted
            rows={data.databases}
            acc={{ name: (d) => d.name, size: (d) => d.size, conns: (d) => d.connections, commits: (d) => d.commits, rollbacks: (d) => d.rollbacks, cache: (d) => d.cache_hit, deadlocks: (d) => d.deadlocks, temp: (d) => d.temp_bytes }}
            initial={{ key: "size", dir: "desc" }}
          >
            {(S) => (
              <table className="table">
                <thead>
                  <tr>
                    {S.th("name", "Database")}
                    {S.th("size", "Size", "num")}
                    {S.th("conns", "Connections", "num")}
                    {S.th("commits", "Commits", "num")}
                    {S.th("rollbacks", "Rollbacks", "num")}
                    {S.th("cache", "Cache hit", "num")}
                    {S.th("deadlocks", "Deadlocks", "num")}
                    {S.th("temp", "Temp files", "num")}
                  </tr>
                </thead>
                <tbody>
                  {S.rows.map((d) => (
                    <tr key={d.name}>
                      <td className="mono" style={{ fontWeight: 500 }}>{d.name}</td>
                      <td className="num tnum">{bytes(d.size)}</td>
                      <td className="num tnum">{d.connections}</td>
                      <td className="num tnum small">{d.commits.toLocaleString()}</td>
                      <td className="num tnum small">{d.rollbacks.toLocaleString()}</td>
                      <td className="num tnum small">{d.cache_hit == null ? "—" : `${d.cache_hit.toFixed(1)}%`}</td>
                      <td className="num tnum small" style={{ color: d.deadlocks ? "var(--crit-text)" : undefined }}>{d.deadlocks}</td>
                      <td className="num tnum small">{bytes(d.temp_bytes)}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </Sorted>
        </div>
      </div>
      {data.replication.length > 0 && (
        <div className="card">
          <div className="card-head"><h3>Replication</h3></div>
          <div className="table-wrap">
            <Sorted rows={data.replication} acc={{ name: (r) => r.name, client: (r) => r.client, state: (r) => r.state, sync: (r) => r.sync, bytes: (r) => r.lag_bytes, secs: (r) => r.lag_s }}>
              {(S) => (
                <table className="table">
                  <thead>
                    <tr>
                      {S.th("name", "Standby")}
                      {S.th("client", "Address")}
                      {S.th("state", "State")}
                      {S.th("sync", "Mode")}
                      {S.th("bytes", "Behind (WAL)", "num")}
                      {S.th("secs", "Replay lag", "num")}
                    </tr>
                  </thead>
                  <tbody>
                    {S.rows.map((r, i) => (
                      <tr key={i}>
                        <td>{r.name || "—"}</td>
                        <td className="mono small">{r.client || "—"}</td>
                        <td><span className={`chip ${r.state === "streaming" ? "good" : "warn"}`}>{r.state}</span></td>
                        <td className="small">{r.sync}</td>
                        <td className="num tnum small">{bytes(r.lag_bytes)}</td>
                        <td className="num tnum small">{duration(r.lag_s)}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              )}
            </Sorted>
          </div>
        </div>
      )}
    </div>
  );
}

function PgActivity({ s }: { s: Server }) {
  const { can } = useAuth();
  const confirm = useConfirm();
  const { data, error, loading, reload } = usePoll(() => rpc<{ sessions: Session[] }>(s.id, { op: "pg_activity" }), [s.id], 5_000);
  const [q, setQ] = useState("");
  const [f, setF] = useState<"active" | "all" | "blocked">("active");
  if (!data && !error) return <Loading />;
  const all = data?.sessions ?? [];
  const needle = q.trim().toLowerCase();
  const list = all.filter(
    (x) =>
      (f === "all" || (f === "active" ? x.state === "active" || x.state.startsWith("idle in transaction") : x.blocked_by > 0 || x.wait_type === "Lock")) &&
      (!needle || [x.query, x.user, x.database, x.app, x.client].some((v) => v.toLowerCase().includes(needle))),
  );
  const act = (x: Session, kind: "cancel" | "terminate") =>
    confirm({
      title: kind === "cancel" ? `Cancel the query of session ${x.pid}?` : `Terminate session ${x.pid}?`,
      body: kind === "cancel" ? "The running query is stopped; the connection stays open." : "The connection is closed and any open transaction is rolled back.",
      confirm: kind === "cancel" ? "Cancel query" : "Terminate",
      danger: true,
      action: () => rpc(s.id, { op: "pg_backend", pid: x.pid, kind }).finally(reload),
      success: kind === "cancel" ? "Query cancelled" : "Session terminated",
    });
  return (
    <div>
      <Toolbar onRefresh={reload} loading={loading}>
        <SearchBox value={q} onChange={setQ} placeholder="Filter by query, user, database or app" />
        <Seg
          value={f}
          onChange={setF}
          options={[
            { value: "active", label: "Running" },
            { value: "blocked", label: "Waiting on locks" },
            { value: "all", label: `All ${all.length}` },
          ]}
        />
      </Toolbar>
      {error && <ErrorBanner error={error} onRetry={reload} />}
      <div className="card table-wrap">
        {list.length === 0 ? (
          <Empty icon={<Database size={26} />} title={f === "blocked" ? "Nothing is waiting on a lock" : "No sessions match"}>{plural(all.length, "session")} connected in total.</Empty>
        ) : (
          <Sorted
            rows={list}
            acc={{ pid: (x) => x.pid, db: (x) => x.database, user: (x) => x.user, state: (x) => x.state, dur: (x) => x.query_s, xact: (x) => x.xact_s, wait: (x) => x.wait }}
            initial={{ key: "dur", dir: "desc" }}
          >
            {(S) => (
              <table className="table">
                <thead>
                  <tr>
                    {S.th("pid", "PID", "num")}
                    {S.th("db", "Database")}
                    {S.th("user", "User")}
                    {S.th("state", "State")}
                    {S.th("dur", "Query time", "num")}
                    {S.th("xact", "Transaction", "num")}
                    {S.th("wait", "Waiting for")}
                    <th>Query</th>
                    <th />
                  </tr>
                </thead>
                <tbody>
                  {S.rows.map((x) => (
                    <tr key={x.pid}>
                      <td className="num tnum small">{x.pid}</td>
                      <td className="small">{x.database || "—"}</td>
                      <td className="small">
                        {x.user}
                        <div className="faint">{x.app || x.client}</div>
                      </td>
                      <td><span className={`chip ${x.state === "active" ? "good" : x.state.startsWith("idle in") ? "warn" : ""}`}>{x.state || "—"}</span></td>
                      <td className="num tnum small">{x.state === "active" ? duration(x.query_s) : "—"}</td>
                      <td className="num tnum small">{x.xact_s ? duration(x.xact_s) : "—"}</td>
                      <td className="small">
                        {x.wait ? `${x.wait_type}: ${x.wait}` : "—"}
                        {x.blocked_by > 0 && <div style={{ color: "var(--crit-text)" }}>blocked by {x.blocked_by}</div>}
                      </td>
                      <td style={{ maxWidth: 480 }}><div className="mono small ellipsis" title={x.query}>{x.query}</div></td>
                      <td className="actions">
                        {can("postgres.control") && (
                          <div className="row" style={{ gap: 0, justifyContent: "flex-end" }}>
                            <button className="icon-btn sm" title="Cancel query" aria-label="Cancel query" disabled={x.state !== "active"} onClick={() => act(x, "cancel")}><Ban size={17} /></button>
                            <button className="icon-btn sm danger" title="Terminate session" aria-label="Terminate session" onClick={() => act(x, "terminate")}><Unplug size={17} /></button>
                          </div>
                        )}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </Sorted>
        )}
      </div>
      <div className="small faint" style={{ marginTop: 10 }}><PlugZap size={13} style={{ verticalAlign: "-2px" }} /> Query text can contain data; only people with the “View PostgreSQL” permission see it.</div>
    </div>
  );
}
