import { ReactNode, useMemo, useState } from "react";
import { Link, useNavigate, useSearchParams } from "react-router-dom";
import {
  AlertTriangle, Boxes, CircleCheck, History, Play as PlayIcon, Cpu, FileText, Layers, MoreVertical, Network, Pause, Play, RotateCcw, Scale, Server as ServerIcon,
  SquareTerminal, Trash2, Workflow, Lock, HardDrive as HardDriveIcon,
} from "lucide-react";
import { rpc } from "../api";
import { useAuth } from "../auth";
import type { Server } from "../types";
import { Dialog, Empty, ErrorBanner, Loading, Menu, Meter, SearchBox, Seg, Spinner, useConfirm, useLocalStorage, usePoll, useToast, Sorted } from "../components/ui";
import { ago, bytes, plural } from "../format";
import { Toolbar } from "./DockerTabs";
import { UsageHistory } from "../components/UsageHistory";

// ------------------------------------------------------------------ types

export interface K8sNode {
  name: string;
  ready: boolean;
  roles: string[];
  unschedulable: boolean;
  version: string;
  os: string;
  kernel: string;
  runtime: string;
  arch: string;
  ip: string;
  cpu_capacity: number;
  mem_capacity: number;
  pods_capacity: number;
  pods: number;
  cpu_used: number | null;
  mem_used: number | null;
  problems: string[];
  created: number;
}

interface Summary {
  nodes: number;
  nodes_ready: number;
  pods: number;
  pods_running: number;
  pods_failing: number;
  workloads: number;
  workloads_degraded: number;
  not_ready_nodes: string[];
  failing: string[];
}

interface Overview {
  version: string;
  distribution: string;
  metrics_available: boolean;
  nodes: K8sNode[];
  namespaces: number;
  services: number;
  summary: Summary;
  source: string;
}

export interface Pod {
  namespace: string;
  name: string;
  phase: string;
  status: string;
  failing: boolean;
  ready: string;
  restarts: number;
  last_restart: number | null;
  node: string;
  ip: string;
  created: number;
  containers: string[];
  init_containers: string[];
  images: string[];
  owner: { kind: string; name: string } | null;
  cpu: number | null;
  mem: number | null;
  cpu_req?: number;
  cpu_lim?: number;
  mem_req?: number;
  mem_lim?: number;
  workload?: string;
  oom_killed?: boolean;
}

interface Workload {
  kind: "deployment" | "stateful_set" | "daemon_set";
  namespace: string;
  name: string;
  desired: number;
  ready: number;
  available: number;
  updated: number;
  degraded: boolean;
  images: string[];
  created: number;
  restarted_at: number | null;
}

interface Service {
  namespace: string;
  name: string;
  type: string;
  cluster_ip: string;
  external: string[];
  ports: string[];
  created: number;
}

interface KEvent {
  namespace: string;
  type: string;
  reason: string;
  message: string;
  object: string;
  kind: string;
  name: string;
  count: number;
  last: number;
}

const KIND_LABEL: Record<Workload["kind"], string> = { deployment: "Deployment", stateful_set: "StatefulSet", daemon_set: "DaemonSet" };

export const cores = (m: number | null | undefined) => (m == null ? "—" : m >= 1000 ? `${(m / 1000).toFixed(m >= 10000 ? 0 : 2)} cores` : `${Math.round(m)}m`);

export function PodStatus({ p }: { p: Pick<Pod, "status" | "failing" | "phase"> }) {
  const cls = p.failing ? "crit" : p.status === "Running" ? "good" : p.phase === "Succeeded" || p.status === "Completed" ? "" : "warn";
  return <span className={`chip ${cls}`}>{p.status || p.phase}</span>;
}

export const podPath = (serverId: string, ns: string, name: string, tab?: string) =>
  `/servers/${serverId}/k8s/${encodeURIComponent(ns)}/${encodeURIComponent(name)}${tab ? `?tab=${tab}` : ""}`;

// ------------------------------------------------------------------ tab

type Sub = "overview" | "namespaces" | "workloads" | "pods" | "jobs" | "network" | "storage" | "config" | "events";
const SUBS: Sub[] = ["overview", "namespaces", "workloads", "pods", "jobs", "network", "storage", "config", "events"];

export function KubernetesTab({ s }: { s: Server }) {
  const [params, setParams] = useSearchParams();
  const sub = (SUBS.includes((params.get("k") ?? "") as Sub) ? params.get("k") : params.get("k") === "services" ? "network" : "overview") as Sub;
  const [ns, setNs] = useLocalStorage<string>(`monitorlab.k8s.ns.${s.id}`, "");
  const nss = usePoll(() => rpc<string[]>(s.id, { op: "k8s_namespaces" }), [s.id], 60_000);
  const setSub = (k: Sub) => {
    const p = new URLSearchParams(params);
    p.set("k", k);
    p.delete("q");
    setParams(p, { replace: true });
  };

  if (!s.online) return <Empty icon={<Boxes size={26} />} title="Server is offline">Kubernetes information is live and needs the agent to be connected.</Empty>;

  return (
    <div>
      <div className="row wrap" style={{ gap: 12, marginBottom: 16 }}>
        <Seg
          value={sub}
          onChange={setSub}
          options={[
            { value: "overview", label: "Cluster" },
            { value: "namespaces", label: "Namespaces" },
            { value: "workloads", label: "Workloads" },
            { value: "pods", label: "Pods" },
            { value: "jobs", label: "Jobs" },
            { value: "network", label: "Network" },
            { value: "storage", label: "Storage" },
            { value: "config", label: "Config" },
            { value: "events", label: "Events" },
          ]}
        />
        <span className="spacer" />
        {sub !== "overview" && sub !== "namespaces" && (
          <select className="select" style={{ height: 36, minWidth: 180, width: "auto" }} value={ns} onChange={(e) => setNs(e.target.value)} aria-label="Namespace">
            <option value="">All namespaces</option>
            {(nss.data ?? []).map((n) => (
              <option key={n} value={n}>{n}</option>
            ))}
          </select>
        )}
      </div>
      {sub === "overview" && <ClusterOverview s={s} onPods={() => setSub("pods")} />}
      {sub === "workloads" && <Workloads s={s} ns={ns} />}
      {sub === "pods" && <Pods s={s} ns={ns} />}
      {sub === "network" && (
        <div className="col" style={{ gap: 16 }}>
          <Ingresses s={s} ns={ns} />
          <Services s={s} ns={ns} />
        </div>
      )}
      {sub === "namespaces" && <Namespaces s={s} onOpen={(n) => { setNs(n); setSub("pods"); }} />}
      {sub === "jobs" && <Jobs s={s} ns={ns} />}
      {sub === "storage" && <Storage s={s} ns={ns} />}
      {sub === "config" && <ConfigRes s={s} ns={ns} />}
      {sub === "events" && <Events s={s} ns={ns} />}
    </div>
  );
}

