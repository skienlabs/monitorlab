import { FormEvent, useState } from "react";
import { Link, useNavigate, useParams } from "react-router-dom";
import { ChevronRight, Globe2, Lock, MoreVertical, Pause, Pencil, Play, Plus, RotateCcw, Trash2, Zap } from "lucide-react";
import { api } from "../api";
import { useAuth } from "../auth";
import { Dialog, Empty, ErrorBanner, Loading, Menu, Seg, Spinner, useConfirm, usePoll, useToast, Sorted } from "../components/ui";
import { TimeChart } from "../components/Chart";
import { ago, dateTime } from "../format";
import { ChartCard } from "./ServerPage";

export interface Monitor {
  id: number;
  name: string;
  kind: "http" | "tcp";
  target: string;
  method: string;
  expect_status: string;
  keyword: string;
  interval_secs: number;
  timeout_secs: number;
  fail_threshold: number;
  ssl_warn_days: number;
  ignore_tls: boolean;
  slow_ms: number;
  enabled: boolean;
  last_check: number | null;
  last_ms: number | null;
  last_error: string | null;
  last_status: number | null;
  cert_expires: number | null;
  cert_issuer: string | null;
  cert_subject: string | null;
  state: "up" | "down" | "degraded" | "pending" | "paused";
  uptime_24h: number | null;
  uptime_7d: number | null;
  uptime_30d: number | null;
  avg_ms_24h: number | null;
  recent: [number, boolean, number | null][];
}

const STATE: Record<Monitor["state"], [string, string]> = {
  up: ["good", "Up"],
  down: ["crit", "Down"],
  degraded: ["warn", "Failing"],
  pending: ["", "Waiting"],
  paused: ["", "Paused"],
};

const upPct = (v: number | null) => (v == null ? "—" : v >= 99.995 ? "100%" : `${v.toFixed(v >= 99 ? 2 : 1)}%`);
const certDays = (exp: number | null) => (exp == null ? null : Math.floor((exp - Date.now()) / 86_400_000));

function CertChip({ m }: { m: Pick<Monitor, "cert_expires" | "ssl_warn_days" | "cert_issuer"> }) {
  const d = certDays(m.cert_expires);
  if (d == null) return <span className="faint">—</span>;
  const cls = d < 7 ? "crit" : d < m.ssl_warn_days ? "warn" : "good";
  return (
    <span className={`chip ${cls}`} title={`${m.cert_issuer ?? ""} · expires ${dateTime(m.cert_expires)}`}>
      <Lock size={12} /> {d < 0 ? "expired" : `${d} days`}
    </span>
  );
}

/** Last 60 checks as a strip of bars (height = response time, red = failed). */
function Beats({ recent }: { recent: Monitor["recent"] }) {
  const max = Math.max(1, ...recent.map((r) => r[2] ?? 0));
  return (
    <div className="beats" aria-label="Recent checks">
      {Array.from({ length: Math.max(0, 60 - recent.length) }).map((_, i) => <i key={`e${i}`} className="empty" />)}
      {recent.map(([t, ok, ms]) => (
        <i key={t} className={ok ? "ok" : "bad"} style={{ height: ok ? `${30 + ((ms ?? 0) / max) * 70}%` : "100%" }} title={`${dateTime(t)} · ${ok ? `${ms} ms` : "failed"}`} />
      ))}
    </div>
  );
}


interface Form {
  name: string;
  kind: "http" | "tcp";
  target: string;
  method: string;
  expect_status: string;
  keyword: string;
  interval_secs: number;
  timeout_secs: number;
  fail_threshold: number;
  ssl_warn_days: number;
  ignore_tls: boolean;
  slow_ms: number;
  enabled: boolean;
}

const EMPTY: Form = { name: "", kind: "http", target: "https://", method: "GET", expect_status: "200-399", keyword: "", interval_secs: 60, timeout_secs: 10, fail_threshold: 2, ssl_warn_days: 14, ignore_tls: false, slow_ms: 0, enabled: true };

