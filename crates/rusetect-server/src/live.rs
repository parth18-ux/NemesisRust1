//! Live ingest: an outside system pushes a payment event to a signed webhook, and the same request scores it, decides
//! the action (allow / verify / hold / block), opens a review alert when a person must look, appends a ledger entry and
//! commits the result. No background worker is needed, so it runs as a serverless function (Vercel). See docs/live.md.
//!
//! Signature (Stripe style): X-RuseTect-Timestamp is epoch seconds; X-RuseTect-Signature is `v1=<hex>`, the
//! HMAC-SHA256 of `{timestamp}.{raw body}` under RUSETECT_WEBHOOK_SECRET (several comma separated `v1=` values are
//! accepted; during a rotation RUSETECT_WEBHOOK_SECRET_PREVIOUS is accepted too). Requests more than 5 minutes off the
//! server clock are refused.
//!
//! Store: with REDIS_URL or KV_URL (the rediss:// URL Vercel's Upstash Redis integration sets) the decision, the alert,
//! the ledger entry, the live feed and the per-account state (scale::redis_state, shared with the streaming workers) are
//! written to Redis, so they survive cold starts and are shared by every function instance. Without one everything is
//! kept in this process's memory and /api/health says `"durable": false`.
//!
//! Idempotency: the event_id is claimed before scoring; a repeated event_id returns the stored result unchanged (header
//! `Idempotent-Replayed: true`) and is not scored or written again.
//!
//! Concurrency (Redis mode): scoring and Redis I/O run outside the state mutex, on a small connection pool
//! (RUSETECT_LIVE_POOL, default 2 x logical CPUs). One payer's payments are scored one at a time (a payer stripe held
//! across the account-state read, write and pair window); the state mutex is held only for the in-process part (drift,
//! PRAMAAN, desk alert and chain); a decision that is not `allow` holds `ledger_order` from before that lock through the
//! commit, so live:ledger receives the desk's hash chain in order. Lock order: ledger_order, then state. Per event:
//! four Redis round trips (claim and seq, account-state read, account-state write with pair window, commit).
//! Memory mode stays one locked call.
use crate::inputs::AssessIn;
use crate::scale::{redis_state::RedisState, worker::score_one};
use crate::{assess, check_auth_request, header, parse_obj, payment_of, require_reader, AppRef, AppState, ApiError, PyJson};
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use hmac::{Hmac, Mac};
use indexmap::IndexMap;
use rusetect::context_rules::RULESET_VERSION;
use rusetect::engine::{decision_rank, DECISIONS};
use rusetect::pyfmt::{jf, py_round, utc_now_z};
use rusetect::review::hash_obj;
use rusetect::{paths, Engine};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

/// Largest clock difference accepted between the sender's timestamp and the server (seconds).
pub const TOLERANCE_S: i64 = 300;
/// Live decisions kept for the console feed.
const FEED: usize = 200;
/// Event ids remembered for idempotency in memory mode (oldest dropped first).
const MEM_EVENTS: usize = 100_000;
/// How long Redis keeps an event's result for idempotency, and how long an unfinished claim blocks a retry.
const EVENT_TTL_S: u64 = 7 * 86_400;
const CLAIM_TTL_S: u64 = 60;
/// Entries kept in `live:ledger` (oldest trimmed first).
pub const LEDGER_MAX: usize = 100_000;
pub const ACTIONS: [&str; 4] = ["allow", "verify", "hold", "block"];
const STRIPES: usize = 64;
/// Claim the event id and take its sequence number in one round trip; a known id returns what is stored (or "pending").
const CLAIM_AND_SEQ: &str = "if redis.call('SET', KEYS[1], 'pending', 'NX', 'EX', ARGV[1]) then return {1, tostring(redis.call('INCR', KEYS[2]))} end \
    return {0, redis.call('GET', KEYS[1])}";

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Check the webhook signature over the raw body against any of `secrets` (current, then previous). `now` is epoch seconds.
pub fn verify_signature(secrets: &[&[u8]], ts: Option<&str>, sig: Option<&str>, body: &[u8], now: i64) -> Result<(), ApiError> {
    let ts = ts.ok_or_else(|| ApiError::new(401, "missing X-RuseTect-Timestamp"))?;
    let t: i64 = ts.trim().parse().map_err(|_| ApiError::new(401, "X-RuseTect-Timestamp must be epoch seconds"))?;
    if now.abs_diff(t) > TOLERANCE_S as u64 {
        return Err(ApiError::new(401, "X-RuseTect-Timestamp is more than 5 minutes from the server clock"));
    }
    let sig = sig.ok_or_else(|| ApiError::new(401, "missing X-RuseTect-Signature"))?;
    let sigs: Vec<Vec<u8>> = sig.split(',').filter_map(|p| p.trim().strip_prefix("v1=")).filter_map(|h| hex::decode(h).ok()).collect();
    let good = secrets.iter().any(|secret| {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC takes any key length");
        mac.update(ts.as_bytes());
        mac.update(b".");
        mac.update(body);
        // verify_slice compares in constant time
        sigs.iter().any(|b| mac.clone().verify_slice(b).is_ok())
    });
    if good { Ok(()) } else { Err(ApiError::new(401, "bad signature")) }
}

