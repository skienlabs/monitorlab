import { createContext, ReactNode, useCallback, useContext, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { Check, Copy, Search as SearchIcon, X } from "lucide-react";
import type { Status } from "../types";

// ------------------------------------------------------------------ toasts

type Toast = { id: number; msg: string; error?: boolean };
const ToastCtx = createContext<(msg: string, error?: boolean) => void>(() => {});

export function ToastProvider({ children }: { children: ReactNode }) {
  const [toasts, setToasts] = useState<Toast[]>([]);
  const push = useCallback((msg: string, error = false) => {
    const id = Date.now() + Math.random();
    setToasts((t) => [...t.slice(-3), { id, msg, error }]);
    setTimeout(() => setToasts((t) => t.filter((x) => x.id !== id)), error ? 7000 : 4000);
  }, []);
  return (
    <ToastCtx.Provider value={push}>
      {children}
      <div className="toasts" role="status" aria-live="polite">
        {toasts.map((t) => (
          <div key={t.id} className={`toast ${t.error ? "error" : ""}`}>
            <span className="grow">{t.msg}</span>
          </div>
        ))}
      </div>
    </ToastCtx.Provider>
  );
}
export const useToast = () => useContext(ToastCtx);

// ------------------------------------------------------------------ dialogs

export function Dialog(props: { title: ReactNode; icon?: ReactNode; onClose: () => void; children: ReactNode; footer?: ReactNode; wide?: boolean }) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && props.onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [props.onClose]);
  return (
    <div className="scrim" onMouseDown={(e) => e.target === e.currentTarget && props.onClose()}>
      <div className={`dialog ${props.wide ? "wide" : ""}`} role="dialog" aria-modal="true">
        <div className="dialog-head">
          {props.icon}
          <h2 className="grow">{props.title}</h2>
          <button className="icon-btn sm" onClick={props.onClose} aria-label="Close">
            <X size={18} />
          </button>
        </div>
        <div className="dialog-body">{props.children}</div>
        {props.footer && <div className="dialog-foot">{props.footer}</div>}
      </div>
    </div>
  );
}

export interface ConfirmOpts {
  title: string;
  body: ReactNode;
  confirm: string;
  danger?: boolean;
  /** When set, the user must type this text to enable the confirm button. */
  typeToConfirm?: string;
  /** Optional checkbox (e.g. "force"); its value is passed to the action. */
  option?: string;
  action: (option: boolean) => Promise<unknown>;
  success?: string | ((result: any) => string);
}

const ConfirmCtx = createContext<(o: ConfirmOpts) => void>(() => {});
export const useConfirm = () => useContext(ConfirmCtx);

export function ConfirmProvider({ children }: { children: ReactNode }) {
  const [opts, setOpts] = useState<ConfirmOpts | null>(null);
  const [busy, setBusy] = useState(false);
  const [typed, setTyped] = useState("");
  const [opt, setOpt] = useState(false);
  const toast = useToast();
  const open = useCallback((o: ConfirmOpts) => {
    setTyped("");
    setOpt(false);
    setOpts(o);
  }, []);
  const close = useCallback(() => !busy && setOpts(null), [busy]);
  const run = async () => {
    if (!opts) return;
    setBusy(true);
    try {
      const r = await opts.action(opt);
      if (opts.success) toast(typeof opts.success === "function" ? opts.success(r) : opts.success);
      setOpts(null);
    } catch (e: any) {
      toast(e.message ?? String(e), true);
    } finally {
      setBusy(false);
    }
  };
  const blocked = !!opts?.typeToConfirm && typed.trim() !== opts.typeToConfirm;
  return (
    <ConfirmCtx.Provider value={open}>
      {children}
      {opts && (
        <Dialog
          title={opts.title}
          onClose={close}
          footer={
            <>
              <button className="btn" onClick={close} disabled={busy}>
                Cancel
              </button>
              <button className={`btn ${opts.danger ? "danger" : "filled"}`} onClick={run} disabled={busy || blocked}>
                {busy && <span className="spinner" style={{ width: 16, height: 16, borderTopColor: "#fff" }} />}
                {opts.confirm}
              </button>
            </>
          }
        >
          <div>{opts.body}</div>
          {opts.option && (
            <label className="check">
              <input type="checkbox" checked={opt} onChange={(e) => setOpt(e.target.checked)} />
              {opts.option}
            </label>
          )}
          {opts.typeToConfirm && (
            <div className="field">
              <label>
                Type <b className="mono">{opts.typeToConfirm}</b> to confirm
              </label>
              <input className="input" autoFocus value={typed} onChange={(e) => setTyped(e.target.value)} onKeyDown={(e) => e.key === "Enter" && !blocked && run()} />
            </div>
          )}
        </Dialog>
      )}
    </ConfirmCtx.Provider>
  );
}

