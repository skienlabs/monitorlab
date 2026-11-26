import { Fragment, ReactNode, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ArrowDown, CaseSensitive, Clock, Download, Eraser, Pause, Play, Regex, RotateCw, WrapText } from "lucide-react";
import { openStream, StreamConn } from "../api";
import type { LogLine } from "../types";
import { SearchBox, Seg } from "./ui";

type Level = "error" | "warn" | "info" | "debug" | "";
interface Line {
  id: number;
  ts?: string;
  s: number;
  text: string;
  lvl: Level;
  json?: any;
  /** Container / unit the line came from (merged views). */
  src?: string;
}

/** Stable colour per source in merged logs. */
const SRC_COLORS = ["#2563eb", "#9333ea", "#0d9488", "#db2777", "#ea7a0c", "#16a34a", "#0891d1", "#c026d3", "#ca8a04", "#dc2626"];
function srcColor(s: string) {
  let h = 0;
  for (const ch of s) h = (h * 31 + ch.charCodeAt(0)) >>> 0;
  return SRC_COLORS[h % SRC_COLORS.length];
}

const MAX_LINES = 20_000;
const ROW = 20;
const ANSI = /\x1b\[[0-9;?]*[ -/]*[@-~]/g;
const ERR = /\b(error|err|fail(ed|ure)?|fatal|panic|exception|crit(ical)?|unhandled)\b/i;
const WARN = /\b(warn(ing)?)\b/i;
const DEBUG = /\b(debug|dbug|trace|trce|verbose)\b/i;
// W3C (IIS) lines end with "sc-status sc-substatus sc-win32-status time-taken".
const W3C_STATUS = /\s([1-5]\d\d)\s\d+\s\d+\s\d+\s*$/;
const PREFIX = /^\s*(?:\[?\s*)?(fail|crit|error|err|warn|warning|info|dbug|debug|trce|trace)\s*[:\]|]/i;

function levelOf(text: string, s: number, json: any): Level {
  if (json && typeof json === "object") {
    const v = String(json.level ?? json.lvl ?? json.severity ?? json["@l"] ?? json.Level ?? json.LogLevel ?? "").toLowerCase();
    if (v) {
      if (/^(err|error|fatal|crit|critical|panic|fail)/.test(v)) return "error";
      if (/^warn/.test(v)) return "warn";
      if (/^(debug|trace|verbose)/.test(v)) return "debug";
      return "info";
    }
  }
  if (text.startsWith("#")) return "debug";
  const w = W3C_STATUS.exec(text);
  if (w) return w[1].startsWith("5") ? "error" : w[1].startsWith("4") ? "warn" : "info";
  const p = PREFIX.exec(text);
  if (p) {
    const k = p[1].toLowerCase();
    if (["fail", "crit", "error", "err"].includes(k)) return "error";
    if (k.startsWith("warn")) return "warn";
    if (["dbug", "debug", "trce", "trace"].includes(k)) return "debug";
    return "info";
  }
  if (ERR.test(text)) return "error";
  if (WARN.test(text)) return "warn";
  if (DEBUG.test(text)) return "debug";
  return s === 2 ? "warn" : "";
}

function fmtTs(ts: string | undefined, withDate: boolean) {
  if (!ts) return "";
  const d = new Date(ts);
  if (isNaN(d.getTime())) return ts.slice(0, 23);
  const t = d.toLocaleTimeString(undefined, { hour12: false }) + "." + String(d.getMilliseconds()).padStart(3, "0");
  return withDate ? `${d.toLocaleDateString(undefined, { month: "short", day: "2-digit" })} ${t}` : t;
}

function highlight(text: string, re: RegExp | null): ReactNode {
  if (!re) return text;
  const out: ReactNode[] = [];
  let last = 0;
  re.lastIndex = 0;
  let m: RegExpExecArray | null;
  let guard = 0;
  while ((m = re.exec(text)) && guard++ < 200) {
    if (m[0].length === 0) {
      re.lastIndex++;
      continue;
    }
    out.push(text.slice(last, m.index), <mark key={m.index}>{m[0]}</mark>);
    last = m.index + m[0].length;
  }
  out.push(text.slice(last));
  return out;
}

export interface LogSource {
  serverId: string;
  /** Stream request without tail; `tail` and (for reconnects) `since` are added. */
  request: Record<string, unknown>;
  /** Auto-reconnect when the stream ends (containers that restart). */
  reconnect?: boolean;
  /** Supports Docker's `since` parameter for gap-free reconnects. */
  supportsSince?: boolean;
}

export function LogViewer({ source, fileName, height = "calc(100vh - 300px)", extraBar }: { source: LogSource; fileName: string; height?: string; extraBar?: ReactNode }) {
  const linesRef = useRef<Line[]>([]);
  const pending = useRef<Line[]>([]);
  const nextId = useRef(1);
  const [version, setVersion] = useState(0);
  const [tail, setTail] = useState("500");
  const [q, setQ] = useState("");
  const [useRegex, setUseRegex] = useState(false);
  const [caseSens, setCaseSens] = useState(false);
  const [lvl, setLvl] = useState<"all" | "error" | "warn">("all");
  const [stream, setStream] = useState<"all" | "out" | "err">("all");
  const [showTs, setShowTs] = useState(true);
  const [withDate, setWithDate] = useState(false);
  const [wrap, setWrap] = useState(false);
  const [follow, setFollow] = useState(true);
  const [paused, setPaused] = useState(false);
  const [status, setStatus] = useState<"connecting" | "live" | "ended">("connecting");
  const [endMsg, setEndMsg] = useState<string | null>(null);
  const [expanded, setExpanded] = useState<Set<number>>(new Set());
  const [newWhilePaused, setNewWhilePaused] = useState(0);
  const [attempt, setAttempt] = useState(0);
  const view = useRef<HTMLDivElement>(null);
  const [scrollTop, setScrollTop] = useState(0);
  const [viewH, setViewH] = useState(600);
  const conn = useRef<StreamConn | null>(null);
  const pausedRef = useRef(paused);
  pausedRef.current = paused;
  const lastTs = useRef<string | undefined>(undefined);
  const srcKey = JSON.stringify(source.request) + source.serverId;

  const flushTimer = useRef<number | null>(null);
  const scheduleFlush = useCallback(() => {
    if (flushTimer.current != null) return;
    flushTimer.current = window.setTimeout(() => {
      flushTimer.current = null;
      if (pausedRef.current) {
        setNewWhilePaused(pending.current.length);
        return;
      }
      if (pending.current.length) {
        const all = linesRef.current.concat(pending.current);
        pending.current = [];
        linesRef.current = all.length > MAX_LINES ? all.slice(all.length - MAX_LINES) : all;
        setVersion((v) => v + 1);
      }
    }, 100);
  }, []);

  // (Re)connect
  useEffect(() => {
    let cancelled = false;
    let retry: number | null = null;
    const isReconnect = attempt > 0 && linesRef.current.length > 0;
    if (!isReconnect) {
      linesRef.current = [];
      pending.current = [];
      lastTs.current = undefined;
      setExpanded(new Set());
      setVersion((v) => v + 1);
    }
    setStatus("connecting");
    setEndMsg(null);
    const req: Record<string, unknown> = { ...source.request, tail: tail === "all" ? 100000 : Number(tail) };
    if (isReconnect && source.supportsSince && lastTs.current) {
      req.since = Math.floor(new Date(lastTs.current).getTime() / 1000);
    }
    const sinceTs = isReconnect ? lastTs.current : undefined;
    conn.current = openStream(source.serverId, req, {
      onOpen: () => !cancelled && setStatus("live"),
      onData: (d) => {
        if (cancelled) return;
        if (d.k === "lines") {
          for (const l of d.lines as LogLine[]) {
            if (sinceTs && l.ts && l.ts <= sinceTs) continue; // dedupe after reconnect
            const text = l.text.replace(ANSI, "");
            let json: any;
            const t = text.trimStart();
            if (t.startsWith("{") && t.endsWith("}")) {
              try {
                json = JSON.parse(t);
              } catch {
                /* not JSON */
              }
            }
            if (l.ts) lastTs.current = l.ts;
            // Sources with real severities (journald priority, Event Log level) win over guessing.
            const given = (l as any).lvl as number | undefined;
            const lvl: Level = given == null ? levelOf(text, l.s, json) : given <= 3 ? "error" : given === 4 ? "warn" : "";
            pending.current.push({ id: nextId.current++, ts: l.ts, s: l.s, text, lvl, json, src: (l as any).src });
          }
          scheduleFlush();
        } else if (d.k === "info") {
          pending.current.push({ id: nextId.current++, s: 0, text: `— ${d.msg} —`, lvl: "" });
          scheduleFlush();
        }
      },
      onEnd: (err) => {
        if (cancelled) return;
        setStatus("ended");
        setEndMsg(err);
        if (source.reconnect && !(err && /permission|No such|not found|offline/i.test(err))) {
          retry = window.setTimeout(() => setAttempt((a) => a + 1), 3000);
        }
      },
    });
    return () => {
      cancelled = true;
      if (retry) clearTimeout(retry);
      conn.current?.close();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [srcKey, tail, attempt]);

  // Changing source/tail resets attempts.
  useEffect(() => setAttempt(0), [srcKey, tail]);

  useEffect(() => {
    if (!paused && pending.current.length) {
      setNewWhilePaused(0);
      scheduleFlush();
    }
  }, [paused, scheduleFlush]);

  const re = useMemo(() => {
    if (!q) return null;
    try {
      return new RegExp(useRegex ? q : q.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"), caseSens ? "g" : "gi");
    } catch {
      return null;
    }
  }, [q, useRegex, caseSens]);
  const badRegex = useRegex && q !== "" && re === null;

  const filtered = useMemo(() => {
    const all = linesRef.current;
    return all.filter((l) => {
      if (lvl === "error" && l.lvl !== "error") return false;
      if (lvl === "warn" && l.lvl !== "error" && l.lvl !== "warn") return false;
      if (stream === "out" && l.s === 2) return false;
      if (stream === "err" && l.s !== 2) return false;
      if (re) {
        re.lastIndex = 0;
        if (!re.test(l.text) && !(l.src && re.test(l.src))) return false;
      }
      return true;
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [version, lvl, stream, re]);

  const virtual = !wrap && expanded.size === 0;
  const NONVIRT_CAP = 3000;
  const shown = virtual ? filtered : filtered.slice(-NONVIRT_CAP);

  // Auto-scroll when following.
  useEffect(() => {
    const el = view.current;
    if (el && follow) el.scrollTop = el.scrollHeight;
  }, [shown.length, version, follow, wrap]);

  useEffect(() => {
    const el = view.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setViewH(el.clientHeight));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  const onScroll = () => {
    const el = view.current;
    if (!el) return;
    setScrollTop(el.scrollTop);
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 30;
    if (atBottom !== follow) setFollow(atBottom);
  };

  const jump = () => {
    setFollow(true);
    setPaused(false);
    const el = view.current;
    if (el) el.scrollTop = el.scrollHeight;
  };

  const download = () => {
    const body = linesRef.current.map((l) => (l.ts ? `${l.ts} ${l.text}` : l.text)).join("\n");
    const blob = new Blob([body + "\n"], { type: "text/plain" });
    const a = document.createElement("a");
    a.href = URL.createObjectURL(blob);
    a.download = `${fileName}-${new Date().toISOString().replace(/[:.]/g, "-")}.log`;
    a.click();
    setTimeout(() => URL.revokeObjectURL(a.href), 1000);
  };

  const toggleJson = (id: number) =>
    setExpanded((s) => {
      const n = new Set(s);
      n.has(id) ? n.delete(id) : n.add(id);
      return n;
    });

  let start = 0;
  let end = shown.length;
  if (virtual) {
    start = Math.max(0, Math.floor(scrollTop / ROW) - 30);
    end = Math.min(shown.length, Math.ceil((scrollTop + viewH) / ROW) + 30);
  }
  const rows = shown.slice(start, end);
  const errors = useMemo(() => linesRef.current.reduce((a, l) => a + (l.lvl === "error" ? 1 : 0), 0), [version]);

  return (
    <div className="logs" style={{ height }}>
      <div className="logs-bar">
        <SearchBox value={q} onChange={setQ} placeholder={useRegex ? "Regular expression" : "Search logs"} />
        <button className={`icon-btn sm ${caseSens ? "on" : ""}`} onClick={() => setCaseSens((v) => !v)} title="Match case" aria-label="Match case">
          <CaseSensitive size={18} />
        </button>
        <button className={`icon-btn sm ${useRegex ? "on" : ""}`} onClick={() => setUseRegex((v) => !v)} title="Regular expression" aria-label="Regular expression">
          <Regex size={17} />
        </button>
        <Seg value={lvl} onChange={setLvl} options={[{ value: "all", label: "All" }, { value: "warn", label: "Warn+" }, { value: "error", label: `Errors${errors ? ` ${errors}` : ""}` }]} />
        <Seg value={stream} onChange={setStream} options={[{ value: "all", label: "std*" }, { value: "out", label: "stdout" }, { value: "err", label: "stderr" }]} />
        <span className="spacer" />
        {extraBar}
        <select className="select" style={{ height: 32, width: "auto", paddingRight: 8, borderRadius: 999, fontSize: 13 }} value={tail} onChange={(e) => setTail(e.target.value)} aria-label="Lines to load">
          {["100", "500", "1000", "5000", "20000"].map((t) => (
            <option key={t} value={t}>
              Last {Number(t).toLocaleString()}
            </option>
          ))}
        </select>
        <button className={`icon-btn sm ${showTs ? "on" : ""}`} onClick={() => setShowTs((v) => !v)} onDoubleClick={() => setWithDate((v) => !v)} title="Timestamps (double-click: include date)" aria-label="Toggle timestamps">
          <Clock size={17} />
        </button>
        <button className={`icon-btn sm ${wrap ? "on" : ""}`} onClick={() => setWrap((v) => !v)} title="Wrap lines" aria-label="Wrap lines">
          <WrapText size={17} />
        </button>
        <button className="icon-btn sm" onClick={() => setPaused((p) => !p)} title={paused ? "Resume" : "Pause"} aria-label={paused ? "Resume" : "Pause"}>
          {paused ? <Play size={17} /> : <Pause size={17} />}
        </button>
        <button
          className="icon-btn sm"
          onClick={() => {
            linesRef.current = [];
            setExpanded(new Set());
            setVersion((v) => v + 1);
          }}
          title="Clear view"
          aria-label="Clear view"
        >
          <Eraser size={17} />
        </button>
        <button className="icon-btn sm" onClick={download} title="Download" aria-label="Download">
          <Download size={17} />
        </button>
      </div>

      <div className={`logs-view ${wrap ? "wrap" : ""}`} ref={view} onScroll={onScroll} role="log" aria-live="off">
        {virtual && <div style={{ height: start * ROW }} />}
        {rows.map((l) => {
          const isOpen = expanded.has(l.id);
          return (
            <Fragment key={l.id}>
              <div
                className={`ln ${l.lvl} ${l.s === 2 ? "stderr" : ""} ${l.s === 0 && l.text.startsWith("— ") ? "info-msg" : ""}`}
                style={virtual ? { height: ROW } : undefined}
                onClick={l.json ? () => toggleJson(l.id) : undefined}
                title={l.json ? "Click to expand JSON" : undefined}
              >
                {showTs && <span className="ts">{fmtTs(l.ts, withDate) || " "}</span>}
                {l.src && (
                  <span className="src" style={{ color: srcColor(l.src) }} title={l.src}>
                    {l.src}
                  </span>
                )}
                <span className="tx" style={showTs ? undefined : { paddingLeft: 10 }}>
                  {highlight(l.text, re)}
                </span>
              </div>
              {isOpen && <pre className="json" style={{ paddingLeft: showTs ? 120 : 14, margin: 0 }}>{JSON.stringify(l.json, null, 2)}</pre>}
            </Fragment>
          );
        })}
        {virtual && <div style={{ height: (shown.length - end) * ROW }} />}
        {shown.length === 0 && (
          <div className="chart-empty" style={{ height: "100%", fontFamily: "var(--font)" }}>
            {status === "connecting" ? "Connecting…" : linesRef.current.length ? "No lines match the filter" : "No log output yet"}
          </div>
        )}
      </div>
      {(!follow || paused) && shown.length > 0 && (
        <button className="btn filled sm jump-latest" onClick={jump}>
          <ArrowDown size={16} /> {paused && newWhilePaused ? `${newWhilePaused} new lines` : "Jump to latest"}
        </button>
      )}

      <div className="logs-foot">
        {status === "live" && (
          <span className="row" style={{ gap: 6 }}>
            <span className="dot good pulse" /> {paused ? "Paused" : "Live"}
          </span>
        )}
        {status === "connecting" && <span>Connecting…</span>}
        {status === "ended" && (
          <span className="row" style={{ gap: 6 }}>
            <span className="dot" /> {endMsg ? `Stream ended: ${endMsg}` : "Stream ended"}
            {source.reconnect ? " · reconnecting…" : ""}
            <button className="btn sm" onClick={() => setAttempt((a) => a + 1)}>
              <RotateCw size={14} /> Reconnect
            </button>
          </span>
        )}
        <span className="spacer" />
        {badRegex && <span style={{ color: "var(--crit-text)" }}>Invalid regular expression</span>}
        <span className="tnum">
          {q || lvl !== "all" || stream !== "all" ? `${filtered.length.toLocaleString()} of ` : ""}
          {linesRef.current.length.toLocaleString()} lines
        </span>
        {!virtual && filtered.length > NONVIRT_CAP && <span>· showing last {NONVIRT_CAP.toLocaleString()}</span>}
      </div>
    </div>
  );
}
