//! Self-update: download a new agent binary from the hub (over the same pinned TLS as the
//! agent connection), verify its SHA-256 and that it runs, swap it in, then exit so the
//! service manager (systemd `Restart=always` / Windows recovery actions) starts the new one.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use futures_util::StreamExt;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone)]
pub struct UpdateSource {
    pub hub_url: String,
    pub fingerprint: Option<String>,
}

const MAX_SIZE: u64 = 96 * 1024 * 1024;

fn http_base(hub: &str) -> String {
    let h = hub.trim().trim_end_matches('/');
    if let Some(r) = h.strip_prefix("wss://") {
        format!("https://{r}")
    } else if let Some(r) = h.strip_prefix("ws://") {
        format!("http://{r}")
    } else {
        h.to_string()
    }
}

fn sibling(exe: &Path, suffix: &str) -> PathBuf {
    let mut s = exe.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

pub async fn self_update(src: &UpdateSource, file: &str, sha256: &str, version: &str) -> Result<Value> {
    if !crate::util::is_plain_file_name(file) || !file.starts_with("monitorlab-agent-") {
        bail!("invalid update file name");
    }
    if version == monitorlab_common::VERSION {
        return Ok(json!({ "ok": true, "already": true }));
    }
    let exe = std::env::current_exe()?.canonicalize()?;
    let dir = exe.parent().ok_or_else(|| anyhow!("binary has no directory"))?;
    let new = sibling(&exe, ".new");
    // Fail early (before downloading) when the binary's directory is not writable.
    std::fs::write(&new, b"").map_err(|e| {
        anyhow!(
            "cannot write next to {} ({e}). Agents installed before v1.3 keep their binary in a root-owned directory: re-run the installer once on this server, later updates are automatic.",
            exe.display()
        )
    })?;

    let mut b = reqwest::Client::builder().no_proxy().connect_timeout(Duration::from_secs(10)).timeout(Duration::from_secs(600));
    if src.hub_url.starts_with("https://") || src.hub_url.starts_with("wss://") {
        b = b.use_preconfigured_tls(crate::conn::tls_config(src.fingerprint.as_deref())?);
    }
    let url = format!("{}/api/download/{file}", http_base(&src.hub_url));
    let res = b.build()?.get(&url).send().await.context("downloading the update")?;
    if !res.status().is_success() {
        let _ = std::fs::remove_file(&new);
        bail!("hub returned {} for {file}", res.status());
    }
    let mut hasher = Sha256::new();
    let mut f = tokio::fs::File::create(&new).await?;
    let mut size = 0u64;
    let mut st = res.bytes_stream();
    use tokio::io::AsyncWriteExt;
    while let Some(chunk) = st.next().await {
        let chunk = chunk?;
        size += chunk.len() as u64;
        if size > MAX_SIZE {
            drop(f);
            let _ = std::fs::remove_file(&new);
            bail!("update is larger than expected");
        }
        hasher.update(&chunk);
        f.write_all(&chunk).await?;
    }
    f.sync_all().await?;
    drop(f);
    let got = hex::encode(hasher.finalize());
    if !got.eq_ignore_ascii_case(sha256) {
        let _ = std::fs::remove_file(&new);
        bail!("checksum mismatch (expected {sha256}, got {got})");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&new, std::fs::Permissions::from_mode(0o755))?;
    }
    // The new binary must start and report the expected version.
    let out = tokio::time::timeout(Duration::from_secs(15), tokio::process::Command::new(&new).arg("--version").output())
        .await
        .map_err(|_| anyhow!("new binary did not answer --version"))??;
    let reported = String::from_utf8_lossy(&out.stdout).to_string();
    if !out.status.success() || !reported.contains(version) {
        let _ = std::fs::remove_file(&new);
        bail!("new binary reported '{}' instead of {version}", reported.trim());
    }
    #[cfg(windows)]
    {
        // A running .exe can be renamed but not overwritten.
        let old = sibling(&exe, ".old");
        let _ = std::fs::remove_file(&old);
        std::fs::rename(&exe, &old).context("moving the running binary aside")?;
        if let Err(e) = std::fs::rename(&new, &exe) {
            let _ = std::fs::rename(&old, &exe);
            bail!("installing the new binary: {e}");
        }
    }
    #[cfg(not(windows))]
    std::fs::rename(&new, &exe).context("installing the new binary")?;
    let _ = dir;
    tracing::info!("updated to {version}; restarting");
    tokio::spawn(async {
        // Let the reply reach the hub first.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        std::process::exit(if cfg!(windows) { 1 } else { 0 });
    });
    Ok(json!({ "ok": true, "from": monitorlab_common::VERSION, "to": version, "bytes": size }))
}

/// Remove the previous binary left by a Windows update.
pub fn cleanup() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(sibling(&exe, ".old"));
        let _ = std::fs::remove_file(sibling(&exe, ".new"));
    }
}
