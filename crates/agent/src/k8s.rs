//! Kubernetes (k3s / k8s) integration: talks to the API server with the credentials of a
//! kubeconfig (or the pod's service account) — nodes, workloads, pods, services, events,
//! pod logs and exec, plus rollout restart / scale / delete pod / cordon.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use monitorlab_common::{AgentMsg, K8sKind, K8sSummary, LogLine, StreamData};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use serde_json::{json, Map, Value};
use tokio::sync::mpsc;

use crate::config::K8sConfig;
use crate::session::StreamCtl;
use crate::util::{b64, now_ms};

const K3S_KUBECONFIG: &str = "/etc/rancher/k3s/k3s.yaml";
const SA_DIR: &str = "/var/run/secrets/kubernetes.io/serviceaccount";
/// Name used for TLS when a loopback API address has to be reached from a container
/// (k3s and kubeadm serving certificates always include it).
const LOOPBACK_ALIAS: &str = "kubernetes";

pub struct Kube {
    http: reqwest::Client,
    /// Base URL, e.g. `https://127.0.0.1:6443`.
    base: String,
    token: Option<String>,
    tls: Arc<rustls::ClientConfig>,
    /// Host name used in URLs / TLS, and the address to dial when it was rewritten.
    host: String,
    port: u16,
    dial: Option<SocketAddr>,
    pub source: String,
    summary: Mutex<Option<(Instant, Option<K8sSummary>)>>,
}

struct Creds {
    server: String,
    ca: Option<Vec<u8>>,
    insecure: bool,
    cert: Option<Vec<u8>>,
    key: Option<Vec<u8>>,
    token: Option<String>,
    source: String,
}

fn with_root(host_root: Option<&str>, p: &str) -> PathBuf {
    match host_root.filter(|r| !r.is_empty() && *r != "/") {
        Some(r) if p.starts_with('/') => Path::new(r).join(p.trim_start_matches('/')),
        _ => PathBuf::from(p),
    }
}

fn read_file(base_dir: &Path, p: &str, host_root: Option<&str>) -> Result<Vec<u8>> {
    let path = if Path::new(p).is_absolute() { with_root(host_root, p) } else { base_dir.join(p) };
    std::fs::read(&path).with_context(|| format!("reading {}", path.display()))
}

fn b64_field(v: &serde_yaml::Value, key: &str) -> Option<Vec<u8>> {
    v.get(key)?.as_str().and_then(|s| base64::engine::general_purpose::STANDARD.decode(s.trim()).ok())
}

fn named<'a>(list: Option<&'a serde_yaml::Value>, name: &str, inner: &str) -> Option<&'a serde_yaml::Value> {
    list?.as_sequence()?.iter().find(|e| e.get("name").and_then(|n| n.as_str()) == Some(name))?.get(inner)
}

/// Credentials of the current context of a kubeconfig file.
fn from_kubeconfig(path: &Path, host_root: Option<&str>) -> Result<Creds> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let doc: serde_yaml::Value = serde_yaml::from_str(&text).context("parsing kubeconfig")?;
    let dir = path.parent().unwrap_or(Path::new("."));
    let ctx_name = doc.get("current-context").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let ctx = named(doc.get("contexts"), &ctx_name, "context")
        .or_else(|| doc.get("contexts")?.as_sequence()?.first()?.get("context"))
        .ok_or_else(|| anyhow!("kubeconfig has no context"))?;
    let cluster_name = ctx.get("cluster").and_then(|v| v.as_str()).unwrap_or_default();
    let user_name = ctx.get("user").and_then(|v| v.as_str()).unwrap_or_default();
    let cluster = named(doc.get("clusters"), cluster_name, "cluster").ok_or_else(|| anyhow!("cluster {cluster_name} not found in kubeconfig"))?;
    let user = named(doc.get("users"), user_name, "user").cloned().unwrap_or(serde_yaml::Value::Null);

    let server = cluster.get("server").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("cluster has no server"))?.to_string();
    let ca = match b64_field(cluster, "certificate-authority-data") {
        Some(d) => Some(d),
        None => match cluster.get("certificate-authority").and_then(|v| v.as_str()) {
            Some(p) => Some(read_file(dir, p, host_root)?),
            None => None,
        },
    };
    let insecure = cluster.get("insecure-skip-tls-verify").and_then(|v| v.as_bool()).unwrap_or(false);
    let cert = match b64_field(&user, "client-certificate-data") {
        Some(d) => Some(d),
        None => match user.get("client-certificate").and_then(|v| v.as_str()) {
            Some(p) => Some(read_file(dir, p, host_root)?),
            None => None,
        },
    };
    let key = match b64_field(&user, "client-key-data") {
        Some(d) => Some(d),
        None => match user.get("client-key").and_then(|v| v.as_str()) {
            Some(p) => Some(read_file(dir, p, host_root)?),
            None => None,
        },
    };
    let token = match user.get("token").and_then(|v| v.as_str()) {
        Some(t) => Some(t.trim().to_string()),
        None => match user.get("tokenFile").and_then(|v| v.as_str()) {
            Some(p) => Some(String::from_utf8_lossy(&read_file(dir, p, host_root)?).trim().to_string()),
            None => None,
        },
    };
    if user.get("exec").is_some() && cert.is_none() && token.is_none() {
        bail!("kubeconfig user '{user_name}' uses an exec credential plugin, which the agent cannot run; use a service-account token or client certificate");
    }
    Ok(Creds { server, ca, insecure, cert, key, token, source: path.display().to_string() })
}

fn in_cluster() -> Option<Creds> {
    let host = std::env::var("KUBERNETES_SERVICE_HOST").ok()?;
    let port = std::env::var("KUBERNETES_SERVICE_PORT").unwrap_or_else(|_| "443".into());
    let token = std::fs::read_to_string(format!("{SA_DIR}/token")).ok()?;
    let ca = std::fs::read(format!("{SA_DIR}/ca.crt")).ok();
    let host = if host.contains(':') { format!("[{host}]") } else { host };
    Some(Creds {
        server: format!("https://{host}:{port}"),
        ca,
        insecure: false,
        cert: None,
        key: None,
        token: Some(token.trim().to_string()),
        source: "in-cluster service account".into(),
    })
}

/// Default IPv4 gateway of this network namespace (the Docker host, when in a bridge network).
fn default_gateway() -> Option<std::net::Ipv4Addr> {
    let text = std::fs::read_to_string("/proc/net/route").ok()?;
    for l in text.lines().skip(1) {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() > 2 && f[1] == "00000000" {
            let g = u32::from_str_radix(f[2], 16).ok()?;
            return Some(std::net::Ipv4Addr::from(g.to_le_bytes()));
        }
    }
    None
}

#[derive(Debug)]
struct NoVerify(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(m, c, d, &self.0.signature_verification_algorithms)
    }
    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(m, c, d, &self.0.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

fn tls_config(c: &Creds) -> Result<rustls::ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider.clone()).with_safe_default_protocol_versions()?;
    let builder = if c.insecure {
        builder.dangerous().with_custom_certificate_verifier(Arc::new(NoVerify(provider)))
    } else {
        let mut roots = rustls::RootCertStore::empty();
        match &c.ca {
            Some(pem) => {
                for cert in CertificateDer::pem_slice_iter(pem) {
                    roots.add(cert.context("certificate authority")?)?;
                }
            }
            None => roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned()),
        }
        builder.with_root_certificates(roots)
    };
    Ok(match (&c.cert, &c.key) {
        (Some(cert), Some(key)) => {
            let chain: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(cert).collect::<std::result::Result<_, _>>().context("client certificate")?;
            let key = PrivateKeyDer::from_pem_slice(key).context("client key")?;
            builder.with_client_auth_cert(chain, key)?
        }
        _ => builder.with_no_client_auth(),
    })
}

impl Kube {
    /// Find credentials: explicit config → `KUBECONFIG` → k3s → `~/.kube/config` → in-cluster.
    pub fn detect(cfg: &K8sConfig, host_root: Option<&str>) -> Option<Kube> {
        if cfg.enabled == Some(false) {
            return None;
        }
        let mut tried: Vec<PathBuf> = Vec::new();
        if let Some(p) = &cfg.kubeconfig {
            tried.push(with_root(host_root, p));
        }
        if let Ok(p) = std::env::var("KUBECONFIG") {
            tried.extend(p.split(':').filter(|s| !s.is_empty()).map(PathBuf::from));
        }
        tried.push(with_root(host_root, K3S_KUBECONFIG));
        tried.push(with_root(host_root, "/root/.kube/config"));
        if let Ok(home) = std::env::var("HOME") {
            tried.push(Path::new(&home).join(".kube/config"));
        }
        let mut creds = None;
        for p in tried {
            if !p.exists() {
                continue;
            }
            match from_kubeconfig(&p, host_root) {
                Ok(c) => {
                    creds = Some(c);
                    break;
                }
                Err(e) => tracing::warn!("kubernetes: {} not usable: {e:#}", p.display()),
            }
        }
        let creds = creds.or_else(in_cluster)?;
        match Self::build(creds, host_root) {
            Ok(k) => Some(k),
            Err(e) => {
                tracing::warn!("kubernetes integration disabled: {e:#}");
                None
            }
        }
    }

