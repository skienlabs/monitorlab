#!/usr/bin/env python3
"""
MonitorLab end-to-end regression suite.

Runs against a live hub (with its built-in agent and access to Docker) and exercises the
real API, WebSocket streams, enrollment, RBAC, alerting and audit. Every check prints PASS/FAIL;
the exit code is non-zero if anything failed.

  pip install requests websockets
  python3 tests/e2e.py --hub https://127.0.0.1:8443 --email admin@test.local --password '...' \
      --agent-bin ./target/release/monitorlab-agent

Requirements: the hub's local agent can reach Docker, `docker` CLI on this machine talks to the
same engine, and this machine can run the agent binary (for the enrollment tests).
Creates and removes its own containers/volumes/users/servers (all prefixed "e2e-").
"""
import argparse
import asyncio, base64, json, os, secrets, ssl, subprocess, sys, threading, time
from http.server import BaseHTTPRequestHandler, HTTPServer

import requests
import urllib3
import websockets

urllib3.disable_warnings()
RESULTS = []


def check(name, cond, detail=""):
    RESULTS.append((name, bool(cond)))
    print(f"  {'PASS' if cond else 'FAIL'}  {name}{'' if cond else '  -> ' + str(detail)[:300]}", flush=True)
    return bool(cond)


def section(t):
    print(f"\n== {t}", flush=True)


class Client:
    def __init__(self, hub):
        self.hub = hub.rstrip("/")
        self.s = requests.Session()
        self.s.verify = False
        self.s.trust_env = False  # ignore REQUESTS_CA_BUNDLE / proxies from the environment

    def req(self, method, path, body=None, csrf=True, headers=None):
        h = {"content-type": "application/json"}
        if csrf:
            h["x-monitorlab-csrf"] = "1"
        h.update(headers or {})
        r = self.s.request(method, self.hub + "/api" + path, data=None if body is None else json.dumps(body), headers=h, timeout=120)
        try:
            data = r.json()
        except Exception:
            data = r.text
        return r.status_code, data

    def get(self, p):
        return self.req("GET", p)

    def post(self, p, b=None):
        return self.req("POST", p, b if b is not None else {})

    def login(self, email, pw):
        return self.post("/auth/login", {"email": email, "password": pw})

    def cookie(self):
        return self.s.cookies.get("monitorlab_session")


def sh(*args, check_rc=True):
    r = subprocess.run(args, capture_output=True, text=True)
    if check_rc and r.returncode != 0:
        raise RuntimeError(f"{args}: {r.stderr}")
    return r.stdout.strip()


async def ws_stream(hub, cookie, sid, req, sends=(), dur=3.0, origin=None):
    host = hub.split("://", 1)[1]
    url = ("wss://" if hub.startswith("https") else "ws://") + host + f"/api/servers/{sid}/stream"
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    out = []
    try:
        async with websockets.connect(url, ssl=ctx if hub.startswith("https") else None, origin=origin or hub,
                                      additional_headers={"Cookie": f"monitorlab_session={cookie}"}) as ws:
            await ws.send(json.dumps(req))
            for s in sends:
                await asyncio.sleep(0.8)
                await ws.send(json.dumps(s))
            try:
                async with asyncio.timeout(dur):
                    async for m in ws:
                        out.append(json.loads(m))
            except TimeoutError:
                pass
    except Exception as e:
        return f"ERR {e}"
    return out


def stream(*a, **k):
    return asyncio.run(ws_stream(*a, **k))


def wait_for(fn, timeout=30, step=1.0):
    end = time.time() + timeout
    while time.time() < end:
        v = fn()
        if v:
            return v
        time.sleep(step)
    return None


# ---------------------------------------------------------------- webhook sink
HOOKS = []


class Hook(BaseHTTPRequestHandler):
    def do_POST(self):
        HOOKS.append((self.path, json.loads(self.rfile.read(int(self.headers.get("content-length", 0))) or b"{}")))
        self.send_response(200)
        self.end_headers()
        self.wfile.write(b"ok")

    def log_message(self, *a):
        pass


