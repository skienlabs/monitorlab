import { useMemo, useState } from "react";
import { CircleCheck, CirclePlay, CircleStop, FileText, MoreVertical, Power, PowerOff, RotateCcw, ScrollText, ShieldAlert, X } from "lucide-react";
import { rpc } from "../api";
import { useAuth } from "../auth";
import type { Server } from "../types";
import { Empty, ErrorBanner, Loading, Menu, SearchBox, Seg, useConfirm, useLocalStorage, usePoll, Sorted } from "../components/ui";
import { LogViewer } from "../components/LogViewer";
import { ago, bytes } from "../format";
import { Toolbar } from "./DockerTabs";

interface Svc {
  name: string;
  description: string;
  load?: string;
  active: string; // active | inactive | failed | activating | deactivating
  sub: string;
  enabled: string;
  protected: boolean;
  mem?: number | null;
  cpu_ns?: number | null;
  tasks?: number | null;
  pid?: number | null;
  restarts?: number | null;
  since?: number | null;
  account?: string;
}

const stateChip = (s: Svc) => {
  const cls = s.active === "active" ? "good" : s.active === "failed" ? "crit" : s.active === "activating" || s.active === "deactivating" ? "warn" : "";
  return <span className={`chip ${cls}`}>{s.active === "active" ? s.sub || "running" : s.active}</span>;
};

