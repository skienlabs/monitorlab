import { useEffect, useState } from "react";
import { Link } from "react-router-dom";
import { ScrollText } from "lucide-react";
import { api } from "../api";
import { Empty, ErrorBanner, Loading, SearchBox, Spinner } from "../components/ui";
import { dateTime } from "../format";

interface Entry {
  id: number;
  ts: number;
  user: string;
  ip: string;
  action: string;
  server_id: string | null;
  server: string | null;
  target: string | null;
  detail: string | null;
  ok: boolean;
}

const LABELS: Record<string, string> = {
  login: "Signed in",
  login_failed: "Failed sign-in",
  logout: "Signed out",
  password_changed: "Changed password",
  user_created: "Created user",
  user_updated: "Updated user",
  user_deleted: "Deleted user",
  role_created: "Created role",
  role_updated: "Updated role",
  role_deleted: "Deleted role",
  server_added: "Added server",
  server_removed: "Removed server",
  server_renamed: "Renamed server",
  server_token_created: "Issued install token",
  agent_enrolled: "Agent enrolled",
  container_exec: "Opened container terminal",
  docker_container_action: "Container action",
  docker_container_remove: "Deleted container",
  docker_image_remove: "Deleted image",
  docker_volume_remove: "Deleted volume",
  docker_network_remove: "Deleted network",
  docker_prune: "Docker cleanup",
  iis_site_action: "IIS site action",
  iis_app_pool_action: "IIS pool action",
};

function describe(e: Entry) {
  if (!e.detail) return e.target ?? "";
  try {
    const d = JSON.parse(e.detail.split(" — error:")[0]);
    const parts = [d.action, d.target, d.name, d.id].filter(Boolean).map(String);
    const err = e.detail.includes(" — error:") ? ` (error: ${e.detail.split(" — error:")[1].trim()})` : "";
    return parts.join(" · ") + err;
  } catch {
    return e.detail;
  }
}

export function AuditPage() {
  const [q, setQ] = useState("");
  const [rows, setRows] = useState<Entry[] | null>(null);
  const [more, setMore] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = async (reset: boolean) => {
    setBusy(true);
    try {
      const before = !reset && rows?.length ? `&before=${rows[rows.length - 1].id}` : "";
      const page = await api<Entry[]>(`/audit?limit=100${before}${q.trim().length > 1 ? `&q=${encodeURIComponent(q.trim())}` : ""}`);
      setRows(reset ? page : [...(rows ?? []), ...page]);
      setMore(page.length === 100);
      setError(null);
    } catch (e: any) {
      setError(e.message);
    } finally {
      setBusy(false);
    }
  };

  useEffect(() => {
    const t = setTimeout(() => load(true), 250);
    return () => clearTimeout(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [q]);

  return (
    <div className="page">
      <div className="page-head">
        <div className="grow">
          <h1>Audit log</h1>
          <div className="sub">Sign-ins, administration and every action that changed a server. Kept for one year.</div>
        </div>
      </div>
      <div className="row" style={{ marginBottom: 16 }}>
        <SearchBox value={q} onChange={setQ} placeholder="Search user, action, server or IP" />
      </div>
      {error && <ErrorBanner error={error} onRetry={() => load(true)} />}
      {!rows ? (
        <Loading />
      ) : rows.length === 0 ? (
        <div className="card"><Empty icon={<ScrollText size={26} />} title="No entries" /></div>
      ) : (
        <div className="card table-wrap">
          <table className="table">
            <thead><tr><th>When</th><th>Who</th><th>Action</th><th>Server</th><th>Details</th><th>IP</th></tr></thead>
            <tbody>
              {rows.map((e) => (
                <tr key={e.id}>
                  <td className="small nowrap tnum">{dateTime(e.ts)}</td>
                  <td className="small">{e.user || <span className="faint">system</span>}</td>
                  <td>
                    <span className={`chip ${e.ok ? "" : "crit"}`}>{LABELS[e.action] ?? e.action}</span>
                  </td>
                  <td className="small">{e.server_id && e.server ? <Link to={`/servers/${e.server_id}`}>{e.server}</Link> : "—"}</td>
                  <td className="small mono" style={{ maxWidth: 420, wordBreak: "break-word" }}>{describe(e)}</td>
                  <td className="small mono faint">{e.ip}</td>
                </tr>
              ))}
            </tbody>
          </table>
          {more && (
            <div className="row" style={{ justifyContent: "center", padding: 12 }}>
              <button className="btn" onClick={() => load(false)} disabled={busy}>{busy && <Spinner />} Load older entries</button>
            </div>
          )}
        </div>
      )}
    </div>
  );
}
