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
//! `Idempotent-Replayed: true`) and is not scored or written again. In Redis the claim is `pending:<seq>`, a token unique
//! to this request; the commit is applied only while ev:{id} still holds that token, and a failed request deletes only
//! its own claim, so a request that outlived its claim (CLAIM_TTL_S) cannot overwrite or delete a retry's claim or result.
//! A commit that did not land takes its alert and desk-chain entries back out of the desk (Attempt::unwind), so the
//! retry leaves one alert and one decision; when the outcome is unknown, the entries are voided by the next attempt that
//! finds the event uncommitted. The retry is scored from the features stored with the payment's first account write
//! (seen:{eid}) and the pair-window score saved by its first scoring (pair-score:{eid}).
//! A success means the decision and its feed, ledger and alert records are all written: a record the commit could not
//! write stays listed in repair:{id}, the answer is a 503, and every replay completes the list before answering.
//!
//! Concurrency (Redis mode): scoring and Redis I/O run outside the state mutex, on a small connection pool
//! (RUSETECT_LIVE_POOL, default 2 x logical CPUs). Payments that share a payer or a payee are scored one at a time
//! (account stripes for both, taken in ascending order, held across the account-state read, write and pair window); the
//! state mutex is held only for the in-process part (drift, PRAMAAN, desk alert and chain); a decision that is not
//! `allow` holds `ledger_order` from before that lock through the commit, so live:ledger receives the desk's hash chain in
//! order. Lock order: ledger_order, then state; alert_io, then state. Per event: three Redis round trips (claim and seq
//! with the account-state read, account-state write with pair window, commit; docs/decision.md 8ap). Memory mode stays
//! one locked call.
use crate::inputs::AssessIn;
use crate::scale::{redis_state::RedisState, worker::score_read};
use rusetect::engine::Features;
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
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, RwLock};
use std::time::{Duration, Instant};

/// Largest clock difference accepted between the sender's timestamp and the server (seconds).
pub const TOLERANCE_S: i64 = 300;
/// Live decisions kept for the console feed.
const FEED: usize = 200;
/// Event ids remembered for idempotency in memory mode. When full, a new event id is refused (503) rather than an old
/// one forgotten: a forgotten id would be scored again on a re-send, writing its payment into account history twice.
/// RUSETECT_MAX_EVENTS overrides the default.
pub static MEM_EVENTS: std::sync::LazyLock<usize> =
    std::sync::LazyLock::new(|| std::env::var("RUSETECT_MAX_EVENTS").ok().and_then(|v| v.parse().ok()).unwrap_or(100_000));
/// How long Redis keeps an event's result for idempotency, and how long an unfinished claim blocks a retry.
const EVENT_TTL_S: u64 = 7 * 86_400;
pub const CLAIM_TTL_S: u64 = 60;
/// Entries kept in `live:ledger` (oldest trimmed first).
pub const LEDGER_MAX: usize = 100_000;
pub const ACTIONS: [&str; 4] = ["allow", "verify", "hold", "block"];
/// account stripes: more stripes, fewer unrelated payments waiting on each other (8ap)
const STRIPES: usize = 1024;
/// Claim the event id and take its sequence number in one round trip; the claim is `pending:<seq>`, unique to this
/// request. A known id returns what is stored (a result, or another request's `pending:` claim).
const CLAIM_AND_SEQ: &str = "local v = redis.call('GET', KEYS[1]) if v then return {0, v} end \
    local n = tostring(redis.call('INCR', KEYS[2])) redis.call('SET', KEYS[1], 'pending:' .. n, 'EX', ARGV[1]) return {1, n}";
/// Delete a claim only while it is still ours (a retry may hold the key by now).
const RELEASE: &str = "if redis.call('GET', KEYS[1]) == ARGV[1] then return redis.call('DEL', KEYS[1]) end return 0";
/// Key types the side records need (none: not created yet); a key of another type is refused before anything is written.
const CHECK_TYPES: &str = "local function bad_type(k, want) local t = redis.call('TYPE', k)['ok'] return t ~= 'none' and t ~= want end \
    if bad_type(KEYS[2], 'list') or bad_type(KEYS[3], 'list') or bad_type(KEYS[4], 'hash') or bad_type(KEYS[5], 'hash') then return {2, false} end ";
/// Apply the side records listed in repair:{id} (KEYS[5]), each removed from it once written; returns how many are
/// still missing. Shared by COMMIT and REPAIR, so a retry completes exactly the records a commit could not write.
const APPLY: &str = "local function apply(feed_last, ledger_first, fail) local left = 0 \
    for _, f in ipairs({'feed', 'ledger', 'alert'}) do local v = redis.call('HGET', KEYS[5], f) if v then local r \
    if f == fail then r = {err = 'injected fault'} \
    elseif f == 'feed' then r = redis.pcall('LPUSH', KEYS[2], v) redis.pcall('LTRIM', KEYS[2], 0, feed_last) \
    elseif f == 'ledger' then r = redis.pcall('RPUSH', KEYS[3], v) redis.pcall('LTRIM', KEYS[3], ledger_first, -1) \
    else r = redis.pcall('HSET', KEYS[4], redis.call('HGET', KEYS[5], 'alert_id'), v) end \
    if type(r) == 'table' and r.err then left = left + 1 else redis.call('HDEL', KEYS[5], f) end end end \
    if left == 0 then redis.call('DEL', KEYS[5]) end return left end ";
