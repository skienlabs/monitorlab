//! Registry credentials from Docker CLI config files (`docker login` stores them there), so
//! "Recreate with pull" works for private registries without a separate login in MonitorLab.
//! Credential helpers (`credsStore`/`credHelpers`) cannot be used from here and are skipped.

use base64::Engine;
use bollard::auth::DockerCredentials;
use serde_json::Value;

fn candidates(host_root: Option<&str>) -> Vec<std::path::PathBuf> {
    let mut v = Vec::new();
    if let Ok(dir) = std::env::var("DOCKER_CONFIG") {
        v.push(std::path::Path::new(&dir).join("config.json"));
    }
    let roots: Vec<String> = match host_root.filter(|r| !r.is_empty() && *r != "/") {
        Some(r) => vec![r.trim_end_matches('/').to_string(), String::new()],
        None => vec![String::new()],
    };
    for root in roots {
        v.push(format!("{root}/root/.docker/config.json").into());
        if let Ok(rd) = std::fs::read_dir(format!("{root}/home")) {
            for e in rd.flatten() {
                v.push(e.path().join(".docker/config.json"));
            }
        }
    }
    if let Ok(home) = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) {
        v.push(std::path::Path::new(&home).join(".docker/config.json"));
    }
    v
}

fn matches(key: &str, registry: &str) -> bool {
    let host = key.trim_start_matches("https://").trim_start_matches("http://").split('/').next().unwrap_or("");
    if registry == "docker.io" {
        return matches!(host, "index.docker.io" | "docker.io" | "registry-1.docker.io");
    }
    host == registry
}

pub fn from_config(cfg: &Value, registry: &str) -> Option<DockerCredentials> {
    let auths = cfg.get("auths")?.as_object()?;
    for (key, entry) in auths {
        if !matches(key, registry) {
            continue;
        }
        if let Some(tok) = entry.get("identitytoken").and_then(Value::as_str).filter(|t| !t.is_empty()) {
            return Some(DockerCredentials { identitytoken: Some(tok.to_string()), serveraddress: Some(key.clone()), ..Default::default() });
        }
        let raw = entry.get("auth").and_then(Value::as_str)?;
        let decoded = base64::engine::general_purpose::STANDARD.decode(raw.trim()).ok()?;
        let text = String::from_utf8(decoded).ok()?;
        let (user, pass) = text.split_once(':')?;
        return Some(DockerCredentials {
            username: Some(user.to_string()),
            password: Some(pass.to_string()),
            serveraddress: Some(key.clone()),
            ..Default::default()
        });
    }
    None
}

pub fn credentials_for(registry: &str, host_root: Option<&str>) -> Option<DockerCredentials> {
    for path in candidates(host_root) {
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let Ok(cfg) = serde_json::from_str::<Value>(&text) else { continue };
        if let Some(c) = from_config(&cfg, registry) {
            tracing::debug!("using registry credentials for {registry} from {}", path.display());
            return Some(c);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_auths() {
        let cfg = serde_json::json!({ "auths": {
            "https://index.docker.io/v1/": { "auth": base64::engine::general_purpose::STANDARD.encode("bob:s3cret") },
            "us-central1-docker.pkg.dev": { "auth": base64::engine::general_purpose::STANDARD.encode("_json_key:{\"a\":1}") }
        }, "credHelpers": { "gcr.io": "gcloud" } });
        let hub = from_config(&cfg, "docker.io").unwrap();
        assert_eq!(hub.username.as_deref(), Some("bob"));
        assert_eq!(hub.password.as_deref(), Some("s3cret"));
        let gar = from_config(&cfg, "us-central1-docker.pkg.dev").unwrap();
        assert_eq!(gar.username.as_deref(), Some("_json_key"));
        assert_eq!(gar.password.as_deref(), Some("{\"a\":1}"));
        assert!(from_config(&cfg, "gcr.io").is_none());
    }
}