pub struct Durable {
    client: redis::Client,
    px: String,
    /// idle connections; a connection goes back only after a command on it succeeded
    idle: Mutex<Vec<RedisState>>,
    /// webhook requests doing Redis work at once (RUSETECT_LIVE_POOL, default 2 x logical CPUs); also the pool's size
    slots: tokio::sync::Semaphore,
    size: usize,
    /// result of the last command in this instance: 0 none yet, 1 reachable, 2 unreachable
    reachable: AtomicU8,
    /// stateless engine for the Redis scoring path (account state lives in Redis), built on first use as Scorer::new does
    eng: OnceLock<Arc<Engine>>,
    /// one payer's payments are scored one at a time: read, write and pair window see each other's effects
    payer_stripes: [Mutex<()>; STRIPES],
    /// held from before the state lock through the commit for decisions the desk chains, so live:ledger keeps chain order
    ledger_order: Mutex<()>,
}

impl Durable {
    fn new(client: redis::Client, px: &str) -> Self {
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2);
        let size = std::env::var("RUSETECT_LIVE_POOL").ok().and_then(|v| v.trim().parse().ok()).filter(|&n: &usize| n > 0).unwrap_or(2 * cores);
        Durable { client, px: px.into(), idle: Mutex::new(vec![]), slots: tokio::sync::Semaphore::new(size), size, reachable: AtomicU8::new(0),
                  eng: OnceLock::new(), payer_stripes: std::array::from_fn(|_| Mutex::new(())), ledger_order: Mutex::new(()) }
    }

    fn key(&self, k: &str) -> String {
        format!("{}live:{k}", self.px)
    }

    fn engine(&self) -> redis::RedisResult<Arc<Engine>> {
        if let Some(e) = self.eng.get() {
            return Ok(e.clone());
        }
        let e = Engine::new(&paths::models_dir(), true)
            .map_err(|e| redis::RedisError::from((redis::ErrorKind::ClientError, "scoring engine unavailable", format!("{e:#}"))))?;
        Ok(self.eng.get_or_init(|| Arc::new(e)).clone())
    }

    fn connect(&self) -> redis::RedisResult<RedisState> {
        let cfg = self.engine()?.cfg.clone();
        let con = self.client.get_connection_with_timeout(Duration::from_secs(5))?;
        con.set_read_timeout(Some(Duration::from_secs(5)))?;
        con.set_write_timeout(Some(Duration::from_secs(5)))?;
        Ok(RedisState::new(con, cfg, &self.px))
    }

    fn call<T>(&self, con: &mut Option<RedisState>, f: &mut impl FnMut(&mut RedisState) -> redis::RedisResult<T>) -> redis::RedisResult<T> {
        if con.is_none() {
            *con = Some(self.connect()?);
        }
        f(con.as_mut().unwrap())
    }

    /// Run `f` on a pooled connection. A warm function instance outlives idle sockets, so when `retry` is set and a pooled
    /// connection turns out dropped, reconnect once and run `f` again (no PING per request: the free plan counts every
    /// command). A connection goes back to the pool only after success; one with an I/O error (a timeout included: its
    /// late reply would desync it) is dropped.
    fn run<T>(&self, retry: bool, mut f: impl FnMut(&mut RedisState) -> redis::RedisResult<T>) -> redis::RedisResult<T> {
        let mut con = lock(&self.idle).pop();
        let kept = con.is_some();
        let mut r = self.call(&mut con, &mut f);
        if let Err(e) = &r {
            if e.is_io_error() || e.is_connection_dropped() {
                con = None;
                if retry && kept && !e.is_timeout() {
                    r = self.call(&mut con, &mut f);
                }
            }
        }
        let down = r.as_ref().err().is_some_and(|e| e.is_io_error() || e.is_connection_dropped() || e.is_connection_refusal());
        self.reachable.store(if down { 2 } else { 1 }, Ordering::Relaxed);
        if let (Ok(_), Some(c)) = (&r, con) {
            let mut idle = lock(&self.idle);
            if idle.len() < self.size {
                idle.push(c);
            }
        }
        r
    }

    /// Delete an unfinished claim so the sender's retry is processed (account writes are deduped by seen:{eid}).
    fn release(&self, key: &str) {
        let _ = self.run(true, |st| redis::cmd("DEL").arg(key).query::<i64>(&mut st.con));
    }

    fn payer_stripe(&self, d: &rusetect::payment::Payment) -> MutexGuard<'_, ()> {
        let mut h = DefaultHasher::new();
        (&d.from_bank, &d.from_account).hash(&mut h);
        lock(&self.payer_stripes[(h.finish() % STRIPES as u64) as usize])
    }
}

