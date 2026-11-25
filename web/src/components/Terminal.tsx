import { useEffect, useRef, useState } from "react";
import { Terminal as XTerm } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { RotateCw } from "lucide-react";
import { openStream, StreamConn } from "../api";

function b64ToBytes(s: string) {
  const bin = atob(s);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

export function ContainerTerminal({ serverId, containerId }: { serverId: string; containerId: string }) {
  const [shell, setShell] = useState("auto");
  const [user, setUser] = useState("");
  return (
    <ExecTerminal
      serverId={serverId}
      deps={[containerId, shell, user]}
      request={(cols, rows) => ({ kind: "container_exec", id: containerId, cmd: shell === "auto" ? undefined : [shell], user: user || undefined, cols, rows })}
      where="the container"
      controls={
        <>
          <ShellSelect value={shell} onChange={setShell} />
          <input className="input" style={{ height: 36, width: 160 }} placeholder="User (default)" value={user} onChange={(e) => setUser(e.target.value.trim())} aria-label="Run as user" />
        </>
      }
    />
  );
}

/** Shell in a Kubernetes pod container. */
export function PodTerminal({ serverId, namespace, pod, containers }: { serverId: string; namespace: string; pod: string; containers: string[] }) {
  const [shell, setShell] = useState("auto");
  const [container, setContainer] = useState(containers[0] ?? "");
  return (
    <ExecTerminal
      serverId={serverId}
      deps={[namespace, pod, container, shell]}
      request={(cols, rows) => ({ kind: "k8s_pod_exec", namespace, name: pod, container: container || undefined, cmd: shell === "auto" ? undefined : [shell], cols, rows })}
      where="the pod"
      controls={
        <>
          {containers.length > 1 && (
            <select className="select" style={{ height: 36, width: 180 }} value={container} onChange={(e) => setContainer(e.target.value)} aria-label="Container">
              {containers.map((c) => (
                <option key={c} value={c}>{c}</option>
              ))}
            </select>
          )}
          <ShellSelect value={shell} onChange={setShell} />
        </>
      }
    />
  );
}

function ShellSelect({ value, onChange }: { value: string; onChange: (v: string) => void }) {
  return (
    <select className="select" style={{ height: 36, width: 160 }} value={value} onChange={(e) => onChange(e.target.value)} aria-label="Shell">
      <option value="auto">Auto (bash → sh)</option>
      <option value="/bin/bash">/bin/bash</option>
      <option value="/bin/sh">/bin/sh</option>
      <option value="/bin/ash">/bin/ash</option>
    </select>
  );
}

function ExecTerminal({
  serverId,
  request,
  deps,
  controls,
  where,
}: {
  serverId: string;
  request: (cols: number, rows: number) => Record<string, unknown>;
  deps: unknown[];
  controls: React.ReactNode;
  where: string;
}) {
  const box = useRef<HTMLDivElement>(null);
  const [session, setSession] = useState(0);
  const [state, setState] = useState<"connecting" | "open" | "closed">("connecting");
  const [msg, setMsg] = useState<string | null>(null);

  useEffect(() => {
    const el = box.current;
    if (!el) return;
    const term = new XTerm({
      fontFamily: getComputedStyle(document.documentElement).getPropertyValue("--mono"),
      fontSize: 13,
      lineHeight: 1.2,
      cursorBlink: true,
      scrollback: 5000,
      theme: { background: "#101214", foreground: "#e3e3e3", cursor: "#a8c7fa", selectionBackground: "#3a4a63" },
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(el);
    try {
      fit.fit();
    } catch {
      /* not visible yet */
    }
    setState("connecting");
    setMsg(null);
    const conn: StreamConn = openStream(
      serverId,
      request(term.cols, term.rows),
      {
        onOpen: () => {
          setState("open");
          term.focus();
        },
        onData: (d) => d.k === "bytes" && term.write(b64ToBytes(d.b64)),
        onEnd: (err) => {
          setState("closed");
          setMsg(err);
          term.write(`\r\n\x1b[2m— session ended${err ? `: ${err}` : ""} —\x1b[0m\r\n`);
        },
      },
    );
    const sub = term.onData((d) => conn.input(d));
    const ro = new ResizeObserver(() => {
      try {
        fit.fit();
        conn.resize(term.cols, term.rows);
      } catch {
        /* ignore */
      }
    });
    ro.observe(el);
    return () => {
      ro.disconnect();
      sub.dispose();
      conn.close();
      term.dispose();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [serverId, session, ...deps]);

  return (
    <div>
      <div className="term-bar">
        {controls}
        <span className="small muted">
          {state === "open" ? `Connected — commands run inside ${where} and are audited.` : state === "connecting" ? "Connecting…" : msg ? `Closed: ${msg}` : "Session closed"}
        </span>
        <span className="spacer" />
        <button className="btn sm tonal" onClick={() => setSession((s) => s + 1)}>
          <RotateCw size={15} /> New session
        </button>
      </div>
      <div className="terminal-wrap">
        <div ref={box} style={{ height: "100%" }} />
      </div>
    </div>
  );
}
