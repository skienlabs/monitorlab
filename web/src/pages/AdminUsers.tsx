import { useState } from "react";
import { Ban, CircleCheck, KeyRound, MoreVertical, Pencil, Plus, Trash2, Users, Wand2 } from "lucide-react";
import { api } from "../api";
import { useAuth } from "../auth";
import { useServers } from "../servers";
import { CopyButton, Dialog, Empty, Loading, Menu, SearchBox, Spinner, useConfirm, usePoll, useToast } from "../components/ui";
import { ago } from "../format";

interface User {
  id: number;
  email: string;
  name: string;
  role_id: number;
  role: string;
  server_scope: string[] | null;
  disabled: boolean;
  must_change_password: boolean;
  created_at: number;
  last_login_at: number | null;
  sessions: number;
}
export interface Role {
  id: number;
  name: string;
  description: string;
  permissions: string[];
  builtin: boolean;
  locked: boolean;
  users: number;
}

function genPassword() {
  const chars = "ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnpqrstuvwxyz23456789";
  const buf = new Uint32Array(16);
  crypto.getRandomValues(buf);
  return Array.from(buf, (n) => chars[n % chars.length]).join("");
}

function PasswordField({ value, onChange, label = "Password" }: { value: string; onChange: (v: string) => void; label?: string }) {
  return (
    <div className="field">
      <label>{label}</label>
      <div className="row">
        <input className="input mono" value={value} onChange={(e) => onChange(e.target.value)} placeholder="At least 10 characters" autoComplete="new-password" />
        <button type="button" className="icon-btn" title="Generate" aria-label="Generate password" onClick={() => onChange(genPassword())}>
          <Wand2 size={18} />
        </button>
        {value && <CopyButton text={value} />}
      </div>
      <span className="hint">Share it with the user through a secure channel.</span>
    </div>
  );
}

