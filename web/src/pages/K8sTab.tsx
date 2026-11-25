import { ReactNode, useMemo, useState } from "react";
import { Link, useNavigate, useSearchParams } from "react-router-dom";
import {
  AlertTriangle, Boxes, CircleCheck, Cpu, FileText, Layers, MoreVertical, Network, Pause, Play, RotateCcw, Scale, Server as ServerIcon,
  SquareTerminal, Trash2, Workflow,
} from "lucide-react";
import { rpc } from "../api";
import { useAuth } from "../auth";
import type { Server } from "../types";
import { Dialog, Empty, ErrorBanner, Loading, Menu, Meter, SearchBox, Seg, Spinner, useConfirm, useLocalStorage, usePoll, useToast } from "../components/ui";
import { ago, bytes, plural } from "../format";
import { Toolbar } from "./DockerTabs";

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

type Sub = "overview" | "workloads" | "pods" | "services" | "events";

export function KubernetesTab({ s }: { s: Server }) {
  const [params, setParams] = useSearchParams();
  const sub = (["overview", "workloads", "pods", "services", "events"].includes(params.get("k") ?? "") ? params.get("k") : "overview") as Sub;
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
            { value: "workloads", label: "Workloads" },
            { value: "pods", label: "Pods" },
            { value: "services", label: "Services" },
            { value: "events", label: "Events" },
          ]}
        />
        <span className="spacer" />
        {sub !== "overview" && (
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
      {sub === "services" && <Services s={s} ns={ns} />}
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
                    <b className="mono">{n.name}</b>
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
          <table className="table">
            <thead>
              <tr>
                <th>Name</th>
                <th>Kind</th>
                <th>Ready</th>
                <th className="num">Up to date</th>
                <th>Images</th>
                <th>Age</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {list.map((w) => (
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
                    {can("k8s.control") && (
                      <div className="row" style={{ gap: 0, justifyContent: "flex-end" }}>
                        <button className="icon-btn sm" title="Rolling restart" aria-label="Rolling restart" onClick={() => restart(w)}><RotateCcw size={17} /></button>
                        {w.kind !== "daemon_set" && (
                          <button className="icon-btn sm" title="Scale" aria-label="Scale" onClick={() => setScaling(w)}><Scale size={17} /></button>
                        )}
                      </div>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
      {scaling && <ScaleDialog s={s} w={scaling} onClose={() => setScaling(null)} onDone={() => { setScaling(null); reload(); }} />}
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
                  <td><PodStatus p={p} /></td>
                  <td className="tnum small">{p.ready}</td>
                  <td className="num tnum">
                    <span style={{ color: p.restarts > 0 ? "var(--warn-text)" : undefined }}>{p.restarts}</span>
                    {p.last_restart ? <div className="small faint">{ago(p.last_restart)}</div> : null}
                  </td>
                  <td className="num tnum small">{data.metrics_available ? cores(p.cpu) : "—"}</td>
                  <td className="num tnum small">{data.metrics_available && p.mem != null ? bytes(p.mem) : "—"}</td>
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
        <table className="table">
          <thead>
            <tr><th>Name</th><th>Type</th><th>Cluster IP</th><th>External</th><th>Ports</th><th>Age</th></tr>
          </thead>
          <tbody>
            {list.map((v) => (
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