    fn build(c: Creds, host_root: Option<&str>) -> Result<Kube> {
        let url = reqwest::Url::parse(&c.server).context("kubeconfig server URL")?;
        let mut host = url.host_str().ok_or_else(|| anyhow!("server URL has no host"))?.trim_matches(['[', ']']).to_string();
        let port = url.port_or_known_default().unwrap_or(6443);
        let tls = tls_config(&c)?;
        let mut builder = reqwest::Client::builder()
            .use_preconfigured_tls(tls.clone())
            // The API server is reached directly, like the Docker socket; ignore HTTPS_PROXY.
            .no_proxy()
            .connect_timeout(Duration::from_secs(5))
            .pool_idle_timeout(Duration::from_secs(60));
        // A loopback API server seen from a container on its own network: dial the host
        // (the default gateway) and verify the certificate for the name "kubernetes".
        let mut dial = None;
        let loopback = matches!(host.as_str(), "127.0.0.1" | "localhost" | "::1");
        if loopback && crate::hostnet::detect(host_root).is_some() {
            if let Some(gw) = default_gateway() {
                let addr = SocketAddr::from((gw, port));
                builder = builder.resolve(LOOPBACK_ALIAS, addr);
                host = LOOPBACK_ALIAS.to_string();
                dial = Some(addr);
                tracing::info!("kubernetes: API server on the host's loopback, reaching it via {addr}");
            }
        }
        let shown = if host.contains(':') { format!("[{host}]") } else { host.clone() };
        Ok(Kube {
            http: builder.build()?,
            base: format!("https://{shown}:{port}"),
            token: c.token,
            tls: Arc::new(tls),
            host,
            port,
            dial,
            source: c.source,
            summary: Mutex::new(None),
        })
    }

    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let mut r = self.http.request(method, format!("{}{path}", self.base)).header("accept", "application/json");
        if let Some(t) = &self.token {
            r = r.bearer_auth(t);
        }
        r
    }

    async fn send(&self, r: reqwest::RequestBuilder) -> Result<Value> {
        let res = r.timeout(Duration::from_secs(20)).send().await.map_err(|e| anyhow!("Kubernetes API unreachable: {}", err_chain(&e)))?;
        let status = res.status();
        let body: Value = res.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            let msg = body.get("message").and_then(Value::as_str).unwrap_or(status.canonical_reason().unwrap_or("error"));
            bail!("Kubernetes API: {msg} ({})", status.as_u16());
        }
        Ok(body)
    }

    async fn get(&self, path: &str) -> Result<Value> {
        self.send(self.req(reqwest::Method::GET, path)).await
    }

    async fn items(&self, path: &str) -> Result<Vec<Value>> {
        Ok(self.get(path).await?.get("items").and_then(Value::as_array).cloned().unwrap_or_default())
    }

    /// `/api/v1/pods` or `/api/v1/namespaces/{ns}/pods` style list path.
    fn list_path(group: &str, resource: &str, ns: Option<&str>) -> String {
        match ns.filter(|n| !n.is_empty()) {
            Some(n) => format!("{group}/namespaces/{}/{resource}", enc(n)),
            None => format!("{group}/{resource}"),
        }
    }

    pub async fn version(&self) -> Result<String> {
        let v = self.get("/version").await?;
        Ok(v.get("gitVersion").and_then(Value::as_str).unwrap_or("unknown").to_string())
    }

    async fn pod_metrics(&self, ns: Option<&str>) -> Option<std::collections::HashMap<String, (f64, f64)>> {
        let items = self.items(&Self::list_path("/apis/metrics.k8s.io/v1beta1", "pods", ns)).await.ok()?;
        let mut m = std::collections::HashMap::new();
        for p in items {
            let key = format!("{}/{}", s(&p, &["metadata", "namespace"]), s(&p, &["metadata", "name"]));
            let (mut cpu, mut mem) = (0.0, 0.0);
            for c in p.get("containers").and_then(Value::as_array).into_iter().flatten() {
                cpu += cpu_millis(s(c, &["usage", "cpu"]));
                mem += bytes_of(s(c, &["usage", "memory"]));
            }
            m.insert(key, (cpu, mem));
        }
        Some(m)
    }

    /// Per-container usage of one pod (`[{name, cpu, mem}]`).
    async fn container_metrics(&self, ns: &str, name: &str) -> Option<Vec<Value>> {
        let m = self.get(&format!("/apis/metrics.k8s.io/v1beta1/namespaces/{}/pods/{}", enc(ns), enc(name))).await.ok()?;
        Some(
            arr(&m, &["containers"])
                .iter()
                .map(|c| json!({ "name": s(c, &["name"]), "cpu": cpu_millis(s(c, &["usage", "cpu"])), "mem": bytes_of(s(c, &["usage", "memory"])) }))
                .collect(),
        )
    }

    /// Usage of every pod with its owning workload, for the stored history.
    pub async fn pod_usage(&self) -> Option<Vec<monitorlab_common::PodUsage>> {
        let fut = async {
            let (pods, metrics) = tokio::join!(self.items("/api/v1/pods"), self.items("/apis/metrics.k8s.io/v1beta1/pods"));
            let owners: std::collections::HashMap<String, String> = pods
                .ok()?
                .iter()
                .map(|p| (format!("{}/{}", s(p, &["metadata", "namespace"]), s(p, &["metadata", "name"])), workload_of(p)))
                .collect();
            Some(
                metrics
                    .ok()?
                    .iter()
                    .map(|m| {
                        let ns = s(m, &["metadata", "namespace"]).to_string();
                        let name = s(m, &["metadata", "name"]).to_string();
                        let (mut cpu, mut mem) = (0.0, 0.0);
                        for c in arr(m, &["containers"]) {
                            cpu += cpu_millis(s(c, &["usage", "cpu"]));
                            mem += bytes_of(s(c, &["usage", "memory"]));
                        }
                        let owner = owners.get(&format!("{ns}/{name}")).cloned().unwrap_or_default();
                        monitorlab_common::PodUsage { ns, name, owner, cpu, mem: mem as u64 }
                    })
                    .collect(),
            )
        };
        tokio::time::timeout(Duration::from_secs(10), fut).await.ok().flatten()
    }

    pub async fn namespace_usage(&self) -> Result<Value> {
        let (nss, pods, metrics) = tokio::join!(self.items("/api/v1/namespaces"), self.items("/api/v1/pods"), self.pod_metrics(None));
        let now = now_ms();
        let mut rows: std::collections::BTreeMap<String, Value> = nss?
            .iter()
            .map(|n| {
                let name = s(n, &["metadata", "name"]).to_string();
                (
                    name.clone(),
                    json!({ "name": name, "phase": s(n, &["status", "phase"]), "created": ts_ms(s(n, &["metadata", "creationTimestamp"])),
                            "pods": 0, "running": 0, "failing": 0, "restarts": 0, "cpu": 0.0, "mem": 0.0,
                            "cpu_req": 0.0, "cpu_lim": 0.0, "mem_req": 0.0, "mem_lim": 0.0 }),
                )
            })
            .collect();
        for p in pods? {
            let ns = s(&p, &["metadata", "namespace"]).to_string();
            let Some(r) = rows.get_mut(&ns) else { continue };
            let add = |r: &mut Value, k: &str, v: f64| r[k] = json!(r[k].as_f64().unwrap_or(0.0) + v);
            add(r, "pods", 1.0);
            if s(&p, &["status", "phase"]) == "Running" {
                add(r, "running", 1.0);
            }
            if pod_status(&p, now).1 {
                add(r, "failing", 1.0);
            }
            let restarts: u64 = arr(&p, &["status", "containerStatuses"]).iter().map(|c| c.get("restartCount").and_then(Value::as_u64).unwrap_or(0)).sum();
            add(r, "restarts", restarts as f64);
            if s(&p, &["status", "phase"]) == "Running" || s(&p, &["status", "phase"]) == "Pending" {
                add(r, "cpu_req", res_sum(&p, "requests", "cpu"));
                add(r, "cpu_lim", res_sum(&p, "limits", "cpu"));
                add(r, "mem_req", res_sum(&p, "requests", "memory"));
                add(r, "mem_lim", res_sum(&p, "limits", "memory"));
            }
            if let Some((c, m)) = metrics.as_ref().and_then(|m| m.get(&format!("{ns}/{}", s(&p, &["metadata", "name"])))) {
                add(r, "cpu", *c);
                add(r, "mem", *m);
            }
        }
        Ok(json!({ "namespaces": rows.into_values().collect::<Vec<_>>(), "metrics_available": metrics.is_some() }))
    }

    pub async fn node_detail(&self, name: &str) -> Result<Value> {
        let field = format!("spec.nodeName={name}");
        let (p1, p2, p3) = (
            format!("/api/v1/nodes/{}", enc(name)),
            format!("/api/v1/pods?fieldSelector={}", enc(&field)),
            format!("/apis/metrics.k8s.io/v1beta1/nodes/{}", enc(name)),
        );
        let (node, pods, nm, pm) = tokio::join!(self.get(&p1), self.items(&p2), self.get(&p3), self.pod_metrics(None));
        let n = node?;
        let pods = pods?;
        let now = now_ms();
        let mut alloc = (0.0, 0.0, 0.0, 0.0);
        let rows: Vec<Value> = pods
            .iter()
            .map(|p| {
                let active = matches!(s(p, &["status", "phase"]), "Running" | "Pending");
                let (cr, cl, mr, ml) = (res_sum(p, "requests", "cpu"), res_sum(p, "limits", "cpu"), res_sum(p, "requests", "memory"), res_sum(p, "limits", "memory"));
                if active {
                    alloc.0 += cr;
                    alloc.1 += cl;
                    alloc.2 += mr;
                    alloc.3 += ml;
                }
                let key = format!("{}/{}", s(p, &["metadata", "namespace"]), s(p, &["metadata", "name"]));
                let u = pm.as_ref().and_then(|m| m.get(&key));
                let (status, failing) = pod_status(p, now);
                json!({
                    "namespace": s(p, &["metadata", "namespace"]), "name": s(p, &["metadata", "name"]), "status": status, "failing": failing,
                    "phase": s(p, &["status", "phase"]),
                    "restarts": arr(p, &["status", "containerStatuses"]).iter().map(|c| c.get("restartCount").and_then(Value::as_u64).unwrap_or(0)).sum::<u64>(),
                    "cpu": u.map(|x| x.0), "mem": u.map(|x| x.1), "cpu_req": cr, "cpu_lim": cl, "mem_req": mr, "mem_lim": ml,
                    "created": ts_ms(s(p, &["metadata", "creationTimestamp"])),
                })
            })
            .collect();
        let nm = nm.ok();
        Ok(json!({
            "name": name,
            "ready": node_ready(&n),
            "roles": node_roles(&n),
            "unschedulable": n.pointer("/spec/unschedulable").and_then(Value::as_bool).unwrap_or(false),
            "taints": n.pointer("/spec/taints").cloned().unwrap_or(json!([])),
            "labels": n.pointer("/metadata/labels").cloned().unwrap_or(json!({})),
            "conditions": arr(&n, &["status", "conditions"]).iter().map(|c| json!({ "type": s(c, &["type"]), "status": s(c, &["status"]), "reason": s(c, &["reason"]), "message": s(c, &["message"]), "since": ts_ms(s(c, &["lastTransitionTime"])) })).collect::<Vec<_>>(),
            "addresses": n.pointer("/status/addresses").cloned().unwrap_or(json!([])),
            "info": n.pointer("/status/nodeInfo").cloned().unwrap_or(json!({})),
            "capacity": { "cpu": cpu_millis(s(&n, &["status", "capacity", "cpu"])), "mem": bytes_of(s(&n, &["status", "capacity", "memory"])), "pods": s(&n, &["status", "capacity", "pods"]).parse::<u32>().unwrap_or(0), "storage": bytes_of(s(&n, &["status", "capacity", "ephemeral-storage"])) },
            "allocatable": { "cpu": cpu_millis(s(&n, &["status", "allocatable", "cpu"])), "mem": bytes_of(s(&n, &["status", "allocatable", "memory"])), "pods": s(&n, &["status", "allocatable", "pods"]).parse::<u32>().unwrap_or(0) },
            "allocated": { "cpu_req": alloc.0, "cpu_lim": alloc.1, "mem_req": alloc.2, "mem_lim": alloc.3 },
            "usage": nm.as_ref().map(|m| json!({ "cpu": cpu_millis(s(m, &["usage", "cpu"])), "mem": bytes_of(s(m, &["usage", "memory"])) })),
            "images": arr(&n, &["status", "images"]).len(),
            "created": ts_ms(s(&n, &["metadata", "creationTimestamp"])),
            "pods": rows,
        }))
    }

    pub async fn resources(&self, resource: &str, ns: Option<&str>) -> Result<Value> {
        let now = now_ms();
        let rows: Vec<Value> = match resource {
            "jobs" => self
                .items(&Self::list_path("/apis/batch/v1", "jobs", ns))
                .await?
                .iter()
                .map(|j| {
                    let succeeded = j.pointer("/status/succeeded").and_then(Value::as_u64).unwrap_or(0);
                    let failed = j.pointer("/status/failed").and_then(Value::as_u64).unwrap_or(0);
                    let active = j.pointer("/status/active").and_then(Value::as_u64).unwrap_or(0);
                    let completions = j.pointer("/spec/completions").and_then(Value::as_u64).unwrap_or(1);
                    let cond = arr(j, &["status", "conditions"]).iter().find(|c| s(c, &["status"]) == "True").map(|c| s(c, &["type"]).to_string());
                    let start = ts_ms(s(j, &["status", "startTime"]));
                    let end = ts_ms(s(j, &["status", "completionTime"]));
                    json!({
                        "namespace": s(j, &["metadata", "namespace"]), "name": s(j, &["metadata", "name"]),
                        "status": cond.clone().unwrap_or_else(|| if active > 0 { "Running".into() } else { "Pending".into() }),
                        "failing": cond.as_deref() == Some("Failed"),
                        "completions": format!("{succeeded}/{completions}"), "failed": failed, "active": active,
                        "duration_ms": if start > 0 { Some(if end > 0 { end - start } else { now - start }) } else { None },
                        "owner": arr(j, &["metadata", "ownerReferences"]).first().map(|o| s(o, &["name"]).to_string()),
                        "created": ts_ms(s(j, &["metadata", "creationTimestamp"])),
                    })
                })
                .collect(),
            "cronjobs" => self
                .items(&Self::list_path("/apis/batch/v1", "cronjobs", ns))
                .await?
                .iter()
                .map(|c| {
                    json!({
                        "namespace": s(c, &["metadata", "namespace"]), "name": s(c, &["metadata", "name"]),
                        "schedule": s(c, &["spec", "schedule"]), "timezone": s(c, &["spec", "timeZone"]),
                        "suspended": c.pointer("/spec/suspend").and_then(Value::as_bool).unwrap_or(false),
                        "active": arr(c, &["status", "active"]).len(),
                        "last_schedule": ts_ms(s(c, &["status", "lastScheduleTime"])),
                        "last_success": ts_ms(s(c, &["status", "lastSuccessfulTime"])),
                        "created": ts_ms(s(c, &["metadata", "creationTimestamp"])),
                    })
                })
                .collect(),
            "ingresses" => self
                .items(&Self::list_path("/apis/networking.k8s.io/v1", "ingresses", ns))
                .await?
                .iter()
                .map(|i| {
                    let mut rules = Vec::new();
                    for r in arr(i, &["spec", "rules"]) {
                        let host = s(r, &["host"]);
                        for p in arr(r, &["http", "paths"]) {
                            let svc = s(p, &["backend", "service", "name"]);
                            let port = p.pointer("/backend/service/port/number").map(|v| v.to_string()).unwrap_or_else(|| s(p, &["backend", "service", "port", "name"]).to_string());
                            rules.push(json!({ "host": if host.is_empty() { "*" } else { host }, "path": s(p, &["path"]), "service": format!("{svc}:{port}") }));
                        }
                    }
                    let tls_hosts: Vec<&str> = arr(i, &["spec", "tls"]).iter().flat_map(|t| arr(t, &["hosts"]).iter().filter_map(Value::as_str)).collect();
                    json!({
                        "namespace": s(i, &["metadata", "namespace"]), "name": s(i, &["metadata", "name"]),
                        "class": i.pointer("/spec/ingressClassName").and_then(Value::as_str).unwrap_or(""),
                        "rules": rules, "tls": tls_hosts,
                        "address": arr(i, &["status", "loadBalancer", "ingress"]).iter().map(|a| if s(a, &["ip"]).is_empty() { s(a, &["hostname"]) } else { s(a, &["ip"]) }).collect::<Vec<_>>(),
                        "created": ts_ms(s(i, &["metadata", "creationTimestamp"])),
                    })
                })
                .collect(),
            "pvcs" => self
                .items(&Self::list_path("/api/v1", "persistentvolumeclaims", ns))
                .await?
                .iter()
                .map(|v| {
                    json!({
                        "namespace": s(v, &["metadata", "namespace"]), "name": s(v, &["metadata", "name"]),
                        "status": s(v, &["status", "phase"]), "volume": s(v, &["spec", "volumeName"]),
                        "capacity": bytes_of(s(v, &["status", "capacity", "storage"])), "requested": bytes_of(s(v, &["spec", "resources", "requests", "storage"])),
                        "class": s(v, &["spec", "storageClassName"]), "modes": v.pointer("/spec/accessModes").cloned().unwrap_or(json!([])),
                        "created": ts_ms(s(v, &["metadata", "creationTimestamp"])),
                    })
                })
                .collect(),
            "configmaps" | "secrets" => self
                .items(&Self::list_path("/api/v1", resource, ns))
                .await?
                .iter()
                .map(|v| {
                    // Only key names: secret (and config) values never leave the server.
                    let mut keys: Vec<String> = v.get("data").and_then(Value::as_object).map(|o| o.keys().cloned().collect()).unwrap_or_default();
                    keys.extend(v.get("binaryData").and_then(Value::as_object).map(|o| o.keys().cloned().collect::<Vec<_>>()).unwrap_or_default());
                    json!({
                        "namespace": s(v, &["metadata", "namespace"]), "name": s(v, &["metadata", "name"]),
                        "type": s(v, &["type"]), "keys": keys,
                        "created": ts_ms(s(v, &["metadata", "creationTimestamp"])),
                    })
                })
                .collect(),
            other => bail!("unknown resource '{other}'"),
        };
        Ok(Value::Array(rows))
    }

    /// Run a CronJob now (what `kubectl create job --from=cronjob/x` does).
    pub async fn trigger_cronjob(&self, ns: &str, name: &str) -> Result<Value> {
        let cj = self.get(&format!("/apis/batch/v1/namespaces/{}/cronjobs/{}", enc(ns), enc(name))).await?;
        let spec = cj.pointer("/spec/jobTemplate/spec").cloned().ok_or_else(|| anyhow!("cronjob has no job template"))?;
        let base: String = name.chars().take(40).collect();
        let job_name = format!("{base}-manual-{}", now_ms() / 1000 % 100_000_000);
        let mut labels = cj.pointer("/spec/jobTemplate/metadata/labels").cloned().unwrap_or(json!({}));
        if !labels.is_object() {
            labels = json!({});
        }
        let body = json!({
            "apiVersion": "batch/v1", "kind": "Job",
            "metadata": {
                "name": job_name, "namespace": ns, "labels": labels,
                "annotations": { "cronjob.kubernetes.io/instantiate": "manual" },
                "ownerReferences": [{ "apiVersion": "batch/v1", "kind": "CronJob", "name": name, "uid": s(&cj, &["metadata", "uid"]), "controller": true }],
            },
            "spec": spec,
        });
        let r = self.req(reqwest::Method::POST, &format!("/apis/batch/v1/namespaces/{}/jobs", enc(ns))).json(&body);
        self.send(r).await?;
        Ok(json!({ "ok": true, "job": job_name }))
    }

    // ------------------------------------------------------------------ read

    pub async fn overview(&self) -> Result<Value> {
        let (version, nodes, pods, deps, sts, dss, svcs, nss, nmetrics) = tokio::join!(
            self.get("/version"),
            self.items("/api/v1/nodes"),
            self.items("/api/v1/pods"),
            self.items("/apis/apps/v1/deployments"),
            self.items("/apis/apps/v1/statefulsets"),
            self.items("/apis/apps/v1/daemonsets"),
            self.items("/api/v1/services"),
            self.items("/api/v1/namespaces"),
            self.items("/apis/metrics.k8s.io/v1beta1/nodes"),
        );
        let version = version?;
        let nodes = nodes?;
        let pods = pods?;
        let usage: std::collections::HashMap<String, (f64, f64)> = nmetrics
            .as_ref()
            .map(|v| {
                v.iter()
                    .map(|n| (s(n, &["metadata", "name"]).to_string(), (cpu_millis(s(n, &["usage", "cpu"])), bytes_of(s(n, &["usage", "memory"])))))
                    .collect()
            })
            .unwrap_or_default();
        let pods_per_node = pods.iter().fold(std::collections::HashMap::<String, u32>::new(), |mut m, p| {
            *m.entry(s(p, &["spec", "nodeName"]).to_string()).or_default() += 1;
            m
        });
        let node_rows: Vec<Value> = nodes
            .iter()
            .map(|n| {
                let name = s(n, &["metadata", "name"]).to_string();
                let u = usage.get(&name);
                let problems: Vec<String> = arr(n, &["status", "conditions"])
                    .iter()
                    .filter(|c| s(c, &["type"]) != "Ready" && s(c, &["status"]) == "True")
                    .map(|c| s(c, &["type"]).to_string())
                    .collect();
                json!({
                    "name": name,
                    "ready": node_ready(n),
                    "roles": node_roles(n),
                    "unschedulable": n.pointer("/spec/unschedulable").and_then(Value::as_bool).unwrap_or(false),
                    "version": s(n, &["status", "nodeInfo", "kubeletVersion"]),
                    "os": s(n, &["status", "nodeInfo", "osImage"]),
                    "kernel": s(n, &["status", "nodeInfo", "kernelVersion"]),
                    "runtime": s(n, &["status", "nodeInfo", "containerRuntimeVersion"]),
                    "arch": s(n, &["status", "nodeInfo", "architecture"]),
                    "ip": arr(n, &["status", "addresses"]).iter().find(|a| s(a, &["type"]) == "InternalIP").map(|a| s(a, &["address"])).unwrap_or(""),
                    "cpu_capacity": cpu_millis(s(n, &["status", "allocatable", "cpu"])),
                    "mem_capacity": bytes_of(s(n, &["status", "allocatable", "memory"])),
                    "pods_capacity": s(n, &["status", "allocatable", "pods"]).parse::<u32>().unwrap_or(0),
                    "pods": pods_per_node.get(&name).copied().unwrap_or(0),
                    "cpu_used": u.map(|x| x.0),
                    "mem_used": u.map(|x| x.1),
                    "problems": problems,
                    "created": ts_ms(s(n, &["metadata", "creationTimestamp"])),
                })
            })
            .collect();
        let summary = summarize(&nodes, &pods, deps.as_deref().unwrap_or(&[]), sts.as_deref().unwrap_or(&[]), dss.as_deref().unwrap_or(&[]));
        let git = s(&version, &["gitVersion"]).to_string();
        Ok(json!({
            "version": git,
            "distribution": if git.contains("k3s") { "k3s" } else if git.contains("rke2") { "RKE2" } else if git.contains("eks") { "EKS" } else if git.contains("gke") { "GKE" } else { "Kubernetes" },
            "platform": s(&version, &["platform"]),
            "metrics_available": nmetrics.is_ok(),
            "nodes": node_rows,
            "namespaces": nss.map(|v| v.len()).unwrap_or(0),
            "services": svcs.map(|v| v.len()).unwrap_or(0),
            "summary": summary,
            "source": self.source,
        }))
    }

    pub async fn namespaces(&self) -> Result<Value> {
        let mut names: Vec<String> = self.items("/api/v1/namespaces").await?.iter().map(|n| s(n, &["metadata", "name"]).to_string()).collect();
        names.sort();
        Ok(json!(names))
    }

    pub async fn pods(&self, ns: Option<&str>) -> Result<Value> {
        let path = Self::list_path("/api/v1", "pods", ns);
        let (pods, metrics) = tokio::join!(self.items(&path), self.pod_metrics(ns));
        let now = now_ms();
        let rows: Vec<Value> = pods?
            .iter()
            .map(|p| {
                let namespace = s(p, &["metadata", "namespace"]);
                let name = s(p, &["metadata", "name"]);
                let statuses = arr(p, &["status", "containerStatuses"]);
                let ready = statuses.iter().filter(|c| c.get("ready").and_then(Value::as_bool).unwrap_or(false)).count();
                let restarts: u64 = statuses.iter().map(|c| c.get("restartCount").and_then(Value::as_u64).unwrap_or(0)).sum();
                let last_restart = statuses
                    .iter()
                    .filter_map(|c| c.pointer("/lastState/terminated/finishedAt").and_then(Value::as_str))
                    .map(ts_ms)
                    .max();
                let owner = arr(p, &["metadata", "ownerReferences"]).first().map(|o| json!({ "kind": s(o, &["kind"]), "name": s(o, &["name"]) }));
                let u = metrics.as_ref().and_then(|m| m.get(&format!("{namespace}/{name}")));
                let (status, failing) = pod_status(p, now);
                json!({
                    "namespace": namespace,
                    "name": name,
                    "phase": s(p, &["status", "phase"]),
                    "status": status,
                    "failing": failing,
                    "ready": format!("{ready}/{}", arr(p, &["spec", "containers"]).len()),
                    "restarts": restarts,
                    "last_restart": last_restart,
                    "node": s(p, &["spec", "nodeName"]),
                    "ip": s(p, &["status", "podIP"]),
                    "created": ts_ms(s(p, &["metadata", "creationTimestamp"])),
                    "containers": arr(p, &["spec", "containers"]).iter().map(|c| s(c, &["name"])).collect::<Vec<_>>(),
                    "init_containers": arr(p, &["spec", "initContainers"]).iter().map(|c| s(c, &["name"])).collect::<Vec<_>>(),
                    "images": arr(p, &["spec", "containers"]).iter().map(|c| s(c, &["image"])).collect::<Vec<_>>(),
                    "owner": owner,
                    "cpu": u.map(|x| x.0),
                    "mem": u.map(|x| x.1),
                    "cpu_req": res_sum(p, "requests", "cpu"),
                    "cpu_lim": res_sum(p, "limits", "cpu"),
                    "mem_req": res_sum(p, "requests", "memory"),
                    "mem_lim": res_sum(p, "limits", "memory"),
                    "workload": workload_of(p),
                    "oom_killed": statuses.iter().any(|c| c.pointer("/lastState/terminated/reason").and_then(Value::as_str) == Some("OOMKilled") || c.pointer("/state/terminated/reason").and_then(Value::as_str) == Some("OOMKilled")),
                })
            })
            .collect();
        Ok(json!({ "pods": rows, "metrics_available": metrics.is_some() }))
    }

    pub async fn workloads(&self, ns: Option<&str>) -> Result<Value> {
        let (pd, ps, pa) = (
            Self::list_path("/apis/apps/v1", "deployments", ns),
            Self::list_path("/apis/apps/v1", "statefulsets", ns),
            Self::list_path("/apis/apps/v1", "daemonsets", ns),
        );
        let (d, st, ds) = tokio::join!(self.items(&pd), self.items(&ps), self.items(&pa));
        let mut rows = Vec::new();
        for (kind, list) in [("deployment", d?), ("stateful_set", st?), ("daemon_set", ds?)] {
            for w in list {
                let (desired, ready, available, updated) = replica_counts(kind, &w);
                rows.push(json!({
                    "kind": kind,
                    "namespace": s(&w, &["metadata", "namespace"]),
                    "name": s(&w, &["metadata", "name"]),
                    "desired": desired,
                    "ready": ready,
                    "available": available,
                    "updated": updated,
                    "degraded": desired > 0 && ready < desired,
                    "images": arr(&w, &["spec", "template", "spec", "containers"]).iter().map(|c| s(c, &["image"])).collect::<Vec<_>>(),
                    "created": ts_ms(s(&w, &["metadata", "creationTimestamp"])),
                    "restarted_at": w.pointer("/spec/template/metadata/annotations/kubectl.kubernetes.io~1restartedAt").and_then(Value::as_str).map(ts_ms),
                    "selector": w.pointer("/spec/selector/matchLabels").cloned().unwrap_or(Value::Null),
                }));
            }
        }
        Ok(Value::Array(rows))
    }

    pub async fn services(&self, ns: Option<&str>) -> Result<Value> {
        let rows: Vec<Value> = self
            .items(&Self::list_path("/api/v1", "services", ns))
            .await?
            .iter()
            .map(|v| {
                let ports: Vec<String> = arr(v, &["spec", "ports"])
                    .iter()
                    .map(|p| {
                        let port = p.get("port").and_then(Value::as_u64).unwrap_or(0);
                        let proto = s(p, &["protocol"]);
                        match p.get("nodePort").and_then(Value::as_u64) {
                            Some(np) => format!("{port}:{np}/{proto}"),
                            None => format!("{port}/{proto}"),
                        }
                    })
                    .collect();
                let mut external: Vec<String> = arr(v, &["status", "loadBalancer", "ingress"])
                    .iter()
                    .map(|i| if !s(i, &["ip"]).is_empty() { s(i, &["ip"]) } else { s(i, &["hostname"]) }.to_string())
                    .collect();
                external.extend(arr(v, &["spec", "externalIPs"]).iter().filter_map(|x| x.as_str().map(String::from)));
                json!({
                    "namespace": s(v, &["metadata", "namespace"]),
                    "name": s(v, &["metadata", "name"]),
                    "type": s(v, &["spec", "type"]),
                    "cluster_ip": s(v, &["spec", "clusterIP"]),
                    "external": external,
                    "ports": ports,
                    "selector": v.pointer("/spec/selector").cloned().unwrap_or(Value::Null),
                    "created": ts_ms(s(v, &["metadata", "creationTimestamp"])),
                })
            })
            .collect();
        Ok(Value::Array(rows))
    }

    pub async fn events(&self, ns: Option<&str>) -> Result<Value> {
        let mut rows: Vec<Value> = self
            .items(&Self::list_path("/api/v1", "events", ns))
            .await?
            .iter()
            .map(|e| {
                let last = [
                    s(e, &["lastTimestamp"]),
                    s(e, &["eventTime"]),
                    s(e, &["firstTimestamp"]),
                    s(e, &["metadata", "creationTimestamp"]),
                ]
                .into_iter()
                .find(|t| !t.is_empty())
                .map(ts_ms)
                .unwrap_or(0);
                json!({
                    "namespace": s(e, &["metadata", "namespace"]),
                    "type": s(e, &["type"]),
                    "reason": s(e, &["reason"]),
                    "message": s(e, &["message"]),
                    "object": format!("{}/{}", s(e, &["involvedObject", "kind"]), s(e, &["involvedObject", "name"])),
                    "kind": s(e, &["involvedObject", "kind"]),
                    "name": s(e, &["involvedObject", "name"]),
                    "count": e.get("count").and_then(Value::as_u64).unwrap_or(1),
                    "last": last,
                })
            })
            .collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r["last"].as_i64().unwrap_or(0)));
        rows.truncate(500);
        Ok(Value::Array(rows))
    }

    pub async fn get_object(&self, kind: K8sKind, ns: Option<&str>, name: &str) -> Result<Value> {
        let (group, res, namespaced) = kind_path(kind);
        let path = if namespaced {
            let ns = ns.filter(|n| !n.is_empty()).ok_or_else(|| anyhow!("namespace required"))?;
            format!("{group}/namespaces/{}/{res}/{}", enc(ns), enc(name))
        } else {
            format!("{group}/{res}/{}", enc(name))
        };
        let mut v = self.get(&path).await?;
        if let Some(m) = v.get_mut("metadata").and_then(Value::as_object_mut) {
            m.remove("managedFields");
        }
        let usage = if kind == K8sKind::Pod { self.container_metrics(ns.unwrap_or_default(), name).await } else { None };
        // Events for this object, newest first.
        let field = format!("involvedObject.name={name}");
        let ev_path = match ns.filter(|n| !n.is_empty()) {
            Some(n) => format!("/api/v1/namespaces/{}/events?fieldSelector={}", enc(n), enc(&field)),
            None => format!("/api/v1/events?fieldSelector={}", enc(&field)),
        };
        let mut events: Vec<Value> = self
            .items(&ev_path)
            .await
            .unwrap_or_default()
            .iter()
            .map(|e| {
                json!({
                    "type": s(e, &["type"]),
                    "reason": s(e, &["reason"]),
                    "message": s(e, &["message"]),
                    "count": e.get("count").and_then(Value::as_u64).unwrap_or(1),
                    "last": ts_ms(if s(e, &["lastTimestamp"]).is_empty() { s(e, &["eventTime"]) } else { s(e, &["lastTimestamp"]) }),
                })
            })
            .collect();
        events.sort_by_key(|r| std::cmp::Reverse(r["last"].as_i64().unwrap_or(0)));
        Ok(json!({ "object": v, "events": events, "usage": usage }))
    }

    // ------------------------------------------------------------------ actions

    pub async fn delete_pod(&self, ns: &str, name: &str, force: bool) -> Result<Value> {
        let mut r = self.req(reqwest::Method::DELETE, &format!("/api/v1/namespaces/{}/pods/{}", enc(ns), enc(name)));
        if force {
            r = r.json(&json!({ "kind": "DeleteOptions", "apiVersion": "v1", "gracePeriodSeconds": 0 }));
        }
        self.send(r).await?;
        Ok(json!({ "ok": true }))
    }

    pub async fn rollout_restart(&self, kind: K8sKind, ns: &str, name: &str) -> Result<Value> {
        let (group, res, _) = kind_path(kind);
        if !matches!(kind, K8sKind::Deployment | K8sKind::StatefulSet | K8sKind::DaemonSet) {
            bail!("only deployments, stateful sets and daemon sets can be restarted");
        }
        let now = chrono_like_now();
        let body = json!({ "spec": { "template": { "metadata": { "annotations": { "kubectl.kubernetes.io/restartedAt": now } } } } });
        let r = self
            .req(reqwest::Method::PATCH, &format!("{group}/namespaces/{}/{res}/{}", enc(ns), enc(name)))
            .header("content-type", "application/strategic-merge-patch+json")
            .body(body.to_string());
        self.send(r).await?;
        Ok(json!({ "ok": true }))
    }

    pub async fn scale(&self, kind: K8sKind, ns: &str, name: &str, replicas: u32) -> Result<Value> {
        let (group, res, _) = kind_path(kind);
        if !matches!(kind, K8sKind::Deployment | K8sKind::StatefulSet) {
            bail!("only deployments and stateful sets can be scaled");
        }
        if replicas > 500 {
            bail!("refusing to scale above 500 replicas");
        }
        let r = self
            .req(reqwest::Method::PATCH, &format!("{group}/namespaces/{}/{res}/{}/scale", enc(ns), enc(name)))
            .header("content-type", "application/merge-patch+json")
            .body(json!({ "spec": { "replicas": replicas } }).to_string());
        self.send(r).await?;
        Ok(json!({ "ok": true }))
    }

    pub async fn cordon(&self, name: &str, unschedulable: bool) -> Result<Value> {
        let r = self
            .req(reqwest::Method::PATCH, &format!("/api/v1/nodes/{}", enc(name)))
            .header("content-type", "application/merge-patch+json")
            .body(json!({ "spec": { "unschedulable": unschedulable } }).to_string());
        self.send(r).await?;
        Ok(json!({ "ok": true }))
    }

    // ------------------------------------------------------------------ summary for metrics

    /// Cluster summary for the metrics sample; cached for 30 s, never blocks the sample long.
    pub async fn summary(&self) -> Option<K8sSummary> {
        if let Some((at, s)) = self.summary.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            if at.elapsed() < Duration::from_secs(30) {
                return s;
            }
        }
        let fetch = async {
            let (nodes, pods, d, st, ds) = tokio::join!(
                self.items("/api/v1/nodes"),
                self.items("/api/v1/pods"),
                self.items("/apis/apps/v1/deployments"),
                self.items("/apis/apps/v1/statefulsets"),
                self.items("/apis/apps/v1/daemonsets"),
            );
            Some(summarize(&nodes.ok()?, &pods.ok()?, &d.unwrap_or_default(), &st.unwrap_or_default(), &ds.unwrap_or_default()))
        };
        let s = tokio::time::timeout(Duration::from_secs(8), fetch).await.ok().flatten();
        *self.summary.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), s.clone()));
        s
    }

    // ------------------------------------------------------------------ streams

    #[allow(clippy::too_many_arguments)]
    pub async fn stream_logs(
        &self,
        ns: &str,
        name: &str,
        container: Option<String>,
        tail: u32,
        since: Option<i64>,
        follow: bool,
        previous: bool,
        sid: u64,
        out: &mpsc::Sender<AgentMsg>,
    ) -> Result<()> {
        let mut q = vec![("timestamps", "true".to_string()), ("follow", follow.to_string())];
        if let Some(c) = container.filter(|c| !c.is_empty()) {
            q.push(("container", c));
        }
        match since {
            Some(t) => q.push(("sinceSeconds", ((now_ms() / 1000 - t).max(1)).to_string())),
            None => q.push(("tailLines", tail.clamp(1, 100_000).to_string())),
        }
        if previous {
            q.push(("previous", "true".into()));
        }
        let res = self
            .req(reqwest::Method::GET, &format!("/api/v1/namespaces/{}/pods/{}/log", enc(ns), enc(name)))
            .query(&q)
            .send()
            .await
            .map_err(|e| anyhow!("Kubernetes API unreachable: {}", err_chain(&e)))?;
        if !res.status().is_success() {
            let st = res.status();
            let body: Value = res.json().await.unwrap_or(Value::Null);
            bail!("{}", body.get("message").and_then(Value::as_str).unwrap_or(st.as_str()));
        }
        let mut stream = res.bytes_stream();
        let mut buf = String::new();
        let mut batch: Vec<LogLine> = Vec::new();
        let mut tick = tokio::time::interval(Duration::from_millis(120));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                item = stream.next() => match item {
                    Some(Ok(chunk)) => {
                        buf.push_str(&String::from_utf8_lossy(&chunk));
                        while let Some(i) = buf.find('\n') {
                            let line: String = buf.drain(..=i).collect();
                            batch.push(crate::docker::parse_log_line(line.trim_end_matches('\n'), 1));
                        }
                        if buf.len() > 64 * 1024 {
                            batch.push(crate::docker::parse_log_line(&std::mem::take(&mut buf), 1));
                        }
                        if batch.len() >= 500 {
                            send_lines(out, sid, &mut batch).await?;
                        }
                    }
                    Some(Err(e)) => {
                        send_lines(out, sid, &mut batch).await?;
                        return Err(anyhow!("log stream interrupted: {e}"));
                    }
                    None => {
                        if !buf.is_empty() {
                            batch.push(crate::docker::parse_log_line(&std::mem::take(&mut buf), 1));
                        }
                        send_lines(out, sid, &mut batch).await?;
                        return Ok(());
                    }
                },
                _ = tick.tick() => send_lines(out, sid, &mut batch).await?,
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn exec(
        &self,
        ns: &str,
        name: &str,
        container: Option<String>,
        cmd: Option<Vec<String>>,
        cols: u16,
        rows: u16,
        sid: u64,
        out: &mpsc::Sender<AgentMsg>,
        mut ctl: mpsc::Receiver<StreamCtl>,
    ) -> Result<()> {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        use tokio_tungstenite::tungstenite::Message;

        let cmd = cmd.filter(|c| !c.is_empty()).unwrap_or_else(|| {
            vec![
                "/bin/sh".into(),
                "-c".into(),
                "TERM=xterm-256color; export TERM; if command -v bash >/dev/null 2>&1; then exec bash; else exec sh; fi".into(),
            ]
        });
        let mut q: Vec<(String, String)> = vec![
            ("stdin".into(), "true".into()),
            ("stdout".into(), "true".into()),
            ("stderr".into(), "true".into()),
            ("tty".into(), "true".into()),
        ];
        if let Some(c) = container.filter(|c| !c.is_empty()) {
            q.push(("container".into(), c));
        }
        for c in cmd {
            q.push(("command".into(), c));
        }
        let qs: String = q.iter().map(|(k, v)| format!("{k}={}", enc(v))).collect::<Vec<_>>().join("&");
        let shown = if self.host.contains(':') { format!("[{}]", self.host) } else { self.host.clone() };
        let url = format!("wss://{shown}:{}/api/v1/namespaces/{}/pods/{}/exec?{qs}", self.port, enc(ns), enc(name));
        let mut request = url.into_client_request()?;
        request.headers_mut().insert("Sec-WebSocket-Protocol", "v4.channel.k8s.io".parse()?);
        if let Some(t) = &self.token {
            request.headers_mut().insert("Authorization", format!("Bearer {t}").parse()?);
        }
        let addr = match self.dial {
            Some(a) => a,
            None => tokio::net::lookup_host((self.host.as_str(), self.port)).await?.next().ok_or_else(|| anyhow!("cannot resolve {}", self.host))?,
        };
        let tcp = tokio::time::timeout(Duration::from_secs(8), tokio::net::TcpStream::connect(addr)).await.map_err(|_| anyhow!("timed out connecting to the Kubernetes API"))??;
        let connector = tokio_tungstenite::Connector::Rustls(self.tls.clone());
        let (ws, _) = tokio_tungstenite::client_async_tls_with_config(request, tcp, None, Some(connector))
            .await
            .map_err(|e| anyhow!("exec failed: {e}"))?;
        let (mut tx, mut rx) = ws.split();
        let resize = |c: u16, r: u16| {
            let mut m = vec![4u8];
            m.extend_from_slice(json!({ "Width": c.max(1), "Height": r.max(1) }).to_string().as_bytes());
            Message::Binary(m.into())
        };
        tx.send(resize(cols, rows)).await?;
        loop {
            tokio::select! {
                msg = rx.next() => match msg {
                    Some(Ok(Message::Binary(b))) if !b.is_empty() => match b[0] {
                        1 | 2 => {
                            out.send(AgentMsg::Stream { sid, data: StreamData::Bytes { b64: b64(&b[1..]) } }).await.map_err(|_| anyhow!("closed"))?;
                        }
                        3 => {
                            let st: Value = serde_json::from_slice(&b[1..]).unwrap_or(Value::Null);
                            if st.get("status").and_then(Value::as_str) == Some("Failure") {
                                let msg = st.get("message").and_then(Value::as_str).unwrap_or("command failed");
                                bail!("{msg}");
                            }
                        }
                        _ => {}
                    },
                    Some(Ok(Message::Close(_))) | None => return Ok(()),
                    Some(Ok(_)) => {}
                    Some(Err(e)) => return Err(anyhow!("exec connection lost: {e}")),
                },
                c = ctl.recv() => match c {
                    Some(StreamCtl::Input(data)) => {
                        let mut m = Vec::with_capacity(data.len() + 1);
                        m.push(0u8);
                        m.extend_from_slice(&data);
                        tx.send(Message::Binary(m.into())).await?;
                    }
                    Some(StreamCtl::Resize(c, r)) => tx.send(resize(c, r)).await?,
                    None => {
                        let _ = tx.send(Message::Close(None)).await;
                        return Ok(());
                    }
                },
            }
        }
    }
}