/// Commit only while ev:{id} holds our claim. A side-record key of the wrong type: {2}, nothing written. Otherwise the
/// side records (feed, ledger, alert) are listed in repair:{id} first, then the result is stored, then each record is
/// written and struck off the list: {1, records still missing}. A result is never stored without its repair list, so
/// a missing record is completed by the next replay (REPAIR). Not ours: {0, what ev:{id} holds} (a result, another
/// request's claim, or nil when the claim expired), having written nothing.
/// KEYS ev feed ledger alerts repair; ARGV token record event_ttl feed_last ledger_entry ledger_first alert_id alert fault.
const COMMIT: &str = "local v = redis.call('GET', KEYS[1]) if v ~= ARGV[1] then return {0, v} end ";
/// Replay of a stored result: complete its missing side records ({2} on a key of the wrong type, else {1, still
/// missing}). KEYS as COMMIT; ARGV feed_last ledger_first.
const REPAIR: &str = "if redis.call('EXISTS', KEYS[5]) == 0 then return {1, '0'} end ";

fn commit_script() -> &'static str {
    static S: OnceLock<String> = OnceLock::new();
    S.get_or_init(|| format!("{COMMIT}{CHECK_TYPES}{APPLY}\
        redis.call('HSET', KEYS[5], 'feed', ARGV[2], 'ledger', ARGV[5]) \
        if ARGV[7] ~= '' then redis.call('HSET', KEYS[5], 'alert', ARGV[8], 'alert_id', ARGV[7]) end \
        redis.call('EXPIRE', KEYS[5], ARGV[3]) redis.call('SET', KEYS[1], ARGV[2], 'EX', ARGV[3]) \
        return {{1, tostring(apply(ARGV[4], ARGV[6], ARGV[9]))}}"))
}

fn repair_script() -> &'static str {
    static S: OnceLock<String> = OnceLock::new();
    S.get_or_init(|| format!("{REPAIR}{CHECK_TYPES}{APPLY}return {{1, tostring(apply(ARGV[1], ARGV[2], ''))}}"))
}

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
    slots: Arc<tokio::sync::Semaphore>,
    size: usize,
    /// result of the last command in this instance: 0 none yet, 1 reachable, 2 unreachable
    reachable: AtomicU8,
    /// stateless engine for the Redis scoring path (account state lives in Redis), built on first use as Scorer::new does
    eng: OnceLock<Arc<Engine>>,
    /// payments sharing a payer or a payee are scored one at a time: read, write and pair window see each other's effects
    stripes: [Mutex<()>; STRIPES],
    /// held from before the state lock through the commit for decisions the desk chains, so live:ledger keeps chain order
    ledger_order: Mutex<()>,
    /// live alerts: an analyst action (write) holds it from its HGET through its HSET, a read (read) from its HGET until
    /// the copy is in the desk, so a read never puts a copy older than an action into the desk
    alert_io: RwLock<()>,
    /// failure injection for tests (`fail_next_commit`): 0 none, 1 commit not sent, 2 commit applied and its reply lost,
    /// 3 commit not sent and the outcome check fails too (outcome unknown), 5/6/7 the feed/ledger/alert write fails
    /// after the result is stored
    fault: AtomicU8,
}

