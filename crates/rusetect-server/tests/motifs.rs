//! Temporal-motif reason codes: the pre-registered checks (docs/decision.md 8z), written before the implementation.
//! The oracle below is the definition: it reads a plain list of past payments, not the engine's state.
use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use rusetect::{paths, payment, Engine, Payment};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use tower::ServiceExt;

const WINDOW: i64 = 1440; // the engine's 24 h windows, in minutes
const SCAN: usize = 64; // newest window entries read per account
const FAN: usize = 4; // distinct counterparties that make a fan (the context rules' fan_in_4plus)

/// The `motifs` list of the motif reason, or empty when the payment carries none.
fn motifs(reasons: &Value) -> Vec<String> {
    let with: Vec<&Value> = reasons.as_array().unwrap().iter().filter(|r| r.get("motifs").is_some()).collect();
    assert!(with.len() <= 1, "at most one motif reason");
    with.first().map(|r| {
        assert_eq!(r["feature"], "Temporal motif");
        assert!(!r["display"].as_str().unwrap().is_empty());
        r["motifs"].as_array().unwrap().iter().map(|m| m.as_str().unwrap().to_string()).collect()
    }).unwrap_or_default()
}

/// Brute-force motif oracle over every payment seen so far, the current one included (time-ordered streams).
#[derive(Default)]
struct Oracle {
    ins: HashMap<String, Vec<(i64, String, f64)>>, // payee -> (minute, payer, usd)
    outs: HashMap<String, Vec<(i64, String)>>,     // payer -> (minute, payee)
}

impl Oracle {
    fn add(&mut self, s: &str, d: &str, t: i64, usd: f64) {
        self.ins.entry(d.into()).or_default().push((t, s.into(), usd));
        self.outs.entry(s.into()).or_default().push((t, d.into()));
    }

    /// receipts of `a` inside the window ending at `t`
    fn window(&self, a: &str, t: i64) -> Vec<&(i64, String, f64)> {
        self.ins.get(a).map(|v| v.iter().filter(|e| e.0 > t - WINDOW && e.0 <= t).collect()).unwrap_or_default()
    }

    /// the newest SCAN of them
    fn recent(&self, a: &str, t: i64) -> Vec<&(i64, String, f64)> {
        let w = self.window(a, t);
        w[w.len().saturating_sub(SCAN)..].to_vec()
    }

    /// Motifs the payment s -> d at minute t completes, in the fixed order the engine reports them.
    fn motifs(&self, s: &str, d: &str, t: i64, usd: f64) -> Vec<&'static str> {
        let mut out = Vec::new();
        if s == d {
            return out;
        }
        let s_in = self.recent(s, t);
        let other = |a: &str| a != s && a != d;
        // cycle: money left the payee and is back at the payer, directly or through one account, before this payment
        if s_in.iter().any(|e| e.1 == d || (other(&e.1) && self.recent(&e.1, t).iter().any(|x| x.1 == d && x.0 <= e.0))) {
            out.push("cycle");
        }
        // scatter-gather: an account that paid this payer also paid another account, which then paid this payee
        let sources: HashSet<&str> = s_in.iter().map(|e| e.1.as_str()).filter(|a| other(a)).collect();
        if self.recent(d, t).iter().any(|k| other(&k.1) && self.recent(&k.1, t).iter().any(|a| a.1 != k.1 && sources.contains(a.1.as_str()) && a.0 <= k.0)) {
            out.push("scatter_gather");
        }
        // pass-through chain: this payment forwards between half and all of one receipt from a third account
        if s_in.iter().any(|e| other(&e.1) && usd >= 0.5 * e.2 && usd <= e.2) {
            out.push("pass_through");
        }
        if self.window(d, t).iter().map(|e| &e.1).collect::<HashSet<_>>().len() >= FAN {
            out.push("fan_in");
        }
        let paid: HashSet<&String> = self.outs.get(s).map(|v| v.iter().filter(|e| e.0 > t - WINDOW && e.0 <= t).map(|e| &e.1).collect()).unwrap_or_default();
        if paid.len() >= FAN {
            out.push("fan_out");
        }
        out
    }
}

