import { useState } from "react";
import { Link } from "react-router-dom";
import { BellOff, Check, Settings } from "lucide-react";
import { api } from "../api";
import { useAuth } from "../auth";
import { Empty, ErrorBanner, Loading, Seg, usePoll, useToast } from "../components/ui";
import { dateTime, duration } from "../format";

export interface Alert {
  id: number;
  server_id: string;
  server: string;
  rule: string;
  severity: "warning" | "critical";
  message: string;
  started_at: number;
  resolved_at: number | null;
  acked_by: string | null;
  acked_at: number | null;
}

export function SeverityChip({ a }: { a: Pick<Alert, "severity" | "resolved_at"> }) {
  if (a.resolved_at) return <span className="chip good"><span className="dot good" />resolved</span>;
  return (
    <span className={`chip ${a.severity === "critical" ? "crit" : "warn"}`}>
      <span className={`dot ${a.severity === "critical" ? "crit" : "warn"}`} />
      {a.severity}
    </span>
  );
}

export function AlertsTable({ rows, onAck, showServer = true }: { rows: Alert[]; onAck?: (a: Alert) => void; showServer?: boolean }) {
  return (
    <div className="table-wrap">
      <table className="table">
        <thead>
          <tr>
            <th>Severity</th>
            {showServer && <th>Server</th>}
            <th>Alert</th>
            <th>Started</th>
            <th>Duration</th>
            <th />
          </tr>
        </thead>
        <tbody>
          {rows.map((a) => (
            <tr key={a.id}>
              <td><SeverityChip a={a} /></td>
              {showServer && <td>{a.server ? <Link to={`/servers/${a.server_id}`}>{a.server}</Link> : <span className="faint">removed</span>}</td>}
              <td>
                <div>{a.message}</div>
                <div className="small faint mono">{a.rule}</div>
              </td>
              <td className="small nowrap">{dateTime(a.started_at)}</td>
              <td className="small nowrap tnum">{duration(((a.resolved_at ?? Date.now()) - a.started_at) / 1000)}</td>
              <td className="actions small">
                {a.acked_by ? (
                  <span className="faint" title={dateTime(a.acked_at)}>acknowledged by {a.acked_by}</span>
                ) : (
                  !a.resolved_at &&
                  onAck && (
                    <button className="btn sm tonal" onClick={() => onAck(a)}>
                      <Check size={15} /> Acknowledge
                    </button>
                  )
                )}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

export function AlertsPage() {
  const { can } = useAuth();
  const toast = useToast();
  const [view, setView] = useState<"active" | "all">("active");
  const { data, error, reload } = usePoll(() => api<Alert[]>(`/alerts?state=${view}`), [view], 15_000);
  const ack = async (a: Alert) => {
    try {
      await api(`/alerts/${a.id}/ack`, { method: "POST" });
      toast("Alert acknowledged");
      reload();
    } catch (e: any) {
      toast(e.message, true);
    }
  };
  return (
    <div className="page">
      <div className="page-head">
        <div className="grow">
          <h1>Alerts</h1>
          <div className="sub">Checked every 30 seconds. Notifications go to the channels configured in alert settings.</div>
        </div>
        {can("alerts.manage") && (
          <Link to="/admin/alerts" className="btn outlined">
            <Settings size={17} /> Alert settings
          </Link>
        )}
      </div>
      <div className="row" style={{ marginBottom: 16 }}>
        <Seg value={view} onChange={setView} options={[{ value: "active", label: "Active" }, { value: "all", label: "History" }]} />
      </div>
      {error && <ErrorBanner error={error} onRetry={reload} />}
      {!data ? (
        <Loading />
      ) : data.length === 0 ? (
        <div className="card">
          <Empty icon={<BellOff size={26} />} title={view === "active" ? "All clear" : "No alerts yet"}>
            {view === "active" ? "Nothing needs attention right now." : "Alerts appear here when thresholds are crossed or agents go offline."}
          </Empty>
        </div>
      ) : (
        <div className="card">
          <AlertsTable rows={data} onAck={can("alerts.manage") ? ack : undefined} />
        </div>
      )}
    </div>
  );
}