// ------------------------------------------------------------------ menu

/** Row/overflow menu. Rendered in a portal with fixed positioning, so it is never clipped by a
 *  scrolling table or card, and it flips above the button when there is no room below. */
export function Menu({ trigger, children, label = "More actions" }: { trigger: ReactNode; children: ReactNode; label?: string }) {
  const [open, setOpen] = useState(false);
  const [pos, setPos] = useState<{ top: number; left: number } | null>(null);
  const btn = useRef<HTMLButtonElement>(null);
  const pop = useRef<HTMLDivElement>(null);

  useLayoutEffect(() => {
    if (!open) {
      setPos(null);
      return;
    }
    const place = () => {
      const b = btn.current?.getBoundingClientRect();
      const m = pop.current;
      if (!b || !m) return;
      const w = m.offsetWidth;
      const h = m.offsetHeight;
      const vw = window.innerWidth;
      const vh = window.innerHeight;
      let left = b.right - w;
      left = Math.max(8, Math.min(left, vw - w - 8));
      let top = b.bottom + 4;
      if (top + h > vh - 8 && b.top - 4 - h >= 8) top = b.top - 4 - h;
      top = Math.max(8, Math.min(top, vh - h - 8));
      setPos({ top, left });
    };
    place();
    const close = () => setOpen(false);
    // Follow the button when the page or a table scrolls; close once it leaves the screen.
    let raf = 0;
    const follow = (e: Event) => {
      if (pop.current?.contains(e.target as Node)) return;
      cancelAnimationFrame(raf);
      raf = requestAnimationFrame(() => {
        const b = btn.current?.getBoundingClientRect();
        if (!b || b.bottom < 0 || b.top > window.innerHeight || b.right < 0 || b.left > window.innerWidth) close();
        else place();
      });
    };
    const outside = (e: MouseEvent) => {
      const t = e.target as Node;
      if (!btn.current?.contains(t) && !pop.current?.contains(t)) setOpen(false);
    };
    const key = (e: KeyboardEvent) => e.key === "Escape" && setOpen(false);
    document.addEventListener("mousedown", outside);
    document.addEventListener("keydown", key);
    window.addEventListener("resize", close);
    window.addEventListener("scroll", follow, true);
    return () => {
      document.removeEventListener("mousedown", outside);
      document.removeEventListener("keydown", key);
      window.removeEventListener("resize", close);
      window.removeEventListener("scroll", follow, true);
      cancelAnimationFrame(raf);
    };
  }, [open]);

  return (
    <div className="menu-anchor">
      <button
        ref={btn}
        className="icon-btn sm"
        aria-label={label}
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={(e) => {
          e.stopPropagation();
          setOpen((o) => !o);
        }}
      >
        {trigger}
      </button>
      {open &&
        createPortal(
          <div
            ref={pop}
            className="menu"
            role="menu"
            style={{ top: pos?.top ?? 0, left: pos?.left ?? 0, visibility: pos ? "visible" : "hidden" }}
            onClick={(e) => {
              e.stopPropagation();
              setOpen(false);
            }}
          >
            {children}
          </div>,
          document.body,
        )}
    </div>
  );
}

// ------------------------------------------------------------------ small pieces

export function Spinner({ lg }: { lg?: boolean }) {
  return <span className={`spinner ${lg ? "lg" : ""}`} role="progressbar" aria-label="Loading" />;
}

export function Loading({ label = "Loading…" }: { label?: string }) {
  return (
    <div className="center-fill">
      <div className="col" style={{ alignItems: "center", gap: 12 }}>
        <Spinner lg />
        <span className="muted">{label}</span>
      </div>
    </div>
  );
}

export function Empty({ icon, title, children }: { icon: ReactNode; title: string; children?: ReactNode }) {
  return (
    <div className="empty">
      <div className="ico">{icon}</div>
      <h3>{title}</h3>
      {children && <div className="small" style={{ maxWidth: 460 }}>{children}</div>}
    </div>
  );
}

export function ErrorBanner({ error, onRetry }: { error: string; onRetry?: () => void }) {
  return (
    <div className="banner crit">
      <span className="grow">{error}</span>
      {onRetry && (
        <button className="btn sm" onClick={onRetry}>
          Retry
        </button>
      )}
    </div>
  );
}

const STATUS_LABEL: Record<Status, string> = { ok: "Healthy", warning: "Warning", critical: "Critical", offline: "Offline", pending: "Awaiting agent" };
const STATUS_CLASS: Record<Status, string> = { ok: "good", warning: "warn", critical: "crit", offline: "", pending: "info" };

