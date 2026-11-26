//! Docker engine integration (containers, logs, exec, stats, images, volumes, networks, cleanup).

use std::sync::Arc;
use std::collections::{HashMap, HashSet};
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use bollard::container::{
    CreateContainerOptions, InspectContainerOptions, RenameContainerOptions, KillContainerOptions, ListContainersOptions, LogOutput, LogsOptions,
    PruneContainersOptions, RemoveContainerOptions, RestartContainerOptions, StartContainerOptions, Stats,
    StatsOptions, StopContainerOptions,
};
use bollard::exec::{CreateExecOptions, ResizeExecOptions, StartExecOptions, StartExecResults};
use bollard::image::{CreateImageOptions, ListImagesOptions, PruneImagesOptions, RemoveImageOptions};
use bollard::network::{ConnectNetworkOptions, ListNetworksOptions, PruneNetworksOptions};
use bollard::volume::{ListVolumesOptions, PruneVolumesOptions, RemoveVolumeOptions};
use bollard::Docker;
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;
use monitorlab_common::{AgentMsg, ContainerAction, ContainerLiveStat, ContainerStatsSample, ContainerUsage, StackAction, DockerSummary, LogLine, PruneTarget, StreamData};

use crate::session::StreamCtl;
use crate::util::{b64, now_ms, truncate};

const MAX_LOG_LINE: usize = 64 * 1024;

#[derive(Clone)]
pub struct DockerCtl {
    pub d: Docker,
    #[cfg_attr(not(unix), allow(dead_code))]
    socket: String,
    live: Arc<LiveStats>,
}

/// Latest stats of every running container, kept by one background task (one Docker stats
/// stream per container, like `docker stats`). Feeds the live list and the stored history.
#[derive(Default)]
struct LiveStats {
    map: std::sync::Mutex<HashMap<String, (ContainerLiveStat, String, Option<String>)>>,
    started: std::sync::atomic::AtomicBool,
}

