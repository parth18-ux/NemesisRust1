//! Analyst review, audit trail and evidence packs (port of service/review.py).
//!
//! * Every assessed payment that needs a human (final decision above Approve) becomes an ALERT with an id.
//! * Analysts act on alerts: release, verify, hold, or approve_block (two different analysts must approve a block).
//!   The model never blocks on its own: "Block + STR draft" is shown as "Hold + recommend block" until then.
//! * Every event is appended to a hash-chained audit log, so an edit to history is detectable.
//! * An evidence pack bundles what the model saw and what people did, with its own content hash.
use crate::pyfmt::{dumps, utc_now_z, HASH_STYLE};
use indexmap::IndexMap;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

pub const ACTIONS: [&str; 4] = ["approve_block", "hold", "release", "verify"]; // sorted, as Python prints them

/// sha256 of `json.dumps(obj, sort_keys=True, default=str)`
pub fn hash_obj(obj: &Value) -> String {
    hex::encode(Sha256::digest(dumps(obj, HASH_STYLE).as_bytes()))
}

#[derive(Debug)]
pub enum ReviewError {
    NotFound,
    Invalid(String),
}

#[derive(Default)]
pub struct ReviewDesk {
    pub alerts: IndexMap<String, Value>,
    pub log: Vec<Value>,
    next_id: u64,
}

