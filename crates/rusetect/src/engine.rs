//! RuseTect streaming engine (port of service/rusetect_engine.py): the online version of the offline feature
//! pipeline plus the cascade.
//!
//! Keeps per-account state in memory, computes the same causal features for each incoming payment (only past
//! payments are used), then runs:
//!   L1 gatekeeper (30-tree XGBoost)  -> clean traffic approved
//!   L2 graph-boost (XGBoost + LightGBM average) on survivors
//!   Decision band + top reasons (XGBoost SHAP contributions)
//!   L4 ring linker: alerted payments are unioned into cases by shared accounts
//! L0 onboarding (BAF LightGBM with age-group thresholds) scores new account applications.
//!
//! Accounts are interned to integer ids, so the pair index is a set of u64 and windows key on u32; the features
//! are identical to the Python engine (checked against it on the bundled demo stream).
use crate::models::{LiteBooster, XgbBooster};
use crate::payment::Payment;
use crate::pyfmt::{fmt_comma, group_thousands, hour_dow, jf, py_round, py_sum};
use anyhow::{bail, Context, Result};
use indexmap::IndexMap;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::time::Instant;

pub const MIN24: i64 = 1440;

/// Live feature names, in the engine's (and the models') order.
pub const FEATURES: [&str; 31] = [
    "log_usd", "fmt_code", "cur_code", "hour", "dow", "cross_bank", "cross_cur", "self_loop", "round_100",
    "s_out_n", "s_out_mean_usd", "s_gap_out", "s_age_out", "new_pair", "s_uniq_dst",
    "d_in_n", "d_gap_in", "d_uniq_src", "s_in_n", "s_mins_since_in", "s_out_over_last_in", "s_passthrough",
    "d_out_n", "d_in_usd_log", "s_fanout_24h", "s_cnt_24h", "s_usd_24h", "d_fanin_24h", "d_cnt_24h", "reciprocal",
    "new_bank_pair",
];
/// Features that are floats in Python (all others are ints, or NaN when unknown).
pub const FLOAT_FEATURES: [&str; 6] = ["log_usd", "s_out_mean_usd", "s_out_over_last_in", "s_passthrough", "d_in_usd_log", "s_usd_24h"];

pub mod fx {
    pub const LOG_USD: usize = 0;
    pub const FMT_CODE: usize = 1;
    pub const CUR_CODE: usize = 2;
    pub const HOUR: usize = 3;
    pub const DOW: usize = 4;
    pub const CROSS_BANK: usize = 5;
    pub const CROSS_CUR: usize = 6;
    pub const SELF_LOOP: usize = 7;
    pub const ROUND_100: usize = 8;
    pub const S_OUT_N: usize = 9;
    pub const S_OUT_MEAN_USD: usize = 10;
    pub const S_GAP_OUT: usize = 11;
    pub const S_AGE_OUT: usize = 12;
    pub const NEW_PAIR: usize = 13;
    pub const S_UNIQ_DST: usize = 14;
    pub const D_IN_N: usize = 15;
    pub const D_GAP_IN: usize = 16;
    pub const D_UNIQ_SRC: usize = 17;
    pub const S_IN_N: usize = 18;
    pub const S_MINS_SINCE_IN: usize = 19;
    pub const S_OUT_OVER_LAST_IN: usize = 20;
    pub const S_PASSTHROUGH: usize = 21;
    pub const D_OUT_N: usize = 22;
    pub const D_IN_USD_LOG: usize = 23;
    pub const S_FANOUT_24H: usize = 24;
    pub const S_CNT_24H: usize = 25;
    pub const S_USD_24H: usize = 26;
    pub const D_FANIN_24H: usize = 27;
    pub const D_CNT_24H: usize = 28;
    pub const RECIPROCAL: usize = 29;
    pub const NEW_BANK_PAIR: usize = 30;
}

/// The 31 live features of one payment, by name order of [`FEATURES`]. NaN = unknown (None in the API).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Features(pub [f64; 31]);

impl Features {
    pub fn get(&self, name: &str) -> f64 {
        self.0[FEATURES.iter().position(|f| *f == name).expect("unknown feature")]
    }

    /// Bit-exact equality treating NaN == NaN (for parity tests).
    pub fn same(&self, other: &Features) -> bool {
        self.0.iter().zip(other.0.iter()).all(|(a, b)| (a.is_nan() && b.is_nan()) || a == b)
    }

    /// JSON value of one feature as the Python engine reports it (int, float, or null).
    pub fn value(&self, i: usize) -> Value {
        let v = self.0[i];
        if v.is_nan() {
            Value::Null
        } else if FLOAT_FEATURES.contains(&FEATURES[i]) {
            jf(v)
        } else {
            json!(v as i64)
        }
    }

    /// `{name: value}` in engine order.
    pub fn to_json(&self) -> Map<String, Value> {
        FEATURES.iter().enumerate().map(|(i, k)| (k.to_string(), self.value(i))).collect()
    }
}