impl Durable {
    fn new(client: redis::Client, px: &str) -> Self {
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2);
        let size = std::env::var("RUSETECT_LIVE_POOL").ok().and_then(|v| v.trim().parse().ok()).filter(|&n: &usize| n > 0).unwrap_or(2 * cores);
        Durable { client, px: px.into(), idle: Mutex::new(vec![]), slots: Arc::new(tokio::sync::Semaphore::new(size)), size,
                  reachable: AtomicU8::new(0), eng: OnceLock::new(), stripes: std::array::from_fn(|_| Mutex::new(())),
                  ledger_order: Mutex::new(()), alert_io: RwLock::new(()), fault: AtomicU8::new(0) }
    }

    /// Test hook: make the next commit fail as `fault` describes (see the field).
    #[doc(hidden)]
    pub fn fail_next_commit(&self, fault: u8) {
        self.fault.store(fault, Ordering::Relaxed);
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

    /// Delete our unfinished claim so the sender's retry is processed (account writes are deduped by seen:{eid}); a
    /// claim that expired and was taken by a retry, or a result stored since, is left alone.
    fn release(&self, key: &str, token: &str) {
        let _ = self.run(true, |st| redis::cmd("EVAL").arg(RELEASE).arg(1).arg(key).arg(token).query::<i64>(&mut st.con));
    }

    /// Lock the payer's and the payee's stripes, in ascending order (once when they are the same stripe).
    fn account_stripes(&self, d: &rusetect::payment::Payment) -> (MutexGuard<'_, ()>, Option<MutexGuard<'_, ()>>) {
        let stripe = |bank: &String, acct: &String| {
            let mut h = DefaultHasher::new();
            (bank, acct).hash(&mut h);
            (h.finish() % STRIPES as u64) as usize
        };
        let (a, b) = (stripe(&d.from_bank, &d.from_account), stripe(&d.to_bank, &d.to_account));
        let first = lock(&self.stripes[a.min(b)]);
        (first, (a != b).then(|| lock(&self.stripes[a.max(b)])))
    }

    /// Claim ev:{id} (`key`), take a sequence number and read `p`'s account state, in one round trip (call it with the
    /// account stripes held): Ok((seq, token, features, account-state write, sender fan-in)), or Err(what ev:{id}
    /// already holds). A read that fails after our claim releases the claim, so the sender's retry is processed.
    #[allow(clippy::type_complexity)]
    fn claim_and_read(&self, key: &str, p: &rusetect::payment::Payment) -> redis::RedisResult<Result<(u64, String, Features, redis::Cmd, usize), Option<String>>> {
        let mut c = redis::cmd("EVAL");
        c.arg(CLAIM_AND_SEQ).arg(2).arg(key).arg(self.key("seq")).arg(CLAIM_TTL_S);
        // our claim's token, kept across a reconnect-and-retry: the retry finds it in ev:{id} and it is still ours
        let mut ours: Option<String> = None;
        let mut claim: Option<(i64, Option<String>)> = None;
        let read = self.run(true, |st| {
            let mut reply = None;
            let r = st.features_read_after(p, Some(&c), &mut reply);
            if let Some(v) = reply {
                let (n, v): (i64, Option<String>) = redis::from_redis_value(&v)?;
                if n == 1 {
                    ours = v.as_ref().map(|s| format!("pending:{s}"));
                }
                claim = Some((n, v));
            }
            r.map(|(f, w)| (f, w, st.last_sender_fan_in))
        });
        let token = match claim {
            Some((1, _)) => ours.clone(),
            Some((_, v)) if v.is_some() && v == ours => ours.clone(),
            Some((_, v)) => return Ok(Err(v)),
            None => {
                if let Some(t) = &ours {
                    self.release(key, t);
                }
                return Err(read.err().unwrap_or_else(|| (redis::ErrorKind::ResponseError, "no claim reply").into()));
            }
        }.unwrap_or_default();
        match read {
            Ok((f, w, sf)) => {
                let seq: u64 = token.strip_prefix("pending:").and_then(|n| n.parse().ok()).unwrap_or(0);
                Ok(Ok((seq, token, f, w, sf)))
            }
            Err(e) => {
                self.release(key, &token);
                Err(e)
            }
        }
    }

    /// KEYS of COMMIT and REPAIR for the event whose ev: key is `key`.
    fn side_keys(&self, key: &str) -> [String; 5] {
        let id = key.rsplit_once("live:ev:").map(|x| x.1).unwrap_or(key);
        [key.to_string(), self.key("feed"), self.key("ledger"), self.key("alerts"), self.key(&format!("repair:{id}"))]
    }

    /// The COMMIT script: (1, side records still missing) when the result was stored, (2, _) when a side-record key has
    /// the wrong type (nothing written), else (0, what ev:{id} holds). No retry: a commit whose reply was lost may have
    /// been applied.
    fn commit(&self, key: &str, token: &str, rec: &Value, entry: &Value, alert: Option<&Value>) -> redis::RedisResult<(i64, Option<String>)> {
        let (aid, abody) = alert.map(|a| (a["alert_id"].as_str().unwrap_or("").to_string(), a.to_string())).unwrap_or_default();
        let fault = self.fault.load(Ordering::Relaxed);
        let side_fault = match fault { 5 => "feed", 6 => "ledger", 7 => "alert", _ => "" };
        if !side_fault.is_empty() {
            self.fault.store(0, Ordering::Relaxed);
        }
        let mut c = redis::cmd("EVAL");
        c.arg(commit_script()).arg(5).arg(&self.side_keys(key)[..]).arg(token).arg(rec.to_string()).arg(EVENT_TTL_S).arg(FEED - 1)
            .arg(entry.to_string()).arg(-(LEDGER_MAX as i64)).arg(&aid).arg(&abody).arg(side_fault);
        let injected = || Err(redis::RedisError::from((redis::ErrorKind::IoError, "injected fault")));
        if fault == 1 || fault == 3 {
            self.fault.store(if fault == 3 { 4 } else { 0 }, Ordering::Relaxed);
            return injected();
        }
        let r = self.run(false, |st| c.query(&mut st.con));
        if fault == 2 && r.is_ok() {
            self.fault.store(0, Ordering::Relaxed);
            return injected();
        }
        r
    }

    /// After a commit error: what ev:{id} holds now.
    fn outcome(&self, key: &str) -> redis::RedisResult<Option<String>> {
        if self.fault.swap(0, Ordering::Relaxed) == 4 {
            return Err(redis::RedisError::from((redis::ErrorKind::IoError, "injected fault")));
        }
        self.run(true, |st| redis::cmd("GET").arg(key).query(&mut st.con))
    }

    /// Before a stored result is answered: complete the side records its commit could not write. Ok when none is missing.
    fn repair(&self, key: &str) -> Result<(), ApiError> {
        let mut c = redis::cmd("EVAL");
        c.arg(repair_script()).arg(5).arg(&self.side_keys(key)[..]).arg(FEED - 1).arg(-(LEDGER_MAX as i64));
        let (code, left): (i64, Option<String>) = self.run(true, |st| c.query(&mut st.con)).map_err(store_err)?;
        if code == 1 && left.as_deref() == Some("0") {
            return Ok(());
        }
        Err(incomplete())
    }

    fn get_alert(&self, id: &str) -> Result<Option<Value>, ApiError> {
        let k = self.key("alerts");
        let v: Option<String> = self.run(true, |st| redis::cmd("HGET").arg(&k).arg(id).query(&mut st.con)).map_err(store_err)?;
        Ok(v.and_then(|v| serde_json::from_str(&v).ok()))
    }

    /// Latest live decisions, newest first.
    fn feed(&self, n: usize) -> Result<Vec<Value>, ApiError> {
        let key = self.key("feed");
        let rows: Vec<String> = self.run(true, |st| redis::cmd("LRANGE").arg(&key).arg(0).arg(n as i64 - 1).query(&mut st.con)).map_err(store_err)?;
        Ok(rows.iter().filter_map(|r| serde_json::from_str(r).ok()).collect())
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
    /// memory mode: overrides MEM_EVENTS (tests)
    pub max_events: Option<usize>,
    /// Redis mode: attempts whose commit outcome was unknown (store unreachable), by event id, with the void entry that
    /// settles their desk-chain entries if a later attempt finds the event uncommitted (`settle_unknown`)
    unsettled: IndexMap<String, Value>,
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
}

/// A stored decision whose feed, ledger or alert record is not written yet: a 503 (with Retry-After), so the sender
/// retries and the replay completes the records.
fn incomplete() -> ApiError {
    ApiError::new(503, "durable store: the decision is stored but its feed, ledger or alert record is not written yet; retry")
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

/// Attempts with an unknown commit outcome remembered per instance (oldest dropped first).
const MAX_UNSETTLED: usize = 10_000;

/// What `decide` added to this instance's desk for one attempt: its alert and the desk-chain entries log[mark..end].
struct Attempt {
    seq: u64,
    alert_id: Option<String>,
    mark: usize,
    end: usize,
}

impl Attempt {
    /// The void entry for this attempt's desk-chain entries: it names them by hash, so the chain stays append-only.
    fn void(&self, s: &AppState, ev_id: &str, reason: &str) -> Value {
        let voids: Vec<Value> = s.desk.log.get(self.mark..self.end).unwrap_or_default().iter().map(|e| e["hash"].clone()).collect();
        json!({"type": "live_decision_void", "event_id": ev_id, "event_seq": self.seq, "alert_id": self.alert_id, "voids": voids,
               "reason": reason})
    }

    /// The commit did not land: take this attempt out of the desk, so a retry leaves one alert and one decision. Its
    /// chain entries are removed while they are still the chain's tail (nothing was chained after them, and they were
    /// never committed or answered); otherwise a void entry names them.
    fn unwind(&self, s: &mut AppState, ev_id: &str) {
        if let Some(a) = &self.alert_id {
            s.desk.alerts.remove(a);
        }
        if self.end == self.mark {
            return;
        }
        let tail = s.desk.log.len() == self.end;
        if tail {
            s.desk.log.truncate(self.mark);
        } else {
            let v = self.void(s, ev_id, "commit not applied");
            s.desk.append(v);
        }
    }

    /// The commit outcome is unknown (the store was unreachable): the desk's copy of the alert goes (live alerts are read
    /// from Redis, where it is if the commit landed); the chain entries stay, since live:ledger may hold them, and are
    /// voided only if a later attempt finds the event uncommitted.
    fn unknown(&self, s: &mut AppState, ev_id: &str) {
        if let Some(a) = &self.alert_id {
            s.desk.alerts.remove(a);
        }
        if self.end > self.mark {
            let v = self.void(s, ev_id, "commit outcome unknown; a later attempt found the event uncommitted");
            s.live.unsettled.insert(ev_id.to_string(), v);
            if s.live.unsettled.len() > MAX_UNSETTLED {
                s.live.unsettled.shift_remove_index(0);
            }
        }
    }
}

/// Called holding a fresh claim on ev:{id}, so no result is stored: an earlier attempt here whose outcome was unknown did
/// not commit, and its chain entries are voided before this attempt decides.
fn settle_unknown(s: &mut AppState, ev_id: &str) {
    if let Some(v) = s.live.unsettled.shift_remove(ev_id) {
        s.desk.append(v);
    }
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
    if s.live.events.len() >= s.live.max_events.unwrap_or(*MEM_EVENTS) {
        return Err(ApiError::new(503, "memory-mode idempotency store is full; nothing was committed. Use Redis (REDIS_URL) or raise RUSETECT_MAX_EVENTS and restart"));
    }
    s.live.seq += 1;
    let seq = s.live.seq;
    let (mut rec, _, _) = decide(s, ev, &d, seq, None)?;
    rec["latency_ms"] = jf(py_round(t0.elapsed().as_secs_f64() * 1000.0, 2));
    // 5. commit
    s.live.events.insert(ev.id.clone(), rec.clone());
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
    // P1, no state lock, one payment at a time per payer and per payee (account stripes):
    // 1. idempotency: a known event id returns what was stored; a new one is claimed (and numbered) before any write,
    // in the same round trip as the account-state read
    let key = du.key(&format!("ev:{}", ev.id));
    let mut p = d.clone();
    p.eid = Some(eid_of(&ev.id));
    let accounts = du.account_stripes(&d);
    let (seq, token, f, write, sf) = match du.claim_and_read(&key, &p).map_err(store_err)? {
        Ok(x) => x,
        Err(v) => {
            drop(accounts);
            return replay(du, &key, v);
        }
    };
    // 2. score on the bank-scale path: account-state write and pair window in Redis
    let scored = du.run(true, |st| score_read(&eng, st, &p, &f, write.clone(), sf));
    drop(accounts);
    let r = match scored {
        Ok(r) => r,
        Err(e) => {
            du.release(&key, &token);
            return Err(store_err(e));
        }
    };
    // P2, state lock: 3-4 (PRAMAAN only raises, so an allow without a PRAMAAN request stays unchained)
    let chained = decision_rank(r["final_decision"].as_str().unwrap_or("Approve")) > 0 || ev.auth.is_some();
    // (held to the end, so the desk-chain entries of a commit that did not land are still the chain's tail when unwound)
    let _order = chained.then(|| lock(&du.ledger_order));
    let (res, att) = {
        let mut s = lock(&app.state);
        settle_unknown(&mut s, &ev.id);
        let mark = s.desk.log.len();
        let res = decide(&mut s, ev, &d, seq, Some(&r));
        let att = Attempt { seq, alert_id: res.as_ref().ok().and_then(|x| x.0["alert_id"].as_str().map(String::from)), mark, end: s.desk.log.len() };
        (res, att)
    };
    let unwind = || att.unwind(&mut lock(&app.state), &ev.id);
    let (mut rec, alert, entry) = match res {
        Ok(x) => x,
        Err(e) => {
            unwind();
            du.release(&key, &token);
            return Err(e);
        }
    };
    rec["latency_ms"] = jf(py_round(t0.elapsed().as_secs_f64() * 1000.0, 2));
    // P3, no state lock: 5. commit, only while ev:{id} still holds our claim
    match du.commit(&key, &token, &rec, &entry, alert.as_ref()) {
        Ok((1, left)) if left.as_deref() == Some("0") => Ok((rec, false)),
        // the decision is stored (the desk keeps it) but a side record is not: the replay completes it
        Ok((1, left)) => {
            eprintln!("live commit {}: result stored, {} feed/ledger/alert record(s) left for the replay", ev.id, left.unwrap_or_default());
            Err(incomplete())
        }
        // a side-record key of the wrong type: nothing was written, so the retry is processed afresh
        Ok((2, _)) => {
            unwind();
            du.release(&key, &token);
            Err(ApiError::new(503, "durable store: a feed, ledger or alert key has the wrong type; nothing was committed, retry"))
        }
        // our claim expired while we worked: what ev:{id} holds now wins, and nothing of ours was written
        Ok((_, v)) => {
            unwind();
            replay(du, &key, v)
        }
        Err(e) => {
            // look before releasing the claim: our stored result means the commit was applied (a reply timeout)
            match du.outcome(&key) {
                Ok(Some(v)) if !v.starts_with("pending") => {
                    let r: Value = serde_json::from_str(&v).unwrap_or(rec);
                    let ours = r["seq"].as_u64() == Some(seq);
                    if !ours {
                        unwind();
                    }
                    du.repair(&key)?;
                    return Ok((r, !ours));
                }
                Ok(_) => {
                    unwind();
                    du.release(&key, &token);
                }
                // unknown: the claim expires after CLAIM_TTL_S, and a late commit needs the claim, so no double commit
                Err(_) => att.unknown(&mut lock(&app.state), &ev.id),
            }
            Err(store_err(e))
        }
    }
}

/// `stored`, with a stored result's missing side records completed first (else a 503).
fn replay(du: &Durable, key: &str, v: Option<String>) -> Result<(Value, bool), ApiError> {
    if v.as_deref().is_some_and(|v| !v.starts_with("pending")) {
        du.repair(key)?;
    }
    stored(v)
}

/// What ev:{id} holds when it is not ours: a stored result is replayed, another request's claim is a 409 (with
/// Retry-After), and nothing at all (our claim expired, no retry took it) is a 503: nothing was committed.
fn stored(v: Option<String>) -> Result<(Value, bool), ApiError> {
    match v {
        Some(v) if v.starts_with("pending") => Err(ApiError::new(409, "this event_id is still being processed; retry in a minute")),
        Some(v) => serde_json::from_str(&v).map(|r| (r, true)).map_err(|_| ApiError::new(409, "this event_id is still being processed; retry in a minute")),
        None => Err(ApiError::new(503, "durable store: the claim on this event_id expired before the commit; nothing was committed, retry")),
    }
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

/// The Redis store, None in memory mode; with Redis the engine check every live path makes first. Called under the state
/// lock, so the Redis I/O that follows can run without it.
fn store_of(s: &mut AppState) -> Result<Option<Arc<Durable>>, ApiError> {
    if !s.live.configured() {
        return Ok(None);
    }
    s.need_engine()?;
    Ok(s.live.redis.clone())
}

fn read_io(d: &Durable) -> std::sync::RwLockReadGuard<'_, ()> {
    d.alert_io.read().unwrap_or_else(|p| p.into_inner())
}

/// With Redis, `live:alerts` is the authority for live alerts: load the alert into this instance's desk before it is read
/// (refreshing a copy already there, since another instance may have acted on it), then run `f` with the state locked.
/// The HGET runs without the state lock. In memory mode just `f`.
pub fn with_alert<T>(app: &crate::Shared, id: &str, f: impl FnOnce(&mut AppState) -> Result<T, ApiError>) -> Result<T, ApiError> {
    let du = store_of(&mut lock(&app.state))?;
    let _io = du.as_deref().map(read_io);
    let a = match &du { Some(d) => d.get_alert(id)?, None => None };
    let mut s = lock(&app.state);
    if let Some(a) = a {
        s.desk.alerts.insert(id.to_string(), a);
    }
    f(&mut s)
}

/// An analyst action on a live alert: `pre` (the nonce check) under the state lock, then as `with_alert` with `act`, then
/// (Redis mode) the acted alert is written back to `live:alerts` (other alerts are left alone). Redis I/O runs without
/// the state lock; one instance's actions on live alerts are applied one at a time (alert_io).
pub fn act_on_alert<T>(app: &crate::Shared, id: &str, pre: impl FnOnce(&mut AppState) -> Result<(), ApiError>,
                       act: impl FnOnce(&mut AppState) -> Result<T, ApiError>) -> Result<T, ApiError> {
    let d = {
        let mut s = lock(&app.state);
        pre(&mut s)?;
        match store_of(&mut s)? {
            Some(d) => d,
            None => return act(&mut s),
        }
    };
    let _io = d.alert_io.write().unwrap_or_else(|p| p.into_inner());
    let a = d.get_alert(id)?;
    let (out, save) = {
        let mut s = lock(&app.state);
        if let Some(a) = a {
            s.desk.alerts.insert(id.to_string(), a);
        }
        let out = act(&mut s)?;
        (out, s.desk.alerts.get(id).filter(|a| a.get("event_id").is_some()).map(|a| a.to_string()))
    };
    if let Some(a) = save {
        let k = d.key("alerts");
        d.run(true, |st| redis::cmd("HSET").arg(&k).arg(id).arg(&a).query::<i64>(&mut st.con)).map_err(store_err)?;
    }
    Ok(out)
}

/// The desk's alerts newest first, then (Redis mode) live alerts only in `live:alerts`, newest id first. A live alert
/// in both is shown as stored in Redis. The HGETALL runs without the state lock.
pub fn all_alerts(app: &crate::Shared) -> Result<Vec<Value>, ApiError> {
    let du = store_of(&mut lock(&app.state))?;
    let mut stored: IndexMap<String, Value> = IndexMap::new();
    if let Some(du) = du {
        let k = du.key("alerts");
        let m: Vec<(String, String)> = du.run(true, |st| redis::cmd("HGETALL").arg(&k).query(&mut st.con)).map_err(store_err)?;
        stored = m.into_iter().filter_map(|(id, v)| serde_json::from_str(&v).ok().map(|a| (id, a))).collect();
        stored.sort_by(|a, _, b, _| b.cmp(a));
    }
    let s = lock(&app.state);
    let mut out: Vec<Value> = s.desk.alerts.iter().rev().map(|(id, a)| stored.shift_remove(id).unwrap_or_else(|| a.clone())).collect();
    out.extend(stored.into_values());
    Ok(out)
}

/// The newest stored live decisions, newest first, for the website's read endpoints (intel.rs): up to `n` of the
/// events kept in memory, or of the feed (FEED) with Redis, where older results exist only under their own ev:{id}
/// key. Returns (records, events ingested so far, how many the store can list). Redis I/O runs without the state lock.
pub(crate) fn recent(app: &crate::Shared, n: usize) -> Result<(Vec<Value>, u64, usize), ApiError> {
    let du = {
        let mut s = lock(&app.state);
        match store_of(&mut s)? {
            Some(d) => d,
            None => return Ok((s.live.events.values().rev().take(n).cloned().collect(), s.live.seq, s.live.max_events.unwrap_or(*MEM_EVENTS))),
        }
    };
    let k = du.key("seq");
    let seq: Option<u64> = du.run(true, |st| redis::cmd("GET").arg(&k).query(&mut st.con)).map_err(store_err)?;
    Ok((du.feed(n.min(FEED))?, seq.unwrap_or(0), FEED))
}

/// One stored live decision by event id (with Redis for EVENT_TTL_S after it was ingested); None when unknown or
/// still being processed.
pub(crate) fn stored_event(app: &crate::Shared, id: &str) -> Result<Option<Value>, ApiError> {
    let du = {
        let mut s = lock(&app.state);
        match store_of(&mut s)? {
            Some(d) => d,
            None => return Ok(s.live.events.get(id).cloned()),
        }
    };
    let k = du.key(&format!("ev:{id}"));
    let v: Option<String> = du.run(true, |st| redis::cmd("GET").arg(&k).query(&mut st.con)).map_err(store_err)?;
    Ok(v.filter(|v| !v.starts_with("pending")).and_then(|v| serde_json::from_str(&v).ok()))
}

/// Investigations (workflow.rs) follow the alerts: with Redis the hash `live:cases` is the authority, so the case is
/// loaded into this instance before `act` runs with the state locked, and written back after it when `save`. One
/// instance's case and alert writes are applied one at a time (alert_io), so `act` must not call act_on_alert.
pub(crate) fn act_on_case<T>(app: &crate::Shared, id: &str, save: bool, act: impl FnOnce(&mut AppState) -> Result<T, ApiError>) -> Result<T, ApiError> {
    let d = {
        let mut s = lock(&app.state);
        match store_of(&mut s)? {
            Some(d) => d,
            None => return act(&mut s),
        }
    };
    let _io = d.alert_io.write().unwrap_or_else(|p| p.into_inner());
    let k = d.key("cases");
    let c: Option<String> = d.run(true, |st| redis::cmd("HGET").arg(&k).arg(id).query(&mut st.con)).map_err(store_err)?;
    let (out, stored) = {
        let mut s = lock(&app.state);
        if let Some(c) = c.and_then(|c| serde_json::from_str(&c).ok()) {
            s.cases.insert(id.to_string(), c);
        }
        let out = act(&mut s)?;
        (out, s.cases.get(id).filter(|_| save).map(|c| c.to_string()))
    };
    if let Some(c) = stored {
        d.run(true, |st| redis::cmd("HSET").arg(&k).arg(id).arg(&c).query::<i64>(&mut st.con)).map_err(store_err)?;
    }
    Ok(out)
}

/// Every investigation, newest first (with Redis: as stored in `live:cases`).
pub(crate) fn all_cases(app: &crate::Shared) -> Result<Vec<Value>, ApiError> {
    let du = {
        let mut s = lock(&app.state);
        match store_of(&mut s)? {
            Some(d) => d,
            None => return Ok(s.cases.values().rev().cloned().collect()),
        }
    };
    let k = du.key("cases");
    let m: Vec<(String, String)> = du.run(true, |st| redis::cmd("HGETALL").arg(&k).query(&mut st.con)).map_err(store_err)?;
    let mut out: Vec<(String, Value)> = m.into_iter().filter_map(|(id, v)| serde_json::from_str(&v).ok().map(|c| (id, c))).collect();
    out.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(out.into_iter().map(|x| x.1).collect())
}

/// The next investigation number: a store-wide counter with Redis (ids from several instances do not collide).
pub(crate) fn next_case_seq(app: &crate::Shared) -> Result<u64, ApiError> {
    let du = {
        let mut s = lock(&app.state);
        match store_of(&mut s)? {
            Some(d) => d,
            None => {
                s.case_seq += 1;
                return Ok(s.case_seq);
            }
        }
    };
    let k = du.key("case_seq");
    du.run(false, |st| redis::cmd("INCR").arg(&k).query(&mut st.con)).map_err(store_err)
}

/// POST /api/v2/events/payment
pub async fn v2_events_payment(State(app): State<AppRef>, h: HeaderMap, b: Bytes) -> Response {
    match events_payment(app, h, b).await {
        Ok(r) => r,
        Err(e) => {
            // a 409 here means another request holds the claim: it is done or expired within CLAIM_TTL_S
            let conflict = e.status == 409;
            let mut r = e.into_response();
            if conflict {
                r.headers_mut().insert("retry-after", HeaderValue::from(CLAIM_TTL_S));
            }
            r
        }
    }
}

fn internal(e: impl std::fmt::Display) -> ApiError {
    ApiError::new(500, format!("internal error: {e}"))
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
    // the store as configured now (tests swap it), read under a brief lock on the blocking pool; memory mode is done
    // there in the same locked call
    let app2 = app.clone();
    let first = tokio::task::spawn_blocking(move || {
        let mut st = lock(&app2.state);
        match st.live.redis.clone() {
            Some(du) => Ok((du, st.need_engine().map(|_| ()), ev)),
            None => Err(ingest(&mut st, &ev)),
        }
    }).await.map_err(internal)?;
    let (rec, replayed) = match first {
        Err(done) => done?,
        Ok((du, ready, ev)) => {
            // Redis work waits for a pool slot; the permit moves into the job, so a client disconnect cannot free it early
            let slot = du.slots.clone().acquire_owned().await.map_err(internal)?;
            tokio::task::spawn_blocking(move || {
                let _slot = slot;
                ingest_redis(&app, &du, &ev, ready)
            }).await.map_err(internal)??
        }
    };
    let mut r = PyJson(StatusCode::OK, rec).into_response();
    if replayed {
        r.headers_mut().insert("idempotent-replayed", HeaderValue::from_static("true"));
    }
    Ok(r)
}

/// GET /api/v2/live: the latest live decisions (newest first) for the console, which polls it. The LRANGE runs without
/// the state lock.
pub async fn v2_live(State(app): State<AppRef>, h: HeaderMap) -> Result<PyJson, ApiError> {
    require_reader(&h)?;
    tokio::task::spawn_blocking(move || {
        let (du, mem) = {
            let mut st = lock(&app.state);
            st.need_engine()?;
            (st.live.redis.clone(), st.live.feed.iter().take(50).cloned().collect::<Vec<_>>())
        };
        let events = match du {
            Some(d) => d.feed(50)?,
            None => mem,
        };
        let st = lock(&app.state);
        Ok(PyJson(StatusCode::OK, json!({"durable": st.live.durable(), "store": st.live.store_json(), "events": events})))
    }).await.map_err(internal)?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Child, Command, Stdio};

    struct Server(Child, String);
    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.0.kill();
        }
    }

    fn server() -> Option<Server> {
        let port = std::net::TcpListener::bind("127.0.0.1:0").ok()?.local_addr().ok()?.port();
        let child = Command::new("redis-server").args(["--port", &port.to_string(), "--save", "", "--appendonly", "no"])
            .stdout(Stdio::null()).spawn().map_err(|_| eprintln!("redis-server not installed: skipping")).ok()?;
        let url = format!("redis://127.0.0.1:{port}/0");
        for _ in 0..100 {
            if redis::Client::open(url.as_str()).ok().and_then(|c| c.get_connection().ok()).is_some() {
                return Some(Server(child, url));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        None
    }

    fn get(con: &mut redis::Connection, k: &str) -> Option<String> {
        redis::cmd("GET").arg(k).query(con).unwrap()
    }

    fn pay(i: usize, payee: &str) -> rusetect::payment::Payment {
        rusetect::payment::Payment {
            from_bank: "070".into(), from_account: format!("KP{i}"), to_bank: "011".into(), to_account: payee.into(),
            amount_paid: 100.0, payment_currency: "US Dollar".into(), receiving_currency: "US Dollar".into(),
            payment_format: "ACH".into(), tmin: 27_000_000, eid: Some(i as i64 + 1),
        }
    }

    fn claim(du: &Durable, key: &str) -> (u64, String) {
        let (seq, token, ..) = du.claim_and_read(key, &pay(0, "CLAIMPAYEE")).unwrap().unwrap();
        (seq, token)
    }

    /// 8ap: the claim shares its round trip with the account-state read, which reads what a read on its own reads; a
    /// known event id gets what ev:{id} holds and takes no sequence number.
    #[test]
    fn the_claim_and_the_account_read_share_one_round_trip() {
        let Some(r) = server() else { return };
        let du = Durable::new(redis::Client::open(r.1.as_str()).unwrap(), "m:");
        let mut con = redis::Client::open(r.1.as_str()).unwrap().get_connection().unwrap();
        let p = pay(1, "MPAYEE");
        let (seq, token, f, w, _) = du.claim_and_read(&du.key("ev:m1"), &p).unwrap().unwrap();
        assert_eq!((seq, token.as_str()), (1, "pending:1"));
        let (alone, _) = du.run(true, |st| st.features_read(&p)).unwrap();
        assert_eq!(f.0.map(f64::to_bits), alone.0.map(f64::to_bits));
        du.run(true, |st| w.query::<i64>(&mut st.con)).unwrap();
        let p2 = pay(1, "MPAYEE2");
        let (seq2, _, f2, ..) = du.claim_and_read(&du.key("ev:m2"), &rusetect::payment::Payment { eid: Some(99), ..p2 }).unwrap().unwrap();
        assert_eq!((seq2, f2.0[rusetect::engine::fx::S_OUT_N]), (2, 1.0), "the next payment of the payer reads the write");
        assert!(matches!(du.claim_and_read(&du.key("ev:m1"), &p).unwrap(), Err(Some(v)) if v == token));
        assert_eq!(get(&mut con, &du.key("seq")).as_deref(), Some("2"));
    }

    /// A request whose claim expired and was taken by a retry neither commits over, nor deletes, the retry's claim or
    /// result: it gets a 409 while the retry works and the retry's result afterwards.
    #[test]
    fn a_stale_claim_holder_cannot_overwrite_or_delete_a_newer_claim_or_result() {
        let Some(r) = server() else { return };
        let du = Durable::new(redis::Client::open(r.1.as_str()).unwrap(), "s:");
        let mut con = redis::Client::open(r.1.as_str()).unwrap().get_connection().unwrap();
        let key = du.key("ev:e1");
        let (seq_a, a) = claim(&du, &key);
        redis::cmd("DEL").arg(&key).query::<()>(&mut con).unwrap(); // A's claim expires (CLAIM_TTL_S)
        let (seq_b, b) = claim(&du, &key);
        assert_ne!(a, b);
        let rec = |seq| json!({"event_id": "e1", "seq": seq});
        // A, still working, fails: its release leaves B's claim
        du.release(&key, &a);
        assert_eq!(get(&mut con, &key).as_deref(), Some(b.as_str()));
        // A commits: refused, nothing written, and the sender is told B is still processing
        let (applied, held) = du.commit(&key, &a, &rec(seq_a), &json!({"n": "a"}), None).unwrap();
        assert_eq!((applied, held.as_deref()), (0, Some(b.as_str())));
        assert_eq!(stored(held).unwrap_err().status, 409);
        assert_eq!(redis::cmd("LLEN").arg(du.key("ledger")).query::<i64>(&mut con).unwrap(), 0);
        // B commits; then A's late commit and release leave B's result alone, and A gets B's result as a replay
        assert_eq!(du.commit(&key, &b, &rec(seq_b), &json!({"n": "b"}), Some(&json!({"alert_id": "L1"}))).unwrap().0, 1);
        let (applied, held) = du.commit(&key, &a, &rec(seq_a), &json!({"n": "a"}), None).unwrap();
        assert_eq!(applied, 0);
        assert_eq!(stored(held).unwrap(), (rec(seq_b), true));
        du.release(&key, &a);
        assert_eq!(get(&mut con, &key).map(|v| serde_json::from_str::<Value>(&v).unwrap()), Some(rec(seq_b)));
        assert_eq!(redis::cmd("LLEN").arg(du.key("ledger")).query::<i64>(&mut con).unwrap(), 1);
        assert_eq!(redis::cmd("LLEN").arg(du.key("feed")).query::<i64>(&mut con).unwrap(), 1);
        // a claim that expired with no retry: A's commit writes nothing (a 503: nothing committed)
        let key2 = du.key("ev:e2");
        let (_, c) = claim(&du, &key2);
        redis::cmd("DEL").arg(&key2).query::<()>(&mut con).unwrap();
        let (applied, held) = du.commit(&key2, &c, &rec(9), &json!({}), None).unwrap();
        assert_eq!((applied, &held), (0, &None));
        assert_eq!(stored(held).unwrap_err().status, 503);
        assert_eq!(get(&mut con, &key2), None);
    }

    /// K payers paying one payee at once: with the payee's stripe held too, each scoring reads the payee's state after
    /// the previous one wrote it, so the payee's in-count features are 0..K, as in serial scoring.
    #[test]
    fn concurrent_payers_into_one_payee_see_each_others_writes() {
        let Some(r) = server() else { return };
        let du = Arc::new(Durable::new(redis::Client::open(r.1.as_str()).unwrap(), "p:"));
        const K: usize = 24;
        let barrier = Arc::new(std::sync::Barrier::new(K));
        let threads: Vec<_> = (0..K).map(|i| {
            let (du, barrier) = (du.clone(), barrier.clone());
            std::thread::spawn(move || {
                let p = pay(i, "ONEPAYEE");
                barrier.wait();
                let _accounts = du.account_stripes(&p);
                du.run(true, |st| {
                    let (f, w) = st.features_read(&p)?;
                    w.query::<i64>(&mut st.con)?;
                    Ok((f.0[rusetect::engine::fx::D_IN_N], f.0[rusetect::engine::fx::D_FANIN_24H]))
                }).unwrap()
            })
        }).collect();
        let mut seen: Vec<(f64, f64)> = threads.into_iter().map(|t| t.join().unwrap()).collect();
        seen.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let serial: Vec<(f64, f64)> = (0..K).map(|n| (n as f64, n as f64 + 1.0)).collect();
        assert_eq!(seen, serial, "each payment sees the payees in_n and fan-in after the previous write");
    }
}