#[derive(Default)]
pub struct Live {
    pub redis: Option<Arc<Durable>>,
    /// why the store is in memory although something looked configured
    pub note: Option<String>,
    events: IndexMap<String, Value>,
    feed: VecDeque<Value>,
    seq: u64,
}

impl Live {
    /// REDIS_URL, else KV_URL (both rediss:// with Upstash); RUSETECT_REDIS_PREFIX namespaces the keys.
    pub fn from_env() -> Self {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        match var("REDIS_URL").or_else(|| var("KV_URL")) {
            Some(url) => Live::redis(&url, &var("RUSETECT_REDIS_PREFIX").unwrap_or_else(|| "rusetect:".into())).unwrap_or_else(|e| {
                eprintln!("live store: {e}; keeping decisions in memory");
                Live { note: Some(format!("Redis URL rejected ({e}); in memory")), ..Default::default() }
            }),
            None if var("UPSTASH_REDIS_REST_URL").or_else(|| var("KV_REST_API_URL")).is_some() => Live {
                note: Some("only Upstash REST variables are set; connect the database so REDIS_URL or KV_URL (rediss://) is set".into()),
                ..Default::default()
            },
            None => Live::default(),
        }
    }

    pub fn redis(url: &str, prefix: &str) -> redis::RedisResult<Self> {
        Ok(Live { redis: Some(Arc::new(Durable::new(redis::Client::open(url)?, prefix))), ..Default::default() })
    }

    /// A Redis store is configured.
    pub fn configured(&self) -> bool {
        self.redis.is_some()
    }

    /// Configured and its last command in this instance did not fail to connect (unknown before the first one).
    pub fn durable(&self) -> bool {
        self.redis.as_ref().is_some_and(|d| d.reachable.load(Ordering::Relaxed) != 2)
    }

    pub fn store_json(&self) -> Value {
        json!({"kind": if self.configured() { "redis" } else { "memory" }, "configured": self.configured(),
               "reachable": self.redis.as_ref().and_then(|d| match d.reachable.load(Ordering::Relaxed) { 0 => None, r => Some(r == 1) }),
               "note": self.note})
    }

    /// Latest live decisions, newest first.
    pub fn feed(&self, n: usize) -> Result<Vec<Value>, ApiError> {
        let Some(d) = self.redis.as_ref() else { return Ok(self.feed.iter().take(n).cloned().collect()) };
        let key = d.key("feed");
        let rows: Vec<String> = d.run(true, |st| redis::cmd("LRANGE").arg(&key).arg(0).arg(n as i64 - 1).query(&mut st.con)).map_err(store_err)?;
        Ok(rows.iter().filter_map(|r| serde_json::from_str(r).ok()).collect())
    }
}

