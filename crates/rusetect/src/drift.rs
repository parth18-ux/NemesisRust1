//! Drift monitor: is live traffic still scored like the validation period the thresholds were fitted on?
//!
//! A bounded window of the last N scored payments (default [`WINDOW`]) is compared with the reference profile written by
//! research/drift_reference.py from HI-Small validation (service/models/drift_reference.json):
//!   score     PSI of the served risk over the reference's decile bins
//!   features  PSI of a few payment inputs (amount, format, currency, cross bank, round amount); a missing value
//!             (null) has its own bin
//!   bands     share of payments in each served band (verify, hold, block) over the reference share
//! PSI = sum (a - e) ln(a / e) over bins, shares floored at [`EPS`]. Status, the worst of all checks:
//!   PSI >= [`PSI_WARN`] (0.1) warn, >= [`PSI_ALERT`] (0.25) alert (the usual credit-scoring rule of thumb);
//!   band ratio >= [`RATIO_WARN`] (2) or <= 1/2 warn, >= [`RATIO_ALERT`] (3) or <= 1/3 alert, checked only once the
//!   window expects at least [`MIN_EXPECTED`] payments in that band.
//! Below [`MIN_N`] payments the window is warming up and the status stays ok. Counts are updated on push and evict, so
//! an observation is O(bins). The window is this process's memory only.
use serde_json::{json, Map, Value};
use std::collections::VecDeque;

/// Default window, about a day of HI-Small validation traffic: the smallest of 10,000, 50,000 and 250,000 with no alert
/// on validation itself (shorter windows see the time-of-day mix; rule and replay: research/drift_replay.py,
/// results/drift_validation.json). RUSETECT_DRIFT_WINDOW sets another (see [`window_from_env`]).
pub const WINDOW: usize = 250_000;
pub const MAX_WINDOW: usize = 1_000_000;
/// Most tracked features (an entry keeps their bins inline).
pub const MAX_FEATS: usize = 8;
pub const MIN_N: usize = 1_000;
pub const PSI_WARN: f64 = 0.1;
pub const PSI_ALERT: f64 = 0.25;
pub const RATIO_WARN: f64 = 2.0;
pub const RATIO_ALERT: f64 = 3.0;
pub const MIN_EXPECTED: f64 = 10.0;
pub const EPS: f64 = 1e-4;
pub const BANDS: [&str; 3] = ["verify", "hold", "block"];
const STATUS: [&str; 3] = ["ok", "warn", "alert"];
const NONE: u16 = u16::MAX;

/// Bins of one tracked quantity: interior edges, expected shares (the last one for missing values) and live counts.
struct Hist {
    name: String,
    edges: Vec<f64>,
    expected: Vec<f64>,
    counts: Vec<u32>,
    n: u32,
}

impl Hist {
    fn from_ref(name: &str, v: &Value) -> Option<Hist> {
        let nums = |k: &str| v[k].as_array().map(|a| a.iter().filter_map(Value::as_f64).collect::<Vec<f64>>());
        let edges = nums("edges")?;
        let mut expected = nums("shares")?;
        if expected.len() != edges.len() + 1 {
            return None;
        }
        expected.push(v["missing"].as_f64().unwrap_or(0.0));
        Some(Hist { name: name.into(), counts: vec![0; expected.len()], edges, expected, n: 0 })
    }

    /// bin = number of edges <= x (numpy searchsorted side='right'); NaN goes to the missing bin
    fn bin(&self, x: f64) -> u16 {
        if x.is_nan() { self.edges.len() as u16 + 1 } else { self.edges.partition_point(|e| *e <= x) as u16 }
    }

    fn add(&mut self, b: u16, d: i64) {
        if b != NONE {
            self.counts[b as usize] = (self.counts[b as usize] as i64 + d) as u32;
            self.n = (self.n as i64 + d) as u32;
        }
    }

    fn psi(&self) -> f64 {
        let n = self.n.max(1) as f64;
        self.expected.iter().zip(&self.counts).map(|(e, c)| {
            let (a, e) = ((*c as f64 / n).max(EPS), e.max(EPS));
            (a - e) * (a / e).ln()
        }).sum()
    }
}