impl ReviewDesk {
    pub fn new() -> Self {
        ReviewDesk { next_id: 1, ..Default::default() }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    pub fn append(&mut self, event: Map<String, Value>) -> Value {
        let prev = self.log.last().and_then(|e| e["hash"].as_str()).map(String::from).unwrap_or_else(|| "0".repeat(64));
        let mut e = Map::new();
        e.insert("seq".into(), json!(self.log.len() + 1));
        e.insert("at".into(), json!(utc_now_z()));
        for (k, v) in event {
            e.insert(k, v);
        }
        e.insert("prev".into(), json!(prev));
        let h = hash_obj(&Value::Object(e.clone()));
        e.insert("hash".into(), json!(h));
        let v = Value::Object(e);
        self.log.push(v.clone());
        v
    }

    /// Open an alert for an assessed payment (None when the final decision is Approve).
    pub fn open_alert(&mut self, payment_in: &Map<String, Value>, assessed: &Value) -> Option<Value> {
        let fd = assessed["final_decision"].as_str().unwrap_or("Approve");
        if fd == "Approve" {
            return None;
        }
        let aid = format!("A{:05}", self.next_id);
        self.next_id += 1;
        let shown = if fd == "Block + STR draft" { "Hold + recommend block" } else { fd };
        let payment: Map<String, Value> = ["timestamp", "from_bank", "from_account", "to_bank", "to_account", "amount_paid", "payment_currency", "payment_format"]
            .iter().map(|k| (k.to_string(), payment_in.get(*k).cloned().unwrap_or(Value::Null))).collect();
        let inputs_hash = hash_obj(&Value::Object(payment.clone()));
        let a = json!({"alert_id": aid, "status": "open", "shown_decision": shown, "payment": payment, "assessed": assessed,
                       "actions": [], "block_approvals": []});
        self.alerts.insert(aid.clone(), a.clone());
        let mut ev = Map::new();
        ev.insert("type".into(), json!("alert_opened"));
        ev.insert("alert_id".into(), json!(aid));
        ev.insert("decision".into(), json!(shown));
        ev.insert("context_band".into(), assessed["context_band"].clone());
        ev.insert("ruleset".into(), assessed["context"]["ruleset"].clone());
        ev.insert("inputs_hash".into(), json!(inputs_hash));
        self.append(ev);
        Some(a)
    }

    /// Replace the stored assessment (the API finishes the result object after opening the alert).
    pub fn set_assessed(&mut self, alert_id: &str, assessed: Value) {
        if let Some(a) = self.alerts.get_mut(alert_id) {
            a["assessed"] = assessed;
        }
    }

    pub fn act(&mut self, alert_id: &str, analyst: &str, action: &str, note: &str) -> Result<Value, ReviewError> {
        if !self.alerts.contains_key(alert_id) {
            return Err(ReviewError::NotFound);
        }
        if !ACTIONS.contains(&action) {
            return Err(ReviewError::Invalid(format!("action must be one of ['{}']", ACTIONS.join("', '"))));
        }
        if analyst.is_empty() {
            return Err(ReviewError::Invalid("analyst id is required".into()));
        }
        let a = self.alerts.get_mut(alert_id).unwrap();
        if action == "approve_block" {
            let appr = a["block_approvals"].as_array_mut().unwrap();
            if appr.iter().any(|x| x == analyst) {
                return Err(ReviewError::Invalid("four-eyes: the same analyst cannot approve a block twice".into()));
            }
            appr.push(json!(analyst));
            let n = appr.len();
            a["status"] = json!(if n >= 2 { "blocked" } else { "block_pending_second_approval" });
        } else {
            a["status"] = json!(match action {
                "release" => "released",
                "verify" => "customer_verification",
                _ => "held",
            });
        }
        a["actions"].as_array_mut().unwrap().push(json!({"analyst": analyst, "action": action, "note": note}));
        let status = a["status"].clone();
        let out = a.clone();
        let mut ev = Map::new();
        ev.insert("type".into(), json!("analyst_action"));
        ev.insert("alert_id".into(), json!(alert_id));
        ev.insert("analyst".into(), json!(analyst));
        ev.insert("action".into(), json!(action));
        ev.insert("status".into(), status);
        self.append(ev);
        Ok(out)
    }

    /// Customer and system events (pause screen answer, re-verification, duress hold, consortium publish).
    pub fn system_event(&mut self, alert_id: &str, status: &str, actor: &str, detail: &str, set_status: bool) -> Result<Value, ReviewError> {
        let a = self.alerts.get_mut(alert_id).ok_or(ReviewError::NotFound)?;
        if set_status && a["status"] != "blocked" {
            a["status"] = json!(status);
        }
        a["actions"].as_array_mut().unwrap().push(json!({"analyst": actor, "action": status, "note": detail}));
        let st = a["status"].clone();
        let out = a.clone();
        let mut ev = Map::new();
        ev.insert("type".into(), json!("event"));
        ev.insert("alert_id".into(), json!(alert_id));
        ev.insert("actor".into(), json!(actor));
        ev.insert("action".into(), json!(status));
        ev.insert("status".into(), st);
        self.append(ev);
        Ok(out)
    }

    pub fn set_field(&mut self, alert_id: &str, k: &str, v: Value) {
        if let Some(a) = self.alerts.get_mut(alert_id) {
            a[k] = v;
        }
    }

    pub fn verify_chain(&self) -> bool {
        let mut prev = "0".repeat(64);
        for e in &self.log {
            let mut body = e.as_object().cloned().unwrap_or_default();
            let h = body.shift_remove("hash");
            if e["prev"].as_str() != Some(prev.as_str()) || h.as_ref().and_then(|x| x.as_str()) != Some(hash_obj(&Value::Object(body)).as_str()) {
                return false;
            }
            prev = h.and_then(|x| x.as_str().map(String::from)).unwrap_or_default();
        }
        true
    }

    pub fn evidence(&self, alert_id: &str) -> Option<Value> {
        let a = self.alerts.get(alert_id)?;
        let accts = |x: &Value| [x["payment"]["from_account"].clone(), x["payment"]["to_account"].clone()];
        let mine = accts(a);
        let linked: Vec<Value> = self.alerts.iter().filter(|(k, _)| k.as_str() != alert_id)
            .filter(|(_, b)| accts(b).iter().any(|x| mine.contains(x))).map(|(k, _)| json!(k)).collect();
        let audit: Vec<Value> = self.log.iter().filter(|e| e.get("alert_id").and_then(|v| v.as_str()) == Some(alert_id)).cloned().collect();
        let mut pack = json!({"alert": a, "linked_alerts": linked, "audit_entries": audit,
                              "disclaimer": "Draft for compliance review. Risk points are rule outputs, not fraud probabilities."});
        let h = hash_obj(&pack);
        pack["pack_hash"] = json!(h);
        Some(pack)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tampering_breaks_the_chain() {
        let mut d = ReviewDesk::new();
        let ev = |t: &str| { let mut m = Map::new(); m.insert("type".into(), json!(t)); m };
        d.append(ev("x"));
        d.append(ev("y"));
        assert!(d.verify_chain());
        d.log[0]["type"] = json!("edited");
        assert!(!d.verify_chain());
    }
}
