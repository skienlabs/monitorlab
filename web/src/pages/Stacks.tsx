import { useState } from "react";
import { Link, useNavigate, useParams, useSearchParams } from "react-router-dom";
import { ChevronRight, CirclePlay, CircleStop, CloudDownload, FileCode2, FileText, History, Layers, RefreshCcwDot, RotateCcw } from "lucide-react";
import { api, rpc } from "../api";
import { useAuth } from "../auth";
import type { Server } from "../types";
import { CodeBlock, Empty, ErrorBanner, Loading, SearchBox, Spinner, useConfirm, usePoll, useToast, Sorted } from "../components/ui";
import { LogViewer } from "../components/LogViewer";
import { UsageHistory } from "../components/UsageHistory";
import { bytes } from "../format";
import { Toolbar } from "./DockerTabs";

interface StackContainer {
  id: string;
  name: string;
  service: string | null;
  state: string;
  status: string;
  image: string;
  cpu: number;
  mem: number;
}
interface Stack {
  name: string;
  config_files: string;
  working_dir: string;
  containers: StackContainer[];
  running: number;
  total: number;
  cpu: number;
  mem: number;
}

const stackPath = (sid: string, p: string, tab?: string) => `/servers/${sid}/stacks/${encodeURIComponent(p)}${tab ? `?tab=${tab}` : ""}`;

export function StacksTab({ s }: { s: Server }) {
  const nav = useNavigate();
  const [q, setQ] = useState("");
  const { data, error, loading, reload } = usePoll(() => rpc<Stack[]>(s.id, { op: "docker_stacks" }), [s.id], 10_000);
  if (!s.online) return <Empty icon={<Layers size={26} />} title="Server is offline" />;
  if (error && !data) return <ErrorBanner error={error} onRetry={reload} />;
  if (!data) return <Loading />;
  const n = q.trim().toLowerCase();
  const list = data.filter((st) => !n || st.name.toLowerCase().includes(n) || st.containers.some((c) => c.name.toLowerCase().includes(n) || c.image.toLowerCase().includes(n)));
  return (
    <div>
      <Toolbar onRefresh={reload} loading={loading}>
        <SearchBox value={q} onChange={setQ} placeholder="Filter stacks, services or images" />
      </Toolbar>
      {list.length === 0 ? (
        <div className="card">
          <Empty icon={<Layers size={26} />} title="No compose stacks">Containers started with docker compose show up here, grouped by project.</Empty>
        </div>
      ) : (
        <div className="card table-wrap">
          <Sorted rows={list} acc={{ name: s => s.name, services: s => s.total, state: s => s.total ? s.running / s.total : 0, cpu: s => s.cpu, mem: s => s.mem }}>{(S) => (
<table className="table">
            <thead>
              <tr>
                {S.th("name", "Stack")}
                {S.th("services", "Services")}
                {S.th("state", "State")}
                {S.th("cpu", "CPU", "num")}
                {S.th("mem", "Memory", "num")}
                <th>Compose file</th>
              </tr>
            </thead>
            <tbody>
              {S.rows.map((st) => (
                <tr key={st.name} style={{ cursor: "pointer" }} onClick={() => nav(stackPath(s.id, st.name))}>
                  <td>
                    <Link to={stackPath(s.id, st.name)} style={{ fontWeight: 600, color: "var(--text)" }} onClick={(e) => e.stopPropagation()}>{st.name}</Link>
                  </td>
                  <td className="small" style={{ maxWidth: 360 }}>
                    {st.containers.map((c) => (
                      <span key={c.id} className={`chip ${c.state === "running" ? "outline" : "warn"}`} style={{ marginRight: 4, marginBottom: 2 }}>{c.service ?? c.name}</span>
                    ))}
                  </td>
                  <td><span className={`chip ${st.running === st.total ? "good" : st.running === 0 ? "crit" : "warn"}`}>{st.running}/{st.total} running</span></td>
                  <td className="num tnum small">{st.cpu.toFixed(1)}%</td>
                  <td className="num tnum small">{bytes(st.mem)}</td>
                  <td className="small mono faint ellipsis" style={{ maxWidth: 280 }} title={st.config_files}>{st.config_files || "—"}</td>
                </tr>
              ))}
            </tbody>
          </table>
)}</Sorted>
        </div>
      )}
    </div>
  );
}

