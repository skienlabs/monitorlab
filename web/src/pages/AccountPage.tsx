import { ChangeEvent, FormEvent, useRef, useState } from "react";
import { Camera, LogOut, Monitor, Smartphone, Trash2 } from "lucide-react";
import { api, ApiError } from "../api";
import { useAuth } from "../auth";
import { useTheme } from "../theme";
import { Avatar, Seg, Spinner, useConfirm, usePoll, useToast } from "../components/ui";
import { PasswordForm } from "./Login";
import { ago, dateTime } from "../format";

/** Square-crop the centre of the picture and scale it to 256×256 (WebP, JPEG fallback). */
async function preparePhoto(file: File): Promise<Blob> {
  if (!/^image\/(png|jpeg|webp|gif|bmp|avif)$/.test(file.type)) throw new Error("Choose a PNG, JPEG or WebP picture");
  if (file.size > 20 * 1024 * 1024) throw new Error("That picture is larger than 20 MB");
  const bmp = await createImageBitmap(file).catch(() => {
    throw new Error("That file could not be read as a picture");
  });
  const side = Math.min(bmp.width, bmp.height);
  const canvas = document.createElement("canvas");
  canvas.width = canvas.height = 256;
  const ctx = canvas.getContext("2d")!;
  ctx.imageSmoothingQuality = "high";
  ctx.drawImage(bmp, (bmp.width - side) / 2, (bmp.height - side) / 2, side, side, 0, 0, 256, 256);
  bmp.close();
  const toBlob = (type: string, q: number) => new Promise<Blob | null>((res) => canvas.toBlob(res, type, q));
  const webp = await toBlob("image/webp", 0.88);
  const blob = webp && webp.type === "image/webp" ? webp : await toBlob("image/jpeg", 0.9);
  if (!blob) throw new Error("Could not process the picture");
  return blob;
}

function ProfileCard() {
  const { me, refresh } = useAuth();
  const toast = useToast();
  const confirm = useConfirm();
  const file = useRef<HTMLInputElement>(null);
  const [name, setName] = useState(me?.name ?? "");
  const [title, setTitle] = useState(me?.title ?? "");
  const [phone, setPhone] = useState(me?.phone ?? "");
  const [email, setEmail] = useState(me?.email ?? "");
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [photoBusy, setPhotoBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  if (!me) return null;
  const emailChanged = email.trim().toLowerCase() !== me.email.toLowerCase();
  const dirty = name !== (me.name ?? "") || title !== (me.title ?? "") || phone !== (me.phone ?? "") || emailChanged;

  const save = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await api("/auth/profile", { method: "PUT", body: { name, title, phone, email, current_password: emailChanged ? password : undefined } });
      setPassword("");
      await refresh();
      toast("Profile saved");
    } catch (err: any) {
      setError(err.message);
    } finally {
      setBusy(false);
    }
  };

  const upload = async (e: ChangeEvent<HTMLInputElement>) => {
    const f = e.target.files?.[0];
    e.target.value = "";
    if (!f) return;
    setPhotoBusy(true);
    try {
      const blob = await preparePhoto(f);
      const res = await fetch("/api/auth/avatar", { method: "PUT", credentials: "same-origin", headers: { "content-type": blob.type, "x-monitorlab-csrf": "1" }, body: blob });
      if (!res.ok) throw new ApiError(res.status, (await res.json().catch(() => null))?.error ?? res.statusText);
      await refresh();
      toast("Profile photo updated");
    } catch (err: any) {
      toast(err.message, true);
    } finally {
      setPhotoBusy(false);
    }
  };

  const removePhoto = () =>
    confirm({
      title: "Remove profile photo?",
      body: "Your initial will be shown instead.",
      confirm: "Remove",
      action: () => api("/auth/avatar", { method: "DELETE" }).then(refresh),
      success: "Profile photo removed",
    });

  return (
    <div className="card card-pad">
      <div className="profile-head" style={{ marginBottom: 24 }}>
        <div className="profile-photo">
          <Avatar src={me.avatar} name={me.name || me.email} size={88} />
        </div>
        <div className="grow" style={{ minWidth: 200 }}>
          <h2 style={{ margin: 0 }}>{me.name || me.email}</h2>
          <div className="muted small" style={{ marginTop: 2 }}>
            {[me.title, me.role].filter(Boolean).join(" · ")}
          </div>
          <div className="row wrap" style={{ marginTop: 12, gap: 8 }}>
            <input ref={file} type="file" accept="image/png,image/jpeg,image/webp" hidden onChange={upload} />
            <button type="button" className="btn tonal sm" disabled={photoBusy} onClick={() => file.current?.click()}>
              {photoBusy ? <Spinner /> : <Camera size={15} />} {me.avatar ? "Change photo" : "Upload photo"}
            </button>
            {me.avatar && (
              <button type="button" className="btn sm" onClick={removePhoto}>
                <Trash2 size={15} /> Remove
              </button>
            )}
          </div>
        </div>
      </div>
      <form onSubmit={save} className="col" style={{ gap: 16 }}>
        <div className="form-grid">
          <div className="field">
            <label htmlFor="pf-name">Full name</label>
            <input id="pf-name" className="input" maxLength={80} value={name} onChange={(e) => setName(e.target.value)} autoComplete="name" />
          </div>
          <div className="field">
            <label htmlFor="pf-title">Job title</label>
            <input id="pf-title" className="input" maxLength={80} value={title} placeholder="e.g. DevOps Engineer" onChange={(e) => setTitle(e.target.value)} autoComplete="organization-title" />
          </div>
          <div className="field">
            <label htmlFor="pf-email">Email (used to sign in)</label>
            <input id="pf-email" className="input" type="email" required value={email} onChange={(e) => setEmail(e.target.value)} autoComplete="email" />
          </div>
          <div className="field">
            <label htmlFor="pf-phone">Phone</label>
            <input id="pf-phone" className="input" type="tel" maxLength={32} value={phone} placeholder="+91 …" onChange={(e) => setPhone(e.target.value)} autoComplete="tel" />
          </div>
          {emailChanged && (
            <div className="field">
              <label htmlFor="pf-pw">Current password</label>
              <input id="pf-pw" className="input" type="password" required value={password} onChange={(e) => setPassword(e.target.value)} autoComplete="current-password" />
              <span className="hint">Needed to change your sign-in email.</span>
            </div>
          )}
        </div>
        {error && <div className="banner crit small">{error}</div>}
        <div className="row" style={{ justifyContent: "flex-end", gap: 8 }}>
          {dirty && (
            <button
              type="button"
              className="btn"
              onClick={() => {
                setName(me.name ?? "");
                setTitle(me.title ?? "");
                setPhone(me.phone ?? "");
                setEmail(me.email);
                setError(null);
              }}
            >
              Cancel
            </button>
          )}
          <button className="btn filled" disabled={busy || !dirty || (emailChanged && !password)}>
            {busy && <Spinner />} Save profile
          </button>
        </div>
      </form>
    </div>
  );
}