pub const READABLE: [(&str, &str); 31] = [
    ("log_usd", "Payment amount"), ("fmt_code", "Payment format"), ("cur_code", "Currency"), ("hour", "Hour of day"),
    ("dow", "Day of week"), ("cross_bank", "Cross bank transfer"), ("cross_cur", "Currency conversion"),
    ("self_loop", "Self transfer"), ("round_100", "Round amount"),
    ("s_out_n", "Sender: past payments"), ("s_out_mean_usd", "Sender: usual amount"), ("s_gap_out", "Sender: time since last payment"),
    ("s_age_out", "Sender: time active"), ("new_pair", "First payment to this receiver"), ("s_uniq_dst", "Sender: distinct receivers"),
    ("d_in_n", "Receiver: past receipts"), ("d_gap_in", "Receiver: time since last receipt"), ("d_uniq_src", "Receiver: distinct senders"),
    ("s_in_n", "Sender: money received before"), ("s_mins_since_in", "Sender: minutes since money came in"),
    ("s_out_over_last_in", "Amount vs. last money received"), ("s_passthrough", "Sender: pass through ratio"),
    ("d_out_n", "Receiver: onward payments"), ("d_in_usd_log", "Receiver: money received so far"),
    ("s_fanout_24h", "Sender fan out (24h)"), ("s_cnt_24h", "Sender payments (24h)"), ("s_usd_24h", "Sender volume (24h)"),
    ("d_fanin_24h", "Receiver fan in (24h)"), ("d_cnt_24h", "Receiver receipts (24h)"), ("reciprocal", "Money flowing back (cycle)"),
    ("new_bank_pair", "New bank corridor"),
];

pub fn readable(k: &str) -> &str {
    READABLE.iter().find(|(f, _)| *f == k).map(|(_, r)| *r).unwrap_or(k)
}

/// 24h sliding window with O(1) amortised distinct-counterparty count and running sum.
#[derive(Debug, Default, Clone)]
pub struct Window {
    pub q: VecDeque<(i64, u32, f64)>,
    cnt: HashMap<u32, u32>,
    usd: f64,
}

impl Window {
    pub fn add(&mut self, t: i64, key: u32, usd: f64) -> (usize, usize, f64) {
        self.q.push_back((t, key, usd));
        *self.cnt.entry(key).or_insert(0) += 1;
        self.usd += usd;
        while self.q.front().map(|e| e.0 <= t - MIN24).unwrap_or(false) {
            let (_, k, u) = self.q.pop_front().unwrap();
            self.usd -= u;
            let c = self.cnt.get_mut(&k).unwrap();
            *c -= 1;
            if *c == 0 {
                self.cnt.remove(&k);
            }
        }
        (self.cnt.len(), self.q.len(), self.usd.max(0.0))
    }

    /// Distinct counterparties with an entry strictly after t0.
    pub fn distinct_since(&self, t0: i64) -> usize {
        self.q.iter().filter(|e| e.0 > t0).map(|e| e.1).collect::<HashSet<_>>().len()
    }

    pub fn last_t(&self) -> Option<i64> {
        self.q.back().map(|e| e.0)
    }
}

#[derive(Debug, Default, Clone)]
pub struct Acct {
    pub out_n: i64,
    pub out_usd: f64,
    pub last_out_t: Option<i64>,
    pub first_out_t: Option<i64>,
    pub dsts: HashSet<u32>,
    pub in_n: i64,
    pub in_usd: f64,
    pub last_in_t: Option<i64>,
    pub last_in_usd: Option<f64>,
    pub srcs: HashSet<u32>,
    pub win_out: Window,
    pub win_in: Window,
}

/// Parsed service/models/cascade_config.json.
#[derive(Debug, Clone)]
pub struct CascadeConfig {
    pub raw: Value,
    pub features: Vec<String>,
    pub usd_rates: HashMap<String, f64>,
    pub fmt_codes: IndexMap<String, i64>,
    pub cur_codes: IndexMap<String, i64>,
    pub l1_threshold: f64,
    pub block_threshold: f64,
    pub hold_threshold: f64,
    pub verify_threshold: f64,
}

impl CascadeConfig {
    pub fn load(models_dir: &Path) -> Result<Self> {
        let raw: Value = serde_json::from_str(&std::fs::read_to_string(models_dir.join("cascade_config.json")).context("cascade_config.json")?)?;
        Self::from_json(raw)
    }

