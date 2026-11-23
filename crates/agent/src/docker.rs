//! Docker engine integration (containers, logs, exec, stats, images, volumes, networks, cleanup).

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use bollard::container::{
    InspectContainerOptions, KillContainerOptions, ListContainersOptions, LogOutput, LogsOptions,
    PruneContainersOptions, RemoveContainerOptions, RestartContainerOptions, StartContainerOptions, Stats,
    StatsOptions, StopContainerOptions,
};
use bollard::exec::{CreateExecOptions, ResizeExecOptions, StartExecOptions, StartExecResults};
use bollard::image::{ListImagesOptions, PruneImagesOptions, RemoveImageOptions};
use bollard::network::{ListNetworksOptions, PruneNetworksOptions};
use bollard::volume::{ListVolumesOptions, PruneVolumesOptions, RemoveVolumeOptions};
use bollard::Docker;
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;
use vigil_common::{AgentMsg, ContainerAction, ContainerStatsSample, DockerSummary, LogLine, PruneTarget, StreamData};

use crate::session::StreamCtl;
use crate::util::{b64, now_ms, truncate};

const MAX_LOG_LINE: usize = 64 * 1024;

#[derive(Clone)]
pub struct DockerCtl {
    pub d: Docker,
    #[cfg_attr(not(unix), allow(dead_code))]
    socket: String,
}

#[cfg_attr(not(unix), allow(dead_code))]
fn default_socket() -> String {
    if let Ok(h) = std::env::var("DOCKER_HOST") {
        if let Some(p) = h.strip_prefix("unix://") {
            return p.to_string();
        }
    }
    "/var/run/docker.sock".to_string()
}

impl DockerCtl {
    pub fn new(socket: Option<String>) -> Result<Self> {
        #[cfg(unix)]
        {
            let socket = socket.unwrap_or_else(default_socket);
            let d = Docker::connect_with_socket(&socket, 120, bollard::API_DEFAULT_VERSION)?;
            Ok(Self { d, socket })
        }
        #[cfg(not(unix))]
        {
            let _ = socket;
            let d = Docker::connect_with_local_defaults()?;
            Ok(Self { d, socket: String::new() })
        }
    }

    pub async fn version(&self) -> Option<String> {
        match tokio::time::timeout(Duration::from_secs(3), self.d.version()).await {
            Ok(Ok(v)) => Some(v.version.unwrap_or_else(|| "unknown".into())),
            _ => None,
        }
    }

    pub async fn summary(&self) -> Option<DockerSummary> {
        let list = tokio::time::timeout(
            Duration::from_secs(5),
            self.d.list_containers(Some(ListContainersOptions::<String> { all: true, ..Default::default() })),
        )
        .await
        .ok()?
        .ok()?;
        let mut s = DockerSummary { total: list.len() as u32, ..Default::default() };
        for c in &list {
            match c.state.as_deref() {
                Some("running") => s.running += 1,
                Some("paused") => s.paused += 1,
                Some("restarting") => {
                    s.unhealthy += 1;
                    s.stopped += 1;
                }
                _ => s.stopped += 1,
            }
            if c.status.as_deref().map(|st| st.contains("(unhealthy)")).unwrap_or(false) {
                s.unhealthy += 1;
            }
        }
        if let Ok(Ok(info)) = tokio::time::timeout(Duration::from_secs(5), self.d.info()).await {
            s.images = info.images.unwrap_or(0).max(0) as u32;
        }
        Some(s)
    }

    // ------------------------------------------------------------------ containers

