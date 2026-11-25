import { useEffect, useState } from "react";
import { BellRing, Mail, MessageSquare, Pencil, Plus, Send, Trash2, Webhook } from "lucide-react";
import { api } from "../api";
import { Dialog, ErrorBanner, Loading, Spinner, useConfirm, usePoll, useToast } from "../components/ui";
import { ago } from "../format";
import { useBrand } from "../brand";

interface Threshold {
  warn: number;
  crit: number;
  for_min: number;
}
interface Rules {
  enabled: boolean;
  cpu: Threshold;
  mem: Threshold;
  disk: Threshold;
  offline_min: number;
  containers_unhealthy: boolean;
  mssql_stopped: boolean;
  iis_stopped: boolean;
  k8s_problems: boolean;
}
interface Channel {
  id: number;
  name: string;
  kind: "email" | "teams" | "slack" | "webhook";
  url: string;
  enabled: boolean;
  min_severity: "warning" | "critical";
  last_error: string | null;
  last_sent_at: number | null;
}
interface Smtp {
  host: string;
  port: number;
  security: "starttls" | "tls" | "none";
  username: string;
  password_set: boolean;
  from_address: string;
  from_name: string;
  accept_invalid_certs: boolean;
}

const KIND_LABEL: Record<Channel["kind"], string> = { email: "Email", teams: "Microsoft Teams", slack: "Slack / Google Chat / Mattermost", webhook: "Generic webhook (JSON)" };
const KIND_ICON = { email: <Mail size={18} />, teams: <MessageSquare size={18} />, slack: <MessageSquare size={18} />, webhook: <Webhook size={18} /> };

function Num({ label, value, onChange, suffix, min = 0, max = 100 }: { label: string; value: number; onChange: (v: number) => void; suffix: string; min?: number; max?: number }) {
  return (
    <div className="field">
      <label>{label}</label>
      <div className="row">
        <input className="input tnum" type="number" min={min} max={max} value={value} onChange={(e) => onChange(Number(e.target.value))} style={{ width: 110 }} />
        <span className="small muted">{suffix}</span>
      </div>
    </div>
  );
}

function ThresholdRow({ title, t, onChange, durationNote }: { title: string; t: Threshold; onChange: (t: Threshold) => void; durationNote?: boolean }) {
  return (
    <div className="col" style={{ gap: 10 }}>
      <h3>{title}</h3>
      <div className="row wrap" style={{ gap: 20 }}>
        <Num label="Warning at" value={t.warn} onChange={(v) => onChange({ ...t, warn: v })} suffix="%" min={1} />
        <Num label="Critical at" value={t.crit} onChange={(v) => onChange({ ...t, crit: v })} suffix="%" min={1} />
        {durationNote !== false && <Num label="Sustained for" value={t.for_min} onChange={(v) => onChange({ ...t, for_min: v })} suffix="minutes (0 = immediately)" max={120} />}
      </div>
    </div>
  );
}