    pub fn from_json(raw: Value) -> Result<Self> {
        let mut missing: Vec<&str> = ["l1_threshold", "block_threshold", "hold_threshold", "verify_threshold"]
            .into_iter().filter(|k| raw.get(*k).is_none()).collect();
        missing.sort();
        if !missing.is_empty() {
            bail!("cascade_config.json is missing thresholds: {:?}", missing);
        }
        let codes = |k: &str| -> IndexMap<String, i64> {
            raw[k].as_object().map(|m| m.iter().map(|(a, b)| (a.clone(), b.as_i64().unwrap_or(0))).collect()).unwrap_or_default()
        };
        let features: Vec<String> = raw["features"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
        for f in &features {
            if !FEATURES.contains(&f.as_str()) {
                bail!("cascade_config.json names an unknown live feature: {f}");
            }
        }
        Ok(CascadeConfig {
            features,
            usd_rates: raw["usd_rates"].as_object().map(|m| m.iter().map(|(a, b)| (a.clone(), b.as_f64().unwrap_or(1.0))).collect()).unwrap_or_default(),
            fmt_codes: codes("fmt_codes"),
            cur_codes: codes("cur_codes"),
            l1_threshold: raw["l1_threshold"].as_f64().unwrap_or(0.0),
            block_threshold: raw["block_threshold"].as_f64().unwrap_or(1.0),
            hold_threshold: raw["hold_threshold"].as_f64().unwrap_or(1.0),
            verify_threshold: raw["verify_threshold"].as_f64().unwrap_or(1.0),
            raw,
        })
    }

    pub fn usd(&self, amount: f64, currency: &str) -> f64 {
        amount * self.usd_rates.get(currency).copied().unwrap_or(1.0)
    }

    /// Column order of the model input vector.
    pub fn feature_index(&self) -> Vec<usize> {
        self.features.iter().map(|f| FEATURES.iter().position(|x| x == f).unwrap()).collect()
    }
}

/// L0 onboarding configuration (service/models/l0_config.json).
#[derive(Debug, Clone)]
pub struct L0Config {
    pub features: Vec<String>,
    pub categorical: HashSet<String>,
    pub categories: HashMap<String, Vec<String>>,
    pub age_50_plus: f64,
    pub age_under_50: f64,
}

pub struct Models {
    pub l1: XgbBooster,
    pub l2x: XgbBooster,
    pub l2l: LiteBooster,
    pub l0: LiteBooster,
    pub l0cfg: L0Config,
    pub hardening: Option<Hardening>,
}

/// Format-hardened risk (research/hardened_eval.py, adopted under its pre-registered rule): a launderer who re-sends a
/// payment in another format drops the cascade's PR-AUC from 0.71 to under 0.01, so a format-free XGBoost gives a
/// second opinion. risk = max(iso_served(cascade risk), iso_free(format-free score)); both maps were fitted on
/// validation and the band thresholds keep the cascade's validation alert rates.
pub struct Hardening {
    pub model: XgbBooster,
    /// positions of the model's features in the engine's feature vector
    pub cols: Vec<usize>,
    pub iso_served: (Vec<f64>, Vec<f64>),
    pub iso_free: (Vec<f64>, Vec<f64>),
    pub verify: f64,
    pub hold: f64,
    pub block: f64,
    /// aggregate over all of a payer's payments ("payer", research/account_aggregation.py) instead of each pair
    pub payer_window: bool,
}

fn out_features(out: &Map<String, Value>) -> Map<String, Value> {
    out.get("features").and_then(|v| v.as_object()).cloned().unwrap_or_default()
}

/// np.interp: linear between knots, clamped outside them.
/// 24 h causal noisy-OR of risks per key, current payment included (the structuring defence). Keys idle for a day are
/// swept every 65,536 pushes, so memory follows the last day's traffic instead of every pair ever seen.
pub struct PairWindow<K> {
    q: HashMap<K, VecDeque<(i64, f64)>>,
    pushes: u32,
}

impl<K: std::hash::Hash + Eq> PairWindow<K> {
    pub fn new() -> Self {
        PairWindow { q: HashMap::new(), pushes: 0 }
    }

    /// add risk `r` at minute `t`; returns (payments in the window, noisy-OR of their risks)
    pub fn push(&mut self, key: K, t: i64, r: f64) -> (usize, f64) {
        self.pushes = self.pushes.wrapping_add(1);
        if self.pushes.is_multiple_of(65_536) {
            self.q.retain(|_, d| d.back().is_some_and(|x| x.0 > t - 1440));
        }
        let d = self.q.entry(key).or_default();
        while d.front().is_some_and(|x| x.0 <= t - 1440) {
            d.pop_front();
        }
        d.push_back((t, r));
        (d.len(), 1.0 - d.iter().map(|x| 1.0 - x.1).product::<f64>())
    }

    pub fn keys(&self) -> usize {
        self.q.len()
    }