    pub async fn containers(&self) -> Result<Value> {
        let list = self.d.list_containers(Some(ListContainersOptions::<String> { all: true, ..Default::default() })).await?;
        let out: Vec<Value> = list
            .into_iter()
            .map(|c| {
                let names: Vec<String> =
                    c.names.unwrap_or_default().into_iter().map(|n| n.trim_start_matches('/').to_string()).collect();
                let labels = c.labels.unwrap_or_default();
                let status = c.status.unwrap_or_default();
                let health = if status.contains("(unhealthy)") {
                    Some("unhealthy")
                } else if status.contains("(healthy)") {
                    Some("healthy")
                } else if status.contains("(health: starting)") {
                    Some("starting")
                } else {
                    None
                };
                let mut ports: Vec<Value> = c
                    .ports
                    .unwrap_or_default()
                    .into_iter()
                    .map(|p| {
                        json!({
                            "ip": p.ip,
                            "private": p.private_port,
                            "public": p.public_port,
                            "proto": p.typ.map(|t| t.to_string()).unwrap_or_else(|| "tcp".into()),
                        })
                    })
                    .collect();
                // Docker reports IPv4 and IPv6 bindings separately; keep one per mapping.
                let mut seen = HashSet::new();
                ports.retain(|p| seen.insert((p["private"].as_u64(), p["public"].as_u64(), p["proto"].to_string())));
                let networks: Vec<String> = c
                    .network_settings
                    .and_then(|n| n.networks)
                    .map(|m| m.into_keys().collect())
                    .unwrap_or_default();
                let mounts: Vec<Value> = c
                    .mounts
                    .unwrap_or_default()
                    .into_iter()
                    .map(|m| {
                        json!({
                            "type": m.typ.map(|t| t.to_string()),
                            "name": m.name,
                            "source": m.source,
                            "destination": m.destination,
                            "rw": m.rw,
                        })
                    })
                    .collect();
                json!({
                    "id": c.id,
                    "name": names.first().cloned().unwrap_or_default(),
                    "names": names,
                    "image": c.image,
                    "image_id": c.image_id,
                    "command": c.command,
                    "created": c.created,
                    "state": c.state,
                    "status": status,
                    "health": health,
                    "ports": ports,
                    "project": labels.get("com.docker.compose.project"),
                    "service": labels.get("com.docker.compose.service"),
                    "labels": labels,
                    "networks": networks,
                    "mounts": mounts,
                })
            })
            .collect();
        Ok(Value::Array(out))
    }

    pub async fn inspect(&self, id: &str) -> Result<Value> {
        let r = self.d.inspect_container(id, None::<InspectContainerOptions>).await?;
        Ok(serde_json::to_value(r)?)
    }

    pub async fn action(&self, id: &str, action: ContainerAction) -> Result<Value> {
        match action {
            ContainerAction::Start => self.d.start_container(id, None::<StartContainerOptions<String>>).await?,
            ContainerAction::Stop => self.d.stop_container(id, Some(StopContainerOptions { t: 15 })).await?,
            ContainerAction::Restart => self.d.restart_container(id, Some(RestartContainerOptions { t: 15 })).await?,
            ContainerAction::Pause => self.d.pause_container(id).await?,
            ContainerAction::Unpause => self.d.unpause_container(id).await?,
            ContainerAction::Kill => self.d.kill_container(id, None::<KillContainerOptions<String>>).await?,
        }
        Ok(json!({ "ok": true }))
    }

    pub async fn remove(&self, id: &str, force: bool, volumes: bool) -> Result<Value> {
        self.d.remove_container(id, Some(RemoveContainerOptions { force, v: volumes, link: false })).await?;
        Ok(json!({ "ok": true }))
    }

    // ------------------------------------------------------------------ images / volumes / networks

    async fn container_refs(&self) -> Result<(HashMap<String, Vec<String>>, HashMap<String, Vec<String>>, HashMap<String, u32>)> {
        let list = self.d.list_containers(Some(ListContainersOptions::<String> { all: true, ..Default::default() })).await?;
        let mut by_image: HashMap<String, Vec<String>> = HashMap::new();
        let mut by_volume: HashMap<String, Vec<String>> = HashMap::new();
        let mut by_network: HashMap<String, u32> = HashMap::new();
        for c in list {
            let name = c.names.as_ref().and_then(|n| n.first()).map(|n| n.trim_start_matches('/').to_string()).unwrap_or_default();
            if let Some(img) = c.image_id {
                by_image.entry(img).or_default().push(name.clone());
            }
            for m in c.mounts.unwrap_or_default() {
                if let Some(v) = m.name {
                    by_volume.entry(v).or_default().push(name.clone());
                }
            }
            if let Some(nets) = c.network_settings.and_then(|n| n.networks) {
                for (_, ep) in nets {
                    if let Some(nid) = ep.network_id {
                        *by_network.entry(nid).or_default() += 1;
                    }
                }
            }
        }
        Ok((by_image, by_volume, by_network))
    }