function RulesCard() {
  const toast = useToast();
  const { data, error, reload } = usePoll(() => api<Rules>("/alert-rules"), []);
  const [r, setR] = useState<Rules | null>(null);
  const [busy, setBusy] = useState(false);
  useEffect(() => setR(data), [data]);
  if (error) return <ErrorBanner error={error} onRetry={reload} />;
  if (!r) return <Loading />;
  const save = async () => {
    setBusy(true);
    try {
      await api("/alert-rules", { method: "PUT", body: r });
      toast("Alert rules saved");
    } catch (e: any) {
      toast(e.message, true);
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="card card-pad col" style={{ gap: 20 }}>
      <div className="row">
        <BellRing size={20} />
        <h2 className="grow">Rules</h2>
        <label className="check">
          <input type="checkbox" checked={r.enabled} onChange={(e) => setR({ ...r, enabled: e.target.checked })} /> Alerting enabled
        </label>
      </div>
      <ThresholdRow title="CPU" t={r.cpu} onChange={(cpu) => setR({ ...r, cpu })} />
      <ThresholdRow title="Memory" t={r.mem} onChange={(mem) => setR({ ...r, mem })} />
      <ThresholdRow title="Disk (any mounted volume)" t={r.disk} onChange={(disk) => setR({ ...r, disk })} durationNote={false} />
      <div className="col" style={{ gap: 10 }}>
        <h3>Availability</h3>
        <Num label="Agent offline after" value={r.offline_min} onChange={(v) => setR({ ...r, offline_min: v })} suffix="minutes without contact" min={1} max={1440} />
        <label className="check"><input type="checkbox" checked={r.containers_unhealthy} onChange={(e) => setR({ ...r, containers_unhealthy: e.target.checked })} /> Unhealthy or restart-looping Docker containers (warning)</label>
        <label className="check"><input type="checkbox" checked={r.mssql_stopped} onChange={(e) => setR({ ...r, mssql_stopped: e.target.checked })} /> SQL Server instance stopped (critical)</label>
        <label className="check"><input type="checkbox" checked={r.iis_stopped} onChange={(e) => setR({ ...r, iis_stopped: e.target.checked })} /> IIS site or application pool stopped (warning)</label>
        <label className="check"><input type="checkbox" checked={r.k8s_problems ?? true} onChange={(e) => setR({ ...r, k8s_problems: e.target.checked })} /> Kubernetes: node not ready (critical), failing pods or degraded workloads (warning)</label>
      </div>
      <div className="row" style={{ justifyContent: "flex-end" }}>
        <button className="btn filled" onClick={save} disabled={busy}>{busy && <Spinner />} Save rules</button>
      </div>
    </div>
  );
}

function SmtpCard() {
  const toast = useToast();
  const { name: brand } = useBrand();
  const { data, error, reload } = usePoll(() => api<Smtp>("/smtp"), []);
  const [s, setS] = useState<Smtp | null>(null);
  const [password, setPassword] = useState("");
  const [to, setTo] = useState("");
  const [busy, setBusy] = useState<"" | "save" | "test">("");
  useEffect(() => setS(data), [data]);
  if (error) return <ErrorBanner error={error} onRetry={reload} />;
  if (!s) return <Loading />;
  const save = async () => {
    setBusy("save");
    try {
      await api("/smtp", { method: "PUT", body: { ...s, password: password || undefined } });
      setPassword("");
      toast("Email server saved");
      reload();
    } catch (e: any) {
      toast(e.message, true);
    } finally {
      setBusy("");
    }
  };
  const test = async () => {
    setBusy("test");
    try {
      await api("/smtp/test", { method: "POST", body: { to } });
      toast(`Test email sent to ${to}`);
    } catch (e: any) {
      toast(e.message, true);
    } finally {
      setBusy("");
    }
  };
  const setSec = (security: Smtp["security"]) => setS({ ...s, security, port: security === "tls" ? 465 : security === "starttls" ? 587 : 25 });
  return (
    <div className="card card-pad col" style={{ gap: 16 }}>
      <div className="row">
        <Mail size={20} />
        <h2 className="grow">Email server (SMTP)</h2>
      </div>
      <div className="small muted" style={{ marginTop: -8 }}>Used by email channels. Use your own mail server, e.g. mail.yourdomain.com with a dedicated alerts@ mailbox.</div>
      <div className="grid grid-3">
        <div className="field" style={{ gridColumn: "span 2" }}>
          <label>Host</label>
          <input className="input" placeholder="mail.example.com" value={s.host} onChange={(e) => setS({ ...s, host: e.target.value })} />
        </div>
        <div className="field">
          <label>Port</label>
          <input className="input tnum" type="number" value={s.port} onChange={(e) => setS({ ...s, port: Number(e.target.value) })} />
        </div>
        <div className="field">
          <label>Security</label>
          <select className="select" value={s.security} onChange={(e) => setSec(e.target.value as Smtp["security"])}>
            <option value="starttls">STARTTLS (587)</option>
            <option value="tls">TLS / SSL (465)</option>
            <option value="none">None (25, internal relay)</option>
          </select>
        </div>
        <div className="field">
          <label>Username</label>
          <input className="input" autoComplete="off" value={s.username} onChange={(e) => setS({ ...s, username: e.target.value })} placeholder="alerts@example.com" />
        </div>
        <div className="field">
          <label>Password</label>
          <input className="input" type="password" autoComplete="new-password" value={password} onChange={(e) => setPassword(e.target.value)} placeholder={s.password_set ? "•••••••• (unchanged)" : ""} />
        </div>
        <div className="field">
          <label>From address</label>
          <input className="input" value={s.from_address} onChange={(e) => setS({ ...s, from_address: e.target.value })} placeholder="alerts@example.com" />
        </div>
        <div className="field">
          <label>From name</label>
          <input className="input" value={s.from_name} onChange={(e) => setS({ ...s, from_name: e.target.value })} placeholder={`${brand} Alerts`} />
        </div>
      </div>
      <label className="check">
        <input type="checkbox" checked={s.accept_invalid_certs} onChange={(e) => setS({ ...s, accept_invalid_certs: e.target.checked })} />
        Accept self-signed / internal certificates on the mail server
      </label>
      <div className="row wrap" style={{ gap: 12 }}>
        <input className="input" style={{ maxWidth: 300 }} placeholder="Send a test to…" value={to} onChange={(e) => setTo(e.target.value)} />
        <button className="btn outlined" onClick={test} disabled={!to || !!busy || !data?.host}>
          {busy === "test" ? <Spinner /> : <Send size={16} />} Send test
        </button>
        <span className="spacer" />
        <button className="btn filled" onClick={save} disabled={!!busy || !s.host || !s.from_address}>{busy === "save" && <Spinner />} Save</button>
      </div>
    </div>
  );
}

function ChannelDialog({ ch, onClose, onSaved }: { ch: Channel | null; onClose: () => void; onSaved: () => void }) {
  const toast = useToast();
  const [c, setC] = useState<Omit<Channel, "id" | "last_error" | "last_sent_at">>(
    ch ?? { name: "", kind: "email", url: "", enabled: true, min_severity: "warning" },
  );
  const [busy, setBusy] = useState(false);
  const save = async () => {
    setBusy(true);
    try {
      if (ch) await api(`/alert-channels/${ch.id}`, { method: "PUT", body: c });
      else await api("/alert-channels", { method: "POST", body: c });
      toast("Channel saved");
      onSaved();
      onClose();
    } catch (e: any) {
      toast(e.message, true);
    } finally {
      setBusy(false);
    }
  };
  const isEmail = c.kind === "email";
  return (
    <Dialog
      title={ch ? "Edit channel" : "Add notification channel"}
      onClose={onClose}
      footer={
        <>
          <button className="btn" onClick={onClose}>Cancel</button>
          <button className="btn filled" disabled={busy || !c.name.trim() || !c.url.trim()} onClick={save}>{busy && <Spinner />} Save</button>
        </>
      }
    >
      <div className="field">
        <label>Name</label>
        <input className="input" autoFocus value={c.name} onChange={(e) => setC({ ...c, name: e.target.value })} placeholder={isEmail ? "Ops team" : "#ops-alerts"} />
      </div>
      <div className="field">
        <label>Type</label>
        <select className="select" value={c.kind} onChange={(e) => setC({ ...c, kind: e.target.value as Channel["kind"] })}>
          {Object.entries(KIND_LABEL).map(([k, v]) => <option key={k} value={k}>{v}</option>)}
        </select>
      </div>
      <div className="field">
        <label>{isEmail ? "Recipients" : "Webhook URL"}</label>
        {isEmail ? (
          <textarea className="textarea" rows={3} value={c.url} onChange={(e) => setC({ ...c, url: e.target.value })} placeholder="ops@example.com, oncall@example.com" />
        ) : (
          <input className="input mono" value={c.url} onChange={(e) => setC({ ...c, url: e.target.value })} placeholder="https://…" />
        )}
        <span className="hint">
          {isEmail ? "Separate addresses with commas. Sent through the email server configured above." : c.kind === "teams" ? "Teams → channel → Workflows → “Post to a channel when a webhook request is received”." : c.kind === "slack" ? "Incoming-webhook URL; works for Slack, Google Chat and Mattermost." : "Receives a JSON POST with event, severity, server, rule, message and link."}
        </span>
      </div>
      <div className="field">
        <label>Send</label>
        <select className="select" value={c.min_severity} onChange={(e) => setC({ ...c, min_severity: e.target.value as Channel["min_severity"] })}>
          <option value="warning">Warnings and critical alerts</option>
          <option value="critical">Critical alerts only</option>
        </select>
        <span className="hint">Recoveries are always sent for alerts the channel was notified about.</span>
      </div>
      <label className="check"><input type="checkbox" checked={c.enabled} onChange={(e) => setC({ ...c, enabled: e.target.checked })} /> Enabled</label>
    </Dialog>
  );
}

function ChannelsCard() {
  const toast = useToast();
  const confirm = useConfirm();
  const { data, reload } = usePoll(() => api<Channel[]>("/alert-channels"), [], 30_000);
  const [editing, setEditing] = useState<Channel | null | "new">(null);
  const [testing, setTesting] = useState<number | null>(null);
  const test = async (c: Channel) => {
    setTesting(c.id);
    try {
      await api(`/alert-channels/${c.id}/test`, { method: "POST" });
      toast(`Test sent via ${c.name}`);
    } catch (e: any) {
      toast(e.message, true);
    } finally {
      setTesting(null);
      reload();
    }
  };
  return (
    <div className="card">
      <div className="card-head">
        <Send size={20} />
        <h2 className="grow">Notification channels</h2>
        <button className="btn tonal sm" onClick={() => setEditing("new")}><Plus size={16} /> Add channel</button>
      </div>
      <div className="card-body">
        {!data ? (
          <Loading />
        ) : data.length === 0 ? (
          <div className="small muted">No channels yet. Alerts are still recorded and shown here.</div>
        ) : (
          <div className="table-wrap">
            <table className="table">
              <thead><tr><th>Channel</th><th>Sends</th><th>Last delivery</th><th /></tr></thead>
              <tbody>
                {data.map((c) => (
                  <tr key={c.id} style={{ opacity: c.enabled ? 1 : 0.55 }}>
                    <td>
                      <div className="row" style={{ gap: 10 }}>
                        {KIND_ICON[c.kind]}
                        <div className="grow">
                          <div style={{ fontWeight: 500 }}>{c.name}{!c.enabled && <span className="chip" style={{ marginLeft: 8 }}>disabled</span>}</div>
                          <div className="small faint ellipsis" style={{ maxWidth: 380 }}>{c.kind === "email" ? c.url : KIND_LABEL[c.kind]}</div>
                        </div>
                      </div>
                    </td>
                    <td className="small">{c.min_severity === "critical" ? "Critical only" : "Warning + critical"}</td>
                    <td className="small">
                      {c.last_sent_at ? (c.last_error ? <span style={{ color: "var(--crit-text)" }} title={c.last_error}>failed {ago(c.last_sent_at)}</span> : <span className="muted">ok, {ago(c.last_sent_at)}</span>) : <span className="faint">never</span>}
                    </td>
                    <td className="actions">
                      <button className="btn sm" onClick={() => test(c)} disabled={testing === c.id}>{testing === c.id ? <Spinner /> : <Send size={15} />} Test</button>
                      <button className="icon-btn sm" aria-label="Edit" onClick={() => setEditing(c)}><Pencil size={16} /></button>
                      <button
                        className="icon-btn sm danger"
                        aria-label="Delete"
                        onClick={() => confirm({ title: `Delete ${c.name}?`, body: "This channel stops receiving notifications.", confirm: "Delete", danger: true, action: () => api(`/alert-channels/${c.id}`, { method: "DELETE" }).then(reload), success: "Channel deleted" })}
                      >
                        <Trash2 size={16} />
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
            {data.some((c) => c.last_error) && <div className="banner crit small" style={{ marginTop: 12 }}>{data.find((c) => c.last_error)?.name}: {data.find((c) => c.last_error)?.last_error}</div>}
          </div>
        )}
      </div>
      {editing && <ChannelDialog ch={editing === "new" ? null : editing} onClose={() => setEditing(null)} onSaved={reload} />}
    </div>
  );
}

export function AlertSettings() {
  return (
    <div className="page page-narrow">
      <div className="page-head">
        <div className="grow">
          <h1>Alert settings</h1>
          <div className="sub">Thresholds apply to every server. CPU and memory must stay above the limit for the whole duration, so short spikes are ignored.</div>
        </div>
      </div>
      <div className="col" style={{ gap: 16 }}>
        <ChannelsCard />
        <SmtpCard />
        <RulesCard />
      </div>
    </div>
  );
}
