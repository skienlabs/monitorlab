//! IIS: sites, application pools, worker processes and W3C logs.
//!
//! Site layout (ids, bindings, physical paths, log directories) is read from
//! `applicationHost.config`; runtime state comes from `appcmd.exe /xml`.
//! The XML parsing is platform independent so it is unit-tested everywhere.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Result};
use serde::Serialize;
use serde_json::{json, Value};
use vigil_common::{IisPoolAction, IisSiteAction, IisSummary};

use crate::system::Collector;
use crate::util::{expand_env_vars, is_plain_file_name};

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SiteConfig {
    pub id: u64,
    pub name: String,
    pub bindings: Vec<String>,
    pub physical_path: Option<String>,
    pub apps: Vec<AppConfig>,
    pub log_dir: String,
    pub log_format: String,
    pub auto_start: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AppConfig {
    pub path: String,
    pub pool: String,
    pub physical_path: Option<String>,
}

pub fn parse_application_host(xml: &str) -> Result<Vec<SiteConfig>> {
    let xml = xml.trim_start_matches('\u{feff}');
    let doc = roxmltree::Document::parse(xml)?;
    let sites_node = doc
        .descendants()
        .find(|n| n.has_tag_name("sites") && n.parent().map(|p| p.has_tag_name("system.applicationHost")).unwrap_or(false))
        .ok_or_else(|| anyhow!("<sites> not found in applicationHost.config"))?;

    let mut default_log_dir = "%SystemDrive%\\inetpub\\logs\\LogFiles".to_string();
    let mut default_log_format = "W3C".to_string();
    let mut default_pool = "DefaultAppPool".to_string();
    for child in sites_node.children().filter(|n| n.is_element()) {
        if child.has_tag_name("siteDefaults") {
            if let Some(lf) = child.children().find(|n| n.has_tag_name("logFile")) {
                if let Some(d) = lf.attribute("directory") {
                    default_log_dir = d.to_string();
                }
                if let Some(f) = lf.attribute("logFormat") {
                    default_log_format = f.to_string();
                }
            }
        } else if child.has_tag_name("applicationDefaults") {
            if let Some(p) = child.attribute("applicationPool") {
                default_pool = p.to_string();
            }
        }
    }

    let mut out = Vec::new();
    for site in sites_node.children().filter(|n| n.has_tag_name("site")) {
        let Some(id) = site.attribute("id").and_then(|v| v.parse::<u64>().ok()) else { continue };
        let name = site.attribute("name").unwrap_or_default().to_string();
        let site_pool = site
            .children()
            .find(|n| n.has_tag_name("applicationDefaults"))
            .and_then(|n| n.attribute("applicationPool"))
            .unwrap_or(&default_pool)
            .to_string();
        let mut apps = Vec::new();
        for app in site.children().filter(|n| n.has_tag_name("application")) {
            let path = app.attribute("path").unwrap_or("/").to_string();
            let pool = app.attribute("applicationPool").unwrap_or(&site_pool).to_string();
            let physical_path = app
                .children()
                .find(|n| n.has_tag_name("virtualDirectory") && n.attribute("path") == Some("/"))
                .and_then(|n| n.attribute("physicalPath"))
                .map(str::to_string);
            apps.push(AppConfig { path, pool, physical_path });
        }
        let bindings = site
            .children()
            .find(|n| n.has_tag_name("bindings"))
            .map(|b| {
                b.children()
                    .filter(|n| n.has_tag_name("binding"))
                    .map(|n| format!("{} {}", n.attribute("protocol").unwrap_or("?"), n.attribute("bindingInformation").unwrap_or("")))
                    .collect()
            })
            .unwrap_or_default();
        let lf = site.children().find(|n| n.has_tag_name("logFile"));
        let log_dir = lf.and_then(|n| n.attribute("directory")).unwrap_or(&default_log_dir).to_string();
        let log_format = lf.and_then(|n| n.attribute("logFormat")).unwrap_or(&default_log_format).to_string();
        let physical_path = apps.iter().find(|a| a.path == "/").and_then(|a| a.physical_path.clone());
        out.push(SiteConfig {
            id,
            name,
            bindings,
            physical_path,
            apps,
            log_dir,
            log_format,
            auto_start: site.attribute("serverAutoStart") != Some("false"),
        });
    }
    Ok(out)
}

/// Parse `appcmd list <x> /xml` output into a list of attribute maps.
pub fn parse_appcmd_xml(xml: &str, tag: &str) -> Result<Vec<HashMap<String, String>>> {
    let xml = xml.trim_start_matches('\u{feff}').trim();
    if xml.is_empty() {
        return Ok(vec![]);
    }
    let doc = roxmltree::Document::parse(xml)?;
    Ok(doc
        .descendants()
        .filter(|n| n.has_tag_name(tag))
        .map(|n| n.attributes().map(|a| (a.name().to_string(), a.value().to_string())).collect())
        .collect())
}

/// IIS directory for a site's logs: `<logDir>\W3SVC<id>`.
pub fn site_log_dir(site: &SiteConfig) -> PathBuf {
    PathBuf::from(expand_env_vars(&site.log_dir)).join(format!("W3SVC{}", site.id))
}

pub fn list_log_files(dir: &Path) -> Vec<(String, u64, i64)> {
    let mut files: Vec<(String, u64, i64)> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().to_string();
                    if !name.to_ascii_lowercase().ends_with(".log") {
                        return None;
                    }
                    let md = e.metadata().ok()?;
                    if !md.is_file() {
                        return None;
                    }
                    let mtime = md
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or(0);
                    Some((name, md.len(), mtime))
                })
                .collect()
        })
        .unwrap_or_default();
    files.sort_by(|a, b| b.2.cmp(&a.2).then(b.0.cmp(&a.0)));
    files
}

