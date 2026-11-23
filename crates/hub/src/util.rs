use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use rand::RngCore;
use sha2::{Digest, Sha256};

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// URL-safe random token with `bytes` bytes of entropy.
pub fn random_token(bytes: usize) -> String {
    let mut b = vec![0u8; bytes];
    rand::rngs::OsRng.fill_bytes(&mut b);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

/// Human friendly password (no ambiguous characters).
pub fn random_password(len: usize) -> String {
    const CHARS: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnpqrstuvwxyz23456789";
    let mut b = vec![0u8; len];
    rand::rngs::OsRng.fill_bytes(&mut b);
    b.iter().map(|x| CHARS[*x as usize % CHARS.len()] as char).collect()
}

pub fn sha256_hex(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

pub fn ct_eq(a: &str, b: &str) -> bool {
    use subtle::ConstantTimeEq;
    a.len() == b.len() && bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}