async fn send_lines(out: &mpsc::Sender<AgentMsg>, sid: u64, batch: &mut Vec<LogLine>) -> Result<()> {
    if batch.is_empty() {
        return Ok(());
    }
    let lines = std::mem::take(batch);
    out.send(AgentMsg::Stream { sid, data: StreamData::Lines { lines } }).await.map_err(|_| anyhow!("closed"))
}

// ====================================================================== helpers

fn enc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            o.push(b as char);
        } else {
            o.push_str(&format!("%{b:02X}"));
        }
    }
    o
}

fn s<'a>(v: &'a Value, path: &[&str]) -> &'a str {
    let mut cur = v;
    for p in path {
        match cur.get(*p) {
            Some(x) => cur = x,
            None => return "",
        }
    }
    cur.as_str().unwrap_or("")
}

fn arr<'a>(v: &'a Value, path: &[&str]) -> &'a [Value] {
    let mut cur = v;
    for p in path {
        match cur.get(*p) {
            Some(x) => cur = x,
            None => return &[],
        }
    }
    cur.as_array().map(Vec::as_slice).unwrap_or(&[])
}

fn kind_path(k: K8sKind) -> (&'static str, &'static str, bool) {
    match k {
        K8sKind::Pod => ("/api/v1", "pods", true),
        K8sKind::Service => ("/api/v1", "services", true),
        K8sKind::Node => ("/api/v1", "nodes", false),
        K8sKind::Namespace => ("/api/v1", "namespaces", false),
        K8sKind::Deployment => ("/apis/apps/v1", "deployments", true),
        K8sKind::StatefulSet => ("/apis/apps/v1", "statefulsets", true),
        K8sKind::DaemonSet => ("/apis/apps/v1", "daemonsets", true),
    }
}