    pub async fn images(&self) -> Result<Value> {
        let (by_image, _, _) = self.container_refs().await?;
        let list = self.d.list_images(Some(ListImagesOptions::<String> { all: false, ..Default::default() })).await?;
        let out: Vec<Value> = list
            .into_iter()
            .map(|i| {
                let tags: Vec<String> = i.repo_tags.into_iter().filter(|t| t != "<none>:<none>").collect();
                let used_by = by_image.get(&i.id).cloned().unwrap_or_default();
                json!({
                    "id": i.id,
                    "tags": tags,
                    "digests": i.repo_digests,
                    "created": i.created,
                    "size": i.size,
                    "dangling": tags.is_empty(),
                    "in_use": !used_by.is_empty(),
                    "used_by": used_by,
                })
            })
            .collect();
        Ok(Value::Array(out))
    }

    pub async fn remove_image(&self, id: &str, force: bool) -> Result<Value> {
        let r = self.d.remove_image(id, Some(RemoveImageOptions { force, noprune: false }), None).await?;
        Ok(json!({ "deleted": r.len() }))
    }

    pub async fn volumes(&self) -> Result<Value> {
        let (_, by_volume, _) = self.container_refs().await?;
        let r = self.d.list_volumes(None::<ListVolumesOptions<String>>).await?;
        let out: Vec<Value> = r
            .volumes
            .unwrap_or_default()
            .into_iter()
            .map(|v| {
                let used_by = by_volume.get(&v.name).cloned().unwrap_or_default();
                json!({
                    "name": v.name,
                    "driver": v.driver,
                    "mountpoint": v.mountpoint,
                    "created": serde_json::to_value(&v.created_at).unwrap_or(Value::Null),
                    "labels": v.labels,
                    "project": v.labels.get("com.docker.compose.project"),
                    "anonymous": v.labels.get("com.docker.volume.anonymous").is_some()
                        || (v.name.len() == 64 && v.name.chars().all(|c| c.is_ascii_hexdigit())),
                    "in_use": !used_by.is_empty(),
                    "used_by": used_by,
                })
            })
            .collect();
        Ok(Value::Array(out))
    }

    pub async fn remove_volume(&self, name: &str) -> Result<Value> {
        self.d.remove_volume(name, None::<RemoveVolumeOptions>).await?;
        Ok(json!({ "ok": true }))
    }

    pub async fn networks(&self) -> Result<Value> {
        let (_, _, by_network) = self.container_refs().await?;
        let list = self.d.list_networks(None::<ListNetworksOptions<String>>).await?;
        let out: Vec<Value> = list
            .into_iter()
            .map(|n| {
                let id = n.id.clone().unwrap_or_default();
                let name = n.name.clone().unwrap_or_default();
                let subnets: Vec<String> = n
                    .ipam
                    .as_ref()
                    .and_then(|i| i.config.as_ref())
                    .map(|cfgs| cfgs.iter().filter_map(|c| c.subnet.clone()).collect())
                    .unwrap_or_default();
                json!({
                    "id": id,
                    "name": name,
                    "driver": n.driver,
                    "scope": n.scope,
                    "internal": n.internal,
                    "created": serde_json::to_value(&n.created).unwrap_or(Value::Null),
                    "subnets": subnets,
                    "containers": by_network.get(&id).copied().unwrap_or(0),
                    "builtin": matches!(name.as_str(), "bridge" | "host" | "none"),
                    "project": n.labels.as_ref().and_then(|l| l.get("com.docker.compose.project")),
                })
            })
            .collect();
        Ok(Value::Array(out))
    }

    pub async fn remove_network(&self, id: &str) -> Result<Value> {
        self.d.remove_network(id).await?;
        Ok(json!({ "ok": true }))
    }

