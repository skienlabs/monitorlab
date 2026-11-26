# MonitorLab

> The name shown to users defaults to **MonitorLab** and can be changed in **Settings → General** (or with `MONITORLAB_BRAND_NAME` before the first start).

Lightweight monitoring and operations for Linux and Windows servers: a **central hub** (one Docker container) plus a **tiny Rust agent** on each server.
It combines what you would otherwise run as Prometheus + Grafana + Dozzle + Portainer, without putting any of them on production machines.

| | |
|---|---|
| **Linux servers** | CPU / memory / swap / load / disks / network / disk I/O · Docker containers (grouped by compose stack), live CPU/memory per container, live logs, terminal, live stats, start/stop/restart/pause/resume/kill/rename/recreate (with image pull)/delete, bulk actions on selected containers · images, volumes, networks · `docker system df` with one-click cleanup (dangling/unused images, stopped containers, unused volumes, networks, build cache) · SQL Server on Linux (service state, CPU/RAM, error log) |
| **Kubernetes (k3s / k8s)** | Nodes (ready, cordon, CPU/memory/pods), deployments / stateful sets / daemon sets (rolling restart, scale), pods (status, restarts, CPU/memory, live logs incl. previous container, terminal, delete), services, warning events · alerts for not-ready nodes, failing pods and degraded workloads |
| **Windows servers** | System metrics · IIS sites, bindings, app pools and worker processes (start/stop/restart site, recycle pool) · live W3C logs per site · SQL Server instances (service state, CPU/RAM, version, live ERRORLOG) |
| **Hub** | Web console, profiles with photo and session management, history charts (15 min → 30 days), alerting (email, Teams, Slack, webhooks), users, roles and per-server access, audit log, agent enrollment, TLS with certificate pinning |

## Architecture

```
            browser ──HTTPS──▶ ┌──────────────── hub (container) ────────────────┐
                               │ web console · REST/WebSocket API · SQLite        │
                               │ auth · RBAC · audit · metrics store · built-in   │
                               │ agent for the hub host                           │
                               └──────▲──────────────▲──────────────▲─────────────┘
                     outbound WSS     │              │              │   (agents dial out;
                     (pinned TLS)     │              │              │    no inbound ports)
                               ┌──────┴─────┐ ┌──────┴─────┐ ┌──────┴──────┐
                               │ agent      │ │ agent      │ │ agent       │
                               │ Linux +    │ │ Linux +    │ │ Windows +   │
                               │ Docker     │ │ SQL Server │ │ IIS + SQL   │
                               └────────────┘ └────────────┘ └─────────────┘
```

* **Agents connect out** to the hub over one WebSocket. Production servers need no open ports, and the hub can sit in a test network as long as agents can reach it.
* Agents push a metrics sample every 10 s. Everything else (container lists, logs, terminal, IIS, cleanup) is fetched on demand, so idle agents use very little CPU or memory.
* The hub stores raw samples for 3 days and 5-minute rollups for 90 days (configurable).
* The hub monitors its own host by running the same agent code in-process.
* Measured footprint: agent ≈ **7–8 MB RAM**, 2 threads, near-zero idle CPU; hub container ≈ 25 MB idle (≈ 110 MB right after a heavy test run); image 178 MB.

## Quick start (hub)

```bash
docker build -t monitorlab-hub .          # builds UI, hub, Linux (static) and Windows agents
# edit MONITORLAB_PUBLIC_URL / MONITORLAB_TLS_SANS in docker-compose.yml, then:
docker compose up -d
docker logs monitorlab                    # shows the generated admin password (once)
```

