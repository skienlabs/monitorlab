//! TLS certificate management and fingerprints used for agent certificate pinning.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::ServerConfig;
use sha2::{Digest, Sha256};

use crate::config::{HubConfig, TlsMode};

#[derive(Debug, Clone)]
pub struct TlsInfo {
    /// SHA-256 of the leaf certificate DER, colon separated upper-case hex (agent pinning).
    pub fingerprint: String,
    /// `sha256//<base64>` of the SubjectPublicKeyInfo (curl --pinnedpubkey).
    pub spki_pin: Option<String>,
    pub self_signed: bool,
    pub config: Arc<ServerConfig>,
}

pub fn fingerprint(der: &[u8]) -> String {
    Sha256::digest(der).iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":")
}

/// Minimal DER walk: Certificate -> tbsCertificate -> subjectPublicKeyInfo.
pub fn spki_from_cert(der: &[u8]) -> Option<&[u8]> {
    fn tlv(d: &[u8]) -> Option<(u8, &[u8], &[u8], &[u8])> {
        let tag = *d.first()?;
        let l0 = *d.get(1)? as usize;
        let (len, hdr) = if l0 < 0x80 {
            (l0, 2)
        } else {
            let n = l0 & 0x7f;
            if n == 0 || n > 4 {
                return None;
            }
            let mut len = 0usize;
            for i in 0..n {
                len = (len << 8) | *d.get(2 + i)? as usize;
            }
            (len, 2 + n)
        };
        let end = hdr.checked_add(len)?;
        if d.len() < end {
            return None;
        }
        Some((tag, &d[hdr..end], &d[end..], &d[..end]))
    }
    let (t, cert, _, _) = tlv(der)?;
    if t != 0x30 {
        return None;
    }
    let (t, tbs, _, _) = tlv(cert)?;
    if t != 0x30 {
        return None;
    }
    let mut rest = tbs;
    if rest.first() == Some(&0xA0) {
        rest = tlv(rest)?.2; // [0] version
    }
    for expected in [0x02u8, 0x30, 0x30, 0x30, 0x30] {
        // serial, signature alg, issuer, validity, subject
        let (t, _, r, _) = tlv(rest)?;
        if t != expected {
            return None;
        }
        rest = r;
    }
    let (t, _, _, whole) = tlv(rest)?;
    (t == 0x30).then_some(whole)
}

pub fn spki_pin(cert_der: &[u8]) -> Option<String> {
    spki_from_cert(cert_der).map(|spki| {
        format!("sha256//{}", base64::engine::general_purpose::STANDARD.encode(Sha256::digest(spki)))
    })
}

fn generate(dir: &Path, sans: &str) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut names: Vec<String> = vec!["localhost".into()];
    if let Ok(h) = std::fs::read_to_string("/etc/hostname") {
        let h = h.trim();
        if !h.is_empty() {
            names.push(h.to_string());
        }
    }
    names.extend(sans.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()));
    names.dedup();
    let ck = rcgen::generate_simple_self_signed(names.clone())?;
    std::fs::write(dir.join("cert.pem"), ck.cert.pem())?;
    write_private(&dir.join("key.pem"), ck.key_pair.serialize_pem().as_bytes())?;
    tracing::info!("generated self-signed TLS certificate for {}", names.join(", "));
    Ok(())
}

#[cfg(unix)]
fn write_private(p: &Path, data: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new().create(true).truncate(true).write(true).mode(0o600).open(p)?;
    f.write_all(data)?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private(p: &Path, data: &[u8]) -> Result<()> {
    std::fs::write(p, data)?;
    Ok(())
}

pub fn load(cfg: &HubConfig) -> Result<Option<TlsInfo>> {
    let (cert_path, key_path, self_signed): (PathBuf, PathBuf, bool) = match cfg.tls {
        TlsMode::Off => return Ok(None),
        TlsMode::Auto => {
            let dir = cfg.data_dir.join("tls");
            let (c, k) = (dir.join("cert.pem"), dir.join("key.pem"));
            if !c.exists() || !k.exists() {
                generate(&dir, &cfg.tls_sans)?;
            }
            (c, k, true)
        }
        TlsMode::Files => {
            let c = cfg.tls_cert.clone().ok_or_else(|| anyhow!("MONITORLAB_TLS=files requires MONITORLAB_TLS_CERT"))?;
            let k = cfg.tls_key.clone().ok_or_else(|| anyhow!("MONITORLAB_TLS=files requires MONITORLAB_TLS_KEY"))?;
            (c, k, false)
        }
    };
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(&cert_path)
        .with_context(|| format!("reading {}", cert_path.display()))?
        .collect::<Result<_, _>>()
        .with_context(|| format!("parsing {}", cert_path.display()))?;
    if certs.is_empty() {
        bail!("no certificate found in {}", cert_path.display());
    }
    let key = PrivateKeyDer::from_pem_file(&key_path).with_context(|| format!("reading key {}", key_path.display()))?;
    let leaf = certs[0].as_ref().to_vec();

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut server = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .context("invalid certificate / key pair")?;
    // HTTP/1.1 only: WebSockets are upgraded over HTTP/1.1.
    server.alpn_protocols = vec![b"http/1.1".to_vec()];

    Ok(Some(TlsInfo {
        fingerprint: fingerprint(&leaf),
        spki_pin: spki_pin(&leaf),
        self_signed,
        config: Arc::new(server),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spki_matches_rcgen() {
        let ck = rcgen::generate_simple_self_signed(vec!["x.local".to_string()]).unwrap();
        let der = ck.cert.der().to_vec();
        let spki = spki_from_cert(&der).expect("spki");
        assert_eq!(spki, ck.key_pair.public_key_der().as_slice());
        assert!(spki_pin(&der).unwrap().starts_with("sha256//"));
        assert_eq!(fingerprint(&der).len(), 32 * 3 - 1);
    }
}
