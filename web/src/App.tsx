import { ReactNode, useEffect, useState } from "react";
import { BrowserRouter, Link, Navigate, NavLink, Route, Routes, useLocation } from "react-router-dom";
import { Activity, Bell, BellRing, KeyRound, Menu as MenuIcon, SlidersHorizontal, LayoutGrid, LogOut, Monitor, Moon, ScrollText, Server as ServerIcon, ShieldCheck, Sun, UserRound, Users } from "lucide-react";
import { AuthProvider, useAuth } from "./auth";
import { BrandFooter, BrandProvider, useBrand } from "./brand";
import { GeneralSettings } from "./pages/GeneralSettings";
import { ThemeProvider, useTheme } from "./theme";
import { ServersProvider, useServers } from "./servers";
import { Avatar, ConfirmProvider, Loading, Menu, ToastProvider } from "./components/ui";
import { ForcedPassword, Login } from "./pages/Login";
import { Overview } from "./pages/Overview";
import { ServerPage } from "./pages/ServerPage";
import { ContainerPage } from "./pages/ContainerPage";
import { AdminServers } from "./pages/AdminServers";
import { AdminUsers } from "./pages/AdminUsers";
import { AdminRoles } from "./pages/AdminRoles";
import { AuditPage } from "./pages/AuditPage";
import { AccountPage } from "./pages/AccountPage";
import { PodPage } from "./pages/PodPage";
import { AlertsPage } from "./pages/AlertsPage";
import { AlertSettings } from "./pages/AlertSettings";
import { useAlertCount } from "./servers";

function ThemeToggle() {
  const { mode, setMode } = useTheme();
  const next = mode === "light" ? "dark" : "light";
  const Icon = mode === "light" ? Moon : Sun;
  const label = next === "dark" ? "Switch to AMOLED Black" : "Switch to Light";
  return (
    <button className="icon-btn" onClick={() => setMode(next)} title={label} aria-label={label}>
      <Icon size={20} />
    </button>
  );
}

function UserMenu() {
  const { me, logout } = useAuth();
  if (!me) return null;
  return (
    <Menu trigger={<Avatar src={me.avatar} name={me.name || me.email} />} label="Account">
      <div className="menu-label">
        <div style={{ color: "var(--text)", fontSize: 14, fontWeight: 500 }}>{me.name || me.email}</div>
        <div>{me.email}</div>
        <div>{me.role}</div>
      </div>
      <hr />
      <Link to="/account">
        <KeyRound size={18} /> Profile & password
      </Link>
      <button onClick={logout}>
        <LogOut size={18} /> Sign out
      </button>
    </Menu>
  );
}

function Nav({ open }: { open: boolean }) {
  const { can } = useAuth();
  const { servers } = useServers();
  const online = servers?.filter((s) => s.online).length ?? 0;
  const alertCount = useAlertCount();
  return (
    <nav className={`nav ${open ? "open" : ""}`} aria-label="Main">
      <NavLink to="/" end data-accent="cpu">
        <LayoutGrid size={20} /> Overview
        {servers && <span className="count">{online}/{servers.length}</span>}
      </NavLink>
      <NavLink to="/alerts" data-accent="alert">
        <Bell size={20} /> Alerts
        {alertCount > 0 && <span className="count" style={{ color: "var(--crit-text)", fontWeight: 600 }}>{alertCount}</span>}
      </NavLink>
      {servers && servers.length > 0 && <div className="nav-section">Servers</div>}
      {servers?.map((s) => (
        <NavLink key={s.id} to={`/servers/${s.id}`}>
          <span className={`dot ${s.status === "ok" ? "good" : s.status === "warning" ? "warn" : s.status === "critical" ? "crit" : ""}`} style={{ marginLeft: 6, marginRight: 2 }} />
          <span className="ellipsis">{s.name}</span>
        </NavLink>
      ))}
      {(can("servers.manage") || can("users.manage") || can("audit.view") || can("alerts.manage")) && <div className="nav-section">Administration</div>}
      {can("alerts.manage") && (
        <NavLink to="/admin/alerts" data-accent="up">
          <BellRing size={20} /> Alert settings
        </NavLink>
      )}
      {can("servers.manage") && (
        <NavLink to="/admin/servers" data-accent="docker">
          <ServerIcon size={20} /> Servers & agents
        </NavLink>
      )}
      {can("users.manage") && (
        <>
          <NavLink to="/admin/general" data-accent="mem">
            <SlidersHorizontal size={20} /> General
          </NavLink>
          <NavLink to="/admin/users" data-accent="users">
            <Users size={20} /> Users
          </NavLink>
          <NavLink to="/admin/roles" data-accent="net">
            <ShieldCheck size={20} /> Roles
          </NavLink>
        </>
      )}
      {can("audit.view") && (
        <NavLink to="/admin/audit" data-accent="audit">
          <ScrollText size={20} /> Audit log
        </NavLink>
      )}
      <div className="nav-section">You</div>
      <NavLink to="/account" data-accent="disk">
        <UserRound size={20} /> Profile
      </NavLink>
      <BrandFooter />
    </nav>
  );
}

function Shell({ children }: { children: ReactNode }) {
  const { name } = useBrand();
  const [navOpen, setNavOpen] = useState(false);
  const loc = useLocation();
  useEffect(() => setNavOpen(false), [loc.pathname, loc.search]);
  return (
    <div className="shell">
      <header className="topbar">
        <button className="icon-btn nav-toggle" onClick={() => setNavOpen((o) => !o)} aria-label="Menu" aria-expanded={navOpen}>
          <MenuIcon size={20} />
        </button>
        <Link to="/" className="brand">
          <span className="brand-mark">
            <Activity size={19} />
          </span>
          <span className="ellipsis">{name}</span>
        </Link>
        <div className="spacer" />
        <ThemeToggle />
        <UserMenu />
      </header>
      <Nav open={navOpen} />
      {navOpen && <div className="nav-scrim" onClick={() => setNavOpen(false)} />}
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
  const { name } = useBrand();
  if (loading) return <Loading label={`Starting ${name}…`} />;
  if (!me) return <Login />;
  if (me.must_change_password) return <ForcedPassword />;
  return (
    <ServersProvider>
      <Shell>
        <Routes>
          <Route path="/" element={<Overview />} />
          <Route path="/servers/:id" element={<ServerPage />} />
          <Route path="/servers/:id/containers/:cid" element={<ContainerPage />} />
          <Route path="/servers/:id/k8s/:ns/:pod" element={<PodPage />} />
          <Route path="/admin/servers" element={<Guard perm="servers.manage"><AdminServers /></Guard>} />
          <Route path="/admin/general" element={<Guard perm="users.manage"><GeneralSettings /></Guard>} />
          <Route path="/admin/users" element={<Guard perm="users.manage"><AdminUsers /></Guard>} />
          <Route path="/admin/roles" element={<Guard perm="users.manage"><AdminRoles /></Guard>} />
          <Route path="/admin/audit" element={<Guard perm="audit.view"><AuditPage /></Guard>} />
          <Route path="/alerts" element={<AlertsPage />} />
          <Route path="/admin/alerts" element={<Guard perm="alerts.manage"><AlertSettings /></Guard>} />
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
      <BrandProvider>
      <ToastProvider>
        <ConfirmProvider>
          <AuthProvider>
            <BrowserRouter>
              <Routed />
            </BrowserRouter>
          </AuthProvider>
        </ConfirmProvider>
      </ToastProvider>
      </BrandProvider>
    </ThemeProvider>
  );
}