// ------------------------------------------------------------------ cluster overview

function ClusterOverview({ s, onPods }: { s: Server; onPods: () => void }) {
  const { can } = useAuth();
  const confirm = useConfirm();
  const { data, error, loading, reload } = usePoll(() => rpc<Overview>(s.id, { op: "k8s_overview" }), [s.id], 10_000);
  if (error && !data) return <ErrorBanner error={error} onRetry={reload} />;
  if (!data) return <Loading />;
  const sm = data.summary;
  const cordon = (n: K8sNode) =>
    confirm({
      title: `${n.unschedulable ? "Uncordon" : "Cordon"} node ${n.name}?`,
      body: n.unschedulable ? "New pods may be scheduled on this node again." : "No new pods will be scheduled on this node. Running pods stay where they are.",
      confirm: n.unschedulable ? "Uncordon" : "Cordon",
      action: () => rpc(s.id, { op: "k8s_node_cordon", name: n.name, unschedulable: !n.unschedulable }).finally(reload),
      success: `${n.name} ${n.unschedulable ? "uncordoned" : "cordoned"}`,
    });

  return (
    <div className="col" style={{ gap: 16 }}>
      <div className="grid grid-4">
        <div className="card kpi" data-accent={sm.nodes_ready < sm.nodes ? "crit" : "cpu"}>
          <div className="label"><ServerIcon size={16} /> Nodes</div>
          <div className="value">{sm.nodes_ready}<span className="unit"> / {sm.nodes} ready</span></div>
          <div className="foot">{data.distribution} {data.version}</div>
        </div>
        <div className="card kpi" data-accent={sm.pods_failing > 0 ? "crit" : "good"} style={{ cursor: "pointer" }} onClick={onPods}>
          <div className="label"><Boxes size={16} /> Pods</div>
          <div className="value">{sm.pods_running}<span className="unit"> / {sm.pods} running</span></div>
          <div className="foot">{sm.pods_failing > 0 ? <span style={{ color: "var(--crit-text)" }}>{plural(sm.pods_failing, "pod")} failing</span> : "No failing pods"}</div>
        </div>
        <div className="card kpi" data-accent={sm.workloads_degraded > 0 ? "warn" : "mem"}>
          <div className="label"><Workflow size={16} /> Workloads</div>
          <div className="value">{sm.workloads}</div>
          <div className="foot">{sm.workloads_degraded > 0 ? `${sm.workloads_degraded} not fully ready` : "All ready"}</div>
        </div>
        <div className="card kpi" data-accent="net">
          <div className="label"><Network size={16} /> Services</div>
          <div className="value">{data.services}</div>
          <div className="foot">{plural(data.namespaces, "namespace")}</div>
        </div>
      </div>

      {sm.failing.length > 0 && (
        <div className="banner crit">
          <div style={{ fontWeight: 600, marginBottom: 4 }}><AlertTriangle size={15} style={{ verticalAlign: "-2px" }} /> Failing pods</div>
          {sm.failing.map((f) => {
            const [ref, reason] = f.split(": ");
            const [ns, name] = ref.split("/");
            return (
              <div key={f} className="small">
                <Link to={podPath(s.id, ns, name)} className="mono">{ref}</Link> — {reason}
              </div>
            );
          })}
        </div>
      )}
      {!data.metrics_available && (
        <div className="banner small">CPU and memory usage need metrics-server (included with k3s by default). Without it, usage columns stay empty.</div>
      )}

      <div className="row">
        <h2 className="grow" style={{ margin: 0 }}>Nodes</h2>
        <button className="icon-btn" onClick={reload} aria-label="Refresh">{loading ? <Spinner /> : <RotateCcw size={18} />}</button>
      </div>
      <div className="grid grid-2">
        {data.nodes.map((n) => {
          const cpuPct = n.cpu_used != null && n.cpu_capacity ? (n.cpu_used / n.cpu_capacity) * 100 : null;
          const memPct = n.mem_used != null && n.mem_capacity ? (n.mem_used / n.mem_capacity) * 100 : null;
          return (
            <div key={n.name} className="card card-pad">
              <div className="row" style={{ marginBottom: 12 }}>
                <div className="grow" style={{ minWidth: 0 }}>
                  <div className="row wrap" style={{ gap: 8 }}>
                    <Link className="mono" style={{ fontWeight: 700 }} to={`/servers/${s.id}/k8s-nodes/${encodeURIComponent(n.name)}`}>{n.name}</Link>
                    <span className={`chip ${n.ready ? "good" : "crit"}`}>{n.ready ? "Ready" : "Not ready"}</span>
                    {n.unschedulable && <span className="chip warn">Cordoned</span>}
                    {n.roles.map((r) => <span key={r} className="chip outline">{r}</span>)}
                    {n.problems.map((p) => <span key={p} className="chip crit">{p}</span>)}
                  </div>
                  <div className="small faint" style={{ marginTop: 4 }}>{n.ip} · {n.version} · {n.os} · {n.runtime}</div>
                </div>
                {can("k8s.control") && (
                  <Menu trigger={<MoreVertical size={17} />}>
                    <button onClick={() => cordon(n)}>{n.unschedulable ? <Play size={17} /> : <Pause size={17} />} {n.unschedulable ? "Uncordon" : "Cordon"}</button>
                  </Menu>
                )}
              </div>
              <NodeBar icon={<Cpu size={14} />} label="CPU" pct={cpuPct} text={`${cores(n.cpu_used)} of ${cores(n.cpu_capacity)}`} />
              <NodeBar icon={<Layers size={14} />} label="Memory" pct={memPct} text={`${n.mem_used != null ? bytes(n.mem_used) : "—"} of ${bytes(n.mem_capacity)}`} />
              <NodeBar icon={<Boxes size={14} />} label="Pods" pct={n.pods_capacity ? (n.pods / n.pods_capacity) * 100 : null} text={`${n.pods} of ${n.pods_capacity}`} />
            </div>
          );
        })}
      </div>
      <div className="small faint">Credentials: {data.source}</div>
    </div>
  );
}