export function StatusChip({ status }: { status: Status }) {
  return (
    <span className={`chip ${STATUS_CLASS[status]}`}>
      <span className={`dot ${STATUS_CLASS[status]} ${status === "ok" ? "pulse" : ""}`} />
      {STATUS_LABEL[status]}
    </span>
  );
}

export function StateChip({ state, health }: { state: string; health?: string | null }) {
  const s = state.toLowerCase();
  const cls =
    health === "unhealthy" || s === "dead" || s === "restarting"
      ? "crit"
      : s === "running" || s === "started"
        ? health === "starting"
          ? "warn"
          : "good"
        : s === "paused" || s === "starting" || s === "stopping"
          ? "warn"
          : "";
  const label = health && s === "running" ? `${s} · ${health}` : s;
  return (
    <span className={`chip ${cls}`}>
      <span className={`dot ${cls}`} />
      {label}
    </span>
  );
}

export function Meter({ value, warn = 80, crit = 92, lg }: { value: number; warn?: number; crit?: number; lg?: boolean }) {
  const v = Math.max(0, Math.min(100, value || 0));
  const cls = v >= crit ? "crit" : v >= warn ? "warn" : "";
  return (
    <div className={`meter ${cls} ${lg ? "lg" : ""}`} role="meter" aria-valuenow={Math.round(v)} aria-valuemin={0} aria-valuemax={100}>
      <span style={{ width: `${v}%` }} />
    </div>
  );
}

export function SearchBox({ value, onChange, placeholder = "Search", autoFocus }: { value: string; onChange: (s: string) => void; placeholder?: string; autoFocus?: boolean }) {
  return (
    <label className="search">
      <SearchIcon size={18} />
      <input value={value} onChange={(e) => onChange(e.target.value)} placeholder={placeholder} autoFocus={autoFocus} aria-label={placeholder} />
      {value && (
        <button className="icon-btn sm" onClick={() => onChange("")} aria-label="Clear search">
          <X size={16} />
        </button>
      )}
    </label>
  );
}

export function Seg<T extends string>({ value, options, onChange }: { value: T; options: { value: T; label: ReactNode }[]; onChange: (v: T) => void }) {
  return (
    <div className="seg" role="tablist">
      {options.map((o) => (
        <button key={o.value} className={o.value === value ? "on" : ""} onClick={() => onChange(o.value)} role="tab" aria-selected={o.value === value}>
          {o.label}
        </button>
      ))}
    </div>
  );
}

export function CopyButton({ text, label = "Copy" }: { text: string; label?: string }) {
  const [done, setDone] = useState(false);
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      // Clipboard API needs a secure context; fall back to a hidden textarea.
      const ta = document.createElement("textarea");
      ta.value = text;
      document.body.appendChild(ta);
      ta.select();
      document.execCommand("copy");
      ta.remove();
    }
    setDone(true);
    setTimeout(() => setDone(false), 1500);
  };
  return (
    <button className="icon-btn sm copy" onClick={copy} aria-label={label} title={label}>
      {done ? <Check size={16} /> : <Copy size={16} />}
    </button>
  );
}

export function CodeBlock({ children }: { children: string }) {
  return (
    <div className="code-block">
      <div style={{ paddingRight: 36 }}>{children}</div>
      <CopyButton text={children} />
    </div>
  );
}

// ------------------------------------------------------------------ data hooks

