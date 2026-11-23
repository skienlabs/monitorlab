import { useAuth } from "../auth";
import { useTheme } from "../theme";
import { Seg } from "../components/ui";
import { PasswordForm } from "./Login";

export function AccountPage() {
  const { me } = useAuth();
  const { mode, setMode } = useTheme();
  if (!me) return null;
  return (
    <div className="page page-narrow">
      <div className="page-head">
        <div className="grow">
          <h1>Account</h1>
          <div className="sub">{me.email}</div>
        </div>
      </div>
      <div className="col" style={{ gap: 16 }}>
        <div className="card card-pad">
          <dl className="kv">
            <dt>Name</dt>
            <dd>{me.name || "—"}</dd>
            <dt>Role</dt>
            <dd>{me.role}</dd>
            <dt>Server access</dt>
            <dd>{me.server_scope === null ? "All servers" : `${me.server_scope.length} selected server(s)`}</dd>
            <dt>Appearance</dt>
            <dd>
              <Seg value={mode} onChange={setMode} options={[{ value: "light", label: "Light" }, { value: "dark", label: "Dark" }, { value: "system", label: "System" }]} />
            </dd>
          </dl>
        </div>
        <div className="card card-pad">
          <h2 style={{ marginBottom: 16 }}>Change password</h2>
          <PasswordForm />
        </div>
      </div>
    </div>
  );
}
