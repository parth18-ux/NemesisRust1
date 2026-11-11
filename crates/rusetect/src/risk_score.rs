//! One 0 to 1000 display score, derived from RuseTect's decision so the two can never disagree
//! (port of service/risk_score.py):
//!   0 to 299    approve silently
//!   300 to 599  verify: pause screen, then voice or face check
//!   600 to 1000 hold for an analyst (1000 = the payment model's block level, which still needs two analysts)
//! Inside each band the position shows how strong the evidence is. A ranking aid for people, not a probability.
use serde_json::{json, Value};

pub fn display_score(final_decision: &str, payment_risk: Option<f64>, total_points: i64, cfg: &Value) -> Value {
    let r = crate::engine::decision_rank(final_decision);
    let risk = payment_risk.unwrap_or(0.0);
    let get = |k: &str, d: f64| cfg.get(k).and_then(|v| v.as_f64()).unwrap_or(d);
    let (vt, ht, bt) = (get("verify_threshold", 0.1), get("hold_threshold", 0.375), get("block_threshold", 0.83));
    let tp = total_points as f64;
    let pts = (tp / 100.0).min(1.0);
    let s = match r {
        0 => 299.0 * (risk / vt).min(1.0).max((tp / 40.0).min(1.0)),
        1 => 300.0 + 299.0 * ((risk - vt) / (ht - vt).max(1e-9)).min(1.0).max(pts),
        2 => 600.0 + 349.0 * ((risk - ht) / (bt - ht).max(1e-9)).min(1.0).max(pts),
        _ => 950.0 + 50.0 * ((risk - bt) / (1.0 - bt).max(1e-9)).min(1.0),
    };
    let s = crate::pyfmt::py_round_int(s.min(1000.0).max(0.0));
    let band = if s < 300 { "approve" } else if s < 600 { "verify" } else { "analyst hold" };
    json!({"score": s, "band": band, "note": "display score derived from the decision; not a probability"})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn display_score_bands_match_decisions() {
        let cfg = json!({"verify_threshold": 0.1, "hold_threshold": 0.375, "block_threshold": 0.83});
        assert!(display_score("Approve", Some(0.02), 0, &cfg)["score"].as_i64().unwrap() < 300);
        let s = display_score("Step-up verification", Some(0.2), 45, &cfg)["score"].as_i64().unwrap();
        assert!((300..600).contains(&s));
        let s = display_score("Hold for analyst", Some(0.5), 70, &cfg)["score"].as_i64().unwrap();
        assert!((600..950).contains(&s));
        assert!(display_score("Block + STR draft", Some(0.95), 0, &cfg)["score"].as_i64().unwrap() >= 950);
    }
}
