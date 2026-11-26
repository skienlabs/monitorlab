//! An interactive shell on the server itself, over a pseudo-terminal (ConPTY on Windows).
//!
//! The hub only forwards this to users with the "Server shell" permission and audits every session.
//! The shell runs as the account the agent runs as: the `monitorlab` service user on Linux, root on a
//! Mac, and LocalSystem on Windows. Only a fixed list of shells can be requested.

use std::io::{Read, Write};
use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use monitorlab_common::{AgentMsg, StreamData};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use tokio::sync::mpsc;

use crate::session::StreamCtl;
use crate::util::b64;

/// (program, arguments) for a requested shell, or an error for anything not on the list.
pub fn resolve(shell: Option<&str>) -> Result<(PathBuf, Vec<&'static str>)> {
    let want = shell.map(str::trim).filter(|s| !s.is_empty()).unwrap_or("auto").to_ascii_lowercase();
    if cfg!(windows) {
        let sys = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        let ps = PathBuf::from(&sys).join("System32\\WindowsPowerShell\\v1.0\\powershell.exe");
        return match want.as_str() {
            "auto" | "powershell" => Ok((ps, vec!["-NoLogo"])),
            "pwsh" => Ok((find_in_path("pwsh.exe").ok_or_else(|| anyhow!("PowerShell 7 (pwsh) is not installed on this server"))?, vec!["-NoLogo"])),
            "cmd" => Ok((PathBuf::from(&sys).join("System32\\cmd.exe"), vec![])),
            other => bail!("unsupported shell '{other}' (use powershell, pwsh or cmd)"),
        };
    }
    let dirs = ["/bin", "/usr/bin", "/usr/local/bin", "/opt/homebrew/bin"];
    let find = |name: &str| dirs.iter().map(|d| PathBuf::from(d).join(name)).find(|p| p.exists());
    let pick = match want.as_str() {
        "auto" => {
            let order: &[&str] = if cfg!(target_os = "macos") { &["zsh", "bash", "sh"] } else { &["bash", "sh"] };
            order.iter().find_map(|n| find(n))
        }
        "bash" | "sh" | "zsh" => find(&want),
        other => bail!("unsupported shell '{other}' (use bash, sh or zsh)"),
    };
    let p = pick.ok_or_else(|| anyhow!("that shell is not installed on this server"))?;
    let is_sh = p.file_name().map(|n| n == "sh").unwrap_or(false);
    Ok((p, if is_sh { vec![] } else { vec!["-i"] }))
}

#[cfg(windows)]
fn find_in_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join(name)).find(|f| f.exists()))
}
#[cfg(not(windows))]
fn find_in_path(_: &str) -> Option<PathBuf> {
    None
}

struct KillOnDrop(Box<dyn portable_pty::ChildKiller + Send + Sync>);
impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
    }
}

pub async fn run(shell: Option<String>, cols: u16, rows: u16, host: String, sid: u64, out: &mpsc::Sender<AgentMsg>, mut ctl: mpsc::Receiver<StreamCtl>) -> Result<()> {
    let (prog, args) = resolve(shell.as_deref())?;
    let size = |c: u16, r: u16| PtySize { rows: r.clamp(5, 300), cols: c.clamp(10, 500), pixel_width: 0, pixel_height: 0 };
    let pair = native_pty_system().openpty(size(cols, rows)).context("opening a terminal")?;
    let mut cmd = CommandBuilder::new(&prog);
    for a in args {
        cmd.arg(a);
    }
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    if !cfg!(windows) {
        let home = std::env::var("HOME").ok().filter(|h| std::path::Path::new(h).is_dir());
        if let Some(h) = &home {
            cmd.cwd(h);
        } else {
            cmd.env("HOME", "/tmp");
            cmd.cwd("/tmp");
        }
        cmd.env("PS1", r"\u@\h:\w\$ ");
    }
    let mut child = pair.slave.spawn_command(cmd).with_context(|| format!("starting {}", prog.display()))?;
    let _kill = KillOnDrop(child.clone_killer());
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().context("terminal reader")?;
    let mut writer = pair.master.take_writer().context("terminal writer")?;
    let master = pair.master;

    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(64);
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.blocking_send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let hello = format!("\x1b[2mMonitorLab server shell on {host} · {} · this session is audited\x1b[0m\r\n", prog.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default());
    out.send(AgentMsg::Stream { sid, data: StreamData::Bytes { b64: b64(hello.as_bytes()) } }).await.map_err(|_| anyhow!("closed"))?;

    loop {
        tokio::select! {
            data = rx.recv() => match data {
                Some(d) => out.send(AgentMsg::Stream { sid, data: StreamData::Bytes { b64: b64(&d) } }).await.map_err(|_| anyhow!("closed"))?,
                None => break, // the shell exited
            },
            c = ctl.recv() => match c {
                Some(StreamCtl::Input(d)) => {
                    writer.write_all(&d).and_then(|_| writer.flush()).context("writing to the shell")?;
                }
                Some(StreamCtl::Resize(c, r)) => {
                    let _ = master.resize(size(c, r));
                }
                None => break,
            },
        }
    }
    let status = tokio::task::spawn_blocking(move || child.wait()).await.ok().and_then(|r| r.ok());
    if let Some(s) = status {
        let msg = format!("\r\n\x1b[2m[shell exited with code {}]\x1b[0m\r\n", s.exit_code());
        let _ = out.send(AgentMsg::Stream { sid, data: StreamData::Bytes { b64: b64(msg.as_bytes()) } }).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_known_shells_are_accepted() {
        assert!(super::resolve(Some("rm -rf /")).is_err());
        assert!(super::resolve(Some("/bin/bash")).is_err());
        assert!(super::resolve(Some("fish")).is_err());
        #[cfg(unix)]
        assert!(super::resolve(None).is_ok());
    }
}
