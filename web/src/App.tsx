import { ReactNode } from "react";
import { BrowserRouter, Link, Navigate, NavLink, Route, Routes } from "react-router-dom";
import { Activity, KeyRound, LayoutGrid, LogOut, Monitor, Moon, ScrollText, Server as ServerIcon, ShieldCheck, Sun, SunMoon, UserRound, Users } from "lucide-react";
import { AuthProvider, useAuth } from "./auth";
import { ThemeProvider, useTheme } from "./theme";
import { ServersProvider, useServers } from "./servers";
import { ConfirmProvider, Loading, Menu, ToastProvider } from "./components/ui";
import { ForcedPassword, Login } from "./pages/Login";
import { Overview } from "./pages/Overview";
import { ServerPage } from "./pages/ServerPage";
import { ContainerPage } from "./pages/ContainerPage";
import { AdminServers } from "./pages/AdminServers";
import { AdminUsers } from "./pages/AdminUsers";
import { AdminRoles } from "./pages/AdminRoles";
import { AuditPage } from "./pages/AuditPage";
import { AccountPage } from "./pages/AccountPage";

function ThemeToggle() {
  const { mode, setMode } = useTheme();
  const next = mode === "light" ? "dark" : mode === "dark" ? "system" : "light";
  const Icon = mode === "light" ? Sun : mode === "dark" ? Moon : SunMoon;
  return (
    <button className="icon-btn" onClick={() => setMode(next)} title={`Theme: ${mode} (click for ${next})`} aria-label="Toggle theme">
      <Icon size={20} />
    </button>
  );
}

function UserMenu() {
  const { me, logout } = useAuth();
  if (!me) return null;
  const initial = (me.name || me.email).trim().charAt(0).toUpperCase();
  return (
    <Menu trigger={<span className="avatar">{initial}</span>} label="Account">
      <div className="menu-label">
        <div style={{ color: "var(--text)", fontSize: 14, fontWeight: 500 }}>{me.name || me.email}</div>
        <div>{me.email}</div>
        <div>{me.role}</div>
      </div>
      <hr />
      <Link to="/account">
        <KeyRound size={18} /> Account & password
      </Link>
      <button onClick={logout}>
        <LogOut size={18} /> Sign out
      </button>
    </Menu>
  );
}

function Nav() {
  const { can } = useAuth();
  const { servers } = useServers();
  const online = servers?.filter((s) => s.online).length ?? 0;
  return (
    <nav className="nav" aria-label="Main">
      <NavLink to="/" end>
        <LayoutGrid size={20} /> Overview
        {servers && <span className="count">{online}/{servers.length}</span>}
      </NavLink>
      {servers && servers.length > 0 && <div className="nav-section">Servers</div>}
      {servers?.map((s) => (
        <NavLink key={s.id} to={`/servers/${s.id}`}>
          <span className={`dot ${s.status === "ok" ? "good" : s.status === "warning" ? "warn" : s.status === "critical" ? "crit" : ""}`} style={{ marginLeft: 6, marginRight: 2 }} />
          <span className="ellipsis">{s.name}</span>
        </NavLink>
      ))}
      {(can("servers.manage") || can("users.manage") || can("audit.view")) && <div className="nav-section">Administration</div>}
      {can("servers.manage") && (
        <NavLink to="/admin/servers">
          <ServerIcon size={20} /> Servers & agents
        </NavLink>
      )}
      {can("users.manage") && (
        <>
          <NavLink to="/admin/users">
            <Users size={20} /> Users
          </NavLink>
          <NavLink to="/admin/roles">
            <ShieldCheck size={20} /> Roles
          </NavLink>
        </>
      )}
      {can("audit.view") && (
        <NavLink to="/admin/audit">
          <ScrollText size={20} /> Audit log
        </NavLink>
      )}
      <div className="nav-section">You</div>
      <NavLink to="/account">
        <UserRound size={20} /> Account
      </NavLink>
    </nav>
  );
}

function Shell({ children }: { children: ReactNode }) {
  return (
    <div className="shell">
      <header className="topbar">
        <Link to="/" className="brand">
          <span className="brand-mark">
            <Activity size={19} />
          </span>
          Vigil
        </Link>
        <div className="spacer" />
        <ThemeToggle />
        <UserMenu />
      </header>
      <Nav />
      <main className="main">{children}</main>
    </div>
  );
}

function Guard({ perm, children }: { perm: string; children: ReactNode }) {
  const { can } = useAuth();
  if (!can(perm))
    return (
      <div className="page">
        <div className="empty">
          <div className="ico">
            <Monitor size={26} />
          </div>
          <h3>You don't have access to this page</h3>
          <div className="small">Ask an administrator for the “{perm}” permission.</div>
        </div>
      </div>
    );
  return <>{children}</>;
}

function Routed() {
  const { me, loading } = useAuth();
  if (loading) return <Loading label="Starting Vigil…" />;
  if (!me) return <Login />;
  if (me.must_change_password) return <ForcedPassword />;
  return (
    <ServersProvider>
      <Shell>
        <Routes>
          <Route path="/" element={<Overview />} />
          <Route path="/servers/:id" element={<ServerPage />} />
          <Route path="/servers/:id/containers/:cid" element={<ContainerPage />} />
          <Route path="/admin/servers" element={<Guard perm="servers.manage"><AdminServers /></Guard>} />
          <Route path="/admin/users" element={<Guard perm="users.manage"><AdminUsers /></Guard>} />
          <Route path="/admin/roles" element={<Guard perm="users.manage"><AdminRoles /></Guard>} />
          <Route path="/admin/audit" element={<Guard perm="audit.view"><AuditPage /></Guard>} />
          <Route path="/account" element={<AccountPage />} />
          <Route path="*" element={<Navigate to="/" replace />} />
        </Routes>
      </Shell>
    </ServersProvider>
  );
}

export default function App() {
  return (
    <ThemeProvider>
      <ToastProvider>
        <ConfirmProvider>
          <AuthProvider>
            <BrowserRouter>
              <Routed />
            </BrowserRouter>
          </AuthProvider>
        </ConfirmProvider>
      </ToastProvider>
    </ThemeProvider>
  );
}
