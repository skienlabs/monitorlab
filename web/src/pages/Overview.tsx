import { useMemo, useState } from "react";
import { Link } from "react-router-dom";
import { Box, Boxes, CircleCheck, CloudOff, Database, Globe, Plus, Server as ServerIcon, ServerCog, ServerOff, TriangleAlert } from "lucide-react";
import { useServers } from "../servers";
import { useAuth } from "../auth";
import { ErrorBanner, Loading, Meter, SearchBox, Seg, StatusChip } from "../components/ui";
import { OsIcon } from "../components/OsIcon";
import { ago, duration, pct, ratio } from "../format";
import type { Server } from "../types";

type Filter = "all" | "issues" | "linux" | "windows";

function maxDisk(s: Server) {
  const d = s.latest?.disks ?? [];
  return d.reduce((m, x) => Math.max(m, ratio(x.used, x.total)), 0);
}

function ServerCard({ s }: { s: Server }) {
  const m = s.latest;
  const mem = m ? ratio(m.mem_used, m.mem_total) : 0;
  const disk = maxDisk(s);
  const caps = s.hello?.capabilities;
  return (
    <Link to={`/servers/${s.id}`} className={`server-card ${s.online ? "" : "offline"}`}>
      <div className="row" style={{ gap: 12 }}>
        <div className="os">
          <OsIcon os={s.hello?.os} />
        </div>
        <div className="grow">
          <div className="name ellipsis">{s.name}</div>
          <div className="small muted ellipsis">
            {s.hello ? `${s.hello.os_name}` : s.token_pending ? "Waiting for the agent to enroll" : "Not enrolled"}
            {s.is_local && " · hub host"}
          </div>
        </div>
        <StatusChip status={s.status} />
      </div>

      {s.online && m ? (
        <div className="metrics">
          <span className="k">CPU</span>
          <Meter value={m.cpu} warn={85} crit={95} />
          <span className="v">{pct(m.cpu)}</span>
          <span className="k">Memory</span>
          <Meter value={mem} warn={90} crit={95} />
          <span className="v">{pct(mem)}</span>
          <span className="k">Disk</span>
          <Meter value={disk} warn={85} crit={95} />
          <span className="v">{pct(disk)}</span>
        </div>
      ) : (
        <div className="small muted" style={{ minHeight: 66, display: "flex", alignItems: "center" }}>
          {s.enrolled ? `Last seen ${ago(s.last_seen_at)}` : "Install the agent from Servers & agents to start monitoring."}
        </div>
      )}

      {s.reasons.length > 0 && (
        <div className="small" style={{ color: s.status === "critical" ? "var(--crit-text)" : "var(--warn-text)" }}>
          {s.reasons.slice(0, 2).join(" · ")}
          {s.reasons.length > 2 && ` · +${s.reasons.length - 2} more`}
        </div>
      )}

      <div className="row small muted" style={{ marginTop: "auto" }}>
        <div className="badges">
          {caps?.docker && (
            <span className="chip outline">
              <Box size={13} /> {m?.docker ? `${m.docker.running}/${m.docker.total}` : "Docker"}
            </span>
          )}
          {caps?.kubernetes && (
            <span className={`chip ${m?.k8s && (m.k8s.pods_failing > 0 || m.k8s.nodes_ready < m.k8s.nodes) ? "crit" : "outline"}`} title="Kubernetes pods running / total">
              <Boxes size={13} /> {m?.k8s ? `${m.k8s.pods_running}/${m.k8s.pods} pods` : "Kubernetes"}
            </span>
          )}
          {caps?.iis && (
            <span className="chip outline">
              <Globe size={13} /> IIS {m?.iis ? `${m.iis.sites_started}/${m.iis.sites_total}` : ""}
            </span>
          )}
          {caps?.mssql && (
            <span className="chip outline">
              <Database size={13} /> SQL Server
            </span>
          )}
        </div>
        <span className="spacer" />
        {s.online && m && <span className="tnum">up {duration(m.uptime)}</span>}
      </div>
    </Link>
  );
}

export function Overview() {
  const { servers, error, reload } = useServers();
  const { can } = useAuth();
  const [q, setQ] = useState("");
  const [filter, setFilter] = useState<Filter>("all");

  const counts = useMemo(() => {
    const c = { total: 0, ok: 0, warn: 0, offline: 0 };
    for (const s of servers ?? []) {
      c.total++;
      if (s.status === "ok") c.ok++;
      else if (s.status === "warning" || s.status === "critical") c.warn++;
      else c.offline++;
    }
    return c;
  }, [servers]);

  const list = useMemo(() => {
    const needle = q.trim().toLowerCase();
    return (servers ?? []).filter((s) => {
      if (filter === "issues" && !["warning", "critical", "offline"].includes(s.status)) return false;
      if (filter === "linux" && s.hello?.os !== "linux") return false;
      if (filter === "windows" && s.hello?.os !== "windows") return false;
      if (!needle) return true;
      return [s.name, s.hello?.hostname, s.hello?.os_name, ...(s.hello?.ips ?? [])].some((x) => x?.toLowerCase().includes(needle));
    });
  }, [servers, q, filter]);

  if (!servers && !error) return <Loading />;

  return (
    <div className="page">
      <div className="page-head">
        <div className="grow">
          <h1>Overview</h1>
          <div className="sub">All monitored servers, refreshed every 5 seconds</div>
        </div>
        {can("servers.manage") && (
          <Link to="/admin/servers?add=1" className="btn filled">
            <Plus size={18} /> Add server
          </Link>
        )}
      </div>
      {error && <ErrorBanner error={error} onRetry={reload} />}

      <div className="grid grid-4" style={{ marginBottom: 24 }}>
        <div className="card kpi" data-accent="cpu">
          <div className="label">
            <ServerIcon size={16} /> Servers
          </div>
          <div className="value">{counts.total}</div>
        </div>
        <div className="card kpi" data-accent="good">
          <div className="label">
            <CircleCheck size={16} /> Healthy
          </div>
          <div className="value">{counts.ok}</div>
        </div>
        <div className="card kpi" data-accent="warn">
          <div className="label">
            <TriangleAlert size={16} /> Need attention
          </div>
          <div className="value">{counts.warn}</div>
        </div>
        <div className="card kpi" data-accent="muted">
          <div className="label">
            <CloudOff size={16} /> Offline / pending
          </div>
          <div className="value">{counts.offline}</div>
        </div>
      </div>

      <div className="row wrap" style={{ marginBottom: 16, gap: 12 }}>
        <SearchBox value={q} onChange={setQ} placeholder="Search by name, host or IP" />
        <Seg<Filter>
          value={filter}
          onChange={setFilter}
          options={[
            { value: "all", label: "All" },
            { value: "issues", label: "Needs attention" },
            { value: "linux", label: "Linux" },
            { value: "windows", label: "Windows" },
          ]}
        />
      </div>

      {list.length === 0 ? (
        <div className="card">
          <div className="empty">
            <div className="ico">{servers?.length ? <ServerOff size={26} /> : <ServerCog size={26} />}</div>
            <h3>{servers?.length ? "No servers match" : "No servers yet"}</h3>
            <div className="small">
              {servers?.length ? "Try a different search or filter." : "Add a server and run the one-line installer on it. The agent connects out to this hub — no inbound ports needed."}
            </div>
          </div>
        </div>
      ) : (
        <div className="server-grid">
          {list.map((s) => (
            <ServerCard key={s.id} s={s} />
          ))}
        </div>
      )}
    </div>
  );
}
