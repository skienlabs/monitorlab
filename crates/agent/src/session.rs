//! Transport-independent agent core: receives `HubMsg`s, produces `AgentMsg`s.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use monitorlab_common::{AgentHello, AgentMsg, Capabilities, HubMsg, Request, StreamData, StreamRequest};

use crate::config::{AgentConfig, MssqlConfig};
use crate::docker::DockerCtl;
use crate::iis::Iis;
use crate::mssql::Mssql;
use crate::system::Collector;
use crate::util::unb64;

#[derive(Debug, Clone)]
pub struct AgentOptions {
    pub interval_secs: u32,
    pub docker_enabled: bool,
    pub docker_socket: Option<String>,
    pub mssql: MssqlConfig,
    pub iis_enabled: Option<bool>,
    pub host_root: Option<String>,
    pub k8s: crate::config::K8sConfig,
    /// Where updates come from (None for the hub's built-in agent).
    pub update: Option<crate::update::UpdateSource>,
    /// Keep per-container stats for the hub's history.
    pub container_history: bool,
}

impl AgentOptions {
    pub fn from_config(c: &AgentConfig) -> Self {
        Self {
            interval_secs: c.interval_secs.clamp(2, 300),
            docker_enabled: c.docker.enabled,
            docker_socket: c.docker.socket.clone(),
            mssql: c.mssql.clone(),
            iis_enabled: c.iis.enabled,
            host_root: c.host_root.clone(),
            k8s: c.kubernetes.clone(),
            update: Some(crate::update::UpdateSource { hub_url: c.hub_url.clone(), fingerprint: c.fingerprint.clone() }),
            container_history: c.docker.history,
        }
    }
}

#[derive(Debug)]
pub enum StreamCtl {
    Input(Vec<u8>),
    Resize(u16, u16),
}

pub struct Agent {
    opts: AgentOptions,
    docker: Option<DockerCtl>,
    collector: Arc<Mutex<Collector>>,
    mssql: Arc<Mssql>,
    iis: Arc<Iis>,
    kube: Option<Arc<crate::k8s::Kube>>,
    /// Set by `hello` when the Kubernetes API answered.
    kube_ok: std::sync::atomic::AtomicBool,
    services: crate::services::Services,
    tick_no: std::sync::atomic::AtomicU64,
}

struct StreamHandle {
    ctl: mpsc::Sender<StreamCtl>,
    task: JoinHandle<()>,
}

impl Agent {
    pub fn new(opts: AgentOptions) -> Arc<Self> {
        let docker = if opts.docker_enabled {
            match DockerCtl::new(opts.docker_socket.clone()) {
                Ok(d) => Some(d),
                Err(e) => {
                    tracing::info!("docker integration disabled: {e}");
                    None
                }
            }
        } else {
            None
        };
        let collector = Collector::new(opts.host_root.clone(), true);
        let kube = crate::k8s::Kube::detect(&opts.k8s, opts.host_root.as_deref()).map(Arc::new);
        if let Some(k) = &kube {
            tracing::info!("kubernetes credentials found ({})", k.source);
        }
        crate::update::cleanup();
        Arc::new(Self {
            services: crate::services::Services::new(opts.host_root.clone()),
            tick_no: Default::default(),
            kube,
            kube_ok: Default::default(),
            mssql: Arc::new(Mssql::new(opts.mssql.clone(), opts.host_root.clone())),
            iis: Arc::new(Iis::new(opts.iis_enabled)),
            collector: Arc::new(Mutex::new(collector)),
            docker,
            opts,
        })
    }

    pub async fn hello(&self) -> AgentHello {
        let docker_version = match &self.docker {
            Some(d) => d.version().await,
            None => None,
        };
        let kube_version = match &self.kube {
            Some(k) => match tokio::time::timeout(Duration::from_secs(6), k.version()).await {
                Ok(Ok(v)) => Some(v),
                Ok(Err(e)) => {
                    tracing::warn!("kubernetes API not usable: {e:#}");
                    None
                }
                Err(_) => {
                    tracing::warn!("kubernetes API did not answer within 6s");
                    None
                }
            },
            None => None,
        };
        self.kube_ok.store(kube_version.is_some(), std::sync::atomic::Ordering::Relaxed);
        let services_ok = self.services.available().await;
        let logs_ok = self.services.logs_available().await;
        let can_update = self.opts.update.is_some();
        let mssql = self.mssql.clone();
        let iis = self.iis.clone();
        let col = self.collector.clone();
        let interval = self.opts.interval_secs;
        tokio::task::spawn_blocking(move || {
            let caps = Capabilities {
                docker: docker_version.is_some(),
                docker_version,
                mssql: mssql.detected(),
                iis: iis.detected(),
                kubernetes: kube_version.is_some(),
                kubernetes_version: kube_version,
                services: services_ok,
                system_logs: logs_ok,
                self_update: can_update,
            };
            let c = col.lock().unwrap_or_else(|e| e.into_inner());
            c.hello(caps, interval)
        })
        .await
        .expect("hello task panicked")
    }