    /// Equivalent of `docker system df -v`, with the CLI's "reclaimable" maths.
    pub async fn disk_usage(&self) -> Result<Value> {
        let df = self.d.df().await?;
        let layers = df.layers_size.unwrap_or(0).max(0);
        let images = df.images.unwrap_or_default();
        let mut img_used = 0i64;
        let mut img_active = 0;
        let img_rows: Vec<Value> = images
            .iter()
            .map(|i| {
                if i.containers > 0 {
                    img_active += 1;
                    if i.shared_size >= 0 {
                        img_used += i.size - i.shared_size;
                    }
                }
                json!({
                    "id": i.id,
                    "tags": i.repo_tags.iter().filter(|t| *t != "<none>:<none>").collect::<Vec<_>>(),
                    "size": i.size,
                    "shared": i.shared_size,
                    "containers": i.containers,
                    "created": i.created,
                })
            })
            .collect();

        let containers = df.containers.unwrap_or_default();
        let mut c_size = 0i64;
        let mut c_recl = 0i64;
        let mut c_active = 0;
        let c_rows: Vec<Value> = containers
            .iter()
            .map(|c| {
                let sz = c.size_rw.unwrap_or(0).max(0);
                c_size += sz;
                if c.state.as_deref() == Some("running") {
                    c_active += 1;
                } else {
                    c_recl += sz;
                }
                json!({
                    "id": c.id,
                    "name": c.names.as_ref().and_then(|n| n.first()).map(|n| n.trim_start_matches('/')),
                    "image": c.image,
                    "state": c.state,
                    "size_rw": sz,
                })
            })
            .collect();

        let volumes = df.volumes.unwrap_or_default();
        let mut v_size = 0i64;
        let mut v_recl = 0i64;
        let mut v_active = 0;
        let v_rows: Vec<Value> = volumes
            .iter()
            .map(|v| {
                let (sz, refs) = v.usage_data.as_ref().map(|u| (u.size, u.ref_count)).unwrap_or((-1, -1));
                if sz > 0 {
                    v_size += sz;
                }
                if refs > 0 {
                    v_active += 1;
                } else if sz > 0 {
                    v_recl += sz;
                }
                json!({ "name": v.name, "size": sz, "ref_count": refs, "driver": v.driver })
            })
            .collect();

        let cache = df.build_cache.unwrap_or_default();
        let mut b_size = 0i64;
        let mut b_recl = 0i64;
        let mut b_active = 0;
        let b_rows: Vec<Value> = cache
            .iter()
            .map(|b| {
                let sz = b.size.unwrap_or(0).max(0);
                let in_use = b.in_use.unwrap_or(false);
                let shared = b.shared.unwrap_or(false);
                if !shared {
                    b_size += sz;
                }
                if in_use {
                    b_active += 1;
                } else if !shared {
                    b_recl += sz;
                }
                json!({
                    "id": b.id,
                    "type": b.typ.map(|t| t.to_string()),
                    "description": b.description,
                    "size": sz,
                    "in_use": in_use,
                    "shared": shared,
                    "last_used": serde_json::to_value(&b.last_used_at).unwrap_or(Value::Null),
                    "usage_count": b.usage_count,
                })
            })
            .collect();

        Ok(json!({
            "images": { "count": img_rows.len(), "active": img_active, "size": layers, "reclaimable": (layers - img_used).max(0) },
            "containers": { "count": c_rows.len(), "active": c_active, "size": c_size, "reclaimable": c_recl },
            "volumes": { "count": v_rows.len(), "active": v_active, "size": v_size, "reclaimable": v_recl },
            "build_cache": { "count": b_rows.len(), "active": b_active, "size": b_size, "reclaimable": b_recl },
            "details": { "images": img_rows, "containers": c_rows, "volumes": v_rows, "build_cache": b_rows },
        }))
    }

