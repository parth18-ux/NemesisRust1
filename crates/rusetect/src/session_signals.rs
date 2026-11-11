//! Session and identity domain (port of service/session_signals.py): is the customer being coached right now,
//! and is it really them?
//!
//! Signals come from the bank's app at payment time. Nothing needs raw biometrics or raw device identifiers: the
//! device arrives as a salted hash, the SIM swap as "hours since the last swap" from the telecom check.
//!   1. coaching cues   live call (long or video), screen share, remote access app, payee pasted in, long hesitation,
//!                      app sent to the background mid payment, typing rhythm unlike the customer's own
//!   2. identity gate   enrolled device hash vs this device, recent SIM swap: decided BEFORE the OTP is sent
//!   3. attack chains   password reset -> new device -> new payee -> transfer (full, 24 h) or
//!                      new device -> new payee -> transfer within an hour (short)
//! They produce SESSION points (cap 40), a fourth domain in context_rules::assess, so coaching cues alone can ask
//! for verification, never an analyst hold. A coercion index in [0, 1] at 0.65 or more forces at least a step up.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const SESSION_CAP: i64 = 40;
pub const LONG_CALL_MIN: f64 = 20.0;
pub const HESITATION_S: f64 = 20.0;
pub const HIDDEN_S: f64 = 30.0;
pub const SIM_SWAP_RECENT_H: f64 = 72.0;
pub const CHAIN_WINDOW_MIN: i64 = 24 * 60;
pub const SHORT_CHAIN_MIN: i64 = 60;
pub const COERCION_STEP_UP: f64 = 0.65;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    #[serde(rename = "type")]
    pub kind: String,
    pub minute: i64,
}

#[derive(Debug, Clone, Default)]
pub struct SessionSignals {
    pub session_id: Option<String>,
    pub on_call: bool,
    pub call_minutes: f64,
    pub video_call: bool,
    pub screen_share: bool,
    pub remote_access_app: bool,
    pub payee_pasted: bool,
    pub hesitation_s: f64,
    pub app_hidden_s: f64,
    /// from behaviour::TypingProfile
    pub typing_anomaly: bool,
    pub device_hash: Option<String>,
    pub enrolled_device_hashes: Vec<String>,
    /// hours since the last SIM swap; None = no swap known
    pub sim_swap_hours: Option<f64>,
    pub events: Vec<Event>,
    /// payee is on the consortium mule list (set by the server)
    pub network_flag: bool,
}

#[derive(Debug, Clone)]
pub struct SessionResult {
    pub points: i64,
    pub cues: Vec<String>,
    pub identity: Value,
    pub chain: Value,
    pub coercion_index: f64,
    pub force_verify: bool,
}

impl SessionResult {
    pub fn to_json(&self) -> Value {
        json!({"points": self.points, "cues": self.cues, "identity": self.identity, "chain": self.chain,
               "coercion_index": self.coercion_index, "force_verify": self.force_verify})
    }
}

fn recent_swap(s: &SessionSignals) -> bool {
    s.sim_swap_hours.map(|h| h < SIM_SWAP_RECENT_H).unwrap_or(false)
}

/// Run before the OTP is sent. A new, unenrolled device right after a SIM swap is the SIM swap takeover pattern:
/// deny the OTP and require re-verification. This is an authentication step, not a payment block.
pub fn identity_check(s: &SessionSignals) -> Value {
    let new_device = s.device_hash.as_deref().map(|d| !d.is_empty()).unwrap_or(false)
        && !s.enrolled_device_hashes.is_empty()
        && !s.enrolled_device_hashes.iter().any(|e| Some(e.as_str()) == s.device_hash.as_deref());
    let swap = recent_swap(s);
    if new_device && swap {
        return json!({"decision": "deny_otp_reverify", "new_device": true, "recent_sim_swap": true,
                      "reason": format!("unenrolled device within {} h of a SIM swap", SIM_SWAP_RECENT_H as i64)});
    }
    if swap {
        return json!({"decision": "step_up", "new_device": new_device, "recent_sim_swap": true, "reason": "recent SIM swap"});
    }
    if new_device {
        return json!({"decision": "step_up", "new_device": true, "recent_sim_swap": false, "reason": "unenrolled device"});
    }
    json!({"decision": "allow", "new_device": false, "recent_sim_swap": false, "reason": ""})
}

/// Longest ordered chain among the events (by minute).
pub fn attack_chain(events: &[Event]) -> Value {
    let mut ev: Vec<&Event> = events.iter().collect();
    ev.sort_by_key(|e| e.minute);
    let find = |seq: &[&str], window: i64| -> Option<(Vec<String>, i64)> {
        for (i, e) in ev.iter().enumerate() {
            if e.kind != seq[0] {
                continue;
            }
            let (mut k, t0) = (1usize, e.minute);
            let mut path = vec![e.kind.clone()];
            for f in &ev[i + 1..] {
                if f.minute - t0 > window {
                    break;
                }
                if k < seq.len() && f.kind == seq[k] {
                    path.push(f.kind.clone());
                    k += 1;
                    if k == seq.len() {
                        return Some((path, f.minute - t0));
                    }
                }
            }
        }
        None
    };
    let limit = ev.iter().any(|e| e.kind == "limit_raise");
    if let Some((steps, span)) = find(&["password_reset", "new_device", "new_payee", "transfer"], CHAIN_WINDOW_MIN) {
        return json!({"chain": "full", "steps": steps, "minutes": span, "limit_raised": limit});
    }
    if let Some((steps, span)) = find(&["new_device", "new_payee", "transfer"], SHORT_CHAIN_MIN) {
        return json!({"chain": "short", "steps": steps, "minutes": span, "limit_raised": limit});
    }
    json!({"chain": null, "steps": [], "minutes": null, "limit_raised": false})
}