#[cfg_attr(not(unix), allow(dead_code))]
fn default_socket() -> String {
    if let Ok(h) = std::env::var("DOCKER_HOST") {
        if let Some(p) = h.strip_prefix("unix://") {
            return p.to_string();
        }
    }
    if !std::path::Path::new("/var/run/docker.sock").exists() {
        // macOS: Docker Desktop / OrbStack / Colima keep the socket in a user's home folder.
        if let Ok(users) = std::fs::read_dir("/Users") {
            for u in users.flatten() {
                for rel in [".docker/run/docker.sock", ".orbstack/run/docker.sock", ".colima/default/docker.sock", ".rd/docker.sock"] {
                    let p = u.path().join(rel);
                    if p.exists() {
                        return p.display().to_string();
                    }
                }
            }
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
            Ok(Self { d, socket, live: Default::default() })
        }
        #[cfg(not(unix))]
        {
            let _ = socket;
            let d = Docker::connect_with_local_defaults()?;
            Ok(Self { d, socket: String::new(), live: Default::default() })
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

    pub async fn rename(&self, id: &str, name: &str) -> Result<Value> {
        let name = name.trim().trim_start_matches('/');
        if !valid_container_name(name) {
            bail!("Container names may use letters, digits, '_', '.' and '-' and must start with a letter or digit");
        }
        self.d.rename_container(id, RenameContainerOptions { name }).await?;
        Ok(json!({ "ok": true }))
    }

    /// Pull an image reference, using registry credentials from docker config files when present.
    pub async fn pull(&self, reference: &str, host_root: Option<&str>) -> Result<()> {
        let (image, tag) = split_reference(reference);
        let creds = crate::dockerauth::credentials_for(registry_of(reference), host_root);
        let d = self.d.clone().with_timeout(Duration::from_secs(1800));
        let mut st = d.create_image(
            Some(CreateImageOptions { from_image: image.to_string(), tag: tag.to_string(), ..Default::default() }),
            None,
            creds,
        );
        while let Some(item) = st.next().await {
            let item = item.map_err(|e| {
                let msg = e.to_string();
                if msg.contains("unauthorized") || msg.contains("denied") || msg.contains("authentication required") {
                    anyhow!("The registry refused the pull ({msg}). Log in on the server with `docker login` (credentials stored in ~/.docker/config.json), or recreate without pulling.")
                } else {
                    anyhow!("Pull failed: {msg}")
                }
            })?;
            if let Some(err) = item.error {
                bail!("Pull failed: {err}");
            }
        }
        Ok(())
    }

    /// Re-create a container with identical settings. The old container is stopped and renamed,
    /// the new one created and started, then the old one removed. Any failure rolls back.
    pub async fn recreate(&self, id: &str, pull: bool, host_root: Option<&str>) -> Result<Value> {
        let old = self.d.inspect_container(id, None::<InspectContainerOptions>).await?;
        let old_id = old.id.clone().unwrap_or_default();
        if is_own_container(&old_id) {
            bail!("This is the container MonitorLab itself runs in; update it through your deployment (docker compose pull && up -d)");
        }
        let name = old.name.clone().unwrap_or_default().trim_start_matches('/').to_string();
        let cfg = old.config.clone().ok_or_else(|| anyhow!("container has no config"))?;
        let image_ref = cfg.image.clone().unwrap_or_default();
        let old_image = old.image.clone().unwrap_or_default();
        let mut pulled = false;
        if pull {
            if image_ref.contains('@') || image_ref.starts_with("sha256:") || image_ref.is_empty() {
                bail!("This container was created from a fixed image ID/digest, so there is nothing newer to pull");
            }
            self.pull(&image_ref, host_root).await?;
            pulled = true;
        }
        let new_image = if pull {
            self.d.inspect_image(&image_ref).await.ok().and_then(|i| i.id).unwrap_or_default()
        } else {
            old_image.clone()
        };

        let mut host = old.host_config.clone().unwrap_or_default();
        // Keep anonymous volumes: re-attach them by name, otherwise the new container gets empty ones.
        let mut binds = host.binds.clone().unwrap_or_default();
        let covered: HashSet<String> = binds
            .iter()
            .filter_map(|b| b.split(':').nth(1).map(str::to_string))
            .chain(host.mounts.iter().flatten().filter_map(|m| m.target.clone()))
            .collect();
        for m in old.mounts.clone().unwrap_or_default() {
            if m.typ == Some(bollard::models::MountPointTypeEnum::VOLUME) {
                if let (Some(vol), Some(dest)) = (m.name.clone(), m.destination.clone()) {
                    if !covered.contains(&dest) {
                        binds.push(format!("{vol}:{dest}{}", if m.rw == Some(false) { ":ro" } else { "" }));
                    }
                }
            }
        }
        if !binds.is_empty() {
            host.binds = Some(binds);
        }

        // Networks: the primary one goes into create (older engines accept only one), the rest are connected after.
        let short = old_id.chars().take(12).collect::<String>();
        let mode = host.network_mode.clone().unwrap_or_default();
        let mut endpoints: Vec<(String, bollard::models::EndpointSettings)> = old
            .network_settings
            .clone()
            .and_then(|n| n.networks)
            .unwrap_or_default()
            .into_iter()
            .map(|(net, ep)| (net, clean_endpoint(ep, &short)))
            .collect();
        endpoints.sort_by_key(|(n, _)| if *n == mode { 0 } else { 1 });
        let custom_net = !(mode.is_empty() || mode == "host" || mode == "none" || mode.starts_with("container:"));
        let (primary, extra) = if custom_net && !endpoints.is_empty() {
            let first = endpoints.remove(0);
            (Some(first), endpoints)
        } else {
            (None, Vec::new())
        };

        let mut config: bollard::container::Config<String> = cfg.clone().into();
        if config.hostname.as_deref() == Some(short.as_str()) {
            config.hostname = None; // default hostname = old container id
        }
        config.image = Some(image_ref.clone());
        config.host_config = Some(host);
        config.networking_config = primary.map(|(n, ep)| bollard::container::NetworkingConfig { endpoints_config: HashMap::from([(n, ep)]) });

        let was_running = old.state.as_ref().and_then(|s| s.running).unwrap_or(false);
        let backup = format!("{name}-old-{}", now_ms() / 1000);
        if was_running {
            self.d.stop_container(&old_id, Some(StopContainerOptions { t: 15 })).await?;
        }
        self.d.rename_container(&old_id, RenameContainerOptions { name: backup.as_str() }).await?;

        let created = match self.d.create_container(Some(CreateContainerOptions { name: name.clone(), platform: None }), config).await {
            Ok(c) => c,
            Err(e) => return Err(self.restore(&old_id, &name, was_running, e.into(), None).await),
        };
        for (net, ep) in extra {
            let r = self
                .d
                .connect_network(&net, ConnectNetworkOptions { container: created.id.clone(), endpoint_config: ep })
                .await;
            if let Err(e) = r {
                return Err(self.restore(&old_id, &name, was_running, anyhow!("connecting network {net}: {e}"), Some(&created.id)).await);
            }
        }
        if was_running {
            if let Err(e) = self.d.start_container(&created.id, None::<StartContainerOptions<String>>).await {
                return Err(self.restore(&old_id, &name, was_running, e.into(), Some(&created.id)).await);
            }
        }
        // Success: the backup goes; its named and anonymous volumes stay (now used by the new container).
        let _ = self.d.remove_container(&old_id, Some(RemoveContainerOptions { force: true, v: false, link: false })).await;
        Ok(json!({
            "ok": true,
            "id": created.id,
            "pulled": pulled,
            "image_changed": pulled && !new_image.is_empty() && new_image != old_image,
        }))
    }

    /// Roll a failed recreate back: drop the new container, give the old one its name back, restart it.
    async fn restore(&self, old_id: &str, name: &str, was_running: bool, why: anyhow::Error, new_id: Option<&str>) -> anyhow::Error {
        if let Some(nid) = new_id {
            let _ = self.d.remove_container(nid, Some(RemoveContainerOptions { force: true, v: false, link: false })).await;
        }
        let _ = self.d.rename_container(old_id, RenameContainerOptions { name }).await;
        if was_running {
            let _ = self.d.start_container(old_id, None::<StartContainerOptions<String>>).await;
        }
        anyhow!("Recreate failed, the original container was restored: {why}")
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

    pub async fn create_volume(&self, name: &str) -> Result<Value> {
        let name = name.trim();
        if name.is_empty() || name.len() > 128 || !name.chars().all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c)) {
            bail!("volume names may only contain letters, digits, _ . -");
        }
        self.d.create_volume(bollard::volume::CreateVolumeOptions::<String> { name: name.to_string(), ..Default::default() }).await?;
        Ok(json!({ "ok": true }))
    }

    pub async fn create_network(&self, name: &str, subnet: Option<String>) -> Result<Value> {
        let name = name.trim();
        if name.is_empty() || name.len() > 128 || !name.chars().all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c)) {
            bail!("network names may only contain letters, digits, _ . -");
        }
        let ipam = subnet.filter(|s| !s.trim().is_empty()).map(|s| bollard::models::Ipam {
            config: Some(vec![bollard::models::IpamConfig { subnet: Some(s.trim().to_string()), ..Default::default() }]),
            ..Default::default()
        });
        let r = self
            .d
            .create_network(bollard::network::CreateNetworkOptions::<String> { name: name.to_string(), driver: "bridge".into(), ipam: ipam.unwrap_or_default(), ..Default::default() })
            .await?;
        Ok(json!({ "ok": true, "id": r.id }))
    }

    /// `docker run -d` from the "Add container" form.
    pub async fn create(&self, n: NewContainer, host_root: Option<&str>) -> Result<Value> {
        let image = n.image.trim().to_string();
        if image.is_empty() {
            bail!("an image is required");
        }
        let present = self.d.inspect_image(&image).await.is_ok();
        let mut pulled = false;
        if n.pull || !present {
            match self.pull(&image, host_root).await {
                Ok(()) => pulled = true,
                Err(e) if present => tracing::warn!("pull of {image} failed, using the local copy: {e:#}"),
                Err(e) => return Err(e),
            }
        }
        let mut bindings: HashMap<String, Option<Vec<bollard::models::PortBinding>>> = HashMap::new();
        let mut exposed: HashMap<String, HashMap<(), ()>> = HashMap::new();
        for p in n.ports.iter().map(|p| p.trim()).filter(|p| !p.is_empty()) {
            let (spec, proto) = p.split_once('/').unwrap_or((p, "tcp"));
            if !matches!(proto, "tcp" | "udp" | "sctp") {
                bail!("port '{p}': protocol must be tcp, udp or sctp");
            }
            let parts: Vec<&str> = spec.split(':').collect();
            let (ip, host, cont) = match parts.as_slice() {
                [c] => (None, None, *c),
                [h, c] => (None, Some(*h), *c),
                [ip, h, c] => (Some(*ip), Some(*h), *c),
                _ => bail!("port '{p}': use host:container, ip:host:container or container"),
            };
            if cont.parse::<u16>().is_err() || host.map(|h| h.parse::<u16>().is_err()).unwrap_or(false) {
                bail!("port '{p}': ports must be numbers");
            }
            let key = format!("{cont}/{proto}");
            exposed.insert(key.clone(), HashMap::new());
            bindings.entry(key).or_insert_with(|| Some(vec![])).as_mut().unwrap().push(bollard::models::PortBinding { host_ip: ip.map(String::from), host_port: host.map(String::from) });
        }
        for e in n.env.iter().filter(|e| !e.trim().is_empty()) {
            if !e.contains('=') {
                bail!("environment variable '{e}': use KEY=value");
            }
        }
        for v in n.volumes.iter().filter(|v| !v.trim().is_empty()) {
            if v.split(':').count() < 2 {
                bail!("volume '{v}': use name-or-path:/container/path[:ro]");
            }
        }
        let name = n.name.as_deref().map(str::trim).filter(|s| !s.is_empty());
        if let Some(nm) = name {
            if !nm.chars().all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c)) {
                bail!("container names may only contain letters, digits, _ . -");
            }
        }
        let restart = match n.restart.as_deref().unwrap_or("") {
            "" | "no" => None,
            "always" => Some(bollard::models::RestartPolicyNameEnum::ALWAYS),
            "unless-stopped" => Some(bollard::models::RestartPolicyNameEnum::UNLESS_STOPPED),
            "on-failure" => Some(bollard::models::RestartPolicyNameEnum::ON_FAILURE),
            other => bail!("unknown restart policy '{other}'"),
        };
        let host = bollard::models::HostConfig {
            port_bindings: Some(bindings),
            binds: Some(n.volumes.iter().map(|v| v.trim().to_string()).filter(|v| !v.is_empty()).collect()),
            restart_policy: restart.map(|r| bollard::models::RestartPolicy { name: Some(r), maximum_retry_count: None }),
            network_mode: n.network.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(String::from),
            privileged: Some(n.privileged),
            ..Default::default()
        };
        let config: bollard::container::Config<String> = bollard::container::Config {
            image: Some(image.clone()),
            env: Some(n.env.iter().map(|e| e.trim().to_string()).filter(|e| !e.is_empty()).collect()),
            cmd: n.cmd.filter(|c| !c.is_empty()),
            exposed_ports: Some(exposed),
            host_config: Some(host),
            ..Default::default()
        };
        let created = self
            .d
            .create_container(name.map(|nm| CreateContainerOptions { name: nm.to_string(), platform: None }), config)
            .await?;
        if n.start {
            if let Err(e) = self.d.start_container(&created.id, None::<StartContainerOptions<String>>).await {
                // Leave nothing half-made behind (e.g. a host port that is already taken).
                let _ = self.d.remove_container(&created.id, Some(RemoveContainerOptions { force: true, v: false, link: false })).await;
                return Err(anyhow!("could not start it: {e}"));
            }
        }
        Ok(json!({ "ok": true, "id": created.id, "pulled": pulled, "started": n.start }))
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
                                batch.push(parse_log_line(line.trim_end_matches('\n'), s));
                            }
                            if buf.len() > MAX_LOG_LINE {
                                let line = std::mem::take(buf);
                                batch.push(parse_log_line(&line, s));
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
                                    batch.push(parse_log_line(&line, s as u8));
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

    /// Start the background stats collector once.
    pub fn ensure_live(&self) {
        if self.live.started.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let d = self.d.clone();
        let live = self.live.clone();
        tokio::spawn(async move {
            let mut tasks: HashMap<String, tokio::task::JoinHandle<()>> = HashMap::new();
            loop {
                let list = d.list_containers(Some(ListContainersOptions::<String> { all: false, ..Default::default() })).await;
                if let Ok(list) = list {
                    let running: HashMap<String, (String, Option<String>)> = list
                        .into_iter()
                        .filter_map(|c| {
                            let id = c.id?;
                            let name = c.names.and_then(|n| n.first().cloned()).unwrap_or_default().trim_start_matches('/').to_string();
                            let project = c.labels.and_then(|l| l.get("com.docker.compose.project").cloned());
                            Some((id, (name, project)))
                        })
                        .collect();
                    tasks.retain(|id, t| {
                        let keep = running.contains_key(id) && !t.is_finished();
                        if !keep {
                            t.abort();
                        }
                        keep
                    });
                    live.map.lock().unwrap_or_else(|e| e.into_inner()).retain(|id, _| running.contains_key(id));
                    for (id, (name, project)) in running {
                        if tasks.contains_key(&id) {
                            continue;
                        }
                        let (d2, live2, cid) = (d.clone(), live.clone(), id.clone());
                        tasks.insert(
                            id,
                            tokio::spawn(async move {
                                let mut st = d2.stats(&cid, Some(StatsOptions { stream: true, one_shot: false }));
                                while let Some(Ok(item)) = st.next().await {
                                    let s = stats_sample(&item);
                                    live2.map.lock().unwrap_or_else(|e| e.into_inner()).insert(
                                        cid.clone(),
                                        (ContainerLiveStat { id: cid.clone(), cpu: s.cpu, mem_used: s.mem_used, mem_limit: s.mem_limit }, name.clone(), project.clone()),
                                    );
                                }
                            }),
                        );
                    }
                }
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
        });
    }

    /// Current usage of every running container (for the metrics sample / history).
    pub fn usage(&self) -> Vec<ContainerUsage> {
        self.live
            .map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .map(|(s, name, project)| ContainerUsage { id: s.id.clone(), name: name.clone(), project: project.clone(), cpu: s.cpu, mem: s.mem_used })
            .collect()
    }

    /// Live CPU / memory of all running containers, every 2 s, from the background collector.
    pub async fn stream_stats_all(&self, sid: u64, out: &mpsc::Sender<AgentMsg>) -> Result<()> {
        let fresh = !self.live.started.load(std::sync::atomic::Ordering::SeqCst);
        self.ensure_live();
        if fresh {
            tokio::time::sleep(Duration::from_millis(1500)).await;
        }
        let mut tick = tokio::time::interval(Duration::from_secs(2));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let items: Vec<ContainerLiveStat> = self.live.map.lock().unwrap_or_else(|e| e.into_inner()).values().map(|v| v.0.clone()).collect();
            out.send(AgentMsg::Stream { sid, data: StreamData::StatsAll { ts: now_ms(), items } })
                .await
                .map_err(|_| anyhow!("closed"))?;
        }
    }

    // ------------------------------------------------------------------ compose stacks

    async fn stack_containers(&self, project: &str) -> Result<Vec<bollard::models::ContainerSummary>> {
        let mut filters = HashMap::new();
        filters.insert("label".to_string(), vec![format!("com.docker.compose.project={project}")]);
        let mut v = self.d.list_containers(Some(ListContainersOptions::<String> { all: true, filters, ..Default::default() })).await?;
        v.sort_by_key(|c| c.names.as_ref().and_then(|n| n.first().cloned()).unwrap_or_default());
        Ok(v)
    }

    pub async fn stacks(&self) -> Result<Value> {
        let list = self.d.list_containers(Some(ListContainersOptions::<String> { all: true, ..Default::default() })).await?;
        let mut stacks: std::collections::BTreeMap<String, Value> = Default::default();
        let usage: HashMap<String, (f64, u64)> = self.usage().into_iter().map(|u| (u.id, (u.cpu, u.mem))).collect();
        for c in list {
            let labels = c.labels.clone().unwrap_or_default();
            let Some(project) = labels.get("com.docker.compose.project").cloned() else { continue };
            let e = stacks.entry(project.clone()).or_insert_with(|| {
                json!({
                    "name": project,
                    "config_files": labels.get("com.docker.compose.project.config_files").cloned().unwrap_or_default(),
                    "working_dir": labels.get("com.docker.compose.project.working_dir").cloned().unwrap_or_default(),
                    "containers": [], "running": 0, "total": 0, "cpu": 0.0, "mem": 0u64, "images": [],
                })
            });
            let running = c.state.as_deref() == Some("running");
            let (cpu, mem) = c.id.as_ref().and_then(|i| usage.get(i)).copied().unwrap_or((0.0, 0));
            e["total"] = json!(e["total"].as_u64().unwrap_or(0) + 1);
            if running {
                e["running"] = json!(e["running"].as_u64().unwrap_or(0) + 1);
            }
            e["cpu"] = json!(e["cpu"].as_f64().unwrap_or(0.0) + cpu);
            e["mem"] = json!(e["mem"].as_u64().unwrap_or(0) + mem);
            if let Some(a) = e["containers"].as_array_mut() {
                a.push(json!({
                    "id": c.id, "name": c.names.and_then(|n| n.first().cloned()).unwrap_or_default().trim_start_matches('/'),
                    "service": labels.get("com.docker.compose.service"), "state": c.state, "status": c.status, "image": c.image,
                    "cpu": cpu, "mem": mem,
                }));
            }
        }
        Ok(Value::Array(stacks.into_values().collect()))
    }

    /// The compose file(s) of a stack, read from the host (paths come from compose's labels).
    pub async fn stack_file(&self, project: &str, host_root: Option<&str>) -> Result<Value> {
        let cs = self.stack_containers(project).await?;
        let labels = cs.first().and_then(|c| c.labels.clone()).ok_or_else(|| anyhow!("stack {project} not found"))?;
        let files = labels.get("com.docker.compose.project.config_files").cloned().unwrap_or_default();
        let dir = labels.get("com.docker.compose.project.working_dir").cloned().unwrap_or_default();
        let mut out = Vec::new();
        let mut names: Vec<String> = files.split(',').map(|f| f.trim().to_string()).filter(|f| !f.is_empty()).collect();
        // .env next to the compose file: list it but never send values.
        let env_path = if dir.is_empty() { None } else { Some(format!("{}/.env", dir.trim_end_matches('/'))) };
        names.dedup();
        for f in names {
            let full = if f.starts_with('/') || dir.is_empty() { f.clone() } else { format!("{}/{f}", dir.trim_end_matches('/')) };
            let local = crate::util::host_path(host_root, &full);
            let content = match std::fs::metadata(&local) {
                Ok(m) if m.len() > 512 * 1024 => Err("file is larger than 512 KB".to_string()),
                Ok(_) => std::fs::read_to_string(&local).map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            };
            out.push(match content {
                Ok(c) => json!({ "path": full, "content": c }),
                Err(e) => json!({ "path": full, "error": e }),
            });
        }
        let env_keys: Option<Vec<String>> = env_path.as_ref().and_then(|p| std::fs::read_to_string(crate::util::host_path(host_root, p)).ok()).map(|t| {
            t.lines()
                .filter_map(|l| {
                    let l = l.trim();
                    if l.is_empty() || l.starts_with('#') { None } else { l.split_once('=').map(|(k, _)| k.trim().to_string()) }
                })
                .collect()
        });
        Ok(json!({ "project": project, "working_dir": dir, "files": out, "env_file": env_path, "env_keys": env_keys }))
    }

    pub async fn stack_action(&self, project: &str, action: StackAction, host_root: Option<&str>) -> Result<Value> {
        let cs = self.stack_containers(project).await?;
        if cs.is_empty() {
            bail!("stack {project} has no containers");
        }
        let mut results = Vec::new();
        let mut failed = 0;
        // Pull each distinct image once, then recreate.
        let mut pulled: HashSet<String> = HashSet::new();
        for c in &cs {
            let id = c.id.clone().unwrap_or_default();
            let name = c.names.as_ref().and_then(|n| n.first().cloned()).unwrap_or_default().trim_start_matches('/').to_string();
            let running = c.state.as_deref() == Some("running");
            let r: Result<Value> = match action {
                StackAction::Start if !running => self.action(&id, ContainerAction::Start).await,
                StackAction::Start => Ok(json!({ "skipped": "already running" })),
                StackAction::Stop if running => self.action(&id, ContainerAction::Stop).await,
                StackAction::Stop => Ok(json!({ "skipped": "not running" })),
                StackAction::Restart => self.action(&id, ContainerAction::Restart).await,
                StackAction::Redeploy => self.recreate(&id, false, host_root).await,
                StackAction::PullRedeploy => {
                    let image = c.image.clone().unwrap_or_default();
                    if !image.is_empty() && !image.contains('@') && !image.starts_with("sha256:") && pulled.insert(image.clone()) {
                        if let Err(e) = self.pull(&image, host_root).await {
                            failed += 1;
                            results.push(json!({ "container": name, "ok": false, "error": e.to_string() }));
                            continue;
                        }
                    }
                    self.recreate(&id, false, host_root).await.map(|mut v| {
                        v["pulled"] = json!(true);
                        v
                    })
                }
            };
            match r {
                Ok(v) => results.push(json!({ "container": name, "ok": true, "result": v })),
                Err(e) => {
                    failed += 1;
                    results.push(json!({ "container": name, "ok": false, "error": e.to_string() }));
                }
            }
        }
        Ok(json!({ "ok": failed == 0, "failed": failed, "results": results }))
    }

    /// Logs of every container in a stack merged into one stream (each line tagged with its container).
    pub async fn stream_stack_logs(&self, project: &str, tail: u32, since: Option<i64>, sid: u64, out: &mpsc::Sender<AgentMsg>) -> Result<()> {
        let cs = self.stack_containers(project).await?;
        if cs.is_empty() {
            bail!("stack {project} has no containers");
        }
        let (tx, mut rx) = mpsc::channel::<LogLine>(4096);
        let mut handles = Vec::new();
        for c in cs {
            let id = c.id.clone().unwrap_or_default();
            let name = c.names.and_then(|n| n.first().cloned()).unwrap_or_default().trim_start_matches('/').to_string();
            let (d, tx) = (self.d.clone(), tx.clone());
            handles.push(tokio::spawn(async move {
                let opts = LogsOptions::<String> {
                    follow: true,
                    stdout: true,
                    stderr: true,
                    timestamps: true,
                    since: since.unwrap_or(0),
                    until: 0,
                    tail: if since.is_some() { "all".into() } else { tail.min(20_000).to_string() },
                };
                let mut st = d.logs(&id, Some(opts));
                let mut partial: [String; 3] = Default::default();
                while let Some(Ok(o)) = st.next().await {
                    let (s, bytes) = match o {
                        LogOutput::StdOut { message } | LogOutput::Console { message } => (1u8, message),
                        LogOutput::StdErr { message } => (2u8, message),
                        LogOutput::StdIn { .. } => continue,
                    };
                    let buf = &mut partial[s as usize];
                    buf.push_str(&String::from_utf8_lossy(&bytes));
                    while let Some(idx) = buf.find('\n') {
                        let line: String = buf.drain(..=idx).collect();
                        let mut l = parse_log_line(line.trim_end_matches('\n'), s);
                        l.src = Some(name.clone());
                        if tx.send(l).await.is_err() {
                            return;
                        }
                    }
                }
            }));
        }
        drop(tx);
        struct Abort(Vec<tokio::task::JoinHandle<()>>);
        impl Drop for Abort {
            fn drop(&mut self) {
                for h in &self.0 {
                    h.abort();
                }
            }
        }
        let _guard = Abort(handles);
        // Let every container's backlog arrive, then send it in time order.
        let mut batch: Vec<LogLine> = Vec::new();
        let deadline = tokio::time::sleep(Duration::from_millis(700));
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                l = rx.recv() => match l {
                    Some(l) => batch.push(l),
                    None => break,
                },
                _ = &mut deadline => break,
            }
        }
        batch.sort_by(|a, b| a.ts.cmp(&b.ts));
        for chunk in batch.chunks(1000) {
            out.send(AgentMsg::Stream { sid, data: StreamData::Lines { lines: chunk.to_vec() } }).await.map_err(|_| anyhow!("closed"))?;
        }
        let mut batch = Vec::new();
        let mut tick = tokio::time::interval(Duration::from_millis(150));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                l = rx.recv() => match l {
                    Some(l) => {
                        batch.push(l);
                        if batch.len() >= 500 {
                            batch.sort_by(|a: &LogLine, b: &LogLine| a.ts.cmp(&b.ts));
                            flush(out, sid, &mut batch).await?;
                        }
                    }
                    None => {
                        flush(out, sid, &mut batch).await?;
                        return Ok(());
                    }
                },
                _ = tick.tick() => {
                    if !batch.is_empty() {
                        batch.sort_by(|a: &LogLine, b: &LogLine| a.ts.cmp(&b.ts));
                        flush(out, sid, &mut batch).await?;
                    }
                }
            }
        }
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