/** Fetch with optional polling; keeps the previous data visible while refreshing. */
export function usePoll<T>(fn: () => Promise<T>, deps: unknown[], intervalMs = 0) {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const fnRef = useRef(fn);
  fnRef.current = fn;
  const seq = useRef(0);
  const load = useCallback(async () => {
    const my = ++seq.current;
    try {
      const d = await fnRef.current();
      if (my === seq.current) {
        setData(d);
        setError(null);
      }
    } catch (e: any) {
      if (my === seq.current) setError(e.message ?? String(e));
    } finally {
      if (my === seq.current) setLoading(false);
    }
  }, []);
  useEffect(() => {
    setLoading(true);
    setData(null);
    setError(null);
    load();
    if (!intervalMs) return;
    const t = setInterval(() => document.visibilityState === "visible" && load(), intervalMs);
    return () => {
      clearInterval(t);
      seq.current++;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps);
  return { data, error, loading, reload: load, setData };
}

export function useLocalStorage<T>(key: string, initial: T): [T, (v: T) => void] {
  const [v, setV] = useState<T>(() => {
    try {
      const s = localStorage.getItem(key);
      return s ? (JSON.parse(s) as T) : initial;
    } catch {
      return initial;
    }
  });
  const set = useCallback(
    (nv: T) => {
      setV(nv);
      try {
        localStorage.setItem(key, JSON.stringify(nv));
      } catch {
        /* storage unavailable */
      }
    },
    [key],
  );
  return [v, set];
}

// ------------------------------------------------------------------ avatar

const AVATAR_COLORS = ["#4f46e5", "#9333ea", "#db2777", "#ea7a0c", "#0d9488", "#2563eb", "#16a34a", "#c026d3"];

/** Profile photo, or a colored circle with the first letter of the name. */
export function Avatar({ src, name, size = 32 }: { src?: string | null; name: string; size?: number }) {
  const [broken, setBroken] = useState(false);
  useEffect(() => setBroken(false), [src]);
  const label = (name || "?").trim();
  let h = 0;
  for (const ch of label) h = (h * 31 + ch.charCodeAt(0)) >>> 0;
  const style = { width: size, height: size, fontSize: Math.round(size * 0.42) };
  if (src && !broken) return <img className="avatar avatar-img" src={src} alt="" style={style} onError={() => setBroken(true)} />;
  return (
    <span className="avatar" style={{ ...style, background: AVATAR_COLORS[h % AVATAR_COLORS.length] }} aria-hidden>
      {label.charAt(0).toUpperCase()}
    </span>
  );
}

// ------------------------------------------------------------------ sorting for list screens
type SortVal = string | number | boolean | null | undefined;
export type Accessors<T> = Record<string, (r: T) => SortVal>;

const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: "base" });
function cmp(a: SortVal, b: SortVal): number {
  const an = a === null || a === undefined || a === "";
  const bn = b === null || b === undefined || b === "";
  if (an || bn) return an === bn ? 0 : an ? 1 : -1; // empty values always last
  if (typeof a === "number" && typeof b === "number") return a - b;
  if (typeof a === "boolean" && typeof b === "boolean") return Number(a) - Number(b);
  return collator.compare(String(a), String(b));
}

/**
 * Click-to-sort for any table. `th(key, label)` renders a sortable header; `rows` is the sorted list.
 * Click once for ascending, again for descending, a third time to go back to the default order.
 */
export function useSort<T>(rows: T[] | null | undefined, acc: Accessors<T>, initial?: { key: string; dir?: "asc" | "desc" }) {
  const [sort, setSort] = useState<{ key: string; dir: "asc" | "desc" } | null>(initial ? { key: initial.key, dir: initial.dir ?? "asc" } : null);
  const sorted = useMemo(() => {
    const list = rows ?? [];
    const f = sort ? acc[sort.key] : undefined;
    if (!sort || !f) return list;
    const m = sort.dir === "asc" ? 1 : -1;
    // Empty values stay last in both directions.
    return [...list].sort((x, y) => {
      const a = f(x), b = f(y);
      const ae = a === null || a === undefined || a === "";
      const be = b === null || b === undefined || b === "";
      if (ae || be) return ae === be ? 0 : ae ? 1 : -1;
      return m * cmp(a, b);
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [rows, sort]);
  const click = (key: string) =>
    setSort((cur) => (!cur || cur.key !== key ? { key, dir: "asc" } : cur.dir === "asc" ? { key, dir: "desc" } : initial ? { key: initial.key, dir: initial.dir ?? "asc" } : null));
  const th = (key: string, label: ReactNode, className = "") => {
    const on = sort?.key === key;
    return (
      <th
        className={`sortable ${on ? "sorted" : ""} ${className}`.trim()}
        onClick={() => click(key)}
        aria-sort={on ? (sort!.dir === "asc" ? "ascending" : "descending") : "none"}
        tabIndex={0}
        onKeyDown={(e) => (e.key === "Enter" || e.key === " ") && (e.preventDefault(), click(key))}
        title="Click to sort"
      >
        {label}
        <span className="sort-ind" aria-hidden>{on ? (sort!.dir === "asc" ? "▲" : "▼") : ""}</span>
      </th>
    );
  };
  return { rows: sorted, th, sort };
}

/** Wrap a table: `<Sorted rows={list} acc={{name: r => r.name}}>{(S) => <table>…{S.th("name","Name")}…{S.rows.map(…)}</table>}</Sorted>` */
export function Sorted<T>(props: { rows: T[]; acc: Accessors<T>; initial?: { key: string; dir?: "asc" | "desc" }; children: (s: ReturnType<typeof useSort<T>>) => ReactNode }) {
  const s = useSort(props.rows, props.acc, props.initial);
  return <>{props.children(s)}</>;
}