export function MonitorDialog({ m, onClose, onSaved }: { m: Monitor | null; onClose: () => void; onSaved: () => void }) {
  const toast = useToast();
  const [f, setF] = useState<Form>(m ? { name: m.name, kind: m.kind, target: m.target, method: m.method, expect_status: m.expect_status, keyword: m.keyword, interval_secs: m.interval_secs, timeout_secs: m.timeout_secs, fail_threshold: m.fail_threshold, ssl_warn_days: m.ssl_warn_days, ignore_tls: m.ignore_tls, slow_ms: m.slow_ms, enabled: m.enabled } : EMPTY);
  const [busy, setBusy] = useState(false);
  const [testing, setTesting] = useState(false);
  const [result, setResult] = useState<any>(null);
  const [err, setErr] = useState<string | null>(null);
  const set = (p: Partial<Form>) => setF((x) => ({ ...x, ...p }));
  const body = () => ({ ...f, interval_secs: Number(f.interval_secs), timeout_secs: Number(f.timeout_secs), fail_threshold: Number(f.fail_threshold), ssl_warn_days: Number(f.ssl_warn_days), slow_ms: Number(f.slow_ms) });
  const save = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setErr(null);
    try {
      if (m) await api(`/monitors/${m.id}`, { method: "PUT", body: body() });
      else await api("/monitors", { method: "POST", body: body() });
      toast(m ? "Monitor saved" : "Monitor added — first check within a few seconds");
      onSaved();
    } catch (e: any) {
      setErr(e.message);
      setBusy(false);
    }
  };
  const test = async () => {
    setTesting(true);
    setResult(null);
    setErr(null);
    try {
      setResult(await api("/monitors/test", { method: "POST", body: body() }));
    } catch (e: any) {
      setErr(e.message);
    } finally {
      setTesting(false);
    }
  };
  return (
    <Dialog
      title={m ? `Edit ${m.name}` : "New uptime monitor"}
      onClose={() => !busy && onClose()}
      wide
      footer={
        <>
          <button className="btn" type="button" onClick={test} disabled={testing || busy}>{testing ? <Spinner /> : <Zap size={16} />} Test now</button>
          <span className="spacer" />
          <button className="btn" type="button" onClick={onClose} disabled={busy}>Cancel</button>
          <button className="btn filled" form="monitor-form" disabled={busy}>{busy && <Spinner />} {m ? "Save" : "Add monitor"}</button>
        </>
      }
    >
      <form id="monitor-form" onSubmit={save} className="col" style={{ gap: 14 }}>
        <div className="form-grid">
          <div className="field">
            <label htmlFor="m-name">Name</label>
            <input id="m-name" className="input" required maxLength={100} value={f.name} placeholder="Website" onChange={(e) => set({ name: e.target.value })} autoFocus />
          </div>
          <div className="field">
            <label>Type</label>
            <Seg value={f.kind} onChange={(kind) => set({ kind, target: kind === "tcp" ? "" : "https://" })} options={[{ value: "http", label: "Website / API" }, { value: "tcp", label: "TCP port" }]} />
          </div>
          <div className="field" style={{ gridColumn: "1 / -1" }}>
            <label htmlFor="m-target">{f.kind === "http" ? "URL" : "Host:port"}</label>
            <input id="m-target" className="input mono" required value={f.target} placeholder={f.kind === "http" ? "https://example.com/health" : "db.internal:1433"} onChange={(e) => set({ target: e.target.value })} />
          </div>
          {f.kind === "http" && (
            <>
              <div className="field">
                <label htmlFor="m-method">Method</label>
                <select id="m-method" className="select" value={f.method} onChange={(e) => set({ method: e.target.value })}>
                  {["GET", "HEAD", "POST", "OPTIONS"].map((x) => <option key={x}>{x}</option>)}
                </select>
              </div>
              <div className="field">
                <label htmlFor="m-status">Expected status</label>
                <input id="m-status" className="input" value={f.expect_status} placeholder="200-399" onChange={(e) => set({ expect_status: e.target.value })} />
                <span className="hint">e.g. 200-399, 200,204 or 2xx</span>
              </div>
              <div className="field">
                <label htmlFor="m-kw">Must contain (optional)</label>
                <input id="m-kw" className="input" value={f.keyword} placeholder='e.g. "status":"ok"' onChange={(e) => set({ keyword: e.target.value })} />
              </div>
              <div className="field">
                <label htmlFor="m-slow">Warn when slower than (ms, 0 = off)</label>
                <input id="m-slow" className="input" type="number" min={0} value={f.slow_ms} onChange={(e) => set({ slow_ms: Number(e.target.value) })} />
              </div>
              <div className="field">
                <label htmlFor="m-ssl">Warn before the SSL certificate expires (days)</label>
                <input id="m-ssl" className="input" type="number" min={1} max={365} value={f.ssl_warn_days} onChange={(e) => set({ ssl_warn_days: Number(e.target.value) })} />
              </div>
              <label className="check" style={{ alignSelf: "end" }}>
                <input type="checkbox" checked={f.ignore_tls} onChange={(e) => set({ ignore_tls: e.target.checked })} /> Accept invalid / self-signed certificates
              </label>
            </>
          )}
          <div className="field">
            <label htmlFor="m-int">Check every</label>
            <select id="m-int" className="select" value={f.interval_secs} onChange={(e) => set({ interval_secs: Number(e.target.value) })}>
              {[[30, "30 seconds"], [60, "1 minute"], [120, "2 minutes"], [300, "5 minutes"], [900, "15 minutes"], [3600, "1 hour"]].map(([v, l]) => <option key={v} value={v}>{l}</option>)}
            </select>
          </div>
          <div className="field">
            <label htmlFor="m-to">Timeout (seconds)</label>
            <input id="m-to" className="input" type="number" min={1} max={60} value={f.timeout_secs} onChange={(e) => set({ timeout_secs: Number(e.target.value) })} />
          </div>
          <div className="field">
            <label htmlFor="m-ft">Alert after</label>
            <select id="m-ft" className="select" value={f.fail_threshold} onChange={(e) => set({ fail_threshold: Number(e.target.value) })}>
              {[1, 2, 3, 5].map((n) => <option key={n} value={n}>{n} failed check{n > 1 ? "s" : ""} in a row</option>)}
            </select>
          </div>
        </div>
        {result && (
          <div className={`banner ${result.ok ? "good" : "crit"} small`}>
            {result.ok ? `OK — ${result.status ? `HTTP ${result.status}, ` : ""}${result.ms} ms` : `Failed — ${result.error}`}
            {result.cert && <div>Certificate: {result.cert.subject} by {result.cert.issuer}, expires {dateTime(result.cert.expires)}{result.cert.problem ? ` · ${result.cert.problem}` : ""}</div>}
          </div>
        )}
        {err && <div className="banner crit small">{err}</div>}
      </form>
    </Dialog>
  );
}