    /// Serve one hub connection until `inbound` closes.
    pub async fn serve(self: Arc<Self>, mut inbound: mpsc::Receiver<HubMsg>, out: mpsc::Sender<AgentMsg>) {
        let hello = self.hello().await;
        if out.send(AgentMsg::Hello(hello)).await.is_err() {
            return;
        }

        let metrics_task = {
            let me = self.clone();
            let out = out.clone();
            tokio::spawn(async move { me.metrics_loop(out).await })
        };

        let mut streams: HashMap<u64, StreamHandle> = HashMap::new();

        while let Some(msg) = inbound.recv().await {
            match msg {
                HubMsg::Ping => {
                    let _ = out.try_send(AgentMsg::Pong);
                }
                HubMsg::Request { id, req } => {
                    let me = self.clone();
                    let out = out.clone();
                    tokio::spawn(async move {
                        let reply = match me.handle(req).await {
                            Ok(data) => AgentMsg::Reply { id, ok: true, data, error: None },
                            Err(e) => AgentMsg::Reply { id, ok: false, data: Value::Null, error: Some(format!("{e:#}")) },
                        };
                        let _ = out.send(reply).await;
                    });
                }
                HubMsg::Open { sid, req } => {
                    streams.retain(|_, h| !h.task.is_finished());
                    if let Some(old) = streams.remove(&sid) {
                        old.task.abort();
                    }
                    let (ctl_tx, ctl_rx) = mpsc::channel(64);
                    let me = self.clone();
                    let out2 = out.clone();
                    let task = tokio::spawn(async move {
                        let res = me.run_stream(sid, req, ctl_rx, &out2).await;
                        let error = res.err().map(|e| format!("{e:#}"));
                        let _ = out2.send(AgentMsg::StreamEnd { sid, error }).await;
                    });
                    streams.insert(sid, StreamHandle { ctl: ctl_tx, task });
                }
                HubMsg::Input { sid, b64 } => {
                    if let (Some(h), Some(bytes)) = (streams.get(&sid), unb64(&b64)) {
                        let _ = h.ctl.send(StreamCtl::Input(bytes)).await;
                    }
                }
                HubMsg::Resize { sid, cols, rows } => {
                    if let Some(h) = streams.get(&sid) {
                        let _ = h.ctl.send(StreamCtl::Resize(cols, rows)).await;
                    }
                }
                HubMsg::Close { sid } => {
                    if let Some(h) = streams.remove(&sid) {
                        // Dropping the control sender lets exec sessions close stdin gracefully.
                        drop(h.ctl);
                        let task = h.task;
                        tokio::spawn(async move {
                            tokio::time::sleep(Duration::from_millis(500)).await;
                            task.abort();
                        });
                    }
                }
            }
        }

        metrics_task.abort();
        for (_, h) in streams {
            h.task.abort();
        }
    }

    async fn metrics_loop(self: Arc<Self>, out: mpsc::Sender<AgentMsg>) {
        let mut tick = tokio::time::interval(Duration::from_secs(self.opts.interval_secs as u64));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tick.tick().await; // first tick is immediate; give CPU counters a baseline period
        loop {
            tick.tick().await;
            let col = self.collector.clone();
            let mssql = self.mssql.clone();
            let iis = self.iis.clone();
            let sample = tokio::task::spawn_blocking(move || {
                let mut c = col.lock().unwrap_or_else(|e| e.into_inner());
                let mut s = c.sample();
                s.mssql = mssql.summary(&c);
                s.iis = iis.summary(&c);
                s
            })
            .await;
            let Ok(mut sample) = sample else { continue };
            let n = self.tick_no.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let every = |secs: u64| n % (secs / self.opts.interval_secs.max(1) as u64).max(1) == 0;
            if let Some(d) = &self.docker {
                sample.docker = d.summary().await;
                if self.opts.container_history {
                    d.ensure_live();
                    if every(30) {
                        sample.containers = d.usage();
                    }
                }
            }
            if let (Some(k), true) = (&self.kube, self.kube_ok.load(std::sync::atomic::Ordering::Relaxed)) {
                sample.k8s = k.summary().await;
                if every(60) {
                    sample.pods = k.pod_usage().await.unwrap_or_default();
                }
            }
            if every(30) {
                sample.failed_services = tokio::time::timeout(Duration::from_secs(10), self.services.failed()).await.ok().flatten();
            }
            if out.send(AgentMsg::Metrics(sample)).await.is_err() {
                return;
            }
        }
    }

