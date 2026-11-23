import { useState } from "react";
import { Database } from "lucide-react";
import { rpc } from "../api";
import { useAuth } from "../auth";
import type { Server } from "../types";
import { CodeBlock, Empty, ErrorBanner, Loading, StateChip, usePoll } from "../components/ui";
import { LogViewer } from "../components/LogViewer";
import { bytes, pct } from "../format";

interface Instance {
  name: string;
  service: string;
  state: string;
  pid: number | null;
  cpu: number;
  mem: number;
  errorlog: string | null;
  version: string | null;
  edition: string | null;
}
interface Status {
  instances: Instance[];
  query_configured: boolean;
  query?: {
    error?: string;
    info?: Record<string, any>;
    databases?: { name: string; state: string; recovery_model: string; data_mb: number; log_mb: number }[];
    counters?: Record<string, number>;
  };
}

const CONFIG_SNIPPET = `# agent.toml — optional read-only DMV access
[mssql.connection]
host = "127.0.0.1"
port = 1433
user = "vigil_monitor"
password = "…"
trust_cert = true

-- on SQL Server:
-- CREATE LOGIN vigil_monitor WITH PASSWORD = '…';
-- GRANT VIEW SERVER STATE TO vigil_monitor;
-- GRANT VIEW ANY DEFINITION TO vigil_monitor;`;

export function MssqlTab({ s }: { s: Server }) {
  const { can } = useAuth();
  const { data, error, reload } = usePoll(() => rpc<Status>(s.id, { op: "mssql_status" }), [s.id], 15_000);
  const [inst, setInst] = useState<string | null>(null);
  if (!s.online) return <Empty icon={<Database size={26} />} title="Server is offline" />;
  if (!data && !error) return <Loading />;
  if (!data) return <ErrorBanner error={error!} onRetry={reload} />;
  const selected = inst ?? data.instances[0]?.name ?? null;
  const q = data.query;

  return (
    <div className="col" style={{ gap: 16 }}>
      <div className="grid grid-2">
        {data.instances.map((i) => (
          <div key={i.name} className="card card-pad col" style={{ gap: 12 }}>
            <div className="row">
              <Database size={20} />
              <h3 className="grow">{i.name === "MSSQLSERVER" ? "Default instance" : i.name}</h3>
              <StateChip state={i.state} />
            </div>
            <div className="small muted">{i.version ?? "Version unknown (error log not readable)"}</div>
            {i.edition && <div className="small faint">{i.edition}</div>}
            <div className="row" style={{ gap: 32 }}>
              <div>
                <div className="small muted">CPU</div>
                <div className="tnum" style={{ fontSize: 22 }}>{pct(i.cpu, 1)}</div>
              </div>
              <div>
                <div className="small muted">Memory</div>
                <div className="tnum" style={{ fontSize: 22 }}>{bytes(i.mem)}</div>
              </div>
              <div>
                <div className="small muted">Service</div>
                <div className="mono small" style={{ marginTop: 6 }}>{i.service}{i.pid ? ` · pid ${i.pid}` : ""}</div>
              </div>
            </div>
          </div>
        ))}
      </div>

      {q?.error && <div className="banner warn">Database queries failed: {q.error}</div>}
      {q?.info && (
        <div className="grid grid-4">
          <div className="card kpi"><div className="label">User sessions</div><div className="value">{q.info.user_sessions ?? "—"}</div></div>
          <div className="card kpi"><div className="label">Active requests</div><div className="value">{q.info.active_requests ?? "—"}</div></div>
          <div className="card kpi">
            <div className="label">Memory in use</div>
            <div className="value">{bytes((q.info.memory_in_use_kb ?? 0) * 1024)}</div>
            <div className="foot">target {bytes((q.info.target_memory_kb ?? 0) * 1024)}</div>
          </div>
          <div className="card kpi">
            <div className="label">Page life expectancy</div>
            <div className="value">{q.counters?.["Page life expectancy"] ?? "—"}<small> s</small></div>
            <div className="foot">Low values indicate memory pressure</div>
          </div>
        </div>
      )}
      {q?.databases && (
        <div className="card">
          <div className="card-head"><h3>Databases</h3></div>
          <div className="card-body table-wrap">
            <table className="table">
              <thead><tr><th>Name</th><th>State</th><th>Recovery</th><th className="num">Data</th><th className="num">Log</th></tr></thead>
              <tbody>
                {q.databases.map((d) => (
                  <tr key={d.name}>
                    <td className="mono small">{d.name}</td>
                    <td><StateChip state={d.state === "ONLINE" ? "running" : d.state.toLowerCase()} /></td>
                    <td className="small muted">{d.recovery_model}</td>
                    <td className="num">{bytes(d.data_mb * 1048576)}</td>
                    <td className="num">{bytes(d.log_mb * 1048576)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      )}
      {!data.query_configured && (
        <details className="card card-pad">
          <summary style={{ cursor: "pointer", fontWeight: 500 }}>Show databases, sessions and memory counters</summary>
          <p className="small muted">Optionally give the agent a read-only login. Without it, the agent still reports service state, CPU, memory and the error log.</p>
          <CodeBlock>{CONFIG_SNIPPET}</CodeBlock>
        </details>
      )}

      {can("logs.view") && selected && (
        <div className="col" style={{ gap: 10 }}>
          <div className="row">
            <h2 className="grow">Error log</h2>
            {data.instances.length > 1 && (
              <select className="select" style={{ width: 220, height: 36 }} value={selected} onChange={(e) => setInst(e.target.value)}>
                {data.instances.map((i) => <option key={i.name} value={i.name}>{i.name}</option>)}
              </select>
            )}
          </div>
          <LogViewer fileName={`errorlog-${selected}`} height="560px" source={{ serverId: s.id, request: { kind: "mssql_log_follow", instance: selected }, reconnect: false }} />
        </div>
      )}
    </div>
  );
}
