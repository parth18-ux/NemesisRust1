//! Replays research/pramaan/reference.py output through the Rust port and compares it with PRAMAAN's Python engine.
//!     cargo run --release -p rusetect --example pramaan_parity -- <reference.jsonl>
use rusetect::{paths, pramaan};
use std::io::BufRead;

fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).expect("reference.jsonl");
    let mut eng = pramaan::AuthEngine::new(pramaan::AuthModel::load(&paths::models_dir())?.expect("auth model"));
    let (mut n, mut dec, mut ch, mut rule, mut reasons, mut feat, mut fin, mut max_risk) = (0, 0, 0, 0, 0, 0, 0, 0f64);
    for line in std::io::BufReader::new(std::fs::File::open(path)?).lines() {
        let v: serde_json::Value = serde_json::from_str(&line?)?;
        let req = &v["request"];
        let id = req["request_id"].as_str().unwrap().to_string();
        let out = eng.authenticate(req, id.clone());
        let e = &v["expected"];
        n += 1;
        if out["decision"] != e["decision"] { dec += 1; if dec <= 5 { eprintln!("decision {id}: rust {} python {} (risk {} vs {})", out["decision"], e["decision"], out["risk_score"], e["risk_score"]); } }
        if out["challenge_type"] != e["challenge_type"] { ch += 1; }
        if out["rule"] != e["rule"] { rule += 1; }
        let rs: Vec<&str> = out["reasons"].as_array().unwrap().iter().map(|r| r["signal"].as_str().unwrap()).collect();
        let es: Vec<&str> = v["expected_reasons"].as_array().unwrap().iter().map(|r| r.as_str().unwrap()).collect();
        if rs != es { reasons += 1; }
        max_risk = max_risk.max((out["risk_score"].as_f64().unwrap() - e["risk_score"].as_f64().unwrap()).abs());
        for (k, ev) in e["features"].as_object().unwrap() {
            let rv = &out["features"][k];
            let same = match (rv.as_f64(), ev.as_f64()) { (Some(a), Some(b)) => (a - b).abs() <= 1.5e-4, (None, None) => true, _ => false };
            if !same { feat += 1; if feat <= 5 { eprintln!("feature {id} {k}: rust {rv} python {ev}"); } }
        }
        if let Some(c) = v.get("challenge") {
            let r = eng.verify_challenge(&id, c["passed"].as_bool().unwrap()).expect("pending challenge");
            if r["decision"] != c["decision"] || r["device_trusted"] != c["device_trusted"] { fin += 1; }
        }
    }
    println!("{n} steps: decision mismatches {dec}, challenge type {ch}, rule {rule}, reasons {reasons}, feature values {feat}, verify {fin}; max |risk diff| {max_risk:.2e}");
    Ok(())
}