fn store_err(e: redis::RedisError) -> ApiError {
    ApiError::new(503, format!("durable store unavailable: {e}"))
}

/// A parsed payment event: the /v2/assess body plus event_id and an optional PRAMAAN request.
pub struct Event {
    pub id: String,
    pub a: AssessIn,
    pub auth: Option<Value>,
}

/// Stable numeric id for the Redis account-state dedupe key (seen:{eid}).
fn eid_of(id: &str) -> i64 {
    let h = Sha256::digest(id.as_bytes());
    i64::from_be_bytes(h[..8].try_into().unwrap()) & i64::MAX
}

fn action_of(decision: &str) -> &'static str {
    ACTIONS[decision_rank(decision)]
}

/// Memory mode: score, decide, act and commit one event in one locked call. Returns (stored result, replayed).
fn ingest(s: &mut AppState, ev: &Event) -> Result<(Value, bool), ApiError> {
    let t0 = Instant::now();
    let d = payment_of(&ev.a.payment)?;
    s.need_engine()?;
    // 1. idempotency: a known event id returns what was stored
    if let Some(r) = s.live.events.get(&ev.id) {
        return Ok((r.clone(), true));
    }
    s.live.seq += 1;
    let seq = s.live.seq;
    let (mut rec, _, _) = decide(s, ev, &d, seq, None)?;
    rec["latency_ms"] = jf(py_round(t0.elapsed().as_secs_f64() * 1000.0, 2));
    // 5. commit
    s.live.events.insert(ev.id.clone(), rec.clone());
    if s.live.events.len() > MEM_EVENTS {
        s.live.events.shift_remove_index(0);
    }
    s.live.feed.push_front(rec.clone());
    s.live.feed.truncate(FEED);
    Ok((rec, false))
}

/// Redis mode: the same steps in three phases, the state mutex held only for the second (module docs). `ready` is the
/// engine check made under the state lock. Returns (stored result, replayed).
fn ingest_redis(app: &crate::Shared, du: &Durable, ev: &Event, ready: Result<(), ApiError>) -> Result<(Value, bool), ApiError> {
    let t0 = Instant::now();
    let d = payment_of(&ev.a.payment)?;
    ready?;
    let eng = du.engine().map_err(store_err)?;
    // 1. idempotency: a known event id returns what was stored; a new one is claimed (and numbered) before any work
    let key = du.key(&format!("ev:{}", ev.id));
    let seq_key = du.key("seq");
    let claim = |st: &mut RedisState| redis::cmd("EVAL").arg(CLAIM_AND_SEQ).arg(2).arg(&key).arg(&seq_key).arg(CLAIM_TTL_S).query(&mut st.con);
    let (claimed, v): (i64, Option<String>) = du.run(true, claim).map_err(store_err)?;
    if claimed == 0 {
        return match v.as_deref().map(serde_json::from_str::<Value>) {
            Some(Ok(r)) => Ok((r, true)),
            _ => Err(ApiError::new(409, "this event_id is still being processed; retry in a minute")),
        };
    }
    let seq: u64 = v.and_then(|n| n.parse().ok()).unwrap_or(0);
    // P1, no state lock: 2. score on the bank-scale path, account state and pair window in Redis (one payer at a time)
    let mut p = d.clone();
    p.eid = Some(eid_of(&ev.id));
    let scored = {
        let _payer = du.payer_stripe(&d);
        du.run(true, |st| score_one(&eng, st, &p))
    };
    let r = match scored {
        Ok(r) => r,
        Err(e) => {
            du.release(&key);
            return Err(store_err(e));
        }
    };
    // P2, state lock: 3-4 (PRAMAAN only raises, so an allow without a PRAMAAN request stays unchained)
    let chained = decision_rank(r["final_decision"].as_str().unwrap_or("Approve")) > 0 || ev.auth.is_some();
    let _order = chained.then(|| lock(&du.ledger_order));
    let res = decide(&mut lock(&app.state), ev, &d, seq, Some(&r));
    let (mut rec, alert, entry) = match res {
        Ok(x) => x,
        Err(e) => {
            du.release(&key);
            return Err(e);
        }
    };
    rec["latency_ms"] = jf(py_round(t0.elapsed().as_secs_f64() * 1000.0, 2));
    // P3, no state lock: 5. commit
    let (ek, fk, ak, lk) = (du.key(&format!("ev:{}", ev.id)), du.key("feed"), du.key("alerts"), du.key("ledger"));
    let body = rec.to_string();
    let mut pl = redis::pipe();
    pl.atomic().cmd("SET").arg(&ek).arg(&body).arg("EX").arg(EVENT_TTL_S).ignore()
        .cmd("LPUSH").arg(&fk).arg(&body).ignore()
        .cmd("LTRIM").arg(&fk).arg(0).arg(FEED - 1).ignore()
        .cmd("RPUSH").arg(&lk).arg(entry.to_string()).ignore()
        .cmd("LTRIM").arg(&lk).arg(-(LEDGER_MAX as i64)).arg(-1).ignore();
    if let Some(a) = &alert {
        pl.cmd("HSET").arg(&ak).arg(a["alert_id"].as_str().unwrap_or("")).arg(a.to_string()).ignore();
    }
    // no retry: a transaction whose reply was lost may have been applied
    if let Err(e) = du.run(false, |st| pl.query::<()>(&mut st.con)) {
        // look before releasing the claim: a stored result means the commit was applied (a reply timeout)
        match du.run(true, |st| redis::cmd("GET").arg(&ek).query::<Option<String>>(&mut st.con)) {
            Ok(Some(v)) if v != "pending" => return Ok((serde_json::from_str(&v).unwrap_or(rec), false)),
            Ok(_) => du.release(&ek),
            Err(_) => {} // unknown: the claim expires after CLAIM_TTL_S, so a retry cannot commit twice
        }
        return Err(store_err(e));
    }
    Ok((rec, false))
}

