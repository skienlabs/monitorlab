import { ReactNode, useEffect, useMemo, useState } from "react";
import { Link, useNavigate } from "react-router-dom";
import {
  Box, ChevronDown, ChevronRight, CirclePause, CirclePlay, CircleStop, FileText, HardDrive, Layers, MoreVertical, Network, RefreshCw,
  RotateCcw, Skull, SquareTerminal, Trash2, Sparkles, Hammer, Pause, Play, Zap, X, PencilLine, RefreshCcwDot,
} from "lucide-react";
import { openStream, rpc } from "../api";
import { useAuth } from "../auth";
import type { Container, Server } from "../types";
import { Dialog, Empty, ErrorBanner, Loading, Menu, SearchBox, Seg, Spinner, StateChip, useConfirm, usePoll, useToast } from "../components/ui";
import { ago, bytes, plural, shortId } from "../format";

export function Toolbar({ children, onRefresh, loading }: { children?: ReactNode; onRefresh: () => void; loading: boolean }) {
  return (
    <div className="row wrap" style={{ marginBottom: 16, gap: 12 }}>
      {children}
      <span className="spacer" />
      <button className="icon-btn" onClick={onRefresh} aria-label="Refresh" title="Refresh">
        {loading ? <Spinner /> : <RefreshCw size={18} />}
      </button>
    </div>
  );
}

// =================================================================== containers

interface LiveStat {
  id: string;
  cpu: number;
  mem_used: number;
  mem_limit: number;
}
interface LiveHist {
  cpu: number[];
  mem: number[];
  last: LiveStat;
}
const HIST = 40; // samples kept per container (~80 s)

/** Live CPU / memory of all running containers, streamed from the agent every 2 s. */
function useContainerLive(serverId: string, enabled: boolean) {
  const [hist, setHist] = useState<Record<string, LiveHist>>({});
  const [retry, setRetry] = useState(0);
  useEffect(() => {
    if (!enabled) return;
    let timer: number | undefined;
    const c = openStream(serverId, { kind: "container_stats_all" }, {
      onData: (d) => {
        if (d?.k !== "stats_all") return;
        setHist((prev) => {
          const next: Record<string, LiveHist> = {};
          for (const it of d.items as LiveStat[]) {
            const p = prev[it.id];
            next[it.id] = {
              cpu: [...(p?.cpu ?? []).slice(-(HIST - 1)), it.cpu],
              mem: [...(p?.mem ?? []).slice(-(HIST - 1)), it.mem_used],
              last: it,
            };
          }
          return next;
        });
      },
      onEnd: () => (timer = window.setTimeout(() => setRetry((r) => r + 1), 5000)),
    });
    return () => {
      window.clearTimeout(timer);
      c.close();
    };
  }, [serverId, enabled, retry]);
  return hist;
}

function Spark({ values, max, color }: { values: number[]; max?: number; color: string }) {
  const w = 72;
  const h = 20;
  if (values.length < 2) return <svg className="spark" width={w} height={h} aria-hidden />;
  const top = Math.max(max ?? 0, ...values, 1e-9);
  const step = w / (HIST - 1);
  const x0 = w - (values.length - 1) * step;
  const pts = values.map((v, i) => `${(x0 + i * step).toFixed(1)},${(h - 1 - (v / top) * (h - 3)).toFixed(1)}`).join(" ");
  return (
    <svg className="spark" width={w} height={h} viewBox={`0 0 ${w} ${h}`} aria-hidden>
      <polyline points={pts} fill="none" stroke={`var(${color})`} strokeWidth="1.5" strokeLinejoin="round" />
    </svg>
  );
}

function LiveCells({ live, running }: { live?: LiveHist; running: boolean }) {
  if (!running || !live)
    return (
      <>
        <td className="small faint">{running ? "…" : "—"}</td>
        <td className="small faint">{running ? "…" : "—"}</td>
      </>
    );
  const l = live.last;
  return (
    <>
      <td className="nowrap live-cell" title="100% = one full CPU core (same as docker stats)">
        <Spark values={live.cpu} max={5} color="--accent-cpu" />
        <span className="tnum small">{l.cpu < 10 ? l.cpu.toFixed(1) : Math.round(l.cpu)}%</span>
      </td>
      <td className="nowrap live-cell" title={l.mem_limit ? `Limit ${bytes(l.mem_limit)}` : undefined}>
        <Spark values={live.mem} max={16 * 1024 * 1024} color="--accent-mem" />
        <span className="tnum small">{bytes(l.mem_used)}</span>
      </td>
    </>
  );
}

