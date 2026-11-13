//! Joins the message side (L-1 lure detector) to the money side (L0-L4 cascade), and decides who actually gets
//! alerted (port of service/lure_fusion.py).
//!
//!   * The victim gets one warning, at the moment it matters: when a payment to a NEW payee follows a risky message
//!     within 72 hours (cool-off + step-up verification), or when a payment goes to a UPI ID / account / phone that
//!     appeared in a risky message (hold).
//!   * Alert-fatigue controls: a push only for the high band, at most one per (account, workflow) per 24h and at most
//!     3 per account per day; caution-band messages are never pushed. Every suppressed alert is counted with its reason.
use crate::payment::Payment;
use crate::pyfmt::{jf, py_round, py_strip};
use indexmap::IndexMap;
use serde_json::{json, Map, Value};
use std::collections::VecDeque;

pub const LOOKBACK_MIN: i64 = 72 * 60;
pub const DEDUP_MIN: i64 = 24 * 60;
pub const DAILY_CAP: usize = 3;
/// Payee watchlist entries kept; the oldest (least recently named) is dropped first.
// shortcut: in-memory and capped, upgrade to a real store (TTL'd key-value) when one process serves many banks' traffic
pub const MAX_WATCH: usize = if cfg!(test) { 3 } else { 100_000 };
/// Accounts kept in the per-account lure and push maps; the oldest-added account is dropped first.
pub const MAX_ACCOUNTS: usize = if cfg!(test) { 3 } else { 100_000 };

const RANK: [&str; 4] = ["Approve", "Step-up verification", "Hold for analyst", "Block + STR draft"];
const BAND: [&str; 4] = ["safe", "med", "med", "high"];

fn rank(d: &str) -> usize {
    RANK.iter().position(|x| *x == d).unwrap_or(0)
}

fn norm(x: &str) -> String {
    py_strip(x).to_lowercase()
}

#[derive(Debug, Clone)]
struct Lure {
    t: i64,
    wlab: String,
    risk: f64,
}

#[derive(Default)]
pub struct LureMemory {
    events: IndexMap<String, VecDeque<Lure>>,
    watch: IndexMap<String, (i64, String, f64)>,
    pushed: IndexMap<String, VecDeque<(i64, String)>>,
    stats: IndexMap<String, i64>,
}

