#!/usr/bin/env python3
"""A small fake Kubernetes API server for MonitorLab tests.

Mimics k3s: HTTPS with its own CA, client-certificate authentication (EC P-256 keys),
real response shapes for the endpoints MonitorLab uses, a streaming log endpoint and the
v4.channel.k8s.io exec WebSocket. Writes a kubeconfig the agent can use.

    python3 tests/mock_k8s.py --dir /tmp/mockk8s --port 16443
    KUBECONFIG=/tmp/mockk8s/kubeconfig monitorlab-hub ...
"""
import argparse
import asyncio
import base64
import json
import os
import ssl
import subprocess
import time
from datetime import datetime, timedelta, timezone

from aiohttp import WSMsgType, web


def iso(dt):
    return dt.strftime("%Y-%m-%dT%H:%M:%SZ")


NOW = datetime.now(timezone.utc)
STATE = {"deleted": [], "patches": [], "cordoned": set(), "exec_input": b""}


def make_pki(d):
    os.makedirs(d, exist_ok=True)
    sh = lambda *a: subprocess.run(a, check=True, capture_output=True, cwd=d)
    if os.path.exists(os.path.join(d, "kubeconfig")):
        return
    sh("openssl", "ecparam", "-name", "prime256v1", "-genkey", "-noout", "-out", "ca.key")
    sh("openssl", "req", "-x509", "-new", "-key", "ca.key", "-subj", "/CN=k3s-server-ca", "-days", "30", "-out", "ca.crt",
       "-addext", "basicConstraints=critical,CA:TRUE", "-addext", "keyUsage=critical,keyCertSign,cRLSign",
       "-addext", "subjectKeyIdentifier=hash")
    open(os.path.join(d, "san.ext"), "w").write("subjectAltName=DNS:kubernetes,DNS:localhost,IP:127.0.0.1\nextendedKeyUsage=serverAuth\n"
                                                "keyUsage=critical,digitalSignature\nauthorityKeyIdentifier=keyid\nbasicConstraints=CA:FALSE\n")
    sh("openssl", "ecparam", "-name", "prime256v1", "-genkey", "-noout", "-out", "server.key")
    sh("openssl", "req", "-new", "-key", "server.key", "-subj", "/CN=k3s", "-out", "server.csr")
    sh("openssl", "x509", "-req", "-in", "server.csr", "-CA", "ca.crt", "-CAkey", "ca.key", "-CAcreateserial", "-days", "30", "-extfile", "san.ext", "-out", "server.crt")
    open(os.path.join(d, "client.ext"), "w").write("extendedKeyUsage=clientAuth\nkeyUsage=critical,digitalSignature\n"
                                                   "authorityKeyIdentifier=keyid\nbasicConstraints=CA:FALSE\n")
    # k3s writes SEC1 "EC PRIVATE KEY" client keys — exercise exactly that format.
    sh("openssl", "ecparam", "-name", "prime256v1", "-genkey", "-noout", "-out", "client.key")
    sh("openssl", "req", "-new", "-key", "client.key", "-subj", "/O=system:masters/CN=system:admin", "-out", "client.csr")
    sh("openssl", "x509", "-req", "-in", "client.csr", "-CA", "ca.crt", "-CAkey", "ca.key", "-CAcreateserial", "-days", "30", "-extfile", "client.ext", "-out", "client.crt")
    b = lambda f: base64.b64encode(open(os.path.join(d, f), "rb").read()).decode()
    return b


def write_kubeconfig(d, port):
    b = lambda f: base64.b64encode(open(os.path.join(d, f), "rb").read()).decode()
    cfg = f"""apiVersion: v1
clusters:
- cluster:
    certificate-authority-data: {b('ca.crt')}
    server: https://127.0.0.1:{port}
  name: default
contexts:
- context:
    cluster: default
    user: default
  name: default
current-context: default
kind: Config
preferences: {{}}
users:
- name: default
  user:
    client-certificate-data: {b('client.crt')}
    client-key-data: {b('client.key')}
"""
    open(os.path.join(d, "kubeconfig"), "w").write(cfg)


