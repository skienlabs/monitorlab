import { Link, useNavigate, useParams } from "react-router-dom";
import { ChevronRight, Cpu, Layers, Boxes } from "lucide-react";
import { api, rpc } from "../api";
import type { Server } from "../types";
import { ErrorBanner, Loading, Meter, usePoll } from "../components/ui";
import { ago, bytes, dateTime } from "../format";
import { cores, podPath, PodStatus, UsageCell } from "./K8sTab";

interface NodeDetail {
  name: string;
  ready: boolean;
  roles: string[];
  unschedulable: boolean;
  taints: { key: string; value?: string; effect: string }[];
  labels: Record<string, string>;
  conditions: { type: string; status: string; reason: string; message: string; since: number }[];
  addresses: { type: string; address: string }[];
  info: Record<string, string>;
  capacity: { cpu: number; mem: number; pods: number; storage: number };
  allocatable: { cpu: number; mem: number; pods: number };
  allocated: { cpu_req: number; cpu_lim: number; mem_req: number; mem_lim: number };
  usage: { cpu: number; mem: number } | null;
  images: number;
  created: number;
  pods: { namespace: string; name: string; status: string; failing: boolean; phase: string; restarts: number; cpu: number | null; mem: number | null; cpu_req: number; cpu_lim: number; mem_req: number; mem_lim: number; created: number }[];
}

function Alloc({ icon, label, used, req, lim, cap, fmt }: { icon: React.ReactNode; label: string; used: number | null; req: number; lim: number; cap: number; fmt: (v: number) => string }) {
  const p = (v: number) => (cap ? (v / cap) * 100 : 0);
  return (
    <div className="card card-pad">
      <div className="row small" style={{ marginBottom: 10, gap: 6 }}>{icon}<b>{label}</b><span className="spacer" /><span className="muted">allocatable {fmt(cap)}</span></div>
      {[["Used", used], ["Requests", req], ["Limits", lim]].map(([k, v]) => (
        <div key={k as string} style={{ marginBottom: 8 }}>
          <div className="row small"><span className="muted">{k as string}</span><span className="spacer" /><span className="tnum">{v == null ? "—" : `${fmt(v as number)} · ${p(v as number).toFixed(0)}%`}</span></div>
          <Meter value={v == null ? 0 : Math.min(100, p(v as number))} />
        </div>
      ))}
      {lim > cap && <div className="small" style={{ color: "var(--warn-text)" }}>Limits are overcommitted ({p(lim).toFixed(0)}% of allocatable).</div>}
    </div>
  );
}

export function NodePage() {
  const { id: serverId = "", node = "" } = useParams();
  const nav = useNavigate();
  const server = usePoll(() => api<Server>(`/servers/${serverId}`), [serverId], 30_000);
  const { data: n, error, reload } = usePoll(() => rpc<NodeDetail>(serverId, { op: "k8s_node_detail", name: node }), [serverId, node], 10_000);
  if (error && !n) return <div className="page"><ErrorBanner error={error} onRetry={reload} /></div>;
  if (!n) return <Loading />;
  const pods = [...n.pods].sort((a, b) => (b.cpu ?? 0) - (a.cpu ?? 0));
  return (
    <div className="page">
      <div className="page-head">
        <div className="grow">
          <div className="crumbs">
            <Link to="/">Overview</Link><ChevronRight size={14} />
            <Link to={`/servers/${serverId}`}>{server.data?.name ?? "Server"}</Link><ChevronRight size={14} />
            <Link to={`/servers/${serverId}?tab=k8s`}>Kubernetes</Link><ChevronRight size={14} />
            <span>{node}</span>
          </div>
          <div className="row wrap" style={{ gap: 10 }}>
            <h1 className="mono">{n.name}</h1>
            <span className={`chip ${n.ready ? "good" : "crit"}`}>{n.ready ? "Ready" : "Not ready"}</span>
            {n.unschedulable && <span className="chip warn">Cordoned</span>}
            {n.roles.map((r) => <span key={r} className="chip outline">{r}</span>)}
          </div>
          <div className="sub small">{n.info.osImage} · {n.info.kubeletVersion} · {n.info.containerRuntimeVersion} · {n.addresses.map((a) => a.address).join(", ")} · created {dateTime(n.created)}</div>
        </div>
      </div>
      <div className="grid grid-3" style={{ marginBottom: 16 }}>
        <Alloc icon={<Cpu size={15} />} label="CPU" used={n.usage?.cpu ?? null} req={n.allocated.cpu_req} lim={n.allocated.cpu_lim} cap={n.allocatable.cpu} fmt={cores} />
        <Alloc icon={<Layers size={15} />} label="Memory" used={n.usage?.mem ?? null} req={n.allocated.mem_req} lim={n.allocated.mem_lim} cap={n.allocatable.mem} fmt={(v) => bytes(v)} />
        <div className="card card-pad">
          <div className="row small" style={{ marginBottom: 10, gap: 6 }}><Boxes size={15} /><b>Pods</b><span className="spacer" /><span className="muted">max {n.allocatable.pods}</span></div>
          <div className="row small"><span className="muted">Scheduled</span><span className="spacer" /><span className="tnum">{n.pods.length}</span></div>
          <Meter value={n.allocatable.pods ? (n.pods.length / n.allocatable.pods) * 100 : 0} />
          <div className="small muted" style={{ marginTop: 10 }}>{n.images} images on the node · ephemeral storage {bytes(n.capacity.storage)}</div>
          {n.taints.length > 0 && <div className="small" style={{ marginTop: 8 }}>Taints: {n.taints.map((t) => <span key={t.key} className="chip outline mono" style={{ fontSize: 11, marginRight: 4 }}>{t.key}{t.value ? `=${t.value}` : ""}:{t.effect}</span>)}</div>}
        </div>
      </div>
      <div className="card" style={{ marginBottom: 16 }}>
        <div className="card-head"><h3>Conditions</h3></div>
        <div className="card-body col" style={{ gap: 6 }}>
          {n.conditions.map((c) => {
            const bad = c.type === "Ready" ? c.status !== "True" : c.status === "True";
            return (
              <div key={c.type} className="row small" style={{ gap: 8 }}>
                <span className={`chip ${bad ? "crit" : "good"}`}>{c.type}</span>
                <span className="muted">{c.reason} — {c.message}</span>
                <span className="spacer" /><span className="faint">{ago(c.since)}</span>
              </div>
            );
          })}
        </div>
      </div>
      <div className="card table-wrap">
        <div className="card-head"><h3>Pods on this node</h3></div>
        <table className="table">
          <thead><tr><th>Pod</th><th>Status</th><th className="num">Restarts</th><th className="num">CPU</th><th className="num">Memory</th><th>Age</th></tr></thead>
          <tbody>
            {pods.map((p) => (
              <tr key={`${p.namespace}/${p.name}`} style={{ cursor: "pointer" }} onClick={() => nav(podPath(serverId, p.namespace, p.name))}>
                <td><div style={{ fontWeight: 500 }}>{p.name}</div><div className="small faint">{p.namespace}</div></td>
                <td><PodStatus p={p} /></td>
                <td className="num tnum small">{p.restarts}</td>
                <td className="num tnum small"><UsageCell used={p.cpu} req={p.cpu_req} lim={p.cpu_lim} fmt={cores} /></td>
                <td className="num tnum small"><UsageCell used={p.mem} req={p.mem_req} lim={p.mem_lim} fmt={(v) => bytes(v)} /></td>
                <td className="small muted nowrap">{ago(p.created)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}