export function UptimePage() {
  const { can } = useAuth();
  const nav = useNavigate();
  const confirm = useConfirm();
  const toast = useToast();
  const { data, error, reload } = usePoll(() => api<Monitor[]>("/monitors"), [], 15_000);
  const [edit, setEdit] = useState<Monitor | null | "new">(null);
  if (error && !data) return <div className="page"><ErrorBanner error={error} onRetry={reload} /></div>;
  if (!data) return <Loading />;
  const down = data.filter((m) => m.state === "down").length;
  const certSoon = data.filter((m) => { const d = certDays(m.cert_expires); return d != null && d < m.ssl_warn_days; }).length;
  return (
    <div className="page">
      <div className="page-head">
        <div className="grow">
          <h1>Uptime</h1>
          <div className="sub">Websites, APIs and ports checked from the hub — response time, uptime and SSL certificate expiry.</div>
        </div>
        {can("alerts.manage") && <button className="btn filled" onClick={() => setEdit("new")}><Plus size={18} /> Add monitor</button>}
      </div>
      {data.length > 0 && (
        <div className="grid grid-4" style={{ marginBottom: 16 }}>
          <div className="card kpi" data-accent="net"><div className="label"><Globe2 size={16} /> Monitors</div><div className="value">{data.length}</div></div>
          <div className="card kpi" data-accent={down ? "crit" : "good"}><div className="label">Down</div><div className="value">{down}</div></div>
          <div className="card kpi" data-accent={certSoon ? "warn" : "good"}><div className="label"><Lock size={16} /> Certificates expiring</div><div className="value">{certSoon}</div></div>
          <div className="card kpi" data-accent="cpu"><div className="label">Average uptime (30 days)</div><div className="value">{upPct(data.reduce((a, m) => a + (m.uptime_30d ?? 100), 0) / data.length)}</div></div>
        </div>
      )}
      {data.length === 0 ? (
        <div className="card">
          <Empty icon={<Globe2 size={28} />} title="No monitors yet">
            Add a website, API health endpoint or TCP port. You'll be alerted when it goes down, gets slow, or its SSL certificate is about to expire.
          </Empty>
        </div>
      ) : (
        <div className="card table-wrap">
          <Sorted rows={data} acc={{ name: m => m.name, state: m => m.state, ms: m => m.last_ms, d1: m => m.uptime_24h, d30: m => m.uptime_30d }}>{(S) => (
<table className="table">
            <thead>
              <tr>{S.th("name", "Monitor")}{S.th("state", "Status")}<th>Last 60 checks</th>{S.th("ms", "Response", "num")}{S.th("d1", "24 h", "num")}{S.th("d30", "30 days", "num")}<th>SSL</th><th /></tr>
            </thead>
            <tbody>
              {S.rows.map((m) => (
                <tr key={m.id} style={{ cursor: "pointer" }} onClick={() => nav(`/uptime/${m.id}`)}>
                  <td style={{ maxWidth: 320 }}>
                    <Link to={`/uptime/${m.id}`} style={{ fontWeight: 600, color: "var(--text)" }} onClick={(e) => e.stopPropagation()}>{m.name}</Link>
                    <div className="small faint mono ellipsis">{m.target}</div>
                  </td>
                  <td>
                    <span className={`chip ${STATE[m.state][0]}`}>{STATE[m.state][1]}</span>
                    {m.state !== "up" && m.last_error && <div className="small" style={{ color: "var(--crit-text)", maxWidth: 220 }}>{m.last_error}</div>}
                  </td>
                  <td><Beats recent={m.recent} /></td>
                  <td className="num tnum small">{m.last_ms != null ? `${m.last_ms} ms` : "—"}{m.avg_ms_24h != null && <div className="faint">avg {m.avg_ms_24h} ms</div>}</td>
                  <td className="num tnum small">{upPct(m.uptime_24h)}</td>
                  <td className="num tnum small">{upPct(m.uptime_30d)}</td>
                  <td>{m.kind === "http" && m.target.startsWith("https") ? <CertChip m={m} /> : <span className="faint">—</span>}</td>
                  <td className="actions" onClick={(e) => e.stopPropagation()}>
                    {can("alerts.manage") && (
                      <Menu trigger={<MoreVertical size={17} />}>
                        <button onClick={() => api(`/monitors/${m.id}/check`, { method: "POST" }).then(() => { toast("Checked"); reload(); }).catch((e) => toast(e.message, true))}><RotateCcw size={17} /> Check now</button>
                        <button onClick={() => setEdit(m)}><Pencil size={17} /> Edit</button>
                        <button onClick={() => api(`/monitors/${m.id}`, { method: "PUT", body: { ...m, enabled: !m.enabled } }).then(reload).catch((e) => toast(e.message, true))}>
                          {m.enabled ? <Pause size={17} /> : <Play size={17} />} {m.enabled ? "Pause" : "Resume"}
                        </button>
                        <hr />
                        <button className="danger" onClick={() => confirm({ title: `Delete ${m.name}?`, body: "Its check history is deleted too.", confirm: "Delete", danger: true, action: () => api(`/monitors/${m.id}`, { method: "DELETE" }).then(reload), success: "Monitor deleted" })}>
                          <Trash2 size={17} /> Delete
                        </button>
                      </Menu>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
)}</Sorted>
        </div>
      )}
      {edit && <MonitorDialog m={edit === "new" ? null : edit} onClose={() => setEdit(null)} onSaved={() => { setEdit(null); reload(); }} />}
    </div>
  );
}

interface Results {
  ts: number[];
  series: { ms: (number | null)[]; up: (number | null)[] };
  incidents: { start: number; end: number | null; error: string; checks: number }[];
}

export function MonitorPage() {
  const { mid = "" } = useParams();
  const { can } = useAuth();
  const toast = useToast();
  const [range, setRange] = useState<"1h" | "24h" | "7d" | "30d">("24h");
  const [editing, setEditing] = useState(false);
  const list = usePoll(() => api<Monitor[]>("/monitors"), [], 15_000);
  const res = usePoll(() => api<Results>(`/monitors/${mid}/results?range=${range}`), [mid, range], 30_000);
  const m = list.data?.find((x) => String(x.id) === mid);
  if (list.error && !list.data) return <div className="page"><ErrorBanner error={list.error} onRetry={list.reload} /></div>;
  if (!list.data) return <Loading />;
  if (!m) return <div className="page"><Empty icon={<Globe2 size={26} />} title="Monitor not found"><Link to="/uptime">Back to uptime</Link></Empty></div>;
  const d = certDays(m.cert_expires);
  return (
    <div className="page">
      <div className="page-head">
        <div className="grow">
          <div className="crumbs"><Link to="/uptime">Uptime</Link><ChevronRight size={14} /><span>{m.name}</span></div>
          <div className="row wrap" style={{ gap: 12 }}>
            <h1>{m.name}</h1>
            <span className={`chip ${STATE[m.state][0]}`}>{STATE[m.state][1]}</span>
          </div>
          <div className="sub mono small">{m.kind === "http" ? `${m.method} ${m.target}` : `TCP ${m.target}`} · every {m.interval_secs >= 60 ? `${m.interval_secs / 60} min` : `${m.interval_secs} s`}</div>
        </div>
        {can("alerts.manage") && (
          <div className="row" style={{ gap: 8 }}>
            <button className="btn" onClick={() => api(`/monitors/${m.id}/check`, { method: "POST" }).then(() => { toast("Checked"); list.reload(); res.reload(); }).catch((e) => toast(e.message, true))}><RotateCcw size={16} /> Check now</button>
            <button className="btn tonal" onClick={() => setEditing(true)}><Pencil size={16} /> Edit</button>
          </div>
        )}
      </div>
      {m.state !== "up" && m.last_error && <div className="banner crit" style={{ marginBottom: 16 }}>{m.last_error} <span className="faint">· checked {ago(m.last_check)}</span></div>}
      <div className="grid grid-4" style={{ marginBottom: 16 }}>
        <div className="card kpi" data-accent="cpu"><div className="label">Response time</div><div className="value">{m.last_ms ?? "—"}<span className="unit"> ms</span></div><div className="foot">24 h average {m.avg_ms_24h ?? "—"} ms</div></div>
        <div className="card kpi" data-accent="good"><div className="label">Uptime 24 h</div><div className="value">{upPct(m.uptime_24h)}</div><div className="foot">7 days {upPct(m.uptime_7d)}</div></div>
        <div className="card kpi" data-accent="mem"><div className="label">Uptime 30 days</div><div className="value">{upPct(m.uptime_30d)}</div><div className="foot">{res.data ? `${res.data.incidents.length} outage${res.data.incidents.length === 1 ? "" : "s"}` : " "}</div></div>
        <div className="card kpi" data-accent={d == null ? "net" : d < 7 ? "crit" : d < m.ssl_warn_days ? "warn" : "net"}>
          <div className="label"><Lock size={16} /> SSL certificate</div>
          <div className="value">{d == null ? "—" : d < 0 ? "Expired" : d}<span className="unit">{d != null && d >= 0 ? " days left" : ""}</span></div>
          <div className="foot ellipsis" title={m.cert_issuer ?? ""}>{m.cert_expires ? `${m.cert_subject} · ${m.cert_issuer}` : m.kind === "http" ? "Not HTTPS / not read yet" : "Not applicable"}</div>
        </div>
      </div>
      <div className="row" style={{ marginBottom: 12 }}>
        <h2 className="grow" style={{ margin: 0 }}>History</h2>
        <Seg value={range} onChange={setRange} options={(["1h", "24h", "7d", "30d"] as const).map((r) => ({ value: r, label: r }))} />
      </div>
      <div className="grid grid-2" style={{ marginBottom: 16 }}>
        <ChartCard title="Response time" accent="cpu">
          <TimeChart ts={res.data?.ts ?? []} fmt={(v) => `${Math.round(v)} ms`} series={[{ label: "Response", values: res.data?.series.ms ?? [], color: "--accent-cpu" }]} />
        </ChartCard>
        <ChartCard title="Availability" accent="good">
          <TimeChart ts={res.data?.ts ?? []} fmt={(v) => `${v.toFixed(0)}%`} yMax={100} series={[{ label: "Up", values: res.data?.series.up ?? [], color: "--good" }]} />
        </ChartCard>
      </div>
      <div className="card">
        <div className="card-head"><h3>Outages (last 30 days)</h3></div>
        <div className="card-body">
          {!res.data?.incidents.length ? (
            <div className="small muted">No outages recorded.</div>
          ) : (
            <table className="table">
              <thead><tr><th>Started</th><th>Duration</th><th>Failed checks</th><th>Error</th></tr></thead>
              <tbody>
                {res.data.incidents.map((i) => (
                  <tr key={i.start}>
                    <td className="small nowrap">{dateTime(i.start)}</td>
                    <td className="small">{i.end ? fmtDur(i.end - i.start) : <span className="chip crit">ongoing</span>}</td>
                    <td className="small tnum">{i.checks}</td>
                    <td className="small">{i.error}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>
      </div>
      {editing && <MonitorDialog m={m} onClose={() => setEditing(false)} onSaved={() => { setEditing(false); list.reload(); }} />}
    </div>
  );
}

function fmtDur(ms: number) {
  const s = Math.round(ms / 1000);
  if (s < 90) return `${s} s`;
  const m = Math.round(s / 60);
  if (m < 90) return `${m} min`;
  return `${(m / 60).toFixed(1)} h`;
}
