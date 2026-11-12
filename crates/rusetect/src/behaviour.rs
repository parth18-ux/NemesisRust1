//! Typing rhythm check: is the person typing the account owner? (port of service/behaviour.py)
//!
//! Enrol from at least three sessions of key to key timings for pairs of consecutive letters (digraphs, mean ms per
//! pair); at login or payment, compare the new session with the profile. A flag marks typing_anomaly in the session
//! signals. Two scorers, same input:
//!   * the person-independent verifier in service/models/typing_verifier.json when present (trained and tested on
//!     the Aalto 136M Keystrokes data, see results/aalto_eval.json);
//!   * otherwise the original statistic: median z score over shared key pairs, threshold 2.5.
//! Both stay a supporting signal: the typing check alone never holds a payment.
use crate::models::LiteBooster;
use crate::pyfmt::{fmean, median, pstdev, py_round};
use indexmap::IndexMap;
use serde_json::{json, Map, Value};
use std::path::Path;

pub const MIN_SESSIONS: usize = 3;
pub const THRESHOLD: f64 = 2.5;
pub const MIN_PAIRS: usize = 5;

pub const VERIFIER_FEATURES: [&str; 13] = ["median_z", "mean_z_clipped", "frac_z_over_2", "frac_z_over_3", "log_shared_pairs", "frac_shared",
    "median_log_ratio", "abs_median_log_ratio", "mean_abs_log_ratio", "spread_log_ratio", "owner_variability", "attempt_speed", "owner_speed"];

/// (mean, population std, count) per key pair
pub type Stats = IndexMap<String, (f64, f64, usize)>;

/// Attempt-vs-profile features. NaNs when fewer than MIN_PAIRS pairs were seen in at least MIN_SESSIONS sessions.
/// Enrolment bounds per account (a typing sample has a few hundred key pairs; a profile needs a handful of sessions).
pub const MAX_SESSIONS: usize = 20;
pub const MAX_DIGRAPHS: usize = 1000;

pub fn verifier_features(stats: &Stats, attempt: &IndexMap<String, f64>) -> Vec<f64> {
    let shared: Vec<(f64, (f64, f64, usize))> = attempt.iter().filter_map(|(k, v)| {
        stats.get(k).filter(|s| s.2 >= MIN_SESSIONS && *v > 0.0).map(|s| (*v, *s))
    }).collect();
    if shared.len() < MIN_PAIRS || attempt.is_empty() {
        return vec![f64::NAN; VERIFIER_FEATURES.len()];
    }
    let mut z: Vec<f64> = shared.iter().map(|(v, (mu, sd, _))| (v - mu).abs() / sd.max(0.15 * mu).max(5.0)).collect();
    z.sort_by(|a, b| a.total_cmp(b));
    let mut r: Vec<f64> = shared.iter().filter(|(_, (mu, _, _))| *mu > 0.0).map(|(v, (mu, _, _))| (v / mu).ln()).collect();
    r.sort_by(|a, b| a.total_cmp(b));
    let med_r = median(&r);
    let n = z.len() as f64;
    vec![
        median(&z),
        fmean(&z.iter().map(|x| x.min(10.0)).collect::<Vec<_>>()),
        z.iter().filter(|x| **x > 2.0).count() as f64 / n,
        z.iter().filter(|x| **x > 3.0).count() as f64 / n,
        (shared.len() as f64).ln_1p(),
        shared.len() as f64 / attempt.len() as f64,
        med_r,
        med_r.abs(),
        fmean(&r.iter().map(|x| x.abs()).collect::<Vec<_>>()),
        median(&r.iter().map(|x| (x - med_r).abs()).collect::<Vec<_>>()),
        median(&shared.iter().filter(|(_, (mu, _, _))| *mu > 0.0).map(|(_, (mu, sd, _))| sd / mu).collect::<Vec<_>>()),
        median(&attempt.values().filter(|v| **v > 0.0).copied().collect::<Vec<_>>()).ln(),
        median(&shared.iter().map(|(_, (mu, _, _))| *mu).collect::<Vec<_>>()).ln(),
    ]
}