export function useContainerActions(serverId: string, reload: () => void) {
  const toast = useToast();
  const confirm = useConfirm();
  const act = async (c: Pick<Container, "id" | "name">, action: string, label: string) => {
    try {
      await rpc(serverId, { op: "docker_container_action", id: c.id, action });
      toast(`${c.name}: ${label}`);
    } catch (e: any) {
      toast(`${c.name}: ${e.message}`, true);
    }
    reload();
  };
  return {
    start: (c: Container) => act(c, "start", "started"),
    unpause: (c: Container) => act(c, "unpause", "resumed"),
    pause: (c: Container) => act(c, "pause", "paused"),
    restart: (c: Container) =>
      confirm({
        title: `Restart ${c.name}?`,
        body: "The container will be stopped (15s grace period) and started again. Active connections will drop.",
        confirm: "Restart",
        action: () => rpc(serverId, { op: "docker_container_action", id: c.id, action: "restart" }).finally(reload),
        success: `${c.name} restarted`,
      }),
    stop: (c: Container) =>
      confirm({
        title: `Stop ${c.name}?`,
        body: "The container receives SIGTERM and is killed after 15 seconds if it has not exited.",
        confirm: "Stop",
        action: () => rpc(serverId, { op: "docker_container_action", id: c.id, action: "stop" }).finally(reload),
        success: `${c.name} stopped`,
      }),
    kill: (c: Container) =>
      confirm({
        title: `Kill ${c.name}?`,
        body: "Sends SIGKILL immediately. The process gets no chance to shut down cleanly.",
        confirm: "Kill",
        danger: true,
        action: () => rpc(serverId, { op: "docker_container_action", id: c.id, action: "kill" }).finally(reload),
        success: `${c.name} killed`,
      }),
    remove: (c: Container, after?: () => void) =>
      confirm({
        title: `Delete container ${c.name}?`,
        body: "The container and its writable layer are permanently deleted. Named volumes are kept unless you choose otherwise below.",
        confirm: "Delete",
        danger: true,
        typeToConfirm: c.name,
        option: "Also remove anonymous volumes and force-stop if running",
        action: (opt) =>
          rpc(serverId, { op: "docker_container_remove", id: c.id, force: opt, volumes: opt }).then(() => {
            reload();
            after?.();
          }),
        success: `${c.name} deleted`,
      }),
    recreate: (c: Pick<Container, "id" | "name">, after?: (newId: string) => void) =>
      confirm({
        title: `Recreate ${c.name}?`,
        body: (
          <>
            The container is stopped and replaced by a new one with exactly the same settings, volumes and networks (like Portainer's
            <i> Recreate</i>). Anything written inside the container outside volumes is lost. If anything fails, the original is restored.
          </>
        ),
        confirm: "Recreate",
        danger: true,
        option: "Pull the latest version of the image first",
        action: (pull) =>
          rpc<{ id: string; pulled: boolean; image_changed: boolean }>(serverId, { op: "docker_container_recreate", id: c.id, pull }).then((r) => {
            reload();
            after?.(r.id);
            return r;
          }),
        success: (r) => (r.pulled ? (r.image_changed ? `${c.name} recreated with the newer image` : `${c.name} recreated (image was already up to date)`) : `${c.name} recreated`),
      }),
  };
}

export type BulkAction = "start" | "stop" | "restart" | "pause" | "unpause" | "kill" | "remove";
const BULK_LABEL: Record<BulkAction, [string, string]> = {
  start: ["Start", "started"],
  stop: ["Stop", "stopped"],
  restart: ["Restart", "restarted"],
  pause: ["Pause", "paused"],
  unpause: ["Resume", "resumed"],
  kill: ["Kill", "killed"],
  remove: ["Remove", "removed"],
};

/** Run one action on many containers (4 at a time) and report how it went. */
export function useBulkActions(serverId: string, reload: () => void) {
  const confirm = useConfirm();
  const toast = useToast();
  const exec = async (list: Pick<Container, "id" | "name">[], a: BulkAction, force = false) => {
    const failed: string[] = [];
    const queue = [...list];
    const worker = async () => {
      for (let c = queue.shift(); c; c = queue.shift()) {
        try {
          if (a === "remove") await rpc(serverId, { op: "docker_container_remove", id: c.id, force, volumes: false });
          else await rpc(serverId, { op: "docker_container_action", id: c.id, action: a });
        } catch (e: any) {
          failed.push(`${c.name}: ${e.message}`);
        }
      }
    };
    await Promise.all(Array.from({ length: Math.min(4, list.length) }, worker));
    reload();
    const ok = list.length - failed.length;
    if (failed.length) toast(`${ok} of ${list.length} ${BULK_LABEL[a][1]}. Failed: ${failed.join("; ")}`, true);
    else toast(`${ok} container${ok === 1 ? "" : "s"} ${BULK_LABEL[a][1]}`);
  };
  return (list: Pick<Container, "id" | "name">[], a: BulkAction, done?: () => void) => {
    const names = list.map((c) => c.name);
    const shown = names.slice(0, 8).join(", ") + (names.length > 8 ? ` and ${names.length - 8} more` : "");
    if (a === "start" || a === "unpause") {
      void exec(list, a).then(done);
      return;
    }
    const danger = a === "kill" || a === "remove";
    confirm({
      title: `${BULK_LABEL[a][0]} ${list.length} container${list.length === 1 ? "" : "s"}?`,
      body: <>{a === "remove" ? "Permanently deletes" : a === "kill" ? "Sends SIGKILL to" : `${BULK_LABEL[a][0]}s`} <b>{shown}</b>.</>,
      confirm: BULK_LABEL[a][0],
      danger,
      option: a === "remove" ? "Force-remove running containers" : undefined,
      typeToConfirm: a === "remove" && list.length > 1 ? `remove ${list.length}` : undefined,
      action: (opt) => exec(list, a, opt).then(done),
    });
  };
}

