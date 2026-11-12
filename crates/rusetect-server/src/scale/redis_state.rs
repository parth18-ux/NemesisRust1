//! Account state in Redis, so scoring workers hold no state and can be added or removed freely.
//!
//! Keys (one Redis Cluster hash slot per account via the {account} tag):
//!   a:{acct}    hash   out_n out_usd last_out_t first_out_t in_n in_usd last_in_t last_in_usd uniq_dst uniq_src
//!   p:{acct}    set    payees this account has paid (pair index; "seen before" and the reverse "money flowing back")
//!   wo:{acct}   zset   outgoing payments in the last 24 h (score = minute), member "eid|counterparty|usd"
//!   wi:{acct}   zset   incoming payments in the last 24 h
//!   bp          set    bank corridors seen
//!   pr:{acct}>payee  zset  cascade risks of this payer->payee pair in the last 24 h (structuring window), member "eid|risk"
//!   seen:{eid}  string  set once per payment eid (26 h), so a redelivered payment does not write account state twice
//! Per payment: one pipelined read round trip, features computed exactly as Engine::features, one pipelined write.
use rusetect::engine::{fx, CascadeConfig, Features, MIN24};
use rusetect::payment::Payment;
use rusetect::pyfmt::{hour_dow, py_repr};
use std::collections::HashSet;

const A_S: [&str; 9] = ["out_n", "out_usd", "last_out_t", "first_out_t", "in_n", "in_usd", "last_in_t", "last_in_usd", "uniq_dst"];
const A_D: [&str; 5] = ["in_n", "last_in_t", "uniq_src", "out_n", "in_usd"];

fn int(v: &Option<String>) -> Option<i64> {
    v.as_deref().and_then(|s| s.parse::<i64>().ok().or_else(|| s.parse::<f64>().ok().map(|f| f as i64)))
}
fn float(v: &Option<String>) -> Option<f64> {
    v.as_deref().and_then(|s| s.parse::<f64>().ok())
}

/// Member "eid|bank|account|usd" -> ("bank|account", usd).
fn cp(m: &str) -> (String, f64) {
    let rest = m.split_once('|').map(|x| x.1).unwrap_or("");
    let (c, u) = rest.rsplit_once('|').unwrap_or((rest, "0"));
    (c.to_string(), u.parse().unwrap_or(0.0))
}

/// Members strictly newer than t - 24 h (already filtered by score) plus the current payment.
fn window(members: &[String], new_key: &str, usd: f64) -> (usize, usize, f64) {
    let mut keys: HashSet<String> = HashSet::new();
    let mut total = usd;
    for m in members {
        let (k, u) = cp(m);
        keys.insert(k);
        total += u;
    }
    keys.insert(new_key.to_string());
    (keys.len(), members.len() + 1, total.max(0.0))
}

pub struct RedisState {
    pub con: redis::Connection,
    pub cfg: CascadeConfig,
    pub px: String,
    pub last_sender_fan_in: usize,
}

impl RedisState {
    pub fn new(con: redis::Connection, cfg: CascadeConfig, prefix: &str) -> Self {
        RedisState { con, cfg, px: prefix.to_string(), last_sender_fan_in: 0 }
    }