pub enum VerifierModel {
    Lgbm(LiteBooster),
    Logistic { mean: Vec<f64>, scale: Vec<f64>, coef: Vec<f64>, intercept: f64 },
}

pub struct Verifier {
    pub model: VerifierModel,
    pub threshold: f64,
    pub kind: String,
}

impl Verifier {
    pub fn load(path: &Path) -> Option<Verifier> {
        let m: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
        let feats: Vec<&str> = m["features"].as_array()?.iter().filter_map(|x| x.as_str()).collect();
        if feats != VERIFIER_FEATURES {
            return None;
        }
        let kind = m["kind"].as_str()?.to_string();
        let fv = |k: &str| -> Vec<f64> { m[k].as_array().map(|a| a.iter().filter_map(|x| x.as_f64()).collect()).unwrap_or_default() };
        let model = if kind == "lightgbm" {
            VerifierModel::Lgbm(LiteBooster::from_json(&m["booster"]).ok()?)
        } else {
            VerifierModel::Logistic { mean: fv("mean"), scale: fv("scale"), coef: fv("coef"), intercept: m["intercept"].as_f64()? }
        };
        Some(Verifier { model, threshold: m["threshold"].as_f64()?, kind })
    }

    pub fn predict(&self, f: &[f64]) -> f64 {
        match &self.model {
            VerifierModel::Lgbm(b) => b.predict(f),
            VerifierModel::Logistic { mean, scale, coef, intercept } => {
                let z = intercept + crate::pyfmt::py_sum(coef.iter().zip(f).zip(mean).zip(scale).map(|(((c, x), mu), s)| c * (x - mu) / s));
                1.0 / (1.0 + (-z).exp())
            }
        }
    }
}

#[derive(Default, Clone, Debug)]
pub struct TypingProfile {
    pub sessions: Vec<IndexMap<String, f64>>,
}

impl TypingProfile {
    /// Keeps the latest MAX_SESSIONS sessions of at most MAX_DIGRAPHS pairs each, so enrolment memory is bounded.
    pub fn enrol(&mut self, mut digraphs: IndexMap<String, f64>) {
        digraphs.truncate(MAX_DIGRAPHS);
        self.sessions.push(digraphs);
        if self.sessions.len() > MAX_SESSIONS {
            self.sessions.remove(0);
        }
    }

    pub fn ready(&self) -> bool {
        self.sessions.len() >= MIN_SESSIONS
    }

    pub fn stats(&self) -> Stats {
        let mut vals: IndexMap<String, Vec<f64>> = IndexMap::new();
        for s in &self.sessions {
            for (k, v) in s {
                vals.entry(k.clone()).or_default().push(*v);
            }
        }
        vals.into_iter().map(|(k, v)| (k, (fmean(&v), pstdev(&v), v.len()))).collect()
    }

    /// Original statistic: median z over shared pairs (kept for the explanation and as the fallback).
    pub fn score(&self, digraphs: &IndexMap<String, f64>) -> Map<String, Value> {
        let mut out = Map::new();
        if !self.ready() {
            out.insert("ready".into(), json!(false));
            out.insert("score".into(), Value::Null);
            out.insert("anomaly".into(), json!(false));
            return out;
        }
        let mut zs = Vec::new();
        for (k, v) in digraphs {
            let vals: Vec<f64> = self.sessions.iter().filter_map(|s| s.get(k).copied()).collect();
            if vals.len() >= MIN_SESSIONS {
                let mu = fmean(&vals);
                let sd = pstdev(&vals).max(0.15 * mu).max(5.0);
                zs.push((v - mu).abs() / sd);
            }
        }
        out.insert("ready".into(), json!(true));
        if zs.len() < MIN_PAIRS {
            out.insert("score".into(), Value::Null);
            out.insert("anomaly".into(), json!(false));
            out.insert("note".into(), json!("too few shared key pairs"));
            return out;
        }
        let z = median(&zs);
        out.insert("score".into(), json!(py_round(z, 2)));
        out.insert("anomaly".into(), json!(z > THRESHOLD));
        out.insert("pairs".into(), json!(zs.len()));
        out
    }