/** Rename dialog shared by the containers list and the container page. */
export function RenameDialog({ serverId, c, onClose, onDone }: { serverId: string; c: Pick<Container, "id" | "name">; onClose: () => void; onDone: (name: string) => void }) {
  const [name, setName] = useState(c.name);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const toast = useToast();
  const valid = /^[a-zA-Z0-9][a-zA-Z0-9_.-]*$/.test(name.trim());
  const save = async () => {
    setBusy(true);
    setErr(null);
    try {
      await rpc(serverId, { op: "docker_container_rename", id: c.id, name: name.trim() });
      toast(`Renamed to ${name.trim()}`);
      onDone(name.trim());
    } catch (e: any) {
      setErr(e.message);
      setBusy(false);
    }
  };
  return (
    <Dialog
      title={`Rename ${c.name}`}
      onClose={() => !busy && onClose()}
      footer={
        <>
          <button className="btn" onClick={onClose} disabled={busy}>Cancel</button>
          <button className="btn filled" onClick={save} disabled={busy || !valid || name.trim() === c.name}>
            {busy && <Spinner />} Rename
          </button>
        </>
      }
    >
      <div className="field">
        <label htmlFor="rn">New name</label>
        <input id="rn" className="input mono" autoFocus value={name} onChange={(e) => setName(e.target.value)} onKeyDown={(e) => e.key === "Enter" && valid && save()} />
        <span className="hint">Letters, digits, _ . and -. Compose-managed containers keep their stack labels.</span>
      </div>
      {err && <div className="banner crit small">{err}</div>}
    </Dialog>
  );
}

function Ports({ c }: { c: Container }) {
  const pub = c.ports.filter((p) => p.public);
  if (pub.length === 0) return <span className="faint">—</span>;
  const host = location.hostname;
  return (
    <span className="row wrap" style={{ gap: 4 }}>
      {pub.slice(0, 3).map((p, i) => (
        <span key={i} className="chip outline mono" style={{ fontSize: 11.5 }} title={`${p.ip ?? "0.0.0.0"}:${p.public} → ${p.private}/${p.proto}`}>
          {p.proto === "tcp" ? (
            <a href={`http://${host}:${p.public}`} target="_blank" rel="noreferrer noopener" onClick={(e) => e.stopPropagation()}>
              {p.public}
            </a>
          ) : (
            p.public
          )}
          →{p.private}
        </span>
      ))}
      {pub.length > 3 && <span className="faint small">+{pub.length - 3}</span>}
    </span>
  );
}

function ContainerRow({ c, s, actions, live, selected, onSelect, onRename }: { c: Container; s: Server; actions: ReturnType<typeof useContainerActions>; live?: LiveHist; selected?: boolean; onSelect?: (on: boolean) => void; onRename: () => void }) {
  const { can } = useAuth();
  const nav = useNavigate();
  const running = c.state === "running";
  const paused = c.state === "paused";
  const base = `/servers/${s.id}/containers/${encodeURIComponent(c.name || c.id)}`;
  return (
    <tr style={{ cursor: "pointer" }} className={selected ? "selected" : undefined} onClick={() => nav(base)}>
      {onSelect && (
        <td className="check-cell" onClick={(e) => e.stopPropagation()}>
          <input type="checkbox" aria-label={`Select ${c.name}`} checked={!!selected} onChange={(e) => onSelect(e.target.checked)} />
        </td>
      )}
      <td style={{ maxWidth: 280 }}>
        <div className="row" style={{ gap: 10 }}>
          <div className="grow">
            <Link to={base} className="ellipsis" style={{ display: "block", fontWeight: 500, color: "var(--text)" }} onClick={(e) => e.stopPropagation()}>
              {c.name}
            </Link>
            <div className="small faint ellipsis mono">{shortId(c.id)}</div>
          </div>
        </div>
      </td>
      <td style={{ maxWidth: 260 }}>
        <div className="ellipsis small" title={c.image}>{c.image}</div>
      </td>
      <td>
        <StateChip state={c.state} health={c.health} />
        <div className="small faint" style={{ marginTop: 2 }}>{c.status}</div>
      </td>
      <LiveCells live={live} running={c.state === "running"} />
      <td>
        <Ports c={c} />
      </td>
      <td className="small muted nowrap">{ago(c.created * 1000)}</td>
      <td className="actions" onClick={(e) => e.stopPropagation()}>
        <div className="row" style={{ gap: 0, justifyContent: "flex-end" }}>
          {can("logs.view") && (
            <Link className="icon-btn sm" to={`${base}?tab=logs`} title="Logs" aria-label="Logs">
              <FileText size={17} />
            </Link>
          )}
          {can("containers.exec") && (
            <Link className={`icon-btn sm ${running ? "" : "hidden"}`} to={`${base}?tab=terminal`} title="Terminal" aria-label="Terminal">
              <SquareTerminal size={17} />
            </Link>
          )}
          {can("containers.control") && (
            <>
              {running ? (
                <button className="icon-btn sm" onClick={() => actions.stop(c)} title="Stop" aria-label="Stop">
                  <CircleStop size={17} />
                </button>
              ) : paused ? (
                <button className="icon-btn sm" onClick={() => actions.unpause(c)} title="Resume" aria-label="Resume">
                  <CirclePlay size={17} />
                </button>
              ) : (
                <button className="icon-btn sm" onClick={() => actions.start(c)} title="Start" aria-label="Start">
                  <CirclePlay size={17} />
                </button>
              )}
              <button className="icon-btn sm" onClick={() => actions.restart(c)} title="Restart" aria-label="Restart">
                <RotateCcw size={17} />
              </button>
            </>
          )}
          {(can("containers.control") || can("containers.delete")) && (
            <Menu trigger={<MoreVertical size={17} />}>
              {can("containers.control") && running && (
                <button onClick={() => actions.pause(c)}>
                  <CirclePause size={17} /> Pause
                </button>
              )}
              {can("containers.control") && (running || paused) && (
                <button onClick={() => actions.kill(c)}>
                  <Skull size={17} /> Kill
                </button>
              )}
              {can("containers.control") && (
                <button onClick={onRename}>
                  <PencilLine size={17} /> Rename
                </button>
              )}
              {can("containers.delete") && (
                <>
                  <button onClick={() => actions.recreate(c)}>
                    <RefreshCcwDot size={17} /> Recreate…
                  </button>
                  <hr />
                  <button className="danger" onClick={() => actions.remove(c)}>
                    <Trash2 size={17} /> Delete container
                  </button>
                </>
              )}
            </Menu>
          )}
        </div>
      </td>
    </tr>
  );
}

