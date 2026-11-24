import { FormEvent, useState } from "react";
import { Activity, Eye, EyeOff } from "lucide-react";
import { api } from "../api";
import { useAuth } from "../auth";
import type { Me } from "../types";
import { Spinner } from "../components/ui";
import { BrandFooter, useBrand } from "../brand";

export function Login() {
  const { setMe } = useAuth();
  const { name: brand } = useBrand();
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [show, setShow] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      setMe(await api<Me>("/auth/login", { method: "POST", body: { email, password } }));
    } catch (err: any) {
      setError(err.message);
      setPassword("");
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="login-page">
      <div className="login-card">
        <div className="brand-mark">
          <Activity size={26} />
        </div>
        <h1>Sign in</h1>
        <div className="muted">to continue to {brand}</div>
        <form onSubmit={submit}>
          <div className="field">
            <label htmlFor="email">Email</label>
            <input id="email" className="input" type="email" autoComplete="username" autoFocus required value={email} onChange={(e) => setEmail(e.target.value)} />
          </div>
          <div className="field">
            <label htmlFor="password">Password</label>
            <div style={{ position: "relative" }}>
              <input
                id="password"
                className="input"
                type={show ? "text" : "password"}
                autoComplete="current-password"
                required
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                style={{ paddingRight: 48 }}
              />
              <button type="button" className="icon-btn sm" style={{ position: "absolute", right: 6, top: 6 }} onClick={() => setShow((s) => !s)} aria-label={show ? "Hide password" : "Show password"}>
                {show ? <EyeOff size={18} /> : <Eye size={18} />}
              </button>
            </div>
          </div>
          {error && <div className="banner crit small">{error}</div>}
          <div className="row" style={{ justifyContent: "flex-end", marginTop: 8 }}>
            <button className="btn filled" disabled={busy || !email || !password}>
              {busy && <Spinner />} Sign in
            </button>
          </div>
        </form>
        <BrandFooter className="login-footer" />
      </div>
    </div>
  );
}

export function PasswordForm({ forced, onDone }: { forced?: boolean; onDone?: () => void }) {
  const { refresh } = useAuth();
  const [current, setCurrent] = useState("");
  const [next, setNext] = useState("");
  const [confirm, setConfirm] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [ok, setOk] = useState(false);
  const mismatch = confirm.length > 0 && next !== confirm;

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (next !== confirm) return;
    setBusy(true);
    setError(null);
    try {
      await api("/auth/password", { method: "POST", body: { current, new: next } });
      setOk(true);
      setCurrent("");
      setNext("");
      setConfirm("");
      await refresh();
      onDone?.();
    } catch (err: any) {
      setError(err.message);
    } finally {
      setBusy(false);
    }
  };

  return (
    <form onSubmit={submit} className="col" style={{ gap: 16 }}>
      {forced && <div className="banner info small">For security, please choose a new password before continuing.</div>}
      <div className="field">
        <label>Current password</label>
        <input className="input" type="password" autoComplete="current-password" required value={current} onChange={(e) => setCurrent(e.target.value)} />
      </div>
      <div className="field">
        <label>New password</label>
        <input className="input" type="password" autoComplete="new-password" required minLength={10} value={next} onChange={(e) => setNext(e.target.value)} />
        <span className="hint">At least 10 characters. A passphrase is easiest to remember.</span>
      </div>
      <div className="field">
        <label>Confirm new password</label>
        <input className="input" type="password" autoComplete="new-password" required value={confirm} onChange={(e) => setConfirm(e.target.value)} />
        {mismatch && <span className="hint" style={{ color: "var(--crit-text)" }}>Passwords do not match</span>}
      </div>
      {error && <div className="banner crit small">{error}</div>}
      {ok && !forced && <div className="banner small">Password updated. Other sessions were signed out.</div>}
      <div className="row" style={{ justifyContent: "flex-end" }}>
        <button className="btn filled" disabled={busy || mismatch || next.length < 10 || !current}>
          {busy && <Spinner />} Update password
        </button>
      </div>
    </form>
  );
}

export function ForcedPassword() {
  const { logout, me } = useAuth();
  return (
    <div className="login-page">
      <div className="login-card">
        <div className="brand-mark">
          <Activity size={26} />
        </div>
        <h1>Set a new password</h1>
        <div className="muted" style={{ marginBottom: 20 }}>{me?.email}</div>
        <PasswordForm forced />
        <button className="btn sm" style={{ marginTop: 12 }} onClick={logout}>
          Sign out
        </button>
      </div>
    </div>
  );
}
