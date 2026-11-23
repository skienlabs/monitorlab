import { ReactNode, useMemo, useState } from "react";
import { Link, useParams, useSearchParams } from "react-router-dom";
import { Box, ChevronRight, Database, Globe, HardDrive, Layers, Network, RefreshCw, Sparkles, Activity, Cpu } from "lucide-react";
import { api, rpc } from "../api";
import { useAuth } from "../auth";
import type { Server } from "../types";
import { ErrorBanner, Loading, Meter, Seg, Spinner, StatusChip, useLocalStorage, usePoll } from "../components/ui";
import { TimeChart } from "../components/Chart";
import { OsIcon } from "../components/OsIcon";
import { bytes, dateTime, duration, pct, rate, ratio } from "../format";
import { ContainersTab, ImagesTab, NetworksTab, CleanupTab, VolumesTab } from "./DockerTabs";
import { MssqlTab } from "./MssqlTab";
import { IisTab } from "./IisTab";

type Range = "15m" | "1h" | "6h" | "24h" | "7d" | "30d";
const RANGES: Range[] = ["15m", "1h", "6h", "24h", "7d", "30d"];

interface MetricsResp {
  ts: number[];
  series: Record<string, (number | null)[]>;
}

export function ChartCard({ title, now, children }: { title: string; now?: ReactNode; children: ReactNode }) {
  return (
    <div className="card chart-card">
      <div className="chart-head">
        <h3>{title}</h3>
        {now != null && <div className="now">{now}</div>}
      </div>
      {children}
    </div>
  );
}

const pctFmt = (v: number) => `${Math.round(v)}%`;
const bytesFmt = (v: number) => bytes(v, 0);
const rateFmt = (v: number) => rate(v);