    /// The structuring window of Engine::finish_risk (engine::PairWindow) kept in Redis, so a restarted or rebalanced
    /// worker keeps it: 24 h noisy-OR of the cascade risks of this payer's payments to `payee` (all payees when `payee`
    /// is empty), current one included. A redelivered payment (same eid) is counted once. Returns (payments, noisy-OR).
    pub fn pair_window(&mut self, p: &Payment, payee: &str, risk: f64) -> redis::RedisResult<(usize, f64)> {
        let key = format!("{}>{}", self.k("pr", &format!("{}|{}", p.from_bank, p.from_account)), payee);
        let eid = p.eid.map(|e| e.to_string()).unwrap_or_else(|| format!("{}.{}", p.tmin, std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let (members,): (Vec<String>,) = redis::pipe()
            .cmd("ZREMRANGEBYSCORE").arg(&key).arg("-inf").arg(p.tmin - MIN24).ignore()
            .cmd("ZADD").arg(&key).arg(p.tmin).arg(format!("{eid}|{risk}")).ignore()
            .cmd("ZRANGE").arg(&key).arg(0).arg(-1)
            .cmd("EXPIRE").arg(&key).arg(MIN24 * 60 + 3600).ignore()
            .query(&mut self.con)?;
        let prod: f64 = members.iter().map(|m| 1.0 - m.rsplit_once('|').and_then(|x| x.1.parse::<f64>().ok()).unwrap_or(0.0)).product();
        Ok((members.len(), 1.0 - prod))
    }

    fn k(&self, kind: &str, acct: &str) -> String {
        format!("{}{}:{{{}}}", self.px, kind, acct)
    }

    pub fn features(&mut self, p: &Payment) -> redis::RedisResult<Features> {
        let s = format!("{}|{}", p.from_bank, p.from_account);
        let d = format!("{}|{}", p.to_bank, p.to_account);
        let t = p.tmin;
        let usd = self.cfg.usd(p.amount_paid, &p.payment_currency);
        let bp = format!("{}>{}", p.from_bank, p.to_bank);
        let eid = p.eid.map(|e| e.to_string()).unwrap_or_else(|| format!("{t}.{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let lo = format!("({}", t - MIN24);
        let bpk = format!("{}bp", self.px);
        // At-least-once delivery replays a payment scored before a crash: it is scored again from the current state
        // (so a decision is still emitted) but its account-state writes are skipped. A payment without an eid has no
        // identity to dedupe on and always writes, as before.
        let first = match p.eid {
            Some(e) => redis::cmd("SET").arg(self.k("seen", &e.to_string())).arg(1).arg("NX").arg("EX").arg(26 * 3600)
                .query::<Option<String>>(&mut self.con)?.is_some(),
            None => true,
        };
        let (sa, da): (Vec<Option<String>>, Vec<Option<String>>);
        let (seen, recip, bp_seen): (bool, bool, bool);
        let (wo, wi, wi_s): (Vec<String>, Vec<String>, Vec<String>);
        (sa, da, seen, recip, bp_seen, wo, wi, wi_s) = redis::pipe()
            .cmd("HMGET").arg(self.k("a", &s)).arg(&A_S[..])
            .cmd("HMGET").arg(self.k("a", &d)).arg(&A_D[..])
            .cmd("SISMEMBER").arg(self.k("p", &s)).arg(&d)
            .cmd("SISMEMBER").arg(self.k("p", &d)).arg(&s)
            .cmd("SISMEMBER").arg(&bpk).arg(&bp)
            .cmd("ZRANGEBYSCORE").arg(self.k("wo", &s)).arg(&lo).arg("+inf")
            .cmd("ZRANGEBYSCORE").arg(self.k("wi", &d)).arg(&lo).arg("+inf")
            .cmd("ZRANGEBYSCORE").arg(self.k("wi", &s)).arg(&lo).arg("+inf")
            .query(&mut self.con)?;
        self.last_sender_fan_in = wi_s.iter().map(|m| cp(m).0).collect::<HashSet<_>>().len();
        let nan = f64::NAN;
        let on = int(&sa[0]).unwrap_or(0);
        let ousd = float(&sa[1]).unwrap_or(0.0);
        let mut f = [0.0f64; 31];
        use fx::*;
        f[LOG_USD] = usd.ln_1p();
        f[FMT_CODE] = self.cfg.fmt_codes.get(&p.payment_format).map(|v| *v as f64).unwrap_or(nan);
        f[CUR_CODE] = self.cfg.cur_codes.get(&p.payment_currency).map(|v| *v as f64).unwrap_or(nan);
        let (h, dow) = hour_dow(t);
        f[HOUR] = h as f64;
        f[DOW] = dow as f64;
        f[CROSS_BANK] = (p.from_bank != p.to_bank) as i64 as f64;
        f[CROSS_CUR] = (p.receiving_currency != p.payment_currency) as i64 as f64;
        f[SELF_LOOP] = (s == d) as i64 as f64;
        f[ROUND_100] = (usd % 100.0 == 0.0) as i64 as f64;
        f[S_OUT_N] = on as f64;
        f[S_OUT_MEAN_USD] = (ousd / on.max(1) as f64).ln_1p();
        f[S_GAP_OUT] = int(&sa[2]).map(|x| (t - x) as f64).unwrap_or(nan);
        f[S_AGE_OUT] = int(&sa[3]).map(|x| (t - x) as f64).unwrap_or(0.0);
        f[NEW_PAIR] = (!seen) as i64 as f64;
        f[S_UNIQ_DST] = int(&sa[8]).unwrap_or(0) as f64;
        f[D_IN_N] = int(&da[0]).unwrap_or(0) as f64;
        f[D_GAP_IN] = int(&da[1]).map(|x| (t - x) as f64).unwrap_or(nan);
        f[D_UNIQ_SRC] = int(&da[2]).unwrap_or(0) as f64;
        f[S_IN_N] = int(&sa[4]).unwrap_or(0) as f64;
        f[S_MINS_SINCE_IN] = int(&sa[6]).map(|x| (t - x) as f64).unwrap_or(nan);
        f[S_OUT_OVER_LAST_IN] = match float(&sa[7]) { Some(l) if l != 0.0 => usd / l, _ => nan };
        f[S_PASSTHROUGH] = ousd / float(&sa[5]).unwrap_or(0.0).max(1.0);
        f[D_OUT_N] = (int(&da[3]).unwrap_or(0) + if s == d { 1 } else { 0 }) as f64;
        f[D_IN_USD_LOG] = float(&da[4]).unwrap_or(0.0).ln_1p();
        let (a, b, c) = window(&wo, &d, usd);
        f[S_FANOUT_24H] = a as f64;
        f[S_CNT_24H] = b as f64;
        f[S_USD_24H] = c.ln_1p();
        let (a, b, _) = window(&wi, &s, usd);
        f[D_FANIN_24H] = a as f64;
        f[D_CNT_24H] = b as f64;
        f[RECIPROCAL] = recip as i64 as f64;
        f[NEW_BANK_PAIR] = (!bp_seen) as i64 as f64;
        if !first {
            return Ok(Features(f));
        }
        // ---- writes
        let (ka_s, ka_d) = (self.k("a", &s), self.k("a", &d));
        let u = py_repr(usd);
        let mut w = redis::pipe();
        w.cmd("HSETNX").arg(&ka_s).arg("first_out_t").arg(t).ignore()
            .cmd("HINCRBY").arg(&ka_s).arg("out_n").arg(1).ignore()
            .cmd("HINCRBYFLOAT").arg(&ka_s).arg("out_usd").arg(&u).ignore()
            .cmd("HSET").arg(&ka_s).arg("last_out_t").arg(t).ignore();
        if !seen {
            w.cmd("SADD").arg(self.k("p", &s)).arg(&d).ignore()
                .cmd("HINCRBY").arg(&ka_s).arg("uniq_dst").arg(1).ignore()
                .cmd("HINCRBY").arg(&ka_d).arg("uniq_src").arg(1).ignore();
        }
        w.cmd("HINCRBY").arg(&ka_d).arg("in_n").arg(1).ignore()
            .cmd("HINCRBYFLOAT").arg(&ka_d).arg("in_usd").arg(&u).ignore()
            .cmd("HSET").arg(&ka_d).arg("last_in_t").arg(t).arg("last_in_usd").arg(&u).ignore()
            .cmd("ZADD").arg(self.k("wo", &s)).arg(t).arg(format!("{eid}|{d}|{u}")).ignore()
            .cmd("ZADD").arg(self.k("wi", &d)).arg(t).arg(format!("{eid}|{s}|{u}")).ignore()
            .cmd("ZREMRANGEBYSCORE").arg(self.k("wo", &s)).arg("-inf").arg(t - MIN24).ignore()
            .cmd("ZREMRANGEBYSCORE").arg(self.k("wi", &d)).arg("-inf").arg(t - MIN24).ignore()
            .cmd("EXPIRE").arg(self.k("wo", &s)).arg(2 * 86400).ignore()
            .cmd("EXPIRE").arg(self.k("wi", &d)).arg(2 * 86400).ignore();
        if !bp_seen {
            w.cmd("SADD").arg(&bpk).arg(&bp).ignore();
        }
        w.query::<()>(&mut self.con)?;
        Ok(Features(f))
    }

    pub fn sender_fan_in_24h(&mut self, p: &Payment) -> redis::RedisResult<usize> {
        let s = format!("{}|{}", p.from_bank, p.from_account);
        let m: Vec<String> = redis::cmd("ZRANGEBYSCORE").arg(self.k("wi", &s)).arg(format!("({}", p.tmin - MIN24)).arg("+inf").query(&mut self.con)?;
        Ok(m.iter().map(|x| cp(x).0).collect::<HashSet<_>>().len())
    }
}