def k8s_checks(hub, admin, sid, mock_dir):
    import requests as rq
    mock = rq.Session()
    mock.trust_env = False
    mock.verify = os.path.join(mock_dir, "ca.crt")
    mock.cert = (os.path.join(mock_dir, "client.crt"), os.path.join(mock_dir, "client.key"))
    kubeconfig = open(os.path.join(mock_dir, "kubeconfig")).read()
    port = kubeconfig.split("server: https://127.0.0.1:")[1].split()[0]
    mstate = lambda: mock.get(f"https://127.0.0.1:{port}/_mock/state", timeout=5).json()

    s = wait_for(lambda: (lambda x: x if (x[1].get("hello") or {}).get("capabilities", {}).get("kubernetes") else None)(admin.get(f"/servers/{sid}")), 20)
    check("kubernetes capability detected", bool(s), admin.get(f"/servers/{sid}")[1].get("hello"))
    rpc = lambda body: admin.post(f"/servers/{sid}/rpc", body)
    code, o = rpc({"op": "k8s_overview"})
    check("cluster overview", code == 200 and o["distribution"] == "k3s" and o["summary"]["nodes_ready"] == 1 and o["metrics_available"], (code, o))
    n = o["nodes"][0] if code == 200 else {}
    check("node capacity and usage parsed", n.get("cpu_capacity") == 8000 and abs((n.get("cpu_used") or 0) - 1234.57) < 1 and n.get("mem_used") == 10485760 * 1024, n)
    check("failing pod reported", code == 200 and o["summary"]["pods_failing"] == 1 and "default/api-5f6d7c8b9-xyz12: CrashLoopBackOff" in o["summary"]["failing"], o.get("summary"))
    code, d = rpc({"op": "k8s_pods"})
    pods = {p["name"]: p for p in d.get("pods", [])} if code == 200 else {}
    api = pods.get("api-5f6d7c8b9-xyz12", {})
    check("pods with status, restarts, metrics", api.get("status") == "CrashLoopBackOff" and api.get("restarts") == 7 and api.get("failing") and pods.get("web-7d4b9c6f5-abcde", {}).get("ready") == "2/2" and pods["web-7d4b9c6f5-abcde"]["cpu"] == 15, api)
    code, d = rpc({"op": "k8s_pods", "namespace": "kube-system"})
    check("namespace filter", code == 200 and {p["namespace"] for p in d["pods"]} == {"kube-system"}, d)
    code, d = rpc({"op": "k8s_workloads"})
    check("workloads with degraded flag", code == 200 and any(w["name"] == "api" and w["degraded"] for w in d) and any(w["name"] == "web" and not w["degraded"] for w in d), d)
    code, d = rpc({"op": "k8s_services"})
    check("services", code == 200 and d[0]["ports"] == ["80:30080/TCP"], d)
    code, d = rpc({"op": "k8s_events"})
    check("events newest first", code == 200 and d[0]["reason"] == "BackOff" and d[0]["count"] == 42, d)
    code, d = rpc({"op": "k8s_get", "kind": "pod", "namespace": "default", "name": "api-5f6d7c8b9-xyz12"})
    check("pod details without managedFields, with events", code == 200 and "managedFields" not in d["object"]["metadata"] and d["events"][0]["reason"] == "BackOff", d)
    code, d = rpc({"op": "k8s_get", "kind": "pod", "namespace": "default", "name": "nope"})
    check("missing pod gives the API's message", code != 200 and "not found" in str(d), (code, d))
    out = stream(hub, admin.cookie(), sid, {"kind": "k8s_pod_logs", "namespace": "default", "name": "web-7d4b9c6f5-abcde", "container": "sidecar", "tail": 3}, dur=2.5)
    lines = [l for msg in out if msg.get("t") == "data" for l in msg["d"].get("lines", [])] if isinstance(out, list) else []
    check("pod log stream (container, tail, follow, timestamps)", any("web-7d4b9c6f5-abcde/sidecar line 0" in l["text"] for l in lines) and any("live line" in l["text"] for l in lines) and all(l.get("ts") for l in lines), out)
    out = stream(hub, admin.cookie(), sid, {"kind": "k8s_pod_logs", "namespace": "default", "name": "api-5f6d7c8b9-xyz12", "previous": True, "follow": False}, dur=2)
    lines = [l for msg in out if msg.get("t") == "data" for l in msg["d"].get("lines", [])] if isinstance(out, list) else []
    check("previous container logs", any("previous" in l["text"] for l in lines), out)
    out = stream(hub, admin.cookie(), sid, {"kind": "k8s_pod_exec", "namespace": "default", "name": "web-7d4b9c6f5-abcde", "container": "web"},
                 sends=[{"t": "resize", "cols": 101, "rows": 33}, {"t": "input", "b64": base64.b64encode(b"ls -la\n").decode()}], dur=3)
    txt = b"".join(base64.b64decode(msg["d"]["b64"]) for msg in out if isinstance(out, list) and msg["t"] == "data" and "b64" in msg["d"]).decode(errors="replace") if isinstance(out, list) else ""
    check("pod exec terminal (stdin, stdout, resize)", "mock shell in web-7d4b9c6f5-abcde/web" in txt and "ls -la" in txt and "[resize 101x33]" in txt, txt or out)
    check("exec input reached the API server", "ls -la" in mstate()["exec_input"], mstate())
    code, d = rpc({"op": "k8s_rollout_restart", "kind": "deployment", "namespace": "default", "name": "web"})
    p = [x for x in mstate()["patches"] if x[0] == "deployments" and x[1] == "web"]
    check("rollout restart patches restartedAt", code == 200 and p and p[-1][2] == "application/strategic-merge-patch+json" and "kubectl.kubernetes.io/restartedAt" in json.dumps(p[-1][3]), (code, d, p))
    code, d = rpc({"op": "k8s_scale", "kind": "deployment", "namespace": "default", "name": "api", "replicas": 3})
    p = [x for x in mstate()["patches"] if x[0] == "deployments" and x[1] == "api"]
    check("scale deployment", code == 200 and p and p[-1][3] == {"spec": {"replicas": 3}} and p[-1][2] == "application/merge-patch+json", (code, d, p))
    code, d = rpc({"op": "k8s_scale", "kind": "daemon_set", "namespace": "default", "name": "x", "replicas": 3})
    check("daemon sets cannot be scaled", code != 200, (code, d))
    code, d = rpc({"op": "k8s_node_cordon", "name": "k3s-node-1", "unschedulable": True})
    check("cordon node", code == 200 and mstate()["cordoned"] == ["k3s-node-1"], (code, d))
    rpc({"op": "k8s_node_cordon", "name": "k3s-node-1", "unschedulable": False})
    code, d = rpc({"op": "k8s_pod_delete", "namespace": "default", "name": "api-5f6d7c8b9-xyz12"})
    check("delete pod", code == 200 and ["default", "api-5f6d7c8b9-xyz12"] in mstate()["deleted"], (code, d))
    m = wait_for(lambda: (lambda x: x if x and x.get("k8s") else None)(admin.get(f"/servers/{sid}")[1].get("latest")), 20)
    check("k8s summary in metrics", bool(m) and m["k8s"]["nodes"] == 1, m and m.get("k8s"))
    fired = wait_for(lambda: any("pod(s) failing" in x["message"] for x in admin.get("/alerts?state=active")[1]), 45)
    check("alert for failing pods", bool(fired), admin.get("/alerts?state=active")[1])
    # RBAC: a Viewer can read the cluster but not change it or open a shell.
    email = f"k8sviewer-{secrets.token_hex(3)}@x.test"
    code, roles = admin.get("/roles")
    viewer = next(r for r in roles if r["name"] == "Viewer")
    check("Viewer role gained k8s.view on upgrade", "k8s.view" in viewer["permissions"] and "k8s.control" not in viewer["permissions"], viewer["permissions"])
    code, u = admin.post("/users", {"email": email, "name": "K8s Viewer", "role_id": viewer["id"], "password": "Viewer-Pass-123", "must_change_password": False})
    v = Client(hub)
    v.login(email, "Viewer-Pass-123")
    check("viewer can list pods", v.post(f"/servers/{sid}/rpc", {"op": "k8s_pods"})[0] == 200)
    check("viewer cannot delete pods", v.post(f"/servers/{sid}/rpc", {"op": "k8s_pod_delete", "namespace": "default", "name": "web-7d4b9c6f5-abcde"})[0] == 403)
    out = stream(hub, v.cookie(), sid, {"kind": "k8s_pod_exec", "namespace": "default", "name": "web-7d4b9c6f5-abcde"}, dur=1.5)
    check("viewer cannot open a pod terminal", isinstance(out, list) and any(m.get("t") == "end" and "permission" in str(m).lower() for m in out) or (isinstance(out, str) and "403" in out), out)
    if code == 200:
        admin.req("DELETE", f"/users/{u['id']}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--hub", required=True)
    ap.add_argument("--email", required=True)
    ap.add_argument("--password", required=True)
    ap.add_argument("--agent-bin", help="monitorlab-agent binary for enrollment tests")
    ap.add_argument("--webhook-port", type=int, default=0, help="local port for the webhook sink (0 = random)")
    ap.add_argument("--webhook-host", default="127.0.0.1",
                    help="address the HUB uses to reach this machine's webhook sink "
                         "(e.g. the Docker network gateway when the hub runs on a bridge network)")
    ap.add_argument("--k8s-mock-dir", help="directory of tests/mock_k8s.py (hub started with its kubeconfig)")
    a = ap.parse_args()
    tag = "e2e-" + secrets.token_hex(3)
    hub = a.hub.rstrip("/")

    bind = "127.0.0.1" if a.webhook_host in ("127.0.0.1", "localhost") else "0.0.0.0"
    srv = HTTPServer((bind, a.webhook_port), Hook)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    hook_url = f"http://{a.webhook_host}:{srv.server_port}"

    admin = Client(hub)
    created_containers, created_users, created_servers, created_channels, agent_procs = [], [], [], [], []
    try:
        # ------------------------------------------------------------ security basics
        section("Security")
        code, _ = Client(hub).get("/servers")
        check("unauthenticated API returns 401", code == 401, code)
        code, d = admin.req("POST", "/auth/login", {"email": a.email, "password": a.password}, csrf=False)
        check("login without CSRF header is rejected", code == 403, (code, d))
        code, d = admin.req("POST", "/auth/login", {"email": a.email, "password": a.password}, headers={"Origin": "https://evil.example"})
        check("login with foreign Origin is rejected", code == 403, (code, d))
        code, d = Client(hub).login(a.email, "definitely-wrong-password")
        check("wrong password returns generic 401", code == 401 and "Invalid email or password" in str(d), (code, d))
        ghost = Client(hub)
        codes = [ghost.login(f"{tag}-ghost@x.test", "nope-nope-nope")[0] for _ in range(6)]
        check("per-account throttling locks after 5 failures", codes[-1] == 429, codes)
        code, me = admin.login(a.email, a.password)
        check("admin login", code == 200 and "users.manage" in me.get("permissions", []), (code, me))
        r = admin.s.get(hub + "/")
        csp = r.headers.get("content-security-policy", "")
        check("security headers on UI", "frame-ancestors 'none'" in csp and r.headers.get("x-frame-options") == "DENY", dict(r.headers))
        setc = admin.s.post(hub + "/api/auth/login", json={"email": a.email, "password": a.password}, headers={"x-monitorlab-csrf": "1"}).headers.get("set-cookie", "")
        check("session cookie is HttpOnly + SameSite=Strict", "HttpOnly" in setc and "SameSite=Strict" in setc, setc)
        code, _ = admin.get("/does-not-exist")
        check("unknown API path is a JSON 404", code == 404)

        # ------------------------------------------------------------ servers & local agent
        section("Servers & built-in agent")
        code, servers = admin.get("/servers")
        local = next((s for s in servers if s["is_local"]), None) if code == 200 else None
        check("local agent registered and online", local and local["online"], servers)
        if not local:
            raise SystemExit("cannot continue without the local agent")
        sid = local["id"]
        check("hello reports Docker capability", local["hello"]["capabilities"]["docker"], local["hello"])
        m = wait_for(lambda: admin.get(f"/servers/{sid}")[1].get("latest"), 30)
        check("metrics sample received", m and m["mem_total"] > 0 and isinstance(m["disks"], list), m)
        code, met = admin.get(f"/servers/{sid}/metrics?range=1h")
        check("history endpoint returns columnar series", code == 200 and "cpu" in met["series"] and len(met["ts"]) == len(met["series"]["cpu"]), met if code != 200 else "")
        for op in ["system_detail", "docker_containers", "docker_images", "docker_volumes", "docker_networks", "docker_disk_usage"]:
            code, d = admin.post(f"/servers/{sid}/rpc", {"op": op})
            check(f"rpc {op}", code == 200, (code, str(d)[:200]))
        code, d = admin.post(f"/servers/{sid}/rpc", {"op": "not_a_real_op"})
        check("unknown rpc op rejected", code in (400, 422), (code, d))
        code, d = admin.post(f"/servers/{sid}/rpc", {"op": "iis_sites"})
        check("IIS on Linux returns a clear error", code == 502 and "Windows" in str(d), (code, d))

        # ------------------------------------------------------------ docker operations
        section("Docker operations")
        img = sh("docker", "inspect", "--format", "{{.Config.Image}}", sh("docker", "ps", "-q").splitlines()[0]) if sh("docker", "ps", "-q") else "alpine"
        # Use any locally available small image that has /bin/sh.
        images = sh("docker", "images", "--format", "{{.Repository}}:{{.Tag}}").splitlines()
        img = next((i for i in images if "alpine" in i and "<none>" not in i), images[0])
        cname = f"{tag}-app"
        sh("docker", "run", "-d", "--name", cname, "--label", f"com.docker.compose.project={tag}", img, "sh", "-c",
           'i=0; while true; do i=$((i+1)); echo "MARK-$i {\\"level\\":\\"info\\"}"; echo "fail: boom $i" >&2; sleep 1; done')
        created_containers.append(cname)
        time.sleep(3)
        code, cs = admin.post(f"/servers/{sid}/rpc", {"op": "docker_containers"})
        mine = next((c for c in cs if c["name"] == cname), None)
        check("container listed with compose project", mine and mine["project"] == tag and mine["state"] == "running", mine)
        out = stream(hub, admin.cookie(), sid, {"kind": "container_logs", "id": cname, "tail": 10}, dur=3)
        lines = [l for msg in out if isinstance(out, list) and msg["t"] == "data" for l in msg["d"].get("lines", [])] if isinstance(out, list) else []
        check("log stream returns stdout and stderr with timestamps", any("MARK-" in l["text"] for l in lines) and any(l["s"] == 2 for l in lines) and all(l.get("ts") for l in lines), out if not lines else "")
        out = stream(hub, admin.cookie(), sid, {"kind": "container_exec", "id": cname},
                     sends=[{"t": "resize", "cols": 91, "rows": 27}, {"t": "input", "b64": base64.b64encode(b"echo E2E_$((40+2)); stty size; exit\n").decode()}], dur=4)
        txt = b"".join(base64.b64decode(msg["d"]["b64"]) for msg in out if isinstance(out, list) and msg["t"] == "data" and "b64" in msg["d"]).decode(errors="replace") if isinstance(out, list) else ""
        check("terminal exec runs commands", "E2E_42" in txt, txt[-200:] or out)
        check("terminal resize applied", "27 91" in txt, txt[-200:])
        out = stream(hub, admin.cookie(), sid, {"kind": "container_stats", "id": cname}, dur=3.5)
        check("live container stats", isinstance(out, list) and any(msg["t"] == "data" and msg["d"]["k"] == "stats" for msg in out), out)
        out = stream(hub, admin.cookie(), sid, {"kind": "system_live", "interval_ms": 1000}, dur=2.5)
        check("live system stream", isinstance(out, list) and len(out) >= 1, out)
        out = stream(hub, admin.cookie(), sid, {"kind": "container_stats_all"}, dur=4.5)
        batches = [msg["d"] for msg in out if msg["t"] == "data" and msg["d"].get("k") == "stats_all"] if isinstance(out, list) else []
        check("live stats for all containers", any(any(i["id"].startswith(cid) or i["id"] == cid for i in b["items"]) for b in batches) if (cid := next((c["id"] for c in admin.post(f"/servers/{sid}/rpc", {"op": "docker_containers"})[1] if c["name"] == cname), None)) else False, out)
        out = stream(hub, admin.cookie(), sid, {"kind": "system_live"}, origin="https://evil.example", dur=1)
        check("cross-origin WebSocket rejected", isinstance(out, str) and "403" in out, out)
        for action in ["restart", "pause", "unpause", "stop", "start"]:
            code, d = admin.post(f"/servers/{sid}/rpc", {"op": "docker_container_action", "id": cname, "action": action})
            check(f"container {action}", code == 200, (code, d))
        state = sh("docker", "inspect", "--format", "{{.State.Status}}", cname)
        check("container running after start", state == "running", state)
        code, d = admin.post(f"/servers/{sid}/rpc", {"op": "docker_container_rename", "id": cname, "name": f"{cname}-ren"})
        check("container rename", code == 200 and sh("docker", "inspect", "--format", "{{.Name}}", f"{cname}-ren") == f"/{cname}-ren", (code, d))
        code, d = admin.post(f"/servers/{sid}/rpc", {"op": "docker_container_rename", "id": f"{cname}-ren", "name": "bad name!"})
        check("invalid container name rejected", code != 200, (code, d))
        admin.post(f"/servers/{sid}/rpc", {"op": "docker_container_rename", "id": f"{cname}-ren", "name": cname})

        # Recreate keeps settings, named + anonymous volumes and networks.
        rname, rnet, rvol = f"{tag}-rc", f"{tag}-net", f"{tag}-rcvol"
        sh("docker", "network", "create", rnet)
        sh("docker", "run", "-d", "--name", rname, "--network", rnet, "--network-alias", "svc-alias", "-e", "KEEP=me",
           "-v", f"{rvol}:/data", "-v", "/anon", "--label", f"com.docker.compose.project={tag}", "--restart", "unless-stopped",
           img, "sh", "-c", "echo hi > /anon/marker; echo v1 > /data/v; sleep 3600")
        created_containers.append(rname)
        time.sleep(1)
        old_id = sh("docker", "inspect", "--format", "{{.Id}}", rname)
        anon = sh("docker", "inspect", "--format", "{{range .Mounts}}{{if eq .Destination \"/anon\"}}{{.Name}}{{end}}{{end}}", rname)
        code, d = admin.post(f"/servers/{sid}/rpc", {"op": "docker_container_recreate", "id": rname, "pull": False})
        new_id = sh("docker", "inspect", "--format", "{{.Id}}", rname, check_rc=False)
        check("recreate returns a new container", code == 200 and d.get("id") == new_id and new_id != old_id, (code, d))
        insp = json.loads(sh("docker", "inspect", rname))[0] if new_id else {}
        check("recreate keeps env, labels, restart policy", "KEEP=me" in insp.get("Config", {}).get("Env", []) and insp["Config"]["Labels"].get("com.docker.compose.project") == tag and insp["HostConfig"]["RestartPolicy"]["Name"] == "unless-stopped", insp.get("Config", {}).get("Env"))
        nets = insp.get("NetworkSettings", {}).get("Networks", {})
        check("recreate keeps network and alias", rnet in nets and "svc-alias" in (nets[rnet].get("Aliases") or []), nets)
        check("recreate keeps named and anonymous volumes", sh("docker", "exec", rname, "cat", "/data/v") == "v1" and sh("docker", "exec", rname, "cat", "/anon/marker") == "hi", anon)
        check("recreate leaves no backup container", not sh("docker", "ps", "-aq", "--filter", f"name={rname}-old"), sh("docker", "ps", "-a", "--format", "{{.Names}}"))
        check("recreate keeps it running", insp.get("State", {}).get("Running") is True, insp.get("State"))
        code, d = admin.post(f"/servers/{sid}/rpc", {"op": "docker_container_recreate", "id": rname, "pull": True})
        check("recreate with image pull", code == 200 and d.get("pulled") is True, (code, d))
        sh("docker", "rm", "-f", rname)
        created_containers.remove(rname)
        sh("docker", "volume", "rm", rvol, check_rc=False)
        if anon:
            sh("docker", "volume", "rm", anon, check_rc=False)
        sh("docker", "network", "rm", rnet, check_rc=False)
        code, d = admin.post(f"/servers/{sid}/rpc", {"op": "docker_container_remove", "id": cname})
        check("remove running container without force fails", code == 502, (code, d))
        code, d = admin.post(f"/servers/{sid}/rpc", {"op": "docker_container_remove", "id": cname, "force": True, "volumes": True})
        check("force remove", code == 200 and not sh("docker", "ps", "-aq", "--filter", f"name=^{cname}$"), (code, d))
        created_containers.remove(cname)
        vol = f"{tag}-vol"
        sh("docker", "volume", "create", vol)
        code, d = admin.post(f"/servers/{sid}/rpc", {"op": "docker_volume_remove", "name": vol})
        check("volume delete", code == 200 and vol not in sh("docker", "volume", "ls", "-q"), (code, d))
        for t in ["images_dangling", "build_cache", "networks", "containers"]:
            code, d = admin.post(f"/servers/{sid}/rpc", {"op": "docker_prune", "target": t})
            check(f"prune {t}", code == 200 and "space_reclaimed" in d, (code, d))

        # ------------------------------------------------------------ users, roles, scope
        section("Users, roles and access control")
        code, roles = admin.get("/roles")
        rid = {r["name"]: r["id"] for r in roles}
        code, d = admin.post("/users", {"email": f"{tag}@x.test", "password": "short", "role_id": rid["Tester"]})
        check("weak password rejected", code == 400, (code, d))
        code, d = admin.post("/users", {"email": f"{tag}@x.test", "name": "E2E", "password": "Tester-Pass-123", "role_id": rid["Tester"], "must_change_password": True})
        check("create tester", code == 200, (code, d))
        uid = d.get("id")
        created_users.append(uid)
        code, d = admin.post("/users", {"email": f"{tag}@x.test", "password": "Tester-Pass-123", "role_id": rid["Tester"]})
        check("duplicate email rejected", code == 400, (code, d))
        t = Client(hub)
        t.login(f"{tag}@x.test", "Tester-Pass-123")
        code, d = t.get("/servers")
        check("forced password change blocks API", code == 403 and d.get("error") == "password_change_required", (code, d))
        code, d = t.post("/auth/password", {"current": "Tester-Pass-123", "new": "Tester-New-456!"})
        check("password change", code == 200, (code, d))
        code, _ = t.post(f"/servers/{sid}/rpc", {"op": "docker_containers"})
        check("tester can view containers", code == 200)
        code, d = t.post(f"/servers/{sid}/rpc", {"op": "docker_prune", "target": "images_unused"})
        check("tester cannot prune", code == 403, (code, d))
        code, d = t.post(f"/servers/{sid}/rpc", {"op": "docker_container_remove", "id": "x"})
        check("tester cannot delete containers", code == 403, (code, d))
        out = stream(hub, t.cookie(), sid, {"kind": "container_exec", "id": "x"}, dur=2)
        check("tester cannot open a terminal", isinstance(out, list) and out and "containers.exec" in str(out[0].get("error")), out)
        for p in ["/users", "/roles", "/audit", "/alert-channels"]:
            code, _ = t.get(p)
            check(f"tester forbidden: GET {p}", code == 403, code)
        code, d = admin.req("PATCH", f"/users/{me['id']}", {"role_id": rid["Viewer"]})
        check("cannot demote yourself", code == 400, (code, d))
        code, d = admin.req("DELETE", f"/users/{me['id']}")
        check("cannot delete yourself", code == 400, (code, d))
        code, d = admin.req("PATCH", f"/roles/{rid['Administrator']}", {"name": "Administrator", "permissions": []})
        check("Administrator role is immutable", code == 400, (code, d))
        # server scope
        code, other = admin.post("/servers", {"name": f"{tag}-scoped"})
        created_servers.append(other["id"])
        admin.req("PATCH", f"/users/{uid}", {"server_scope": [other["id"]]})
        code, lst = t.get("/servers")
        check("scoped user sees only allowed servers", code == 200 and [s["id"] for s in lst] == [other["id"]], lst)
        code, _ = t.get(f"/servers/{sid}")
        check("scoped user gets 404 for other servers", code == 404, code)
        code, _ = t.post(f"/servers/{sid}/rpc", {"op": "docker_containers"})
        check("scoped user cannot call other servers", code == 404, code)
        admin.req("PATCH", f"/users/{uid}", {"disabled": True})
        code, _ = t.get("/auth/me")
        check("disabling a user ends their session", code == 401, code)

        # ------------------------------------------------------------ enrollment
        section("Agent enrollment")
        if a.agent_bin and os.path.exists(a.agent_bin):
            code, info = admin.get("/hub/info")
            fp = info.get("fingerprint")
            code, srvd = admin.post("/servers", {"name": f"{tag}-agent"})
            created_servers.append(srvd["id"])
            cfg = f"/tmp/{tag}.toml"
            if fp:
                bad = subprocess.run([a.agent_bin, "enroll", "--hub", hub, "--token", srvd["token"], "--fingerprint", ":".join(["AA"] * 32), "--config", cfg], capture_output=True, text=True)
                check("wrong fingerprint refused", bad.returncode != 0 and "mismatch" in bad.stderr, bad.stderr[-300:])
            ok = subprocess.run([a.agent_bin, "enroll", "--hub", hub, "--token", srvd["token"], "--config", cfg] + (["--fingerprint", fp] if fp else []), capture_output=True, text=True)
            check("enrollment succeeds", ok.returncode == 0, ok.stderr[-300:])
            again = subprocess.run([a.agent_bin, "enroll", "--hub", hub, "--token", srvd["token"], "--config", cfg + "2"] + (["--fingerprint", fp] if fp else []), capture_output=True, text=True)
            check("token cannot be reused", again.returncode != 0 and "invalid or expired" in again.stderr, again.stderr[-200:])
            check("agent config is private (0600)", oct(os.stat(cfg).st_mode & 0o777) == "0o600", oct(os.stat(cfg).st_mode))
            p = subprocess.Popen([a.agent_bin, "run", "--config", cfg], stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
            agent_procs.append(p)
            online = wait_for(lambda: admin.get(f"/servers/{srvd['id']}")[1].get("online"), 30)
            check("enrolled agent comes online", online)
            code, d = admin.req("PATCH", f"/servers/{srvd['id']}", {"name": f"{tag}-renamed"})
            check("rename server", code == 200)
            code, d = admin.req("DELETE", f"/servers/{srvd['id']}")
            check("remove server", code == 200)
            created_servers.remove(srvd["id"])
            time.sleep(4)
            code, lst = admin.get("/servers")
            check("removed agent cannot reconnect", srvd["id"] not in [s["id"] for s in lst], lst)
        else:
            print("  SKIP  enrollment tests (no --agent-bin)")

        # ------------------------------------------------------------ alerting
        section("Alerting")
        code, rules = admin.get("/alert-rules")
        check("read alert rules", code == 200 and rules["cpu"]["for_min"] >= 0, rules)
        bad = json.loads(json.dumps(rules))
        bad["cpu"]["warn"], bad["cpu"]["crit"] = 90, 50
        code, d = admin.req("PUT", "/alert-rules", bad)
        check("invalid thresholds rejected", code == 400, (code, d))
        code, d = admin.req("PUT", "/alert-rules", rules)
        check("save alert rules", code == 200, (code, d))
        for kind in ["teams", "slack", "webhook"]:
            code, d = admin.post("/alert-channels", {"name": f"{tag}-{kind}", "kind": kind, "url": f"{hook_url}/{kind}"})
            check(f"create {kind} channel", code == 200, (code, d))
            created_channels.append(d.get("id"))
            code, d = admin.post(f"/alert-channels/{d.get('id')}/test")
            check(f"{kind} test notification delivered", code == 200, (code, d))
        paths = {p for p, _ in HOOKS}
        check("teams payload is an Adaptive Card", any(p == "/teams" and b.get("attachments", [{}])[0].get("contentType") == "application/vnd.microsoft.card.adaptive" for p, b in HOOKS), HOOKS)
        check("slack payload has text", any(p == "/slack" and "text" in b for p, b in HOOKS), paths)
        check("generic webhook payload has fields", any(p == "/webhook" and b.get("source") == "monitorlab" and b.get("severity") for p, b in HOOKS), paths)
        code, d = admin.post("/alert-channels", {"name": f"{tag}-mail", "kind": "email", "url": "not-an-email"})
        check("email channel validates recipients", code == 400, (code, d))
        code, d = admin.post("/alert-channels", {"name": f"{tag}-x", "kind": "webhook", "url": "ftp://x"})
        check("webhook channel validates URL", code == 400, (code, d))
        code, d = admin.get("/alerts?state=all")
        check("alerts list", code == 200 and isinstance(d, list), (code, d))

        # ------------------------------------------------------------ branding
        section("Branding")
        code, d = Client(hub).get("/branding")
        check("product name is public (sign-in page)", code == 200 and d.get("name"), (code, d))
        original = d.get("name")
        check("branding exposes company and version", d.get("company") and d.get("version"), d)
        code, d = admin.req("PUT", "/branding", {"name": "x" * 41})
        check("overlong product name rejected", code == 400, (code, d))
        code, d = admin.req("PUT", "/branding", {"name": f"{tag} Monitoring"})
        check("admin can rename the product", code == 200 and Client(hub).get("/branding")[1]["name"] == f"{tag} Monitoring", (code, d))
        code, d = admin.req("PUT", "/branding", {"name": original})
        check("product name restored", code == 200 and Client(hub).get("/branding")[1]["name"] == original, (code, d))

        # ------------------------------------------------------------ audit
        section("Profile")
        code, d = admin.req("PUT", "/auth/profile", {"name": "E2E Admin", "title": "SRE", "phone": "+91 98765 43210"})
        code2, me = admin.get("/auth/me")
        check("profile update", code == 200 and me.get("name") == "E2E Admin" and me.get("title") == "SRE" and me.get("phone") == "+91 98765 43210", (code, me))
        code, d = admin.req("PUT", "/auth/profile", {"name": "E2E Admin", "email": "other@x.test", "current_password": "wrong-password-123"})
        check("email change needs the current password", code == 400, (code, d))
        code, d = admin.req("PUT", "/auth/profile", {"name": "x" * 200})
        check("over-long name rejected", code == 400, (code, d))
        png = base64.b64decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==")
        r = admin.s.put(hub + "/api/auth/avatar", data=png, headers={"x-monitorlab-csrf": "1", "content-type": "image/png"})
        _, me = admin.get("/auth/me")
        check("upload profile photo", r.status_code == 200 and (me.get("avatar") or "").startswith(f"/api/users/{me['id']}/avatar"), (r.status_code, r.text))
        r2 = admin.s.get(hub + me["avatar"])
        check("photo served with nosniff and image type", r2.status_code == 200 and r2.content == png and r2.headers.get("content-type") == "image/png" and r2.headers.get("x-content-type-options") == "nosniff", dict(r2.headers))
        r3 = Client(hub).s.get(hub + me["avatar"])
        check("photo needs a session", r3.status_code == 401, r3.status_code)
        r4 = admin.s.put(hub + "/api/auth/avatar", data=b"<svg xmlns='http://www.w3.org/2000/svg'><script>alert(1)</script></svg>", headers={"x-monitorlab-csrf": "1", "content-type": "image/png"})
        check("SVG / non-image upload rejected", r4.status_code == 400, (r4.status_code, r4.text))
        r5 = admin.s.put(hub + "/api/auth/avatar", data=png + b"0" * (600 * 1024), headers={"x-monitorlab-csrf": "1", "content-type": "image/png"})
        check("oversized photo rejected", r5.status_code in (400, 413), r5.status_code)
        r6 = admin.s.put(hub + "/api/auth/avatar", data=png, headers={"content-type": "image/png"})
        check("photo upload needs CSRF header", r6.status_code == 403, r6.status_code)
        other = Client(hub)
        other.login(a.email, a.password)
        code, ss = admin.get("/auth/sessions")
        check("sessions listed with current marked", code == 200 and sum(1 for x in ss if x["current"]) == 1 and len(ss) >= 2, ss)
        victim = next((x for x in ss if not x["current"]), None)
        code, d = admin.req("DELETE", f"/auth/sessions/{victim['id']}") if victim else (0, None)
        check("revoke one session", code == 200, (code, d))
        code, d = admin.post("/auth/sessions/revoke-others")
        check("revoke all other sessions", code == 200 and other.get("/auth/me")[0] == 401, (code, d))
        code, d = admin.req("DELETE", "/auth/avatar")
        check("remove profile photo", code == 200 and admin.get("/auth/me")[1].get("avatar") is None, (code, d))

        if a.k8s_mock_dir:
            section("Kubernetes (mock API server)")
            k8s_checks(hub, admin, sid, a.k8s_mock_dir)

        section("Audit")
        code, d = admin.get("/audit?limit=200")
        actions = {e["action"] for e in d} if code == 200 else set()
        for need in ["login", "login_failed", "container_exec", "docker_container_recreate", "profile_updated", "docker_container_remove", "docker_prune", "user_created", "server_added", "channel_created", "branding_updated"]:
            check(f"audit contains {need}", need in actions, sorted(actions))
    finally:
        section("Cleanup")
        for p in agent_procs:
            p.terminate()
        for c in created_containers:
            subprocess.run(["docker", "rm", "-f", c], capture_output=True)
        for u in created_users:
            admin.req("DELETE", f"/users/{u}")
        for s in created_servers:
            admin.req("DELETE", f"/servers/{s}")
        for c in created_channels:
            if c:
                admin.req("DELETE", f"/alert-channels/{c}")
        srv.shutdown()
        print("  done")

    failed = [n for n, ok in RESULTS if not ok]
    print(f"\n{len(RESULTS) - len(failed)}/{len(RESULTS)} checks passed")
    if failed:
        print("FAILED:\n  " + "\n  ".join(failed))
        sys.exit(1)


if __name__ == "__main__":
    main()
