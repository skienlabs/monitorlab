import { useEffect, useMemo, useState } from "react";
import { Link, useNavigate, useParams, useSearchParams } from "react-router-dom";
import { Activity, ChevronRight, CirclePlay, CircleStop, Eye, EyeOff, FileText, Info, RotateCcw, SquareTerminal, Trash2 } from "lucide-react";
import { api, openStream, rpc } from "../api";
import { useAuth } from "../auth";
import type { Server } from "../types";
import { CodeBlock, Empty, ErrorBanner, Loading, StateChip, usePoll } from "../components/ui";
import { LogViewer } from "../components/LogViewer";
import { ContainerTerminal } from "../components/Terminal";
import { TimeChart } from "../components/Chart";
import { ChartCard } from "./ServerPage";
import { useContainerActions } from "./DockerTabs";
import { bytes, dateTime, pct, rate, shortId } from "../format";

interface Sample {
  ts: number;
  cpu: number;
  mem_used: number;
  mem_limit: number;
  net_rx: number;
  net_tx: number;
  blk_read: number;
  blk_write: number;
  pids: number;
}

function StatsTab({ serverId, id }: { serverId: string; id: string }) {
  const [samples, setSamples] = useState<Sample[]>([]);
  const [err, setErr] = useState<string | null>(null);
  useEffect(() => {
    setSamples([]);
    const c = openStream(serverId, { kind: "container_stats", id }, {
      onData: (d) => d.k === "stats" && setSamples((s) => [...s.slice(-299), d as Sample]),
      onEnd: (e) => setErr(e ?? "Stats stream ended (container stopped?)"),
    });
    return () => c.close();
  }, [serverId, id]);

  const view = useMemo(() => {
    const ts = samples.map((s) => s.ts / 1000);
    const rateOf = (k: keyof Sample) =>
      samples.map((s, i) => {
        if (i === 0) return null;
        const p = samples[i - 1];
        const dt = (s.ts - p.ts) / 1000;
        return dt > 0 ? Math.max(0, ((s[k] as number) - (p[k] as number)) / dt) : null;
      });
    return {
      ts,
      cpu: samples.map((s) => s.cpu),
      mem: samples.map((s) => s.mem_used),
      rx: rateOf("net_rx"),
      tx: rateOf("net_tx"),
      br: rateOf("blk_read"),
      bw: rateOf("blk_write"),
    };
  }, [samples]);
  const last = samples[samples.length - 1];

  return (
    <div className="col" style={{ gap: 16 }}>
      {err && <div className="banner warn">{err}</div>}
      <div className="grid grid-4">
        <div className="card kpi">
          <div className="label">CPU</div>
          <div className="value">{last ? pct(last.cpu, 1) : "—"}</div>
          <div className="foot">100% = one full core</div>
        </div>
        <div className="card kpi">
          <div className="label">Memory</div>
          <div className="value">{last ? bytes(last.mem_used) : "—"}</div>
          <div className="foot">limit {last ? bytes(last.mem_limit) : "—"}</div>
        </div>
        <div className="card kpi">
          <div className="label">Network total</div>
          <div className="value" style={{ fontSize: 20 }}>↓ {last ? bytes(last.net_rx) : "—"}</div>
          <div className="value" style={{ fontSize: 20 }}>↑ {last ? bytes(last.net_tx) : "—"}</div>
        </div>
        <div className="card kpi">
          <div className="label">Processes</div>
          <div className="value">{last?.pids ?? "—"}</div>
          <div className="foot">Block I/O {last ? `${bytes(last.blk_read)} read / ${bytes(last.blk_write)} written` : "—"}</div>
        </div>
      </div>
      <div className="grid grid-2">
        <ChartCard title="CPU">
          <TimeChart live ts={view.ts} series={[{ label: "CPU", values: view.cpu, color: "--series-1" }]} fmt={(v) => `${v.toFixed(v < 10 ? 1 : 0)}%`} minTop={5} empty="Collecting samples…" />
        </ChartCard>
        <ChartCard title="Memory">
          <TimeChart live ts={view.ts} series={[{ label: "Memory", values: view.mem, color: "--series-1" }]} fmt={(v) => bytes(v, 0)} minTop={1048576} empty="Collecting samples…" />
        </ChartCard>
        <ChartCard title="Network">
          <TimeChart live ts={view.ts} series={[{ label: "Received", values: view.rx, color: "--series-1" }, { label: "Sent", values: view.tx, color: "--series-2" }]} fmt={rate} minTop={1024} empty="Collecting samples…" />
        </ChartCard>
        <ChartCard title="Block I/O">
          <TimeChart live ts={view.ts} series={[{ label: "Read", values: view.br, color: "--series-1" }, { label: "Write", values: view.bw, color: "--series-2" }]} fmt={rate} minTop={1024} empty="Collecting samples…" />
        </ChartCard>
      </div>
    </div>
  );
}