    pub fn clear(&mut self) {
        self.q.clear();
    }
}

impl<K: std::hash::Hash + Eq> Default for PairWindow<K> {
    fn default() -> Self {
        Self::new()
    }
}

pub fn interp(m: &(Vec<f64>, Vec<f64>), v: f64) -> f64 {
    let (x, y) = m;
    if x.is_empty() { return v; }
    if v <= x[0] { return y[0]; }
    if v >= x[x.len() - 1] { return y[y.len() - 1]; }
    let i = x.partition_point(|k| *k <= v);
    let (x0, x1, y0, y1) = (x[i - 1], x[i], y[i - 1], y[i]);
    if x1 == x0 { y1 } else { y0 + (y1 - y0) * (v - x0) / (x1 - x0) }
}

impl Hardening {
    /// Loads service/models/hardening.json if present (None when the file is absent).
    pub fn load(dir: &Path, features: &[String]) -> Result<Option<Self>> {
        let path = dir.join("hardening.json");
        if !path.exists() {
            return Ok(None);
        }
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
        let nums = |a: &Value| -> Vec<f64> { a.as_array().map(|a| a.iter().filter_map(|x| x.as_f64()).collect()).unwrap_or_default() };
        let iso = |k: &str| (nums(&v[k]["x"]), nums(&v[k]["y"]));
        let cols = v["features"].as_array().ok_or_else(|| anyhow::anyhow!("hardening.json: features"))?.iter()
            .map(|n| n.as_str().and_then(|n| features.iter().position(|f| f == n)).ok_or_else(|| anyhow::anyhow!("hardening.json: unknown feature {n}")))
            .collect::<Result<Vec<_>>>()?;
        let thr = |k: &str| v["thresholds"][k].as_f64().ok_or_else(|| anyhow::anyhow!("hardening.json: thresholds.{k}"));
        Ok(Some(Hardening { model: XgbBooster::load(&dir.join(v["model"].as_str().unwrap_or("hardening_xgb.json")))?, cols,
                            iso_served: iso("iso_served"), iso_free: iso("iso_free"), verify: thr("verify")?, hold: thr("hold")?, block: thr("block")?,
                            payer_window: v["window"].as_str() == Some("payer") }))
    }

    /// (hardened risk, calibrated format-free score) for the engine's feature vector and the cascade's risk.
    pub fn risk(&self, x: &[f32], served: f64) -> (f64, f64) {
        let sub: Vec<f32> = self.cols.iter().map(|&i| x[i]).collect();
        let free = interp(&self.iso_free, self.model.predict(&sub) as f64);
        (interp(&self.iso_served, served).max(free), free)
    }
}

impl Models {
    pub fn load(dir: &Path) -> Result<Self> {
        let l0raw: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("l0_config.json"))?)?;
        let strs = |v: &Value| -> Vec<String> { v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default() };
        let l0cfg = L0Config {
            features: strs(&l0raw["features"]),
            categorical: strs(&l0raw["categorical"]).into_iter().collect(),
            categories: l0raw["categories"].as_object().map(|m| m.iter().map(|(k, v)| (k.clone(), strs(v))).collect()).unwrap_or_default(),
            age_50_plus: l0raw["age_50_plus"].as_f64().unwrap_or(0.5),
            age_under_50: l0raw["age_under_50"].as_f64().unwrap_or(0.5),
        };
        Ok(Models {
            l1: XgbBooster::load(&dir.join("l1_gatekeeper.json"))?,
            l2x: XgbBooster::load(&dir.join("l2_xgb.json"))?,
            l2l: LiteBooster::load(&dir.join("l2_lgbm.json"))?,
            l0: LiteBooster::load(&dir.join("l0_onboarding_lgbm.json"))?,
            l0cfg,
            hardening: None,
        })
    }
}

#[derive(Debug, Clone)]
pub struct CaseEdge {
    pub from: String,
    pub to: String,
    pub amount: f64,
    pub format: String,
}

pub struct Engine {
    pub cfg: CascadeConfig,
    col: Vec<usize>,
    ids: HashMap<(String, String), u32>,
    pub accts: Vec<Acct>,
    banks: HashMap<String, u32>,
    pairs: HashSet<u64>,
    bank_pairs: HashSet<u64>,
    parent: HashMap<String, String>,
    pub case_alerts: IndexMap<String, Vec<CaseEdge>>,
    pub n_seen: u64,
    pub decisions: IndexMap<String, i64>,
    /// served risks of each payer->payee pair in the last 24 h (structuring defence)
    pair_risks: PairWindow<(u32, u32)>,
    pub models: Option<Models>,
}

/// Stable case id (the Python engine used the per-process randomised `hash()`; this one survives restarts).
pub fn case_id(root: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in root.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("CASE-{}", h % 100_000_000)
}

pub const DECISIONS: [&str; 4] = ["Approve", "Step-up verification", "Hold for analyst", "Block + STR draft"];

pub fn decision_rank(d: &str) -> usize {
    DECISIONS.iter().position(|x| *x == d).unwrap_or(0)
}

impl Engine {
    pub fn new(models_dir: &Path, load_models: bool) -> Result<Self> {
        let cfg = CascadeConfig::load(models_dir)?;
        let models = if load_models {
            let mut m = Models::load(models_dir)?;
            m.hardening = Hardening::load(models_dir, &cfg.features)?;
            Some(m)
        } else {
            None
        };
        Ok(Self::with_config(cfg, models))
    }