    /// What the API uses: the trained verifier when installed, else the median z statistic.
    pub fn check(&self, digraphs: &IndexMap<String, f64>, verifier: Option<&Verifier>) -> Value {
        let mut base = self.score(digraphs);
        let ready = base["ready"].as_bool().unwrap_or(false);
        let Some(ver) = verifier.filter(|_| ready) else {
            base.insert("method".into(), json!("median_z"));
            return Value::Object(base);
        };
        let f = verifier_features(&self.stats(), digraphs);
        if f.iter().any(|x| x.is_nan()) {
            base.insert("method".into(), json!("verifier"));
            base.insert("score".into(), Value::Null);
            base.insert("anomaly".into(), json!(false));
            base.insert("note".into(), json!("too few shared key pairs"));
            return Value::Object(base);
        }
        let p = ver.predict(&f);
        json!({"ready": true, "method": "verifier", "score": py_round(p, 3), "anomaly": p > ver.threshold,
               "threshold": py_round(ver.threshold, 3), "median_z": base.get("score").cloned().unwrap_or(Value::Null),
               "pairs": base.get("pairs").cloned().unwrap_or(Value::Null)})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{Rng, SeedableRng};

    fn digraphs(keys: &[&str], f: impl Fn(usize) -> f64) -> IndexMap<String, f64> {
        keys.iter().enumerate().map(|(i, k)| (k.to_string(), f(i))).collect()
    }

    #[test]
    fn typing_profile_flags_a_different_typist() {
        let mut rnd = rand::rngs::StdRng::seed_from_u64(0);
        let keys = ["th", "he", "in", "er", "an", "re", "on", "at"];
        let mut prof = TypingProfile::default();
        for _ in 0..4 {
            let r: Vec<f64> = (0..keys.len()).map(|_| rnd.gen_range(0.95..1.05)).collect();
            prof.enrol(digraphs(&keys, |i| (90.0 + 15.0 * i as f64) * r[i]));
        }
        assert_eq!(prof.score(&digraphs(&keys, |i| (90.0 + 15.0 * i as f64) * 1.02))["anomaly"], false);
        assert_eq!(prof.score(&digraphs(&keys, |i| (90.0 + 15.0 * i as f64) * 2.2))["anomaly"], true);
    }

    #[test]
    fn verifier_is_installed_and_separates_typists() {
        let v = Verifier::load(&crate::paths::models_dir().join("typing_verifier.json")).expect("typing verifier");
        assert_eq!(v.kind, "lightgbm");
        let mut rnd = rand::rngs::StdRng::seed_from_u64(1);
        let keys = ["th", "he", "in", "er", "an", "re", "on", "at", "en", "nd"];
        let base = |i: usize| 110.0 + 12.0 * i as f64;
        let mut prof = TypingProfile::default();
        for _ in 0..5 {
            let r: Vec<f64> = (0..keys.len()).map(|_| rnd.gen_range(0.9..1.1)).collect();
            prof.enrol(digraphs(&keys, |i| base(i) * r[i]));
        }
        let same = prof.check(&digraphs(&keys, |i| base(i) * 1.03), Some(&v));
        let other = prof.check(&digraphs(&keys, |i| base(i) * 2.0), Some(&v));
        assert_eq!(same["method"], "verifier");
        assert_eq!(same["anomaly"], false);
        assert_eq!(other["anomaly"], true);
        assert!(other["score"].as_f64() > same["score"].as_f64());
    }
}
