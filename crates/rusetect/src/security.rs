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

/// RUSETECT_ANALYST_KEYS: a JSON object {key: {"analyst": name, "role": role}} with at least one key, every key at least
/// 16 characters and every entry naming an analyst and a known role. Checked at start-up (check_config), so a bad value
/// stops the server instead of failing every request later.
pub fn parse_keys(raw: &str) -> Result<IndexMap<String, Analyst>, String> {
    let v: Value = serde_json::from_str(raw).map_err(|e| format!("not JSON: {e}"))?;
    let m = v.as_object().ok_or("not a JSON object")?;
    if m.is_empty() {
        return Err("no keys".into());
    }
    m.iter().map(|(k, a)| {
        let analyst = a["analyst"].as_str().filter(|s| !s.trim().is_empty()).ok_or(format!("key {}...: no analyst", &k[..k.len().min(4)]))?;
        let role = a["role"].as_str().filter(|r| ROLES.contains(r)).ok_or(format!("key {}...: role must be one of {ROLES:?}", &k[..k.len().min(4)]))?;
        if k.chars().count() < 16 {
            return Err(format!("key {}...: shorter than 16 characters", &k[..k.len().min(4)]));
        }
        Ok((k.clone(), Analyst { analyst: analyst.into(), role: role.into() }))
    }).collect()
}

/// viewer: read alerts and evidence; channel: a bank system calling the scoring and customer routes; analyst and
/// supervisor: review decisions (supervisor also resets demo state)
pub const ROLES: [&str; 4] = ["viewer", "channel", "analyst", "supervisor"];

pub fn load_keys() -> Keys {
    if let Ok(raw) = std::env::var("RUSETECT_ANALYST_KEYS") {
        let keys = parse_keys(&raw).expect("RUSETECT_ANALYST_KEYS is checked by check_config at start-up");
        return Keys { keys, demo_mode: false };
    }
    Keys { keys: DEMO_KEYS.iter().map(|(k, a, r)| (k.to_string(), Analyst { analyst: a.to_string(), role: r.to_string() })).collect(), demo_mode: true }
}

pub static KEYS: LazyLock<Keys> = LazyLock::new(load_keys);

pub fn analyst_for(key: Option<&str>) -> Option<Analyst> {
    KEYS.keys.get(key.unwrap_or("")).cloned()
}

/// API routes open without a key outside demo mode: liveness, the public signing key and signature checks, and model
/// documentation. Everything else under /api names accounts, changes state or costs CPU.
pub const PUBLIC_API: [&str; 7] = ["", "/", "/health", "/v2/keys/public", "/v2/verify", "/v2/model-card", "/v2/demo/analysts"];

/// POST routes that take a review decision or publish to the shared ledger: analyst or supervisor only.
const REVIEW_ROUTES: [&str; 2] = ["/v2/alerts/*/review", "/v2/consortium/publish"];

/// `path` against a pattern whose `*` segments stand for one path parameter (an alert id).
fn route_is(path: &str, pattern: &str) -> bool {
    let (mut a, mut b) = (path.split('/'), pattern.split('/'));
    loop {
        match (a.next(), b.next()) {
            (None, None) => return true,
            (Some(x), Some(y)) if y == "*" || x == y => {}
            _ => return false,
        }
    }
}

