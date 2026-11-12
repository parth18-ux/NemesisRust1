//! Redis backed state equals the in memory engine; partitioned workers score every payment once
//! (port of tests/test_scale.py). Skipped when redis-server is not installed.
use rusetect::payment::{read_demo_rows, Payment};
use rusetect::{paths, Engine};
use rusetect_server::scale::{producer::publish, redis_state::RedisState, worker::run};
use std::process::{Child, Command, Stdio};

struct Redis {
    child: Child,
    url: String,
}

impl Drop for Redis {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn start() -> Option<Redis> {
    let which = Command::new("sh").arg("-c").arg("command -v redis-server").output().ok()?;
    if !which.status.success() {
        eprintln!("redis-server not installed: skipping");
        return None;
    }
    let port = std::net::TcpListener::bind("127.0.0.1:0").ok()?.local_addr().ok()?.port();
    let child = Command::new("redis-server").args(["--port", &port.to_string(), "--save", "", "--appendonly", "no"])
        .stdout(Stdio::null()).spawn().ok()?;
    let url = format!("redis://127.0.0.1:{port}/0");
    for _ in 0..100 {
        if redis::Client::open(url.as_str()).ok().and_then(|c| c.get_connection().ok()).is_some() {
            return Some(Redis { child, url });
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    None
}

fn rows(n: usize) -> Vec<Payment> {
    read_demo_rows(&paths::models_dir().join("demo_stream.csv.gz")).unwrap().iter().take(n).enumerate()
        .map(|(i, r)| Payment { eid: Some(i as i64), ..Payment::from_demo_row(r).unwrap() }).collect()
}

#[test]
fn redis_state_matches_engine() {
    let Some(r) = start() else { return };
    let mut eng = Engine::new(&paths::models_dir(), false).unwrap();
    let con = redis::Client::open(r.url.as_str()).unwrap().get_connection().unwrap();
    let mut rs = RedisState::new(con, eng.cfg.clone(), "");
    for (i, p) in rows(6000).iter().enumerate() {
        let sf = eng.sender_fan_in_24h(p);
        let fa = eng.features(p);
        let fb = rs.features(p).unwrap();
        assert_eq!(rs.last_sender_fan_in, sf, "row {i}");
        for k in 0..31 {
            let (a, b) = (fa.0[k], fb.0[k]);
            // math.isclose(a, b, rel_tol=1e-9, abs_tol=1e-9): Redis keeps running sums as long doubles
            let same = (a.is_nan() && b.is_nan()) || a == b || (a - b).abs() <= (1e-9 * a.abs().max(b.abs())).max(1e-9);
            assert!(same, "row {i} feature {}: {a} vs {b}", rusetect::engine::FEATURES[k]);
        }
    }
}

#[test]
fn partitioned_workers_score_everything() {
    let Some(r) = start() else { return };
    let mut con = redis::Client::open(r.url.as_str()).unwrap().get_connection().unwrap();
    let pays = rows(3000);
    publish(&mut con, &pays, 2, "", 2000, true).unwrap();
    let hs: Vec<_> = (0..2).map(|k| { let u = r.url.clone(); std::thread::spawn(move || run(&u, k, 200, "rusetect", "").unwrap()) }).collect();
    let total: usize = hs.into_iter().map(|h| h.join().unwrap()).sum();
    assert_eq!(total, 3000);
    let out: Vec<redis::Value> = redis::cmd("XRANGE").arg("decisions").arg("-").arg("+").query(&mut con).unwrap();
    let mut eids = vec![];
    for e in out {
        let redis::Value::Array(parts) = e else { continue };
        let Some(redis::Value::Array(kv)) = parts.get(1) else { continue };
        for c in kv.chunks(2) {
            if let (redis::Value::BulkString(k), redis::Value::BulkString(v)) = (&c[0], &c[1]) {
                if k == b"d" {
                    let d: serde_json::Value = serde_json::from_slice(v).unwrap();
                    assert!(rusetect::engine::DECISIONS.contains(&d["final_decision"].as_str().unwrap()));
                    eids.push(d["eid"].as_i64().unwrap());
                }
            }
        }
    }
    eids.sort();
    assert_eq!(eids, (0..3000).collect::<Vec<i64>>());
}

#[test]
fn redis_pair_window_matches_engine_window() {
    let Some(r) = start() else { return };
    let con = redis::Client::open(r.url.as_str()).unwrap().get_connection().unwrap();
    let mut rs = RedisState::new(con, Engine::new(&paths::models_dir(), false).unwrap().cfg.clone(), "");
    let mut w = rusetect::engine::PairWindow::new();
    let mut x = 7u64;
    let mut t = 0i64;
    for i in 0..3000i64 {
        x ^= x << 13; x ^= x >> 7; x ^= x << 17;
        t += (x % 60) as i64;
        let (payer, payee, risk) = (format!("P{}", x % 7), format!("D{}", (x >> 8) % 3), ((x >> 16) % 1000) as f64 / 1000.0);
        let p = Payment { from_bank: "1".into(), from_account: payer.clone(), to_bank: "2".into(), to_account: "x".into(), amount_paid: 1.0,
            payment_currency: "US Dollar".into(), receiving_currency: "US Dollar".into(), payment_format: "ACH".into(), tmin: t, eid: Some(i) };
        let (n, a) = w.push((payer, payee.clone()), t, risk);
        let (m, b) = rs.pair_window(&p, &payee, risk).unwrap();
        assert_eq!(n, m, "row {i}");
        assert!((a - b).abs() < 1e-12, "row {i}: {a} vs {b}");
    }
    // a redelivered payment is not counted twice
    let p = Payment { from_bank: "1".into(), from_account: "Q".into(), to_bank: "2".into(), to_account: "x".into(), amount_paid: 1.0,
        payment_currency: "US Dollar".into(), receiving_currency: "US Dollar".into(), payment_format: "ACH".into(), tmin: t, eid: Some(99_999) };
    assert_eq!(rs.pair_window(&p, "d", 0.5).unwrap().0, 1);
    assert_eq!(rs.pair_window(&p, "d", 0.5).unwrap().0, 1);
}

#[test]
fn worker_decisions_match_engine() {
    let Some(r) = start() else { return };
    let con = redis::Client::open(r.url.as_str()).unwrap().get_connection().unwrap();
    let mut sc = rusetect_server::scale::worker::Scorer::new(con, "").unwrap();
    let mut eng = Engine::new(&paths::models_dir(), true).unwrap();
    let pays = rows(4000);
    let got = sc.score(&pays).unwrap();
    let mut flagged = 0;
    for (i, (p, g)) in pays.iter().zip(&got).enumerate() {
        let e = eng.score_payment(p);
        assert_eq!(g["payment_decision"], e["decision"], "row {i}");
        flagged += (e["decision"] != rusetect::engine::DECISIONS[0]) as usize;
    }
    assert!(flagged > 0, "the slice exercises flagged payments");
}

#[test]
fn malformed_messages_go_to_dead_letter() {
    let Some(r) = start() else { return };
    let mut con = redis::Client::open(r.url.as_str()).unwrap().get_connection().unwrap();
    redis::cmd("XADD").arg("payments:0").arg("*").arg("p").arg("{not json").query::<()>(&mut con).unwrap();
    let upi = Payment { payment_format: "UPI".into(), ..rows(1)[0].clone() };
    publish(&mut con, &[upi], 1, "", 1, false).unwrap();
    publish(&mut con, &rows(50), 1, "", 50, true).unwrap();
    assert_eq!(run(&r.url, 0, 20, "rusetect", "").unwrap(), 50, "the worker survives and scores every good payment");
    let n: usize = redis::cmd("XLEN").arg("deadletter").query(&mut con).unwrap();
    assert_eq!(n, 2, "bad JSON and an unknown payment format are both dead-lettered");
}