    fn kube(&self) -> Result<&crate::k8s::Kube> {
        self.kube.as_deref().ok_or_else(|| anyhow!("Kubernetes was not found on this server"))
    }

    fn docker(&self) -> Result<&DockerCtl> {
        self.docker.as_ref().ok_or_else(|| anyhow!("Docker is not available on this server"))
    }

    /// Run a closure that needs the shared collector on the blocking pool.
    async fn with_collector<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Collector) -> Result<T> + Send + 'static,
    {
        let col = self.collector.clone();
        tokio::task::spawn_blocking(move || {
            let c = col.lock().unwrap_or_else(|e| e.into_inner());
            f(&c)
        })
        .await?
    }

    async fn blocking<T, F>(f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T> + Send + 'static,
    {
        tokio::task::spawn_blocking(f).await?
    }

    pub async fn handle(&self, req: Request) -> Result<Value> {
        match req {
            Request::SystemDetail => {
                let root = self.opts.host_root.clone();
                Self::blocking(move || Ok(Collector::detail(root))).await
            }
            Request::DockerContainers => self.docker()?.containers().await,
            Request::DockerInspect { id } => self.docker()?.inspect(&id).await,
            Request::DockerContainerAction { id, action } => self.docker()?.action(&id, action).await,
            Request::DockerContainerRemove { id, force, volumes } => self.docker()?.remove(&id, force, volumes).await,
            Request::K8sOverview => self.kube()?.overview().await,
            Request::K8sNamespaces => self.kube()?.namespaces().await,
            Request::K8sPods { namespace } => self.kube()?.pods(namespace.as_deref()).await,
            Request::K8sWorkloads { namespace } => self.kube()?.workloads(namespace.as_deref()).await,
            Request::K8sServices { namespace } => self.kube()?.services(namespace.as_deref()).await,
            Request::K8sEvents { namespace } => self.kube()?.events(namespace.as_deref()).await,
            Request::K8sGet { kind, namespace, name } => self.kube()?.get_object(kind, namespace.as_deref(), &name).await,
            Request::K8sPodDelete { namespace, name, force } => self.kube()?.delete_pod(&namespace, &name, force).await,
            Request::K8sRolloutRestart { kind, namespace, name } => self.kube()?.rollout_restart(kind, &namespace, &name).await,
            Request::K8sScale { kind, namespace, name, replicas } => self.kube()?.scale(kind, &namespace, &name, replicas).await,
            Request::K8sNodeCordon { name, unschedulable } => self.kube()?.cordon(&name, unschedulable).await,
            Request::K8sResources { resource, namespace } => self.kube()?.resources(&resource, namespace.as_deref()).await,
            Request::K8sNodeDetail { name } => self.kube()?.node_detail(&name).await,
            Request::K8sNamespaceUsage => self.kube()?.namespace_usage().await,
            Request::K8sCronJobTrigger { namespace, name } => self.kube()?.trigger_cronjob(&namespace, &name).await,
            Request::ServicesList => self.services.list().await,
            Request::ServiceAction { name, action } => self.services.action(&name, action).await,
            Request::DockerStacks => self.docker()?.stacks().await,
            Request::DockerStackFile { project } => self.docker()?.stack_file(&project, self.opts.host_root.as_deref()).await,
            Request::DockerStackAction { project, action } => self.docker()?.stack_action(&project, action, self.opts.host_root.as_deref()).await,
            Request::SelfUpdate { file, sha256, version } => match &self.opts.update {
                Some(src) => crate::update::self_update(src, &file, &sha256, &version).await,
                None => Err(anyhow!("the hub's built-in agent is updated with the hub image")),
            },
            Request::DockerContainerRename { id, name } => self.docker()?.rename(&id, &name).await,
            Request::DockerContainerRecreate { id, pull } => self.docker()?.recreate(&id, pull, self.opts.host_root.as_deref()).await,
            Request::DockerImages => self.docker()?.images().await,
            Request::DockerImageRemove { id, force } => self.docker()?.remove_image(&id, force).await,
            Request::DockerVolumes => self.docker()?.volumes().await,
            Request::DockerVolumeRemove { name } => self.docker()?.remove_volume(&name).await,
            Request::DockerNetworks => self.docker()?.networks().await,
            Request::DockerNetworkRemove { id } => self.docker()?.remove_network(&id).await,
            Request::DockerDiskUsage => self.docker()?.disk_usage().await,
            Request::DockerPrune { target } => self.docker()?.prune(target).await,

            Request::MssqlStatus => {
                let m = self.mssql.clone();
                let mut status = self.with_collector(move |c| Ok(m.status(c))).await?;
                if let Some(q) = self.mssql.query().await {
                    status["query"] = q;
                }
                Ok(status)
            }
            Request::MssqlErrorLog { instance, lines } => {
                let m = self.mssql.clone();
                Self::blocking(move || m.error_log(instance.as_deref(), lines)).await
            }

            Request::IisSites => {
                let iis = self.iis.clone();
                Self::blocking(move || iis.sites()).await
            }
            Request::IisAppPools => {
                let iis = self.iis.clone();
                self.with_collector(move |c| iis.app_pools(c)).await
            }
            Request::IisSiteAction { name, action } => {
                let iis = self.iis.clone();
                Self::blocking(move || iis.site_action(&name, action)).await
            }
            Request::IisAppPoolAction { name, action } => {
                let iis = self.iis.clone();
                Self::blocking(move || iis.pool_action(&name, action)).await
            }
            Request::IisLogFiles { site_id } => {
                let iis = self.iis.clone();
                Self::blocking(move || iis.log_files(site_id)).await
            }
            Request::IisLogTail { site_id, file, lines } => {
                let iis = self.iis.clone();
                Self::blocking(move || iis.log_tail(site_id, file.as_deref(), lines)).await
            }
        }
    }

    async fn run_stream(
        &self,
        sid: u64,
        req: StreamRequest,
        ctl: mpsc::Receiver<StreamCtl>,
        out: &mpsc::Sender<AgentMsg>,
    ) -> Result<()> {
        match req {
            StreamRequest::ContainerLogs { id, tail, since, follow } => {
                self.docker()?.stream_logs(&id, tail, since, follow, sid, out).await
            }
            StreamRequest::ContainerExec { id, cmd, user, cols, rows } => {
                self.docker()?.exec(&id, cmd, user, cols, rows, sid, out, ctl).await
            }
            StreamRequest::ContainerStats { id } => self.docker()?.stream_stats(&id, sid, out).await,
            StreamRequest::ContainerStatsAll {} => self.docker()?.stream_stats_all(sid, out).await,
            StreamRequest::K8sPodLogs { namespace, name, container, tail, since, follow, previous } => {
                self.kube()?.stream_logs(&namespace, &name, container, tail, since, follow, previous, sid, out).await
            }
            StreamRequest::StackLogs { project, tail, since } => self.docker()?.stream_stack_logs(&project, tail, since, sid, out).await,
            #[cfg(target_os = "linux")]
            StreamRequest::SystemLogs { unit, log: _, priority, tail, since, follow } => self.services.stream_logs(unit, priority, tail, since, follow, sid, out).await,
            #[cfg(windows)]
            StreamRequest::SystemLogs { unit, log, priority, tail, since, follow } => self.services.stream_logs(log, unit, priority, tail, since, follow, sid, out).await,
            #[cfg(not(any(target_os = "linux", windows)))]
            StreamRequest::SystemLogs { .. } => Err(anyhow!("system logs are not supported on this platform")),
            StreamRequest::K8sPodExec { namespace, name, container, cmd, cols, rows } => {
                self.kube()?.exec(&namespace, &name, container, cmd, cols, rows, sid, out, ctl).await
            }
            StreamRequest::SystemLive { interval_ms } => {
                let interval = Duration::from_millis(interval_ms.clamp(1000, 60_000) as u64);
                let root = self.opts.host_root.clone();
                let col = Arc::new(Mutex::new(tokio::task::spawn_blocking(move || Collector::new(root, false)).await?));
                loop {
                    tokio::time::sleep(interval).await;
                    let c = col.clone();
                    let sample = tokio::task::spawn_blocking(move || c.lock().unwrap_or_else(|e| e.into_inner()).sample()).await?;
                    out.send(AgentMsg::Stream { sid, data: StreamData::Live(sample) }).await.map_err(|_| anyhow!("closed"))?;
                }
            }
            StreamRequest::IisLogFollow { site_id, tail } => {
                let dir = {
                    let iis = self.iis.clone();
                    Self::blocking(move || iis.log_dir_for(site_id)).await?
                };
                crate::textlog::follow(move || crate::iis::newest_log(&dir), tail, sid, out).await
            }
            StreamRequest::MssqlLogFollow { instance, tail } => {
                let m = self.mssql.clone();
                let path = Self::blocking(move || m.errorlog_path(instance.as_deref())).await?;
                if !path.exists() {
                    bail!("error log not found");
                }
                crate::textlog::follow(move || Some(path.clone()), tail, sid, out).await
            }
        }
    }
}