/// 2-4: score with the engine, apply PRAMAAN, open the alert, append the ledger entry. Returns (record, alert, entry).
/// `scored` is the bank-scale result in Redis mode (scored before the state lock), None in memory mode.
fn decide(s: &mut AppState, ev: &Event, d: &rusetect::payment::Payment, seq: u64, scored: Option<&Value>) -> Result<(Value, Option<Value>, Value), ApiError> {
    let inputs = ev.a.dump_without_message();
    // 2. score: in memory the full /v2/assess path (cascade, hardening, pair window, lures, context, session, alert);
    // with Redis the bank-scale path, whose account state and pair window live in Redis and are shared by every instance
    // (it returns no reasons, so SHAP and temporal-motif reasons are memory-mode only: the motif pass reads other
    // accounts' windows, which would cost extra Redis round trips per payment)
    let (mut assessed, mut alert_id, reasons) = match scored {
        None => {
            let out = assess(s, &ev.a, d)?.1;
            (json!({"final_decision": out["final_decision"], "payment_decision": out["payment_decision"], "risk": out["payment"]["risk"],
                    "context_band": out["context_band"], "context": {"ruleset": out["context"]["ruleset"]}}),
             out["alert_id"].as_str().map(String::from), out["payment"]["reasons"].clone())
        }
        Some(r) => {
            if let Some(m) = s.drift.as_mut() {
                m.observe(r["risk"].as_f64(), r["payment_decision"].as_str().unwrap_or(""), None); // no features on this path
            }
            (json!({"final_decision": r["final_decision"], "payment_decision": r["payment_decision"], "risk": r["risk"],
                    "context_band": r["context_band"], "context": {"ruleset": RULESET_VERSION}}), None, json!([]))
        }
    };
    // 3. PRAMAAN account-takeover check when the event carries one: challenge asks for verification, block blocks
    let mut pramaan = Value::Null;
    if let (Some(req), Some(a)) = (ev.auth.as_ref(), s.auth.as_mut()) {
        let r = a.authenticate(req, format!("ev-{}", ev.id));
        let rank = match r["decision"].as_str() { Some("block") => 3, Some("challenge") => 1, _ => 0 };
        let cur = decision_rank(assessed["final_decision"].as_str().unwrap_or("Approve"));
        if rank > cur {
            assessed["final_decision"] = json!(DECISIONS[rank]);
            if let Some(aid) = &alert_id {
                s.desk.set_field(aid, "shown_decision", json!(if rank == 3 { "Hold + recommend block" } else { DECISIONS[rank] }));
            }
        }
        pramaan = json!({"decision": r["decision"], "decision_id": r["decision_id"], "risk_score": r["risk_score"], "raised": rank > cur});
    }
    // 4. act: a person looks at anything above allow (the desk opens the alert); every decision gets a ledger entry
    if alert_id.is_none() {
        alert_id = s.desk.open_alert_as(format!("L{seq:06}"), &inputs, &assessed).and_then(|a| a["alert_id"].as_str().map(String::from));
    }
    let final_decision = assessed["final_decision"].as_str().unwrap_or("Approve").to_string();
    let action = action_of(&final_decision);
    let payment: serde_json::Map<String, Value> = ["timestamp", "from_bank", "from_account", "to_bank", "to_account", "amount_paid", "payment_currency", "payment_format"]
        .iter().map(|k| (k.to_string(), inputs.get(*k).cloned().unwrap_or(Value::Null))).collect();
    let event = json!({"type": "live_decision", "event_id": ev.id, "event_seq": seq, "action": action, "final_decision": final_decision,
                       "alert_id": alert_id, "inputs_hash": hash_obj(&Value::Object(payment.clone()))});
    let entry = if s.live.configured() && action == "allow" {
        // with Redis, allow decisions go to live:ledger only (seq null): this instance's chain would otherwise grow
        // with every payment, and the desk log cannot be trimmed without breaking its hash chain
        let mut e = event;
        e["at"] = json!(utc_now_z());
        e["seq"] = Value::Null;
        e["hash"] = json!(hash_obj(&e));
        e
    } else {
        s.desk.append(event)
    };
    if let Some(aid) = &alert_id {
        s.desk.set_field(aid, "event_id", json!(ev.id));
    }
    let alert = alert_id.as_ref().and_then(|a| s.desk.alerts.get(a)).cloned();
    let durable = s.live.configured();
    let rec = json!({"event_id": ev.id, "seq": seq, "source": "live", "received_at": utc_now_z(), "action": action,
                     "final_decision": final_decision, "payment_decision": assessed["payment_decision"], "risk": assessed["risk"],
                     "context_band": assessed["context_band"], "reasons": reasons, "pramaan": pramaan, "alert_id": alert_id,
                     "ledger": {"seq": entry["seq"], "hash": entry["hash"]}, "payment": payment, "durable": durable,
                     "store": if durable { "redis" } else { "memory" }});
    Ok((rec, alert, entry))
}

