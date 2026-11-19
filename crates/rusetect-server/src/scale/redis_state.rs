//! Account state in Redis, so scoring workers hold no state and can be added or removed freely.
//!
//! Keys (one Redis Cluster hash slot per account via the {account} tag):
//!   a:{acct}    hash   out_n out_usd last_out_t first_out_t in_n in_usd last_in_t last_in_usd uniq_dst uniq_src
//!   p:{acct}    set    payees this account has paid (pair index; "seen before" and the reverse "money flowing back")
//!   wo:{acct}   zset   outgoing payments in the last 24 h (score = minute), member "eid|counterparty|usd"
//!   wi:{acct}   zset   incoming payments in the last 24 h
//!   bp          set    bank corridors seen
//!   pr:{acct}>payee  zset  cascade risks of this payer->payee pair in the last 24 h (structuring window), member "eid|risk",
//!               one member per eid (a rescored payment replaces its own member)
//!   seen:{eid}  string  set once per payment eid (26 h), holding the features it was first scored with: a redelivered
//!               payment does not write account state twice and is scored again from the same features
//!   pair-score:{eid}  string  first live scoring attempt's pair count and noisy-OR (26 h)
//! Per payment: one pipelined read round trip, features computed exactly as Engine::features, then one Lua script that
//! claims seen:{eid} and applies the writes atomically (a crash cannot leave the claim without the writes). score_one
//! sends that script and the pair window in one round trip.
//! The script touches payer, payee, bp and seen keys, which sit in different hash slots: it needs a single Redis node
//! (or a proxy that does not split scripts), not Redis Cluster.
use rusetect::engine::{fx, CascadeConfig, Features, MIN24};
use rusetect::payment::Payment;
use rusetect::pyfmt::py_repr;
use std::collections::HashSet;

const A_S: [&str; 9] = ["out_n", "out_usd", "last_out_t", "first_out_t", "in_n", "in_usd", "last_in_t", "last_in_usd", "uniq_dst"];
const A_D: [&str; 5] = ["in_n", "last_in_t", "uniq_src", "out_n", "in_usd"];
const SEEN_TTL_S: i64 = 26 * 3600;

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

