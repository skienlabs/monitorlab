// Thin API client. All state-changing calls carry X-Vigil-Csrf (checked by the hub).

export class ApiError extends Error {
  status: number;
  constructor(status: number, message: string) {
    super(message);
    this.status = status;
  }
}

type Listener = (reason: "unauthorized" | "password") => void;
const listeners = new Set<Listener>();
export function onAuthProblem(l: Listener) {
  listeners.add(l);
  return () => {
    listeners.delete(l);
  };
}

export async function api<T = any>(path: string, opts: { method?: string; body?: unknown; signal?: AbortSignal } = {}): Promise<T> {
  const res = await fetch(`/api${path}`, {
    method: opts.method ?? "GET",
    credentials: "same-origin",
    headers: { "content-type": "application/json", "x-vigil-csrf": "1" },
    body: opts.body === undefined ? undefined : JSON.stringify(opts.body),
    signal: opts.signal,
  });
  let data: any = null;
  const text = await res.text();
  if (text) {
    try {
      data = JSON.parse(text);
    } catch {
      data = { error: text };
    }
  }
  if (!res.ok) {
    const msg = (data && data.error) || res.statusText || "Request failed";
    if (res.status === 401 && !path.startsWith("/auth/login")) listeners.forEach((l) => l("unauthorized"));
    if (res.status === 403 && msg === "password_change_required") listeners.forEach((l) => l("password"));
    throw new ApiError(res.status, msg);
  }
  return data as T;
}

export const rpc = <T = any>(serverId: string, req: Record<string, unknown>) =>
  api<T>(`/servers/${encodeURIComponent(serverId)}/rpc`, { method: "POST", body: req });

export interface StreamHandlers {
  onData: (d: any) => void;
  onEnd?: (error: string | null) => void;
  onOpen?: () => void;
}

export interface StreamConn {
  close: () => void;
  input: (s: string) => void;
  resize: (cols: number, rows: number) => void;
}

const enc = new TextEncoder();
function b64(s: string) {
  const bytes = enc.encode(s);
  let bin = "";
  for (let i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i]);
  return btoa(bin);
}

/** Open a live stream (logs, terminal, stats...) proxied by the hub to the agent. */
export function openStream(serverId: string, req: Record<string, unknown>, h: StreamHandlers): StreamConn {
  const proto = location.protocol === "https:" ? "wss:" : "ws:";
  const ws = new WebSocket(`${proto}//${location.host}/api/servers/${encodeURIComponent(serverId)}/stream`);
  let ended = false;
  const end = (err: string | null) => {
    if (ended) return;
    ended = true;
    h.onEnd?.(err);
  };
  ws.onopen = () => {
    ws.send(JSON.stringify(req));
    h.onOpen?.();
  };
  ws.onmessage = (ev) => {
    let m: any;
    try {
      m = JSON.parse(ev.data);
    } catch {
      return;
    }
    if (m.t === "data") h.onData(m.d);
    else if (m.t === "end") end(m.error ?? null);
  };
  ws.onerror = () => end("Connection error");
  ws.onclose = (ev) => end(ev.code === 1000 || ev.code === 1005 ? null : `Connection closed (${ev.code})`);
  return {
    close: () => {
      ended = true;
      try {
        ws.close(1000);
      } catch {
        /* ignore */
      }
    },
    input: (s: string) => ws.readyState === WebSocket.OPEN && ws.send(JSON.stringify({ t: "input", b64: b64(s) })),
    resize: (cols: number, rows: number) => ws.readyState === WebSocket.OPEN && ws.send(JSON.stringify({ t: "resize", cols, rows })),
  };
}