/// With Redis, `live:alerts` is the authority for live alerts: load the alert into this instance's desk before it is read
/// or acted on (refreshing a copy already there, since another instance may have acted on it). No-op in memory mode.
pub fn load_alert(s: &mut AppState, id: &str) -> Result<(), ApiError> {
    if !s.live.configured() {
        return Ok(());
    }
    s.need_engine()?;
    let du = s.live.redis.clone().unwrap();
    let k = du.key("alerts");
    let v: Option<String> = du.run(true, |st| redis::cmd("HGET").arg(&k).arg(id).query(&mut st.con)).map_err(store_err)?;
    if let Some(a) = v.and_then(|v| serde_json::from_str::<Value>(&v).ok()) {
        s.desk.alerts.insert(id.to_string(), a);
    }
    Ok(())
}

/// Write a live alert back to `live:alerts` after an analyst action (Redis mode; other alerts are left alone).
pub fn save_alert(s: &mut AppState, id: &str) -> Result<(), ApiError> {
    let Some(a) = s.desk.alerts.get(id).filter(|a| a.get("event_id").is_some()).map(|a| a.to_string()) else { return Ok(()) };
    if !s.live.configured() {
        return Ok(());
    }
    s.need_engine()?;
    let du = s.live.redis.clone().unwrap();
    let k = du.key("alerts");
    du.run(true, |st| redis::cmd("HSET").arg(&k).arg(id).arg(&a).query::<i64>(&mut st.con)).map_err(store_err)?;
    Ok(())
}

