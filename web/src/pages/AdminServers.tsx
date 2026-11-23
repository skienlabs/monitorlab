import { useEffect, useState } from "react";
import { Link, useSearchParams } from "react-router-dom";
import { KeyRound, MoreVertical, Pencil, Plus, ServerCog, Trash2 } from "lucide-react";
import { api } from "../api";
import { useServers } from "../servers";
import { CodeBlock, Dialog, Empty, Loading, Menu, Seg, Spinner, StatusChip, useConfirm, usePoll, useToast } from "../components/ui";
import { OsIcon } from "../components/OsIcon";
import { ago, dateTime } from "../format";
import type { Server } from "../types";

interface HubInfo {
  version: string;
  tls: boolean;
  self_signed: boolean;
  fingerprint: string | null;
  spki_pin: string | null;
  public_url: string | null;
  downloads: { linux_amd64: boolean; linux_arm64: boolean; windows_amd64: boolean };
}
interface Token {
  id: string;
  name: string;
  token: string;
  expires_at: number;
}

function InstallSteps({ t, info }: { t: Token; info: HubInfo }) {
  const [os, setOs] = useState<"linux" | "windows">("linux");
  const hub = (info.public_url || location.origin).replace(/\/$/, "");
  const fp = info.self_signed ? info.fingerprint : null;
  const pin = info.self_signed ? info.spki_pin : null;
  const linux =
    `curl -fsSL ${pin ? `-k --pinnedpubkey '${pin}' ` : ""}${hub}/download/install.sh | sudo sh -s -- \\\n` +
    `  --hub ${hub} --token ${t.token}${fp ? ` \\\n  --fingerprint ${fp}` : ""}${pin ? ` \\\n  --pin '${pin}'` : ""}`;
  const win = `.\\vigil-agent-windows-amd64.exe install --hub ${hub} --token ${t.token}${fp ? ` \`\n  --fingerprint ${fp}` : ""}`;
  const local = !info.public_url && /^(localhost|127\.|\[::1\])/.test(location.hostname);
  return (
    <div className="col" style={{ gap: 14 }}>
      {local && <div className="banner warn small">You are using the hub via {location.host}. Other servers cannot reach that address — set VIGIL_PUBLIC_URL on the hub, or open the hub by its network address.</div>}
      <Seg value={os} onChange={setOs} options={[{ value: "linux", label: "Linux (systemd)" }, { value: "windows", label: "Windows Server" }]} />
      {os === "linux" ? (
        <>
          <div className="small">Run on the server you want to monitor. It downloads the agent from this hub{pin ? " (certificate-pinned)" : ""}, enrolls it and installs a systemd service that runs as an unprivileged <span className="mono">vigil</span> user with Docker access.</div>
          <CodeBlock>{linux}</CodeBlock>
          {!info.downloads.linux_amd64 && <div className="banner warn small">This hub image does not bundle the Linux agent binary. Build it with <span className="mono">cargo build -p vigil-agent --release</span> and place it in the agents directory.</div>}
        </>
      ) : (
        <>
          <ol className="small" style={{ margin: 0, paddingLeft: 18, display: "flex", flexDirection: "column", gap: 8 }}>
            <li>
              Download{" "}
              {info.downloads.windows_amd64 ? <a href="/download/vigil-agent-windows-amd64.exe">vigil-agent-windows-amd64.exe</a> : <span className="mono">vigil-agent-windows-amd64.exe</span>} and copy it to the Windows server.
            </li>
            <li>Open <b>PowerShell as Administrator</b> in that folder and run:</li>
          </ol>
          <CodeBlock>{win}</CodeBlock>
          <div className="small muted">Installs the <span className="mono">VigilAgent</span> service (LocalSystem, automatic start, restart on failure). It collects system metrics, IIS sites, pools and logs, and SQL Server status and error logs.</div>
        </>
      )}
      {fp && (
        <div className="small faint">
          Hub certificate fingerprint (SHA-256): <span className="mono" style={{ wordBreak: "break-all" }}>{fp}</span>
        </div>
      )}
      <div className="small faint">The token can be used once and expires {dateTime(t.expires_at)}.</div>
    </div>
  );
}

export function AdminServers() {
  const { reload: reloadNav } = useServers();
  const { data: servers, reload } = usePoll(() => api<Server[]>("/servers"), [], 10_000);
  const { data: info } = usePoll(() => api<HubInfo>("/hub/info"), []);
  const [params, setParams] = useSearchParams();
  const [adding, setAdding] = useState(false);
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);
  const [token, setToken] = useState<Token | null>(null);
  const [rename, setRename] = useState<Server | null>(null);
  const [newName, setNewName] = useState("");
  const toast = useToast();
  const confirm = useConfirm();

  useEffect(() => {
    if (params.get("add")) {
      setAdding(true);
      setParams({}, { replace: true });
    }
  }, [params, setParams]);

  const refreshAll = () => {
    reload();
    reloadNav();
  };

  const create = async () => {
    setBusy(true);
    try {
      const t = await api<Token>("/servers", { method: "POST", body: { name } });
      setToken(t);
      setAdding(false);
      setName("");
      refreshAll();
    } catch (e: any) {
      toast(e.message, true);
    } finally {
      setBusy(false);
    }
  };

  const newToken = async (s: Server) => {
    try {
      setToken(await api<Token>(`/servers/${s.id}/enroll-token`, { method: "POST" }));
      refreshAll();
    } catch (e: any) {
      toast(e.message, true);
    }
  };

  const doRename = async () => {
    if (!rename) return;
    try {
      await api(`/servers/${rename.id}`, { method: "PATCH", body: { name: newName } });
      toast("Server renamed");
      setRename(null);
      refreshAll();
    } catch (e: any) {
      toast(e.message, true);
    }
  };

  if (!servers || !info) return <Loading />;

  return (
    <div className="page">
      <div className="page-head">
        <div className="grow">
          <h1>Servers & agents</h1>
          <div className="sub">Agents connect out to this hub over {info.tls ? "TLS" : "HTTP"}; monitored servers need no open ports.</div>
        </div>
        <button className="btn filled" onClick={() => setAdding(true)}>
          <Plus size={18} /> Add server
        </button>
      </div>

      <div className="card table-wrap">
        {servers.length === 0 ? (
          <Empty icon={<ServerCog size={26} />} title="No servers yet" />
        ) : (
          <table className="table">
            <thead>
              <tr>
                <th>Server</th>
                <th>Status</th>
                <th>Agent</th>
                <th>Address</th>
                <th>Last seen</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {servers.map((s) => (
                <tr key={s.id}>
                  <td>
                    <div className="row" style={{ gap: 12 }}>
                      <OsIcon os={s.hello?.os} />
                      <div>
                        <Link to={`/servers/${s.id}`} style={{ fontWeight: 500, color: "var(--text)" }}>{s.name}</Link>
                        <div className="small faint">{s.hello ? `${s.hello.hostname} · ${s.hello.os_name}` : "Not enrolled yet"}{s.is_local ? " · hub host" : ""}</div>
                      </div>
                    </div>
                  </td>
                  <td>
                    <StatusChip status={s.status} />
                    {!s.enrolled && s.token_pending && <div className="small faint" style={{ marginTop: 2 }}>token issued</div>}
                  </td>
                  <td className="small">{s.hello ? `v${s.hello.agent_version}` : "—"}</td>
                  <td className="mono small">{s.is_local ? "local" : s.remote ?? s.hello?.ips[0] ?? "—"}</td>
                  <td className="small muted">{s.online ? "now" : ago(s.last_seen_at)}</td>
                  <td className="actions">
                    {!s.enrolled && !s.is_local && (
                      <button className="btn sm tonal" onClick={() => newToken(s)}>
                        <KeyRound size={15} /> Install command
                      </button>
                    )}
                    <Menu trigger={<MoreVertical size={17} />}>
                      <button onClick={() => { setRename(s); setNewName(s.name); }}>
                        <Pencil size={17} /> Rename
                      </button>
                      {!s.is_local && (
                        <button onClick={() => newToken(s)}>
                          <KeyRound size={17} /> {s.enrolled ? "Re-install / rotate credentials" : "New install command"}
                        </button>
                      )}
                      {!s.is_local && (
                        <>
                          <hr />
                          <button
                            className="danger"
                            onClick={() =>
                              confirm({
                                title: `Remove ${s.name}?`,
                                body: "The server, its credentials and all stored metrics are deleted from the hub. The agent is disconnected and can no longer reconnect. Uninstall it on the server with: vigil-agent uninstall --purge",
                                confirm: "Remove server",
                                danger: true,
                                typeToConfirm: s.name,
                                action: () => api(`/servers/${s.id}`, { method: "DELETE" }).then(refreshAll),
                                success: `${s.name} removed`,
                              })
                            }
                          >
                            <Trash2 size={17} /> Remove server
                          </button>
                        </>
                      )}
                    </Menu>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>

      {adding && (
        <Dialog
          title="Add a server"
          onClose={() => setAdding(false)}
          footer={
            <>
              <button className="btn" onClick={() => setAdding(false)}>Cancel</button>
              <button className="btn filled" disabled={!name.trim() || busy} onClick={create}>
                {busy && <Spinner />} Continue
              </button>
            </>
          }
        >
          <div className="field">
            <label>Display name</label>
            <input className="input" autoFocus placeholder="e.g. prod-api-01" value={name} maxLength={64} onChange={(e) => setName(e.target.value)} onKeyDown={(e) => e.key === "Enter" && name.trim() && create()} />
            <span className="hint">You will get a one-time install command for Linux or Windows.</span>
          </div>
        </Dialog>
      )}

      {token && (
        <Dialog wide title={`Install the agent on ${token.name}`} onClose={() => setToken(null)} footer={<button className="btn filled" onClick={() => setToken(null)}>Done</button>}>
          <InstallSteps t={token} info={info} />
        </Dialog>
      )}

      {rename && (
        <Dialog
          title="Rename server"
          onClose={() => setRename(null)}
          footer={
            <>
              <button className="btn" onClick={() => setRename(null)}>Cancel</button>
              <button className="btn filled" disabled={!newName.trim()} onClick={doRename}>Save</button>
            </>
          }
        >
          <input className="input" autoFocus value={newName} maxLength={64} onChange={(e) => setNewName(e.target.value)} onKeyDown={(e) => e.key === "Enter" && doRename()} />
        </Dialog>
      )}
    </div>
  );
}