function OverviewTab({ s }: { s: Server }) {
  const [range, setRange] = useLocalStorage<Range>("vigil.range", "1h");
  const long = range === "7d" || range === "30d";
  const { data } = usePoll(() => api<MetricsResp>(`/servers/${s.id}/metrics?range=${range}`), [s.id, range], long ? 60_000 : 10_000);
  const m = s.latest;
  const h = s.hello;
  const ts = data?.ts ?? [];
  const ser = data?.series ?? {};
  const col = (k: string) => ser[k] ?? [];
  const hasMssql = !!m?.mssql;
  const hasIis = !!m?.iis;

  return (
    <div className="col" style={{ gap: 16 }}>
      <div className="grid grid-4">
        <div className="card kpi">
          <div className="label">
            <Cpu size={16} /> CPU
          </div>
          <div className="value">{pct(m?.cpu)}</div>
          <Meter value={m?.cpu ?? 0} warn={85} crit={95} />
          <div className="foot">
            {h?.cpu_cores} logical cores{m?.load ? ` · load ${m.load.map((l) => l.toFixed(2)).join(" ")}` : ""}
          </div>
        </div>
        <div className="card kpi">
          <div className="label">
            <Layers size={16} /> Memory
          </div>
          <div className="value">
            {m ? bytes(m.mem_used) : "—"}
            <small> / {m ? bytes(m.mem_total) : "—"}</small>
          </div>
          <Meter value={m ? ratio(m.mem_used, m.mem_total) : 0} warn={90} crit={95} />
          <div className="foot">{m && m.swap_total > 0 ? `Swap ${bytes(m.swap_used)} of ${bytes(m.swap_total)}` : "No swap"}</div>
        </div>
        <div className="card kpi">
          <div className="label">
            <Network size={16} /> Network
          </div>
          <div className="value" style={{ fontSize: 22 }}>
            ↓ {rate(m?.net_rx_bps)}
          </div>
          <div className="value" style={{ fontSize: 22 }}>
            ↑ {rate(m?.net_tx_bps)}
          </div>
          <div className="foot">Physical interfaces only</div>
        </div>
        <div className="card kpi">
          <div className="label">
            <Activity size={16} /> Uptime
          </div>
          <div className="value">{duration(m?.uptime)}</div>
          <div className="foot">{m?.process_count} processes · since {h ? dateTime(h.boot_time * 1000) : "—"}</div>
        </div>
      </div>

      <div className="row">
        <h2 className="grow">History</h2>
        <Seg<Range> value={range} onChange={setRange} options={RANGES.map((r) => ({ value: r, label: r }))} />
      </div>

      <div className="grid grid-2">
        <ChartCard title="CPU usage" now={pct(m?.cpu)}>
          <TimeChart ts={ts} series={[{ label: "CPU", values: col("cpu"), color: "--series-1" }]} fmt={pctFmt} yMax={100} />
        </ChartCard>
        <ChartCard title="Memory usage" now={m ? pct(ratio(m.mem_used, m.mem_total)) : "—"}>
          <TimeChart ts={ts} series={[{ label: "Memory", values: col("mem_pct"), color: "--series-1" }]} fmt={pctFmt} yMax={100} />
        </ChartCard>
        <ChartCard title="Network throughput">
          <TimeChart
            ts={ts}
            series={[
              { label: "Received", values: col("net_rx"), color: "--series-1" },
              { label: "Sent", values: col("net_tx"), color: "--series-2" },
            ]}
            fmt={rateFmt}
            minTop={1024}
          />
        </ChartCard>
        <ChartCard title="Disk I/O">
          <TimeChart
            ts={ts}
            series={[
              { label: "Read", values: col("disk_read"), color: "--series-1" },
              { label: "Write", values: col("disk_write"), color: "--series-2" },
            ]}
            fmt={rateFmt}
            minTop={1024}
          />
        </ChartCard>
        {hasMssql && (
          <>
            <ChartCard title="SQL Server CPU" now={pct(m?.mssql?.instances.reduce((a, i) => a + i.cpu, 0))}>
              <TimeChart ts={ts} series={[{ label: "SQL Server", values: col("mssql_cpu"), color: "--series-1" }]} fmt={pctFmt} yMax={100} />
            </ChartCard>
            <ChartCard title="SQL Server memory" now={bytes(m?.mssql?.instances.reduce((a, i) => a + i.mem, 0))}>
              <TimeChart ts={ts} series={[{ label: "SQL Server", values: col("mssql_mem"), color: "--series-1" }]} fmt={bytesFmt} minTop={1048576} />
            </ChartCard>
          </>
        )}
        {hasIis && (
          <>
            <ChartCard title="IIS worker CPU" now={pct(m?.iis?.worker_cpu)}>
              <TimeChart ts={ts} series={[{ label: "w3wp", values: col("iis_cpu"), color: "--series-1" }]} fmt={pctFmt} yMax={100} />
            </ChartCard>
            <ChartCard title="IIS worker memory" now={bytes(m?.iis?.worker_mem)}>
              <TimeChart ts={ts} series={[{ label: "w3wp", values: col("iis_mem"), color: "--series-1" }]} fmt={bytesFmt} minTop={1048576} />
            </ChartCard>
          </>
        )}
      </div>

      <div className="grid grid-2">
        <div className="card">
          <div className="card-head">
            <HardDrive size={18} />
            <h3>Disks</h3>
          </div>
          <div className="card-body col" style={{ gap: 14 }}>
            {(m?.disks ?? []).length === 0 && <div className="muted small">No disk data yet.</div>}
            {m?.disks.map((d) => {
              const p = ratio(d.used, d.total);
              return (
                <div key={d.device + d.mount} className="col" style={{ gap: 6 }}>
                  <div className="row small">
                    <b className="mono" style={{ fontSize: 13 }}>{d.mount}</b>
                    <span className="faint ellipsis">
                      {d.device} · {d.fs}
                    </span>
                    <span className="spacer" />
                    <span className="tnum">
                      {bytes(d.used)} / {bytes(d.total)}
                    </span>
                    <b className="tnum" style={{ width: 44, textAlign: "right" }}>{pct(p)}</b>
                  </div>
                  <Meter value={p} warn={85} crit={95} lg />
                </div>
              );
            })}
          </div>
        </div>
        <div className="card">
          <div className="card-head">
            <Cpu size={18} />
            <h3>CPU cores</h3>
            <span className="spacer" />
            <span className="small muted">live, per logical core</span>
          </div>
          <div className="card-body">
            <div className="core-bars">
              {(m?.cpu_cores ?? []).map((c, i) => (
                <div key={i} title={`Core ${i}: ${pct(c)}`}>
                  <span style={{ height: `${Math.min(100, c)}%` }} />
                </div>
              ))}
            </div>
            <hr className="divider" style={{ margin: "16px 0" }} />
            <dl className="kv">
              <dt>Host</dt>
              <dd>{h?.hostname}</dd>
              <dt>OS</dt>
              <dd>
                {h?.os_name} {h?.os_version && `(${h.os_version})`}
              </dd>
              <dt>Kernel</dt>
              <dd>{h?.kernel || "—"}</dd>
              <dt>CPU</dt>
              <dd>
                {h?.cpu_model} · {h?.physical_cores ?? "?"} cores / {h?.cpu_cores} threads · {h?.arch}
              </dd>
              <dt>IP addresses</dt>
              <dd className="mono">{h?.ips.join(", ") || "—"}</dd>
              <dt>Agent</dt>
              <dd>
                v{h?.agent_version} · reports every {h?.interval_secs}s{s.remote && s.remote !== "local" ? ` · from ${s.remote}` : ""}
              </dd>
            </dl>
          </div>
        </div>
      </div>

      <ProcessesCard id={s.id} online={s.online} />
    </div>
  );
}