def node(name, ready=True):
    return {
        "metadata": {"name": name, "creationTimestamp": iso(NOW - timedelta(days=40)),
                     "labels": {"node-role.kubernetes.io/control-plane": "true", "node-role.kubernetes.io/master": "true"}},
        "spec": {"unschedulable": name in STATE["cordoned"]} if name in STATE["cordoned"] else {},
        "status": {
            "conditions": [{"type": "MemoryPressure", "status": "False"}, {"type": "Ready", "status": "True" if ready else "False"}],
            "addresses": [{"type": "InternalIP", "address": "10.0.0.5"}],
            "allocatable": {"cpu": "8", "memory": "32780416Ki", "pods": "110"},
            "nodeInfo": {"kubeletVersion": "v1.31.4+k3s1", "osImage": "Ubuntu 24.04.4 LTS", "kernelVersion": "6.8.0-147-generic",
                         "containerRuntimeVersion": "containerd://1.7.23-k3s2", "architecture": "amd64"},
        },
    }


def pod(ns, name, owner, state="running", restarts=0, containers=("app",)):
    statuses = []
    for c in containers:
        if state == "running":
            st = {"running": {"startedAt": iso(NOW - timedelta(hours=3))}}
            ready = True
        elif state == "crash":
            st = {"waiting": {"reason": "CrashLoopBackOff", "message": "back-off 5m0s restarting failed container"}}
            ready = False
        else:
            st = {"waiting": {"reason": "ImagePullBackOff"}}
            ready = False
        s = {"name": c, "ready": ready, "restartCount": restarts, "state": st, "image": f"registry/{name}:1"}
        if restarts:
            s["lastState"] = {"terminated": {"reason": "Error", "exitCode": 1, "finishedAt": iso(NOW - timedelta(minutes=4))}}
        statuses.append(s)
    return {
        "metadata": {"namespace": ns, "name": name, "creationTimestamp": iso(NOW - timedelta(hours=5)),
                     "labels": {"app": name.rsplit("-", 2)[0]},
                     "ownerReferences": [{"kind": "ReplicaSet", "name": owner}] if owner else []},
        "spec": {"nodeName": "k3s-node-1", "restartPolicy": "Always", "containers": [{"name": c, "image": f"registry/{name}:1",
                 "resources": {"requests": {"cpu": "100m", "memory": "128Mi"}}} for c in containers]},
        "status": {"phase": "Running", "podIP": "10.42.0.12", "qosClass": "Burstable", "containerStatuses": statuses,
                   "conditions": [{"type": "Ready", "status": "True" if state == "running" else "False", "lastTransitionTime": iso(NOW)}]},
    }


def all_pods():
    pods = [
        pod("default", "web-7d4b9c6f5-abcde", "web-7d4b9c6f5", containers=("web", "sidecar")),
        pod("default", "api-5f6d7c8b9-xyz12", "api-5f6d7c8b9", state="crash", restarts=7),
        pod("kube-system", "coredns-ccb96694c-q8x2m", "coredns-ccb96694c"),
        pod("kube-system", "metrics-server-5985cbc9d7-ab12c", "metrics-server-5985cbc9d7"),
    ]
    return [p for p in pods if (p["metadata"]["namespace"], p["metadata"]["name"]) not in STATE["deleted"]]


def deployment(ns, name, replicas, ready):
    return {"metadata": {"namespace": ns, "name": name, "creationTimestamp": iso(NOW - timedelta(days=3))},
            "spec": {"replicas": replicas, "selector": {"matchLabels": {"app": name}},
                     "template": {"spec": {"containers": [{"name": "app", "image": f"registry/{name}:1"}]}}},
            "status": {"readyReplicas": ready, "availableReplicas": ready, "updatedReplicas": replicas}}


def ns_filter(items, request):
    ns = request.match_info.get("ns")
    return [i for i in items if not ns or i["metadata"].get("namespace") == ns]


def lst(items):
    return web.json_response({"kind": "List", "apiVersion": "v1", "metadata": {}, "items": items})


async def version(_):
    return web.json_response({"major": "1", "minor": "31", "gitVersion": "v1.31.4+k3s1", "platform": "linux/amd64"})


async def nodes(_):
    return lst([node("k3s-node-1")])


