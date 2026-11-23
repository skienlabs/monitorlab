//! Outbound WebSocket connection to the hub, TLS (with optional certificate pinning),
//! enrollment and the reconnect loop.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use rand::Rng;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::WebPkiSupportedAlgorithms;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{Connector, MaybeTlsStream, WebSocketStream};
use vigil_common::{AgentMsg, HubMsg};

use crate::config::AgentConfig;
use crate::session::{Agent, AgentOptions};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Hub silence after which the connection is considered dead (hub pings every 20s).
const IDLE_TIMEOUT: Duration = Duration::from_secs(75);

pub fn parse_fingerprint(fp: &str) -> Result<[u8; 32]> {
    let clean: String = fp.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    let bytes = hex::decode(&clean).map_err(|_| anyhow!("fingerprint must be hex"))?;
    bytes.try_into().map_err(|_| anyhow!("fingerprint must be a SHA-256 (32 bytes / 64 hex chars)"))
}

pub fn ws_url(hub: &str, path: &str) -> Result<String> {
    let hub = hub.trim().trim_end_matches('/');
    let base = if let Some(rest) = hub.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = hub.strip_prefix("http://") {
        format!("ws://{rest}")
    } else if hub.starts_with("wss://") || hub.starts_with("ws://") {
        hub.to_string()
    } else {
        bail!("hub URL must start with https:// (or http:// on a trusted network)");
    };
    Ok(format!("{base}{path}"))
}

#[derive(Debug)]
struct PinnedVerifier {
    fingerprint: [u8; 32],
    algs: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let got = Sha256::digest(end_entity.as_ref());
        if got.as_slice() == self.fingerprint {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(format!(
                "hub certificate fingerprint mismatch (got {})",
                hex::encode(got)
            )))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algs)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algs)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algs.supported_schemes()
    }
}

fn tls_config(fingerprint: Option<&str>) -> Result<ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = ClientConfig::builder_with_provider(provider.clone()).with_safe_default_protocol_versions()?;
    let cfg = match fingerprint.filter(|f| !f.trim().is_empty()) {
        Some(fp) => builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(PinnedVerifier {
                fingerprint: parse_fingerprint(fp)?,
                algs: provider.signature_verification_algorithms,
            }))
            .with_no_client_auth(),
        None => {
            let roots = RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            builder.with_root_certificates(roots).with_no_client_auth()
        }
    };
    Ok(cfg)
}