function UserDialog({ user, roles, onClose, onSaved }: { user: User | null; roles: Role[]; onClose: () => void; onSaved: () => void }) {
  const { me } = useAuth();
  const { servers } = useServers();
  const toast = useToast();
  const [email, setEmail] = useState(user?.email ?? "");
  const [name, setName] = useState(user?.name ?? "");
  const [roleId, setRoleId] = useState<number>(user?.role_id ?? roles.find((r) => r.name === "Viewer")?.id ?? roles[0]?.id);
  const [password, setPassword] = useState(user ? "" : genPassword());
  const [mustChange, setMustChange] = useState(true);
  const [allServers, setAllServers] = useState(user ? user.server_scope === null : true);
  const [scope, setScope] = useState<string[]>(user?.server_scope ?? []);
  const [busy, setBusy] = useState(false);
  const self = user?.id === me?.id;
  const role = roles.find((r) => r.id === roleId);

  const save = async () => {
    setBusy(true);
    try {
      const server_scope = allServers ? null : scope;
      if (user) {
        await api(`/users/${user.id}`, { method: "PATCH", body: { name, role_id: roleId, server_scope } });
        toast("User updated");
      } else {
        await api("/users", { method: "POST", body: { email, name, password, role_id: roleId, server_scope, must_change_password: mustChange } });
        toast(`User ${email} created`);
      }
      onSaved();
      onClose();
    } catch (e: any) {
      toast(e.message, true);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog
      wide
      title={user ? `Edit ${user.email}` : "Add user"}
      onClose={onClose}
      footer={
        <>
          <button className="btn" onClick={onClose}>Cancel</button>
          <button className="btn filled" disabled={busy || (!user && (!email || password.length < 10)) || (!allServers && scope.length === 0)} onClick={save}>
            {busy && <Spinner />} {user ? "Save" : "Create user"}
          </button>
        </>
      }
    >
      <div className="grid grid-2">
        <div className="field">
          <label>Email</label>
          <input className="input" type="email" value={email} disabled={!!user} onChange={(e) => setEmail(e.target.value)} autoFocus={!user} />
        </div>
        <div className="field">
          <label>Name</label>
          <input className="input" value={name} onChange={(e) => setName(e.target.value)} />
        </div>
      </div>
      {!user && (
        <>
          <PasswordField value={password} onChange={setPassword} label="Initial password" />
          <label className="check">
            <input type="checkbox" checked={mustChange} onChange={(e) => setMustChange(e.target.checked)} />
            Require a new password at first sign-in
          </label>
        </>
      )}
      <div className="field">
        <label>Role</label>
        <select className="select" value={roleId} disabled={self} onChange={(e) => setRoleId(Number(e.target.value))}>
          {roles.map((r) => (
            <option key={r.id} value={r.id}>{r.name}</option>
          ))}
        </select>
        {role && <span className="hint">{role.description || `${role.permissions.length} permissions`}</span>}
        {self && <span className="hint">You cannot change your own role.</span>}
      </div>
      <div className="field">
        <label>Server access</label>
        <div className="row" style={{ gap: 20 }}>
          <label className="check">
            <input type="radio" checked={allServers} disabled={self} onChange={() => setAllServers(true)} /> All servers (including future ones)
          </label>
          <label className="check">
            <input type="radio" checked={!allServers} disabled={self} onChange={() => setAllServers(false)} /> Only selected servers
          </label>
        </div>
        {!allServers && (
          <div className="card" style={{ padding: "10px 14px", maxHeight: 220, overflowY: "auto", display: "flex", flexDirection: "column", gap: 8 }}>
            {(servers ?? []).map((s) => (
              <label key={s.id} className="check">
                <input type="checkbox" checked={scope.includes(s.id)} onChange={(e) => setScope((v) => (e.target.checked ? [...v, s.id] : v.filter((x) => x !== s.id)))} />
                {s.name}
              </label>
            ))}
          </div>
        )}
      </div>
    </Dialog>
  );
}

export function AdminUsers() {
  const { me } = useAuth();
  const toast = useToast();
  const confirm = useConfirm();
  const { data: users, reload } = usePoll(() => api<User[]>("/users"), []);
  const { data: roles } = usePoll(() => api<Role[]>("/roles"), []);
  const { servers } = useServers();
  const [q, setQ] = useState("");
  const [editing, setEditing] = useState<User | null | "new">(null);
  const [resetFor, setResetFor] = useState<User | null>(null);
  const [newPw, setNewPw] = useState("");

  if (!users || !roles) return <Loading />;
  const needle = q.trim().toLowerCase();
  const list = users.filter((u) => !needle || u.email.includes(needle) || u.name.toLowerCase().includes(needle) || u.role.toLowerCase().includes(needle));

  const patch = async (u: User, body: object, msg: string) => {
    try {
      await api(`/users/${u.id}`, { method: "PATCH", body });
      toast(msg);
      reload();
    } catch (e: any) {
      toast(e.message, true);
    }
  };

  const scopeLabel = (u: User) => {
    if (u.server_scope === null) return "All servers";
    const names = u.server_scope.map((id) => servers?.find((s) => s.id === id)?.name).filter(Boolean);
    return names.length <= 2 ? names.join(", ") || "None" : `${names.length} servers`;
  };

  return (
    <div className="page">
      <div className="page-head">
        <div className="grow">
          <h1>Users</h1>
          <div className="sub">People who can sign in, their role and which servers they can see</div>
        </div>
        <button className="btn filled" onClick={() => setEditing("new")}>
          <Plus size={18} /> Add user
        </button>
      </div>
      <div className="row" style={{ marginBottom: 16 }}>
        <SearchBox value={q} onChange={setQ} placeholder="Search users" />
      </div>
      <div className="card table-wrap">
        {list.length === 0 ? (
          <Empty icon={<Users size={26} />} title="No users match" />
        ) : (
          <table className="table">
            <thead>
              <tr><th>User</th><th>Role</th><th>Server access</th><th>Status</th><th>Last sign-in</th><th /></tr>
            </thead>
            <tbody>
              {list.map((u) => (
                <tr key={u.id}>
                  <td>
                    <div className="row" style={{ gap: 12 }}>
                      <span className="avatar" style={{ background: u.disabled ? "var(--text-3)" : undefined }}>{(u.name || u.email).charAt(0).toUpperCase()}</span>
                      <div>
                        <div style={{ fontWeight: 500 }}>{u.name || u.email}{u.id === me?.id && <span className="chip" style={{ marginLeft: 8 }}>you</span>}</div>
                        <div className="small faint">{u.email}</div>
                      </div>
                    </div>
                  </td>
                  <td>{u.role}</td>
                  <td className="small">{scopeLabel(u)}</td>
                  <td>
                    {u.disabled ? <span className="chip">disabled</span> : u.must_change_password ? <span className="chip warn">password change pending</span> : <span className="chip good">active</span>}
                  </td>
                  <td className="small muted">{ago(u.last_login_at)}{u.sessions > 0 && ` · ${u.sessions} session${u.sessions > 1 ? "s" : ""}`}</td>
                  <td className="actions">
                    <button className="icon-btn sm" title="Edit" aria-label="Edit user" onClick={() => setEditing(u)}><Pencil size={17} /></button>
                    <Menu trigger={<MoreVertical size={17} />}>
                      <button onClick={() => { setResetFor(u); setNewPw(genPassword()); }}><KeyRound size={17} /> Reset password</button>
                      {u.id !== me?.id &&
                        (u.disabled ? (
                          <button onClick={() => patch(u, { disabled: false }, `${u.email} enabled`)}><CircleCheck size={17} /> Enable</button>
                        ) : (
                          <button onClick={() => patch(u, { disabled: true }, `${u.email} disabled and signed out`)}><Ban size={17} /> Disable & sign out</button>
                        ))}
                      {u.id !== me?.id && (
                        <>
                          <hr />
                          <button
                            className="danger"
                            onClick={() =>
                              confirm({
                                title: `Delete ${u.email}?`,
                                body: "The account is deleted and all its sessions end. Audit entries are kept.",
                                confirm: "Delete user",
                                danger: true,
                                action: () => api(`/users/${u.id}`, { method: "DELETE" }).then(reload),
                                success: "User deleted",
                              })
                            }
                          >
                            <Trash2 size={17} /> Delete user
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

      {editing && <UserDialog user={editing === "new" ? null : editing} roles={roles} onClose={() => setEditing(null)} onSaved={reload} />}
      {resetFor && (
        <Dialog
          title={`Reset password for ${resetFor.email}`}
          onClose={() => setResetFor(null)}
          footer={
            <>
              <button className="btn" onClick={() => setResetFor(null)}>Cancel</button>
              <button
                className="btn filled"
                disabled={newPw.length < 10}
                onClick={async () => {
                  await patch(resetFor, { password: newPw, must_change_password: true }, "Password reset; the user must choose a new one at sign-in");
                  setResetFor(null);
                }}
              >
                Reset password
              </button>
            </>
          }
        >
          <PasswordField value={newPw} onChange={setNewPw} label="Temporary password" />
          <div className="small">All of this user's sessions are signed out.</div>
        </Dialog>
      )}
    </div>
  );
}
