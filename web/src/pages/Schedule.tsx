import { useMemo, useState } from "react";
import { CalendarClock, CirclePlay, CircleStop, Clock, FileText, Power, PowerOff, X } from "lucide-react";
import { rpc } from "../api";
import { useAuth } from "../auth";
import type { Server } from "../types";
import { CopyButton, Empty, ErrorBanner, Loading, SearchBox, Seg, Sorted, useConfirm, usePoll } from "../components/ui";
import { LogViewer } from "../components/LogViewer";
import { ago, dateTime } from "../format";
import { Toolbar } from "./DockerTabs";

interface Item {
  id: string;
  kind: "cron" | "timer" | "task";
  name: string;
  description?: string;
  command: string;
  user?: string | null;
  schedule: string;
  summary?: string;
  source?: string;
  unit?: string;
  timer_unit?: string;
  folder?: string;
  system?: boolean;
  enabled: boolean;
  state: string;
  next_run?: number | null;
  next_runs?: number[];
  last_run?: number | null;
  last_result?: string | null;
}
interface Data {
  platform: string;
  items: Item[];
  notes: string[];
  cron_unit: string | null;
}

const KIND: Record<Item["kind"], string> = { cron: "Cron", timer: "Timer", task: "Task" };

function resultChip(i: Item) {
  const r = i.last_result;
  if (!r) return <span className="faint">—</span>;
  const ok = r === "success";
  const warn = r === "running" || r === "never run" || r === "disabled";
  return <span className={`chip ${ok ? "good" : warn ? "" : "crit"}`}>{r}</span>;
}

function when(ms?: number | null) {
  return ms ? <span title={dateTime(ms)}>{ago(ms)}</span> : <span className="faint">—</span>;
}

/** Future time as "in 5 min". `ago` only handles the past. */
function until(ms?: number | null) {
  if (!ms) return <span className="faint">—</span>;
  const d = ms - Date.now();
  if (d <= 0) return <span title={dateTime(ms)}>due</span>;
  const m = Math.round(d / 60_000);
  const t = m < 1 ? "in under a minute" : m < 90 ? `in ${m} min` : m < 48 * 60 ? `in ${Math.round(m / 60)} h` : `in ${Math.round(m / 1440)} days`;
  return <span title={dateTime(ms)}>{t}</span>;
}

function logRequest(s: Server, i: Item, cronUnit: string | null): Record<string, unknown> | null {
  if (i.kind === "timer") return { kind: "system_logs", unit: i.unit };
  if (i.kind === "cron") return cronUnit ? { kind: "system_logs", unit: cronUnit, grep: i.command.slice(0, 200) } : null;
  if (s.hello?.os === "windows") return { kind: "system_logs", log: "Microsoft-Windows-TaskScheduler/Operational", grep: i.name };
  return null;
}