/// The desk's alerts newest first, then (Redis mode) live alerts only in `live:alerts`, newest id first. A live alert
/// in both is shown as stored in Redis.
pub fn all_alerts(s: &mut AppState) -> Result<Vec<Value>, ApiError> {
    let mut stored: IndexMap<String, Value> = IndexMap::new();
    if s.live.configured() {
        s.need_engine()?;
        let du = s.live.redis.clone().unwrap();
        let k = du.key("alerts");
        let m: Vec<(String, String)> = du.run(true, |st| redis::cmd("HGETALL").arg(&k).query(&mut st.con)).map_err(store_err)?;
        stored = m.into_iter().filter_map(|(id, v)| serde_json::from_str(&v).ok().map(|a| (id, a))).collect();
        stored.sort_by(|a, _, b, _| b.cmp(a));
    }
    let mut out: Vec<Value> = s.desk.alerts.iter().rev().map(|(id, a)| stored.shift_remove(id).unwrap_or_else(|| a.clone())).collect();
    out.extend(stored.into_values());
    Ok(out)
}

/// POST /api/v2/events/payment
pub async fn v2_events_payment(State(app): State<AppRef>, h: HeaderMap, b: Bytes) -> Response {
    match events_payment(app, h, b).await {
        Ok(r) => r,
        Err(e) => e.into_response(),
    }
}

async fn events_payment(app: AppRef, h: HeaderMap, b: Bytes) -> Result<Response, ApiError> {
    let secret = std::env::var("RUSETECT_WEBHOOK_SECRET").unwrap_or_default();
    if secret.trim().len() < 16 {
        return Err(ApiError::new(503, "ingest webhook disabled: set RUSETECT_WEBHOOK_SECRET (16+ characters)"));
    }
    // rotation: the old secret stays valid while senders switch; a short or empty value is ignored
    let previous = std::env::var("RUSETECT_WEBHOOK_SECRET_PREVIOUS").unwrap_or_default();
    let mut secrets = vec![secret.trim().as_bytes()];
    if previous.trim().len() >= 16 {
        secrets.push(previous.trim().as_bytes());
    }
    let now = chrono::Utc::now().timestamp();
    verify_signature(&secrets, header(&h, "x-rusetect-timestamp").as_deref(), header(&h, "x-rusetect-signature").as_deref(), &b, now)?;
    let ev = parse_obj(&b, |o, e| {
        let id = o.req_str(e, "event_id");
        if o.get("event_id").is_some_and(|v| v.is_string()) {
            o.check_len(e, "event_id", &id, Some(1), Some(128));
        }
        Event { id, a: AssessIn::parse(o, e), auth: o.get("auth").filter(|v| !v.is_null()).cloned() }
    })?;
    if let Some(a) = &ev.auth {
        check_auth_request(a).map_err(|e| ApiError { detail: json!(format!("auth.{}", e.detail.as_str().unwrap_or(""))), ..e })?;
    }
    // the store as configured now (tests swap it), read under a brief lock; Redis work waits for a pool slot
    let (du, ready) = {
        let mut st = lock(&app.state);
        (st.live.redis.clone(), st.need_engine().map(|_| ()))
    };
    let _slot = match &du {
        Some(d) => Some(d.slots.acquire().await.map_err(|e| ApiError::new(500, format!("internal error: {e}")))?),
        None => None,
    };
    let job = du.clone();
    let (rec, replayed) = tokio::task::spawn_blocking(move || match job {
        Some(du) => ingest_redis(&app, &du, &ev, ready),
        None => ingest(&mut lock(&app.state), &ev),
    }).await.map_err(|e| ApiError::new(500, format!("internal error: {e}")))??;
    let mut r = PyJson(StatusCode::OK, rec).into_response();
    if replayed {
        r.headers_mut().insert("idempotent-replayed", HeaderValue::from_static("true"));
    }
    Ok(r)
}

/// GET /api/v2/live: the latest live decisions (newest first) for the console, which polls it.
pub async fn v2_live(State(app): State<AppRef>, h: HeaderMap) -> Result<PyJson, ApiError> {
    require_reader(&h)?;
    tokio::task::spawn_blocking(move || {
        let mut st = lock(&app.state);
        st.need_engine()?;
        let events = st.live.feed(50)?;
        Ok(PyJson(StatusCode::OK, json!({"durable": st.live.durable(), "store": st.live.store_json(), "events": events})))
    }).await.map_err(|e| ApiError::new(500, format!("internal error: {e}")))?
}
