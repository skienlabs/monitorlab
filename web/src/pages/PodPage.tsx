import { useState } from "react";
import { Link, useNavigate, useParams, useSearchParams } from "react-router-dom";
import { ChevronRight, FileText, Gauge, Info, SquareTerminal, Trash2 } from "lucide-react";
import { UsageHistory } from "../components/UsageHistory";
import { bytes } from "../format";
import { api, rpc } from "../api";
import { useAuth } from "../auth";
import type { Server } from "../types";
import { CodeBlock, Empty, ErrorBanner, Loading, useConfirm, usePoll } from "../components/ui";
import { LogViewer } from "../components/LogViewer";
import { PodTerminal } from "../components/Terminal";
import { ago, dateTime } from "../format";
import { cores, PodStatus, UsageCell } from "./K8sTab";

/** CPU/memory of each container against its requests and limits, plus history. */
function Usage({ serverId, ns, pod, d }: { serverId: string; ns: string; pod: string; d: PodDetail }) {
  const q = (v: string | undefined, cpu: boolean) => {
    if (!v) return 0;
    if (cpu) return v.endsWith("m") ? parseFloat(v) : parseFloat(v) * 1000;
    const units: Record<string, number> = { Ki: 1024, Mi: 1048576, Gi: 1073741824, Ti: 1099511627776, k: 1e3, M: 1e6, G: 1e9, T: 1e12 };
    const m = v.match(/^([\d.]+)([A-Za-z]*)$/);
    return m ? parseFloat(m[1]) * (units[m[2]] ?? 1) : 0;
  };
  const specs: any[] = d.object.spec?.containers ?? [];
  const rows = specs.map((c) => {
    const u = d.usage?.find((x) => x.name === c.name);
    const r = c.resources ?? {};
    return {
      name: c.name,
      cpu: u?.cpu ?? null,
      mem: u?.mem ?? null,
      cpu_req: q(r.requests?.cpu, true),
      cpu_lim: q(r.limits?.cpu, true),
      mem_req: q(r.requests?.memory, false),
      mem_lim: q(r.limits?.memory, false),
    };
  });
  const sum = (k: keyof (typeof rows)[number]) => rows.reduce((a, r) => a + ((r[k] as number) || 0), 0);
  return (
    <div className="col" style={{ gap: 16 }}>
      <div className="card table-wrap">
        <div className="card-head"><h3>Right now</h3></div>
        {d.usage == null && <div className="card-body small muted">No live usage: metrics-server is not installed or has no data for this pod yet.</div>}
        <table className="table">
          <thead><tr><th>Container</th><th className="num">CPU</th><th className="num">CPU request / limit</th><th className="num">Memory</th><th className="num">Memory request / limit</th></tr></thead>
          <tbody>
            {rows.map((r) => (
              <tr key={r.name}>
                <td style={{ fontWeight: 500 }}>{r.name}</td>
                <td className="num tnum small"><UsageCell used={r.cpu} req={r.cpu_req} lim={r.cpu_lim} fmt={cores} /></td>
                <td className="num tnum small faint">{r.cpu_req ? cores(r.cpu_req) : "none"} / {r.cpu_lim ? cores(r.cpu_lim) : "none"}</td>
                <td className="num tnum small"><UsageCell used={r.mem} req={r.mem_req} lim={r.mem_lim} fmt={(v) => bytes(v)} /></td>
                <td className="num tnum small faint">{r.mem_req ? bytes(r.mem_req) : "none"} / {r.mem_lim ? bytes(r.mem_lim) : "none"}</td>
              </tr>
            ))}
          </tbody>
        </table>
        {rows.some((r) => !r.mem_lim) && <div className="card-body small faint">Containers without a memory limit can use all of the node's memory.</div>}
      </div>
      <UsageHistory serverId={serverId} kind="pod" name={`${ns}/${pod}`} limits={{ cpu_req: sum("cpu_req"), cpu_lim: sum("cpu_lim"), mem_req: sum("mem_req"), mem_lim: sum("mem_lim") }} />
    </div>
  );
}

interface PodDetail {
  usage: { name: string; cpu: number; mem: number }[] | null;
  object: any;
  events: { type: string; reason: string; message: string; count: number; last: number }[];
}

function containerState(st: any): { text: string; cls: string } {
  if (!st) return { text: "unknown", cls: "" };
  if (st.running) return { text: `running since ${dateTime(Date.parse(st.running.startedAt))}`, cls: "good" };
  if (st.waiting) {
    const bad = ["CrashLoopBackOff", "ImagePullBackOff", "ErrImagePull", "CreateContainerConfigError", "CreateContainerError", "InvalidImageName", "RunContainerError"];
    return { text: st.waiting.reason || "waiting", cls: bad.includes(st.waiting.reason) ? "crit" : "warn" };
  }
  if (st.terminated) return { text: `${st.terminated.reason || "terminated"} (exit ${st.terminated.exitCode})`, cls: st.terminated.exitCode === 0 ? "" : "crit" };
  return { text: "unknown", cls: "" };
}

function Details({ d }: { d: PodDetail }) {
  const [raw, setRaw] = useState(false);
  const o = d.object;
  const statuses: any[] = [...(o.status?.initContainerStatuses ?? []), ...(o.status?.containerStatuses ?? [])];
  const specs: any[] = [...(o.spec?.initContainers ?? []).map((c: any) => ({ ...c, init: true })), ...(o.spec?.containers ?? [])];
  return (
    <div className="col" style={{ gap: 16 }}>
      <div className="grid grid-2">
        <div className="card">
          <div className="card-head"><h3>Pod</h3></div>
          <div className="card-body">
            <dl className="kv">
              <dt>Namespace</dt><dd>{o.metadata.namespace}</dd>
              <dt>Node</dt><dd className="mono">{o.spec.nodeName || "—"}</dd>
              <dt>Pod IP</dt><dd className="mono">{o.status.podIP || "—"}</dd>
              <dt>Controlled by</dt><dd>{o.metadata.ownerReferences?.map((r: any) => `${r.kind}/${r.name}`).join(", ") || "—"}</dd>
              <dt>QoS class</dt><dd>{o.status.qosClass || "—"}</dd>
              <dt>Service account</dt><dd>{o.spec.serviceAccountName || "default"}</dd>
              <dt>Created</dt><dd>{dateTime(Date.parse(o.metadata.creationTimestamp))}</dd>
              <dt>Restart policy</dt><dd>{o.spec.restartPolicy}</dd>
            </dl>
          </div>
        </div>
        <div className="card">
          <div className="card-head"><h3>Conditions</h3></div>
          <div className="card-body col" style={{ gap: 8 }}>
            {(o.status.conditions ?? []).map((c: any) => (
              <div key={c.type} className="row small">
                <span className={`chip ${c.status === "True" ? "good" : "warn"}`}>{c.status === "True" ? "✓" : "✗"}</span>
                <b>{c.type}</b>
                <span className="faint">{c.reason ? `${c.reason}${c.message ? ` — ${c.message}` : ""}` : ago(Date.parse(c.lastTransitionTime))}</span>
              </div>
            ))}
          </div>
        </div>
      </div>
      <div className="card">
        <div className="card-head"><h3>Containers</h3></div>
        <div className="card-body table-wrap">
          <table className="table">
            <thead><tr><th>Name</th><th>Image</th><th>State</th><th className="num">Restarts</th><th>Last termination</th><th>Requests / limits</th></tr></thead>
            <tbody>
              {specs.map((c) => {
                const st = statuses.find((x) => x.name === c.name);
                const cs = containerState(st?.state);
                const last = st?.lastState?.terminated;
                const r = c.resources ?? {};
                return (
                  <tr key={c.name}>
                    <td><b>{c.name}</b>{c.init && <span className="chip outline" style={{ marginLeft: 6 }}>init</span>}</td>
                    <td className="mono small" style={{ maxWidth: 300, overflowWrap: "anywhere" }}>{c.image}</td>
                    <td><span className={`chip ${cs.cls}`}>{cs.text}</span></td>
                    <td className="num tnum">{st?.restartCount ?? 0}</td>
                    <td className="small">{last ? `${last.reason} (exit ${last.exitCode}) ${ago(Date.parse(last.finishedAt))}` : "—"}</td>
                    <td className="small mono">
                      cpu {r.requests?.cpu ?? "—"} / {r.limits?.cpu ?? "—"}
                      <br />mem {r.requests?.memory ?? "—"} / {r.limits?.memory ?? "—"}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      </div>
      <div className="card">
        <div className="card-head"><h3>Events</h3></div>
        <div className="card-body col" style={{ gap: 6 }}>
          {d.events.length === 0 && <div className="small muted">No recent events (Kubernetes keeps them for about an hour).</div>}
          {d.events.map((e, i) => (
            <div key={i} className="small">
              <span className={`chip ${e.type === "Warning" ? "warn" : "outline"}`} style={{ marginRight: 8 }}>{e.reason}</span>
              <span className="faint">{ago(e.last)}{e.count > 1 ? ` ×${e.count}` : ""}</span> · {e.message}
            </div>
          ))}
        </div>
      </div>
      <div className="card">
        <div className="card-head"><h3>Labels</h3></div>
        <div className="card-body row wrap" style={{ gap: 6 }}>
          {Object.entries(o.metadata.labels ?? {}).map(([k, v]) => <span key={k} className="chip outline mono" style={{ fontSize: 11.5 }}>{k}={String(v)}</span>)}
        </div>
      </div>
      <div>
        <button className="btn" onClick={() => setRaw((r) => !r)}>{raw ? "Hide" : "Show"} raw JSON</button>
        {raw && <CodeBlock>{JSON.stringify(o, null, 2)}</CodeBlock>}
      </div>
    </div>
  );
}

export function PodPage() {
  const { id: serverId = "", ns = "", pod = "" } = useParams();
  const [params, setParams] = useSearchParams();
  const { can } = useAuth();
  const confirm = useConfirm();
  const nav = useNavigate();
  const server = usePoll(() => api<Server>(`/servers/${serverId}`), [serverId], 30_000);
  const det = usePoll(() => rpc<PodDetail>(serverId, { op: "k8s_get", kind: "pod", namespace: ns, name: pod }), [serverId, ns, pod], 5000);
  const [container, setContainer] = useState("");
  const [previous, setPrevious] = useState(false);

  const tabs = [
    { key: "logs", label: "Logs", icon: <FileText size={17} />, show: can("logs.view") },
    { key: "terminal", label: "Terminal", icon: <SquareTerminal size={17} />, show: can("k8s.exec") },
    { key: "usage", label: "Usage", icon: <Gauge size={17} />, show: true },
    { key: "details", label: "Details", icon: <Info size={17} />, show: true },
  ].filter((t) => t.show);
  const tab = tabs.some((t) => t.key === params.get("tab")) ? params.get("tab")! : tabs[0].key;

  const d = det.data;
  if (det.error && !d) {
    return (
      <div className="page">
        <ErrorBanner error={det.error} onRetry={det.reload} />
        <Link to={`/servers/${serverId}?tab=k8s&k=pods`}>Back to pods</Link>
      </div>
    );
  }
  if (!d) return <Loading />;
  const o = d.object;
  const containers: string[] = (o.spec?.containers ?? []).map((c: any) => c.name);
  const inits: string[] = (o.spec?.initContainers ?? []).map((c: any) => c.name);
  const cur = container || containers[0] || "";
  const statuses: any[] = o.status?.containerStatuses ?? [];
  const restarts = statuses.reduce((a, c) => a + (c.restartCount ?? 0), 0);
  const failing = statuses.some((c) => ["CrashLoopBackOff", "ImagePullBackOff", "ErrImagePull"].includes(c.state?.waiting?.reason));
  const status = o.metadata.deletionTimestamp ? "Terminating" : statuses.find((c) => c.state?.waiting)?.state.waiting.reason ?? o.status?.phase;
  const running = o.status?.phase === "Running";
  const owner = o.metadata.ownerReferences?.[0];

  const del = () =>
    confirm({
      title: `Delete pod ${pod}?`,
      body: owner ? `${owner.kind} ${owner.name} will create a replacement pod (this is how a pod is restarted).` : "This pod is not managed by a controller and will NOT be recreated.",
      confirm: "Delete pod",
      danger: true,
      option: "Force (skip the graceful shutdown period)",
      action: (force) => rpc(serverId, { op: "k8s_pod_delete", namespace: ns, name: pod, force }).then(() => nav(`/servers/${serverId}?tab=k8s&k=pods`)),
      success: `${pod} deleted`,
    });

  return (
    <div className="page">
      <div className="page-head" style={{ paddingBottom: 8 }}>
        <div className="grow">
          <div className="crumbs">
            <Link to="/">Overview</Link>
            <ChevronRight size={14} />
            <Link to={`/servers/${serverId}`}>{server.data?.name ?? "Server"}</Link>
            <ChevronRight size={14} />
            <Link to={`/servers/${serverId}?tab=k8s&k=pods`}>Pods</Link>
            <ChevronRight size={14} />
            <span>{pod}</span>
          </div>
          <div className="row wrap" style={{ gap: 14 }}>
            <h1 style={{ overflowWrap: "anywhere" }}>{pod}</h1>
            <PodStatus p={{ status, failing, phase: o.status?.phase }} />
            {restarts > 0 && <span className="chip warn">{restarts} restart{restarts === 1 ? "" : "s"}</span>}
          </div>
          <div className="sub small">
            {ns} · {o.spec?.nodeName ?? "unscheduled"}{owner ? ` · ${owner.kind}/${owner.name}` : ""}
          </div>
        </div>
        {can("k8s.control") && (
          <button className="btn sm danger-text" onClick={del}>
            <Trash2 size={16} /> {owner ? "Delete (restart)" : "Delete"}
          </button>
        )}
      </div>

      <div className="tabs" role="tablist">
        {tabs.map((t) => (
          <button key={t.key} className={tab === t.key ? "on" : ""} onClick={() => setParams({ tab: t.key }, { replace: true })} role="tab" aria-selected={tab === t.key}>
            {t.icon}
            {t.label}
          </button>
        ))}
      </div>

      {tab === "logs" && (
        <LogViewer
          key={`${cur}-${previous}`}
          fileName={`${pod}-${cur}`}
          source={{ serverId, request: { kind: "k8s_pod_logs", namespace: ns, name: pod, container: cur, follow: !previous, previous }, reconnect: !previous, supportsSince: !previous }}
          extraBar={
            <>
              {containers.length + inits.length > 1 && (
                <select className="select" style={{ height: 32, width: "auto" }} value={cur} onChange={(e) => setContainer(e.target.value)} aria-label="Container">
                  {containers.map((c) => <option key={c} value={c}>{c}</option>)}
                  {inits.map((c) => <option key={c} value={c}>{c} (init)</option>)}
                </select>
              )}
              <label className="check small" title="Logs of the previous container instance, e.g. before a crash">
                <input type="checkbox" checked={previous} onChange={(e) => setPrevious(e.target.checked)} /> Previous
              </label>
            </>
          }
        />
      )}
      {tab === "terminal" &&
        (running ? <PodTerminal serverId={serverId} namespace={ns} pod={pod} containers={containers} /> : <Empty icon={<SquareTerminal size={26} />} title="Pod is not running" />)}
      {tab === "usage" && <Usage serverId={serverId} ns={ns} pod={pod} d={d} />}
      {tab === "details" && <Details d={d} />}
    </div>
  );
}
