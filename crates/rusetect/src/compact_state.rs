//! Compact account state for bank scale streaming (port of service/compact_state.py).
//!
//! Same features as Engine::features, from much smaller state:
//!   * accounts get integer ids; per account totals live in flat typed vectors
//!   * "distinct payees / payers so far" are counters that move only when the pair index says "first time"
//!   * 24 h windows exist only for accounts active in the last 24 h and are dropped when they empty
//!   * the pair index ("has A paid B before") is pluggable:
//!       exact  a set of 64 bit pair ids   (identical features; memory grows with distinct pairs)
//!       bloom  a fixed size Bloom filter  (fixed memory; a false positive can only turn a new pair into "seen")
use crate::engine::{fx, CascadeConfig, Features, Window, MIN24};
use crate::payment::Payment;
use crate::pyfmt::hour_dow;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

pub trait PairIndex: Send {
    fn seen_add(&mut self, key: u64) -> bool;
    fn seen(&self, key: u64) -> bool;
    fn nbytes(&self) -> usize;
    fn kind(&self) -> &'static str;
}

#[derive(Default)]
pub struct ExactPairs(HashSet<u64>);

impl PairIndex for ExactPairs {
    fn seen_add(&mut self, key: u64) -> bool {
        !self.0.insert(key)
    }
    fn seen(&self, key: u64) -> bool {
        self.0.contains(&key)
    }
    fn nbytes(&self) -> usize {
        self.0.capacity() * 9
    }
    fn kind(&self) -> &'static str {
        "exact"
    }
}

/// k hash Bloom filter over 64 bit pair ids, m bits sized for `capacity` at `fp_rate`.
pub struct BloomPairs {
    pub k: usize,
    pub m: u64,
    bits: Vec<u8>,
}

impl BloomPairs {
    pub fn new(capacity: u64, fp_rate: f64) -> Self {
        let ln2 = std::f64::consts::LN_2;
        let m = (-(capacity as f64) * fp_rate.ln() / (ln2 * ln2)) as u64;
        let k = ((m as f64 / capacity as f64 * ln2).round_ties_even() as usize).max(1);
        BloomPairs { k, m, bits: vec![0u8; m.div_ceil(8) as usize] }
    }

    fn idx(&self, key: u64) -> impl Iterator<Item = u64> + '_ {
        let h1 = key.wrapping_mul(0x9E3779B97F4A7C15);
        let h2 = (key ^ (key >> 29)).wrapping_mul(0xBF58476D1CE4E5B9) | 1;
        (0..self.k as u64).map(move |i| h1.wrapping_add(i.wrapping_mul(h2)) % self.m)
    }
}

impl PairIndex for BloomPairs {
    fn seen(&self, key: u64) -> bool {
        self.idx(key).all(|i| self.bits[(i >> 3) as usize] >> (i & 7) & 1 == 1)
    }
    fn seen_add(&mut self, key: u64) -> bool {
        let idx: Vec<u64> = self.idx(key).collect();
        let mut hit = true;
        for i in idx {
            let (j, bit) = ((i >> 3) as usize, 1u8 << (i & 7));
            if self.bits[j] & bit == 0 {
                hit = false;
                self.bits[j] |= bit;
            }
        }
        hit
    }
    fn nbytes(&self) -> usize {
        self.bits.len()
    }
    fn kind(&self) -> &'static str {
        "bloom"
    }
}

pub enum PairKind {
    Exact,
    Bloom { capacity: u64, fp_rate: f64 },
}

const NONE: i64 = i64::MIN;

pub struct CompactState {
    pub cfg: CascadeConfig,
    kind: PairKind,
    ids: HashMap<String, u32>,
    out_n: Vec<i64>,
    in_n: Vec<i64>,
    uniq_dst: Vec<i64>,
    uniq_src: Vec<i64>,
    out_usd: Vec<f64>,
    in_usd: Vec<f64>,
    last_in_usd: Vec<f64>,
    last_out_t: Vec<i64>,
    first_out_t: Vec<i64>,
    last_in_t: Vec<i64>,
    win_out: HashMap<u32, Window>,
    win_in: HashMap<u32, Window>,
    pairs: Box<dyn PairIndex>,
    bank_pairs: HashSet<(String, String)>,
    pub n_seen: u64,
}