async def node_patch(request):
    body = await request.json()
    name = request.match_info["name"]
    if body.get("spec", {}).get("unschedulable"):
        STATE["cordoned"].add(name)
    else:
        STATE["cordoned"].discard(name)
    STATE["patches"].append(("node", name, request.content_type, body))
    return web.json_response(node(name))


async def namespaces(_):
    return lst([{"metadata": {"name": n}} for n in ("default", "kube-system")])


async def pods(request):
    return lst(ns_filter(all_pods(), request))


async def pod_get(request):
    ns, name = request.match_info["ns"], request.match_info["name"]
    for p in all_pods():
        if p["metadata"]["namespace"] == ns and p["metadata"]["name"] == name:
            p = json.loads(json.dumps(p))
            p["metadata"]["managedFields"] = [{"manager": "k3s"}]
            return web.json_response(p)
    return web.json_response({"kind": "Status", "status": "Failure", "message": f'pods "{name}" not found', "code": 404}, status=404)


async def pod_delete(request):
    ns, name = request.match_info["ns"], request.match_info["name"]
    STATE["deleted"].append((ns, name))
    return web.json_response({"kind": "Pod", "metadata": {"name": name}})


async def deployments(request):
    return lst(ns_filter([deployment("default", "web", 1, 1), deployment("default", "api", 1, 0),
                          deployment("kube-system", "coredns", 1, 1), deployment("kube-system", "metrics-server", 1, 1)], request))


async def empty(_):
    return lst([])


async def workload_patch(request):
    body = await request.json()
    STATE["patches"].append((request.match_info["res"], request.match_info["name"], request.content_type, body))
    return web.json_response({"metadata": {"name": request.match_info["name"]}})


async def services(request):
    return lst(ns_filter([{"metadata": {"namespace": "default", "name": "web", "creationTimestamp": iso(NOW)},
                           "spec": {"type": "NodePort", "clusterIP": "10.43.12.7", "ports": [{"port": 80, "nodePort": 30080, "protocol": "TCP"}]},
                           "status": {}}], request))


async def events(request):
    ev = [{"metadata": {"namespace": "default", "creationTimestamp": iso(NOW)}, "type": "Warning", "reason": "BackOff",
           "message": "Back-off restarting failed container app in pod api-5f6d7c8b9-xyz12",
           "involvedObject": {"kind": "Pod", "name": "api-5f6d7c8b9-xyz12"}, "count": 42, "lastTimestamp": iso(NOW - timedelta(seconds=30))},
          {"metadata": {"namespace": "default", "creationTimestamp": iso(NOW)}, "type": "Normal", "reason": "Pulled",
           "message": "Container image already present", "involvedObject": {"kind": "Pod", "name": "web-7d4b9c6f5-abcde"}, "count": 1,
           "lastTimestamp": iso(NOW - timedelta(minutes=10))}]
    sel = request.query.get("fieldSelector", "")
    if sel.startswith("involvedObject.name="):
        ev = [e for e in ev if e["involvedObject"]["name"] == sel.split("=", 1)[1]]
    return lst(ns_filter(ev, request))


async def node_metrics(_):
    return lst([{"metadata": {"name": "k3s-node-1"}, "usage": {"cpu": "1234567890n", "memory": "10485760Ki"}}])


async def pod_metrics(request):
    return lst(ns_filter([{"metadata": {"namespace": p["metadata"]["namespace"], "name": p["metadata"]["name"]},
                           "containers": [{"name": "app", "usage": {"cpu": "15000000n", "memory": "65536Ki"}}]} for p in all_pods()], request))


async def logs(request):
    name = request.match_info["name"]
    follow = request.query.get("follow") == "true"
    tail = int(request.query.get("tailLines", "100"))
    container = request.query.get("container", "")
    prev = request.query.get("previous") == "true"
    resp = web.StreamResponse(headers={"content-type": "text/plain"})
    await resp.prepare(request)
    for i in range(min(tail, 5)):
        t = iso(NOW - timedelta(seconds=10 - i)).replace("Z", ".123456789Z")
        tag = "previous " if prev else ""
        await resp.write(f"{t} {tag}{name}/{container or 'default'} line {i} ERROR sample\n".encode())
    if follow:
        for i in range(3):
            await asyncio.sleep(0.3)
            await resp.write(f"{iso(datetime.now(timezone.utc))} live line {i}\n".encode())
        await asyncio.sleep(30)
    return resp


