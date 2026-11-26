//! `monitorlab-agent install` / `uninstall`: enroll with the hub and register a system service.

#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;

#[cfg(unix)]
use anyhow::bail;
use anyhow::{Context, Result};

use crate::config::{default_config_path, AgentConfig, DockerConfig, IisConfig, K8sConfig, MssqlConfig};

pub struct InstallArgs {
    pub hub: String,
    pub token: String,
    pub fingerprint: Option<String>,
    pub config: Option<PathBuf>,
}

/// Enroll and write the config file. Returns the config path.
pub async fn enroll_and_save(args: &InstallArgs) -> Result<PathBuf> {
    let path = args.config.clone().unwrap_or_else(default_config_path);
    if args.token.is_empty() {
        // Upgrade: keep the existing enrollment and only reinstall the binary and service.
        anyhow::ensure!(path.exists(), "no existing configuration at {}; this server has not been installed yet", path.display());
        AgentConfig::load(&path).context("the existing configuration is not readable")?;
        println!("Keeping the existing enrollment ({}).", path.display());
        return Ok(path);
    }
    let (agent_id, secret, name) = crate::conn::enroll(&args.hub, &args.token, args.fingerprint.as_deref()).await?;
    let cfg = AgentConfig {
        hub_url: args.hub.trim_end_matches('/').to_string(),
        agent_id,
        secret,
        fingerprint: args.fingerprint.clone().filter(|f| !f.trim().is_empty()),
        interval_secs: 5,
        docker: DockerConfig::default(),
        mssql: MssqlConfig::default(),
        postgres: Default::default(),
        shell: Default::default(),
        iis: IisConfig::default(),
        kubernetes: K8sConfig::default(),
        host_root: None,
    };
    cfg.save(&path).with_context(|| format!("writing {}", path.display()))?;
    println!("Enrolled as '{name}'. Configuration written to {}", path.display());
    Ok(path)
}

// ======================================================================= Linux (systemd)

#[cfg(target_os = "linux")]
const UNIT_PATH: &str = "/etc/systemd/system/monitorlab-agent.service";
/// The agent owns its binary directory so it can update itself when the hub asks.
#[cfg(target_os = "linux")]
const BIN_DIR: &str = "/opt/monitorlab/bin";
#[cfg(target_os = "linux")]
const BIN_PATH: &str = "/opt/monitorlab/bin/monitorlab-agent";
#[cfg(target_os = "linux")]
const BIN_LINK: &str = "/usr/local/bin/monitorlab-agent";
#[cfg(target_os = "linux")]
const POLKIT_RULE: &str = "/etc/polkit-1/rules.d/50-monitorlab-agent.rules";
#[cfg(target_os = "linux")]
const SERVICE_USER: &str = "monitorlab";
#[cfg(target_os = "linux")]
const K3S_KUBECONFIG: &str = "/etc/rancher/k3s/k3s.yaml";

#[cfg(unix)]
fn run(cmd: &str, args: &[&str]) -> Result<std::process::Output> {
    std::process::Command::new(cmd).args(args).output().with_context(|| format!("running {cmd}"))
}

#[cfg(target_os = "linux")]
fn group_exists(g: &str) -> bool {
    run("getent", &["group", g]).map(|o| o.status.success()).unwrap_or(false)
}

#[cfg(unix)]
fn is_root() -> bool {
    run("id", &["-u"]).map(|o| String::from_utf8_lossy(&o.stdout).trim() == "0").unwrap_or(false)
}