interface StackFile {
  working_dir: string;
  files: { path: string; content?: string; error?: string }[];
  env_file: string | null;
  env_keys: string[] | null;
}

function ComposeFile({ serverId, project }: { serverId: string; project: string }) {
  const { data, error, reload } = usePoll(() => rpc<StackFile>(serverId, { op: "docker_stack_file", project }), [serverId, project]);
  if (error) return <ErrorBanner error={error} onRetry={reload} />;
  if (!data) return <Loading />;
  return (
    <div className="col" style={{ gap: 12 }}>
      <div className="small muted">
        Read-only view of the file the stack was started from (working directory <span className="mono">{data.working_dir}</span>). Edit it on the server or in your repository, then use{" "}
        <b>Pull &amp; redeploy</b> or run <span className="mono">docker compose up -d</span> there.
      </div>
      {data.files.map((f) => (
        <div key={f.path} className="card">
          <div className="card-head"><h3 className="mono" style={{ fontSize: 13 }}>{f.path}</h3></div>
          <div className="card-body">{f.error ? <div className="banner warn small">Cannot read the file: {f.error}</div> : <CodeBlock>{f.content ?? ""}</CodeBlock>}</div>
        </div>
      ))}
      {data.env_keys && (
        <div className="card card-pad">
          <h3 style={{ marginBottom: 8 }}><span className="mono">{data.env_file}</span></h3>
          <div className="small muted" style={{ marginBottom: 8 }}>Variable names only; values stay on the server.</div>
          <div className="row wrap" style={{ gap: 6 }}>{data.env_keys.map((k) => <span key={k} className="chip outline mono">{k}</span>)}</div>
        </div>
      )}
    </div>
  );
}