export function ContainersTab({ s }: { s: Server }) {
  const { can } = useAuth();
  const { data, error, loading, reload } = usePoll(() => rpc<Container[]>(s.id, { op: "docker_containers" }), [s.id], 5000);
  const [q, setQ] = useState("");
  const [state, setState] = useState<"all" | "running" | "stopped">("all");
  const [collapsed, setCollapsed] = useState<Record<string, boolean>>({});
  const [sort, setSort] = useState<"name" | "cpu" | "mem">("name");
  const [sel, setSel] = useState<Set<string>>(new Set());
  const [renaming, setRenaming] = useState<Container | null>(null);
  const actions = useContainerActions(s.id, reload);
  const bulk = useBulkActions(s.id, reload);
  const canSelect = can("containers.control") || can("containers.delete");
  const live = useContainerLive(s.id, s.online);

  const groups = useMemo(() => {
    const needle = q.trim().toLowerCase();
    const metric = (c: Container) => {
      const l = live[c.id]?.last;
      return l ? (sort === "cpu" ? l.cpu : l.mem_used) : -1;
    };
    const list = (data ?? [])
      .filter((c) => (state === "all" ? true : state === "running" ? c.state === "running" : c.state !== "running"))
      .filter((c) => !needle || [c.name, c.image, c.project, c.service, c.id].some((x) => x?.toLowerCase().includes(needle)))
      .sort((a, b) => (sort === "name" ? a.name.localeCompare(b.name) : metric(b) - metric(a) || a.name.localeCompare(b.name)));
    // Sorting by usage shows one flat list, so the heaviest containers are on top.
    if (sort !== "name") return [["", list]] as [string, Container[]][];
    const map = new Map<string, Container[]>();
    for (const c of list) {
      const k = c.project ?? "";
      map.set(k, [...(map.get(k) ?? []), c]);
    }
    return [...map.entries()].sort((a, b) => (a[0] === "" ? 1 : b[0] === "" ? -1 : a[0].localeCompare(b[0])));
  }, [data, q, state, sort, live]);

  if (!s.online) return <Empty icon={<Box size={26} />} title="Server is offline">Container information is live and needs the agent to be connected.</Empty>;
  if (!data && !error) return <Loading />;
  const visible = groups.flatMap(([p, l]) => (collapsed[p] && sort === "name" ? [] : l));
  const chosen = (data ?? []).filter((c) => sel.has(c.id));
  const allVisible = visible.length > 0 && visible.every((c) => sel.has(c.id));
  const toggle = (id: string, on: boolean) =>
    setSel((prev) => {
      const n = new Set(prev);
      if (on) n.add(id);
      else n.delete(id);
      return n;
    });
  const clear = () => setSel(new Set());
  const anyRunning = chosen.some((c) => c.state === "running");
  const anyStopped = chosen.some((c) => c.state !== "running" && c.state !== "paused");
  const anyPaused = chosen.some((c) => c.state === "paused");
  const total = data?.length ?? 0;
  const running = data?.filter((c) => c.state === "running").length ?? 0;

  return (
    <div>
      <Toolbar onRefresh={reload} loading={loading}>
        <SearchBox value={q} onChange={setQ} placeholder="Filter by name, image or stack" />
        <Seg
          value={state}
          onChange={setState}
          options={[
            { value: "all", label: `All ${total}` },
            { value: "running", label: `Running ${running}` },
            { value: "stopped", label: `Stopped ${total - running}` },
          ]}
        />
      </Toolbar>
      {error && <ErrorBanner error={error} onRetry={reload} />}
      {chosen.length > 0 && (
        <div className="bulk-bar" role="toolbar" aria-label="Actions for selected containers">
          <b>{chosen.length} selected</b>
          {can("containers.control") && (
            <>
              <button className="btn sm" disabled={!anyStopped} onClick={() => bulk(chosen.filter((c) => c.state !== "running" && c.state !== "paused"), "start", clear)}><CirclePlay size={15} /> Start</button>
              <button className="btn sm" disabled={!anyRunning} onClick={() => bulk(chosen.filter((c) => c.state === "running"), "stop", clear)}><CircleStop size={15} /> Stop</button>
              <button className="btn sm" onClick={() => bulk(chosen, "restart", clear)}><RotateCcw size={15} /> Restart</button>
              <button className="btn sm" disabled={!anyRunning} onClick={() => bulk(chosen.filter((c) => c.state === "running"), "pause", clear)}><Pause size={15} /> Pause</button>
              <button className="btn sm" disabled={!anyPaused} onClick={() => bulk(chosen.filter((c) => c.state === "paused"), "unpause", clear)}><Play size={15} /> Resume</button>
              <button className="btn sm danger-text" disabled={!anyRunning} onClick={() => bulk(chosen.filter((c) => c.state === "running"), "kill", clear)}><Zap size={15} /> Kill</button>
            </>
          )}
          {can("containers.delete") && (
            <button className="btn sm danger-text" onClick={() => bulk(chosen, "remove", clear)}><Trash2 size={15} /> Remove</button>
          )}
          <span className="grow" />
          <button className="btn sm" onClick={clear}><X size={15} /> Clear</button>
        </div>
      )}
      <div className="card">
        {groups.length === 0 ? (
          <Empty icon={<Box size={26} />} title="No containers match" />
        ) : (
          <div className="table-wrap">
            <table className="table">
              <thead>
                <tr>
                  {canSelect && (
                    <th className="check-cell">
                      <input
                        type="checkbox"
                        aria-label="Select all shown containers"
                        checked={allVisible}
                        ref={(el) => {
                          if (el) el.indeterminate = !allVisible && visible.some((c) => sel.has(c.id));
                        }}
                        onChange={(e) => setSel(e.target.checked ? new Set([...sel, ...visible.map((c) => c.id)]) : new Set([...sel].filter((id) => !visible.some((c) => c.id === id))))}
                      />
                    </th>
                  )}
                  <SortTh active={sort === "name"} onClick={() => setSort("name")}>Name</SortTh>
                  <th>Image</th>
                  <th>State</th>
                  <SortTh active={sort === "cpu"} onClick={() => setSort(sort === "cpu" ? "name" : "cpu")}>CPU</SortTh>
                  <SortTh active={sort === "mem"} onClick={() => setSort(sort === "mem" ? "name" : "mem")}>Memory</SortTh>
                  <th>Ports</th>
                  <th>Created</th>
                  <th />
                </tr>
              </thead>
              <tbody>
                {groups.map(([project, list]) => {
                  const showHeader = sort === "name" && (groups.length > 1 || project !== "");
                  const isCollapsed = collapsed[project];
                  const up = list.filter((c) => c.state === "running").length;
                  return (
                    <GroupRows key={project || "_"}>
                      {showHeader && (
                        <tr className="group-row" onClick={() => setCollapsed((m) => ({ ...m, [project]: !m[project] }))}>
                          <td colSpan={canSelect ? 9 : 8}>
                            <span className="row">
                              {isCollapsed ? <ChevronRight size={16} /> : <ChevronDown size={16} />}
                              {project ? <Layers size={15} /> : <Box size={15} />}
                              {project || "Standalone containers"}
                              <span className="faint" style={{ fontWeight: 400 }}>
                                {up}/{list.length} running
                              </span>
                            </span>
                          </td>
                        </tr>
                      )}
                      {!isCollapsed && list.map((c) => <ContainerRow key={c.id} c={c} s={s} actions={actions} live={live[c.id]} selected={sel.has(c.id)} onSelect={canSelect ? (on) => toggle(c.id, on) : undefined} onRename={() => setRenaming(c)} />)}
                    </GroupRows>
                  );
                })}
              </tbody>
            </table>
          </div>
        )}
      </div>
      {renaming && (
        <RenameDialog
          serverId={s.id}
          c={renaming}
          onClose={() => setRenaming(null)}
          onDone={() => {
            setRenaming(null);
            reload();
          }}
        />
      )}
    </div>
  );
}