impl CompactState {
    pub fn new(cfg: CascadeConfig, kind: PairKind) -> Self {
        let pairs = Self::make_pairs(&kind);
        CompactState {
            cfg, kind, ids: HashMap::new(), out_n: vec![], in_n: vec![], uniq_dst: vec![], uniq_src: vec![], out_usd: vec![], in_usd: vec![],
            last_in_usd: vec![], last_out_t: vec![], first_out_t: vec![], last_in_t: vec![], win_out: HashMap::new(), win_in: HashMap::new(),
            pairs, bank_pairs: HashSet::new(), n_seen: 0,
        }
    }

    fn make_pairs(kind: &PairKind) -> Box<dyn PairIndex> {
        match kind {
            PairKind::Exact => Box::<ExactPairs>::default(),
            PairKind::Bloom { capacity, fp_rate } => Box::new(BloomPairs::new(*capacity, *fp_rate)),
        }
    }

    pub fn reset(&mut self) {
        let cfg = self.cfg.clone();
        let kind = std::mem::replace(&mut self.kind, PairKind::Exact);
        *self = Self::new(cfg, kind);
    }

    fn id(&mut self, bank: &str, acct: &str) -> u32 {
        let k = format!("{bank}|{acct}");
        if let Some(i) = self.ids.get(&k) {
            return *i;
        }
        let i = self.ids.len() as u32;
        self.ids.insert(k, i);
        for a in [&mut self.out_n, &mut self.in_n, &mut self.uniq_dst, &mut self.uniq_src] {
            a.push(0);
        }
        self.out_usd.push(0.0);
        self.in_usd.push(0.0);
        self.last_in_usd.push(f64::NAN);
        for a in [&mut self.last_out_t, &mut self.first_out_t, &mut self.last_in_t] {
            a.push(NONE);
        }
        i
    }

    pub fn sender_fan_in_24h(&self, p: &Payment) -> usize {
        self.ids.get(&format!("{}|{}", p.from_bank, p.from_account)).and_then(|i| self.win_in.get(i))
            .map(|w| w.distinct_since(p.tmin - MIN24)).unwrap_or(0)
    }

