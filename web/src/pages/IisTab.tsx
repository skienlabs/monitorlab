import { useState } from "react";
import { CirclePlay, CircleStop, Globe, RefreshCw, RotateCcw } from "lucide-react";
import { rpc } from "../api";
import { useAuth } from "../auth";
import type { Server } from "../types";
import { Empty, ErrorBanner, Loading, Seg, Spinner, StateChip, useConfirm, usePoll, useToast } from "../components/ui";
import { LogViewer } from "../components/LogViewer";
import { bytes, pct } from "../format";

interface Site {
  id: number;
  name: string;
  state: string;
  bindings: string[];
  physical_path: string | null;
  pools: string[];
  log_dir: string;
  apps: { path: string; pool: string }[];
}
interface Pool {
  name: string;
  state: string;
  runtime: string;
  pipeline: string;
  worker_pids: number[];
  cpu: number;
  mem: number;
}

export function IisTab({ s }: { s: Server }) {
  const { can } = useAuth();
  const confirm = useConfirm();
  const toast = useToast();
  const [view, setView] = useState<"sites" | "pools" | "logs">("sites");
  const sites = usePoll(() => rpc<Site[]>(s.id, { op: "iis_sites" }), [s.id], 15_000);
  const pools = usePoll(() => rpc<Pool[]>(s.id, { op: "iis_app_pools" }), [s.id], 15_000);
  const [logSite, setLogSite] = useState<number | null>(null);

  if (!s.online) return <Empty icon={<Globe size={26} />} title="Server is offline" />;
  if (!sites.data && !sites.error) return <Loading />;

  const siteAction = (site: Site, action: "start" | "stop" | "restart") => {
    const run = () => rpc(s.id, { op: "iis_site_action", name: site.name, action }).finally(sites.reload);
    if (action === "start") {
      run().then(() => toast(`${site.name} started`)).catch((e) => toast(e.message, true));
      return;
    }
    confirm({
      title: `${action === "stop" ? "Stop" : "Restart"} site ${site.name}?`,
      body: action === "stop" ? "The site stops accepting requests until it is started again." : "The site is stopped and started; in-flight requests may fail.",
      confirm: action === "stop" ? "Stop site" : "Restart site",
      action: run,
      success: `${site.name}: ${action === "stop" ? "stopped" : "restarted"}`,
    });
  };
  const poolAction = (p: Pool, action: "start" | "stop" | "recycle") => {
    const run = () => rpc(s.id, { op: "iis_app_pool_action", name: p.name, action }).finally(pools.reload);
    if (action === "start") {
      run().then(() => toast(`${p.name} started`)).catch((e) => toast(e.message, true));
      return;
    }
    confirm({
      title: `${action === "recycle" ? "Recycle" : "Stop"} application pool ${p.name}?`,
      body: action === "recycle" ? "IIS starts a new worker process and drains the old one (overlapped recycle). Session state held in memory is lost." : "All sites using this pool return 503 until it is started.",
      confirm: action === "recycle" ? "Recycle" : "Stop pool",
      action: run,
      success: `${p.name}: ${action === "recycle" ? "recycled" : "stopped"}`,
    });
  };

  const selectedSite = logSite ?? sites.data?.[0]?.id ?? null;

  return (
    <div className="col" style={{ gap: 16 }}>
      <div className="row">
        <Seg value={view} onChange={setView} options={[{ value: "sites", label: `Sites ${sites.data?.length ?? ""}` }, { value: "pools", label: `Application pools ${pools.data?.length ?? ""}` }, ...(can("logs.view") ? [{ value: "logs" as const, label: "Logs" }] : [])]} />
        <span className="spacer" />
        <button className="icon-btn" onClick={() => { sites.reload(); pools.reload(); }} aria-label="Refresh">
          {sites.loading || pools.loading ? <Spinner /> : <RefreshCw size={18} />}
        </button>
      </div>
      {sites.error && <ErrorBanner error={sites.error} onRetry={sites.reload} />}

      {view === "sites" && (
        <div className="card table-wrap">
          <table className="table">
            <thead><tr><th>Site</th><th>State</th><th>Bindings</th><th>Application pool</th><th>Physical path</th><th /></tr></thead>
            <tbody>
              {(sites.data ?? []).map((site) => (
                <tr key={site.id}>
                  <td><div style={{ fontWeight: 500 }}>{site.name}</div><div className="small faint">ID {site.id}</div></td>
                  <td><StateChip state={site.state} /></td>
                  <td className="mono small">{site.bindings.map((b) => <div key={b}>{b}</div>)}</td>
                  <td className="small">{site.pools.join(", ")}</td>
                  <td className="mono small ellipsis" style={{ maxWidth: 260 }} title={site.physical_path ?? ""}>{site.physical_path ?? "—"}</td>
                  <td className="actions">
                    {can("iis.control") && (
                      <>
                        {site.state.toLowerCase() === "started" ? (
                          <button className="icon-btn sm" title="Stop" aria-label="Stop site" onClick={() => siteAction(site, "stop")}><CircleStop size={17} /></button>
                        ) : (
                          <button className="icon-btn sm" title="Start" aria-label="Start site" onClick={() => siteAction(site, "start")}><CirclePlay size={17} /></button>
                        )}
                        <button className="icon-btn sm" title="Restart" aria-label="Restart site" onClick={() => siteAction(site, "restart")}><RotateCcw size={17} /></button>
                      </>
                    )}
                    {can("logs.view") && <button className="btn sm" onClick={() => { setLogSite(site.id); setView("logs"); }}>Logs</button>}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      {view === "pools" && (
        <div className="card table-wrap">
          {pools.error && <ErrorBanner error={pools.error} onRetry={pools.reload} />}
          <table className="table">
            <thead><tr><th>Pool</th><th>State</th><th>.NET CLR</th><th>Pipeline</th><th>Workers</th><th className="num">CPU</th><th className="num">Memory</th><th /></tr></thead>
            <tbody>
              {(pools.data ?? []).map((p) => (
                <tr key={p.name}>
                  <td style={{ fontWeight: 500 }}>{p.name}</td>
                  <td><StateChip state={p.state} /></td>
                  <td className="small">{p.runtime}</td>
                  <td className="small muted">{p.pipeline}</td>
                  <td className="mono small">{p.worker_pids.length ? p.worker_pids.join(", ") : <span className="faint">idle</span>}</td>
                  <td className="num">{pct(p.cpu, 1)}</td>
                  <td className="num">{bytes(p.mem)}</td>
                  <td className="actions">
                    {can("iis.control") && (
                      <>
                        <button className="btn sm" onClick={() => poolAction(p, "recycle")}>Recycle</button>
                        {p.state.toLowerCase() === "started" ? (
                          <button className="icon-btn sm" title="Stop" aria-label="Stop pool" onClick={() => poolAction(p, "stop")}><CircleStop size={17} /></button>
                        ) : (
                          <button className="icon-btn sm" title="Start" aria-label="Start pool" onClick={() => poolAction(p, "start")}><CirclePlay size={17} /></button>
                        )}
                      </>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      {view === "logs" && selectedSite != null && (
        <div className="col" style={{ gap: 10 }}>
          <div className="banner small">IIS buffers W3C logs and flushes them to disk about once a minute, so new requests appear with a short delay.</div>
          <LogViewer
            fileName={`iis-W3SVC${selectedSite}`}
            height="calc(100vh - 360px)"
            source={{ serverId: s.id, request: { kind: "iis_log_follow", site_id: selectedSite }, reconnect: true }}
            extraBar={
              <select className="select" style={{ width: 200, height: 32, borderRadius: 999, fontSize: 13 }} value={selectedSite} onChange={(e) => setLogSite(Number(e.target.value))} aria-label="Site">
                {(sites.data ?? []).map((x) => <option key={x.id} value={x.id}>{x.name}</option>)}
              </select>
            }
          />
        </div>
      )}
    </div>
  );
}