pub fn newest_log(dir: &Path) -> Option<PathBuf> {
    list_log_files(dir).first().map(|(n, _, _)| dir.join(n))
}

/// The most recent `#Fields:` directive near the start of a W3C log.
fn w3c_fields(path: &Path) -> Vec<String> {
    let head = crate::textlog::head_text(path, 8192).unwrap_or_default();
    head.lines()
        .filter_map(|l| l.strip_prefix("#Fields:"))
        .last()
        .map(|f| f.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default()
}

pub struct Iis {
    enabled: Option<bool>,
}

impl Iis {
    pub fn new(enabled: Option<bool>) -> Self {
        Self { enabled }
    }

    fn windir() -> PathBuf {
        PathBuf::from(std::env::var("windir").or_else(|_| std::env::var("SystemRoot")).unwrap_or_else(|_| "C:\\Windows".into()))
    }

    fn appcmd_path() -> PathBuf {
        Self::windir().join("System32").join("inetsrv").join("appcmd.exe")
    }

    fn config_path() -> PathBuf {
        Self::windir().join("System32").join("inetsrv").join("config").join("applicationHost.config")
    }

    pub fn detected(&self) -> bool {
        if self.enabled == Some(false) || !cfg!(windows) {
            return false;
        }
        Self::appcmd_path().exists()
    }

    fn ensure(&self) -> Result<()> {
        if !cfg!(windows) {
            bail!("IIS is only available on Windows servers");
        }
        if !self.detected() {
            bail!("IIS is not installed on this server");
        }
        Ok(())
    }

    fn appcmd(args: &[String]) -> Result<String> {
        let mut cmd = std::process::Command::new(Self::appcmd_path());
        cmd.args(args);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let out = cmd.output()?;
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            let msg = if err.trim().is_empty() { stdout.trim().to_string() } else { err.trim().to_string() };
            bail!("appcmd failed: {msg}");
        }
        Ok(stdout)
    }

    pub fn site_configs(&self) -> Result<Vec<SiteConfig>> {
        self.ensure()?;
        let xml = std::fs::read_to_string(Self::config_path())?;
        parse_application_host(&xml)
    }

    fn site_states() -> HashMap<String, String> {
        Self::appcmd(&["list".into(), "site".into(), "/xml".into()])
            .ok()
            .and_then(|x| parse_appcmd_xml(&x, "SITE").ok())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|m| Some((m.get("SITE.NAME")?.clone(), m.get("state").cloned().unwrap_or_default())))
            .collect()
    }

    fn pools_raw() -> Vec<HashMap<String, String>> {
        Self::appcmd(&["list".into(), "apppool".into(), "/xml".into()])
            .ok()
            .and_then(|x| parse_appcmd_xml(&x, "APPPOOL").ok())
            .unwrap_or_default()
    }

    /// Pool name -> worker process ids.
    fn worker_pids() -> HashMap<String, Vec<u32>> {
        let mut m: HashMap<String, Vec<u32>> = HashMap::new();
        if let Ok(x) = Self::appcmd(&["list".into(), "wp".into(), "/xml".into()]) {
            for wp in parse_appcmd_xml(&x, "WP").unwrap_or_default() {
                if let (Some(pid), Some(pool)) = (wp.get("WP.NAME").and_then(|p| p.parse().ok()), wp.get("APPPOOL.NAME")) {
                    m.entry(pool.clone()).or_default().push(pid);
                }
            }
        }
        m
    }

    pub fn summary(&self, col: &Collector) -> Option<IisSummary> {
        if !self.detected() {
            return None;
        }
        let states = Self::site_states();
        let pools = Self::pools_raw();
        let mut s = IisSummary {
            sites_total: states.len() as u32,
            sites_started: states.values().filter(|v| v.eq_ignore_ascii_case("started")).count() as u32,
            pools_total: pools.len() as u32,
            pools_started: pools.iter().filter(|p| p.get("state").map(|v| v.eq_ignore_ascii_case("started")).unwrap_or(false)).count() as u32,
            ..Default::default()
        };
        let (cpu, mem, _) = col.proc_usage(|p| p.name().to_string_lossy().eq_ignore_ascii_case("w3wp.exe"));
        s.worker_cpu = cpu;
        s.worker_mem = mem;
        Some(s)
    }

    pub fn sites(&self) -> Result<Value> {
        let configs = self.site_configs()?;
        let states = Self::site_states();
        let list: Vec<Value> = configs
            .into_iter()
            .map(|c| {
                let state = states.get(&c.name).cloned().unwrap_or_else(|| "Unknown".into());
                let log_dir = site_log_dir(&c);
                json!({
                    "id": c.id,
                    "name": c.name,
                    "state": state,
                    "bindings": c.bindings,
                    "physical_path": c.physical_path.as_deref().map(expand_env_vars),
                    "apps": c.apps,
                    "pools": c.apps.iter().map(|a| a.pool.clone()).collect::<std::collections::BTreeSet<_>>(),
                    "log_dir": log_dir.display().to_string(),
                    "log_format": c.log_format,
                    "auto_start": c.auto_start,
                })
            })
            .collect();
        Ok(Value::Array(list))
    }

    pub fn app_pools(&self, col: &Collector) -> Result<Value> {
        self.ensure()?;
        let pools = Self::pools_raw();
        let wps = Self::worker_pids();
        let list: Vec<Value> = pools
            .into_iter()
            .map(|p| {
                let name = p.get("APPPOOL.NAME").cloned().unwrap_or_default();
                let pids = wps.get(&name).cloned().unwrap_or_default();
                let (mut cpu, mut mem) = (0.0f32, 0u64);
                for pid in &pids {
                    if let Some((c, m)) = col.proc_by_pid(*pid) {
                        cpu += c;
                        mem += m;
                    }
                }
                json!({
                    "name": name,
                    "state": p.get("state"),
                    "runtime": p.get("RuntimeVersion").filter(|v| !v.is_empty()).cloned().unwrap_or_else(|| "No Managed Code".into()),
                    "pipeline": p.get("PipelineMode"),
                    "worker_pids": pids,
                    "cpu": cpu,
                    "mem": mem,
                })
            })
            .collect();
        Ok(Value::Array(list))
    }

    pub fn site_action(&self, name: &str, action: IisSiteAction) -> Result<Value> {
        let configs = self.site_configs()?;
        if !configs.iter().any(|c| c.name == name) {
            bail!("unknown site '{name}'");
        }
        let arg = format!("/site.name:{name}");
        match action {
            IisSiteAction::Start => {
                Self::appcmd(&["start".into(), "site".into(), arg])?;
            }
            IisSiteAction::Stop => {
                Self::appcmd(&["stop".into(), "site".into(), arg])?;
            }
            IisSiteAction::Restart => {
                let _ = Self::appcmd(&["stop".into(), "site".into(), arg.clone()]);
                Self::appcmd(&["start".into(), "site".into(), arg])?;
            }
        }
        Ok(json!({ "ok": true }))
    }

    pub fn pool_action(&self, name: &str, action: IisPoolAction) -> Result<Value> {
        self.ensure()?;
        let pools = Self::pools_raw();
        if !pools.iter().any(|p| p.get("APPPOOL.NAME").map(|n| n == name).unwrap_or(false)) {
            bail!("unknown application pool '{name}'");
        }
        let verb = match action {
            IisPoolAction::Start => "start",
            IisPoolAction::Stop => "stop",
            IisPoolAction::Recycle => "recycle",
        };
        Self::appcmd(&[verb.into(), "apppool".into(), format!("/apppool.name:{name}")])?;
        Ok(json!({ "ok": true }))
    }

    pub fn log_dir_for(&self, site_id: u64) -> Result<PathBuf> {
        let configs = self.site_configs()?;
        let site = configs.iter().find(|c| c.id == site_id).ok_or_else(|| anyhow!("unknown site id {site_id}"))?;
        Ok(site_log_dir(site))
    }

    pub fn log_files(&self, site_id: u64) -> Result<Value> {
        let dir = self.log_dir_for(site_id)?;
        let files: Vec<Value> = list_log_files(&dir)
            .into_iter()
            .take(200)
            .map(|(name, size, mtime)| json!({ "name": name, "size": size, "modified": mtime }))
            .collect();
        Ok(json!({ "dir": dir.display().to_string(), "files": files }))
    }

    pub fn log_tail(&self, site_id: u64, file: Option<&str>, lines: u32) -> Result<Value> {
        let dir = self.log_dir_for(site_id)?;
        let path = match file {
            Some(f) => {
                if !is_plain_file_name(f) {
                    bail!("invalid file name");
                }
                let p = dir.join(f);
                if !p.is_file() {
                    bail!("log file not found");
                }
                p
            }
            None => newest_log(&dir).ok_or_else(|| anyhow!("no log files yet in {}", dir.display()))?,
        };
        let fields = w3c_fields(&path);
        let lines = crate::textlog::tail_lines(&path, lines.clamp(1, 20_000) as usize, 16 * 1024 * 1024)?;
        Ok(json!({
            "file": path.file_name().map(|n| n.to_string_lossy().to_string()),
            "fields": fields,
            "lines": lines,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CFG: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<configuration>
  <system.applicationHost>
    <applicationPools>
      <add name="DefaultAppPool" />
      <add name="ApiPool" managedRuntimeVersion="" />
    </applicationPools>
    <sites>
      <site name="Default Web Site" id="1" serverAutoStart="true">
        <application path="/">
          <virtualDirectory path="/" physicalPath="%SystemDrive%\inetpub\wwwroot" />
        </application>
        <bindings>
          <binding protocol="http" bindingInformation="*:80:" />
        </bindings>
      </site>
      <site name="Orders API" id="3">
        <application path="/" applicationPool="ApiPool">
          <virtualDirectory path="/" physicalPath="D:\apps\orders" />
        </application>
        <application path="/v2" applicationPool="ApiPool">
          <virtualDirectory path="/" physicalPath="D:\apps\orders-v2" />
        </application>
        <bindings>
          <binding protocol="https" bindingInformation="*:443:orders.local" sslFlags="1" />
        </bindings>
        <logFile directory="E:\logs\iis" />
      </site>
      <siteDefaults>
        <logFile logFormat="W3C" directory="%SystemDrive%\inetpub\logs\LogFiles" />
      </siteDefaults>
      <applicationDefaults applicationPool="DefaultAppPool" />
    </sites>
  </system.applicationHost>
</configuration>"#;

    #[test]
    fn parses_sites() {
        let sites = parse_application_host(CFG).unwrap();
        assert_eq!(sites.len(), 2);
        assert_eq!(sites[0].name, "Default Web Site");
        assert_eq!(sites[0].apps[0].pool, "DefaultAppPool");
        assert_eq!(sites[0].log_dir, "%SystemDrive%\\inetpub\\logs\\LogFiles");
        assert_eq!(sites[1].id, 3);
        assert_eq!(sites[1].apps.len(), 2);
        assert_eq!(sites[1].apps[1].pool, "ApiPool");
        assert_eq!(sites[1].bindings, vec!["https *:443:orders.local"]);
        assert_eq!(sites[1].log_dir, "E:\\logs\\iis");
        assert_eq!(sites[1].physical_path.as_deref(), Some("D:\\apps\\orders"));
        assert!(site_log_dir(&sites[1]).to_string_lossy().ends_with("W3SVC3"));
    }

    #[test]
    fn parses_appcmd() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<appcmd>
    <SITE SITE.NAME="Default Web Site" SITE.ID="1" bindings="http/*:80:" state="Started" />
    <SITE SITE.NAME="Orders API" SITE.ID="3" bindings="https/*:443:orders.local" state="Stopped" />
</appcmd>"#;
        let v = parse_appcmd_xml(xml, "SITE").unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[1]["state"], "Stopped");
        assert_eq!(v[0]["SITE.NAME"], "Default Web Site");
        assert!(parse_appcmd_xml("", "SITE").unwrap().is_empty());
    }
}