function ProcessesCard({ id, online }: { id: string; online: boolean }) {
  const [by, setBy] = useState<"cpu" | "mem">("cpu");
  const [loaded, setLoaded] = useState(false);
  const { data, loading, error, reload } = usePoll(() => (loaded && online ? rpc<any>(id, { op: "system_detail" }) : Promise.resolve(null)), [id, loaded, online]);
  const rows: any[] = (by === "cpu" ? data?.top_cpu : data?.top_mem) ?? [];
  return (
    <div className="card">
      <div className="card-head">
        <h3 className="grow">Top processes</h3>
        {loaded && <Seg value={by} onChange={setBy} options={[{ value: "cpu", label: "By CPU" }, { value: "mem", label: "By memory" }]} />}
        {loaded ? (
          <button className="icon-btn sm" onClick={reload} aria-label="Refresh" disabled={loading}>
            {loading ? <Spinner /> : <RefreshCw size={16} />}
          </button>
        ) : (
          <button className="btn tonal sm" onClick={() => setLoaded(true)} disabled={!online}>
            Load processes
          </button>
        )}
      </div>
      <div className="card-body">
        {!loaded && <div className="small muted">Process lists are fetched on demand to keep the agent light.</div>}
        {error && <ErrorBanner error={error} />}
        {loaded && loading && !data && <Loading label="Sampling processes…" />}
        {rows.length > 0 && (
          <div className="table-wrap">
            <table className="table">
              <thead>
                <tr>
                  <th>Process</th>
                  <th>PID</th>
                  <th>User</th>
                  <th className="num">CPU</th>
                  <th className="num">Memory</th>
                </tr>
              </thead>
              <tbody>
                {rows.map((p) => (
                  <tr key={p.pid}>
                    <td className="mono">{p.name}</td>
                    <td className="tnum muted">{p.pid}</td>
                    <td className="muted">{p.user || "—"}</td>
                    <td className="num">{pct(p.cpu, 1)}</td>
                    <td className="num">{bytes(p.mem)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </div>
    </div>
  );
}

export function ServerPage() {
  const { id = "" } = useParams();
  const [params, setParams] = useSearchParams();
  const { can } = useAuth();
  const { data: s, error, reload } = usePoll(() => api<Server>(`/servers/${id}`), [id], 10_000);

  const tabs = useMemo(() => {
    const caps = s?.hello?.capabilities;
    const t: { key: string; label: string; icon: ReactNode; show: boolean }[] = [
      { key: "overview", label: "Overview", icon: <Activity size={17} />, show: true },
      { key: "containers", label: "Containers", icon: <Box size={17} />, show: !!caps?.docker && can("docker.view") },
      { key: "images", label: "Images", icon: <Layers size={17} />, show: !!caps?.docker && can("docker.view") },
      { key: "volumes", label: "Volumes", icon: <HardDrive size={17} />, show: !!caps?.docker && can("docker.view") },
      { key: "networks", label: "Networks", icon: <Network size={17} />, show: !!caps?.docker && can("docker.view") },
      { key: "cleanup", label: "Storage & cleanup", icon: <Sparkles size={17} />, show: !!caps?.docker && can("docker.view") },
      { key: "mssql", label: "SQL Server", icon: <Database size={17} />, show: !!caps?.mssql && can("mssql.view") },
      { key: "iis", label: "IIS", icon: <Globe size={17} />, show: !!caps?.iis && can("iis.view") },
    ];
    return t.filter((x) => x.show);
  }, [s, can]);

  if (error && !s) return <div className="page"><ErrorBanner error={error} onRetry={reload} /></div>;
  if (!s) return <Loading />;

  const tab = tabs.some((t) => t.key === params.get("tab")) ? params.get("tab")! : "overview";
  const setTab = (k: string) => setParams(k === "overview" ? {} : { tab: k }, { replace: true });
  const m = s.latest;

  return (
    <div className="page">
      <div className="page-head" style={{ paddingBottom: 8 }}>
        <div className="grow">
          <div className="crumbs">
            <Link to="/">Overview</Link>
            <ChevronRight size={14} />
            <span>{s.name}</span>
          </div>
          <div className="row" style={{ gap: 14 }}>
            <div className="os" style={{ width: 44, height: 44, borderRadius: 14, background: "var(--surface-2)", display: "grid", placeItems: "center", color: "var(--text-2)" }}>
              <OsIcon os={s.hello?.os} size={22} />
            </div>
            <div>
              <h1>{s.name}</h1>
              <div className="sub small">
                {s.hello ? `${s.hello.hostname} · ${s.hello.os_name}` : "No agent data yet"}
                {m?.docker && ` · ${m.docker.running} of ${m.docker.total} containers running`}
              </div>
            </div>
            <StatusChip status={s.status} />
          </div>
        </div>
      </div>

      {!s.online && (
        <div className="banner warn" style={{ marginBottom: 16 }}>
          {s.enrolled
            ? `The agent is offline (last seen ${s.last_seen_at ? dateTime(s.last_seen_at) : "never"}). Charts show stored history; live actions are unavailable.`
            : "This server has not enrolled yet. Run the installer shown in Servers & agents."}
        </div>
      )}
      {s.reasons.length > 0 && (
        <div className={`banner ${s.status === "critical" ? "crit" : "warn"}`} style={{ marginBottom: 16 }}>
          {s.reasons.join(" · ")}
        </div>
      )}

      <div className="tabs" role="tablist">
        {tabs.map((t) => (
          <button key={t.key} className={tab === t.key ? "on" : ""} onClick={() => setTab(t.key)} role="tab" aria-selected={tab === t.key}>
            {t.icon}
            {t.label}
            {t.key === "containers" && m?.docker && <span className="badge">{m.docker.total}</span>}
          </button>
        ))}
      </div>

      {tab === "overview" && <OverviewTab s={s} />}
      {tab === "containers" && <ContainersTab s={s} />}
      {tab === "images" && <ImagesTab s={s} />}
      {tab === "volumes" && <VolumesTab s={s} />}
      {tab === "networks" && <NetworksTab s={s} />}
      {tab === "cleanup" && <CleanupTab s={s} />}
      {tab === "mssql" && <MssqlTab s={s} />}
      {tab === "iis" && <IisTab s={s} />}
    </div>
  );
}