function SortTh({ active, onClick, children }: { active: boolean; onClick: () => void; children: ReactNode }) {
  return (
    <th className={`sortable ${active ? "sorted" : ""}`} onClick={onClick} aria-sort={active ? "descending" : "none"}>
      {children}
      {active && <ChevronDown size={14} style={{ verticalAlign: "-2px", marginLeft: 2 }} />}
    </th>
  );
}

function GroupRows({ children }: { children: ReactNode }) {
  return <>{children}</>;
}

// =================================================================== images

interface Image {
  id: string;
  tags: string[];
  created: number;
  size: number;
  dangling: boolean;
  in_use: boolean;
  used_by: string[];
}

export function ImagesTab({ s }: { s: Server }) {
  const { can } = useAuth();
  const confirm = useConfirm();
  const { data, error, loading, reload } = usePoll(() => rpc<Image[]>(s.id, { op: "docker_images" }), [s.id]);
  const [q, setQ] = useState("");
  const [f, setF] = useState<"all" | "unused" | "dangling">("all");
  const list = useMemo(() => {
    const needle = q.trim().toLowerCase();
    return (data ?? [])
      .filter((i) => (f === "unused" ? !i.in_use : f === "dangling" ? i.dangling : true))
      .filter((i) => !needle || i.tags.some((t) => t.toLowerCase().includes(needle)) || i.id.includes(needle))
      .sort((a, b) => b.size - a.size);
  }, [data, q, f]);
  if (!s.online) return <Empty icon={<Layers size={26} />} title="Server is offline" />;
  if (!data && !error) return <Loading />;
  const unused = (data ?? []).filter((i) => !i.in_use);
  const unusedSize = unused.reduce((a, i) => a + i.size, 0);

  const remove = (i: Image) =>
    confirm({
      title: `Delete image ${i.tags[0] ?? shortId(i.id)}?`,
      body: i.in_use ? `This image is used by ${i.used_by.join(", ")}. Docker refuses unless forced.` : "The image is removed from this server. It can be pulled again later.",
      confirm: "Delete",
      danger: true,
      option: "Force (remove even if tagged in several repositories or used by stopped containers)",
      action: (force) => rpc(s.id, { op: "docker_image_remove", id: i.id, force }).finally(reload),
      success: "Image deleted",
    });

  return (
    <div>
      <Toolbar onRefresh={reload} loading={loading}>
        <SearchBox value={q} onChange={setQ} placeholder="Filter by tag" />
        <Seg value={f} onChange={setF} options={[{ value: "all", label: `All ${data?.length ?? 0}` }, { value: "unused", label: `Unused ${unused.length}` }, { value: "dangling", label: "Dangling" }]} />
        {can("docker.cleanup") && unused.length > 0 && (
          <button
            className="btn tonal sm"
            onClick={() =>
              confirm({
                title: "Remove all unused images?",
                body: `Deletes every image not used by any container (${unused.length} images, about ${bytes(unusedSize)}). They can be pulled again later.`,
                confirm: "Remove unused",
                danger: true,
                action: () => rpc(s.id, { op: "docker_prune", target: "images_unused" }).finally(reload),
                success: (r) => `Removed ${r.deleted} images, reclaimed ${bytes(r.space_reclaimed)}`,
              })
            }
          >
            <Trash2 size={16} /> Remove unused ({bytes(unusedSize)})
          </button>
        )}
      </Toolbar>
      {error && <ErrorBanner error={error} onRetry={reload} />}
      <div className="card table-wrap">
        <table className="table">
          <thead>
            <tr>
              <th>Repository / tag</th>
              <th>ID</th>
              <th className="num">Size</th>
              <th>Created</th>
              <th>Used by</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {list.map((i) => (
              <tr key={i.id}>
                <td>
                  {i.tags.length ? (
                    i.tags.map((t) => (
                      <div key={t} className="mono small">{t}</div>
                    ))
                  ) : (
                    <span className="chip warn">dangling</span>
                  )}
                </td>
                <td className="mono small faint">{shortId(i.id)}</td>
                <td className="num">{bytes(i.size)}</td>
                <td className="small muted nowrap">{ago(i.created * 1000)}</td>
                <td className="small">{i.in_use ? i.used_by.join(", ") : <span className="chip">unused</span>}</td>
                <td className="actions">
                  {can("docker.cleanup") && (
                    <button className="icon-btn sm danger" onClick={() => remove(i)} aria-label="Delete image" title="Delete image">
                      <Trash2 size={17} />
                    </button>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
        {list.length === 0 && <Empty icon={<Layers size={26} />} title="No images match" />}
      </div>
    </div>
  );
}

// =================================================================== volumes

interface Volume {
  name: string;
  driver: string;
  mountpoint: string;
  created: string | null;
  project: string | null;
  anonymous: boolean;
  in_use: boolean;
  used_by: string[];
}

export function VolumesTab({ s }: { s: Server }) {
  const { can } = useAuth();
  const confirm = useConfirm();
  const { data, error, loading, reload } = usePoll(() => rpc<Volume[]>(s.id, { op: "docker_volumes" }), [s.id]);
  const [q, setQ] = useState("");
  const [f, setF] = useState<"all" | "unused">("all");
  const list = useMemo(() => {
    const needle = q.trim().toLowerCase();
    return (data ?? [])
      .filter((v) => (f === "unused" ? !v.in_use : true))
      .filter((v) => !needle || v.name.toLowerCase().includes(needle) || v.project?.toLowerCase().includes(needle))
      .sort((a, b) => Number(b.in_use) - Number(a.in_use) || a.name.localeCompare(b.name));
  }, [data, q, f]);
  if (!s.online) return <Empty icon={<HardDrive size={26} />} title="Server is offline" />;
  if (!data && !error) return <Loading />;
  const unused = (data ?? []).filter((v) => !v.in_use).length;

  return (
    <div>
      <Toolbar onRefresh={reload} loading={loading}>
        <SearchBox value={q} onChange={setQ} placeholder="Filter volumes" />
        <Seg value={f} onChange={setF} options={[{ value: "all", label: `All ${data?.length ?? 0}` }, { value: "unused", label: `Unused ${unused}` }]} />
        {can("docker.cleanup") && unused > 0 && (
          <button
            className="btn tonal sm"
            onClick={() =>
              confirm({
                title: "Delete all unused volumes?",
                body: (
                  <>
                    <p style={{ marginTop: 0 }}>
                      Permanently deletes {plural(unused, "volume")} not attached to any container — <b>including named volumes that may hold database data</b> for stopped
                      stacks. This cannot be undone.
                    </p>
                  </>
                ),
                confirm: "Delete volumes",
                danger: true,
                typeToConfirm: "delete volumes",
                action: () => rpc(s.id, { op: "docker_prune", target: "volumes" }).finally(reload),
                success: (r) => `Deleted ${r.deleted} volumes, reclaimed ${bytes(r.space_reclaimed)}`,
              })
            }
          >
            <Trash2 size={16} /> Delete unused
          </button>
        )}
      </Toolbar>
      {error && <ErrorBanner error={error} onRetry={reload} />}
      <div className="card table-wrap">
        <table className="table">
          <thead>
            <tr>
              <th>Name</th>
              <th>Stack</th>
              <th>Driver</th>
              <th>Used by</th>
              <th>Created</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {list.map((v) => (
              <tr key={v.name}>
                <td style={{ maxWidth: 360 }}>
                  <div className="mono small ellipsis" title={v.mountpoint}>{v.anonymous ? shortId(v.name) : v.name}</div>
                  {v.anonymous && <span className="chip" style={{ marginTop: 4 }}>anonymous</span>}
                </td>
                <td className="small">{v.project ?? <span className="faint">—</span>}</td>
                <td className="small muted">{v.driver}</td>
                <td className="small">{v.in_use ? v.used_by.join(", ") : <span className="chip">unused</span>}</td>
                <td className="small muted nowrap">{v.created ? ago(Date.parse(v.created)) : "—"}</td>
                <td className="actions">
                  {can("docker.cleanup") && (
                    <button
                      className="icon-btn sm danger"
                      disabled={v.in_use}
                      title={v.in_use ? "In use" : "Delete volume"}
                      aria-label="Delete volume"
                      onClick={() =>
                        confirm({
                          title: "Delete volume?",
                          body: "All data stored in this volume is permanently deleted.",
                          confirm: "Delete",
                          danger: true,
                          typeToConfirm: v.anonymous ? undefined : v.name,
                          action: () => rpc(s.id, { op: "docker_volume_remove", name: v.name }).finally(reload),
                          success: "Volume deleted",
                        })
                      }
                    >
                      <Trash2 size={17} />
                    </button>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
        {list.length === 0 && <Empty icon={<HardDrive size={26} />} title="No volumes" />}
      </div>
    </div>
  );
}

// =================================================================== networks

interface Net {
  id: string;
  name: string;
  driver: string;
  scope: string;
  internal: boolean;
  subnets: string[];
  containers: number;
  builtin: boolean;
  project: string | null;
}

export function NetworksTab({ s }: { s: Server }) {
  const { can } = useAuth();
  const confirm = useConfirm();
  const { data, error, loading, reload } = usePoll(() => rpc<Net[]>(s.id, { op: "docker_networks" }), [s.id]);
  if (!s.online) return <Empty icon={<Network size={26} />} title="Server is offline" />;
  if (!data && !error) return <Loading />;
  const unused = (data ?? []).filter((n) => !n.builtin && n.containers === 0).length;
  return (
    <div>
      <Toolbar onRefresh={reload} loading={loading}>
        {can("docker.cleanup") && unused > 0 && (
          <button
            className="btn tonal sm"
            onClick={() =>
              confirm({
                title: "Remove unused networks?",
                body: `Removes ${plural(unused, "custom network")} with no containers attached.`,
                confirm: "Remove",
                danger: true,
                action: () => rpc(s.id, { op: "docker_prune", target: "networks" }).finally(reload),
                success: (r) => `Removed ${r.deleted} networks`,
              })
            }
          >
            <Trash2 size={16} /> Remove unused ({unused})
          </button>
        )}
      </Toolbar>
      {error && <ErrorBanner error={error} onRetry={reload} />}
      <div className="card table-wrap">
        <table className="table">
          <thead>
            <tr>
              <th>Name</th>
              <th>Driver</th>
              <th>Scope</th>
              <th>Subnet</th>
              <th className="num">Containers</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {(data ?? []).map((n) => (
              <tr key={n.id}>
                <td>
                  <div className="row">
                    <span className="mono small">{n.name}</span>
                    {n.builtin && <span className="chip">built-in</span>}
                    {n.internal && <span className="chip">internal</span>}
                  </div>
                  {n.project && <div className="small faint">stack {n.project}</div>}
                </td>
                <td className="small muted">{n.driver}</td>
                <td className="small muted">{n.scope}</td>
                <td className="mono small">{n.subnets.join(", ") || "—"}</td>
                <td className="num">{n.containers}</td>
                <td className="actions">
                  {can("docker.cleanup") && (
                    <button
                      className="icon-btn sm danger"
                      disabled={n.builtin || n.containers > 0}
                      aria-label="Delete network"
                      title={n.builtin ? "Built-in network" : n.containers > 0 ? "In use" : "Delete network"}
                      onClick={() =>
                        confirm({
                          title: `Delete network ${n.name}?`,
                          body: "The network is removed. Containers can no longer join it.",
                          confirm: "Delete",
                          danger: true,
                          action: () => rpc(s.id, { op: "docker_network_remove", id: n.id }).finally(reload),
                          success: "Network deleted",
                        })
                      }
                    >
                      <Trash2 size={17} />
                    </button>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}

// =================================================================== storage & cleanup

interface DfPart {
  count: number;
  active: number;
  size: number;
  reclaimable: number;
}
interface Df {
  images: DfPart;
  containers: DfPart;
  volumes: DfPart;
  build_cache: DfPart;
  details: { images: any[]; containers: any[]; volumes: any[]; build_cache: any[] };
}

export function CleanupTab({ s }: { s: Server }) {
  const { can } = useAuth();
  const confirm = useConfirm();
  const { data, error, loading, reload } = usePoll(() => rpc<Df>(s.id, { op: "docker_disk_usage" }), [s.id]);
  if (!s.online) return <Empty icon={<Sparkles size={26} />} title="Server is offline" />;
  if (!data && !error) return <Loading label="Calculating disk usage (like docker system df)…" />;
  if (!data) return <ErrorBanner error={error!} onRetry={reload} />;

  const total = data.images.size + data.containers.size + data.volumes.size + data.build_cache.size;
  const reclaim = data.images.reclaimable + data.containers.reclaimable + data.volumes.reclaimable + data.build_cache.reclaimable;
  const prune = (target: string, title: string, body: ReactNode, danger = false, typeToConfirm?: string) =>
    confirm({
      title,
      body,
      confirm: "Clean up",
      danger,
      typeToConfirm,
      action: () => rpc(s.id, { op: "docker_prune", target }).finally(reload),
      success: (r) => `Removed ${r.deleted} item(s), reclaimed ${bytes(r.space_reclaimed)}`,
    });
  const cleanup = can("docker.cleanup");

  const parts: { key: keyof Df; label: string; icon: ReactNode; desc: string; color: string; actions: ReactNode }[] = [
    {
      key: "images",
      label: "Images",
      icon: <Layers size={18} />,
      color: "--series-1",
      desc: `${data.images.count} images · ${data.images.active} in use`,
      actions: cleanup && (
        <>
          <button className="btn sm outlined" onClick={() => prune("images_dangling", "Remove dangling images?", "Removes untagged image layers left behind by rebuilds. Safe for running workloads.")}>
            Dangling only
          </button>
          <button className="btn sm tonal" onClick={() => prune("images_unused", "Remove all unused images?", "Removes every image not used by a container. They can be pulled again later.", true)}>
            All unused
          </button>
        </>
      ),
    },
    {
      key: "containers",
      label: "Containers",
      icon: <Box size={18} />,
      color: "--series-2",
      desc: `${data.containers.count} containers · ${data.containers.active} running`,
      actions: cleanup && (
        <button className="btn sm tonal" onClick={() => prune("containers", "Remove stopped containers?", "Deletes every container that is not running, including their logs.", true)}>
          Remove stopped
        </button>
      ),
    },
    {
      key: "volumes",
      label: "Local volumes",
      icon: <HardDrive size={18} />,
      color: "--series-3",
      desc: `${data.volumes.count} volumes · ${data.volumes.active} in use`,
      actions: cleanup && (
        <button
          className="btn sm tonal"
          onClick={() =>
            prune(
              "volumes",
              "Delete unused volumes?",
              <>Permanently deletes volumes not attached to any container, <b>including named volumes</b> of stopped stacks. This cannot be undone.</>,
              true,
              "delete volumes",
            )
          }
        >
          Delete unused
        </button>
      ),
    },
    {
      key: "build_cache",
      label: "Build cache",
      icon: <Hammer size={18} />,
      color: "--text-3",
      desc: `${data.build_cache.count} entries · ${data.build_cache.active} in use`,
      actions: cleanup && (
        <button className="btn sm tonal" onClick={() => prune("build_cache", "Clear build cache?", "Removes all unused BuildKit cache. The next image builds will be slower once.")}>
          Clear cache
        </button>
      ),
    },
  ];

  const largestImages = [...data.details.images].sort((a, b) => b.size - a.size).slice(0, 8);
  const largestVolumes = [...data.details.volumes].filter((v) => v.size > 0).sort((a, b) => b.size - a.size).slice(0, 8);

  return (
    <div className="col" style={{ gap: 16 }}>
      <div className="card card-pad">
        <div className="row wrap" style={{ gap: 32, alignItems: "flex-end" }}>
          <div>
            <div className="muted small" style={{ fontWeight: 500 }}>Reclaimable</div>
            <div style={{ fontSize: 40, fontWeight: 400, letterSpacing: "-0.02em" }} className="tnum">{bytes(reclaim)}</div>
          </div>
          <div>
            <div className="muted small" style={{ fontWeight: 500 }}>Used by Docker</div>
            <div style={{ fontSize: 24 }} className="tnum">{bytes(total)}</div>
          </div>
          <span className="spacer" />
          <button className="btn" onClick={reload}>
            {loading ? <Spinner /> : <RefreshCw size={16} />} Recalculate
          </button>
        </div>
        <div className="stack-bar" style={{ marginTop: 18 }} role="img" aria-label="Docker disk usage by type">
          {parts.map((p) => {
            const v = (data[p.key] as DfPart).size;
            return v > 0 ? <span key={p.key} title={`${p.label}: ${bytes(v)}`} style={{ width: `${(v / Math.max(total, 1)) * 100}%`, background: `var(${p.color})` }} /> : null;
          })}
        </div>
        <div className="legend" style={{ marginTop: 10 }}>
          {parts.map((p) => (
            <span key={p.key}>
              <i style={{ background: `var(${p.color})`, height: 8, width: 8, borderRadius: 2 }} />
              {p.label} {bytes((data[p.key] as DfPart).size)}
            </span>
          ))}
        </div>
      </div>

      <div className="grid grid-2">
        {parts.map((p) => {
          const d = data[p.key] as DfPart;
          return (
            <div key={p.key} className="card card-pad col" style={{ gap: 10 }}>
              <div className="row">
                {p.icon}
                <h3 className="grow">{p.label}</h3>
                <span className="tnum" style={{ fontSize: 18 }}>{bytes(d.size)}</span>
              </div>
              <div className="small muted">{p.desc}</div>
              <div className="row small">
                <span className="grow">Reclaimable</span>
                <b className="tnum">{bytes(d.reclaimable)}</b>
              </div>
              <div className="meter">
                <span style={{ width: `${d.size ? (d.reclaimable / d.size) * 100 : 0}%`, background: "var(--text-3)" }} />
              </div>
              <div className="row" style={{ justifyContent: "flex-end", gap: 8, marginTop: 4 }}>
                {d.reclaimable > 0 || (p.key === "containers" && d.count > d.active) ? p.actions : <span className="small faint">Nothing to clean</span>}
              </div>
            </div>
          );
        })}
      </div>

      <div className="grid grid-2">
        <div className="card">
          <div className="card-head">
            <h3>Largest images</h3>
          </div>
          <div className="card-body table-wrap">
            <table className="table">
              <tbody>
                {largestImages.map((i) => (
                  <tr key={i.id}>
                    <td className="mono small ellipsis" style={{ maxWidth: 320 }}>{i.tags[0] ?? shortId(i.id)}</td>
                    <td className="small muted">{i.containers > 0 ? `${i.containers} container(s)` : "unused"}</td>
                    <td className="num">{bytes(i.size)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
        <div className="card">
          <div className="card-head">
            <h3>Largest volumes</h3>
          </div>
          <div className="card-body table-wrap">
            {largestVolumes.length === 0 ? (
              <div className="small muted">No volume data.</div>
            ) : (
              <table className="table">
                <tbody>
                  {largestVolumes.map((v) => (
                    <tr key={v.name}>
                      <td className="mono small ellipsis" style={{ maxWidth: 320 }}>{v.name.length === 64 ? shortId(v.name) : v.name}</td>
                      <td className="small muted">{v.ref_count > 0 ? "in use" : "unused"}</td>
                      <td className="num">{bytes(v.size)}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