/// Access rule for one API request (`path` without the /api prefix). Demo mode is open, as the public demo needs.
/// Otherwise a known key is required and the role sets the reach: any role reads (GET); a write needs channel or
/// above, a review or publish needs an analyst or supervisor, and wiping state (demo reset, demo replay) a supervisor.
/// A write route not named here falls to the channel rule, so a new route is never open to a viewer (as for
/// /v2/sanctions/screen).
pub fn gate(keys: &Keys, method: &str, path: &str, key: Option<&str>) -> Result<(), (u16, &'static str)> {
    if keys.demo_mode || PUBLIC_API.contains(&path) {
        return Ok(());
    }
    let who = keys.keys.get(key.unwrap_or("")).ok_or((401, "missing or unknown X-Analyst-Key"))?;
    let role = who.role.as_str();
    if path.starts_with("/v2/demo/") {
        return if role == "supervisor" { Ok(()) } else { Err((403, "demo reset and replay need a supervisor key")) };
    }
    if matches!(method, "GET" | "HEAD" | "OPTIONS") {
        return Ok(());
    }
    if REVIEW_ROUTES.iter().any(|p| route_is(path, p)) {
        return if allowed(role, "hold") { Ok(()) } else { Err((403, "review and publish need an analyst or supervisor key")) };
    }
    if role == "viewer" {
        return Err((403, "a viewer key is read only"));
    }
    Ok(())
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
        if self.hits.len() > 50_000 {
            // bound memory: forget clients with no hit inside the window
            let per = self.per;
            self.hits.retain(|_, q| q.back().map(|t| *t > now - per).unwrap_or(false));
        }
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
    fn production_gate_and_key_validation() {
        let k = |r: &str| Analyst { analyst: "a".into(), role: r.into() };
        let keys = Keys { keys: ["viewer", "channel", "analyst", "supervisor"].iter().map(|r| (format!("k-{r}"), k(r))).collect(), demo_mode: false };
        assert!(gate(&keys, "GET", "/health", None).is_ok());
        assert_eq!(gate(&keys, "POST", "/v2/payment", None), Err((401, "missing or unknown X-Analyst-Key")));
        assert_eq!(gate(&keys, "GET", "/v2/campaigns", Some("nope")).unwrap_err().0, 401);
        assert!(gate(&keys, "GET", "/v2/alerts/A1/evidence", Some("k-viewer")).is_ok(), "a viewer reads");
        assert_eq!(gate(&keys, "POST", "/v2/payment", Some("k-viewer")).unwrap_err().0, 403, "a viewer never writes");
        assert_eq!(gate(&keys, "POST", "/v2/alerts/A1/customer", Some("k-viewer")).unwrap_err().0, 403);
        assert!(gate(&keys, "POST", "/v2/payment", Some("k-channel")).is_ok());
        assert!(gate(&keys, "POST", "/v2/alerts/A1/customer", Some("k-channel")).is_ok());
        assert!(gate(&keys, "POST", "/v2/sanctions/screen", Some("k-channel")).is_ok(), "screening is a channel route");
        assert_eq!(gate(&keys, "POST", "/v2/sanctions/screen", Some("k-viewer")).unwrap_err().0, 403);
        assert_eq!(gate(&keys, "POST", "/v2/alerts/A1/review", Some("k-channel")).unwrap_err().0, 403, "a channel never reviews");
        assert_eq!(gate(&keys, "POST", "/v2/consortium/publish", Some("k-channel")).unwrap_err().0, 403);
        assert!(gate(&keys, "POST", "/v2/alerts/A1/review", Some("k-analyst")).is_ok());
        assert!(gate(&keys, "POST", "/v2/payment", Some("k-analyst")).is_ok());
        assert!(gate(&keys, "POST", "/v2/consortium/publish", Some("k-supervisor")).is_ok());
        assert!(gate(&keys, "POST", "/v2/verify", None).is_ok(), "signature checks stay public");
        assert_eq!(gate(&keys, "POST", "/v2/demo/reset", Some("k-analyst")).unwrap_err().0, 403, "only a supervisor wipes state");
        assert!(gate(&keys, "POST", "/v2/demo/reset", Some("k-supervisor")).is_ok());
        assert!(gate(&Keys { keys: IndexMap::new(), demo_mode: true }, "POST", "/v2/demo/reset", None).is_ok(), "the public demo stays open");
        assert!(parse_keys(r#"{"0123456789abcdef": {"analyst": "asha", "role": "analyst"}}"#).is_ok());
        for bad in ["", "[]", "{}", r#"{"short": {"analyst": "a", "role": "analyst"}}"#, r#"{"0123456789abcdef": {"analyst": "a", "role": "root"}}"#] {
            assert!(parse_keys(bad).is_err(), "{bad:?} must be refused at start-up");
        }
    }

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