    pub fn with_config(cfg: CascadeConfig, models: Option<Models>) -> Self {
        let col = cfg.feature_index();
        Engine {
            cfg,
            col,
            ids: HashMap::new(),
            accts: Vec::new(),
            banks: HashMap::new(),
            pairs: HashSet::new(),
            bank_pairs: HashSet::new(),
            parent: HashMap::new(),
            case_alerts: IndexMap::new(),
            n_seen: 0,
            decisions: IndexMap::new(),
            pair_risks: PairWindow::new(),
            models,
        }
    }

    pub fn models_loaded(&self) -> bool {
        self.models.is_some()
    }

    /// Forget all account state (models stay loaded).
    pub fn reset(&mut self) {
        self.ids.clear();
        self.accts.clear();
        self.banks.clear();
        self.pairs.clear();
        self.bank_pairs.clear();
        self.parent.clear();
        self.case_alerts.clear();
        self.n_seen = 0;
        self.decisions.clear();
        self.pair_risks.clear();
    }

    pub fn n_accounts(&self) -> usize {
        self.accts.len()
    }

    /// Whether an account already has state (scoring it adds no memory).
    pub fn knows(&self, bank: &str, acct: &str) -> bool {
        self.ids.contains_key(&(bank.to_string(), acct.to_string()))
    }

    fn acct_id(&mut self, bank: &str, acct: &str) -> u32 {
        if let Some(i) = self.ids.get(&(bank.to_string(), acct.to_string())) {
            return *i;
        }
        let i = self.accts.len() as u32;
        self.ids.insert((bank.to_string(), acct.to_string()), i);
        self.accts.push(Acct::default());
        i
    }

    fn bank_id(&mut self, bank: &str) -> u32 {
        let n = self.banks.len() as u32;
        *self.banks.entry(bank.to_string()).or_insert(n)
    }

    pub fn lookup(&self, bank: &str, acct: &str) -> Option<&Acct> {
        self.ids.get(&(bank.to_string(), acct.to_string())).map(|i| &self.accts[*i as usize])
    }

    /// Distinct accounts that paid the sender in the 24 h before this payment (read before scoring).
    pub fn sender_fan_in_24h(&self, p: &Payment) -> usize {
        match self.lookup(&p.from_bank, &p.from_account) {
            None => 0,
            Some(a) => a.win_in.distinct_since(p.tmin - 24 * 60),
        }
    }

    /// Causal features of one payment; updates account state.
    pub fn features(&mut self, p: &Payment) -> Features {
        let s = self.acct_id(&p.from_bank, &p.from_account);
        let d = self.acct_id(&p.to_bank, &p.to_account);
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
        {
            let sa = &self.accts[si];
            f[S_OUT_N] = sa.out_n as f64;
            f[S_OUT_MEAN_USD] = (sa.out_usd / (sa.out_n.max(1) as f64)).ln_1p();
            f[S_GAP_OUT] = sa.last_out_t.map(|x| (t - x) as f64).unwrap_or(nan);
            f[S_AGE_OUT] = sa.first_out_t.map(|x| (t - x) as f64).unwrap_or(0.0);
            f[NEW_PAIR] = (!self.pairs.contains(&((s as u64) << 32 | d as u64))) as i64 as f64;
            f[S_UNIQ_DST] = sa.dsts.len() as f64;
            let da = &self.accts[di];
            f[D_IN_N] = da.in_n as f64;
            f[D_GAP_IN] = da.last_in_t.map(|x| (t - x) as f64).unwrap_or(nan);
            f[D_UNIQ_SRC] = da.srcs.len() as f64;
            f[S_IN_N] = sa.in_n as f64;
            f[S_MINS_SINCE_IN] = sa.last_in_t.map(|x| (t - x) as f64).unwrap_or(nan);
            f[S_OUT_OVER_LAST_IN] = match sa.last_in_usd {
                Some(l) if l != 0.0 => usd / l,
                _ => nan,
            };
            f[S_PASSTHROUGH] = sa.out_usd / sa.in_usd.max(1.0);
            f[D_OUT_N] = (da.out_n + if s == d { 1 } else { 0 }) as f64;
            f[D_IN_USD_LOG] = da.in_usd.ln_1p();
        }
        let (fo, co, uo) = self.accts[si].win_out.add(t, d, usd);
        f[S_FANOUT_24H] = fo as f64;
        f[S_CNT_24H] = co as f64;
        f[S_USD_24H] = uo.ln_1p();
        let (fi, ci, _) = self.accts[di].win_in.add(t, s, usd);
        f[D_FANIN_24H] = fi as f64;
        f[D_CNT_24H] = ci as f64;
        f[RECIPROCAL] = self.pairs.contains(&((d as u64) << 32 | s as u64)) as i64 as f64;
        let bp = ((self.bank_id(&p.from_bank) as u64) << 32) | self.bank_id(&p.to_bank) as u64;
        f[NEW_BANK_PAIR] = (!self.bank_pairs.contains(&bp)) as i64 as f64;
        // ---- update state
        {
            let sa = &mut self.accts[si];
            if sa.first_out_t.is_none() {
                sa.first_out_t = Some(t);
            }
            sa.out_n += 1;
            sa.out_usd += usd;
            sa.last_out_t = Some(t);
            sa.dsts.insert(d);
        }
        {
            let da = &mut self.accts[di];
            da.in_n += 1;
            da.in_usd += usd;
            da.last_in_t = Some(t);
            da.last_in_usd = Some(usd);
            da.srcs.insert(s);
        }
        self.pairs.insert((s as u64) << 32 | d as u64);
        self.bank_pairs.insert(bp);
        self.n_seen += 1;
        Features(f)
    }