async def exec_ws(request):
    ws = web.WebSocketResponse(protocols=("v4.channel.k8s.io",))
    await ws.prepare(request)
    cmd = request.query.getall("command", [])
    container = request.query.get("container", "")
    await ws.send_bytes(b"\x01" + f"mock shell in {request.match_info['name']}/{container} cmd={len(cmd)} args\r\n$ ".encode())
    async for msg in ws:
        if msg.type != WSMsgType.BINARY:
            continue
        ch, data = msg.data[0], msg.data[1:]
        if ch == 0:
            STATE["exec_input"] += data
            await ws.send_bytes(b"\x01" + data)  # echo
            if b"exit" in STATE["exec_input"]:
                await ws.send_bytes(b"\x03" + json.dumps({"metadata": {}, "status": "Success"}).encode())
                await ws.close()
                break
        elif ch == 4:
            size = json.loads(data)
            await ws.send_bytes(b"\x01" + f"[resize {size['Width']}x{size['Height']}]".encode())
    return ws


async def state(_):
    return web.json_response({"deleted": STATE["deleted"], "patches": STATE["patches"], "cordoned": sorted(STATE["cordoned"]),
                              "exec_input": STATE["exec_input"].decode(errors="replace")})


def app():
    a = web.Application()
    r = a.router
    r.add_get("/version", version)
    r.add_get("/api/v1/nodes", nodes)
    r.add_patch("/api/v1/nodes/{name}", node_patch)
    r.add_get("/api/v1/namespaces", namespaces)
    r.add_get("/api/v1/pods", pods)
    r.add_get("/api/v1/namespaces/{ns}/pods", pods)
    r.add_get("/api/v1/namespaces/{ns}/pods/{name}", pod_get)
    r.add_delete("/api/v1/namespaces/{ns}/pods/{name}", pod_delete)
    r.add_get("/api/v1/namespaces/{ns}/pods/{name}/log", logs)
    r.add_get("/api/v1/namespaces/{ns}/pods/{name}/exec", exec_ws)
    r.add_get("/api/v1/services", services)
    r.add_get("/api/v1/namespaces/{ns}/services", services)
    r.add_get("/api/v1/events", events)
    r.add_get("/api/v1/namespaces/{ns}/events", events)
    r.add_get("/apis/apps/v1/deployments", deployments)
    r.add_get("/apis/apps/v1/namespaces/{ns}/deployments", deployments)
    for res in ("statefulsets", "daemonsets"):
        r.add_get(f"/apis/apps/v1/{res}", empty)
        r.add_get(f"/apis/apps/v1/namespaces/{{ns}}/{res}", empty)
    r.add_patch("/apis/apps/v1/namespaces/{ns}/{res}/{name}", workload_patch)
    r.add_patch("/apis/apps/v1/namespaces/{ns}/{res}/{name}/scale", workload_patch)
    r.add_get("/apis/metrics.k8s.io/v1beta1/nodes", node_metrics)
    r.add_get("/apis/metrics.k8s.io/v1beta1/pods", pod_metrics)
    r.add_get("/apis/metrics.k8s.io/v1beta1/namespaces/{ns}/pods", pod_metrics)
    r.add_get("/_mock/state", state)
    return a


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dir", default="/tmp/mockk8s")
    ap.add_argument("--port", type=int, default=16443)
    ap.add_argument("--host", default="127.0.0.1")
    a = ap.parse_args()
    make_pki(a.dir)
    write_kubeconfig(a.dir, a.port)
    ctx = ssl.create_default_context(ssl.Purpose.CLIENT_AUTH)
    ctx.load_cert_chain(os.path.join(a.dir, "server.crt"), os.path.join(a.dir, "server.key"))
    ctx.load_verify_locations(os.path.join(a.dir, "ca.crt"))
    ctx.verify_mode = ssl.CERT_REQUIRED  # like the API server: client certificate is the credential
    print(f"mock kubernetes API on https://{a.host}:{a.port} — kubeconfig {a.dir}/kubeconfig", flush=True)
    web.run_app(app(), host=a.host, port=a.port, ssl_context=ctx, print=None)


if __name__ == "__main__":
    main()