#[cfg(target_os = "linux")]
pub async fn install(args: InstallArgs) -> Result<()> {
    if !is_root() {
        bail!("install must be run as root (use sudo)");
    }
    let cfg_path = enroll_and_save(&args).await?;

    // Dedicated unprivileged user with access to docker / mssql groups if present.
    if !run("id", &["-u", SERVICE_USER])?.status.success() {
        let nologin = if Path::new("/usr/sbin/nologin").exists() { "/usr/sbin/nologin" } else { "/bin/false" };
        let o = run("useradd", &["--system", "--no-create-home", "--shell", nologin, SERVICE_USER])?;
        if !o.status.success() {
            bail!("useradd failed: {}", String::from_utf8_lossy(&o.stderr));
        }
    }
    let groups: Vec<&str> = ["docker", "mssql", "systemd-journal"].into_iter().filter(|g| group_exists(g)).collect();
    let svc_uid: u32 = String::from_utf8_lossy(&run("id", &["-u", SERVICE_USER])?.stdout).trim().parse()?;
    let svc_gid: u32 = String::from_utf8_lossy(&run("id", &["-g", SERVICE_USER])?.stdout).trim().parse()?;

    // Binary in a directory owned by the service user (self-update), plus a link on the PATH.
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(BIN_DIR)?;
        std::os::unix::fs::chown(BIN_DIR, Some(svc_uid), Some(svc_gid))?;
        std::fs::set_permissions(BIN_DIR, std::fs::Permissions::from_mode(0o755))?;
        let me = std::env::current_exe()?;
        if me != Path::new(BIN_PATH) {
            let tmp = format!("{BIN_PATH}.new");
            std::fs::copy(&me, &tmp).context("copying binary")?;
            std::fs::rename(&tmp, BIN_PATH)?;
        }
        std::fs::set_permissions(BIN_PATH, std::fs::Permissions::from_mode(0o755))?;
        std::os::unix::fs::chown(BIN_PATH, Some(svc_uid), Some(svc_gid))?;
        let link = Path::new(BIN_LINK);
        if link.exists() || link.is_symlink() {
            let _ = std::fs::remove_file(link);
        }
        std::os::unix::fs::symlink(BIN_PATH, link).context("linking the binary into /usr/local/bin")?;
    }

    // Allow the agent to start / stop / restart services through systemd (polkit).
    #[cfg(target_os = "linux")]
    if Path::new("/etc/polkit-1/rules.d").is_dir() {
        let rule = format!(
            "// Installed by monitorlab-agent: lets the agent manage systemd services when you use MonitorLab.\n\
polkit.addRule(function(action, subject) {{\n\
  if ((action.id == \"org.freedesktop.systemd1.manage-units\" ||\n\
       action.id == \"org.freedesktop.systemd1.manage-unit-files\" ||\n\
       action.id == \"org.freedesktop.systemd1.reload-daemon\") && subject.user == \"{SERVICE_USER}\") {{\n\
    return polkit.Result.YES;\n\
  }}\n\
}});\n"
        );
        std::fs::write(POLKIT_RULE, rule)?;
    }

    // Config readable only by the service user.
    let uid: u32 = String::from_utf8_lossy(&run("id", &["-u", SERVICE_USER])?.stdout).trim().parse()?;
    let gid: u32 = String::from_utf8_lossy(&run("id", &["-g", SERVICE_USER])?.stdout).trim().parse()?;
    std::os::unix::fs::chown(&cfg_path, Some(uid), Some(gid))?;
    if let Some(dir) = cfg_path.parent() {
        std::os::unix::fs::chown(dir, Some(0), Some(gid))?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o750))?;
    }

    // k3s: its kubeconfig is root-only, so give the agent its own copy.
    #[cfg(target_os = "linux")]
    if Path::new(K3S_KUBECONFIG).exists() {
        if let Some(dir) = cfg_path.parent() {
            let dest = dir.join("kubeconfig");
            std::fs::copy(K3S_KUBECONFIG, &dest).context("copying the k3s kubeconfig")?;
            std::os::unix::fs::chown(&dest, Some(uid), Some(gid))?;
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o600))?;
            let mut cfg = AgentConfig::load(&cfg_path)?;
            cfg.kubernetes.kubeconfig = Some(dest.display().to_string());
            cfg.save(&cfg_path)?;
            std::os::unix::fs::chown(&cfg_path, Some(uid), Some(gid))?;
            println!("k3s detected: Kubernetes monitoring enabled (kubeconfig copied to {}).", dest.display());
        }
    }

    let supplementary = if groups.is_empty() { String::new() } else { format!("SupplementaryGroups={}\n", groups.join(" ")) };
    let unit = format!(
        "[Unit]\n\
Description=MonitorLab monitoring agent\n\
After=network-online.target docker.service\n\
Wants=network-online.target\n\n\
[Service]\n\
ExecStart={BIN_PATH} run --config {cfg}\n\
User={SERVICE_USER}\n\
Group={SERVICE_USER}\n\
{supplementary}\
Restart=always\n\
RestartSec=5\n\
Nice=5\n\
NoNewPrivileges=true\n\
ProtectSystem=full\n\
ProtectHome=read-only\n\
PrivateTmp=true\n\
MemoryHigh=96M\n\
MemoryMax=256M\n\n\
[Install]\n\
WantedBy=multi-user.target\n",
        cfg = cfg_path.display()
    );
    std::fs::write(UNIT_PATH, unit)?;
    run("systemctl", &["daemon-reload"])?;
    let o = run("systemctl", &["enable", "--now", "monitorlab-agent"])?;
    if !o.status.success() {
        bail!("systemctl enable failed: {}", String::from_utf8_lossy(&o.stderr));
    }
    let _ = run("systemctl", &["restart", "monitorlab-agent"]);
    println!("Service monitorlab-agent installed and started (groups: {}).", if groups.is_empty() { "none".into() } else { groups.join(", ") });
    println!("Check status with: systemctl status monitorlab-agent   |   logs: journalctl -u monitorlab-agent -f");
    Ok(())
}