/// Settings of the "Add container" form.
pub struct NewContainer {
    pub image: String,
    pub name: Option<String>,
    pub ports: Vec<String>,
    pub env: Vec<String>,
    pub volumes: Vec<String>,
    pub restart: Option<String>,
    pub network: Option<String>,
    pub cmd: Option<Vec<String>>,
    pub privileged: bool,
    pub start: bool,
    pub pull: bool,
}

async fn flush(out: &mpsc::Sender<AgentMsg>, sid: u64, batch: &mut Vec<LogLine>) -> Result<()> {
    if batch.is_empty() {
        return Ok(());
    }
    let lines = std::mem::take(batch);
    out.send(AgentMsg::Stream { sid, data: StreamData::Lines { lines } }).await.map_err(|_| anyhow!("closed"))
}

/// Split Docker's `timestamps=true` prefix (`2024-01-02T03:04:05.123456789Z `) from the line.
pub(crate) fn parse_log_line(line: &str, s: u8) -> LogLine {
    let line = line.strip_suffix('\r').unwrap_or(line);
    if let Some((ts, rest)) = line.split_once(' ') {
        let b = ts.as_bytes();
        if b.len() >= 20 && b[4] == b'-' && b[10] == b'T' && b[0].is_ascii_digit() {
            return LogLine { ts: Some(ts.to_string()), s, text: truncate(rest.to_string(), MAX_LOG_LINE), ..Default::default() };
        }
    }
    LogLine { ts: None, s, text: truncate(line.to_string(), MAX_LOG_LINE), ..Default::default() }
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
        let l = parse_log_line("2024-05-01T10:11:12.123456789Z hello world", 1);
        assert_eq!(l.ts.as_deref(), Some("2024-05-01T10:11:12.123456789Z"));
        assert_eq!(l.text, "hello world");
        let l = parse_log_line("plain line\r", 2);
        assert!(l.ts.is_none());
        assert_eq!(l.text, "plain line");
        assert_eq!(l.s, 2);
    }
}