Open `https://<hub>:8443`, sign in, and set a new password. The hub generates a self-signed certificate on first start. Agents pin its SHA-256 fingerprint, so a self-signed certificate is safe for agent traffic. See [TLS](#tls) to use your own certificate.

> Behind a registry mirror: `docker build --build-arg REGISTRY=mirror.gcr.io/library -t monitorlab-hub .`

## Deploying with GitHub Actions (GAR + VM)

`.github/workflows/deploy.yml` follows the same flow as the other Skien apps. It is started manually: **Actions → Build & Deploy MonitorLab → Run workflow**.

1. **test**: builds the web console (TypeScript type-check plus the Vite build), then runs the Rust unit tests.
2. **build-deploy**: builds the image, then **starts it on the runner and runs the 97-check end-to-end suite against it**. Only if that passes does it push `…/test-images/monitorlab:latest` and `:<sha>` to Artifact Registry, SSH to the VM (port 2232), `docker compose pull && up -d`, and wait for the hub's health check.

It uses the same secrets: `GCP_SA_KEY`, `GCP_PROJECT_ID`, `VM_HOST`, `VM_USER` and `VM_SSH_KEY`. Add them to this repo, or give the org secrets access to it.

**One-time setup** (same pattern as the Tasks app):

1. **Deploy key**: on the VM as root:
   ```bash
   ssh-keygen -t ed25519 -f ~/.ssh/gh_deploy_monitorlab -N "" -C "github-actions monitorlab"
   cat ~/.ssh/gh_deploy_monitorlab.pub >> ~/.ssh/authorized_keys
   cat ~/.ssh/gh_deploy_monitorlab          # → paste into the VM_SSH_KEY secret
   ```
2. **Compose file**: `mkdir -p ~/docker/apps/monitorlab`, then copy `deploy/vm/docker-compose.yml` there. Set `MONITORLAB_PUBLIC_URL` and `MONITORLAB_ADMIN_EMAIL`.
3. **Nginx Proxy Manager**: add a proxy host for `monitorlab.skienlab.com` forwarding to `http` `monitorlab` `8090`, turn **Websockets Support on**, and add an SSL certificate (Let's Encrypt, Force SSL). Point the DNS A record at the VM.
4. **Run the workflow.** After the first deploy, `docker logs monitorlab` shows the generated admin password.
5. **The VM itself** shows up automatically as the "hub host" server. The built-in agent reads host processes through `pid: host`, and host disks and network counters (from `/sys/class/net`) through the read-only `/:/host` mount. Containers come through the Docker socket. You don't need a separate agent install on the hub's own server.

The workflow waits for the container's Docker health check to report `healthy`. To roll back, change the image tag in the compose file to `:<old sha>` and run `docker compose up -d`.

## Adding servers

In the console, go to **Servers & agents → Add server** and give the server a name. You get a one-time install command (valid for 24 h):

**Linux** (systemd; x86_64; static binary, works on any distro):

```bash
curl -fsSL -k --pinnedpubkey 'sha256//…' https://hub:8443/download/install.sh | sudo sh -s -- \
  --hub https://hub:8443 --token <token> --fingerprint <AA:BB:…> --pin 'sha256//…'
```

The installer creates a `monitorlab` system user (in the `docker` and `mssql` groups if they exist), writes `/etc/monitorlab/agent.toml` (mode 0600), and starts `monitorlab-agent.service`, capped at 256 MB of RAM by systemd.

**Windows Server 2016–2025**: download `monitorlab-agent-windows-amd64.exe` from the dialog, then in an elevated PowerShell:

```powershell
.\monitorlab-agent-windows-amd64.exe install --hub https://hub:8443 --token <token> --fingerprint <AA:BB:…>
```

This installs the `MonitorLabAgent` service (LocalSystem, automatic start, restart on failure). The config goes to `C:\ProgramData\MonitorLab\agent.toml`, with ACLs restricted to SYSTEM and Administrators, and the log to `C:\ProgramData\MonitorLab\agent.log`.

To uninstall an agent: `monitorlab-agent uninstall --purge` (Linux, with sudo) or `monitorlab-agent.exe uninstall --purge` (Windows, admin).

### Optional: SQL Server DMV data

Without credentials the agent shows service state, CPU/RAM, version and the error log. For databases, sizes, sessions and page life expectancy, add a read-only login:

```sql
CREATE LOGIN monitorlab_monitor WITH PASSWORD = '…';
GRANT VIEW SERVER STATE TO monitorlab_monitor;
GRANT VIEW ANY DEFINITION TO monitorlab_monitor;
```

```toml
# agent.toml
[mssql.connection]
host = "127.0.0.1"
port = 1433
user = "monitorlab_monitor"
password = "…"
trust_cert = true
```

## Alerting

The hub evaluates rules every 30 seconds for every server:

| Rule | Default | Severity |
|---|---|---|
| CPU sustained | warning ≥ 85 %, critical ≥ 95 % **for 5 minutes** | warning / critical |
| Memory sustained | warning ≥ 90 %, critical ≥ 95 % for 5 minutes | warning / critical |
| Disk (each volume) | warning ≥ 85 %, critical ≥ 95 % | warning / critical |
| Agent offline | no contact for 2 minutes | critical |
| Docker containers unhealthy / restart-looping | on | warning |
| SQL Server instance stopped | on | critical |
| IIS site or application pool stopped | on | warning |

"Sustained" means the *minimum* over the whole window is above the threshold, so short spikes never page anyone. Alerts are stored with full history. Notifications go out when an alert fires, escalates or resolves, and an operator can acknowledge an alert in the UI.

**Channels** (Alert settings → Notification channels):

* **Email** through your own mail server. Set the SMTP server once (host, port, STARTTLS/TLS, login, sender), then add email channels with any recipient lists. Example for your own domain: host `mail.skiensoftwarelab.com`, port 587 STARTTLS, user and from `alerts@skiensoftwarelab.com`. Tick "accept self-signed certificates" if the mail server uses an internal CA.
* **Microsoft Teams**: a Workflows webhook ("Post to a channel when a webhook request is received"). Messages arrive as Adaptive Cards with an "Open in MonitorLab" button.
* **Slack**, Google Chat or Mattermost incoming webhooks.
* **Generic webhook**: a JSON POST (`event`, `severity`, `server`, `rule`, `message`, `started_at`, `link`), useful for PagerDuty or Opsgenie bridges and your own scripts.

Each channel can receive everything or critical alerts only, and has a **Test** button. Set `MONITORLAB_PUBLIC_URL` so notifications link back to the right server page.

## Users, roles and access

Permissions (each agent request maps to exactly one, enforced on the hub):

| Permission | Grants |
|---|---|
| `servers.view` | dashboards, metrics, charts, processes |
| `docker.view` · `mssql.view` · `iis.view` | read-only views of those subsystems |
| `logs.view` | container, pod, IIS and SQL Server logs |
| `containers.control` | start / stop / restart / pause / kill, pull images, create volumes and networks |
| `iis.control` | start / stop sites, recycle / start / stop app pools |
| `containers.exec` | interactive terminal inside containers, and running new containers (a container can mount host folders, so this is as powerful as a shell) |
| `containers.delete` | delete and recreate containers |
| `shell.host` | interactive shell on the server itself: bash / sh / zsh, or PowerShell / cmd on Windows (audited, Administrator only by default) |
| `k8s.view` | Kubernetes nodes, workloads, pods, services, events |
| `k8s.control` | rolling restart, scale, delete pods, cordon / uncordon nodes |
| `k8s.exec` | interactive terminal inside pods (audited) |
| `docker.cleanup` | delete images / volumes / networks, prune, clear build cache |
| `alerts.manage` | alert thresholds, SMTP, notification channels, acknowledge alerts |
| `servers.manage` · `users.manage` · `audit.view` | administration |

Built-in roles: **Administrator** (everything, immutable), **Operator** (everything except deletes and administration), **Tester** (view + logs + restart; no terminal, no deletes), **Viewer** (read-only). You can create custom roles, and restrict any user to selected servers.

Safeguards: you can't delete, disable or demote the last administrator. Every state-changing action, terminal session, sign-in and failed sign-in is written to the audit log, which is kept for one year.

## Security model

* Passwords are hashed with Argon2id; minimum 10 characters. New users and password resets force a change at the next sign-in.
* Sessions are opaque random tokens stored hashed on the server, in an `HttpOnly; SameSite=Strict` cookie (`Secure` over TLS). They expire after 12 h idle (configurable) and 7 days at most. Changing a password signs out the user's other sessions.
* Login throttling: per account (5 failures → 5 min lock) and per IP (20 → 15 min).
* CSRF: state-changing requests require the `X-MonitorLab-Csrf` header plus a same-origin `Origin`. Browser WebSockets require a same-origin `Origin` to block cross-site WebSocket hijacking.
* Security headers: strict CSP (no inline scripts), `frame-ancestors 'none'`, nosniff, and `no-referrer`.
* Agent credentials: one-time enrollment token, then a 256-bit secret stored hashed on the hub. Removing a server revokes it immediately. Agents pin the hub certificate by SHA-256.
* Agents never accept file paths or shell strings from the hub. Logs are addressed by logical ids (container, IIS site id, SQL instance) and resolved by the agent. IIS actions are validated against the live site and pool list, and run without a shell.
* Container environment variables are masked in the UI until you reveal them.

## TLS

| `MONITORLAB_TLS` | Behaviour |
|---|---|
| `auto` (default) | self-signed cert in `/data/tls`, created on first start; agents pin its fingerprint |
| `files` | use `MONITORLAB_TLS_CERT` / `MONITORLAB_TLS_KEY` (PEM); agents validate it against public CAs |
| `off` | plain HTTP, for use behind a TLS-terminating reverse proxy; set `MONITORLAB_TRUST_PROXY=true`, `MONITORLAB_SECURE_COOKIES=true` and `MONITORLAB_PUBLIC_URL=https://…` |

## Hub configuration (environment)

| Variable | Default | |
|---|---|---|
| `MONITORLAB_LISTEN` | `0.0.0.0:8443` | listen address |
| `MONITORLAB_DATA_DIR` | `/data` | database, TLS material |
| `MONITORLAB_PUBLIC_URL` | browser origin | URL shown in install commands |
| `MONITORLAB_TLS`, `MONITORLAB_TLS_CERT`, `MONITORLAB_TLS_KEY`, `MONITORLAB_TLS_SANS` | `auto` | see TLS |
| `MONITORLAB_ADMIN_EMAIL` / `MONITORLAB_ADMIN_PASSWORD` | `admin@monitorlab.local` / random | first start only |
| `MONITORLAB_LOCAL_AGENT` | `true` | monitor the hub host |
| `MONITORLAB_HOST_ROOT` | – | host `/` mounted read-only (e.g. `/host`) for real disk usage |
| `MONITORLAB_RETENTION_RAW_DAYS` / `MONITORLAB_RETENTION_DAYS` | `3` / `90` | metrics retention |
| `MONITORLAB_SESSION_IDLE_HOURS` | `12` | session idle timeout |
| `MONITORLAB_TRUST_PROXY`, `MONITORLAB_SECURE_COOKIES` | `false` | reverse-proxy deployments |
| `MONITORLAB_BRAND_NAME` | `MonitorLab` | product name until changed in Settings → General |
| `MONITORLAB_LOG` | `info` | log filter, e.g. `debug,tower_http=info` |

Back up the `monitorlab-data` volume (`monitorlab.db` and `tls/`). If you lose `tls/`, every agent must be re-enrolled, because they pin the old certificate.

## Agent configuration (`agent.toml`)

```toml
hub_url = "https://hub:8443"
agent_id = "…"            # written by `install`
secret = "…"
fingerprint = "AA:BB:…"   # optional pin
interval_secs = 5         # metrics period (2–300)

[docker]
enabled = true
# socket = "/var/run/docker.sock"

[mssql]
# enabled = true          # default: auto-detect
# errorlog_dir = "/var/opt/mssql/log"

[iis]
# enabled = true          # default: auto-detect (Windows)

[kubernetes]
# enabled = true          # default: auto-detect
# kubeconfig = "/etc/monitorlab/kubeconfig"

[shell]
# enabled = false         # turn the "Shell" tab off for this server (default: on)
```

The hub's own agent honours `MONITORLAB_HOST_SHELL=0`.

## Kubernetes (k3s)

The agent finds the cluster by itself: `kubernetes.kubeconfig`, then `KUBECONFIG`, then k3s's `/etc/rancher/k3s/k3s.yaml`, then `~/.kube/config`, then the pod's service account when the agent runs inside the cluster. On a k3s server, `monitorlab-agent install` copies the k3s kubeconfig to `/etc/monitorlab/kubeconfig` (readable only by the agent's service user), so nothing else is needed. The hub's own built-in agent reads `/host/etc/rancher/k3s/k3s.yaml` and reaches an API server that listens on the host's `127.0.0.1` through the Docker host gateway.

CPU and memory figures come from metrics-server, which k3s installs by default. Credential plugins (`exec:` users, as used by EKS/GKE) aren't supported; use a service-account token or client certificate instead.

## What is new in 1.3

- **Kubernetes in depth**: per-pod and per-namespace CPU and memory with requests and limits, OOMKilled markers, node pages, jobs and CronJobs (with "run now"), ingresses, volume claims, config maps and secrets (names only, never values), and usage history per workload and pod.
- **Compose stacks**: start, stop, restart, redeploy or pull-and-redeploy a whole stack, view its compose file, and read all of its logs merged in one view.
- **Services and logs**: systemd units and journald on Linux, Windows services and the Event Log (Windows paths are compile-checked, not run in CI). The agent gets a polkit rule at install time to manage units. Permissions: `services.view`, `services.control`.
- **Per-container history**: CPU and memory over days (raw 3 days, 15-minute rollups for 30 days).
- **Alerts**: per-server thresholds, maintenance windows that mute alerts, repeat reminders, failed-service alerts.
- **Uptime**: HTTP and TCP monitors with keyword checks, response time, uptime percentages and SSL certificate expiry warnings.
- **Agent self-update**: the hub can update agents (button, or automatically). Agents older than 1.3 cannot do this yet; for them the Update button in Servers & agents shows a one-time command (`curl … /download/install.sh | sudo sh -s -- --upgrade --hub …`, or `monitorlab-agent upgrade` on Windows) that keeps the enrollment and needs no token.
- **Scheduled tasks**: a Scheduled tab per server with cron jobs (system crontabs, `/etc/cron.d`, `cron.daily` scripts; user crontabs when the agent runs as root), systemd timers and Windows scheduled tasks. Shows schedule in plain language, next runs, last run and result, the command, and the job's logs (timer service journal, cron lines from the journal, Task Scheduler log). Timers and Windows tasks can be run now, enabled or disabled. Linux installs keep the binary in `/opt/monitorlab/bin`.
- **macOS agent**: `curl ... install.sh | sudo sh` works on a Mac (launchd service, Docker Desktop / OrbStack / Colima sockets). GitHub's hosted macOS runners are not available to this account, so build the agent on the Mac and upload it to the hub: `HUB=https://your-hub EMAIL=you@example.com ./deploy/build-macos-agent.sh`. Uploaded binaries live in the hub's data volume and survive redeploys; agents on Macs then update themselves like any other. (Alternatively register the Mac as a self-hosted runner and set the repository variable `MAC_RUNNER=true`.)
- **PostgreSQL**: connections, databases (size, commits, rollbacks, cache hit, deadlocks), replication, longest transaction and a live Sessions view with cancel / terminate. Add a read-only role and the connection to `agent.toml`:

  ```toml
  [postgres.connection]
  host = "127.0.0.1"
  port = 5432
  user = "monitorlab"
  password = "…"
  database = "postgres"
  sslmode = "disable"   # or "require"
  ```
  ```sql
  CREATE ROLE monitorlab LOGIN PASSWORD '…';
  GRANT pg_monitor TO monitorlab;
  ```
  For the hub's own host set `MONITORLAB_PG_HOST`, `_PORT`, `_USER`, `_PASSWORD`, `_DATABASE` on the hub container. Alerts: unreachable (critical), connections above 90% of the limit, replica more than 5 minutes behind (warning). Permissions: `postgres.view`, `postgres.control`.
- **Server shell**: a Shell tab (PowerShell on Windows) opens an interactive terminal on the server itself, next to the container and pod terminals. Needs the `shell.host` permission, which only Administrators have by default; every session is written to the audit log. The shell runs as the account the agent runs as: the unprivileged `monitorlab` user on Linux (use `sudo` rules or run the agent as root if you need more), root on macOS, LocalSystem on Windows. For the hub's own host the shell is inside the hub container. Windows (ConPTY) and macOS paths are compile-checked, not run in CI.
- **Portainer-style creation**: pull an image, run a new container (ports, environment, volumes, restart policy, network, command), create volumes and networks, all from the Docker tabs, audited. Not covered: building images, deploying a new stack from a compose editor, registry management, app templates, Swarm.
- **Failed services**: "Clear failed state" (systemd `reset-failed`) per unit or for all failed units, and a Review link from the warning banner. Failed `.path`, `.socket` and `.timer` units are listed too.
- **Restart agent**: Servers & agents → ⋮ → Restart agent restarts only the agent process (its service manager starts it again). Not available for the hub's own agent.
- **Install as an app**: the console is a progressive web app. Use the install button in the header (Chrome/Edge) or Add to Home Screen / Add to Dock (Safari).
- **Sorting**: click any column header on list screens to sort ascending, descending, or back to the default.

## Testing

```bash
cargo test --workspace                      # unit tests (protocol, parsers, log tailing, TLS, alert rules)

# end-to-end regression suite against a running hub (uses Docker on this machine)
pip install requests websockets
python3 tests/e2e.py --hub https://127.0.0.1:8443 --email admin@… --password '…' --agent-bin ./monitorlab-agent

# Kubernetes checks against a fake k3s API server (TLS + client certificates, logs, exec):
pip install aiohttp
python3 tests/mock_k8s.py --dir /tmp/mockk8s &        # start the hub with KUBECONFIG=/tmp/mockk8s/kubeconfig
python3 tests/e2e.py … --k8s-mock-dir /tmp/mockk8s

# browser walk-through of every page (screenshots + fails on console errors)
pip install playwright && python -m playwright install chromium
python3 tests/ui_walk.py --hub https://127.0.0.1:8443 --email admin@… --password '…' --out shots/
```

The e2e suite has 196 checks (222 with `--k8s-mock-dir`). It covers authentication hardening (CSRF, origin checks, throttling, cookie flags), every Docker operation including terminal and log streams, rename and recreate (settings, networks and volumes kept), profiles, photos and sessions, the RBAC matrix, server scoping, last-admin protection, enrollment with pinning and token reuse, server revocation, alert channels with real webhook delivery, and audit coverage. It cleans up everything it creates.

## Development

```bash
# hub + agent
cargo test --workspace
cargo build -p monitorlab-hub
MONITORLAB_DATA_DIR=./dev-data MONITORLAB_LISTEN=127.0.0.1:8443 MONITORLAB_AGENTS_DIR=./target/debug ./target/debug/monitorlab-hub

# web console with hot reload (proxies /api to the hub on :8443)
cd web && npm install && npm run dev

# Windows agent from Linux
rustup target add x86_64-pc-windows-gnu && sudo apt install gcc-mingw-w64-x86-64
cargo build -p monitorlab-agent --release --target x86_64-pc-windows-gnu
```

Layout:

```
crates/common   wire protocol (agent ⇄ hub messages)
crates/agent    agent library + binary: system, docker, mssql, iis, log following, install/service
crates/hub      axum server: auth, rbac, registry, api, metrics store, tls; embeds web/dist
web/            React + TypeScript console (uPlot charts, xterm.js terminal)
deploy/         install.sh served to Linux servers
```

Adding a capability (Kubernetes, PostgreSQL, alerts, …) follows one pattern:

1. Add a `Request` / `StreamRequest` variant in `crates/common`.
2. Implement it in the agent.
3. Map it to a permission in `crates/hub/src/rbac.rs`. The compiler forces this step.
4. Add a tab in the UI.

## Roadmap ideas

per-server alert overrides and maintenance windows · Windows services and Event Log · multi-container merged log view · SSO (OIDC) · hub on Windows.
