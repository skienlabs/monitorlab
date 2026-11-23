import { createContext, ReactNode, useCallback, useContext, useEffect, useRef, useState } from "react";
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

export function Menu({ trigger, children, label = "More actions" }: { trigger: ReactNode; children: ReactNode; label?: string }) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const h = (e: MouseEvent) => ref.current && !ref.current.contains(e.target as Node) && setOpen(false);
    const k = (e: KeyboardEvent) => e.key === "Escape" && setOpen(false);
    document.addEventListener("mousedown", h);
    document.addEventListener("keydown", k);
    return () => {
      document.removeEventListener("mousedown", h);
      document.removeEventListener("keydown", k);
    };
  }, [open]);
  return (
    <div className="menu-anchor" ref={ref}>
      <button className="icon-btn sm" aria-label={label} aria-expanded={open} onClick={(e) => { e.stopPropagation(); setOpen((o) => !o); }}>
        {trigger}
      </button>
      {open && (
        <div className="menu" onClick={() => setOpen(false)}>
          {children}
        </div>
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