    pub fn features(&mut self, p: &Payment) -> Features {
        let s = self.id(&p.from_bank, &p.from_account);
        let d = self.id(&p.to_bank, &p.to_account);
        let (si, di) = (s as usize, d as usize);
        let t = p.tmin;
        let usd = self.cfg.usd(p.amount_paid, &p.payment_currency);
        let nan = f64::NAN;
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
        let on = self.out_n[si];
        f[S_OUT_N] = on as f64;
        f[S_OUT_MEAN_USD] = (self.out_usd[si] / on.max(1) as f64).ln_1p();
        let (lo, fo) = (self.last_out_t[si], self.first_out_t[si]);
        f[S_GAP_OUT] = if lo != NONE { (t - lo) as f64 } else { nan };
        f[S_AGE_OUT] = if fo != NONE { (t - fo) as f64 } else { 0.0 };
        let recip = self.pairs.seen((d as u64) << 32 | s as u64); // before adding this pair
        let new = !self.pairs.seen_add((s as u64) << 32 | d as u64);
        f[NEW_PAIR] = new as i64 as f64;
        f[S_UNIQ_DST] = self.uniq_dst[si] as f64;
        f[D_IN_N] = self.in_n[di] as f64;
        let li = self.last_in_t[di];
        f[D_GAP_IN] = if li != NONE { (t - li) as f64 } else { nan };
        f[D_UNIQ_SRC] = self.uniq_src[di] as f64;
        f[S_IN_N] = self.in_n[si] as f64;
        let sli = self.last_in_t[si];
        f[S_MINS_SINCE_IN] = if sli != NONE { (t - sli) as f64 } else { nan };
        let liu = self.last_in_usd[si];
        f[S_OUT_OVER_LAST_IN] = if !liu.is_nan() && liu != 0.0 { usd / liu } else { nan };
        f[S_PASSTHROUGH] = self.out_usd[si] / self.in_usd[si].max(1.0);
        f[D_OUT_N] = (self.out_n[di] + if s == d { 1 } else { 0 }) as f64;
        f[D_IN_USD_LOG] = self.in_usd[di].ln_1p();
        let (a, b, c) = self.win_out.entry(s).or_default().add(t, d, usd);
        f[S_FANOUT_24H] = a as f64;
        f[S_CNT_24H] = b as f64;
        f[S_USD_24H] = c.ln_1p();
        let (a, b, _) = self.win_in.entry(d).or_default().add(t, s, usd);
        f[D_FANIN_24H] = a as f64;
        f[D_CNT_24H] = b as f64;
        f[RECIPROCAL] = recip as i64 as f64;
        let bp = (p.from_bank.clone(), p.to_bank.clone());
        f[NEW_BANK_PAIR] = (!self.bank_pairs.contains(&bp)) as i64 as f64;
        // ---- update state (same order as Engine::features)
        if fo == NONE {
            self.first_out_t[si] = t;
        }
        self.out_n[si] = on + 1;
        self.out_usd[si] += usd;
        self.last_out_t[si] = t;
        if new {
            self.uniq_dst[si] += 1;
            self.uniq_src[di] += 1;
        }
        self.in_n[di] += 1;
        self.in_usd[di] += usd;
        self.last_in_t[di] = t;
        self.last_in_usd[di] = usd;
        self.bank_pairs.insert(bp);
        self.n_seen += 1;
        if self.n_seen.is_multiple_of(200_000) {
            self.evict(t);
        }
        Features(f)
    }

    /// Drop windows with nothing in the last 24 h (they would return zeros anyway).
    pub fn evict(&mut self, now: i64) {
        for w in [&mut self.win_out, &mut self.win_in] {
            w.retain(|_, x| x.last_t().map(|lt| lt > now - MIN24).unwrap_or(false));
        }
    }

    pub fn stats(&self) -> Value {
        let n = self.ids.len();
        json!({"accounts": n, "active_windows": self.win_out.len() + self.win_in.len(), "pair_index": self.pairs.kind(),
               "pair_index_bytes": self.pairs.nbytes(), "account_arrays_bytes": n * 8 * 10})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{paths, payment, Engine};

    fn rows(n: usize) -> Vec<Payment> {
        let r = payment::read_demo_rows(&paths::models_dir().join("demo_stream.csv.gz")).unwrap();
        r.iter().take(n).map(|x| Payment::from_demo_row(x).unwrap()).collect()
    }

    #[test]
    fn compact_exact_matches_engine() {
        let mut eng = Engine::new(&paths::models_dir(), false).unwrap();
        let mut cs = CompactState::new(eng.cfg.clone(), PairKind::Exact);
        for p in rows(40_000) {
            assert_eq!(eng.sender_fan_in_24h(&p), cs.sender_fan_in_24h(&p));
            let (fa, fb) = (eng.features(&p), cs.features(&p));
            assert!(fa.same(&fb), "{:?} vs {:?}", fa, fb);
        }
    }

    #[test]
    fn bloom_only_errs_towards_seen() {
        let mut eng = Engine::new(&paths::models_dir(), false).unwrap();
        let mut cs = CompactState::new(eng.cfg.clone(), PairKind::Bloom { capacity: 200_000, fp_rate: 0.01 });
        let mut wrong = 0;
        for p in rows(40_000) {
            let (fa, fb) = (eng.features(&p), cs.features(&p));
            if fa.0[fx::NEW_PAIR] != fb.0[fx::NEW_PAIR] {
                assert!(fa.0[fx::NEW_PAIR] == 1.0 && fb.0[fx::NEW_PAIR] == 0.0);
                wrong += 1;
            }
        }
        assert!(wrong < 400);
    }
}
