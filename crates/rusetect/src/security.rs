//! Security for the review desk and the verdicts (port of service/security.py).
//!
//! * Analyst identity comes from an API key (header X-Analyst-Key), so the four eyes rule is enforced on
//!   authenticated people. Roles: analyst, supervisor, viewer. Keys from RUSETECT_ANALYST_KEYS
//!   (JSON {"key": {"analyst": "...", "role": "..."}}); without it the server runs in DEMO mode with published keys.
//! * Every review action carries a fresh nonce (header X-Nonce); a nonce seen in the last 10 minutes is rejected.
//! * A small per client rate limit protects the write endpoints.
//! * Verdicts and evidence packs are signed with Ed25519 (RUSETECT_SIGNING_KEY = base64 32 byte seed; otherwise an
//!   ephemeral key per process). Anyone can verify with GET /api/v2/keys/public.
use crate::pyfmt::{dumps, time_time, CANON_STYLE};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use ed25519_dalek::{Signature, Signer as _, SigningKey, Verifier, VerifyingKey};
use indexmap::IndexMap;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::sync::LazyLock;

pub const DEMO_KEYS: [(&str, &str, &str); 4] = [
    ("demo-analyst-1", "analyst.asha", "analyst"),
    ("demo-analyst-2", "analyst.ravi", "analyst"),
    ("demo-supervisor", "supervisor.meera", "supervisor"),
    ("demo-viewer", "viewer.ops", "viewer"),
];

#[derive(Debug, Clone)]
pub struct Analyst {
    pub analyst: String,
    pub role: String,
}

pub struct Keys {
    pub keys: IndexMap<String, Analyst>,
    pub demo_mode: bool,
}

pub fn load_keys() -> Keys {
    if let Ok(raw) = std::env::var("RUSETECT_ANALYST_KEYS") {
        let v: Value = serde_json::from_str(&raw).expect("RUSETECT_ANALYST_KEYS must be JSON {key: {analyst, role}}");
        let keys = v.as_object().map(|m| m.iter().map(|(k, a)| (k.clone(), Analyst {
            analyst: a["analyst"].as_str().unwrap_or("").into(),
            role: a["role"].as_str().unwrap_or("").into(),
        })).collect()).unwrap_or_default();
        return Keys { keys, demo_mode: false };
    }
    Keys { keys: DEMO_KEYS.iter().map(|(k, a, r)| (k.to_string(), Analyst { analyst: a.to_string(), role: r.to_string() })).collect(), demo_mode: true }
}

pub static KEYS: LazyLock<Keys> = LazyLock::new(load_keys);

pub fn analyst_for(key: Option<&str>) -> Option<Analyst> {
    KEYS.keys.get(key.unwrap_or("")).cloned()
}

pub fn allowed(role: &str, action: &str) -> bool {
    match role {
        "analyst" | "supervisor" => matches!(action, "release" | "verify" | "hold" | "approve_block"),
        _ => false,
    }
}

pub struct NonceCache {
    ttl: f64,
    seen: HashMap<String, f64>,
}

impl Default for NonceCache {
    fn default() -> Self {
        Self::new(600.0)
    }
}

impl NonceCache {
    pub fn new(ttl_s: f64) -> Self {
        NonceCache { ttl: ttl_s, seen: HashMap::new() }
    }

    /// True if the nonce is fresh (and records it); false if missing, too short or reused.
    pub fn use_nonce(&mut self, nonce: Option<&str>) -> bool {
        let now = time_time();
        if self.seen.len() > 50_000 {
            let ttl = self.ttl;
            self.seen.retain(|_, t| now - *t < ttl);
        }
        let Some(n) = nonce else { return false };
        let len = n.chars().count();
        if !(8..=128).contains(&len) {
            return false;
        }
        if let Some(t) = self.seen.get(n) {
            if now - t < self.ttl {
                return false;
            }
        }
        self.seen.insert(n.to_string(), now);
        true
    }
}

pub struct RateLimiter {
    pub limit: usize,
    per: f64,
    hits: HashMap<String, VecDeque<f64>>,
}

impl RateLimiter {
    pub fn new(limit: usize, per_s: f64) -> Self {
        RateLimiter { limit, per: per_s, hits: HashMap::new() }
    }

    pub fn allow(&mut self, client: &str) -> bool {
        let now = time_time();
        let q = self.hits.entry(client.to_string()).or_default();
        while q.front().map(|t| *t <= now - self.per).unwrap_or(false) {
            q.pop_front();
        }
        if q.len() >= self.limit {
            return false;
        }
        q.push_back(now);
        true
    }
}

/// `json.dumps(obj, sort_keys=True, separators=(",", ":"))` bytes
pub fn canon(obj: &Value) -> Vec<u8> {
    dumps(obj, CANON_STYLE).into_bytes()
}

pub struct Signer {
    key: SigningKey,
    pub ephemeral: bool,
    pub public_b64: String,
    pub key_id: String,
}

impl Default for Signer {
    fn default() -> Self {
        Self::from_env()
    }
}

impl Signer {
    pub fn from_env() -> Self {
        match std::env::var("RUSETECT_SIGNING_KEY") {
            Ok(seed) if !seed.is_empty() => {
                let b = B64.decode(seed.trim()).expect("RUSETECT_SIGNING_KEY must be base64");
                let arr: [u8; 32] = b.try_into().expect("RUSETECT_SIGNING_KEY must be a 32 byte seed");
                Self::from_key(SigningKey::from_bytes(&arr), false)
            }
            _ => Self::from_key(SigningKey::generate(&mut rand::rngs::OsRng), true),
        }
    }

    pub fn from_key(key: SigningKey, ephemeral: bool) -> Self {
        let pubk = key.verifying_key().to_bytes();
        Signer { public_b64: B64.encode(pubk), key_id: hex::encode(Sha256::digest(pubk))[..16].to_string(), key, ephemeral }
    }

    pub fn sign(&self, obj: &Value) -> Value {
        let c = canon(obj);
        let digest = hex::encode(Sha256::digest(&c));
        let sig = self.key.sign(&c);
        json!({"alg": "Ed25519", "key_id": self.key_id, "sha256": digest, "signature": B64.encode(sig.to_bytes())})
    }

    pub fn verify(obj: &Value, signature_b64: &str, public_b64: &str) -> bool {
        let Ok(pk) = B64.decode(public_b64) else { return false };
        let Ok(pk): Result<[u8; 32], _> = pk.try_into() else { return false };
        let Ok(vk) = VerifyingKey::from_bytes(&pk) else { return false };
        let Ok(sig) = B64.decode(signature_b64) else { return false };
        let Ok(sig) = Signature::from_slice(&sig) else { return false };
        vk.verify(&canon(obj), &sig).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signer_round_trip_and_tamper() {
        let s = Signer::from_key(SigningKey::generate(&mut rand::rngs::OsRng), true);
        let sig = s.sign(&json!({"a": 1}));
        let sv = sig["signature"].as_str().unwrap();
        assert!(Signer::verify(&json!({"a": 1}), sv, &s.public_b64));
        assert!(!Signer::verify(&json!({"a": 2}), sv, &s.public_b64));
    }

    #[test]
    fn nonce_rate_limit_roles() {
        let mut n = NonceCache::default();
        assert!(n.use_nonce(Some("abcdefgh1")) && !n.use_nonce(Some("abcdefgh1")) && !n.use_nonce(Some("short")));
        let mut r = RateLimiter::new(2, 60.0);
        assert!(r.allow("x") && r.allow("x") && !r.allow("x"));
        assert!(allowed("analyst", "approve_block") && !allowed("viewer", "hold"));
    }
}