#[cfg(target_os = "linux")]
pub fn uninstall(purge: bool) -> Result<()> {
    if !is_root() {
        bail!("uninstall must be run as root (use sudo)");
    }
    let _ = run("systemctl", &["disable", "--now", "monitorlab-agent"]);
    let _ = std::fs::remove_file(UNIT_PATH);
    let _ = run("systemctl", &["daemon-reload"]);
    if purge {
        let cfg = default_config_path();
        let _ = std::fs::remove_file(&cfg);
        if let Some(dir) = cfg.parent() {
            let _ = std::fs::remove_file(dir.join("kubeconfig"));
            let _ = std::fs::remove_dir(dir); // only succeeds when empty
        }
        match run("userdel", &[SERVICE_USER]) {
            Ok(o) if o.status.success() => {}
            Ok(o) => eprintln!("warning: could not remove user '{SERVICE_USER}': {}", String::from_utf8_lossy(&o.stderr).trim()),
            Err(e) => eprintln!("warning: could not remove user '{SERVICE_USER}': {e}"),
        }
        let _ = std::fs::remove_file(BIN_PATH);
        let _ = std::fs::remove_file(BIN_LINK);
        let _ = std::fs::remove_dir(BIN_DIR);
        #[cfg(target_os = "linux")]
        let _ = std::fs::remove_file(POLKIT_RULE);
    }
    println!("monitorlab-agent service removed{}.", if purge { " (config, user and binary purged)" } else { "" });
    Ok(())
}

// ======================================================================= macOS (launchd)

#[cfg(target_os = "macos")]
const PLIST_PATH: &str = "/Library/LaunchDaemons/com.skien.monitorlab-agent.plist";
#[cfg(target_os = "macos")]
const MAC_BIN_DIR: &str = "/usr/local/lib/monitorlab";
#[cfg(target_os = "macos")]
const MAC_BIN_PATH: &str = "/usr/local/lib/monitorlab/monitorlab-agent";
#[cfg(target_os = "macos")]
const MAC_BIN_LINK: &str = "/usr/local/bin/monitorlab-agent";
#[cfg(target_os = "macos")]
const MAC_LABEL: &str = "com.skien.monitorlab-agent";