export function StackPage() {
  const { id: serverId = "", project = "" } = useParams();
  const [params, setParams] = useSearchParams();
  const { can } = useAuth();
  const confirm = useConfirm();
  const toast = useToast();
  const server = usePoll(() => api<Server>(`/servers/${serverId}`), [serverId], 30_000);
  const { data, error, reload } = usePoll(() => rpc<Stack[]>(serverId, { op: "docker_stacks" }), [serverId], 5_000);
  const [busy, setBusy] = useState<string | null>(null);
  const st = data?.find((x) => x.name === project);
  const tabs = [
    { key: "containers", label: "Containers", icon: <Layers size={17} />, show: true },
    { key: "logs", label: "Logs", icon: <FileText size={17} />, show: can("logs.view") },
    { key: "file", label: "Compose file", icon: <FileCode2 size={17} />, show: true },
    { key: "history", label: "History", icon: <History size={17} />, show: true },
  ].filter((t) => t.show);
  const tab = tabs.some((t) => t.key === params.get("tab")) ? params.get("tab")! : "containers";

  const run = (action: string, label: string, body: string, danger = false) =>
    confirm({
      title: `${label} ${project}?`,
      body,
      confirm: label,
      danger,
      action: async () => {
        setBusy(action);
        try {
          const r = await rpc<{ ok: boolean; failed: number; results: { container: string; ok: boolean; error?: string }[] }>(serverId, { op: "docker_stack_action", project, action });
          if (r.failed) toast(`${r.failed} container(s) failed: ${r.results.filter((x) => !x.ok).map((x) => `${x.container}: ${x.error}`).join("; ")}`, true);
          else toast(`${project}: ${label.toLowerCase()} done`);
        } finally {
          setBusy(null);
          reload();
        }
      },
    });

  if (error && !data) return <div className="page"><ErrorBanner error={error} onRetry={reload} /></div>;
  if (!data) return <Loading />;
  if (!st) return <div className="page"><Empty icon={<Layers size={26} />} title={`Stack ${project} not found`}><Link to={`/servers/${serverId}?tab=stacks`}>Back to stacks</Link></Empty></div>;

  return (
    <div className="page">
      <div className="page-head" style={{ paddingBottom: 8 }}>
        <div className="grow">
          <div className="crumbs">
            <Link to="/">Overview</Link>
            <ChevronRight size={14} />
            <Link to={`/servers/${serverId}`}>{server.data?.name ?? "Server"}</Link>
            <ChevronRight size={14} />
            <Link to={`/servers/${serverId}?tab=stacks`}>Stacks</Link>
            <ChevronRight size={14} />
            <span>{project}</span>
          </div>
          <div className="row wrap" style={{ gap: 14 }}>
            <h1>{project}</h1>
            <span className={`chip ${st.running === st.total ? "good" : st.running === 0 ? "crit" : "warn"}`}>{st.running}/{st.total} running</span>
          </div>
          <div className="sub small mono">{st.config_files}</div>
        </div>
      </div>

      {(can("containers.control") || can("containers.delete")) && (
        <div className="card action-bar" style={{ padding: 8, marginBottom: 16 }} role="toolbar" aria-label="Stack actions">
          {can("containers.control") && (
            <>
              <button className="btn sm go" disabled={!!busy || st.running === st.total} onClick={() => run("start", "Start", "Starts every stopped container of the stack.")}>{busy === "start" ? <Spinner /> : <CirclePlay size={16} />} Start</button>
              <button className="btn sm" disabled={!!busy || st.running === 0} onClick={() => run("stop", "Stop", "Stops every running container of the stack.", true)}>{busy === "stop" ? <Spinner /> : <CircleStop size={16} />} Stop</button>
              <button className="btn sm" disabled={!!busy} onClick={() => run("restart", "Restart", "Restarts every container of the stack.")}>{busy === "restart" ? <Spinner /> : <RotateCcw size={16} />} Restart</button>
            </>
          )}
          {can("containers.delete") && (
            <>
              <span className="sep" />
              <button className="btn sm" disabled={!!busy} onClick={() => run("pull_redeploy", "Pull & redeploy", "Pulls the latest image of every service, then recreates each container with its current settings, networks and volumes (like docker compose pull && docker compose up -d). Containers whose image did not change are recreated too.")}>
                {busy === "pull_redeploy" ? <Spinner /> : <CloudDownload size={16} />} Pull &amp; redeploy
              </button>
              <button className="btn sm" disabled={!!busy} onClick={() => run("redeploy", "Redeploy", "Recreates every container with its current settings, without pulling images.")}>
                {busy === "redeploy" ? <Spinner /> : <RefreshCcwDot size={16} />} Redeploy
              </button>
            </>
          )}
          {busy && <span className="small muted">Working… this can take a while for large images.</span>}
        </div>
      )}

      <div className="tabs" role="tablist">
        {tabs.map((t) => (
          <button key={t.key} className={tab === t.key ? "on" : ""} onClick={() => setParams({ tab: t.key }, { replace: true })} role="tab" aria-selected={tab === t.key}>
            {t.icon}
            {t.label}
          </button>
        ))}
      </div>

      {tab === "containers" && (
        <div className="card table-wrap">
          <table className="table">
            <thead><tr><th>Container</th><th>Service</th><th>Image</th><th>State</th><th className="num">CPU</th><th className="num">Memory</th></tr></thead>
            <tbody>
              {st.containers.map((c) => (
                <tr key={c.id}>
                  <td><Link to={`/servers/${serverId}/containers/${encodeURIComponent(c.name)}`} style={{ fontWeight: 500 }}>{c.name}</Link></td>
                  <td className="small">{c.service ?? "—"}</td>
                  <td className="small mono ellipsis" style={{ maxWidth: 320 }} title={c.image}>{c.image}</td>
                  <td><span className={`chip ${c.state === "running" ? "good" : "warn"}`}>{c.state}</span><div className="small faint">{c.status}</div></td>
                  <td className="num tnum small">{c.state === "running" ? `${c.cpu.toFixed(1)}%` : "—"}</td>
                  <td className="num tnum small">{c.state === "running" ? bytes(c.mem) : "—"}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      {tab === "logs" && (
        <LogViewer fileName={`${project}-stack`} source={{ serverId, request: { kind: "stack_logs", project }, reconnect: true, supportsSince: true }} />
      )}
      {tab === "file" && <ComposeFile serverId={serverId} project={project} />}
      {tab === "history" && <UsageHistory serverId={serverId} kind="container" group={project} />}
    </div>
  );
}