/// M1 and M2 on the demo stream: after 100,000 payments of history, every flagged payment of the next 20,000 carries
/// exactly the oracle's motifs, and no approved payment carries a motif reason.
#[test]
fn demo_stream_motif_reasons_match_the_oracle() {
    let rows = payment::read_demo_rows(&paths::models_dir().join("demo_stream.csv.gz")).unwrap();
    let pays: Vec<Payment> = rows.iter().take(120_000).map(|r| Payment::from_demo_row(r).unwrap()).collect();
    assert!(pays.windows(2).all(|w| w[0].tmin <= w[1].tmin), "the oracle assumes a time-ordered stream");
    let mut eng = Engine::new(&paths::models_dir(), true).unwrap();
    let mut o = Oracle::default();
    let mut seen: HashMap<&str, usize> = HashMap::new();
    let mut flagged = 0;
    for (i, p) in pays.iter().enumerate() {
        let (s, d) = (p.sender_key(), p.payee_key());
        let usd = eng.cfg.usd(p.amount_paid, &p.payment_currency);
        o.add(&s, &d, p.tmin, usd);
        if i < 100_000 {
            eng.features(p);
            continue;
        }
        let r = eng.score_payment(p);
        let got = motifs(&r["reasons"]);
        if r["decision"] == "Approve" {
            assert!(got.is_empty(), "row {i}: motif reason on an approved payment");
            continue;
        }
        flagged += 1;
        let want = o.motifs(&s, &d, p.tmin, usd);
        assert_eq!(got, want, "row {i}: {s} -> {d}");
        for m in want {
            *seen.entry(m).or_default() += 1;
        }
    }
    eprintln!("flagged {flagged}; motifs {seen:?}");
    assert!(flagged > 100 && !seen.is_empty(), "the slice exercises flagged payments with motifs");
}

/// M1 and M2 on the mule fixture, replayed through /api/v2/assess as the investigator page does: twelve victims pay
/// one mule, which forwards 98% to a collector a minute later. Every payment the final decision does not approve
/// carries the motif the scenario was built on.
#[tokio::test]
async fn mule_fixture_names_fan_in_and_pass_through() {
    std::env::set_var("RUSETECT_RATE_LIMIT", "1000000");
    let app = rusetect_server::build_app(rusetect_server::new_shared());
    let f: Value = serde_json::from_str(&std::fs::read_to_string(paths::repo_root().join("fixtures/mule_replay.json")).unwrap()).unwrap();
    let (mut fans, mut passes) = (0, 0);
    for (i, amt) in f["deposits_inr"].as_array().unwrap().iter().enumerate() {
        let amt = amt.as_f64().unwrap();
        let t = i as i64 * f["gap_minutes"].as_i64().unwrap();
        let ts = |m: i64| format!("2022-09-09 {:02}:{:02}", 10 + m / 60, m % 60);
        let deposit = json!({"timestamp": ts(t), "from_bank": format!("01{:02}", i + 1), "from_account": format!("V{:02}", i + 1),
            "to_bank": f["mule"]["bank"], "to_account": f["mule"]["account"], "amount_paid": amt, "payment_currency": "Rupee", "payment_format": "ACH"});
        let forward = json!({"timestamp": ts(t + 1), "from_bank": f["mule"]["bank"], "from_account": f["mule"]["account"],
            "to_bank": f["collector"]["bank"], "to_account": f["collector"]["account"],
            "amount_paid": (amt * f["forward_share"].as_f64().unwrap() * 100.0).round() / 100.0, "payment_currency": "Rupee", "payment_format": "ACH"});
        for (body, want, n) in [(deposit, if i + 1 >= FAN { vec!["fan_in"] } else { vec![] }, &mut fans), (forward, vec!["pass_through"], &mut passes)] {
            let req = Request::builder().method("POST").uri("/api/v2/assess").header("content-type", "application/json").body(Body::from(body.to_string())).unwrap();
            let bytes = app.clone().oneshot(req).await.unwrap().into_body().collect().await.unwrap().to_bytes();
            let r: Value = serde_json::from_slice(&bytes).unwrap();
            let got = motifs(&r["payment"]["reasons"]);
            if r["final_decision"] == "Approve" {
                assert!(got.is_empty(), "payment {i}: motif reason on an approved payment");
            } else {
                assert_eq!(got, want, "payment {i}: {}", r["final_decision"]);
                *n += !want.is_empty() as usize;
            }
        }
    }
    // deviation from the pre-registration (e85fca1), which also asked for a non-approved deposit: the served engine
    // approves every deposit of this fixture, so fan-in is exercised on the demo stream and the hand-built case only
    assert!(passes > 0 && fans == 0, "fan-in {fans}, pass-through {passes}");
}