/// macOS: a LaunchDaemon that runs as root (it must reach the Docker socket and read system metrics).
/// launchd restarts it whenever it exits, which is also how self-update takes effect.
#[cfg(target_os = "macos")]
pub async fn install(args: InstallArgs) -> Result<()> {
    if !is_root() {
        bail!("install must be run as root (use sudo)");
    }
    let cfg_path = enroll_and_save(&args).await?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(MAC_BIN_DIR)?;
        std::fs::set_permissions(MAC_BIN_DIR, std::fs::Permissions::from_mode(0o755))?;
        let me = std::env::current_exe()?;
        if me != Path::new(MAC_BIN_PATH) {
            let tmp = format!("{MAC_BIN_PATH}.new");
            std::fs::copy(&me, &tmp).context("copying binary")?;
            std::fs::rename(&tmp, MAC_BIN_PATH)?;
        }
        std::fs::set_permissions(MAC_BIN_PATH, std::fs::Permissions::from_mode(0o755))?;
        // Files fetched with curl carry no quarantine flag, but clear it in case the binary came from a browser.
        let _ = run("xattr", &["-d", "com.apple.quarantine", MAC_BIN_PATH]);
        std::fs::create_dir_all("/usr/local/bin")?;
        let link = Path::new(MAC_BIN_LINK);
        if link.exists() || link.is_symlink() {
            let _ = std::fs::remove_file(link);
        }
        std::os::unix::fs::symlink(MAC_BIN_PATH, link).context("linking the binary into /usr/local/bin")?;
    }
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{MAC_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{MAC_BIN_PATH}</string>
    <string>run</string>
    <string>--config</string>
    <string>{cfg}</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ThrottleInterval</key><integer>5</integer>
  <key>ProcessType</key><string>Background</string>
  <key>StandardOutPath</key><string>/var/log/monitorlab-agent.log</string>
  <key>StandardErrorPath</key><string>/var/log/monitorlab-agent.log</string>
</dict>
</plist>
"#,
        cfg = cfg_path.display()
    );
    std::fs::write(PLIST_PATH, plist)?;
    run("chown", &["root:wheel", PLIST_PATH])?;
    std::fs::set_permissions(PLIST_PATH, <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o644))?;
    // Replace a previous install, then load the new one.
    let _ = run("launchctl", &["bootout", &format!("system/{MAC_LABEL}")]);
    let o = run("launchctl", &["bootstrap", "system", PLIST_PATH])?;
    if !o.status.success() {
        bail!("launchctl bootstrap failed: {}", String::from_utf8_lossy(&o.stderr).trim());
    }
    let _ = run("launchctl", &["kickstart", "-k", &format!("system/{MAC_LABEL}")]);
    println!("Service {MAC_LABEL} installed and started.");
    println!("Logs: tail -f /var/log/monitorlab-agent.log   |   status: sudo launchctl print system/{MAC_LABEL}");
    println!("Docker: the agent looks for /var/run/docker.sock, then the socket Docker Desktop / OrbStack / Colima keep in a user's home folder.");
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn uninstall(purge: bool) -> Result<()> {
    if !is_root() {
        bail!("uninstall must be run as root (use sudo)");
    }
    let _ = run("launchctl", &["bootout", &format!("system/{MAC_LABEL}")]);
    let _ = std::fs::remove_file(PLIST_PATH);
    if purge {
        let cfg = default_config_path();
        let _ = std::fs::remove_file(&cfg);
        if let Some(dir) = cfg.parent() {
            let _ = std::fs::remove_dir(dir);
        }
        let _ = std::fs::remove_file(MAC_BIN_PATH);
        let _ = std::fs::remove_file(MAC_BIN_LINK);
        let _ = std::fs::remove_dir(MAC_BIN_DIR);
        let _ = std::fs::remove_file("/var/log/monitorlab-agent.log");
    }
    println!("monitorlab-agent service removed{}.", if purge { " (config and binary purged)" } else { "" });
    Ok(())
}