    pub async fn prune(&self, target: PruneTarget) -> Result<Value> {
        match target {
            PruneTarget::Containers => {
                let r = self.d.prune_containers(None::<PruneContainersOptions<String>>).await?;
                Ok(json!({ "deleted": r.containers_deleted.map(|v| v.len()).unwrap_or(0), "space_reclaimed": r.space_reclaimed.unwrap_or(0) }))
            }
            PruneTarget::ImagesDangling | PruneTarget::ImagesUnused => {
                let mut filters = HashMap::new();
                let dangling = if target == PruneTarget::ImagesDangling { "true" } else { "false" };
                filters.insert("dangling".to_string(), vec![dangling.to_string()]);
                let r = self.d.prune_images(Some(PruneImagesOptions { filters })).await?;
                Ok(json!({ "deleted": r.images_deleted.map(|v| v.len()).unwrap_or(0), "space_reclaimed": r.space_reclaimed.unwrap_or(0) }))
            }
            PruneTarget::Volumes => {
                // API >= 1.42 only prunes anonymous volumes unless all=true.
                let mut filters = HashMap::new();
                filters.insert("all".to_string(), vec!["true".to_string()]);
                let r = match self.d.prune_volumes(Some(PruneVolumesOptions { filters })).await {
                    Ok(r) => r,
                    // Older engines reject the `all` filter; fall back to the default behaviour.
                    Err(_) => self.d.prune_volumes(None::<PruneVolumesOptions<String>>).await?,
                };
                Ok(json!({ "deleted": r.volumes_deleted.map(|v| v.len()).unwrap_or(0), "space_reclaimed": r.space_reclaimed.unwrap_or(0) }))
            }
            PruneTarget::Networks => {
                let r = self.d.prune_networks(None::<PruneNetworksOptions<String>>).await?;
                Ok(json!({ "deleted": r.networks_deleted.map(|v| v.len()).unwrap_or(0), "space_reclaimed": 0 }))
            }
            PruneTarget::BuildCache => self.build_prune().await,
        }
    }

    /// `POST /build/prune?all=1` — not exposed by bollard 0.18, so spoken directly over the socket.
    #[cfg(unix)]
    async fn build_prune(&self) -> Result<Value> {
        use anyhow::Context;
        use tokio::io::AsyncReadExt;
        let mut s = tokio::net::UnixStream::connect(&self.socket)
            .await
            .with_context(|| format!("connecting to {}", self.socket))?;
        s.write_all(b"POST /build/prune?all=1 HTTP/1.0\r\nHost: docker\r\nContent-Length: 0\r\n\r\n").await?;
        let mut buf = Vec::new();
        tokio::time::timeout(Duration::from_secs(600), s.read_to_end(&mut buf)).await.context("build prune timed out")??;
        let text = String::from_utf8_lossy(&buf);
        let (head, body) = text.split_once("\r\n\r\n").ok_or_else(|| anyhow!("malformed response from docker"))?;
        let status_ok = head.lines().next().map(|l| l.contains(" 200 ")).unwrap_or(false);
        if !status_ok {
            bail!("docker: {}", body.trim());
        }
        let v: Value = serde_json::from_str(body.trim()).unwrap_or(Value::Null);
        Ok(json!({
            "deleted": v["CachesDeleted"].as_array().map(|a| a.len()).unwrap_or(0),
            "space_reclaimed": v["SpaceReclaimed"].as_i64().unwrap_or(0),
        }))
    }

    #[cfg(not(unix))]
    async fn build_prune(&self) -> Result<Value> {
        bail!("build cache prune is only supported on Linux hosts")
    }

    // ------------------------------------------------------------------ streams

