//! Live ingest: an outside system pushes a payment event to a signed webhook, and the same request scores it, decides
//! the action (allow / verify / hold / block), opens a review alert when a person must look, appends a ledger entry and
//! commits the result. No background worker is needed, so it runs as a serverless function (Vercel). See docs/live.md.
//!
//! Signature (Stripe style): X-RuseTect-Timestamp is epoch seconds; X-RuseTect-Signature is `v1=<hex>`, the
//! HMAC-SHA256 of `{timestamp}.{raw body}` under RUSETECT_WEBHOOK_SECRET (several comma separated `v1=` values are
//! accepted, for secret rotation). Requests more than 5 minutes off the server clock are refused.
//!
//! Store: with REDIS_URL or KV_URL (the rediss:// URL Vercel's Upstash Redis integration sets) the decision, the alert,
//! the ledger entry, the live feed and the per-account state (scale::redis_state, shared with the streaming workers) are
//! written to Redis, so they survive cold starts and are shared by every function instance. Without one everything is
//! kept in this process's memory and /api/health says `"durable": false`.
//!
//! Idempotency: the event_id is claimed before scoring; a repeated event_id returns the stored result unchanged (header
//! `Idempotent-Replayed: true`) and is not scored or written again.
//!
//! Redis I/O happens while the state mutex is held, so one instance serves one request at a time; acceptable on Vercel,
//! where a function instance serves one request at a time anyway.
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
use rusetect::engine::{decision_rank, CascadeConfig, DECISIONS};
use rusetect::pyfmt::{jf, py_round, utc_now_z};
use rusetect::review::hash_obj;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
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

/// Check the webhook signature over the raw body. `now` is epoch seconds.
pub fn verify_signature(secret: &[u8], ts: Option<&str>, sig: Option<&str>, body: &[u8], now: i64) -> Result<(), ApiError> {
    let ts = ts.ok_or_else(|| ApiError::new(401, "missing X-RuseTect-Timestamp"))?;
    let t: i64 = ts.trim().parse().map_err(|_| ApiError::new(401, "X-RuseTect-Timestamp must be epoch seconds"))?;
    if now.abs_diff(t) > TOLERANCE_S as u64 {
        return Err(ApiError::new(401, "X-RuseTect-Timestamp is more than 5 minutes from the server clock"));
    }
    let sig = sig.ok_or_else(|| ApiError::new(401, "missing X-RuseTect-Signature"))?;
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC takes any key length");
    mac.update(ts.as_bytes());
    mac.update(b".");
    mac.update(body);
    // verify_slice compares in constant time
    let good = sig.split(',').filter_map(|p| p.trim().strip_prefix("v1=")).filter_map(|h| hex::decode(h).ok())
        .any(|b| mac.clone().verify_slice(&b).is_ok());
    if good { Ok(()) } else { Err(ApiError::new(401, "bad signature")) }
}

pub struct Durable {
    client: redis::Client,
    state: Option<RedisState>,
    px: String,
    /// result of the last command in this instance (None until the first one)
    reachable: Option<bool>,
}

impl Durable {
    fn key(&self, k: &str) -> String {
        format!("{}live:{k}", self.px)
    }

    fn connect(&mut self, cfg: &CascadeConfig) -> redis::RedisResult<&mut RedisState> {
        if self.state.is_none() {
            let con = self.client.get_connection_with_timeout(Duration::from_secs(5))?;
            con.set_read_timeout(Some(Duration::from_secs(5)))?;
            con.set_write_timeout(Some(Duration::from_secs(5)))?;
            self.state = Some(RedisState::new(con, cfg.clone(), &self.px));
        }
        Ok(self.state.as_mut().unwrap())
    }

    /// Run `f` on the kept connection. A warm function instance outlives idle sockets, so when `retry` is set and a kept
    /// connection turns out dropped, reconnect once and run `f` again (no PING per request: the free plan counts every
    /// command). A connection with an I/O error (a timeout included: its late reply would desync it) is dropped.
    fn run<T>(&mut self, cfg: &CascadeConfig, retry: bool, mut f: impl FnMut(&mut RedisState) -> redis::RedisResult<T>) -> redis::RedisResult<T> {
        let kept = self.state.is_some();
        let mut r = self.connect(cfg).and_then(&mut f);
        if let Err(e) = &r {
            if e.is_io_error() || e.is_connection_dropped() {
                self.state = None;
                if retry && kept && !e.is_timeout() {
                    r = self.connect(cfg).and_then(&mut f);
                }
            }
        }
        let down = r.as_ref().err().is_some_and(|e| e.is_io_error() || e.is_connection_dropped() || e.is_connection_refusal());
        if down {
            self.state = None;
        }
        self.reachable = Some(!down);
        r
    }