export function ServicesTab({ s }: { s: Server }) {
  const { can } = useAuth();
  const confirm = useConfirm();
  const windows = s.hello?.os === "windows";
  const [q, setQ] = useState("");
  const [f, setF] = useLocalStorage<"running" | "failed" | "all">(`monitorlab.svc.filter`, "running");
  const [logsOf, setLogsOf] = useState<string | null>(null);
  const { data, error, loading, reload } = usePoll(() => rpc<{ platform: string; services: Svc[] }>(s.id, { op: "services_list" }), [s.id], 15_000);
  const all = data?.services ?? [];
  const list = useMemo(() => {
    const n = q.trim().toLowerCase();
    return all
      .filter((x) => (f === "running" ? x.active === "active" || x.active === "activating" : f === "failed" ? x.active === "failed" : true))
      .filter((x) => !n || x.name.toLowerCase().includes(n) || x.description.toLowerCase().includes(n))
      .sort((a, b) => Number(b.active === "failed") - Number(a.active === "failed") || a.name.localeCompare(b.name));
  }, [all, q, f]);
  if (!s.online) return <Empty icon={<ScrollText size={26} />} title="Server is offline" />;
  if (error && !data) return <ErrorBanner error={error} onRetry={reload} />;
  if (!data) return <Loading />;
  const failed = all.filter((x) => x.active === "failed").length;
  const running = all.filter((x) => x.active === "active").length;

  const act = (svc: Svc, action: string, label: string, danger = false) =>
    confirm({
      title: `${label} ${svc.name}?`,
      body: action === "disable" ? "It will no longer start at boot." : action === "enable" ? "It will start automatically at boot." : `${svc.description || svc.name}`,
      confirm: label,
      danger,
      action: () => rpc(s.id, { op: "service_action", name: svc.name, action }).finally(reload),
      success: `${svc.name}: ${label.toLowerCase()} done`,
    });

  return (
    <div>
      <Toolbar onRefresh={reload} loading={loading}>
        <SearchBox value={q} onChange={setQ} placeholder={windows ? "Filter Windows services" : "Filter systemd services"} />
        <Seg value={f} onChange={setF} options={[{ value: "running", label: `Running ${running}` }, { value: "failed", label: `Failed ${failed}` }, { value: "all", label: `All ${all.length}` }]} />
      </Toolbar>
      {logsOf && (
        <div className="card" style={{ marginBottom: 16 }}>
          <div className="card-head row">
            <h3 className="grow">Logs · <span className="mono">{logsOf}</span></h3>
            <button className="icon-btn sm" onClick={() => setLogsOf(null)} aria-label="Close logs"><X size={17} /></button>
          </div>
          <div className="card-body">
            <LogViewer
              key={logsOf}
              fileName={logsOf}
              source={{ serverId: s.id, request: windows ? { kind: "system_logs", log: "System", unit: logsOf } : { kind: "system_logs", unit: logsOf }, reconnect: true, supportsSince: true }}
            />
          </div>
        </div>
      )}
      <div className="card table-wrap">
        {list.length === 0 ? (
          <Empty icon={<CircleCheck size={26} />} title={f === "failed" ? "No failed services" : "No services match"} />
        ) : (
          <Sorted rows={list} acc={{ name: x => x.name, state: x => x.active, enabled: x => x.enabled, mem: x => x.mem, restarts: x => x.restarts, since: x => x.since }}>{(S) => (
<table className="table">
            <thead>
              <tr>
                {S.th("name", "Service")}
                {S.th("state", "State")}
                {S.th("enabled", "Start")}
                {S.th("mem", "Memory", "num")}
                {!windows && S.th("restarts", "Restarts", "num")}
                {S.th("since", "Since")}
                <th />
              </tr>
            </thead>
            <tbody>
              {S.rows.map((x) => (
                <tr key={x.name}>
                  <td style={{ maxWidth: 420 }}>
                    <div className="mono" style={{ fontWeight: 500 }}>{x.name}{x.protected && <ShieldAlert size={13} style={{ marginLeft: 6, verticalAlign: "-2px", color: "var(--text-3)" }} aria-label="protected" />}</div>
                    <div className="small faint ellipsis">{x.description}</div>
                  </td>
                  <td>{stateChip(x)}</td>
                  <td className="small">{x.enabled}</td>
                  <td className="num tnum small">{x.mem ? bytes(x.mem) : "—"}</td>
                  {!windows && <td className="num tnum small">{x.restarts ?? "—"}</td>}
                  <td className="small muted nowrap">{x.since ? ago(x.since) : x.pid ? `pid ${x.pid}` : "—"}</td>
                  <td className="actions">
                    <div className="row" style={{ gap: 0, justifyContent: "flex-end" }}>
                      {can("logs.view") && (s.hello?.capabilities.system_logs ?? false) && (
                        <button className="icon-btn sm" title="Logs" aria-label={`Logs of ${x.name}`} onClick={() => setLogsOf(x.name)}><FileText size={17} /></button>
                      )}
                      {can("services.control") && (
                        <Menu trigger={<MoreVertical size={17} />}>
                          {x.active !== "active" && <button onClick={() => act(x, "start", "Start")}><CirclePlay size={17} /> Start</button>}
                          {x.active === "active" && <button onClick={() => act(x, "restart", "Restart")}><RotateCcw size={17} /> Restart</button>}
                          {x.active === "active" && !windows && <button onClick={() => act(x, "reload", "Reload")}><RotateCcw size={17} /> Reload config</button>}
                          {x.active === "active" && !x.protected && <button onClick={() => act(x, "stop", "Stop", true)}><CircleStop size={17} /> Stop</button>}
                          <hr />
                          {x.enabled !== "enabled" && x.enabled !== "static" && <button onClick={() => act(x, "enable", windows ? "Set to automatic" : "Enable at boot")}><Power size={17} /> {windows ? "Set to automatic" : "Enable at boot"}</button>}
                          {(x.enabled === "enabled" || x.enabled === "manual") && !x.protected && <button onClick={() => act(x, "disable", "Disable", true)}><PowerOff size={17} /> Disable</button>}
                        </Menu>
                      )}
                    </div>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
)}</Sorted>
        )}
      </div>
      <div className="small faint" style={{ marginTop: 8 }}>
        <ShieldAlert size={12} style={{ verticalAlign: "-1px" }} /> Protected services (SSH, D-Bus, systemd, the agent itself{s.is_local ? ", Docker" : ""}) can't be stopped from here, so you can't lock yourself out.
      </div>
    </div>
  );
}

const WIN_LOGS = ["System", "Application", "Security", "Setup"];

export function SystemLogsTab({ s }: { s: Server }) {
  const windows = s.hello?.os === "windows";
  const [unit, setUnit] = useState("");
  const [applied, setApplied] = useState("");
  const [log, setLog] = useState("System");
  const [prio, setPrio] = useLocalStorage<string>("monitorlab.syslog.prio", "all");
  if (!s.online) return <Empty icon={<ScrollText size={26} />} title="Server is offline" />;
  const request: Record<string, unknown> = { kind: "system_logs", ...(prio !== "all" ? { priority: Number(prio) } : {}) };
  if (windows) request.log = log;
  if (applied) request.unit = applied;
  return (
    <div className="col" style={{ gap: 12 }}>
      <div className="row wrap" style={{ gap: 10 }}>
        {windows && (
          <Seg value={log} onChange={setLog} options={WIN_LOGS.map((l) => ({ value: l, label: l }))} />
        )}
        <form
          className="row"
          style={{ gap: 6 }}
          onSubmit={(e) => {
            e.preventDefault();
            setApplied(unit.trim());
          }}
        >
          <input className="input" style={{ height: 36, width: 240 }} placeholder={windows ? "Event source (optional)" : "Unit, e.g. nginx.service (optional)"} value={unit} onChange={(e) => setUnit(e.target.value)} aria-label={windows ? "Event source" : "Unit"} />
          <button className="btn sm" type="submit">Apply</button>
        </form>
        <select className="select" style={{ height: 36, width: "auto" }} value={prio} onChange={(e) => setPrio(e.target.value)} aria-label="Minimum severity">
          <option value="all">All severities</option>
          <option value="3">Errors and worse</option>
          <option value="4">Warnings and worse</option>
          <option value="6">Info and worse (no debug)</option>
        </select>
      </div>
      <LogViewer
        key={JSON.stringify(request)}
        fileName={windows ? `${log}-eventlog` : applied || "journal"}
        source={{ serverId: s.id, request, reconnect: true, supportsSince: true }}
      />
    </div>
  );
}