struct Entry {
    score: u16,
    band: u16,
    feats: [u16; MAX_FEATS],
}

/// RUSETECT_DRIFT_WINDOW if it is a number, kept within [`MIN_N`]..=[`MAX_WINDOW`]; else [`WINDOW`].
pub fn window_from_env() -> usize {
    std::env::var("RUSETECT_DRIFT_WINDOW").ok().and_then(|v| v.trim().parse::<usize>().ok()).map(|n| n.clamp(MIN_N, MAX_WINDOW)).unwrap_or(WINDOW)
}

pub struct DriftMonitor {
    cap: usize,
    score: Hist,
    feats: Vec<Hist>,
    band_ref: [f64; 3],
    band_n: [u32; 3],
    window: VecDeque<Entry>,
    pub source: String,
}

fn level(psi: f64) -> usize {
    if psi >= PSI_ALERT { 2 } else if psi >= PSI_WARN { 1 } else { 0 }
}

fn ratio_level(r: f64) -> usize {
    if r >= RATIO_ALERT || r <= 1.0 / RATIO_ALERT { 2 } else if r >= RATIO_WARN || r <= 1.0 / RATIO_WARN { 1 } else { 0 }
}

fn r4(x: f64) -> f64 {
    (x * 1e4).round() / 1e4
}

impl DriftMonitor {
    /// From the reference JSON; None if it is malformed.
    pub fn from_reference(r: &Value, cap: usize) -> Option<Self> {
        let feats = r["features"].as_object()?.iter().map(|(k, v)| Hist::from_ref(k, v)).collect::<Option<Vec<_>>>()?;
        if feats.len() > MAX_FEATS {
            return None;
        }
        let b = |k: &str| r["bands"][k].as_f64();
        Some(DriftMonitor { cap: cap.max(1), score: Hist::from_ref("score", &r["score"])?, feats, band_ref: [b("verify")?, b("hold")?, b("block")?],
                            band_n: [0; 3], window: VecDeque::new(), source: r["split"].as_str().unwrap_or("").into() })
    }

    /// Read `path` (service/models/drift_reference.json); None if absent or malformed.
    pub fn load(path: &std::path::Path) -> Option<Self> {
        let v: Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
        Self::from_reference(&v, window_from_env())
    }

    pub fn capacity(&self) -> usize {
        self.cap
    }

    pub fn len(&self) -> usize {
        self.window.len()
    }

    pub fn is_empty(&self) -> bool {
        self.window.is_empty()
    }

    pub fn reset(&mut self) {
        while self.evict() {}
    }

    fn evict(&mut self) -> bool {
        let Some(e) = self.window.pop_front() else { return false };
        self.score.add(e.score, -1);
        if e.band != NONE {
            self.band_n[e.band as usize] -= 1;
        }
        for (h, b) in self.feats.iter_mut().zip(&e.feats) {
            h.add(*b, -1);
        }
        true
    }

    /// One scored payment: its served risk (None if not reported), its payment decision and, when known, its features
    /// as the engine reports them (`{name: value or null}`).
    pub fn observe(&mut self, risk: Option<f64>, decision: &str, features: Option<&Map<String, Value>>) {
        if self.window.len() >= self.cap {
            self.evict();
        }
        let score = risk.map(|r| self.score.bin(r)).unwrap_or(NONE);
        let band = match decision {
            "Step-up verification" => 0,
            "Hold for analyst" => 1,
            "Block + STR draft" => 2,
            _ => NONE,
        };
        let mut feats = [NONE; MAX_FEATS];
        if let Some(f) = features {
            for (b, h) in feats.iter_mut().zip(&self.feats) {
                *b = h.bin(f.get(&h.name).and_then(Value::as_f64).unwrap_or(f64::NAN));
            }
        }
        self.score.add(score, 1);
        if band != NONE {
            self.band_n[band as usize] += 1;
        }
        for (h, b) in self.feats.iter_mut().zip(&feats) {
            h.add(*b, 1);
        }
        self.window.push_back(Entry { score, band, feats });
    }