    pub async fn stream_logs(
        &self,
        id: &str,
        tail: u32,
        since: Option<i64>,
        follow: bool,
        sid: u64,
        out: &mpsc::Sender<AgentMsg>,
    ) -> Result<()> {
        let opts = LogsOptions::<String> {
            follow,
            stdout: true,
            stderr: true,
            timestamps: true,
            since: since.unwrap_or(0),
            until: 0,
            tail: if since.is_some() { "all".into() } else { tail.min(100_000).to_string() },
        };
        let mut stream = self.d.logs(id, Some(opts));
        let mut batch: Vec<LogLine> = Vec::new();
        let mut partial: [String; 3] = Default::default();
        let mut tick = tokio::time::interval(Duration::from_millis(120));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                item = stream.next() => {
                    match item {
                        Some(Ok(o)) => {
                            let (s, bytes) = match o {
                                LogOutput::StdOut { message } => (1u8, message),
                                LogOutput::StdErr { message } => (2u8, message),
                                LogOutput::Console { message } => (1u8, message),
                                LogOutput::StdIn { .. } => continue,
                            };
                            let buf = &mut partial[s as usize];
                            buf.push_str(&String::from_utf8_lossy(&bytes));
                            while let Some(idx) = buf.find('\n') {
                                let line: String = buf.drain(..=idx).collect();
                                batch.push(parse_docker_line(line.trim_end_matches('\n'), s));
                            }
                            if buf.len() > MAX_LOG_LINE {
                                let line = std::mem::take(buf);
                                batch.push(parse_docker_line(&line, s));
                            }
                            if batch.len() >= 500 {
                                flush(out, sid, &mut batch).await?;
                            }
                        }
                        Some(Err(e)) => {
                            flush(out, sid, &mut batch).await?;
                            return Err(e.into());
                        }
                        None => {
                            for (s, p) in partial.iter_mut().enumerate() {
                                if !p.is_empty() {
                                    let line = std::mem::take(p);
                                    batch.push(parse_docker_line(&line, s as u8));
                                }
                            }
                            flush(out, sid, &mut batch).await?;
                            return Ok(());
                        }
                    }
                }
                _ = tick.tick() => {
                    if !batch.is_empty() {
                        flush(out, sid, &mut batch).await?;
                    }
                }
            }
        }
    }

    pub async fn stream_stats(&self, id: &str, sid: u64, out: &mpsc::Sender<AgentMsg>) -> Result<()> {
        let mut stream = self.d.stats(id, Some(StatsOptions { stream: true, one_shot: false }));
        while let Some(item) = stream.next().await {
            let st = item?;
            let sample = stats_sample(&st);
            out.send(AgentMsg::Stream { sid, data: StreamData::Stats(sample) }).await.map_err(|_| anyhow!("closed"))?;
        }
        Ok(())
    }

    pub async fn exec(
        &self,
        id: &str,
        cmd: Option<Vec<String>>,
        user: Option<String>,
        cols: u16,
        rows: u16,
        sid: u64,
        out: &mpsc::Sender<AgentMsg>,
        mut ctl: mpsc::Receiver<StreamCtl>,
    ) -> Result<()> {
        let cmd = cmd.filter(|c| !c.is_empty()).unwrap_or_else(|| {
            vec![
                "/bin/sh".into(),
                "-c".into(),
                "if command -v bash >/dev/null 2>&1; then exec bash; else exec sh; fi".into(),
            ]
        });
        let exec = self
            .d
            .create_exec(
                id,
                CreateExecOptions::<String> {
                    attach_stdin: Some(true),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    tty: Some(true),
                    cmd: Some(cmd),
                    user: user.filter(|u| !u.is_empty()),
                    env: Some(vec!["TERM=xterm-256color".into(), "COLORTERM=truecolor".into()]),
                    ..Default::default()
                },
            )
            .await?;
        let started = self.d.start_exec(&exec.id, Some(StartExecOptions { detach: false, tty: true, output_capacity: None })).await?;
        let StartExecResults::Attached { mut output, mut input } = started else {
            bail!("exec started detached");
        };
        let _ = self.d.resize_exec(&exec.id, ResizeExecOptions { height: rows.max(1), width: cols.max(1) }).await;

        loop {
            tokio::select! {
                item = output.next() => {
                    match item {
                        Some(Ok(o)) => {
                            let bytes = o.into_bytes();
                            out.send(AgentMsg::Stream { sid, data: StreamData::Bytes { b64: b64(&bytes) } })
                                .await
                                .map_err(|_| anyhow!("closed"))?;
                        }
                        Some(Err(e)) => return Err(e.into()),
                        None => return Ok(()),
                    }
                }
                c = ctl.recv() => {
                    match c {
                        Some(StreamCtl::Input(data)) => {
                            input.write_all(&data).await?;
                            input.flush().await?;
                        }
                        Some(StreamCtl::Resize(cols, rows)) => {
                            let _ = self.d.resize_exec(&exec.id, ResizeExecOptions { height: rows.max(1), width: cols.max(1) }).await;
                        }
                        None => {
                            let _ = input.shutdown().await;
                            return Ok(());
                        }
                    }
                }
            }
        }
    }
}