    /// Delete an unfinished claim so the sender's retry is processed (account writes are deduped by seen:{eid}).
    fn release(&mut self, cfg: &CascadeConfig, key: &str) {
        let _ = self.run(cfg, true, |st| redis::cmd("DEL").arg(key).query::<i64>(&mut st.con));
    }
}

#[derive(Default)]
pub struct Live {
    pub redis: Option<Durable>,
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
        Ok(Live { redis: Some(Durable { client: redis::Client::open(url)?, state: None, px: prefix.into(), reachable: None }), ..Default::default() })
    }

    /// A Redis store is configured.
    pub fn configured(&self) -> bool {
        self.redis.is_some()
    }

    /// Configured and its last command in this instance did not fail to connect (unknown before the first one).
    pub fn durable(&self) -> bool {
        self.redis.as_ref().is_some_and(|d| d.reachable != Some(false))
    }

    pub fn store_json(&self) -> Value {
        json!({"kind": if self.configured() { "redis" } else { "memory" }, "configured": self.configured(),
               "reachable": self.redis.as_ref().and_then(|d| d.reachable), "note": self.note})
    }

    /// Latest live decisions, newest first.
    pub fn feed(&mut self, n: usize, cfg: &CascadeConfig) -> Result<Vec<Value>, ApiError> {
        let Some(d) = self.redis.as_mut() else { return Ok(self.feed.iter().take(n).cloned().collect()) };
        let key = d.key("feed");
        let rows: Vec<String> = d.run(cfg, true, |st| redis::cmd("LRANGE").arg(&key).arg(0).arg(n as i64 - 1).query(&mut st.con)).map_err(store_err)?;
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

/// Score, decide, act and commit one event. Returns (stored result, replayed).
pub fn ingest(s: &mut AppState, ev: &Event) -> Result<(Value, bool), ApiError> {
    let t0 = Instant::now();
    let d = payment_of(&ev.a.payment)?;
    let cfg = s.need_engine()?.cfg.clone();
    // 1. idempotency: a known event id returns what was stored; a new one is claimed before any work
    let seq = match s.live.redis.as_mut() {
        None => {
            if let Some(r) = s.live.events.get(&ev.id) {
                return Ok((r.clone(), true));
            }
            s.live.seq += 1;
            s.live.seq
        }
        Some(du) => {
            let key = du.key(&format!("ev:{}", ev.id));
            let seq_key = du.key("seq");
            let claim = |st: &mut RedisState| redis::cmd("SET").arg(&key).arg("pending").arg("NX").arg("EX").arg(CLAIM_TTL_S).query(&mut st.con);
            let claimed: Option<String> = du.run(&cfg, true, claim).map_err(store_err)?;
            if claimed.is_none() {
                let v: Option<String> = du.run(&cfg, true, |st| redis::cmd("GET").arg(&key).query(&mut st.con)).map_err(store_err)?;
                return match v.as_deref().map(serde_json::from_str::<Value>) {
                    Some(Ok(r)) => Ok((r, true)),
                    _ => Err(ApiError::new(409, "this event_id is still being processed; retry in a minute")),
                };
            }
            match du.run(&cfg, true, |st| redis::cmd("INCR").arg(&seq_key).query::<u64>(&mut st.con)) {
                Ok(n) => n,
                Err(e) => {
                    du.release(&cfg, &key);
                    return Err(store_err(e));
                }
            }
        }
    };
    let res = decide(s, ev, &d, seq);
    if let (Err(_), Some(du)) = (&res, s.live.redis.as_mut()) {
        let key = du.key(&format!("ev:{}", ev.id));
        du.release(&cfg, &key);
    }
    let (mut rec, alert, entry) = res?;
    rec["latency_ms"] = jf(py_round(t0.elapsed().as_secs_f64() * 1000.0, 2));
    // 5. commit
    match s.live.redis.as_mut() {
        None => {
            s.live.events.insert(ev.id.clone(), rec.clone());
            if s.live.events.len() > MEM_EVENTS {
                s.live.events.shift_remove_index(0);
            }
            s.live.feed.push_front(rec.clone());
            s.live.feed.truncate(FEED);
        }
        Some(du) => {
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
            if let Err(e) = du.run(&cfg, false, |st| pl.query::<()>(&mut st.con)) {
                // look before releasing the claim: a stored result means the commit was applied (a reply timeout)
                match du.run(&cfg, true, |st| redis::cmd("GET").arg(&ek).query::<Option<String>>(&mut st.con)) {
                    Ok(Some(v)) if v != "pending" => return Ok((serde_json::from_str(&v).unwrap_or(rec), false)),
                    Ok(_) => du.release(&cfg, &ek),
                    Err(_) => {} // unknown: the claim expires after CLAIM_TTL_S, so a retry cannot commit twice
                }
                return Err(store_err(e));
            }
        }
    }
    Ok((rec, false))
}

/// 2-4: score with the engine, apply PRAMAAN, open the alert, append the ledger entry. Returns (record, alert, entry).
fn decide(s: &mut AppState, ev: &Event, d: &rusetect::payment::Payment, seq: u64) -> Result<(Value, Option<Value>, Value), ApiError> {
    let inputs = ev.a.dump_without_message();
    // 2. score: in memory the full /v2/assess path (cascade, hardening, pair window, lures, context, session, alert);
    // with Redis the bank-scale path, whose account state and pair window live in Redis and are shared by every instance
    let (mut assessed, mut alert_id, reasons) = match s.live.redis.as_mut() {
        None => {
            let out = assess(s, &ev.a, d)?.1;
            (json!({"final_decision": out["final_decision"], "payment_decision": out["payment_decision"], "risk": out["payment"]["risk"],
                    "context_band": out["context_band"], "context": {"ruleset": out["context"]["ruleset"]}}),
             out["alert_id"].as_str().map(String::from), out["payment"]["reasons"].clone())
        }
        Some(du) => {
            let eng = s.engine.as_ref().unwrap();
            let mut p = d.clone();
            p.eid = Some(eid_of(&ev.id));
            let r = du.run(&eng.cfg, true, |st| score_one(eng, st, &p)).map_err(store_err)?;
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
    let cfg = s.need_engine()?.cfg.clone();
    let du = s.live.redis.as_mut().unwrap();
    let k = du.key("alerts");
    let v: Option<String> = du.run(&cfg, true, |st| redis::cmd("HGET").arg(&k).arg(id).query(&mut st.con)).map_err(store_err)?;
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
    let cfg = s.need_engine()?.cfg.clone();
    let du = s.live.redis.as_mut().unwrap();
    let k = du.key("alerts");
    du.run(&cfg, true, |st| redis::cmd("HSET").arg(&k).arg(id).arg(&a).query::<i64>(&mut st.con)).map_err(store_err)?;
    Ok(())
}

/// The desk's alerts newest first, then (Redis mode) live alerts only in `live:alerts`, newest id first. A live alert
/// in both is shown as stored in Redis.
pub fn all_alerts(s: &mut AppState) -> Result<Vec<Value>, ApiError> {
    let mut stored: IndexMap<String, Value> = IndexMap::new();
    if s.live.configured() {
        let cfg = s.need_engine()?.cfg.clone();
        let du = s.live.redis.as_mut().unwrap();
        let k = du.key("alerts");
        let m: Vec<(String, String)> = du.run(&cfg, true, |st| redis::cmd("HGETALL").arg(&k).query(&mut st.con)).map_err(store_err)?;
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
    let now = chrono::Utc::now().timestamp();
    verify_signature(secret.trim().as_bytes(), header(&h, "x-rusetect-timestamp").as_deref(), header(&h, "x-rusetect-signature").as_deref(), &b, now)?;
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
    let (rec, replayed) = tokio::task::spawn_blocking(move || {
        let mut st = app.state.lock().unwrap_or_else(|p| p.into_inner());
        ingest(&mut st, &ev)
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
        let mut st = app.state.lock().unwrap_or_else(|p| p.into_inner());
        let cfg = st.need_engine()?.cfg.clone();
        let events = st.live.feed(50, &cfg)?;
        Ok(PyJson(StatusCode::OK, json!({"durable": st.live.durable(), "store": st.live.store_json(), "events": events})))
    }).await.map_err(|e| ApiError::new(500, format!("internal error: {e}")))?
}