    /// One observation from an engine result object (`risk`, `decision`, `features`).
    pub fn observe_result(&mut self, res: &Map<String, Value>) {
        self.observe(res.get("risk").and_then(Value::as_f64), res.get("decision").and_then(Value::as_str).unwrap_or(""),
                     res.get("features").and_then(Value::as_object));
    }

    /// ok / warn / alert
    pub fn status(&self) -> &'static str {
        match self.report()["status"].as_str() {
            Some("alert") => "alert",
            Some("warn") => "warn",
            _ => "ok",
        }
    }

    /// The /api/v2/drift body.
    pub fn report(&self) -> Value {
        let n = self.window.len();
        let warming = n < MIN_N;
        let mut worst = 0usize;
        let mut reasons: Vec<String> = Vec::new();
        let mut hist = |h: &Hist, reasons: &mut Vec<String>| {
            if (h.n as usize) < MIN_N {
                return json!({"n": h.n, "psi": Value::Null, "status": "ok"});
            }
            let psi = h.psi();
            let l = level(psi);
            if l > 0 && !warming {
                reasons.push(format!("{} PSI {:.3} >= {}", h.name, psi, if l == 2 { PSI_ALERT } else { PSI_WARN }));
                worst = worst.max(l);
            }
            json!({"n": h.n, "psi": r4(psi), "status": STATUS[l]})
        };
        let score = hist(&self.score, &mut reasons);
        let feats: Map<String, Value> = self.feats.iter().map(|h| (h.name.clone(), hist(h, &mut reasons))).collect();
        let mut bands = Map::new();
        for (i, b) in BANDS.iter().enumerate() {
            let (rate, re) = (self.band_n[i] as f64 / n.max(1) as f64, self.band_ref[i]);
            let checked = !warming && re > 0.0 && n as f64 * re >= MIN_EXPECTED;
            let ratio = if re > 0.0 { rate / re } else { f64::NAN };
            let l = if checked { ratio_level(ratio) } else { 0 };
            if l > 0 {
                reasons.push(format!("{b} rate {:.2}x the reference", ratio));
                worst = worst.max(l);
            }
            bands.insert(b.to_string(), json!({"rate": r4(rate), "reference": re, "ratio": if ratio.is_finite() { json!(r4(ratio)) } else { Value::Null },
                                                "checked": checked, "status": STATUS[l]}));
        }
        let top = std::iter::once(("score", &score)).chain(feats.iter().map(|(k, v)| (k.as_str(), v)))
            .filter_map(|(k, v)| v["psi"].as_f64().map(|p| (k, p))).max_by(|a, b| a.1.total_cmp(&b.1));
        json!({"status": STATUS[worst], "warming_up": warming, "reasons": reasons,
               "window": {"size": n, "capacity": self.cap, "min_size": MIN_N},
               "score_psi": score["psi"], "score": score, "features": feats, "bands": bands,
               "top_psi": top.map(|(k, p)| json!({"name": k, "psi": p})).unwrap_or(Value::Null),
               "thresholds": {"psi_warn": PSI_WARN, "psi_alert": PSI_ALERT, "rate_ratio_warn": RATIO_WARN, "rate_ratio_alert": RATIO_ALERT,
                              "min_expected_band_count": MIN_EXPECTED},
               "reference": self.source})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference() -> Value {
        json!({"split": "test", "bands": {"verify": 0.1, "hold": 0.05, "block": 0.02},
               "score": {"edges": [0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9], "shares": vec![0.1; 10]},
               "features": {"x": {"edges": [1.0], "shares": [0.5, 0.4], "missing": 0.1}}})
    }

    fn feed(m: &mut DriftMonitor, n: usize, shift: f64) {
        for i in 0..n {
            let r = ((i * 7919) % 1000) as f64 / 1000.0 * (1.0 - shift) + shift;
            let d = if r >= 0.98 { "Block + STR draft" } else if r >= 0.93 { "Hold for analyst" } else if r >= 0.83 { "Step-up verification" } else { "Approve" };
            let x = match i % 10 { 0 => Value::Null, 1..=5 => json!(0.5), _ => json!(2.0) };
            let f: Map<String, Value> = [("x".to_string(), x)].into_iter().collect();
            m.observe(Some(r), d, Some(&f));
        }
    }

    #[test]
    fn matching_traffic_is_ok_and_shifted_traffic_alerts() {
        let mut m = DriftMonitor::from_reference(&reference(), 5_000).unwrap();
        feed(&mut m, 5_000, 0.0);
        let r = m.report();
        assert_eq!(r["status"], "ok", "{r}");
        assert!(r["score_psi"].as_f64().unwrap() < 0.01);
        feed(&mut m, 5_000, 0.8);
        let r = m.report();
        assert_eq!(r["status"], "alert", "{r}");
        assert!(r["score_psi"].as_f64().unwrap() >= PSI_ALERT && r["bands"]["block"]["ratio"].as_f64().unwrap() >= RATIO_ALERT);
        assert_eq!(r["top_psi"]["name"], "score");
    }

    #[test]
    fn window_is_bounded_and_counts_follow_evictions() {
        let mut m = DriftMonitor::from_reference(&reference(), 1_000).unwrap();
        feed(&mut m, 3_500, 0.0);
        assert_eq!(m.len(), 1_000);
        assert_eq!(m.score.n, 1_000);
        assert_eq!(m.score.counts.iter().sum::<u32>(), 1_000);
        assert_eq!(m.feats[0].counts.iter().sum::<u32>(), 1_000);
        m.reset();
        assert!(m.is_empty() && m.score.n == 0 && m.band_n == [0; 3]);
        assert_eq!(m.report()["warming_up"], true);
    }

    /// Traffic laid out exactly as the committed reference (values on the left edge of each bin) is ok: the file loads
    /// and the monitor bins values the way research/drift_reference.py does.
    #[test]
    fn traffic_shaped_like_the_served_reference_is_ok() {
        let r: Value = serde_json::from_slice(&std::fs::read(crate::paths::models_dir().join("drift_reference.json")).unwrap()).unwrap();
        let mut m = DriftMonitor::from_reference(&r, WINDOW).unwrap();
        let at = |h: &Value, i: usize| -> Value {
            let e = h["edges"].as_array().unwrap();
            if i == 0 { json!(e[0].as_f64().unwrap() - 1e-9) } else if i <= e.len() { e[i - 1].clone() } else { Value::Null }
        };
        let draw = |h: &Value, k: usize| -> Value {
            let mut shares: Vec<f64> = h["shares"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
            shares.push(h["missing"].as_f64().unwrap_or(0.0));
            let (mut acc, u) = (0.0, (k as f64 + 0.5) / WINDOW as f64);
            let i = shares.iter().position(|p| { acc += p; u < acc }).unwrap_or(shares.len() - 1);
            at(h, i)
        };
        let b = &r["bands"];
        let (v, h, k) = (b["verify"].as_f64().unwrap(), b["hold"].as_f64().unwrap(), b["block"].as_f64().unwrap());
        for j in 0..WINDOW {
            // a fixed stride spreads each quantity over the window independently of the others
            let f: Map<String, Value> = r["features"].as_object().unwrap().iter().enumerate()
                .map(|(n, (name, hist))| (name.clone(), draw(hist, (j * (2 * n + 3) + n) % WINDOW))).collect();
            let u = (((j * 7) % WINDOW) as f64 + 0.5) / WINDOW as f64;
            let d = if u < k { "Block + STR draft" } else if u < k + h { "Hold for analyst" } else if u < k + h + v { "Step-up verification" } else { "Approve" };
            m.observe(draw(&r["score"], j).as_f64(), d, Some(&f));
        }
        let rep = m.report();
        assert_eq!(rep["status"], "ok", "{rep}");
        assert!(rep["score_psi"].as_f64().unwrap() < 0.01, "{rep}");
    }

    #[test]
    fn small_windows_stay_ok_while_warming_up() {
        let mut m = DriftMonitor::from_reference(&reference(), WINDOW).unwrap();
        feed(&mut m, MIN_N - 1, 0.9);
        assert_eq!(m.status(), "ok");
        feed(&mut m, 1, 0.9);
        assert_eq!(m.status(), "alert");
    }
}