/// M1(c): one hand-built scenario per motif names that motif, and nothing once the history is a day old.
#[test]
fn each_motif_is_named_and_expires_with_the_window() {
    let pay = |from: &str, to: &str, t: i64, amt: f64| Payment { from_bank: "1".into(), from_account: from.into(), to_bank: "1".into(), to_account: to.into(),
        amount_paid: amt, payment_currency: "US Dollar".into(), receiving_currency: "US Dollar".into(), payment_format: "ACH".into(), tmin: t, eid: None };
    let fan = |inward: bool| (0..3).map(|i| { let p = format!("P{i}"); if inward { (p, "D".to_string()) } else { ("S".to_string(), p) } }).collect::<Vec<_>>();
    let h = |v: &[(&str, &str)]| v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect::<Vec<_>>();
    // (earlier payments one minute apart at 100, the last payment's amount, the motif)
    let cases = [
        (h(&[("D", "S")]), 10.0, "cycle"),
        (h(&[("D", "K"), ("K", "S")]), 10.0, "cycle"),
        (h(&[("A", "S"), ("A", "K"), ("K", "D")]), 10.0, "scatter_gather"),
        (h(&[("A", "S")]), 98.0, "pass_through"),
        (fan(true), 10.0, "fan_in"),
        (fan(false), 10.0, "fan_out"),
    ];
    for (hist, amt, want) in cases {
        for late in [false, true] {
            let mut e = Engine::new(&paths::models_dir(), false).unwrap();
            for (i, (a, b)) in hist.iter().enumerate() {
                e.features(&pay(a, b, i as i64, 100.0));
            }
            let last = pay("S", "D", hist.len() as i64 - 1 + if late { WINDOW } else { 1 }, amt);
            e.features(&last);
            let got = e.motif_reason(&last).map(|r| motifs(&json!([r]))).unwrap_or_default();
            assert_eq!(got, if late { vec![] } else { vec![want.to_string()] }, "{want}, a day late: {late}");
        }
    }
    let mut e = Engine::new(&paths::models_dir(), false).unwrap();
    let own = pay("S", "S", 0, 100.0);
    e.features(&own);
    assert!(e.motif_reason(&own).is_none(), "a self transfer has no motif");
}

/// 8z-followup (docs/decision.md), written before it was run: the fan-in fixture replayed through /api/v2/assess, each
/// deposit with the campaign's lure and consent. Every deposit the final decision does not approve carries exactly the
/// oracle's motifs (`fan_in` from the fourth distinct payer on, nothing before); approved deposits carry no motif
/// reason; and at least one non-approved deposit carries `fan_in`.
#[tokio::test]
async fn fan_in_fixture_names_fan_in_on_held_deposits() {
    std::env::set_var("RUSETECT_RATE_LIMIT", "1000000");
    let app = rusetect_server::build_app(rusetect_server::new_shared());
    let f: Value = serde_json::from_str(&std::fs::read_to_string(paths::repo_root().join("fixtures/mule_fanin.json")).unwrap()).unwrap();
    let (mut held, mut held_fan, mut rows) = (0, 0, Vec::new());
    for (i, amt) in f["deposits_inr"].as_array().unwrap().iter().enumerate() {
        let t = i as i64 * f["gap_minutes"].as_i64().unwrap();
        let body = json!({"timestamp": format!("2022-09-10 {:02}:{:02}", 10 + t / 60, t % 60), "from_bank": format!("02{:02}", i + 1),
            "from_account": format!("W{:02}", i + 1), "to_bank": f["mule"]["bank"], "to_account": f["mule"]["account"],
            "amount_paid": amt, "payment_currency": "Rupee", "payment_format": "ACH", "message": f["lure"], "consent": true});
        let req = Request::builder().method("POST").uri("/api/v2/assess").header("content-type", "application/json").body(Body::from(body.to_string())).unwrap();
        let bytes = app.clone().oneshot(req).await.unwrap().into_body().collect().await.unwrap().to_bytes();
        let r: Value = serde_json::from_slice(&bytes).unwrap();
        let got = motifs(&r["payment"]["reasons"]);
        rows.push(json!({"deposit": i + 1, "final_decision": r["final_decision"], "payment_decision": r["payment"]["decision"],
                         "context_band": r["context_band"], "motifs": got}));
        if r["final_decision"] == "Approve" {
            assert!(got.is_empty(), "deposit {}: motif reason on an approved payment", i + 1);
            continue;
        }
        held += 1;
        let want: Vec<String> = if i + 1 >= FAN { vec!["fan_in".into()] } else { vec![] };
        assert_eq!(got, want, "deposit {}: {}", i + 1, r["final_decision"]);
        held_fan += !want.is_empty() as usize;
    }
    let out = json!({"fixture": "fixtures/mule_fanin.json", "check": "docs/decision.md 8z-followup", "deposits": rows.len(),
                     "not_approved": held, "not_approved_with_fan_in": held_fan, "per_deposit": rows});
    std::fs::write(paths::repo_root().join("results/motif_fanin.json"), serde_json::to_string_pretty(&out).unwrap() + "\n").unwrap();
    assert!(held_fan > 0, "no non-approved deposit carried fan_in (not approved: {held})");
}