fn valid_container_name(n: &str) -> bool {
    let mut ch = n.chars();
    matches!(ch.next(), Some(c) if c.is_ascii_alphanumeric())
        && n.len() <= 128
        && n.chars().all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
}

/// `repo[:tag]` → (repo, tag); a `:` before the last `/` is a registry port, not a tag.
pub fn split_reference(r: &str) -> (&str, &str) {
    let last_slash = r.rfind('/').map(|i| i + 1).unwrap_or(0);
    match r[last_slash..].rfind(':') {
        Some(i) => (&r[..last_slash + i], &r[last_slash + i + 1..]),
        None => (r, "latest"),
    }
}

/// Registry host of an image reference ("docker.io" for Docker Hub images).
pub fn registry_of(r: &str) -> &str {
    match r.split_once('/') {
        Some((first, _)) if first.contains('.') || first.contains(':') || first == "localhost" => first,
        _ => "docker.io",
    }
}

/// Strip runtime-assigned fields so the endpoint can be reused on a new container.
fn clean_endpoint(ep: bollard::models::EndpointSettings, short_id: &str) -> bollard::models::EndpointSettings {
    bollard::models::EndpointSettings {
        ipam_config: ep.ipam_config,
        links: ep.links,
        aliases: ep.aliases.map(|a| a.into_iter().filter(|x| x != short_id).collect()),
        driver_opts: ep.driver_opts,
        ..Default::default()
    }
}

