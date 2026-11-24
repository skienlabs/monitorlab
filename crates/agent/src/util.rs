use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

pub fn b64(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}

pub fn unb64(s: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::STANDARD.decode(s).ok()
}

/// Resolve an absolute host path, honouring an optional host-root prefix
/// (used when the agent runs inside a container with the host's `/` mounted at e.g. `/host`).
pub fn host_path(host_root: Option<&str>, p: &str) -> PathBuf {
    match host_root {
        Some(root) if !root.is_empty() && root != "/" => {
            let rel = p.trim_start_matches('/');
            Path::new(root).join(rel)
        }
        _ => PathBuf::from(p),
    }
}

/// Truncate a string to at most `max` bytes on a char boundary.
pub fn truncate(mut s: String, max: usize) -> String {
    if s.len() > max {
        let mut i = max;
        while !s.is_char_boundary(i) {
            i -= 1;
        }
        s.truncate(i);
        s.push_str(" …[truncated]");
    }
    s
}

/// Expand Windows-style `%VAR%` environment references.
pub fn expand_env_vars(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        if let Some(end) = after.find('%') {
            let name = &after[..end];
            match std::env::var(name) {
                Ok(v) if !name.is_empty() => out.push_str(&v),
                _ => {
                    out.push('%');
                    out.push_str(name);
                    out.push('%');
                }
            }
            rest = &after[end + 1..];
        } else {
            out.push_str(&rest[start..]);
            rest = "";
        }
    }
    out.push_str(rest);
    out
}

/// A file name that cannot escape its directory.
pub fn is_plain_file_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() < 256
        && !name.contains(['/', '\\', ':', '\0'])
        && name != "."
        && name != ".."
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_expansion() {
        std::env::set_var("MONITORLAB_TEST_X", "C:");
        assert_eq!(expand_env_vars("%MONITORLAB_TEST_X%\\inetpub"), "C:\\inetpub");
        assert_eq!(expand_env_vars("%NOPE_NOT_SET%\\a"), "%NOPE_NOT_SET%\\a");
        assert_eq!(expand_env_vars("100%"), "100%");
    }

    #[test]
    fn plain_names() {
        assert!(is_plain_file_name("u_ex240101.log"));
        assert!(!is_plain_file_name("..\\x"));
        assert!(!is_plain_file_name("../etc/passwd"));
        assert!(!is_plain_file_name(".."));
        assert!(!is_plain_file_name("C:x"));
    }

    #[test]
    fn host_paths() {
        assert_eq!(host_path(Some("/host"), "/var/opt"), PathBuf::from("/host/var/opt"));
        assert_eq!(host_path(None, "/var/opt"), PathBuf::from("/var/opt"));
    }
}