export function ScheduleTab({ s }: { s: Server }) {
  const { can } = useAuth();
  const confirm = useConfirm();
  const [q, setQ] = useState("");
  const [f, setF] = useState<"all" | "cron" | "timer" | "task">("all");
  const [sys, setSys] = useState(false);
  const [open, setOpen] = useState<string | null>(null);
  const { data, error, loading, reload } = usePoll(() => rpc<Data>(s.id, { op: "schedule_list" }), [s.id], 30_000);
  const windows = s.hello?.os === "windows";
  const all = data?.items ?? [];
  const list = useMemo(() => {
    const n = q.trim().toLowerCase();
    return all
      .filter((x) => (f === "all" ? true : x.kind === f))
      .filter((x) => sys || !x.system)
      .filter((x) => !n || [x.name, x.command, x.schedule, x.user ?? "", x.description ?? ""].some((v) => v.toLowerCase().includes(n)));
  }, [all, q, f, sys]);
  if (!s.online) return <Empty icon={<CalendarClock size={26} />} title="Server is offline" />;
  if (error && !data) return <ErrorBanner error={error} onRetry={reload} />;
  if (!data) return <Loading />;
  const counts = { cron: all.filter((x) => x.kind === "cron").length, timer: all.filter((x) => x.kind === "timer").length, task: all.filter((x) => x.kind === "task" && (sys || !x.system)).length };
  const sel = all.find((x) => x.id === open) ?? null;
  const canLogs = can("logs.view") && (s.hello?.capabilities.system_logs ?? false);

  const run = (i: Item, action: "start" | "stop" | "enable" | "disable", label: string, danger = false) =>
    confirm({
      title: `${label} ${i.name}?`,
      body: i.kind === "timer" ? (action === "start" ? "Starts the job now, outside its schedule." : action === "disable" ? "The timer will no longer fire." : "Changes the timer.") : i.command,
      confirm: label,
      danger,
      action: () =>
        (i.kind === "timer"
          ? rpc(s.id, { op: "service_action", name: action === "start" || action === "stop" ? i.unit : i.timer_unit, action })
          : rpc(s.id, { op: "schedule_action", name: i.source, action })
        ).finally(reload),
      success: `${i.name}: ${label.toLowerCase()} done`,
    });

  return (
    <div>
      <Toolbar onRefresh={reload} loading={loading}>
        <SearchBox value={q} onChange={setQ} placeholder="Filter by name, command or schedule" />
        <Seg
          value={f}
          onChange={setF}
          options={[
            { value: "all", label: "All" },
            ...(counts.cron || !windows ? [{ value: "cron" as const, label: `Cron ${counts.cron}` }] : []),
            ...(!windows ? [{ value: "timer" as const, label: `Timers ${counts.timer}` }] : [{ value: "task" as const, label: `Tasks ${counts.task}` }]),
          ]}
        />
        {windows && (
          <label className="check small">
            <input type="checkbox" checked={sys} onChange={(e) => setSys(e.target.checked)} /> Show built-in Windows tasks
          </label>
        )}
      </Toolbar>
      {error && <ErrorBanner error={error} onRetry={reload} />}
      {data.notes.map((n) => (
        <div key={n} className="banner warn small" style={{ marginBottom: 12 }}>{n}</div>
      ))}
      <div className="card table-wrap">
        {list.length === 0 ? (
          <Empty icon={<CalendarClock size={26} />} title="No scheduled jobs match" />
        ) : (
          <Sorted
            rows={list}
            acc={{ name: (x) => x.name, kind: (x) => x.kind, schedule: (x) => x.schedule, next: (x) => x.next_run, last: (x) => x.last_run, result: (x) => x.last_result, user: (x) => x.user }}
            initial={{ key: "next" }}
          >
            {(S) => (
              <table className="table">
                <thead>
                  <tr>
                    {S.th("name", "Job")}
                    {S.th("kind", "Type")}
                    {S.th("schedule", "Schedule")}
                    {S.th("next", "Next run")}
                    {S.th("last", "Last run")}
                    {S.th("result", "Result")}
                    {S.th("user", "User")}
                    <th />
                  </tr>
                </thead>
                <tbody>
                  {S.rows.map((x) => (
                    <tr key={x.id} style={{ cursor: "pointer", opacity: x.enabled ? 1 : 0.6 }} onClick={() => setOpen(x.id)}>
                      <td style={{ maxWidth: 380 }}>
                        <div style={{ fontWeight: 500 }}>{x.name}{!x.enabled && <span className="chip" style={{ marginLeft: 8 }}>disabled</span>}</div>
                        <div className="small faint mono ellipsis" title={x.command}>{x.command || x.description}</div>
                      </td>
                      <td><span className="chip outline">{KIND[x.kind]}</span></td>
                      <td className="small" style={{ maxWidth: 260 }}>
                        <div className="mono ellipsis" title={x.schedule}>{x.schedule || "—"}</div>
                        {x.summary && <div className="faint">{x.summary}</div>}
                      </td>
                      <td className="small nowrap">{until(x.next_run)}</td>
                      <td className="small muted nowrap">{when(x.last_run)}</td>
                      <td>{resultChip(x)}</td>
                      <td className="small">{x.user || "—"}</td>
                      <td className="actions" onClick={(e) => e.stopPropagation()}>
                        {can("services.control") && (x.kind === "timer" || (x.kind === "task" && !x.system)) && (
                          <div className="row" style={{ gap: 0, justifyContent: "flex-end" }}>
                            <button className="icon-btn sm" title="Run now" aria-label={`Run ${x.name} now`} onClick={() => run(x, "start", "Run now")}><CirclePlay size={17} /></button>
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
      {sel && (
        <Detail s={s} item={sel} cronUnit={data.cron_unit} canLogs={canLogs} onClose={() => setOpen(null)} canControl={can("services.control")} run={run} />
      )}
    </div>
  );
}

function Detail(props: {
  s: Server;
  item: Item;
  cronUnit: string | null;
  canLogs: boolean;
  canControl: boolean;
  onClose: () => void;
  run: (i: Item, action: "start" | "stop" | "enable" | "disable", label: string, danger?: boolean) => void;
}) {
  const { s, item: i, cronUnit, canLogs, canControl, onClose, run } = props;
  const req = canLogs ? logRequest(s, i, cronUnit) : null;
  const row = (k: string, v: React.ReactNode) => (
    <div className="row" style={{ alignItems: "flex-start", gap: 12, padding: "6px 0" }}>
      <div className="small muted" style={{ width: 110, flex: "none" }}>{k}</div>
      <div className="small grow" style={{ overflowWrap: "anywhere" }}>{v}</div>
    </div>
  );
  return (
    <div className="card" style={{ marginTop: 16 }}>
      <div className="card-head row">
        <h3 className="grow"><Clock size={16} style={{ verticalAlign: "-2px", marginRight: 6 }} />{i.name} <span className="chip outline" style={{ marginLeft: 6 }}>{KIND[i.kind]}</span></h3>
        {canControl && i.kind === "timer" && (
          <>
            <button className="btn sm" onClick={() => run(i, "start", "Run now")}><CirclePlay size={15} /> Run now</button>
            {i.enabled ? (
              <button className="btn sm" onClick={() => run(i, "disable", "Disable timer", true)}><PowerOff size={15} /> Disable</button>
            ) : (
              <button className="btn sm" onClick={() => run(i, "enable", "Enable timer")}><Power size={15} /> Enable</button>
            )}
          </>
        )}
        {canControl && i.kind === "task" && !i.system && (
          <>
            <button className="btn sm" onClick={() => run(i, "start", "Run now")}><CirclePlay size={15} /> Run now</button>
            <button className="btn sm" onClick={() => run(i, "stop", "Stop", true)}><CircleStop size={15} /> Stop</button>
            {i.enabled ? (
              <button className="btn sm" onClick={() => run(i, "disable", "Disable task", true)}><PowerOff size={15} /> Disable</button>
            ) : (
              <button className="btn sm" onClick={() => run(i, "enable", "Enable task")}><Power size={15} /> Enable</button>
            )}
          </>
        )}
        <button className="icon-btn sm" onClick={onClose} aria-label="Close details"><X size={17} /></button>
      </div>
      <div className="card-body">
        {i.description && row("Description", i.description)}
        {row("Command", <span className="mono">{i.command || "—"} {i.command && <CopyButton text={i.command} label="" />}</span>)}
        {row("Schedule", <><span className="mono">{i.schedule || "—"}</span>{i.summary && <div className="muted">{i.summary}</div>}</>)}
        {row("Runs as", i.user || "—")}
        {row("Status", `${i.enabled ? "enabled" : "disabled"} · ${i.state}`)}
        {row("Last run", i.last_run ? `${dateTime(i.last_run)} (${ago(i.last_run)})` : "No record")}
        {row("Last result", resultChip(i))}
        {row("Next run", i.next_run ? dateTime(i.next_run) : i.kind === "cron" && i.schedule === "@reboot" ? "At next boot" : "—")}
        {i.next_runs && i.next_runs.length > 1 && row("Then", i.next_runs.slice(1).map((t) => dateTime(t)).join(" · "))}
        {i.unit && row("Starts", <span className="mono">{i.unit}</span>)}
        {i.source && row("Defined in", <span className="mono">{i.source}</span>)}
        {i.kind === "cron" && i.last_run == null && (
          <div className="small faint" style={{ marginTop: 8 }}>
            No recent run was found in the system journal. Last-run times for cron come from the journal's cron lines; cron does not record exit codes, and a job's own output is only kept if the command redirects it.
          </div>
        )}
        <h4 style={{ margin: "16px 0 8px" }}><FileText size={15} style={{ verticalAlign: "-2px" }} /> Logs</h4>
        {req ? (
          <>
            <LogViewer key={i.id} fileName={i.name} source={{ serverId: s.id, request: req, reconnect: true, supportsSince: true }} />
            {i.kind === "cron" && <div className="small faint" style={{ marginTop: 6 }}>Shows when cron started this command. Output and errors of the job appear only if it logs them (for example to syslog, or a file you can open under Logs).</div>}
            {s.hello?.os === "windows" && <div className="small faint" style={{ marginTop: 6 }}>Task history comes from the Task Scheduler Operational log. If it is empty, enable it once: <span className="mono">wevtutil sl Microsoft-Windows-TaskScheduler/Operational /e:true</span></div>}
          </>
        ) : (
          <div className="small muted">{canLogs ? "Logs are not available for this job on this server." : "You need the logs permission to see logs."}</div>
        )}
      </div>
    </div>
  );
}