/// `seen:{eid}`'s value, "f {sender fan-in} {31 features}" (NaN kept), back to (fan-in, features); None for anything else.
fn decode_features(v: &str) -> Option<(usize, [f64; 31])> {
    let mut it = v.strip_prefix("f ")?.split(' ');
    let sf = it.next()?.parse().ok()?;
    let xs: Vec<f64> = it.map(|x| x.parse().ok()).collect::<Option<_>>()?;
    Some((sf, xs.try_into().ok()?))
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
    /// is empty), current one included. A redelivered payment (same eid) is counted once, with its latest risk, and never
    /// reads its own earlier member as a prior payment; payments with a later minute than this one are not counted (they
    /// can be stored already when this one is rescored). Returns (payments, noisy-OR).
    pub fn pair_window(&mut self, p: &Payment, payee: &str, risk: f64) -> redis::RedisResult<(usize, f64)> {
        let mut pl = redis::pipe();
        self.pair_window_cmds(&mut pl, p, payee, risk);
        let (members,): (Vec<String>,) = pl.query(&mut self.con)?;
        Ok(Self::pair_window_of(&members))
    }

    /// Append the pair window's commands to `pl`: one reply that is not ignored, the window's members (current payment
    /// included), for `pair_window_of`.
    pub fn pair_window_cmds(&self, pl: &mut redis::Pipeline, p: &Payment, payee: &str, risk: f64) {
        self.append_pair_window(pl, p, payee, risk, None);
    }

    /// Append a pair window update that returns the first live scoring attempt's (count, noisy-OR).
    pub(crate) fn live_pair_score_cmds(&self, pl: &mut redis::Pipeline, p: &Payment, payee: &str, risk: f64, eid: i64) {
        self.append_pair_window(pl, p, payee, risk, Some(self.k("pair-score", &eid.to_string())));
    }

    fn append_pair_window(&self, pl: &mut redis::Pipeline, p: &Payment, payee: &str, risk: f64, snapshot: Option<String>) {
        let key = format!("{}>{}", self.k("pr", &format!("{}|{}", p.from_bank, p.from_account)), payee);
        let eid = p.eid.map(|e| e.to_string()).unwrap_or_else(|| format!("{}.{}", p.tmin, std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        // Save the aggregate in the same script as the first pair update. A retry can then avoid touching the
        // rolling window, even after a later payment prunes it. Keep the highest risk per eid, in first-seen
        // order, as pair_window_of does for legacy members. Redis rounds Lua numeric replies to integers.
        const PAIR: &str = r#"
            if KEYS[2] then
                local score = redis.call('GET', KEYS[2])
                if score then return {string.match(score, '^(%d+) (.+)$')} end
            end
            redis.call('ZREMRANGEBYSCORE', KEYS[1], '-inf', ARGV[1])
            for _, m in ipairs(redis.call('ZRANGE', KEYS[1], 0, -1)) do
                if string.sub(m, 1, #ARGV[3]) == ARGV[3] then redis.call('ZREM', KEYS[1], m) end
            end
            redis.call('ZADD', KEYS[1], ARGV[2], ARGV[3] .. ARGV[4])
            redis.call('EXPIRE', KEYS[1], ARGV[5])
            local members = redis.call('ZRANGEBYSCORE', KEYS[1], '-inf', ARGV[2])
            if not KEYS[2] then return members end
            local order, risks = {}, {}
            for _, member in ipairs(members) do
                local event, raw = string.match(member, '^(.*)|([^|]*)$')
                event = event or member
                local value = tonumber(raw) or 0
                if risks[event] == nil then table.insert(order, event) end
                risks[event] = math.max(risks[event] or value, value)
            end
            local product = 1
            for _, event in ipairs(order) do product = product * (1 - risks[event]) end
            local count, score = tostring(#order), string.format('%.17g', 1 - product)
            redis.call('SET', KEYS[2], count .. ' ' .. score, 'EX', ARGV[6])
            return {count, score}
        "#;
        pl.cmd("EVAL").arg(PAIR).arg(1 + usize::from(snapshot.is_some())).arg(&key).arg(&snapshot)
            .arg(p.tmin - MIN24).arg(p.tmin).arg(format!("{eid}|")).arg(risk.to_string()).arg(MIN24 * 60 + 3600).arg(SEEN_TTL_S);
    }

    /// (payments, noisy-OR) of the window's members. Members written before the window kept one per eid may repeat an
    /// eid: each eid counts once, with its highest risk.
    pub fn pair_window_of(members: &[String]) -> (usize, f64) {
        let mut by_eid: indexmap::IndexMap<&str, f64> = indexmap::IndexMap::new();
        for m in members {
            let (eid, r) = m.rsplit_once('|').map(|(e, r)| (e, r.parse::<f64>().unwrap_or(0.0))).unwrap_or((m, 0.0));
            let e = by_eid.entry(eid).or_insert(r);
            *e = e.max(r);
        }
        let prod: f64 = by_eid.values().map(|r| 1.0 - r).product();
        (by_eid.len(), 1.0 - prod)
    }

    fn k(&self, kind: &str, acct: &str) -> String {
        format!("{}{}:{{{}}}", self.px, kind, acct)
    }

    pub fn features(&mut self, p: &Payment) -> redis::RedisResult<Features> {
        let (f, w) = self.features_read(p)?;
        w.query::<i64>(&mut self.con)?;
        Ok(f)
    }

    /// The read half of `features` (one pipelined round trip) and the account-state write it implies, as a command the
    /// caller sends (alone, or in one round trip with the pair window).
    pub fn features_read(&mut self, p: &Payment) -> redis::RedisResult<(Features, redis::Cmd)> {
        let s = format!("{}|{}", p.from_bank, p.from_account);
        let d = format!("{}|{}", p.to_bank, p.to_account);
        let t = p.tmin;
        let bp = format!("{}>{}", p.from_bank, p.to_bank);
        let eid = p.eid.map(|e| e.to_string()).unwrap_or_else(|| format!("{t}.{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let lo = format!("({}", t - MIN24);
        let bpk = format!("{}bp", self.px);
        // seen:{eid} of a payment already written holds the features it was scored with (never set for a payment without
        // an eid, whose made-up eid is unique)
        let seen_key = self.k("seen", &eid);
        let (sa, da): (Vec<Option<String>>, Vec<Option<String>>);
        let (seen, recip, bp_seen): (bool, bool, bool);
        let (wo, wi, wi_s): (Vec<String>, Vec<String>, Vec<String>);
        let first: Option<String>;
        (sa, da, seen, recip, bp_seen, wo, wi, wi_s, first) = redis::pipe()
            .cmd("HMGET").arg(self.k("a", &s)).arg(&A_S[..])
            .cmd("HMGET").arg(self.k("a", &d)).arg(&A_D[..])
            .cmd("SISMEMBER").arg(self.k("p", &s)).arg(&d)
            .cmd("SISMEMBER").arg(self.k("p", &d)).arg(&s)
            .cmd("SISMEMBER").arg(&bpk).arg(&bp)
            .cmd("ZRANGEBYSCORE").arg(self.k("wo", &s)).arg(&lo).arg("+inf")
            .cmd("ZRANGEBYSCORE").arg(self.k("wi", &d)).arg(&lo).arg("+inf")
            .cmd("ZRANGEBYSCORE").arg(self.k("wi", &s)).arg(&lo).arg("+inf")
            .cmd("GET").arg(&seen_key)
            .query(&mut self.con)?;
        self.last_sender_fan_in = wi_s.iter().map(|m| cp(m).0).collect::<HashSet<_>>().len();
        let nan = f64::NAN;
        let on = int(&sa[0]).unwrap_or(0);
        let ousd = float(&sa[1]).unwrap_or(0.0);
        let (mut f, usd) = self.cfg.payment_features(p, s == d);
        use fx::*;
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
        let stored = format!("f {} {}", self.last_sender_fan_in, f.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(" "));
        // a redelivered payment: its own write is in the state read above, so score it from the features it was first
        // scored with (the write below is then skipped by the claim)
        if let Some((sf, f0)) = first.as_deref().and_then(decode_features) {
            self.last_sender_fan_in = sf;
            f = f0;
        }
        // ---- writes, as one script: claim seen:{eid} with SET NX, then apply every write, or nothing when the claim
        // exists. At-least-once delivery replays a payment scored before a crash: it is rescored from the current state
        // (so a decision is still emitted) without writing again. A payment without an eid has no identity to dedupe
        // on and always writes. ARGV after the TTL is a flat list of commands, each prefixed by its word count.
        const CLAIM_AND_WRITE: &str = "if KEYS[1] and not redis.call('SET', KEYS[1], ARGV[2], 'NX', 'EX', ARGV[1]) then return 0 end \
            local i = 3 while i <= #ARGV do local n = tonumber(ARGV[i]) redis.call(unpack(ARGV, i + 1, i + n)) i = i + n + 1 end return 1";
        let (ka_s, ka_d, ko_s, ki_d) = (self.k("a", &s), self.k("a", &d), self.k("wo", &s), self.k("wi", &d));
        let (u, ts, old) = (py_repr(usd), t.to_string(), (t - MIN24).to_string());
        let (mo, mi) = (format!("{eid}|{d}|{u}"), format!("{eid}|{s}|{u}"));
        let mut w: Vec<Vec<&str>> = vec![
            vec!["HSETNX", &ka_s, "first_out_t", &ts],
            vec!["HINCRBY", &ka_s, "out_n", "1"],
            vec!["HINCRBYFLOAT", &ka_s, "out_usd", &u],
            vec!["HSET", &ka_s, "last_out_t", &ts],
        ];
        let kp_s = self.k("p", &s);
        if !seen {
            w.extend([vec!["SADD", &kp_s, &d], vec!["HINCRBY", &ka_s, "uniq_dst", "1"], vec!["HINCRBY", &ka_d, "uniq_src", "1"]]);
        }
        w.extend([
            vec!["HINCRBY", &ka_d, "in_n", "1"],
            vec!["HINCRBYFLOAT", &ka_d, "in_usd", &u],
            vec!["HSET", &ka_d, "last_in_t", &ts, "last_in_usd", &u],
            vec!["ZADD", &ko_s, &ts, &mo],
            vec!["ZADD", &ki_d, &ts, &mi],
            vec!["ZREMRANGEBYSCORE", &ko_s, "-inf", &old],
            vec!["ZREMRANGEBYSCORE", &ki_d, "-inf", &old],
            vec!["EXPIRE", &ko_s, "172800"],
            vec!["EXPIRE", &ki_d, "172800"],
        ]);
        if !bp_seen {
            w.push(vec!["SADD", &bpk, &bp]);
        }
        let seen_key = p.eid.map(|_| seen_key);
        let mut c = redis::cmd("EVAL");
        c.arg(CLAIM_AND_WRITE).arg(seen_key.is_some() as i32).arg(&seen_key).arg(SEEN_TTL_S).arg(&stored);
        for x in &w {
            c.arg(x.len()).arg(x);
        }
        Ok((Features(f), c))
    }

    pub fn sender_fan_in_24h(&mut self, p: &Payment) -> redis::RedisResult<usize> {
        let s = format!("{}|{}", p.from_bank, p.from_account);
        let m: Vec<String> = redis::cmd("ZRANGEBYSCORE").arg(self.k("wi", &s)).arg(format!("({}", p.tmin - MIN24)).arg("+inf").query(&mut self.con)?;
        Ok(m.iter().map(|x| cp(x).0).collect::<HashSet<_>>().len())
    }
}