    /// Model input row (float32, in the config's feature order).
    pub fn vec(&self, f: &Features) -> Vec<f32> {
        self.col.iter().map(|&i| f.0[i] as f32).collect()
    }

    /// (verify, hold, block) thresholds on the risk the engine reports: the hardened scale when hardening is loaded.
    pub fn thresholds(&self) -> (f64, f64, f64) {
        match self.models.as_ref().and_then(|m| m.hardening.as_ref()) {
            Some(h) => (h.verify, h.hold, h.block),
            None => (self.cfg.verify_threshold, self.cfg.hold_threshold, self.cfg.block_threshold),
        }
    }

    pub fn band_for_risk(&self, risk: f64) -> (&'static str, &'static str) {
        let (vt, ht, bt) = self.thresholds();
        if risk >= bt {
            ("Block + STR draft", "high")
        } else if risk >= ht {
            ("Hold for analyst", "med")
        } else if risk >= vt {
            ("Step-up verification", "med")
        } else {
            ("Approve", "safe")
        }
    }

    /// Score one payment through the cascade and update state. Returns the API's result object (key order as Python).
    pub fn score_payment(&mut self, p: &Payment) -> Map<String, Value> {
        let t0 = Instant::now();
        let f = self.features(p);
        let x = self.vec(&f);
        let m = self.models.as_ref().expect("models not loaded");
        let s1 = m.l1.predict(&x) as f64;
        let feats = f.to_json();
        let mut out = Map::new();
        out.insert("l1_score".into(), jf(py_round(s1, 5)));
        out.insert("features".into(), Value::Object(feats.clone()));
        if s1 < self.cfg.l1_threshold {
            out.insert("layer_exit".into(), json!("L1"));
            out.insert("risk".into(), jf(py_round(s1, 5)));
            out.insert("decision".into(), json!("Approve"));
            out.insert("band".into(), json!("safe"));
            out.insert("reasons".into(), json!([]));
            out.insert("case_id".into(), Value::Null);
        } else {
            let sx = m.l2x.predict(&x) as f64;
            let sl = m.l2l.predict_f32(&x);
            let risk = (sx + sl) / 2.0;
            // reasons are added in finish_risk, only for flagged payments (TreeSHAP is most of the scoring time)
            let reasons: Vec<Value> = Vec::new();
            let (decision, band) = self.band_for_risk(risk);
            // with hardening the final band is decided in finish_risk, which links the case then
            let hardened = self.models.as_ref().map(|m| m.hardening.is_some()).unwrap_or(false);
            let cid = if band == "high" && !hardened { Value::String(self.link(p)) } else { Value::Null };
            out.insert("layer_exit".into(), json!("L2"));
            out.insert("risk".into(), jf(py_round(risk, 5)));
            out.insert("l2_xgb".into(), jf(py_round(sx, 5)));
            out.insert("l2_lgbm".into(), jf(py_round(sl, 5)));
            out.insert("decision".into(), json!(decision));
            out.insert("band".into(), json!(band));
            out.insert("reasons".into(), Value::Array(reasons));
            out.insert("case_id".into(), cid);
        }
        self.finish_risk(p, &x, &mut out);
        let dec = out["decision"].as_str().unwrap().to_string();
        *self.decisions.entry(dec).or_insert(0) += 1;
        out.insert("latency_ms".into(), jf(py_round(t0.elapsed().as_secs_f64() * 1000.0, 2)));
        out
    }