function NodeBar({ icon, label, pct, text }: { icon: ReactNode; label: string; pct: number | null; text: string }) {
  return (
    <div style={{ marginTop: 10 }}>
      <div className="row small" style={{ marginBottom: 4 }}>
        <span className="muted row" style={{ gap: 6 }}>{icon} {label}</span>
        <span className="spacer" />
        <span className="tnum">{pct != null ? `${pct.toFixed(0)}% · ` : ""}{text}</span>
      </div>
      <Meter value={pct ?? 0} />
    </div>
  );
}

// ------------------------------------------------------------------ workloads

function Workloads({ s, ns }: { s: Server; ns: string }) {
  const { can } = useAuth();
  const confirm = useConfirm();
  const nav = useNavigate();
  const [q, setQ] = useState("");
  const [scaling, setScaling] = useState<Workload | null>(null);
  const [hist, setHist] = useState<Workload | null>(null);
  const { data, error, loading, reload } = usePoll(() => rpc<Workload[]>(s.id, { op: "k8s_workloads", namespace: ns || undefined }), [s.id, ns], 10_000);
  const list = useMemo(() => {
    const n = q.trim().toLowerCase();
    return (data ?? [])
      .filter((w) => !n || w.name.toLowerCase().includes(n) || w.namespace.includes(n) || w.images.some((i) => i.toLowerCase().includes(n)))
      .sort((a, b) => Number(b.degraded) - Number(a.degraded) || a.namespace.localeCompare(b.namespace) || a.name.localeCompare(b.name));
  }, [data, q]);
  if (error && !data) return <ErrorBanner error={error} onRetry={reload} />;
  if (!data) return <Loading />;
  const restart = (w: Workload) =>
    confirm({
      title: `Restart ${w.name}?`,
      body: `Rolling restart of ${KIND_LABEL[w.kind]} ${w.namespace}/${w.name} (like kubectl rollout restart). Pods are replaced one by one.`,
      confirm: "Restart",
      action: () => rpc(s.id, { op: "k8s_rollout_restart", kind: w.kind, namespace: w.namespace, name: w.name }).finally(reload),
      success: `${w.name} is restarting`,
    });
  return (
    <div>
      <Toolbar onRefresh={reload} loading={loading}>
        <SearchBox value={q} onChange={setQ} placeholder="Filter by name or image" />
      </Toolbar>
      <div className="card table-wrap">
        {list.length === 0 ? (
          <Empty icon={<Workflow size={26} />} title="No workloads" />
        ) : (
          <Sorted rows={list} acc={{ name: w => w.name, kind: w => w.kind, ready: w => w.desired ? w.ready / w.desired : 1, updated: w => w.updated, images: w => w.images[0], created: w => w.created }}>{(S) => (
<table className="table">
            <thead>
              <tr>
                {S.th("name", "Name")}
                {S.th("kind", "Kind")}
                {S.th("ready", "Ready")}
                {S.th("updated", "Up to date", "num")}
                {S.th("images", "Images")}
                {S.th("created", "Age")}
                <th />
              </tr>
            </thead>
            <tbody>
              {S.rows.map((w) => (
                <tr key={`${w.kind}/${w.namespace}/${w.name}`} style={{ cursor: "pointer" }} onClick={() => nav(`?tab=k8s&k=pods&q=${encodeURIComponent(w.name)}`)}>
                  <td>
                    <div style={{ fontWeight: 500 }}>{w.name}</div>
                    <div className="small faint">{w.namespace}</div>
                  </td>
                  <td><span className="chip outline">{KIND_LABEL[w.kind]}</span></td>
                  <td><span className={`chip ${w.degraded ? "warn" : w.desired === 0 ? "" : "good"}`}>{w.ready}/{w.desired}</span></td>
                  <td className="num tnum">{w.updated}</td>
                  <td style={{ maxWidth: 320 }}>{w.images.map((i) => <div key={i} className="small mono ellipsis" title={i}>{i}</div>)}</td>
                  <td className="small muted nowrap">{ago(w.created)}{w.restarted_at ? <div className="faint">restarted {ago(w.restarted_at)}</div> : null}</td>
                  <td className="actions" onClick={(e) => e.stopPropagation()}>
                    <div className="row" style={{ gap: 0, justifyContent: "flex-end" }}>
                      <button className="icon-btn sm" title="CPU / memory history" aria-label="History" onClick={() => setHist(w)}><History size={17} /></button>
                    {can("k8s.control") && (
                      <div className="row" style={{ gap: 0, justifyContent: "flex-end" }}>
                        <button className="icon-btn sm" title="Rolling restart" aria-label="Rolling restart" onClick={() => restart(w)}><RotateCcw size={17} /></button>
                        {w.kind !== "daemon_set" && (
                          <button className="icon-btn sm" title="Scale" aria-label="Scale" onClick={() => setScaling(w)}><Scale size={17} /></button>
                        )}
                      </div>
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
      {scaling && <ScaleDialog s={s} w={scaling} onClose={() => setScaling(null)} onDone={() => { setScaling(null); reload(); }} />}
      {hist && (
        <Dialog title={`${hist.namespace}/${hist.name} · all pods`} onClose={() => setHist(null)} wide>
          <UsageHistory serverId={s.id} kind="pod" group={`${hist.namespace}/${hist.name}`} />
        </Dialog>
      )}
    </div>
  );
}

function ScaleDialog({ s, w, onClose, onDone }: { s: Server; w: Workload; onClose: () => void; onDone: () => void }) {
  const [n, setN] = useState(String(w.desired));
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const toast = useToast();
  const v = Number(n);
  const valid = Number.isInteger(v) && v >= 0 && v <= 500;
  const save = async () => {
    setBusy(true);
    try {
      await rpc(s.id, { op: "k8s_scale", kind: w.kind, namespace: w.namespace, name: w.name, replicas: v });
      toast(`${w.name} scaled to ${v}`);
      onDone();
    } catch (e: any) {
      setErr(e.message);
      setBusy(false);
    }
  };
  return (
    <Dialog
      title={`Scale ${w.name}`}
      onClose={() => !busy && onClose()}
      footer={
        <>
          <button className="btn" onClick={onClose} disabled={busy}>Cancel</button>
          <button className={`btn ${v === 0 ? "danger" : "filled"}`} onClick={save} disabled={busy || !valid || v === w.desired}>{busy && <Spinner />} Scale to {valid ? v : "…"}</button>
        </>
      }
    >
      <div className="field">
        <label htmlFor="rep">Replicas</label>
        <input id="rep" className="input" type="number" min={0} max={500} autoFocus value={n} onChange={(e) => setN(e.target.value)} onKeyDown={(e) => e.key === "Enter" && valid && save()} />
        <span className="hint">Currently {w.desired} desired, {w.ready} ready.{v === 0 ? " Scaling to 0 stops every pod of this workload." : ""}</span>
      </div>
      {err && <div className="banner crit small">{err}</div>}
    </Dialog>
  );
}

// ------------------------------------------------------------------ pods

function Pods({ s, ns }: { s: Server; ns: string }) {
  const { can } = useAuth();
  const confirm = useConfirm();
  const nav = useNavigate();
  const [params] = useSearchParams();
  const [q, setQ] = useState(params.get("q") ?? "");
  const [f, setF] = useState<"all" | "failing" | "restarting">("all");
  const [sort, setSort] = useState<"name" | "cpu" | "mem" | "restarts">("name");
  const { data, error, loading, reload } = usePoll(() => rpc<{ pods: Pod[]; metrics_available: boolean }>(s.id, { op: "k8s_pods", namespace: ns || undefined }), [s.id, ns], 5_000);
  const pods = data?.pods ?? [];
  const list = useMemo(() => {
    const n = q.trim().toLowerCase();
    const key = (p: Pod) => (sort === "cpu" ? p.cpu ?? -1 : sort === "mem" ? p.mem ?? -1 : sort === "restarts" ? p.restarts : 0);
    return pods
      .filter((p) => (f === "failing" ? p.failing : f === "restarting" ? p.restarts > 0 : true))
      .filter((p) => !n || p.name.toLowerCase().includes(n) || p.namespace.includes(n) || p.node.includes(n) || p.status.toLowerCase().includes(n))
      .sort((a, b) => (sort === "name" ? Number(b.failing) - Number(a.failing) || a.namespace.localeCompare(b.namespace) || a.name.localeCompare(b.name) : key(b) - key(a)));
  }, [pods, q, f, sort]);
  if (error && !data) return <ErrorBanner error={error} onRetry={reload} />;
  if (!data) return <Loading />;
  const failing = pods.filter((p) => p.failing).length;
  const restarting = pods.filter((p) => p.restarts > 0).length;
  const del = (p: Pod) =>
    confirm({
      title: `Delete pod ${p.name}?`,
      body: p.owner ? `${p.owner.kind} ${p.owner.name} will create a replacement pod (this is how pods are restarted).` : "This pod is not managed by a controller and will NOT be recreated.",
      confirm: "Delete pod",
      danger: true,
      option: "Force (skip the graceful shutdown period)",
      action: (force) => rpc(s.id, { op: "k8s_pod_delete", namespace: p.namespace, name: p.name, force }).finally(reload),
      success: `${p.name} deleted`,
    });
  const th = (k: typeof sort, label: string, num = false) => (
    <th className={`sortable ${sort === k ? "sorted" : ""} ${num ? "num" : ""}`} onClick={() => setSort(sort === k && k !== "name" ? "name" : k)}>{label}</th>
  );
  return (
    <div>
      <Toolbar onRefresh={reload} loading={loading}>
        <SearchBox value={q} onChange={setQ} placeholder="Filter by name, node or status" />
        <Seg value={f} onChange={setF} options={[{ value: "all", label: `All ${pods.length}` }, { value: "failing", label: `Failing ${failing}` }, { value: "restarting", label: `Restarted ${restarting}` }]} />
      </Toolbar>
      <div className="card table-wrap">
        {list.length === 0 ? (
          <Empty icon={f === "failing" ? <CircleCheck size={26} /> : <Boxes size={26} />} title={f === "failing" ? "No failing pods" : "No pods match"} />
        ) : (
          <table className="table">
            <thead>
              <tr>
                {th("name", "Name")}
                <th>Status</th>
                <th>Ready</th>
                {th("restarts", "Restarts", true)}
                {th("cpu", "CPU", true)}
                {th("mem", "Memory", true)}
                <th>Node</th>
                <th>Age</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {list.map((p) => (
                <tr key={`${p.namespace}/${p.name}`} style={{ cursor: "pointer" }} onClick={() => nav(podPath(s.id, p.namespace, p.name))}>
                  <td style={{ maxWidth: 340 }}>
                    <Link to={podPath(s.id, p.namespace, p.name)} className="ellipsis" style={{ display: "block", fontWeight: 500, color: "var(--text)" }} onClick={(e) => e.stopPropagation()}>{p.name}</Link>
                    <div className="small faint">{p.namespace}{p.owner ? ` · ${p.owner.kind}` : ""}</div>
                  </td>
                  <td><PodStatus p={p} />{p.oom_killed && <span className="chip crit" style={{ marginLeft: 4 }} title="A container was killed for using more memory than its limit">OOMKilled</span>}</td>
                  <td className="tnum small">{p.ready}</td>
                  <td className="num tnum">
                    <span style={{ color: p.restarts > 0 ? "var(--warn-text)" : undefined }}>{p.restarts}</span>
                    {p.last_restart ? <div className="small faint">{ago(p.last_restart)}</div> : null}
                  </td>
                  <td className="num tnum small"><UsageCell used={data.metrics_available ? p.cpu : null} req={p.cpu_req} lim={p.cpu_lim} fmt={cores} /></td>
                  <td className="num tnum small"><UsageCell used={data.metrics_available ? p.mem : null} req={p.mem_req} lim={p.mem_lim} fmt={(v) => bytes(v)} /></td>
                  <td className="small mono">{p.node}</td>
                  <td className="small muted nowrap">{ago(p.created)}</td>
                  <td className="actions" onClick={(e) => e.stopPropagation()}>
                    <div className="row" style={{ gap: 0, justifyContent: "flex-end" }}>
                      {can("logs.view") && <Link className="icon-btn sm" to={podPath(s.id, p.namespace, p.name, "logs")} title="Logs" aria-label="Logs"><FileText size={17} /></Link>}
                      {can("k8s.exec") && p.phase === "Running" && <Link className="icon-btn sm" to={podPath(s.id, p.namespace, p.name, "terminal")} title="Terminal" aria-label="Terminal"><SquareTerminal size={17} /></Link>}
                      {can("k8s.control") && <button className="icon-btn sm" title="Delete pod (restart)" aria-label="Delete pod" onClick={() => del(p)}><Trash2 size={17} /></button>}
                    </div>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
    </div>
  );
}

// ------------------------------------------------------------------ services & events

function Services({ s, ns }: { s: Server; ns: string }) {
  const [q, setQ] = useState("");
  const { data, error, loading, reload } = usePoll(() => rpc<Service[]>(s.id, { op: "k8s_services", namespace: ns || undefined }), [s.id, ns], 30_000);
  if (error && !data) return <ErrorBanner error={error} onRetry={reload} />;
  if (!data) return <Loading />;
  const n = q.trim().toLowerCase();
  const list = data.filter((v) => !n || v.name.toLowerCase().includes(n) || v.namespace.includes(n)).sort((a, b) => a.namespace.localeCompare(b.namespace) || a.name.localeCompare(b.name));
  return (
    <div>
      <Toolbar onRefresh={reload} loading={loading}>
        <SearchBox value={q} onChange={setQ} placeholder="Filter services" />
      </Toolbar>
      <div className="card table-wrap">
        <Sorted rows={list} acc={{ name: v => v.name, type: v => v.type, ip: v => v.cluster_ip, ext: v => v.external[0], created: v => v.created }}>{(S) => (
<table className="table">
          <thead>
            <tr>{S.th("name", "Name")}{S.th("type", "Type")}{S.th("ip", "Cluster IP")}{S.th("ext", "External")}<th>Ports</th>{S.th("created", "Age")}</tr>
          </thead>
          <tbody>
            {S.rows.map((v) => (
              <tr key={`${v.namespace}/${v.name}`}>
                <td><div style={{ fontWeight: 500 }}>{v.name}</div><div className="small faint">{v.namespace}</div></td>
                <td><span className={`chip ${v.type === "LoadBalancer" || v.type === "NodePort" ? "info" : "outline"}`}>{v.type}</span></td>
                <td className="mono small">{v.cluster_ip}</td>
                <td className="mono small">{v.external.join(", ") || "—"}</td>
                <td className="mono small">{v.ports.join(", ")}</td>
                <td className="small muted nowrap">{ago(v.created)}</td>
              </tr>
            ))}
          </tbody>
        </table>
)}</Sorted>
      </div>
    </div>
  );
}

function Events({ s, ns }: { s: Server; ns: string }) {
  const [warn, setWarn] = useState<"all" | "warning">("warning");
  const [q, setQ] = useState("");
  const { data, error, loading, reload } = usePoll(() => rpc<KEvent[]>(s.id, { op: "k8s_events", namespace: ns || undefined }), [s.id, ns], 10_000);
  if (error && !data) return <ErrorBanner error={error} onRetry={reload} />;
  if (!data) return <Loading />;
  const n = q.trim().toLowerCase();
  const list = data.filter((e) => (warn === "warning" ? e.type === "Warning" : true)).filter((e) => !n || `${e.object} ${e.reason} ${e.message}`.toLowerCase().includes(n));
  const warnings = data.filter((e) => e.type === "Warning").length;
  return (
    <div>
      <Toolbar onRefresh={reload} loading={loading}>
        <SearchBox value={q} onChange={setQ} placeholder="Filter events" />
        <Seg value={warn} onChange={setWarn} options={[{ value: "warning", label: `Warnings ${warnings}` }, { value: "all", label: `All ${data.length}` }]} />
      </Toolbar>
      <div className="card table-wrap">
        {list.length === 0 ? (
          <Empty icon={<CircleCheck size={26} />} title={warn === "warning" ? "No warning events" : "No events"}>Kubernetes keeps events for about an hour.</Empty>
        ) : (
          <table className="table">
            <thead>
              <tr><th>When</th><th>Type</th><th>Object</th><th>Reason</th><th>Message</th></tr>
            </thead>
            <tbody>
              {list.map((e, i) => (
                <tr key={i}>
                  <td className="small muted nowrap">{ago(e.last)}{e.count > 1 ? <div className="faint">×{e.count}</div> : null}</td>
                  <td><span className={`chip ${e.type === "Warning" ? "warn" : "outline"}`}>{e.type}</span></td>
                  <td className="small">
                    {e.kind === "Pod" ? <Link className="mono" to={podPath(s.id, e.namespace, e.name)}>{e.object}</Link> : <span className="mono">{e.object}</span>}
                    <div className="faint">{e.namespace}</div>
                  </td>
                  <td className="small" style={{ fontWeight: 500 }}>{e.reason}</td>
                  <td className="small" style={{ maxWidth: 520, overflowWrap: "anywhere" }}>{e.message}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
    </div>
  );
}

// ------------------------------------------------------------------ usage vs requests / limits

/** Usage with a bar against the limit (or the request when there is no limit), like Lens. */
export function UsageCell({ used, req, lim, fmt }: { used: number | null | undefined; req?: number; lim?: number; fmt: (v: number) => string }) {
  if (used == null) return <>—</>;
  const ref = lim || req || 0;
  const pct = ref ? (used / ref) * 100 : null;
  const cls = pct == null ? "" : pct >= 90 ? "crit" : pct >= 75 ? "warn" : "";
  return (
    <div style={{ minWidth: 92 }} title={`request ${req ? fmt(req) : "none"} · limit ${lim ? fmt(lim) : "none"}`}>
      <div>{fmt(used)}</div>
      {pct != null && (
        <>
          <div className={`meter thin ${cls}`} style={{ marginTop: 3 }}><span style={{ width: `${Math.min(100, pct)}%` }} /></div>
          <div className="faint" style={{ fontSize: 11 }}>{pct.toFixed(0)}% of {lim ? "limit" : "request"}</div>
        </>
      )}
    </div>
  );
}

// ------------------------------------------------------------------ namespaces

interface NsUsage {
  name: string;
  phase: string;
  pods: number;
  running: number;
  failing: number;
  restarts: number;
  cpu: number;
  mem: number;
  cpu_req: number;
  cpu_lim: number;
  mem_req: number;
  mem_lim: number;
}

function Namespaces({ s, onOpen }: { s: Server; onOpen: (ns: string) => void }) {
  const [hist, setHist] = useState<string | null>(null);
  const { data, error, loading, reload } = usePoll(() => rpc<{ namespaces: NsUsage[]; metrics_available: boolean }>(s.id, { op: "k8s_namespace_usage" }), [s.id], 15_000);
  if (error && !data) return <ErrorBanner error={error} onRetry={reload} />;
  if (!data) return <Loading />;
  const list = [...data.namespaces].sort((a, b) => b.cpu - a.cpu || a.name.localeCompare(b.name));
  const total = list.reduce((a, n) => ({ cpu: a.cpu + n.cpu, mem: a.mem + n.mem }), { cpu: 0, mem: 0 });
  return (
    <div>
      <Toolbar onRefresh={reload} loading={loading}>
        <span className="small muted">Usage now: {cores(total.cpu)} CPU · {bytes(total.mem)} memory across {list.length} namespaces</span>
      </Toolbar>
      <div className="card table-wrap">
        <Sorted rows={list} acc={{ name: n => n.name, pods: n => n.pods, restarts: n => n.restarts, cpu: n => n.cpu, cpu_req: n => n.cpu_req, mem: n => n.mem, mem_req: n => n.mem_req }}>{(S) => (
<table className="table">
          <thead>
            <tr>{S.th("name", "Namespace")}{S.th("pods", "Pods")}{S.th("restarts", "Restarts", "num")}{S.th("cpu", "CPU", "num")}{S.th("cpu_req", "CPU requests / limits", "num")}{S.th("mem", "Memory", "num")}{S.th("mem_req", "Memory requests / limits", "num")}<th /></tr>
          </thead>
          <tbody>
            {S.rows.map((n) => (
              <tr key={n.name} style={{ cursor: "pointer" }} onClick={() => onOpen(n.name)}>
                <td style={{ fontWeight: 600 }}>{n.name}{n.phase !== "Active" && <span className="chip warn" style={{ marginLeft: 6 }}>{n.phase}</span>}</td>
                <td className="small">
                  <span className={`chip ${n.failing ? "crit" : n.running === n.pods ? "good" : ""}`}>{n.running}/{n.pods} running</span>
                  {n.failing > 0 && <span className="small" style={{ color: "var(--crit-text)", marginLeft: 6 }}>{n.failing} failing</span>}
                </td>
                <td className="num tnum small">{n.restarts}</td>
                <td className="num tnum small">{data.metrics_available ? cores(n.cpu) : "—"}</td>
                <td className="num tnum small faint">{cores(n.cpu_req)} / {n.cpu_lim ? cores(n.cpu_lim) : "—"}</td>
                <td className="num tnum small">{data.metrics_available ? bytes(n.mem) : "—"}</td>
                <td className="num tnum small faint">{bytes(n.mem_req)} / {n.mem_lim ? bytes(n.mem_lim) : "—"}</td>
                <td className="actions" onClick={(e) => e.stopPropagation()}>
                  <button className="icon-btn sm" title="CPU / memory history" aria-label="History" onClick={() => setHist(n.name)}><History size={17} /></button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
)}</Sorted>
      </div>
      {hist && (
        <Dialog title={`Namespace ${hist} · all pods`} onClose={() => setHist(null)} wide>
          <UsageHistory serverId={s.id} kind="pod" group={`${hist}/`} />
        </Dialog>
      )}
    </div>
  );
}

// ------------------------------------------------------------------ jobs & cronjobs

function Jobs({ s, ns }: { s: Server; ns: string }) {
  const { can } = useAuth();
  const confirm = useConfirm();
  const cron = usePoll(() => rpc<any[]>(s.id, { op: "k8s_resources", resource: "cronjobs", namespace: ns || undefined }), [s.id, ns], 30_000);
  const jobs = usePoll(() => rpc<any[]>(s.id, { op: "k8s_resources", resource: "jobs", namespace: ns || undefined }), [s.id, ns], 15_000);
  if ((cron.error && !cron.data) || (jobs.error && !jobs.data)) return <ErrorBanner error={(cron.error ?? jobs.error)!} onRetry={() => { cron.reload(); jobs.reload(); }} />;
  if (!cron.data || !jobs.data) return <Loading />;
  const dur = (ms: number | null) => (ms == null ? "—" : ms < 60_000 ? `${Math.round(ms / 1000)} s` : `${Math.round(ms / 60_000)} min`);
  return (
    <div className="col" style={{ gap: 16 }}>
      <div className="card table-wrap">
        <div className="card-head"><h3>CronJobs</h3></div>
        {cron.data.length === 0 ? <div className="card-body small muted">No CronJobs.</div> : (
          <table className="table">
            <thead><tr><th>Name</th><th>Schedule</th><th>Active</th><th>Last run</th><th>Last success</th><th /></tr></thead>
            <tbody>
              {cron.data.map((c) => (
                <tr key={`${c.namespace}/${c.name}`}>
                  <td><div style={{ fontWeight: 500 }}>{c.name}</div><div className="small faint">{c.namespace}</div></td>
                  <td className="mono small">{c.schedule}{c.timezone ? ` (${c.timezone})` : ""}{c.suspended && <span className="chip warn" style={{ marginLeft: 6 }}>Suspended</span>}</td>
                  <td className="small tnum">{c.active}</td>
                  <td className="small muted">{c.last_schedule ? ago(c.last_schedule) : "never"}</td>
                  <td className="small muted">{c.last_success ? ago(c.last_success) : "—"}</td>
                  <td className="actions">
                    {can("k8s.control") && (
                      <button
                        className="btn sm"
                        onClick={() => confirm({ title: `Run ${c.name} now?`, body: "Creates a Job from the CronJob's template, like kubectl create job --from=cronjob.", confirm: "Run now", action: () => rpc(s.id, { op: "k8s_cronjob_trigger", namespace: c.namespace, name: c.name }).then(() => jobs.reload()), success: `${c.name} started` })}
                      >
                        <PlayIcon size={15} /> Run now
                      </button>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
      <div className="card table-wrap">
        <div className="card-head"><h3>Jobs</h3></div>
        {jobs.data.length === 0 ? <div className="card-body small muted">No Jobs.</div> : (
          <table className="table">
            <thead><tr><th>Name</th><th>Status</th><th>Completions</th><th className="num">Failed pods</th><th>Duration</th><th>Started</th></tr></thead>
            <tbody>
              {[...jobs.data].sort((a, b) => b.created - a.created).map((j) => (
                <tr key={`${j.namespace}/${j.name}`}>
                  <td><Link to={`?tab=k8s&k=pods&q=${encodeURIComponent(j.name)}`} style={{ fontWeight: 500 }}>{j.name}</Link><div className="small faint">{j.namespace}{j.owner ? ` · from ${j.owner}` : ""}</div></td>
                  <td><span className={`chip ${j.failing ? "crit" : j.status === "Complete" ? "good" : "warn"}`}>{j.status}</span></td>
                  <td className="tnum small">{j.completions}</td>
                  <td className="num tnum small">{j.failed}</td>
                  <td className="small">{dur(j.duration_ms)}</td>
                  <td className="small muted">{ago(j.created)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
    </div>
  );
}

// ------------------------------------------------------------------ ingress, storage, config

function Ingresses({ s, ns }: { s: Server; ns: string }) {
  const { data, error, reload } = usePoll(() => rpc<any[]>(s.id, { op: "k8s_resources", resource: "ingresses", namespace: ns || undefined }), [s.id, ns], 30_000);
  if (error && !data) return <ErrorBanner error={error} onRetry={reload} />;
  if (!data) return <Loading />;
  return (
    <div className="card table-wrap">
      <div className="card-head"><h3>Ingresses</h3></div>
      {data.length === 0 ? <div className="card-body small muted">No Ingresses.</div> : (
        <table className="table">
          <thead><tr><th>Name</th><th>Class</th><th>Rules</th><th>TLS</th><th>Address</th><th>Age</th></tr></thead>
          <tbody>
            {data.map((i) => (
              <tr key={`${i.namespace}/${i.name}`}>
                <td><div style={{ fontWeight: 500 }}>{i.name}</div><div className="small faint">{i.namespace}</div></td>
                <td className="small">{i.class || "—"}</td>
                <td className="small mono">
                  {i.rules.map((r: any, k: number) => (
                    <div key={k}>
                      {r.host !== "*" ? <a href={`${i.tls.includes(r.host) ? "https" : "http"}://${r.host}${r.path || "/"}`} target="_blank" rel="noreferrer">{r.host}{r.path}</a> : `*${r.path}`} → {r.service}
                    </div>
                  ))}
                </td>
                <td className="small">{i.tls.length ? <span className="chip good"><Lock size={12} /> {i.tls.length}</span> : "—"}</td>
                <td className="small mono">{i.address.join(", ") || "—"}</td>
                <td className="small muted nowrap">{ago(i.created)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}

function Storage({ s, ns }: { s: Server; ns: string }) {
  const { data, error, loading, reload } = usePoll(() => rpc<any[]>(s.id, { op: "k8s_resources", resource: "pvcs", namespace: ns || undefined }), [s.id, ns], 30_000);
  if (error && !data) return <ErrorBanner error={error} onRetry={reload} />;
  if (!data) return <Loading />;
  return (
    <div>
      <Toolbar onRefresh={reload} loading={loading}><span className="small muted">Persistent volume claims</span></Toolbar>
      <div className="card table-wrap">
        {data.length === 0 ? <Empty icon={<HardDriveIcon size={26} />} title="No persistent volume claims" /> : (
          <table className="table">
            <thead><tr><th>Claim</th><th>Status</th><th className="num">Capacity</th><th>Storage class</th><th>Access</th><th>Volume</th><th>Age</th></tr></thead>
            <tbody>
              {data.map((v) => (
                <tr key={`${v.namespace}/${v.name}`}>
                  <td><div style={{ fontWeight: 500 }}>{v.name}</div><div className="small faint">{v.namespace}</div></td>
                  <td><span className={`chip ${v.status === "Bound" ? "good" : v.status === "Pending" ? "warn" : "crit"}`}>{v.status}</span></td>
                  <td className="num tnum small">{bytes(v.capacity || v.requested)}</td>
                  <td className="small">{v.class || "—"}</td>
                  <td className="small">{(v.modes ?? []).join(", ")}</td>
                  <td className="small mono ellipsis" style={{ maxWidth: 220 }}>{v.volume || "—"}</td>
                  <td className="small muted nowrap">{ago(v.created)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
    </div>
  );
}

function ConfigRes({ s, ns }: { s: Server; ns: string }) {
  const [kind, setKind] = useState<"configmaps" | "secrets">("configmaps");
  const [q, setQ] = useState("");
  const { data, error, loading, reload } = usePoll(() => rpc<any[]>(s.id, { op: "k8s_resources", resource: kind, namespace: ns || undefined }), [s.id, ns, kind], 30_000);
  const n = q.trim().toLowerCase();
  return (
    <div>
      <Toolbar onRefresh={reload} loading={loading}>
        <Seg value={kind} onChange={setKind} options={[{ value: "configmaps", label: "ConfigMaps" }, { value: "secrets", label: "Secrets" }]} />
        <SearchBox value={q} onChange={setQ} placeholder="Filter by name or key" />
      </Toolbar>
      {error && !data ? <ErrorBanner error={error} onRetry={reload} /> : !data ? <Loading /> : (
        <div className="card table-wrap">
          <table className="table">
            <thead><tr><th>Name</th>{kind === "secrets" && <th>Type</th>}<th>Keys</th><th>Age</th></tr></thead>
            <tbody>
              {data.filter((c) => !n || c.name.toLowerCase().includes(n) || c.keys.some((k: string) => k.toLowerCase().includes(n))).map((c) => (
                <tr key={`${c.namespace}/${c.name}`}>
                  <td><div style={{ fontWeight: 500 }}>{c.name}</div><div className="small faint">{c.namespace}</div></td>
                  {kind === "secrets" && <td className="small mono">{c.type}</td>}
                  <td className="small">{c.keys.slice(0, 12).map((k: string) => <span key={k} className="chip outline mono" style={{ marginRight: 4, marginBottom: 2, fontSize: 11 }}>{k}</span>)}{c.keys.length > 12 && <span className="faint">+{c.keys.length - 12}</span>}</td>
                  <td className="small muted nowrap">{ago(c.created)}</td>
                </tr>
              ))}
            </tbody>
          </table>
          {kind === "secrets" && <div className="card-body small faint">Only key names are shown — secret values never leave the server.</div>}
        </div>
      )}
    </div>
  );
}