async fn open(url: &str, fingerprint: Option<&str>, headers: &[(&'static str, String)]) -> Result<Ws> {
    let mut req = url.into_client_request()?;
    for (k, v) in headers {
        req.headers_mut().insert(*k, HeaderValue::from_str(v)?);
    }
    req.headers_mut().insert("User-Agent", HeaderValue::from_str(&format!("vigil-agent/{}", vigil_common::VERSION))?);
    let connector = if url.starts_with("wss://") { Some(Connector::Rustls(Arc::new(tls_config(fingerprint)?))) } else { None };
    let (ws, _resp) = tokio::time::timeout(
        Duration::from_secs(20),
        tokio_tungstenite::connect_async_tls_with_config(req, None, true, connector),
    )
    .await
    .context("connection timed out")??;
    Ok(ws)
}

#[derive(Serialize)]
struct EnrollRequest<'a> {
    token: &'a str,
    hostname: String,
    os: &'a str,
    agent_version: &'a str,
}

#[derive(Deserialize)]
struct EnrollResponse {
    #[serde(default)]
    agent_id: Option<String>,
    #[serde(default)]
    secret: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

/// Exchange a one-time enrollment token for permanent credentials.
pub async fn enroll(hub: &str, token: &str, fingerprint: Option<&str>) -> Result<(String, String, String)> {
    let url = ws_url(hub, "/api/agent/enroll")?;
    let mut ws = open(&url, fingerprint, &[]).await.with_context(|| format!("connecting to {url}"))?;
    let req = EnrollRequest {
        token,
        hostname: sysinfo::System::host_name().unwrap_or_default(),
        os: if cfg!(windows) { "windows" } else { "linux" },
        agent_version: vigil_common::VERSION,
    };
    ws.send(Message::Text(serde_json::to_string(&req)?.into())).await?;
    let reply = tokio::time::timeout(Duration::from_secs(20), async {
        while let Some(m) = ws.next().await {
            if let Message::Text(t) = m? {
                return Ok::<_, anyhow::Error>(t.to_string());
            }
        }
        bail!("hub closed the connection")
    })
    .await
    .context("enrollment timed out")??;
    let _ = ws.close(None).await;
    let r: EnrollResponse = serde_json::from_str(&reply)?;
    if let Some(e) = r.error {
        bail!("enrollment rejected: {e}");
    }
    match (r.agent_id, r.secret) {
        (Some(id), Some(secret)) => Ok((id, secret, r.name.unwrap_or_default())),
        _ => bail!("malformed enrollment response"),
    }
}

/// Run forever: connect, serve, reconnect with jittered exponential backoff.
pub async fn run_forever(cfg: AgentConfig) -> Result<()> {
    let agent = Agent::new(AgentOptions::from_config(&cfg));
    let url = ws_url(&cfg.hub_url, "/api/agent/connect")?;
    if url.starts_with("ws://") {
        tracing::warn!("hub URL is not TLS protected; use https:// outside trusted networks");
    }
    let auth = format!("Bearer {}:{}", cfg.agent_id, cfg.secret);
    let mut backoff = 1u64;
    loop {
        match open(&url, cfg.fingerprint.as_deref(), &[("Authorization", auth.clone())]).await {
            Ok(ws) => {
                tracing::info!("connected to hub {}", cfg.hub_url);
                backoff = 1;
                if let Err(e) = pump(ws, agent.clone()).await {
                    tracing::warn!("hub connection lost: {e:#}");
                } else {
                    tracing::info!("hub connection closed");
                }
            }
            Err(e) => {
                let msg = format!("{e:#}");
                if msg.contains("401") || msg.contains("403") {
                    tracing::error!("hub rejected agent credentials ({msg}); was this server removed from the hub?");
                    backoff = 60;
                } else {
                    tracing::warn!("cannot reach hub: {msg}");
                }
            }
        }
        let jitter = rand::thread_rng().gen_range(0..=backoff * 250);
        tokio::time::sleep(Duration::from_millis(backoff * 1000 + jitter)).await;
        backoff = (backoff * 2).min(60);
    }
}

async fn pump(ws: Ws, agent: Arc<Agent>) -> Result<()> {
    let (mut sink, mut stream) = ws.split();
    let (in_tx, in_rx) = mpsc::channel::<HubMsg>(256);
    let (out_tx, mut out_rx) = mpsc::channel::<AgentMsg>(512);

    let serve = tokio::spawn(agent.serve(in_rx, out_tx));

    let writer = tokio::spawn(async move {
        while let Some(m) = out_rx.recv().await {
            let text = serde_json::to_string(&m)?;
            sink.send(Message::Text(text.into())).await?;
        }
        let _ = sink.close().await;
        Ok::<_, anyhow::Error>(())
    });

    let reader = async {
        loop {
            let next = tokio::time::timeout(IDLE_TIMEOUT, stream.next()).await.map_err(|_| anyhow!("hub idle timeout"))?;
            match next {
                Some(Ok(Message::Text(t))) => match serde_json::from_str::<HubMsg>(&t) {
                    Ok(m) => {
                        if in_tx.send(m).await.is_err() {
                            return Ok(());
                        }
                    }
                    Err(e) => tracing::debug!("ignoring malformed hub message: {e}"),
                },
                Some(Ok(Message::Close(_))) | None => return Ok(()),
                Some(Ok(_)) => {}
                Some(Err(e)) => return Err(anyhow::Error::from(e)),
            }
        }
    };

    let res = tokio::select! {
        r = reader => r,
        w = writer => match w {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e),
            Err(e) => Err(anyhow::Error::from(e)),
        },
    };
    // Closing the inbound channel makes `serve` stop its metrics loop and abort open streams.
    drop(in_tx);
    if tokio::time::timeout(Duration::from_secs(5), serve).await.is_err() {
        tracing::warn!("agent session did not stop in time");
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert_eq!(ws_url("https://h:8443/", "/x").unwrap(), "wss://h:8443/x");
        assert_eq!(ws_url("http://h", "/x").unwrap(), "ws://h/x");
        assert!(ws_url("h:8443", "/x").is_err());
    }

    #[test]
    fn fingerprints() {
        let hex64 = "AB".repeat(32);
        assert!(parse_fingerprint(&hex64).is_ok());
        let colon = vec!["ab"; 32].join(":");
        assert_eq!(parse_fingerprint(&colon).unwrap(), [0xab; 32]);
        assert!(parse_fingerprint("abcd").is_err());
    }
}
