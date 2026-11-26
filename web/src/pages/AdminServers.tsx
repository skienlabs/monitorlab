import { useEffect, useState } from "react";
import { Link, useSearchParams } from "react-router-dom";
import { CloudDownload, KeyRound, MoreVertical, Pencil, Plus, ServerCog, Trash2 } from "lucide-react";
import { api } from "../api";
import { useServers } from "../servers";
import { CodeBlock, Dialog, Empty, Loading, Menu, Seg, Spinner, StatusChip, useConfirm, usePoll, useToast, Sorted } from "../components/ui";
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
  downloads: { linux_amd64: boolean; linux_arm64: boolean; windows_amd64: boolean; macos_arm64?: boolean; macos_amd64?: boolean };
}
interface Token {
  id: string;
  name: string;
  token: string;
  expires_at: number;
}

function InstallSteps({ t, info }: { t: Token; info: HubInfo }) {
  const [os, setOs] = useState<"linux" | "macos" | "windows">("linux");
  const hub = (info.public_url || location.origin).replace(/\/$/, "");
  const fp = info.self_signed ? info.fingerprint : null;
  const pin = info.self_signed ? info.spki_pin : null;
  const linux =
    `curl -fsSL ${pin ? `-k --pinnedpubkey '${pin}' ` : ""}${hub}/download/install.sh | sudo sh -s -- \\\n` +
    `  --hub ${hub} --token=${t.token}${fp ? ` \\\n  --fingerprint ${fp}` : ""}${pin ? ` \\\n  --pin '${pin}'` : ""}`;
  const win = `.\\monitorlab-agent-windows-amd64.exe install --hub ${hub} --token=${t.token}${fp ? ` \`\n  --fingerprint ${fp}` : ""}`;
  const local = !info.public_url && /^(localhost|127\.|\[::1\])/.test(location.hostname);
  return (
    <div className="col" style={{ gap: 14 }}>
      {local && <div className="banner warn small">You are using the hub via {location.host}. Other servers cannot reach that address — set MONITORLAB_PUBLIC_URL on the hub, or open the hub by its network address.</div>}
      <Seg value={os} onChange={setOs} options={[{ value: "linux", label: "Linux (systemd)" }, { value: "macos", label: "macOS" }, { value: "windows", label: "Windows Server" }]} />
      {os === "macos" ? (
        <>
          <div className="small">
            Run in Terminal on the Mac (Apple Silicon or Intel). It downloads the right agent from this hub, enrolls it and installs a <span className="mono">launchd</span> service that starts at boot and restarts itself.
          </div>
          <CodeBlock>{linux}</CodeBlock>
          {!info.downloads.macos_arm64 && !info.downloads.macos_amd64 && <div className="banner warn small">This hub does not bundle the macOS agent yet. It is added by the release build.</div>}
          <div className="small muted">Shows CPU, memory, disks, processes and Docker containers (Docker Desktop, OrbStack or Colima). Services and system logs are not available on macOS yet.</div>
        </>
      ) : os === "linux" ? (
        <>
          <div className="small">Run on the server you want to monitor. It downloads the agent from this hub{pin ? " (certificate-pinned)" : ""}, enrolls it and installs a systemd service that runs as an unprivileged <span className="mono">monitorlab</span> user with Docker access.</div>
          <CodeBlock>{linux}</CodeBlock>
          {!info.downloads.linux_amd64 && <div className="banner warn small">This hub image does not bundle the Linux agent binary. Build it with <span className="mono">cargo build -p monitorlab-agent --release</span> and place it in the agents directory.</div>}
        </>
      ) : (
        <>
          <ol className="small" style={{ margin: 0, paddingLeft: 18, display: "flex", flexDirection: "column", gap: 8 }}>
            <li>
              Download{" "}
              {info.downloads.windows_amd64 ? <a href="/download/monitorlab-agent-windows-amd64.exe">monitorlab-agent-windows-amd64.exe</a> : <span className="mono">monitorlab-agent-windows-amd64.exe</span>} and copy it to the Windows server.
            </li>
            <li>Open <b>PowerShell as Administrator</b> in that folder and run:</li>
          </ol>
          <CodeBlock>{win}</CodeBlock>
          <div className="small muted">Installs the <span className="mono">MonitorLabAgent</span> service (LocalSystem, automatic start, restart on failure). It collects system metrics, IIS sites, pools and logs, and SQL Server status and error logs.</div>
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
  const updates = usePoll(() => api<{ auto_update: boolean; hub_version: string }>("/agent-updates"), []);
  const updateAgent = (s: Server) =>
    confirm({
      title: `Update the agent on ${s.name}?`,
      body: `The agent downloads v${updates.data?.hub_version ?? "latest"} from this hub, checks it and restarts. Monitoring pauses for a few seconds.`,
      confirm: "Update",
      action: () => api<{ message: string }>(`/servers/${s.id}/update`, { method: "POST" }).then((r) => { toast(r.message); reload(); }),
    });

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

      {updates.data && (
        <div className="card card-pad row wrap" style={{ marginBottom: 16, gap: 12 }}>
          <CloudDownload size={20} />
          <div className="grow">
            <b>Agent updates</b>
            <div className="small muted">
              Hub version {updates.data.hub_version}. {updates.data.auto_update ? "Agents update themselves to this version when they connect." : "Agents are only updated when you click Update."} Agents older than 1.3 need the installer run once.
            </div>
          </div>
          <label className="check">
            <input
              type="checkbox"
              checked={updates.data.auto_update}
              onChange={(e) => api("/agent-updates", { method: "PUT", body: { auto_update: e.target.checked } }).then(updates.reload).catch((err) => toast(err.message, true))}
            />{" "}
            Update agents automatically
          </label>
        </div>
      )}
      <div className="card table-wrap">
        {servers.length === 0 ? (
          <Empty icon={<ServerCog size={26} />} title="No servers yet" />
        ) : (
          <Sorted rows={servers} acc={{ name: s => s.name, status: s => s.online ? 1 : 0, agent: s => s.hello?.agent_version, address: s => s.remote, seen: s => s.last_seen_at }}>{(S) => (
<table className="table">
            <thead>
              <tr>
                {S.th("name", "Server")}
                {S.th("status", "Status")}
                {S.th("agent", "Agent")}
                {S.th("address", "Address")}
                {S.th("seen", "Last seen")}
                <th />
              </tr>
            </thead>
            <tbody>
              {S.rows.map((s) => (
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
                  <td className="small">
                    {s.hello ? `v${s.hello.agent_version}` : "—"}
                    {s.update_available && (
                      <div>
                        {s.hello?.capabilities.self_update ? (
                          <button className="btn sm tonal" style={{ marginTop: 4 }} onClick={() => updateAgent(s)}>
                            <CloudDownload size={14} /> Update
                          </button>
                        ) : (
                          <span className="chip warn" title="This agent predates self-update: re-run the installer on the server once (Add server → copy the command)">reinstall to update</span>
                        )}
                      </div>
                    )}
                    {s.agent_update && !s.agent_update.ok && <div className="small" style={{ color: "var(--crit-text)", maxWidth: 220 }} title={s.agent_update.message}>Update failed: {s.agent_update.message}</div>}
                  </td>
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
                                body: "The server, its credentials and all stored metrics are deleted from the hub. The agent is disconnected and can no longer reconnect. Uninstall it on the server with: monitorlab-agent uninstall --purge",
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
)}</Sorted>
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