    /// Format hardening and the structuring defence, after the cascade has set `risk`.
    ///
    /// Structuring (research/structuring_defence.py): a sum split into several payments to the same payee scores lower
    /// per part, so the cascade risk is raised to the noisy-OR of the cascade risks of this pair's payments in the last
    /// 24 h, current one included. Hardening (research/hardened_eval.py): the final risk is
    /// max(iso_served(that cascade risk), iso_free(format-free score)) on the hardened band thresholds. Aggregating
    /// before calibration matters: the isotonic map sends small cascade risks to near zero, where a noisy-OR of
    /// calibrated parts barely moves. Both steps can only raise a decision.
    fn finish_risk(&mut self, p: &Payment, x: &[f32], out: &mut Map<String, Value>) {
        let cascade = out["risk"].as_f64().unwrap_or(0.0);
        let payer_window = self.models.as_ref().and_then(|m| m.hardening.as_ref()).map(|h| h.payer_window).unwrap_or(false);
        let payee = if payer_window { u32::MAX } else { self.acct_id(&p.to_bank, &p.to_account) };
        let key = (self.acct_id(&p.from_bank, &p.from_account), payee);
        let (n, pair) = self.pair_risks.push(key, p.tmin, cascade);
        let raised = if n > 1 { pair.max(cascade) } else { cascade };
        let (risk, before, free) = match self.models.as_ref().and_then(|m| m.hardening.as_ref()) {
            Some(h) => {
                let (r, f) = h.risk(x, raised);
                (r, interp(&h.iso_served, cascade).max(f), Some((f, interp(&h.iso_served, raised))))
            }
            None => (raised, cascade, None),
        };
        let (decision, band) = self.band_for_risk(risk);
        if let Some((f, _)) = free {
            out.insert("cascade_risk".into(), jf(py_round(cascade, 5)));
            out.insert("format_free_risk".into(), jf(py_round(f, 5)));
        }
        if n > 1 {
            out.insert("pair_risk".into(), jf(py_round(pair, 5)));
        }
        out.insert("risk".into(), jf(py_round(risk, 5)));
        out.insert("decision".into(), json!(decision));
        out.insert("band".into(), json!(band));
        // reasons from the model that set the risk: the format-free model's SHAP if it did, else the cascade's L2 XGBoost
        let reasons = match free {
            _ if band == "safe" => None,
            Some((f, s)) if f > s => Some(self.free_reasons(x, out_features(out))),
            _ => Some(self.cascade_reasons(x, out_features(out))),
        };
        if let Some(Value::Array(r)) = out.get_mut("reasons") {
            if let Some(fr) = reasons {
                *r = fr;
            }
            if self.band_for_risk(before).0 != decision && raised > cascade {
                let what = if payer_window { "Risky payments by this payer in 24 h" } else { "Repeated payments to this payee in 24 h" };
                r.insert(0, json!({"feature": what, "value": n,
                                   "display": format!("{n} payments"), "impact": py_round(risk - before, 3)}));
            }
        }
        if band == "high" && out["case_id"].is_null() {
            let cid = self.link(p);
            out.insert("case_id".into(), Value::String(cid));
        }
    }

    /// Top four positive SHAP contributions of the cascade's L2 XGBoost half.
    fn cascade_reasons(&self, x: &[f32], feats: Map<String, Value>) -> Vec<Value> {
        let Some(m) = self.models.as_ref() else { return vec![] };
        let contrib = m.l2x.contributions(x);
        let mut order: Vec<usize> = (0..x.len()).collect();
        order.sort_by(|a, b| contrib[*b].partial_cmp(&contrib[*a]).unwrap_or(std::cmp::Ordering::Equal));
        order.into_iter().take(4).filter(|&i| contrib[i] > 0.0).map(|i| {
            let k = &self.cfg.features[i];
            let v = feats.get(k).cloned().unwrap_or(Value::Null);
            json!({"feature": readable(k), "value": v, "display": self.display(k, &v), "impact": py_round(contrib[i] as f64, 3)})
        }).collect()
    }

    /// Top four positive SHAP contributions of the format-free model, in the same shape as the cascade's reasons.
    fn free_reasons(&self, x: &[f32], feats: Map<String, Value>) -> Vec<Value> {
        let Some(h) = self.models.as_ref().and_then(|m| m.hardening.as_ref()) else { return vec![] };
        let sub: Vec<f32> = h.cols.iter().map(|&i| x[i]).collect();
        let contrib = h.model.contributions(&sub);
        let mut order: Vec<usize> = (0..sub.len()).collect();
        order.sort_by(|a, b| contrib[*b].partial_cmp(&contrib[*a]).unwrap_or(std::cmp::Ordering::Equal));
        order.into_iter().take(4).filter(|&i| contrib[i] > 0.0).map(|i| {
            let k = &self.cfg.features[h.cols[i]];
            let v = feats.get(k).cloned().unwrap_or(Value::Null);
            json!({"feature": readable(k), "value": v, "display": self.display(k, &v), "impact": py_round(contrib[i] as f64, 3), "model": "format-free"})
        }).collect()
    }