/// True when `id` is the container this process runs in (the hub's built-in agent).
fn is_own_container(id: &str) -> bool {
    #[cfg(target_os = "linux")]
    {
        if let Ok(h) = std::fs::read_to_string("/etc/hostname") {
            let h = h.trim();
            if h.len() >= 12 && id.starts_with(h) {
                return true;
            }
        }
        if let Ok(m) = std::fs::read_to_string("/proc/self/mountinfo") {
            if id.len() >= 32 && m.contains(&format!("/containers/{id}/")) {
                return true;
            }
        }
    }
    let _ = id;
    false
}

#[cfg(test)]
mod ref_tests {
    use super::*;

    #[test]
    fn references() {
        assert_eq!(split_reference("nginx"), ("nginx", "latest"));
        assert_eq!(split_reference("nginx:1.27-alpine"), ("nginx", "1.27-alpine"));
        assert_eq!(split_reference("localhost:5000/app"), ("localhost:5000/app", "latest"));
        assert_eq!(split_reference("us-central1-docker.pkg.dev/p/r/app:v2"), ("us-central1-docker.pkg.dev/p/r/app", "v2"));
        assert_eq!(registry_of("nginx"), "docker.io");
        assert_eq!(registry_of("library/nginx:1"), "docker.io");
        assert_eq!(registry_of("ghcr.io/a/b"), "ghcr.io");
        assert_eq!(registry_of("localhost:5000/app"), "localhost:5000");
        assert!(valid_container_name("api-devpilot_2.test"));
        assert!(!valid_container_name("-bad"));
        assert!(!valid_container_name("bad name"));
    }
}