impl LureMemory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    fn stat(&mut self, k: &str) {
        *self.stats.entry(k.to_string()).or_insert(0) += 1;
    }

    /// Remember a scored message; return the notification decision.
    pub fn record_message(&mut self, account: Option<&str>, tmin: i64, scored: &Value) -> Value {
        let band = scored["band"].as_str().unwrap_or("safe").to_string();
        let wf = &scored["workflow"];
        let wid = wf.get("id").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();
        let wlab = wf.get("label").and_then(|v| v.as_str()).unwrap_or("Scam pattern").to_string();
        let risk = scored["risk"].as_f64().unwrap_or(0.0);
        self.stat("messages");
        self.stat(&format!("messages_{band}"));
        if band == "safe" {
            return json!({"notify": false, "reason": "Low risk"});
        }
        let ents = &scored["entities"];
        for k in ["upi_ids", "phones", "accounts"] {
            for e in ents[k].as_array().into_iter().flatten() {
                if let Some(s) = e.as_str() {
                    let k = norm(s);
                    self.watch.shift_remove(&k); // re-insert at the back so the order stays oldest-first
                    if self.watch.len() >= MAX_WATCH {
                        self.watch.shift_remove_index(0);
                    }
                    self.watch.insert(k, (tmin, wlab.clone(), risk));
                }
            }
        }
        let Some(account) = account.filter(|a| !a.is_empty()) else {
            self.stat("suppressed_no_account");
            return json!({"notify": false, "reason": "No account given; message kept only for its entities (payee watchlist)"});
        };
        let acct = norm(account);
        if !self.events.contains_key(&acct) && self.events.len() >= MAX_ACCOUNTS {
            self.events.shift_remove_index(0);
        }
        let ev = self.events.entry(acct.clone()).or_default();
        ev.push_back(Lure { t: tmin, wlab: wlab.clone(), risk });
        while ev.front().map(|e| e.t < tmin - LOOKBACK_MIN).unwrap_or(false) {
            ev.pop_front();
        }
        if band != "high" {
            self.stat("suppressed_caution");
            return json!({"notify": false, "reason": "Caution band: shown only if a payment follows (no push)"});
        }
        if !self.pushed.contains_key(&acct) && self.pushed.len() >= MAX_ACCOUNTS {
            self.pushed.shift_remove_index(0);
        }
        let p = self.pushed.entry(acct).or_default();
        while p.front().map(|e| e.0 < tmin - DEDUP_MIN).unwrap_or(false) {
            p.pop_front();
        }
        if p.iter().any(|(_, w)| *w == wid) {
            self.stat("suppressed_duplicate");
            return json!({"notify": false, "reason": format!("Already warned about '{wlab}' in the last 24h")});
        }
        if p.len() >= DAILY_CAP {
            self.stat("suppressed_daily_cap");
            return json!({"notify": false, "reason": format!("Daily cap of {DAILY_CAP} warnings reached; queued for in-app inbox")});
        }
        p.push_back((tmin, wid));
        self.stat("notified");
        json!({"notify": true, "reason": "High-risk scam message",
               "message": format!("This matches a known scam pattern ({wlab}). Do not pay, share an OTP or install any app. Banks and police never ask for this.")})
    }

    /// Escalate a cascade result using recent lures. Never lowers a decision.
    pub fn apply_to_payment(&mut self, p: &Payment, res: &mut Map<String, Value>) {
        let tmin = p.tmin;
        let payer = norm(&format!("{}:{}", p.from_bank, p.from_account));
        // the Python set iterated in hash order; checking the full key first makes the choice deterministic
        let payee_keys = [norm(&format!("{}:{}", p.to_bank, p.to_account)), norm(&p.to_account)];
        let new_payee = res.get("features").and_then(|f| f.get("new_pair")).map(|v| v.as_f64().map(|x| x != 0.0).unwrap_or(v.as_bool().unwrap_or(false))).unwrap_or(true);
        let decision = res["decision"].as_str().unwrap_or("Approve").to_string();
        let mut target = rank(&decision);
        let mut reasons: Vec<Value> = Vec::new();
        let recent: Vec<Lure> = self.events.get(&payer).map(|q| q.iter().filter(|e| (0..=LOOKBACK_MIN).contains(&(tmin - e.t))).cloned().collect()).unwrap_or_default();
        if !recent.is_empty() && new_payee {
            let mut best = &recent[0];
            for e in &recent[1..] {
                if e.risk > best.risk {
                    best = e;
                }
            }
            let hrs = (tmin - best.t) as f64 / 60.0;
            target = target.max(1);
            reasons.push(json!({"feature": "Payer received a scam-like message", "display": format!("{}, {:.0} h ago", best.wlab, hrs),
                                "impact": py_round(best.risk, 3), "value": null}));
        }
        let hit = payee_keys.iter().filter_map(|k| self.watch.get(k)).find(|w| (0..=LOOKBACK_MIN).contains(&(tmin - w.0))).cloned();
        if let Some((_, lab, risk)) = &hit {
            target = target.max(2);
            reasons.push(json!({"feature": "Payee named in a scam message", "display": lab, "impact": py_round(*risk, 3), "value": null}));
        }
        if reasons.is_empty() {
            res.insert("lure_context".into(), Value::Null);
            return;
        }
        if target > rank(&decision) {
            self.stat("payments_escalated");
            res.insert("decision_before_lure".into(), json!(decision));
            res.insert("decision".into(), json!(RANK[target]));
            res.insert("band".into(), json!(BAND[target]));
            if target == 1 {
                res.insert("payer_prompt".into(), json!("Pause: you got a message that matches a known scam a short while ago. Is someone asking you to make this payment?"));
            }
        }
        let old = res.get("reasons").and_then(|r| r.as_array()).cloned().unwrap_or_default();
        reasons.extend(old);
        res.insert("reasons".into(), Value::Array(reasons));
        res.insert("lure_context".into(), json!({"recent_lures": recent.len(), "payee_on_watchlist": hit.is_some()}));
    }

    pub fn summary(&self) -> Value {
        let mut s: Map<String, Value> = self.stats.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
        let m = (*self.stats.get("messages").unwrap_or(&0)).max(1) as f64;
        let notified = *self.stats.get("notified").unwrap_or(&0) as f64;
        s.insert("push_rate_per_100_messages".into(), jf(py_round(100.0 * notified / m, 2)));
        s.insert("watchlist_entities".into(), json!(self.watch.len()));
        Value::Object(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn watchlist_drops_oldest_when_full() {
        let mut m = LureMemory::new();
        for (i, e) in ["u1@ybl", "u2@ybl", "u3@ybl", "u1@ybl", "u4@ybl"].iter().enumerate() {
            m.record_message(None, i as i64, &json!({"band": "high", "risk": 0.9, "workflow": {}, "entities": {"upi_ids": [e]}}));
        }
        assert_eq!(m.watch.keys().cloned().collect::<Vec<_>>(), vec!["u3@ybl", "u1@ybl", "u4@ybl"]);
    }

    #[test]
    fn account_maps_stay_capped() {
        let mut m = LureMemory::new();
        for i in 0..10 {
            m.record_message(Some(&format!("acct{i}")), i, &json!({"band": "high", "risk": 0.9, "workflow": {}, "entities": {}}));
        }
        assert_eq!(m.events.len(), MAX_ACCOUNTS);
        assert_eq!(m.pushed.len(), MAX_ACCOUNTS);
        assert!(m.events.contains_key("acct9") && !m.events.contains_key("acct0"));
    }
}