    /// Human-readable value for a reason chip.
    pub fn display(&self, k: &str, v: &Value) -> String {
        let Some(x) = v.as_f64() else { return "none yet".into() };
        let rev = |codes: &IndexMap<String, i64>| -> String {
            let iv = x.trunc() as i64;
            codes.iter().rev().find(|(_, c)| **c == iv).map(|(n, _)| n.clone()).unwrap_or_else(|| py_str(v))
        };
        match k {
            "fmt_code" => rev(&self.cfg.fmt_codes),
            "cur_code" => rev(&self.cfg.cur_codes),
            "log_usd" | "s_out_mean_usd" | "s_usd_24h" | "d_in_usd_log" => format!("${}", fmt_comma(x.exp_m1(), 0)),
            "s_gap_out" | "d_gap_in" | "s_mins_since_in" | "s_age_out" => {
                if x < 120.0 {
                    format!("{:.0} min", x)
                } else if x < 2880.0 {
                    format!("{:.1} h", x / 60.0)
                } else {
                    format!("{:.1} days", x / 1440.0)
                }
            }
            "s_out_over_last_in" | "s_passthrough" => format!("{:.2}×", x),
            "new_pair" | "reciprocal" | "new_bank_pair" | "cross_bank" | "cross_cur" | "self_loop" | "round_100" => {
                if x != 0.0 { "yes".into() } else { "no".into() }
            }
            "hour" => format!("{:02}:00 UTC", x.trunc() as i64),
            _ => {
                if x.fract() == 0.0 {
                    group_thousands(&(x.trunc() as i64).to_string())
                } else {
                    format!("{:.2}", x)
                }
            }
        }
    }

    // ------------------------------------------------------------ L4 ring linker
    fn find(&mut self, a: &str) -> String {
        let mut a = a.to_string();
        loop {
            let p = self.parent.entry(a.clone()).or_insert_with(|| a.clone()).clone();
            if p == a {
                return a;
            }
            let gp = self.parent.get(&p).cloned().unwrap_or_else(|| p.clone());
            self.parent.insert(a.clone(), gp.clone());
            a = gp;
        }
    }

    fn link(&mut self, p: &Payment) -> String {
        let a = format!("{}_{}", p.from_bank, p.from_account);
        let b = format!("{}_{}", p.to_bank, p.to_account);
        let ra = self.find(&a);
        let rb = self.find(&b);
        if ra != rb {
            self.parent.insert(ra.clone(), rb.clone());
            if let Some(moved) = self.case_alerts.shift_remove(&ra) {
                self.case_alerts.entry(rb.clone()).or_default().extend(moved);
            }
        }
        let root = self.find(&b);
        self.case_alerts.entry(root.clone()).or_default().push(CaseEdge { from: a, to: b, amount: p.amount_paid, format: p.payment_format.clone() });
        case_id(&root)
    }

    pub fn cases(&self, limit: usize) -> Vec<Value> {
        let mut out: Vec<(usize, Value)> = self.case_alerts.iter().map(|(root, al)| {
            let accts: HashSet<&str> = al.iter().flat_map(|e| [e.from.as_str(), e.to.as_str()]).collect();
            let mut formats: Vec<&str> = al.iter().map(|e| e.format.as_str()).collect::<HashSet<_>>().into_iter().collect();
            formats.sort();
            (al.len(), json!({"case_id": case_id(root), "alerts": al.len(), "accounts": accts.len(),
                   "total_amount": py_round(py_sum(al.iter().map(|e| e.amount)), 2), "formats": formats,
                   "edges": al.iter().take(40).map(|e| json!({"from": e.from, "to": e.to, "amount": e.amount})).collect::<Vec<_>>()}))
        }).collect();
        out.sort_by_key(|x| std::cmp::Reverse(x.0));
        out.into_iter().take(limit).map(|x| x.1).collect()
    }

    // ------------------------------------------------------------ L0 onboarding
    /// Score an account-opening application (Feedzai BAF schema). Err when a value is not a number.
    pub fn score_application(&self, app: &Map<String, Value>) -> Result<Value> {
        let m = self.models.as_ref().expect("models not loaded");
        let c = &m.l0cfg;
        let mut x = Vec::with_capacity(c.features.len());
        for k in &c.features {
            let v = app.get(k).unwrap_or(&Value::Null);
            if c.categorical.contains(k) {
                let lst = &c.categories[k];
                x.push(match v.as_str().and_then(|s| lst.iter().position(|c| c == s)) {
                    Some(i) => i as f64,
                    None => f64::NAN,
                });
            } else if v.is_null() {
                x.push(f64::NAN);
            } else {
                x.push(crate::pyfmt::py_float_value(v).ok_or_else(|| anyhow::anyhow!("could not convert {k} to float: {v}"))?);
            }
        }
        let p = m.l0.predict(&x);
        let age = app.get("customer_age").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let thr = if age >= 50.0 { c.age_50_plus } else { c.age_under_50 };
        Ok(json!({"risk": py_round(p, 5), "threshold": py_round(thr, 5), "flag": p >= thr,
                  "decision": if p >= thr { "Enhanced KYC / video verification" } else { "Open account" }}))
    }
}

/// Python `str(v)` for an int/float JSON value.
fn py_str(v: &Value) -> String {
    match v {
        Value::Number(n) if n.is_f64() => crate::pyfmt::py_repr(n.as_f64().unwrap()),
        other => other.to_string(),
    }
}