function Env({ env }: { env: string[] }) {
  const [reveal, setReveal] = useState(false);
  return (
    <div className="card">
      <div className="card-head">
        <h3 className="grow">Environment</h3>
        <button className="btn sm" onClick={() => setReveal((r) => !r)}>
          {reveal ? <EyeOff size={15} /> : <Eye size={15} />} {reveal ? "Hide values" : "Reveal values"}
        </button>
      </div>
      <div className="card-body table-wrap">
        <table className="table">
          <tbody>
            {env.map((e, i) => {
              const idx = e.indexOf("=");
              const k = idx >= 0 ? e.slice(0, idx) : e;
              const v = idx >= 0 ? e.slice(idx + 1) : "";
              return (
                <tr key={i}>
                  <td className="mono small" style={{ width: "35%", wordBreak: "break-all" }}>{k}</td>
                  <td className="mono small" style={{ wordBreak: "break-all" }}>{reveal ? v : v ? "••••••••" : ""}</td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
    </div>
  );
}

function InspectTab({ data }: { data: any }) {
  const [raw, setRaw] = useState(false);
  const cfg = data.Config ?? {};
  const st = data.State ?? {};
  const hc = data.HostConfig ?? {};
  const ports = Object.entries((data.NetworkSettings?.Ports ?? {}) as Record<string, { HostIp: string; HostPort: string }[] | null>);
  const nets = Object.entries((data.NetworkSettings?.Networks ?? {}) as Record<string, any>);
  return (
    <div className="col" style={{ gap: 16 }}>
      <div className="grid grid-2">
        <div className="card">
          <div className="card-head"><h3>Container</h3></div>
          <div className="card-body">
            <dl className="kv">
              <dt>ID</dt><dd className="mono small">{data.Id}</dd>
              <dt>Image</dt><dd className="mono small">{cfg.Image}</dd>
              <dt>Command</dt><dd className="mono small">{[...(cfg.Entrypoint ?? []), ...(cfg.Cmd ?? [])].join(" ") || "—"}</dd>
              <dt>Working dir</dt><dd className="mono small">{cfg.WorkingDir || "—"}</dd>
              <dt>Created</dt><dd>{dateTime(Date.parse(data.Created))}</dd>
              <dt>Started</dt><dd>{st.StartedAt && !st.StartedAt.startsWith("0001") ? dateTime(Date.parse(st.StartedAt)) : "—"}</dd>
              <dt>Restart policy</dt><dd>{hc.RestartPolicy?.Name || "no"}{hc.RestartPolicy?.MaximumRetryCount ? ` (max ${hc.RestartPolicy.MaximumRetryCount})` : ""} · restarted {data.RestartCount ?? 0}×</dd>
              <dt>Exit code</dt><dd>{st.Running ? "running" : st.ExitCode}{st.OOMKilled ? " · OOM killed" : ""}{st.Error ? ` · ${st.Error}` : ""}</dd>
              <dt>Limits</dt><dd>{hc.Memory ? `memory ${bytes(hc.Memory)}` : "no memory limit"} · {hc.NanoCpus ? `${hc.NanoCpus / 1e9} CPUs` : "no CPU limit"}</dd>
            </dl>
          </div>
        </div>
        <div className="card">
          <div className="card-head"><h3>Networking</h3></div>
          <div className="card-body col" style={{ gap: 12 }}>
            {nets.map(([name, n]) => (
              <div key={name} className="row small">
                <b className="mono">{name}</b>
                <span className="mono muted">{n.IPAddress || "—"}</span>
                <span className="faint mono">{n.MacAddress}</span>
              </div>
            ))}
            <hr className="divider" />
            {ports.length === 0 && <div className="small muted">No exposed ports</div>}
            {ports.map(([p, b]) => (
              <div key={p} className="row small mono">
                <span>{p}</span>
                <span className="faint">→</span>
                <span>{b ? b.map((x) => `${x.HostIp || "0.0.0.0"}:${x.HostPort}`).join(", ") : "not published"}</span>
              </div>
            ))}
          </div>
        </div>
      </div>
      <div className="card">
        <div className="card-head"><h3>Mounts</h3></div>
        <div className="card-body table-wrap">
          {(data.Mounts ?? []).length === 0 ? (
            <div className="small muted">No mounts</div>
          ) : (
            <table className="table">
              <thead><tr><th>Type</th><th>Source</th><th>Destination</th><th>Mode</th></tr></thead>
              <tbody>
                {data.Mounts.map((m: any, i: number) => (
                  <tr key={i}>
                    <td className="small">{m.Type}</td>
                    <td className="mono small" style={{ wordBreak: "break-all" }}>{m.Name ?? m.Source}</td>
                    <td className="mono small">{m.Destination}</td>
                    <td className="small">{m.RW ? "read-write" : "read-only"}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>
      </div>
      {cfg.Env?.length > 0 && <Env env={cfg.Env} />}
      {st.Health && (
        <div className="card">
          <div className="card-head"><h3>Health checks</h3><StateChip state="running" health={st.Health.Status} /></div>
          <div className="card-body col">
            {(st.Health.Log ?? []).slice(-5).reverse().map((h: any, i: number) => (
              <div key={i} className="small">
                <span className="faint">{dateTime(Date.parse(h.Start))}</span> · exit {h.ExitCode} · <span className="mono">{String(h.Output ?? "").trim().slice(0, 300)}</span>
              </div>
            ))}
          </div>
        </div>
      )}
      <div className="card">
        <div className="card-head">
          <h3 className="grow">Labels</h3>
        </div>
        <div className="card-body table-wrap">
          <table className="table">
            <tbody>
              {Object.entries(cfg.Labels ?? {}).map(([k, v]) => (
                <tr key={k}><td className="mono small" style={{ width: "40%" }}>{k}</td><td className="mono small" style={{ wordBreak: "break-all" }}>{String(v)}</td></tr>
              ))}
            </tbody>
          </table>
        </div>
      </div>
      <div>
        <button className="btn" onClick={() => setRaw((r) => !r)}>{raw ? "Hide" : "Show"} raw JSON</button>
        {raw && <CodeBlock>{JSON.stringify({ ...data, Config: { ...cfg, Env: cfg.Env ? ["(hidden — see Environment)"] : undefined } }, null, 2)}</CodeBlock>}
      </div>
    </div>
  );
}

export function ContainerPage() {
  const { id: serverId = "", cid = "" } = useParams();
  const [params, setParams] = useSearchParams();
  const nav = useNavigate();
  const { can } = useAuth();
  const server = usePoll(() => api<Server>(`/servers/${serverId}`), [serverId], 15_000);
  const insp = usePoll(() => rpc<any>(serverId, { op: "docker_inspect", id: cid }), [serverId, cid], 5000);
  const actions = useContainerActions(serverId, insp.reload);

  const tabs = [
    { key: "logs", label: "Logs", icon: <FileText size={17} />, show: can("logs.view") },
    { key: "terminal", label: "Terminal", icon: <SquareTerminal size={17} />, show: can("containers.exec") },
    { key: "stats", label: "Live stats", icon: <Activity size={17} />, show: true },
    { key: "inspect", label: "Details", icon: <Info size={17} />, show: true },
  ].filter((t) => t.show);
  const tab = tabs.some((t) => t.key === params.get("tab")) ? params.get("tab")! : tabs[0].key;

  const d = insp.data;
  if (insp.error && !d) return <div className="page"><ErrorBanner error={insp.error} onRetry={insp.reload} /></div>;
  if (!d) return <Loading />;
  const name = String(d.Name ?? "").replace(/^\//, "") || shortId(d.Id);
  const state: string = d.State?.Status ?? "unknown";
  const running = state === "running";
  const c = { id: d.Id, name } as any;
  const s = server.data;

  return (
    <div className="page">
      <div className="page-head" style={{ paddingBottom: 8 }}>
        <div className="grow">
          <div className="crumbs">
            <Link to="/">Overview</Link>
            <ChevronRight size={14} />
            <Link to={`/servers/${serverId}`}>{s?.name ?? "Server"}</Link>
            <ChevronRight size={14} />
            <Link to={`/servers/${serverId}?tab=containers`}>Containers</Link>
            <ChevronRight size={14} />
            <span>{name}</span>
          </div>
          <div className="row" style={{ gap: 14 }}>
            <h1>{name}</h1>
            <StateChip state={state} health={d.State?.Health?.Status} />
          </div>
          <div className="sub small mono">{d.Config?.Image}</div>
        </div>
        {can("containers.control") && (
          <div className="row">
            {running ? (
              <button className="btn outlined" onClick={() => actions.stop(c)}><CircleStop size={17} /> Stop</button>
            ) : (
              <button className="btn outlined" onClick={() => actions.start(c)}><CirclePlay size={17} /> Start</button>
            )}
            <button className="btn tonal" onClick={() => actions.restart(c)}><RotateCcw size={17} /> Restart</button>
            {can("containers.delete") && (
              <button className="icon-btn danger" title="Delete container" aria-label="Delete container" onClick={() => actions.remove(c, () => nav(`/servers/${serverId}?tab=containers`))}>
                <Trash2 size={18} />
              </button>
            )}
          </div>
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

      {tab === "logs" && <LogViewer fileName={name} source={{ serverId, request: { kind: "container_logs", id: d.Id, follow: true }, reconnect: true, supportsSince: true }} />}
      {tab === "terminal" &&
        (running ? <ContainerTerminal serverId={serverId} containerId={d.Id} /> : <Empty icon={<SquareTerminal size={26} />} title="Container is not running">Start the container to open a terminal.</Empty>)}
      {tab === "stats" && (running ? <StatsTab serverId={serverId} id={d.Id} /> : <Empty icon={<Activity size={26} />} title="Container is not running" />)}
      {tab === "inspect" && <InspectTab data={d} />}
    </div>
  );
}