async fn flush(out: &mpsc::Sender<AgentMsg>, sid: u64, batch: &mut Vec<LogLine>) -> Result<()> {
    if batch.is_empty() {
        return Ok(());
    }
    let lines = std::mem::take(batch);
    out.send(AgentMsg::Stream { sid, data: StreamData::Lines { lines } }).await.map_err(|_| anyhow!("closed"))
}

/// Split Docker's `timestamps=true` prefix (`2024-01-02T03:04:05.123456789Z `) from the line.
fn parse_docker_line(line: &str, s: u8) -> LogLine {
    let line = line.strip_suffix('\r').unwrap_or(line);
    if let Some((ts, rest)) = line.split_once(' ') {
        let b = ts.as_bytes();
        if b.len() >= 20 && b[4] == b'-' && b[10] == b'T' && b[0].is_ascii_digit() {
            return LogLine { ts: Some(ts.to_string()), s, text: truncate(rest.to_string(), MAX_LOG_LINE) };
        }
    }
    LogLine { ts: None, s, text: truncate(line.to_string(), MAX_LOG_LINE) }
}

fn stats_sample(st: &Stats) -> ContainerStatsSample {
    let cpu_delta = st.cpu_stats.cpu_usage.total_usage as f64 - st.precpu_stats.cpu_usage.total_usage as f64;
    let sys_delta = st.cpu_stats.system_cpu_usage.unwrap_or(0) as f64 - st.precpu_stats.system_cpu_usage.unwrap_or(0) as f64;
    let ncpu = st
        .cpu_stats
        .online_cpus
        .filter(|n| *n > 0)
        .or_else(|| st.cpu_stats.cpu_usage.percpu_usage.as_ref().map(|v| v.len() as u64))
        .unwrap_or(1) as f64;
    let cpu = if sys_delta > 0.0 && cpu_delta > 0.0 { cpu_delta / sys_delta * ncpu * 100.0 } else { 0.0 };

    let usage = st.memory_stats.usage.unwrap_or(0);
    let cache = match &st.memory_stats.stats {
        Some(bollard::container::MemoryStatsStats::V1(v1)) => v1.total_inactive_file,
        Some(bollard::container::MemoryStatsStats::V2(v2)) => v2.inactive_file,
        None => 0,
    };
    let (mut rx, mut tx) = (0, 0);
    if let Some(nets) = &st.networks {
        for n in nets.values() {
            rx += n.rx_bytes;
            tx += n.tx_bytes;
        }
    }
    let (mut br, mut bw) = (0, 0);
    if let Some(entries) = &st.blkio_stats.io_service_bytes_recursive {
        for e in entries {
            match e.op.to_ascii_lowercase().as_str() {
                "read" => br += e.value,
                "write" => bw += e.value,
                _ => {}
            }
        }
    }
    ContainerStatsSample {
        ts: now_ms(),
        cpu: (cpu * 100.0).round() / 100.0,
        mem_used: usage.saturating_sub(cache),
        mem_limit: st.memory_stats.limit.unwrap_or(0),
        net_rx: rx,
        net_tx: tx,
        blk_read: br,
        blk_write: bw,
        pids: st.pids_stats.current.unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_line_parsing() {
        let l = parse_docker_line("2024-05-01T10:11:12.123456789Z hello world", 1);
        assert_eq!(l.ts.as_deref(), Some("2024-05-01T10:11:12.123456789Z"));
        assert_eq!(l.text, "hello world");
        let l = parse_docker_line("plain line\r", 2);
        assert!(l.ts.is_none());
        assert_eq!(l.text, "plain line");
        assert_eq!(l.s, 2);
    }
}