interface Session {
  id: string;
  current: boolean;
  created_at: number;
  last_seen_at: number;
  ip: string;
  user_agent: string;
}

function describeAgent(ua: string): { device: string; mobile: boolean } {
  const browser = /Edg\//.test(ua) ? "Edge" : /OPR\//.test(ua) ? "Opera" : /Firefox\//.test(ua) ? "Firefox" : /Chrome\//.test(ua) ? "Chrome" : /Safari\//.test(ua) ? "Safari" : /curl|python|Go-http/i.test(ua) ? "Script" : "Browser";
  const os = /Windows/.test(ua) ? "Windows" : /Android/.test(ua) ? "Android" : /iPhone|iPad/.test(ua) ? "iOS" : /Mac OS X/.test(ua) ? "macOS" : /Linux/.test(ua) ? "Linux" : "";
  return { device: os ? `${browser} on ${os}` : browser, mobile: /Mobile|Android|iPhone/.test(ua) };
}

function SessionsCard() {
  const { data, reload } = usePoll(() => api<Session[]>("/auth/sessions"), [], 60_000);
  const confirm = useConfirm();
  const others = (data ?? []).filter((s) => !s.current).length;
  return (
    <div className="card card-pad">
      <div className="row" style={{ marginBottom: 8 }}>
        <h2 className="grow" style={{ margin: 0 }}>Where you're signed in</h2>
        {others > 0 && (
          <button
            className="btn sm"
            onClick={() =>
              confirm({
                title: "Sign out all other sessions?",
                body: `${others} other session(s) will be signed out immediately.`,
                confirm: "Sign out others",
                action: () => api("/auth/sessions/revoke-others", { method: "POST" }).then(reload),
                success: "Other sessions signed out",
              })
            }
          >
            <LogOut size={15} /> Sign out all others
          </button>
        )}
      </div>
      {!data && <Spinner />}
      {data?.map((s) => {
        const d = describeAgent(s.user_agent);
        return (
          <div key={s.id} className="session-row">
            <div className="session-icon">{d.mobile ? <Smartphone size={18} /> : <Monitor size={18} />}</div>
            <div className="grow">
              <div style={{ fontWeight: 500 }}>
                {d.device} {s.current && <span className="chip good" style={{ marginLeft: 6 }}>This device</span>}
              </div>
              <div className="small muted">
                {s.ip || "unknown IP"} · active {ago(s.last_seen_at)} · signed in {dateTime(s.created_at)}
              </div>
            </div>
            {!s.current && (
              <button
                className="btn sm"
                onClick={() =>
                  confirm({
                    title: "Sign out this session?",
                    body: `${d.device} (${s.ip || "unknown IP"}) will have to sign in again.`,
                    confirm: "Sign out",
                    action: () => api(`/auth/sessions/${s.id}`, { method: "DELETE" }).then(reload),
                    success: "Session signed out",
                  })
                }
              >
                Sign out
              </button>
            )}
          </div>
        );
      })}
    </div>
  );
}

export function AccountPage() {
  const { me } = useAuth();
  const { mode, setMode } = useTheme();
  if (!me) return null;
  return (
    <div className="page page-narrow">
      <div className="page-head">
        <div className="grow">
          <h1>Profile</h1>
          <div className="sub">Your details, photo, appearance and sign-in security</div>
        </div>
      </div>
      <div className="col" style={{ gap: 16 }}>
        <ProfileCard />
        <div className="card card-pad">
          <dl className="kv">
            <dt>Role</dt>
            <dd>{me.role}</dd>
            <dt>Server access</dt>
            <dd>{me.server_scope === null ? "All servers" : `${me.server_scope.length} selected server(s)`}</dd>
            <dt>Appearance</dt>
            <dd>
              <Seg value={mode} onChange={setMode} options={[{ value: "light", label: "Light" }, { value: "dark", label: "AMOLED Black" }]} />
            </dd>
          </dl>
        </div>
        <div className="card card-pad">
          <h2 style={{ marginBottom: 16 }}>Change password</h2>
          <PasswordForm />
        </div>
        <SessionsCard />
      </div>
    </div>
  );
}
