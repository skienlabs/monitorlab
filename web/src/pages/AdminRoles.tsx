import { useMemo, useState } from "react";
import { Lock, Pencil, Plus, ShieldCheck, Trash2, TriangleAlert } from "lucide-react";
import { api } from "../api";
import { Dialog, Loading, Spinner, useConfirm, usePoll, useToast } from "../components/ui";
import type { Role } from "./AdminUsers";

interface Perm {
  key: string;
  group: string;
  label: string;
  description: string;
  dangerous: boolean;
}

function RoleDialog({ role, perms, onClose, onSaved }: { role: Role | null; perms: Perm[]; onClose: () => void; onSaved: () => void }) {
  const toast = useToast();
  const [name, setName] = useState(role?.name ?? "");
  const [desc, setDesc] = useState(role?.description ?? "");
  const [sel, setSel] = useState<Set<string>>(new Set(role?.permissions ?? ["servers.view"]));
  const [busy, setBusy] = useState(false);
  const groups = useMemo(() => {
    const m = new Map<string, Perm[]>();
    perms.forEach((p) => m.set(p.group, [...(m.get(p.group) ?? []), p]));
    return [...m.entries()];
  }, [perms]);

  const toggle = (k: string) =>
    setSel((s) => {
      const n = new Set(s);
      n.has(k) ? n.delete(k) : n.add(k);
      return n;
    });

  const save = async () => {
    setBusy(true);
    try {
      const body = { name, description: desc, permissions: [...sel] };
      if (role) await api(`/roles/${role.id}`, { method: "PATCH", body });
      else await api("/roles", { method: "POST", body });
      toast(role ? "Role updated" : "Role created");
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
      title={role ? `Edit role ${role.name}` : "New role"}
      onClose={onClose}
      footer={
        <>
          <button className="btn" onClick={onClose}>Cancel</button>
          <button className="btn filled" disabled={busy || !name.trim()} onClick={save}>{busy && <Spinner />} Save role</button>
        </>
      }
    >
      <div className="grid grid-2">
        <div className="field">
          <label>Name</label>
          <input className="input" value={name} disabled={role?.builtin} onChange={(e) => setName(e.target.value)} autoFocus={!role} />
        </div>
        <div className="field">
          <label>Description</label>
          <input className="input" value={desc} onChange={(e) => setDesc(e.target.value)} placeholder="What this role is for" />
        </div>
      </div>
      <div className="grid grid-2">
        {groups.map(([g, list]) => (
          <div key={g} className="perm-group">
            <h4>{g}</h4>
            {list.map((p) => (
              <label key={p.key} className="perm-item check">
                <input type="checkbox" checked={sel.has(p.key)} onChange={() => toggle(p.key)} />
                <span>
                  <span style={{ color: "var(--text)", fontWeight: 500 }}>{p.label}</span>
                  {p.dangerous && <TriangleAlert size={13} style={{ marginLeft: 6, color: "var(--warn-text)", verticalAlign: -2 }} aria-label="sensitive" />}
                  <div className="desc">{p.description}</div>
                </span>
              </label>
            ))}
          </div>
        ))}
      </div>
    </Dialog>
  );
}

export function AdminRoles() {
  const confirm = useConfirm();
  const { data: roles, reload } = usePoll(() => api<Role[]>("/roles"), []);
  const { data: perms } = usePoll(() => api<Perm[]>("/permissions"), []);
  const [editing, setEditing] = useState<Role | null | "new">(null);
  if (!roles || !perms) return <Loading />;
  const label = (k: string) => perms.find((p) => p.key === k)?.label ?? k;

  return (
    <div className="page">
      <div className="page-head">
        <div className="grow">
          <h1>Roles</h1>
          <div className="sub">Roles bundle permissions. Assign them to users; restrict which servers a user sees on the user itself.</div>
        </div>
        <button className="btn filled" onClick={() => setEditing("new")}><Plus size={18} /> New role</button>
      </div>
      <div className="grid grid-2">
        {roles.map((r) => (
          <div key={r.id} className="card card-pad col" style={{ gap: 12 }}>
            <div className="row">
              <ShieldCheck size={20} />
              <h3 className="grow">{r.name}</h3>
              {r.builtin && <span className="chip">built-in</span>}
              <span className="chip outline">{r.users} user{r.users === 1 ? "" : "s"}</span>
            </div>
            <div className="small muted">{r.description}</div>
            <div className="row wrap" style={{ gap: 6 }}>
              {r.permissions.map((p) => (
                <span key={p} className={`chip ${perms.find((x) => x.key === p)?.dangerous ? "warn" : ""}`}>{label(p)}</span>
              ))}
            </div>
            <div className="row" style={{ justifyContent: "flex-end", marginTop: "auto" }}>
              {r.locked ? (
                <span className="small faint row"><Lock size={14} /> Always has every permission</span>
              ) : (
                <>
                  {!r.builtin && (
                    <button
                      className="btn sm danger-text"
                      disabled={r.users > 0}
                      title={r.users > 0 ? "Reassign its users first" : undefined}
                      onClick={() =>
                        confirm({
                          title: `Delete role ${r.name}?`,
                          body: "The role is removed permanently.",
                          confirm: "Delete role",
                          danger: true,
                          action: () => api(`/roles/${r.id}`, { method: "DELETE" }).then(reload),
                          success: "Role deleted",
                        })
                      }
                    >
                      <Trash2 size={15} /> Delete
                    </button>
                  )}
                  <button className="btn sm tonal" onClick={() => setEditing(r)}><Pencil size={15} /> Edit</button>
                </>
              )}
            </div>
          </div>
        ))}
      </div>
      {editing && <RoleDialog role={editing === "new" ? null : editing} perms={perms} onClose={() => setEditing(null)} onSaved={reload} />}
    </div>
  );
}