pub fn coercion_index(s: &SessionSignals) -> f64 {
    let b = |x: bool| -> f64 { if x { 1.0 } else { 0.0 } };
    let w = 0.30 * b(s.screen_share || s.remote_access_app) + 0.20 * b(s.payee_pasted) + 0.20 * b(s.hesitation_s >= HESITATION_S)
        + 0.15 * b(recent_swap(s)) + 0.10 * b(s.app_hidden_s >= HIDDEN_S) + 0.05 * b(s.network_flag);
    crate::pyfmt::py_round(w.min(1.0), 2)
}

pub fn assess_session(s: Option<&SessionSignals>) -> Option<SessionResult> {
    let s = s?;
    let mut cues: Vec<(&str, i64)> = Vec::new();
    if s.remote_access_app {
        cues.push(("remote_access_app", 20));
    }
    if s.screen_share {
        cues.push(("screen_share", 15));
    }
    if s.on_call || s.video_call {
        cues.push(("on_call_while_paying", 10));
        if s.video_call || s.call_minutes >= LONG_CALL_MIN {
            cues.push(("long_or_video_call", 5));
        }
    }
    if s.payee_pasted {
        cues.push(("payee_pasted", 10));
    }
    if s.hesitation_s >= HESITATION_S {
        cues.push(("long_hesitation", 5)); // weak alone: older and first time users also pause
    }
    if s.app_hidden_s >= HIDDEN_S {
        cues.push(("app_hidden_mid_payment", 5));
    }
    if s.typing_anomaly {
        cues.push(("typing_rhythm_unlike_customer", 5));
    }
    let idn = identity_check(s);
    let new_device = idn["new_device"].as_bool().unwrap_or(false);
    let swap = idn["recent_sim_swap"].as_bool().unwrap_or(false);
    if idn["decision"] == "deny_otp_reverify" {
        cues.push(("sim_swap_takeover", 25));
    } else if swap {
        cues.push(("recent_sim_swap", 10));
    } else if new_device {
        cues.push(("unenrolled_device", 5));
    }
    let ch = attack_chain(&s.events);
    if ch["chain"] == "full" {
        cues.push(("attack_chain_full", 25)); // policy: a full takeover chain plus a new payee transfer reaches an analyst
    } else if ch["chain"] == "short" {
        cues.push(("attack_chain_short", 10));
    }
    let ci = coercion_index(s);
    // policy: always ask when coercion is likely, or when it may not be the customer at all
    let not_them = (new_device && s.typing_anomaly) || swap;
    let mut names: Vec<String> = cues.iter().map(|c| c.0.to_string()).collect();
    names.sort();
    Some(SessionResult {
        points: SESSION_CAP.min(cues.iter().map(|c| c.1).sum()),
        cues: names,
        identity: idn,
        chain: ch,
        coercion_index: ci,
        force_verify: ci >= COERCION_STEP_UP || not_them,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(k: &str, m: i64) -> Event {
        Event { kind: k.into(), minute: m }
    }

    #[test]
    fn sim_swap_takeover_withholds_the_otp() {
        let s = SessionSignals { device_hash: Some("d2".into()), enrolled_device_hashes: vec!["d1".into()], sim_swap_hours: Some(3.0), ..Default::default() };
        assert_eq!(identity_check(&s)["decision"], "deny_otp_reverify");
        let s2 = SessionSignals { device_hash: Some("d1".into()), enrolled_device_hashes: vec!["d1".into()], ..Default::default() };
        assert_eq!(identity_check(&s2)["decision"], "allow");
        assert_eq!(identity_check(&SessionSignals { sim_swap_hours: Some(500.0), ..Default::default() })["decision"], "allow");
    }

    #[test]
    fn attack_chains_need_order_and_window() {
        let e = vec![ev("password_reset", 0), ev("new_device", 30), ev("new_payee", 40), ev("transfer", 50)];
        assert_eq!(attack_chain(&e)["chain"], "full");
        let rev: Vec<Event> = e.iter().rev().map(|x| ev(&x.kind, -x.minute)).collect();
        assert!(attack_chain(&rev)["chain"].is_null());
        assert_eq!(attack_chain(&[ev("new_device", 0), ev("new_payee", 20), ev("transfer", 30)])["chain"], "short");
        assert!(attack_chain(&[ev("new_device", 0), ev("new_payee", 20), ev("transfer", 300)])["chain"].is_null());
    }

    #[test]
    fn coercion_index_and_cap() {
        let s = SessionSignals { screen_share: true, payee_pasted: true, hesitation_s: 30.0, ..Default::default() };
        assert_eq!(coercion_index(&s), 0.7);
        assert!(assess_session(Some(&s)).unwrap().force_verify);
        let all = SessionSignals { on_call: true, video_call: true, screen_share: true, remote_access_app: true, payee_pasted: true,
                                   hesitation_s: 40.0, app_hidden_s: 60.0, ..Default::default() };
        assert_eq!(assess_session(Some(&all)).unwrap().points, 40);
    }
}