// ======================================================================= Windows (SCM)

#[cfg(windows)]
pub async fn install(args: InstallArgs) -> Result<()> {
    use std::ffi::OsString;
    use windows_service::service::{ServiceAccess, ServiceErrorControl, ServiceInfo, ServiceStartType, ServiceType};
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE)
        .context("opening the service manager (run PowerShell as Administrator)")?;

    let cfg_path = enroll_and_save(&args).await?;
    if let Some(dir) = cfg_path.parent() {
        // Restrict the config directory (it contains the agent secret) to SYSTEM and Administrators.
        let _ = std::process::Command::new("icacls")
            .arg(dir)
            .args(["/inheritance:r", "/grant:r", "*S-1-5-18:(OI)(CI)F", "*S-1-5-32-544:(OI)(CI)F"])
            .output();
    }

    let program_files = std::env::var("ProgramFiles").unwrap_or_else(|_| "C:\\Program Files".into());
    let dir = PathBuf::from(program_files).join("MonitorLab");
    std::fs::create_dir_all(&dir)?;
    let target = dir.join("monitorlab-agent.exe");
    let me = std::env::current_exe()?;
    if me != target {
        // If an older service is running it holds the file; stop it first.
        uninstall_service_only();
        std::fs::copy(&me, &target).context("copying monitorlab-agent.exe")?;
    }

    let info = ServiceInfo {
        name: OsString::from(crate::winsvc::SERVICE_NAME),
        display_name: OsString::from("MonitorLab Monitoring Agent"),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: target.clone(),
        launch_arguments: vec![OsString::from("service"), OsString::from("--config"), cfg_path.clone().into_os_string()],
        dependencies: vec![],
        account_name: None, // LocalSystem: needed to read IIS config and SQL Server logs
        account_password: None,
    };
    let service = match manager.create_service(&info, ServiceAccess::CHANGE_CONFIG | ServiceAccess::START) {
        Ok(s) => s,
        Err(_) => {
            uninstall_service_only();
            std::thread::sleep(std::time::Duration::from_secs(2));
            manager.create_service(&info, ServiceAccess::CHANGE_CONFIG | ServiceAccess::START)?
        }
    };
    service.set_description("Collects server, IIS and SQL Server metrics for the MonitorLab hub")?;
    let _ = std::process::Command::new("sc.exe")
        .args(["failure", crate::winsvc::SERVICE_NAME, "reset=", "86400", "actions=", "restart/5000/restart/10000/restart/60000"])
        .output();
    service.start::<&str>(&[])?;
    println!("Service '{}' installed at {} and started.", crate::winsvc::SERVICE_NAME, target.display());
    Ok(())
}

#[cfg(windows)]
fn uninstall_service_only() {
    use windows_service::service::{ServiceAccess, ServiceState};
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    let Ok(manager) = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT) else { return };
    let Ok(svc) = manager.open_service(
        crate::winsvc::SERVICE_NAME,
        ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE,
    ) else {
        return;
    };
    if let Ok(st) = svc.query_status() {
        if st.current_state != ServiceState::Stopped {
            let _ = svc.stop();
            for _ in 0..20 {
                std::thread::sleep(std::time::Duration::from_millis(500));
                if svc.query_status().map(|s| s.current_state == ServiceState::Stopped).unwrap_or(true) {
                    break;
                }
            }
        }
    }
    let _ = svc.delete();
}

#[cfg(windows)]
pub fn uninstall(purge: bool) -> Result<()> {
    uninstall_service_only();
    if purge {
        let _ = std::fs::remove_file(default_config_path());
    }
    println!("MonitorLab agent service removed{}.", if purge { " (config purged)" } else { "" });
    Ok(())
}