/// RFC 3339 → unix ms (0 when unparseable). Kubernetes uses `YYYY-MM-DDTHH:MM:SS[.frac]Z`.
pub fn ts_ms(t: &str) -> i64 {
    let b = t.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[10] != b'T' {
        return 0;
    }
    let n = |r: std::ops::Range<usize>| t.get(r).and_then(|x| x.parse::<i64>().ok());
    let (Some(y), Some(mo), Some(d), Some(h), Some(mi), Some(se)) = (n(0..4), n(5..7), n(8..10), n(11..13), n(14..16), n(17..19)) else {
        return 0;
    };
    // days since epoch (civil from Howard Hinnant)
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * (mo + if mo > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    let mut ms = ((days * 24 + h) * 60 + mi) * 60 * 1000 + se * 1000;
    if b.len() > 20 && b[19] == b'.' {
        let frac: String = t[20..].chars().take_while(char::is_ascii_digit).take(3).collect();
        if let Ok(f) = format!("{frac:0<3}").parse::<i64>() {
            ms += f;
        }
    }
    // numeric offset, e.g. +05:30
    if let Some(pos) = t[19..].find(['+', '-']).map(|p| p + 19) {
        if let (Some(oh), Some(om)) = (n(pos + 1..pos + 3), n(pos + 4..pos + 6)) {
            let off = (oh * 60 + om) * 60_000;
            ms += if b[pos] == b'+' { -off } else { off };
        }
    }
    ms
}

fn chrono_like_now() -> String {
    let ms = now_ms();
    let secs = ms / 1000;
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    // civil from days
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// CPU quantity → millicores ("250m", "2", "1500000n", "12u").
pub fn cpu_millis(q: &str) -> f64 {
    let q = q.trim();
    if q.is_empty() {
        return 0.0;
    }
    let (num, mult) = match q.as_bytes()[q.len() - 1] {
        b'n' => (&q[..q.len() - 1], 1e-6),
        b'u' => (&q[..q.len() - 1], 1e-3),
        b'm' => (&q[..q.len() - 1], 1.0),
        _ => (q, 1000.0),
    };
    num.parse::<f64>().map(|v| v * mult).unwrap_or(0.0)
}

/// Memory quantity → bytes ("128Mi", "1G", "123456Ki", "1e3").
pub fn bytes_of(q: &str) -> f64 {
    let q = q.trim();
    const SUF: [(&str, f64); 12] = [
        ("Ki", 1024.0),
        ("Mi", 1048576.0),
        ("Gi", 1073741824.0),
        ("Ti", 1099511627776.0),
        ("Pi", 1125899906842624.0),
        ("Ei", 1152921504606846976.0),
        ("k", 1e3),
        ("M", 1e6),
        ("G", 1e9),
        ("T", 1e12),
        ("P", 1e15),
        ("E", 1e18),
    ];
    for (s, m) in SUF {
        if let Some(n) = q.strip_suffix(s) {
            return n.parse::<f64>().map(|v| v * m).unwrap_or(0.0);
        }
    }
    q.parse::<f64>().unwrap_or(0.0)
}

/// Sum of container requests / limits of a pod (`cpu` in millicores, `memory` in bytes).
fn res_sum(p: &Value, kind: &str, res: &str) -> f64 {
    arr(p, &["spec", "containers"])
        .iter()
        .map(|c| {
            let q = s(c, &["resources", kind, res]);
            if res == "cpu" {
                cpu_millis(q)
            } else {
                bytes_of(q)
            }
        })
        .sum()
}

/// Name of the workload that owns a pod: ReplicaSet `web-7d4b9c6f5` → Deployment `web`.
pub fn workload_of(p: &Value) -> String {
    let Some(o) = arr(p, &["metadata", "ownerReferences"]).first() else { return String::new() };
    let name = s(o, &["name"]);
    if s(o, &["kind"]) == "ReplicaSet" {
        let hash = s(p, &["metadata", "labels", "pod-template-hash"]);
        if !hash.is_empty() {
            if let Some(n) = name.strip_suffix(&format!("-{hash}")) {
                return n.to_string();
            }
        }
        if let Some((n, _)) = name.rsplit_once('-') {
            return n.to_string();
        }
    }
    name.to_string()
}

fn node_ready(n: &Value) -> bool {
    arr(n, &["status", "conditions"]).iter().any(|c| s(c, &["type"]) == "Ready" && s(c, &["status"]) == "True")
}

fn node_roles(n: &Value) -> Vec<String> {
    let mut r: Vec<String> = n
        .pointer("/metadata/labels")
        .and_then(Value::as_object)
        .map(|m: &Map<String, Value>| m.keys().filter_map(|k| k.strip_prefix("node-role.kubernetes.io/").map(String::from)).collect())
        .unwrap_or_default();
    r.sort();
    if r.is_empty() {
        r.push("worker".into());
    }
    r
}

const BAD_WAITING: &[&str] = &[
    "CrashLoopBackOff",
    "ImagePullBackOff",
    "ErrImagePull",
    "CreateContainerConfigError",
    "CreateContainerError",
    "InvalidImageName",
    "RunContainerError",
    "ErrImageNeverPull",
];

/// kubectl-style status text, and whether the pod counts as failing.
pub fn pod_status(p: &Value, now: i64) -> (String, bool) {
    let phase = s(p, &["status", "phase"]);
    let mut reason = if s(p, &["status", "reason"]).is_empty() { phase.to_string() } else { s(p, &["status", "reason"]).to_string() };
    let mut failing = phase == "Failed";
    for c in arr(p, &["status", "initContainerStatuses"]) {
        let w = s(c, &["state", "waiting", "reason"]);
        if BAD_WAITING.contains(&w) {
            reason = format!("Init:{w}");
            failing = true;
        } else if !w.is_empty() && w != "PodInitializing" {
            reason = format!("Init:{w}");
        }
    }
    let mut any_running_ready = false;
    for c in arr(p, &["status", "containerStatuses"]).iter().rev() {
        let w = s(c, &["state", "waiting", "reason"]);
        let t = s(c, &["state", "terminated", "reason"]);
        if !w.is_empty() {
            reason = w.to_string();
            if BAD_WAITING.contains(&w) {
                failing = true;
            }
        } else if !t.is_empty() {
            reason = t.to_string();
            if t != "Completed" && phase != "Succeeded" {
                failing = true;
            }
        } else if c.pointer("/state/running").is_some() && c.get("ready").and_then(Value::as_bool).unwrap_or(false) {
            any_running_ready = true;
        }
    }
    if phase == "Running" && reason == "Completed" && any_running_ready {
        reason = "Running".into();
    }
    if !s(p, &["metadata", "deletionTimestamp"]).is_empty() {
        reason = "Terminating".into();
    }
    if phase == "Pending" && !failing {
        let created = ts_ms(s(p, &["metadata", "creationTimestamp"]));
        if created > 0 && now - created > 5 * 60_000 {
            failing = true;
        }
    }
    if phase == "Succeeded" {
        failing = false;
    }
    (reason, failing)
}

fn replica_counts(kind: &str, w: &Value) -> (u64, u64, u64, u64) {
    let g = |p: &str| w.pointer(p).and_then(Value::as_u64).unwrap_or(0);
    match kind {
        "daemon_set" => (g("/status/desiredNumberScheduled"), g("/status/numberReady"), g("/status/numberAvailable"), g("/status/updatedNumberScheduled")),
        _ => (
            w.pointer("/spec/replicas").and_then(Value::as_u64).unwrap_or(1),
            g("/status/readyReplicas"),
            g("/status/availableReplicas"),
            g("/status/updatedReplicas"),
        ),
    }
}

pub fn summarize(nodes: &[Value], pods: &[Value], deps: &[Value], sts: &[Value], dss: &[Value]) -> K8sSummary {
    let now = now_ms();
    let mut out = K8sSummary { nodes: nodes.len() as u32, pods: pods.len() as u32, ..Default::default() };
    for n in nodes {
        if node_ready(n) {
            out.nodes_ready += 1;
        } else {
            out.not_ready_nodes.push(s(n, &["metadata", "name"]).to_string());
        }
    }
    for p in pods {
        if s(p, &["status", "phase"]) == "Running" {
            out.pods_running += 1;
        }
        let (reason, failing) = pod_status(p, now);
        if failing {
            out.pods_failing += 1;
            if out.failing.len() < 10 {
                out.failing.push(format!("{}/{}: {reason}", s(p, &["metadata", "namespace"]), s(p, &["metadata", "name"])));
            }
        }
    }
    for (kind, list) in [("deployment", deps), ("stateful_set", sts), ("daemon_set", dss)] {
        for w in list {
            out.workloads += 1;
            let (desired, ready, _, _) = replica_counts(kind, w);
            if desired > 0 && ready < desired {
                out.workloads_degraded += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantities() {
        assert_eq!(cpu_millis("250m"), 250.0);
        assert_eq!(cpu_millis("2"), 2000.0);
        assert_eq!(cpu_millis("1500000n"), 1.5);
        assert_eq!(bytes_of("128Mi"), 134217728.0);
        assert_eq!(bytes_of("32780416Ki"), 32780416.0 * 1024.0);
        assert_eq!(bytes_of("1G"), 1e9);
    }

    #[test]
    fn timestamps() {
        assert_eq!(ts_ms("1970-01-01T00:00:00Z"), 0);
        assert_eq!(ts_ms("2026-11-26T00:00:00Z"), 1795651200000);
        assert_eq!(ts_ms("2026-11-26T05:30:00+05:30"), 1795651200000);
        assert_eq!(ts_ms("2026-11-26T00:00:00.250000Z"), 1795651200250);
        assert_eq!(ts_ms("bad"), 0);
        let now = chrono_like_now();
        assert!((ts_ms(&now) - now_ms()).abs() < 2000, "{now}");
    }

    #[test]
    fn statuses() {
        let crash = json!({ "metadata": { "creationTimestamp": "2026-01-01T00:00:00Z" }, "status": { "phase": "Running", "containerStatuses": [
            { "ready": false, "restartCount": 7, "state": { "waiting": { "reason": "CrashLoopBackOff" } } }
        ] } });
        assert_eq!(pod_status(&crash, now_ms()), ("CrashLoopBackOff".into(), true));
        let ok = json!({ "status": { "phase": "Running", "containerStatuses": [{ "ready": true, "state": { "running": {} } }] } });
        assert_eq!(pod_status(&ok, now_ms()), ("Running".into(), false));
        let done = json!({ "status": { "phase": "Succeeded", "containerStatuses": [{ "ready": false, "state": { "terminated": { "reason": "Completed" } } }] } });
        assert_eq!(pod_status(&done, now_ms()), ("Completed".into(), false));
        let stuck = json!({ "metadata": { "creationTimestamp": "2020-01-01T00:00:00Z" }, "status": { "phase": "Pending" } });
        assert!(pod_status(&stuck, now_ms()).1);
        let oom = json!({ "status": { "phase": "Running", "containerStatuses": [{ "ready": false, "state": { "terminated": { "reason": "OOMKilled" } } }] } });
        assert_eq!(pod_status(&oom, now_ms()), ("OOMKilled".into(), true));
    }

    #[test]
    fn kubeconfig_parsing() {
        let dir = std::env::temp_dir().join(format!("ml-kube-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("config");
        std::fs::write(
            &p,
            "apiVersion: v1\nclusters:\n- cluster:\n    server: https://127.0.0.1:6443\n    insecure-skip-tls-verify: true\n  name: default\ncontexts:\n- context:\n    cluster: default\n    user: default\n  name: default\ncurrent-context: default\nkind: Config\nusers:\n- name: default\n  user:\n    token: abc123\n",
        )
        .unwrap();
        let c = from_kubeconfig(&p, None).unwrap();
        assert_eq!(c.server, "https://127.0.0.1:6443");
        assert!(c.insecure);
        assert_eq!(c.token.as_deref(), Some("abc123"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn summary_counts() {
        let nodes = vec![
            json!({ "metadata": { "name": "a" }, "status": { "conditions": [{ "type": "Ready", "status": "True" }] } }),
            json!({ "metadata": { "name": "b" }, "status": { "conditions": [{ "type": "Ready", "status": "False" }] } }),
        ];
        let pods = vec![
            json!({ "metadata": { "namespace": "x", "name": "ok" }, "status": { "phase": "Running", "containerStatuses": [{ "ready": true, "state": { "running": {} } }] } }),
            json!({ "metadata": { "namespace": "x", "name": "bad" }, "status": { "phase": "Running", "containerStatuses": [{ "ready": false, "state": { "waiting": { "reason": "ImagePullBackOff" } } }] } }),
        ];
        let deps = vec![json!({ "spec": { "replicas": 2 }, "status": { "readyReplicas": 1 } }), json!({ "spec": { "replicas": 0 }, "status": {} })];
        let s = summarize(&nodes, &pods, &deps, &[], &[]);
        assert_eq!((s.nodes, s.nodes_ready, s.pods, s.pods_running, s.pods_failing), (2, 1, 2, 2, 1));
        assert_eq!((s.workloads, s.workloads_degraded), (2, 1));
        assert_eq!(s.not_ready_nodes, vec!["b".to_string()]);
        assert_eq!(s.failing, vec!["x/bad: ImagePullBackOff".to_string()]);
    }

    #[test]
    fn owners_and_resources() {
        let p = json!({ "metadata": { "labels": { "pod-template-hash": "7d4b9c6f5" }, "ownerReferences": [{ "kind": "ReplicaSet", "name": "web-api-7d4b9c6f5" }] },
                        "spec": { "containers": [{ "resources": { "requests": { "cpu": "250m", "memory": "128Mi" }, "limits": { "cpu": "1" } } }, { "resources": { "requests": { "cpu": "50m" } } }] } });
        assert_eq!(workload_of(&p), "web-api");
        assert_eq!(res_sum(&p, "requests", "cpu"), 300.0);
        assert_eq!(res_sum(&p, "limits", "cpu"), 1000.0);
        assert_eq!(res_sum(&p, "requests", "memory"), 134217728.0);
        let sts = json!({ "metadata": { "ownerReferences": [{ "kind": "StatefulSet", "name": "db" }] } });
        assert_eq!(workload_of(&sts), "db");
        assert_eq!(workload_of(&json!({})), "");
    }
}

/// Full error chain, so "error sending request" also shows timeout / refused / TLS.
fn err_chain(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut cur = e.source();
    while let Some(c) = cur {
        out.push_str(": ");
        out.push_str(&c.to_string());
        cur = c.source();
    }
    if out.contains("timed out") || out.contains("deadline") {
        out.push_str(" (timed out: a firewall may be dropping traffic to the API port)");
    }
    out
}
