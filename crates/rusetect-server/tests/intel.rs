//! The website's read endpoints (dashboard, transaction search and detail, entity graph) and the case work around the
//! review desk (alert triage, investigations with a two-person closure), driven through the signed webhook as a bank
//! would: what they return is what the engine decided and the stores hold. The Redis test is skipped when redis-server
//! is not installed. Role checks with configured keys are in tests/intel_auth.rs.
use axum::body::Body;
use axum::http::{Request, StatusCode};
use hmac::{Hmac, Mac};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::process::{Child, Command, Stdio};
use tower::ServiceExt;

const SECRET: &str = "test-webhook-secret-0123456789";
const ASHA: &str = "demo-analyst-1";
const RAVI: &str = "demo-analyst-2";
const MEERA: &str = "demo-supervisor";

fn app() -> (axum::Router, rusetect_server::AppRef) {
    std::env::set_var("RUSETECT_RATE_LIMIT", "1000000");
    std::env::set_var("RUSETECT_WEBHOOK_SECRET", SECRET);
    let shared = rusetect_server::new_shared();
    (rusetect_server::build_app(shared.clone()), shared)
}

async fn call(app: &axum::Router, req: Request<Body>) -> (StatusCode, Value) {
    let r = app.clone().oneshot(req).await.unwrap();
    let st = r.status();
    (st, serde_json::from_slice(&r.into_body().collect().await.unwrap().to_bytes()).unwrap_or(Value::Null))
}

/// POST a signed payment event; returns (status, stored result).
async fn send(app: &axum::Router, body: &Value) -> (StatusCode, Value) {
    let (raw, ts) = (body.to_string(), chrono::Utc::now().timestamp());
    let mut m = Hmac::<sha2::Sha256>::new_from_slice(SECRET.as_bytes()).unwrap();
    m.update(format!("{ts}.{raw}").as_bytes());
    let sig = format!("v1={}", hex::encode(m.finalize().into_bytes()));
    call(app, Request::builder().method("POST").uri("/api/v2/events/payment").header("content-type", "application/json")
        .header("x-rusetect-timestamp", ts.to_string()).header("x-rusetect-signature", sig).body(Body::from(raw)).unwrap()).await
}

async fn get(app: &axum::Router, path: &str) -> (StatusCode, Value) {
    call(app, Request::builder().uri(path).body(Body::empty()).unwrap()).await
}

/// GET that must succeed.
async fn ok(app: &axum::Router, path: &str) -> Value {
    let (s, v) = get(app, path).await;
    assert_eq!(s, StatusCode::OK, "{path}: {v}");
    v
}

fn nonce() -> String {
    format!("{:032x}", rand::random::<u128>())
}

/// POST as an analyst with a fresh nonce (`key` None sends no key).
async fn act(app: &axum::Router, path: &str, key: Option<&str>, body: Value) -> (StatusCode, Value) {
    let mut b = Request::builder().method("POST").uri(path).header("content-type", "application/json").header("x-nonce", nonce());
    if let Some(k) = key {
        b = b.header("x-analyst-key", k);
    }
    call(app, b.body(Body::from(body.to_string())).unwrap()).await
}

fn pay(id: &str, ts: &str, from: (&str, &str), to: (&str, &str), amount: f64) -> Value {
    json!({"event_id": id, "timestamp": ts, "from_bank": from.0, "from_account": from.1, "to_bank": to.0, "to_account": to.1,
           "amount_paid": amount, "payment_currency": "US Dollar", "payment_format": "ACH"})
}

fn at(minute: i64) -> String {
    format!("2022-09-09 {:02}:{:02}", 10 + minute / 60, minute % 60)
}

/// The motif names in a stored result's reasons.
fn motifs(reasons: &Value) -> BTreeSet<String> {
    reasons.as_array().into_iter().flatten().filter_map(|r| r["motifs"].as_array()).flatten().filter_map(|m| m.as_str().map(String::from)).collect()
}

/// A payment the account-takeover check blocks (a failed challenge on a never-seen device): opens an alert.
async fn blocked(app: &axum::Router, tag: &str) -> Value {
    let auth = |ts: f64, amount: f64| json!({"user_id": format!("user-{tag}"), "ts": ts,
        "device": {"device_id": "attacker-phone", "platform": "android", "sim_change_hours": 1.0},
        "network": {"ip": "49.229.31.106", "city": "Mumbai", "lat": 19.07, "lon": 72.87},
        "behaviour": {"time_to_submit_s": 20, "input_events": 30}, "transaction": {"amount": amount, "payee_id": "mule-9"}});
    let mut e1 = pay(&format!("{tag}-1"), "2022-09-09 10:00", ("070", &format!("{tag}A")), ("011", "LM1"), 9000.0);
    e1["auth"] = auth(1.767e9, 60000.0);
    let (s, r1) = send(app, &e1).await;
    assert_eq!((s, r1["pramaan"]["decision"].as_str()), (StatusCode::OK, Some("challenge")), "{r1}");
    let (_, v) = call(app, Request::builder().method("POST").uri("/api/v2/challenge/verify").header("content-type", "application/json")
        .body(Body::from(json!({"decision_id": r1["pramaan"]["decision_id"], "passed": false}).to_string())).unwrap()).await;
    assert_eq!(v["decision"], "block", "{v}");
    let mut e2 = pay(&format!("{tag}-2"), "2022-09-09 10:05", ("070", &format!("{tag}A")), ("011", "LM1"), 9000.0);
    e2["auth"] = auth(1.767e9 + 60.0, 100.0);
    let (s, r2) = send(app, &e2).await;
    assert_eq!((s, r2["action"].as_str()), (StatusCode::OK, Some("block")), "{r2}");
    r2
}

/// The mule fixture through the webhook: twelve victims pay one mule, which forwards 98% to a collector a minute
/// later. Returns the stored results in order.
async fn mule_network(app: &axum::Router) -> Vec<Value> {
    let f: Value = serde_json::from_str(&std::fs::read_to_string(rusetect::paths::repo_root().join("fixtures/mule_replay.json")).unwrap()).unwrap();
    let (mule, coll) = (("0200", "MULE01"), ("0300", "COLL01"));
    let mut out = Vec::new();
    for (i, amt) in f["deposits_inr"].as_array().unwrap().iter().enumerate() {
        let (amt, t) = (amt.as_f64().unwrap(), i as i64 * 7);
        let (bank, victim) = (format!("01{:02}", i + 1), format!("V{:02}", i + 1));
        for mut e in [pay(&format!("dep-{i}"), &at(t), (&bank, &victim), mule, amt),
                      pay(&format!("fwd-{i}"), &at(t + 1), mule, coll, (amt * 98.0).round() / 100.0)] {
            e["payment_currency"] = json!("Rupee");
            let (s, r) = send(app, &e).await;
            assert_eq!(s, StatusCode::OK, "{r}");
            out.push(r);
        }
    }
    out
}

#[tokio::test]
async fn a_mule_network_is_searchable_explained_and_drawn_from_stored_payments() {
    let (app, _) = app();
    let sent = mule_network(&app).await;
    let flagged: Vec<&Value> = sent.iter().filter(|r| r["action"] != "allow").collect();
    assert!(!flagged.is_empty() && flagged.len() < sent.len(), "the fixture has approved and flagged payments");

    // search: every stored payment of the mule, newest first, each as the webhook answered it (no second model)
    let all = ok(&app, "/api/v2/transactions?account=MULE01&limit=200").await;
    assert_eq!((all["total"].as_u64(), all["data"].as_str()), (Some(24), Some("live")), "{}", all["window"]);
    let rows = all["transactions"].as_array().unwrap();
    assert!(rows.windows(2).all(|w| w[0]["timestamp"].as_str() >= w[1]["timestamp"].as_str()), "newest payment first");
    for r in &sent {
        let row = rows.iter().find(|x| x["id"] == r["event_id"]).unwrap();
        assert_eq!((&row["band"], &row["risk"], &row["alert_id"], &row["reasons"]), (&r["action"], &r["risk"], &r["alert_id"], &r["reasons"]));
        assert_eq!((row["source"].as_str(), row["demo"].as_bool()), (Some("live"), Some(false)));
    }
    // the webhook's alerts are a bank's events: never demo, even on a demo-mode instance
    let al = ok(&app, "/api/v2/alerts?q=MULE01&limit=200").await;
    assert!(al["total"].as_u64().unwrap() >= 1 && al["demo"] == 0 && al["alerts"].as_array().unwrap().iter().all(|a| a["demo"] == false), "{al}");
    // filters
    let n = |v: Value| v["total"].as_u64().unwrap();
    assert_eq!(n(ok(&app, "/api/v2/transactions?account=COLL01&bank=0300").await), 12);
    assert_eq!(n(ok(&app, "/api/v2/transactions?account=COLL01&bank=9999").await), 0);
    let by_band: u64 = {
        let mut t = 0;
        for b in ["allow", "verify", "hold", "block"] {
            t += n(ok(&app, &format!("/api/v2/transactions?band={b}")).await);
        }
        t
    };
    assert_eq!(by_band, 24, "every stored decision is in exactly one band");
    assert_eq!(n(ok(&app, "/api/v2/transactions?band=allow").await) as usize, sent.len() - flagged.len());
    let pass = sent.iter().filter(|r| motifs(&r["reasons"]).contains("pass_through")).count();
    assert!(pass > 0, "the forwards are pass-through payments");
    assert_eq!(n(ok(&app, "/api/v2/transactions?typology=pass_through").await) as usize, pass);
    assert_eq!(n(ok(&app, "/api/v2/transactions?min_amount=19000&max_amount=19500").await), 2, "the 19,500 deposit and its 19,110 forward");
    assert_eq!(n(ok(&app, "/api/v2/transactions?from=2022-09-09%2010:00&to=2022-09-09%2010:08").await), 4);
    assert_eq!(n(ok(&app, "/api/v2/transactions?from=2022-09-10").await), 0);
    assert_eq!(n(ok(&app, "/api/v2/transactions?source=demo_stream").await), 0);
    let page = ok(&app, "/api/v2/transactions?limit=5&offset=20").await;
    assert_eq!((page["transactions"].as_array().unwrap().len(), n(page)), (4, 24));

    // detail of a flagged forward: its reason codes, alert, payee history, related accounts and ledger timeline
    let f = flagged.iter().find(|r| motifs(&r["reasons"]).contains("pass_through")).unwrap();
    let d = ok(&app, &format!("/api/v2/transactions/{}", f["event_id"].as_str().unwrap())).await;
    assert_eq!((&d["risk"]["score"], &d["risk"]["band"]), (&f["risk"], &f["action"]));
    assert!(d["risk"]["thresholds"]["hold"].is_f64() && d["model"]["service_version"].is_string() && d["model"]["ruleset"].is_string());
    assert_eq!(d["explanations"]["available"], true);
    assert_eq!(motifs(&d["explanations"]["motif_reasons"]), motifs(&f["reasons"]));
    assert!(d["explanations"]["model_reasons"].as_array().unwrap().iter().all(|r| r["impact"].is_number() && r.get("motifs").is_none()));
    assert_eq!((&d["alert"]["alert_id"], d["alert"]["status"].as_str()), (&f["alert_id"], Some("open")));
    let h = &d["beneficiary_history"];
    assert_eq!((h["payee"].as_str(), h["payments"].as_u64(), h["distinct_payers"].as_u64()), (Some("0300:COLL01"), Some(12), Some(1)));
    assert_eq!(h["flagged_payments"].as_u64().unwrap() as usize, flagged.iter().filter(|r| r["payment"]["to_account"] == "COLL01").count());
    let rel = d["related_accounts"].as_array().unwrap();
    assert_eq!(rel.iter().filter(|r| r["relation"] == "paid the payer").count(), 12, "the twelve victims: {rel:?}");
    let kinds: Vec<&str> = d["timeline"].as_array().unwrap().iter().filter_map(|e| e["type"].as_str()).collect();
    assert!(kinds.contains(&"alert_opened") && kinds.contains(&"live_decision"), "{kinds:?}");
    // an approved payment has a detail too, and says why it has no reasons
    let a = sent.iter().find(|r| r["action"] == "allow").unwrap();
    let d = ok(&app, &format!("/api/v2/transactions/{}", a["event_id"].as_str().unwrap())).await;
    assert_eq!((d["explanations"]["available"].as_bool(), d["alert"].is_null()), (Some(false), true));

    // graph: victims -> mule -> collector, one directed edge per stored payment with its evidence
    let g = ok(&app, "/api/v2/graph?account=MULE01").await;
    assert_eq!(g["counts"], json!({"accounts": 14, "banks": 14, "payments": 24, "payments_between_these_accounts": 24}), "{}", g["counts"]);
    assert_eq!(g["truncated"], false);
    let (nodes, edges) = (g["nodes"].as_array().unwrap(), g["edges"].as_array().unwrap());
    let node = |id: &str| nodes.iter().find(|x| x["id"] == id).unwrap_or_else(|| panic!("no node {id}"));
    let mule = node("account:0200:MULE01");
    assert_eq!((mule["seed"].as_bool(), mule["sent"].as_u64(), mule["received"].as_u64()), (Some(true), Some(12), Some(12)));
    assert_eq!(mule["flagged_payments"].as_u64().unwrap() as usize, flagged.len());
    assert!(mule["indicators"].as_array().unwrap().iter().any(|i| i["kind"] == "motif" && i["name"] == "pass_through" && !i["evidence"].as_array().unwrap().is_empty()));
    assert!(node("account:0101:V01")["indicators"].as_array().unwrap().is_empty(), "a victim of approved deposits carries no indicator");
    assert!(g["disclaimer"].as_str().unwrap().contains("not a finding") && g["not_available"]["devices"].is_string());
    for r in &sent {
        let e = edges.iter().find(|e| e["type"] == "payment" && e["id"] == r["event_id"]).unwrap();
        let p = &r["payment"];
        assert_eq!(e["from"], json!(format!("account:{}:{}", p["from_bank"].as_str().unwrap(), p["from_account"].as_str().unwrap())));
        assert_eq!(e["to"], json!(format!("account:{}:{}", p["to_bank"].as_str().unwrap(), p["to_account"].as_str().unwrap())));
        assert_eq!((&e["amount"], &e["timestamp"], &e["band"]), (&p["amount_paid"], &p["timestamp"], &r["action"]));
        assert_eq!((&e["evidence"]["event_id"], &e["evidence"]["ledger"]["hash"], &e["evidence"]["alert_id"]), (&r["event_id"], &r["ledger"]["hash"], &r["alert_id"]));
    }
    assert_eq!(edges.iter().filter(|e| e["type"] == "held_at").count(), 14);
    assert!(nodes.iter().any(|x| x["type"] == "bank" && x["id"] == "bank:0300"));
    // depth, time range and the size bound
    let accounts = |g: &Value| g["counts"]["accounts"].as_u64().unwrap();
    assert_eq!(accounts(&ok(&app, "/api/v2/graph?account=V01&depth=1").await), 2);
    assert_eq!(accounts(&ok(&app, "/api/v2/graph?account=V01&depth=2").await), 14, "the mule's other counterparties");
    let early = ok(&app, "/api/v2/graph?account=MULE01&to=2022-09-09%2010:08").await;
    assert_eq!((accounts(&early), early["counts"]["payments"].as_u64()), (4, Some(4)));
    let small = ok(&app, "/api/v2/graph?account=MULE01&limit=5").await;
    assert_eq!((accounts(&small), small["truncated"].as_bool()), (5, Some(true)));

    // dashboard: the same decisions, each number with its source
    let d = ok(&app, "/api/v2/dashboard").await;
    assert_eq!((d["data"]["kind"].as_str(), d["payments"]["live_events"]["value"].as_u64(), d["payments"]["scored"]["value"].as_u64()), (Some("live"), Some(24), Some(24)));
    assert_eq!(d["payments"]["flagged"]["value"].as_u64().unwrap() as usize, flagged.len());
    assert_eq!(d["alerts"]["total"]["value"].as_u64().unwrap() as usize, flagged.len());
    assert_eq!((&d["alerts"]["open"]["value"], d["alerts"]["closed"]["value"].as_u64()), (&d["alerts"]["total"]["value"], Some(0)));
    assert_eq!(d["entities"]["flagged_payees"]["value"].as_u64().unwrap() as usize,
               flagged.iter().map(|r| r["payment"]["to_account"].as_str().unwrap()).collect::<BTreeSet<_>>().len());
    assert!(d["typologies"]["value"].as_array().unwrap().iter().any(|t| t["name"] == "pass_through" && t["count"].as_u64() == Some(pass as u64)));
    let series = d["activity"]["series"].as_array().unwrap();
    assert_eq!(series.iter().map(|b| b["total"].as_u64().unwrap()).sum::<u64>(), 24);
    assert!(series.iter().all(|b| b["total"].as_u64() == Some(["allow", "verify", "hold", "block"].iter().map(|k| b[*k].as_u64().unwrap()).sum())));
    assert_eq!(ok(&app, "/api/v2/dashboard?bucket=day").await["activity"]["series"].as_array().unwrap().len(), 1);
    for (group, v) in d.as_object().unwrap().iter().filter(|(k, _)| ["payments", "alerts", "investigations", "entities"].contains(&k.as_str())) {
        for (k, m) in v.as_object().unwrap() {
            assert!(m.get("value").is_some() && m["source"].as_str().is_some_and(|s| !s.is_empty()), "{group}.{k} names its source");
        }
    }
    assert_eq!((d["health"]["engine_loaded"].as_bool(), d["health"]["automated_blocking"].as_bool()), (Some(true), Some(false)));
    assert!(d["drift"]["status"].is_string() && d["detection"]["confirmed_fraud"].is_null());
}

#[tokio::test]
async fn normal_suspicious_duplicate_and_malformed_inputs() {
    let (app, _) = app();
    // a normal payment is stored as allowed; sending it again (same event_id) stores nothing new
    let e = pay("n-1", "2022-09-09 10:00", ("070", "NORM1"), ("011", "SHOP1"), 42.5);
    let (_, first) = send(&app, &e).await;
    assert_eq!(first["action"], "allow", "{first}");
    for _ in 0..3 {
        assert_eq!(send(&app, &e).await.1, first, "a duplicate replays the stored result");
    }
    let t = ok(&app, "/api/v2/transactions?account=NORM1").await;
    assert_eq!((t["total"].as_u64(), t["transactions"][0]["band"].as_str()), (Some(1), Some("allow")));
    assert_eq!(ok(&app, "/api/v2/graph?account=NORM1").await["counts"]["payments"], 1, "one edge, not four");
    // a suspicious payment (blocked by the account-takeover check) is found by band and carries its alert
    let b = blocked(&app, "sus").await;
    let t = ok(&app, "/api/v2/transactions?band=block").await;
    assert_eq!((t["total"].as_u64(), &t["transactions"][0]["id"], &t["transactions"][0]["alert_id"]), (Some(1), &b["event_id"], &b["alert_id"]));
    assert_eq!(t["transactions"][0]["alert_status"], "open", "flagged for a person: the engine blocks nothing on its own");
    assert_eq!(ok(&app, "/api/v2/transactions/sus-2").await["transaction"]["pramaan"]["decision"], "block");
    // malformed and missing fields: the event is refused and nothing is stored
    let before = ok(&app, "/api/v2/transactions").await["total"].clone();
    let mut bad = pay("bad-1", "2022-09-09 10:00", ("070", "X"), ("011", "Y"), 10.0);
    bad.as_object_mut().unwrap().remove("to_account");
    assert_eq!(send(&app, &bad).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(send(&app, &pay("bad-2", "yesterday", ("070", "X"), ("011", "Y"), 10.0)).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(send(&app, &pay("bad-3", "2022-09-09 10:00", ("070", "X"), ("011", "Y"), -5.0)).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(ok(&app, "/api/v2/transactions").await["total"], before);
    assert_eq!(get(&app, "/api/v2/transactions/bad-1").await.0, StatusCode::NOT_FOUND);
    // malformed queries are refused, not guessed
    let long = "a".repeat(200);
    for q in ["/api/v2/transactions?band=maybe", "/api/v2/transactions?source=prod", "/api/v2/transactions?min_amount=lots",
              "/api/v2/transactions?min_amount=-1", "/api/v2/transactions?min_amount=9&max_amount=1", "/api/v2/transactions?max_amount=inf",
              "/api/v2/transactions?from=soon", "/api/v2/transactions?from=2022-09-10&to=2022-09-09", "/api/v2/transactions?limit=many",
              &format!("/api/v2/transactions?account={long}"), "/api/v2/graph", "/api/v2/graph?account=NORM1&depth=9", "/api/v2/graph?account=NORM1&depth=x",
              "/api/v2/graph?account=NORM1&from=never", "/api/v2/dashboard?bucket=minute", &format!("/api/v2/transactions/{long}")] {
        assert_eq!(get(&app, q).await.0, StatusCode::UNPROCESSABLE_ENTITY, "{q}");
    }
    assert_eq!(get(&app, "/api/v2/graph?account=NOBODY").await.0, StatusCode::NOT_FOUND);
    assert_eq!(get(&app, "/api/v2/transactions/unknown-id").await.0, StatusCode::NOT_FOUND);
    // bounds: a huge limit is clamped
    assert_eq!(ok(&app, "/api/v2/transactions?limit=100000").await["limit"], 200);
}

#[tokio::test]
async fn split_payments_and_rapid_dispersal_show_as_stored() {
    let (app, _) = app();
    // splitting: one payer sends 9,000 to one payee as four parts of 2,250, half an hour apart
    let mut parts = Vec::new();
    for i in 0..4 {
        parts.push(send(&app, &pay(&format!("split-{i}"), &at(i * 30), ("070", "SPL1"), ("011", "SPLD"), 2250.0)).await.1);
    }
    let d = ok(&app, "/api/v2/transactions/split-3").await;
    let h = &d["beneficiary_history"];
    assert_eq!((h["payments"].as_u64(), h["distinct_payers"].as_u64(), h["total_amount"].as_f64()), (Some(4), Some(1), Some(9000.0)), "{h}");
    assert_eq!((h["first_seen"].as_str(), h["last_seen"].as_str()), (Some("2022-09-09 10:00"), Some("2022-09-09 11:30")));
    let g = ok(&app, "/api/v2/graph?account=SPL1").await;
    assert_eq!(g["counts"], json!({"accounts": 2, "banks": 2, "payments": 4, "payments_between_these_accounts": 4}), "four parallel edges");
    let rows = ok(&app, "/api/v2/transactions?account=SPLD").await;
    for p in &parts {
        let row = rows["transactions"].as_array().unwrap().iter().find(|x| x["id"] == p["event_id"]).unwrap();
        assert_eq!((&row["band"], &row["risk"]), (&p["action"], &p["risk"]), "the search shows the engine's own decision for each part");
    }

    // rapid dispersal: one account pays six new beneficiaries within six minutes
    let mut outs = Vec::new();
    for i in 0..6 {
        outs.push(send(&app, &pay(&format!("disp-{i}"), &at(120 + i), ("070", "DISP1"), ("011", &format!("NEWB{i}")), 4000.0 + i as f64)).await.1);
    }
    let g = ok(&app, "/api/v2/graph?account=DISP1").await;
    assert_eq!((g["counts"]["accounts"].as_u64(), g["counts"]["payments"].as_u64()), (Some(7), Some(6)));
    let payer = g["nodes"].as_array().unwrap().iter().find(|n| n["id"] == "account:070:DISP1").unwrap();
    assert_eq!((payer["sent"].as_u64(), payer["received"].as_u64()), (Some(6), Some(0)));
    assert_eq!(payer["flagged_payments"].as_u64().unwrap() as usize, outs.iter().filter(|r| r["action"] != "allow").count());
    // the payer's motif indicators are exactly the motifs the engine named on its flagged payments: never invented
    let named: BTreeSet<String> = outs.iter().flat_map(|r| motifs(&r["reasons"])).collect();
    let shown: BTreeSet<String> = payer["indicators"].as_array().unwrap().iter().filter(|i| i["kind"] == "motif").map(|i| i["name"].as_str().unwrap().to_string()).collect();
    assert_eq!(shown, named);
    for r in outs.iter().filter(|r| r["action"] != "allow" && r["seq"].as_u64() >= outs[3]["seq"].as_u64()) {
        assert!(motifs(&r["reasons"]).contains("fan_out"), "a flagged payment to a fourth or later payee names the fan-out: {r}");
    }
    let related = ok(&app, "/api/v2/transactions/disp-5").await["related_accounts"].as_array().unwrap().len();
    assert_eq!(related, 5, "the five other new beneficiaries");
}

#[tokio::test]
async fn triage_and_investigations_enforce_roles_four_eyes_and_the_ledger() {
    let (app, _) = app();
    let (a1, a2) = (blocked(&app, "wf1").await, blocked(&app, "wf2").await);
    let (a1, a2) = (a1["alert_id"].as_str().unwrap().to_string(), a2["alert_id"].as_str().unwrap().to_string());
    let triage = format!("/api/v2/alerts/{a1}/triage");
    // auth failures: no key, unknown key, a read-only role, no nonce, a replayed nonce
    assert_eq!(act(&app, &triage, None, json!({"priority": "high"})).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(act(&app, &triage, Some("not-a-key"), json!({"priority": "high"})).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(act(&app, &triage, Some("demo-viewer"), json!({"priority": "high"})).await.0, StatusCode::FORBIDDEN);
    assert_eq!(act(&app, "/api/v2/investigations", Some("demo-viewer"), json!({"title": "x", "alert_ids": [a1]})).await.0, StatusCode::FORBIDDEN);
    assert_eq!(act(&app, "/api/v2/investigations", None, json!({"title": "x", "alert_ids": [a1]})).await.0, StatusCode::UNAUTHORIZED);
    let with_nonce = |n: Option<&str>| {
        let mut b = Request::builder().method("POST").uri(&triage).header("content-type", "application/json").header("x-analyst-key", ASHA);
        if let Some(n) = n {
            b = b.header("x-nonce", n);
        }
        b.body(Body::from(json!({"state": "in_progress"}).to_string())).unwrap()
    };
    assert_eq!(call(&app, with_nonce(None)).await.0, StatusCode::CONFLICT);
    assert_eq!(call(&app, with_nonce(Some("nonce-triage-0001"))).await.0, StatusCode::OK);
    assert_eq!(call(&app, with_nonce(Some("nonce-triage-0001"))).await.0, StatusCode::CONFLICT, "a replay is refused");
    // malformed bodies
    for bad in [json!({}), json!({"priority": "urgent"}), json!({"state": "blocked"}), json!({"sla_hours": 0}), json!({"sla_hours": 99999}),
                json!({"assignee": 7}), json!({"comment": "x".repeat(1001)}), json!([1])] {
        assert_eq!(act(&app, &triage, Some(ASHA), bad.clone()).await.0, StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
    }
    assert_eq!(act(&app, "/api/v2/alerts/NOPE/triage", Some(ASHA), json!({"priority": "high"})).await.0, StatusCode::NOT_FOUND);
    // assignment: an analyst takes work; giving it away needs a supervisor; the assignee must be a known analyst
    assert_eq!(act(&app, &triage, Some(ASHA), json!({"assignee": "analyst.ravi"})).await.0, StatusCode::FORBIDDEN);
    assert_eq!(act(&app, &triage, Some(MEERA), json!({"assignee": "viewer.ops"})).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(act(&app, &triage, Some(MEERA), json!({"assignee": "nobody"})).await.0, StatusCode::BAD_REQUEST);
    let (s, a) = act(&app, &triage, Some(ASHA), json!({"assignee": "analyst.asha", "priority": "critical", "comment": "payee matches the earlier ring"})).await;
    assert_eq!(s, StatusCode::OK, "{a}");
    let w = &a["workflow"];
    assert_eq!((w["assignee"].as_str(), w["priority"].as_str(), w["state"].as_str()), (Some("analyst.asha"), Some("critical"), Some("in_progress")));
    let due = w["sla_due"].as_str().unwrap().to_string();
    assert!(due > rusetect::pyfmt::utc_now_z(), "critical: due in four hours");
    assert_eq!(a["status"], "open", "triage never decides the payment");
    let (_, a) = act(&app, &triage, Some(MEERA), json!({"assignee": "analyst.ravi", "priority": "high"})).await;
    assert_eq!((a["workflow"]["assignee"].as_str(), a["workflow"]["sla_due"].as_str()), (Some("analyst.ravi"), Some(due.as_str())), "a set SLA is kept");
    assert!(a["actions"].as_array().unwrap().iter().any(|x| x["action"] == "triage" && x["note"] == "payee matches the earlier ring"));

    // open an investigation from both alerts
    let open = |ids: Value| json!({"title": "LM1 payee", "alert_ids": ids, "priority": "high"});
    for bad in [json!({"alert_ids": [a1]}), json!({"title": "", "alert_ids": [a1]}), json!({"title": "t", "alert_ids": []}), json!({"title": "t"}),
                json!({"title": "t", "alert_ids": [a1, a1]}), json!({"title": "t", "alert_ids": "A1"}), json!({"title": "t", "alert_ids": [a1], "priority": "p0"})] {
        assert_eq!(act(&app, "/api/v2/investigations", Some(ASHA), bad.clone()).await.0, StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
    }
    let (s, e) = act(&app, "/api/v2/investigations", Some(ASHA), open(json!([a1, "NOPE"]))).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{e}");
    let (s, c) = act(&app, "/api/v2/investigations", Some(ASHA), open(json!([a1, a2]))).await;
    assert_eq!(s, StatusCode::OK, "the failed attempt left {a1} free: {c}");
    let id = c["case_id"].as_str().unwrap().to_string();
    assert_eq!((c["status"].as_str(), c["created_by"].as_str(), c["sla_breached"].as_bool()), (Some("open"), Some("analyst.asha"), Some(false)));
    assert!(c["sla_due"].as_str().unwrap() > rusetect::pyfmt::utc_now_z().as_str());
    let (s, e) = act(&app, "/api/v2/investigations", Some(RAVI), open(json!([a2]))).await;
    assert_eq!(s, StatusCode::CONFLICT, "an alert is in one investigation: {e}");
    let actions = format!("/api/v2/investigations/{id}/actions");
    // malformed and unknown
    for bad in [json!({}), json!({"action": "delete"}), json!({"action": "assign"}), json!({"action": "comment"}), json!({"action": "comment", "text": "  "}),
                json!({"action": "status", "status": "closed"}), json!({"action": "priority", "priority": "p1"}), json!({"action": "sla"}),
                json!({"action": "add_alert"}), json!({"action": "request_close"})] {
        assert_eq!(act(&app, &actions, Some(ASHA), bad.clone()).await.0, StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
    }
    assert_eq!(act(&app, "/api/v2/investigations/INV-99999/actions", Some(ASHA), json!({"action": "comment", "text": "x"})).await.0, StatusCode::NOT_FOUND);
    assert_eq!(act(&app, &actions, Some("demo-viewer"), json!({"action": "comment", "text": "x"})).await.0, StatusCode::FORBIDDEN);
    assert_eq!(act(&app, &actions, None, json!({"action": "comment", "text": "x"})).await.0, StatusCode::UNAUTHORIZED);
    // the work
    let step = |key: &'static str, body: Value| { let (app, actions) = (app.clone(), actions.clone()); async move { act(&app, &actions, Some(key), body).await } };
    assert_eq!(step(ASHA, json!({"action": "assign", "assignee": "analyst.ravi"})).await.0, StatusCode::FORBIDDEN);
    assert_eq!(step(MEERA, json!({"action": "assign", "assignee": "analyst.ravi"})).await.1["assignee"], "analyst.ravi");
    assert_eq!(step(RAVI, json!({"action": "status", "status": "in_progress"})).await.1["status"], "in_progress");
    assert_eq!(step(RAVI, json!({"action": "priority", "priority": "critical"})).await.1["priority"], "critical");
    assert_eq!(step(RAVI, json!({"action": "comment", "text": "both payers were phoned"})).await.1["comments"][0]["analyst"], "analyst.ravi");
    assert_eq!(step(RAVI, json!({"action": "add_alert", "alert_id": a1})).await.0, StatusCode::CONFLICT, "already in it");
    assert_eq!(step(MEERA, json!({"action": "sla", "sla_hours": 0.0001})).await.0, StatusCode::OK);
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    assert_eq!(ok(&app, &format!("/api/v2/investigations/{id}")).await["investigation"]["sla_breached"], true, "past its due time");
    assert_eq!(ok(&app, "/api/v2/dashboard").await["investigations"]["sla_breached"]["value"], 1);
    // maker-checker closure: the requester cannot approve; a second analyst can; a rejection restores the status
    assert_eq!(step(RAVI, json!({"action": "approve_close"})).await.0, StatusCode::CONFLICT, "nothing to approve yet");
    assert_eq!(step(RAVI, json!({"action": "request_close", "text": "customers confirmed the payments"})).await.1["status"], "pending_closure");
    assert_eq!(step(RAVI, json!({"action": "approve_close"})).await.0, StatusCode::BAD_REQUEST, "four-eyes");
    assert_eq!(step(RAVI, json!({"action": "status", "status": "open"})).await.0, StatusCode::CONFLICT);
    assert_eq!(step(ASHA, json!({"action": "reject_close"})).await.1["status"], "in_progress");
    assert_eq!(step(RAVI, json!({"action": "request_close", "text": "customers confirmed again"})).await.0, StatusCode::OK);
    let (s, closed) = step(ASHA, json!({"action": "approve_close"})).await;
    assert_eq!((s, closed["status"].as_str(), closed["closure"]["approved_by"].as_str(), closed["sla_breached"].as_bool()),
               (StatusCode::OK, Some("closed"), Some("analyst.asha"), Some(false)), "{closed}");
    assert_eq!(step(RAVI, json!({"action": "priority", "priority": "low"})).await.0, StatusCode::CONFLICT, "closed");
    assert_eq!(step(RAVI, json!({"action": "comment", "text": "filed"})).await.0, StatusCode::OK, "comments stay open");
    assert_eq!(step(RAVI, json!({"action": "reopen"})).await.0, StatusCode::FORBIDDEN);
    assert_eq!(step(MEERA, json!({"action": "reopen"})).await.1["status"], "open");

    // one screen: the investigation, its alerts as they are, and its ledger entries
    let d = ok(&app, &format!("/api/v2/investigations/{id}")).await;
    let seen: Vec<&str> = d["investigation"]["history"].as_array().unwrap().iter().filter_map(|h| h["action"].as_str()).collect();
    assert_eq!(seen, ["opened", "assign", "status", "priority", "comment", "sla", "request_close", "reject_close", "request_close", "approve_close", "comment", "reopen"]);
    let alerts = d["alerts"].as_array().unwrap();
    assert_eq!(alerts.len(), 2);
    assert!(alerts.iter().all(|a| a["status"] == "open" && a["payment"]["to_account"] == "LM1"), "closing a case decides no payment: {alerts:?}");
    assert_eq!(alerts[0]["workflow"]["assignee"], "analyst.ravi");
    assert_eq!(d["audit_entries"].as_array().unwrap().len(), seen.len(), "every action is in the ledger");
    let audit = ok(&app, "/api/v2/audit").await;
    assert_eq!(audit["chain_valid"], true);
    let entries = audit["entries"].as_array().unwrap();
    assert_eq!(entries.iter().filter(|e| e["type"] == "alert_triage" && e["alert_id"] == a1.as_str()).count(), 3);
    assert!(entries.iter().all(|e| !e.to_string().contains("phoned") && !e.to_string().contains("earlier ring")), "comment texts stay out of the ledger");
    for h in d["investigation"]["history"].as_array().unwrap() {
        assert!(entries.iter().any(|e| e["seq"] == h["ledger_seq"] && e["hash"] == h["ledger_hash"] && e["type"] == "case_event"));
    }
    assert_eq!(ok(&app, &format!("/api/v2/alerts/{a1}/evidence")).await["alert"]["case_id"], id.as_str(), "the evidence pack names the investigation");
    // listing and filters
    let l = ok(&app, "/api/v2/investigations?status=open&assignee=analyst.ravi&priority=critical").await;
    assert_eq!((l["total"].as_u64(), &l["investigations"][0]["case_id"]), (Some(1), &json!(id)));
    assert_eq!(ok(&app, "/api/v2/investigations?status=closed").await["total"], 0);
    assert_eq!(get(&app, "/api/v2/investigations?status=done").await.0, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(get(&app, "/api/v2/investigations/INV-99999").await.0, StatusCode::NOT_FOUND);
    let dash = ok(&app, "/api/v2/dashboard").await;
    assert_eq!((dash["investigations"]["total"]["value"].as_u64(), dash["alerts"]["by_priority"]["value"]["high"].as_u64()), (Some(1), Some(1)));
    assert_eq!(dash["alerts"]["unassigned"]["value"].as_u64().unwrap(), dash["alerts"]["total"]["value"].as_u64().unwrap() - 1);
}

#[tokio::test]
async fn the_narrative_draft_is_typed_sourced_deterministic_and_decides_nothing() {
    let (app, _) = app();
    let sent = mule_network(&app).await;
    let alerts: Vec<String> = sent.iter().filter_map(|r| r["alert_id"].as_str().map(String::from)).take(6).collect();
    assert!(alerts.len() >= 3, "the mule fixture opens alerts");
    let (s, c) = act(&app, "/api/v2/investigations", Some(ASHA), json!({"title": "MULE01 pass-through", "alert_ids": alerts})).await;
    assert_eq!(s, StatusCode::OK, "{c}");
    let case = c["case_id"].as_str().unwrap().to_string();
    let path = format!("/api/v2/investigations/{case}/narrative");
    let n = ok(&app, &path).await;
    let st = n["statements"].as_array().unwrap();
    assert_eq!((n["requires_human_review"].as_bool(), n["generated_by"].as_str(), n["llm_rephrase"]["enabled"].as_bool()), (Some(true), Some("template v1"), Some(false)));
    // no analyst wrote anything yet: no analyst conclusion, and an open question says so
    assert!(st.iter().all(|s| s["type"] != "analyst_conclusion"));
    assert!(n["open_questions"].as_array().unwrap().iter().any(|q| q["template_id"] == "question.no_conclusion"));
    act(&app, &format!("/api/v2/investigations/{case}/actions"), Some(ASHA), json!({"action": "comment", "text": "MULE01 forwards each deposit within a minute"})).await;
    let (audit_before, alerts_before) = (ok(&app, "/api/v2/audit").await, ok(&app, "/api/v2/alerts").await);
    let n = ok(&app, &path).await;
    assert_eq!(n, ok(&app, &path).await, "the same stored data gives the same draft");
    assert_eq!((ok(&app, "/api/v2/audit").await, ok(&app, "/api/v2/alerts").await), (audit_before.clone(), alerts_before.clone()), "reading the draft changes nothing");
    let st = n["statements"].as_array().unwrap();
    let types = ["observed_fact", "model_output", "rule_finding", "interpretation", "analyst_conclusion"];
    let has = |t: &str, tid: &str, text: &str| st.iter().any(|s| s["type"] == t && s["template_id"] == tid && s["text"].as_str().unwrap().contains(text));
    // the mule fixture: motif model output, the pass-through rule, a conservative interpretation, the analyst's comment
    assert!(has("model_output", "model.motif", "pass-through"), "{st:?}");
    assert!(has("rule_finding", "rule.context", "account graph: rapid_pass_through"), "{st:?}");
    assert!(has("interpretation", "interp.pass_through", "consistent with"), "{st:?}");
    assert!(has("observed_fact", "fact.payment", "from 0200/MULE01 to 0300/COLL01"), "{st:?}");
    let concl: Vec<&Value> = st.iter().filter(|s| s["type"] == "analyst_conclusion").collect();
    assert_eq!(concl.len(), 1, "only what the analyst wrote");
    assert!(concl[0]["text"].as_str().unwrap().contains("within a minute"));
    // every statement, check and question has a source, and every source resolves to stored data
    let audit = ok(&app, "/api/v2/audit").await;
    let seqs: BTreeSet<u64> = audit["entries"].as_array().unwrap().iter().filter_map(|e| e["seq"].as_u64()).collect();
    let inv = ok(&app, &format!("/api/v2/investigations/{case}")).await;
    let known: BTreeSet<String> = alerts_before.as_array().unwrap().iter().filter_map(|a| a["alert_id"].as_str().map(String::from)).collect();
    let items = st.iter().chain(n["suggested_checks"].as_array().unwrap()).chain(n["open_questions"].as_array().unwrap());
    let mut checked = 0;
    for s in items {
        let src = s["sources"].as_array().unwrap();
        assert!(!src.is_empty(), "unsourced: {s}");
        if let Some(t) = s["type"].as_str() {
            assert!(types.contains(&t), "{s}");
            assert!(!s["text"].as_str().unwrap().to_lowercase().contains("is fraud"), "{s}");
        }
        for x in src {
            match x["kind"].as_str().unwrap() {
                "alert" => assert!(known.contains(x["id"].as_str().unwrap()), "{x}"),
                "transaction" => assert_eq!(get(&app, &format!("/api/v2/transactions/{}", x["id"].as_str().unwrap())).await.0, StatusCode::OK, "{x}"),
                "ledger" => assert!(seqs.contains(&x["seq"].as_u64().unwrap()), "{x}"),
                "evidence_pack" => assert_eq!(ok(&app, &format!("/api/v2/alerts/{}/evidence", x["alert_id"].as_str().unwrap())).await["pack_hash"], x["pack_hash"], "{x}"),
                "comment" => assert!(inv["investigation"]["comments"][x["index"].as_u64().unwrap() as usize]["text"].is_string(), "{x}"),
                k => panic!("unknown source kind {k}"),
            }
            checked += 1;
        }
    }
    assert!(checked > 50, "{checked} sources resolved");
    assert_eq!(get(&app, "/api/v2/investigations/INV-99999/narrative").await.0, StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_reads_events_and_case_work_stay_consistent() {
    let (app, _) = app();
    let alert = blocked(&app, "cc").await["alert_id"].as_str().unwrap().to_string();
    let (s, c) = act(&app, "/api/v2/investigations", Some(ASHA), json!({"title": "concurrent", "alert_ids": [alert]})).await;
    assert_eq!(s, StatusCode::OK, "{c}");
    let case = c["case_id"].as_str().unwrap().to_string();
    let mut jobs = Vec::new();
    for i in 0..60usize {
        let (app, alert, case) = (app.clone(), alert.clone(), case.clone());
        jobs.push(tokio::spawn(async move {
            match i % 6 {
                // every event is sent twice (a sender's retry racing its first attempt)
                0 | 1 => send(&app, &pay(&format!("cc-ev-{}", i / 2), &at(i as i64 / 2), ("070", &format!("CP{}", i % 4)), ("011", "CPAYEE"), 100.0 + (i / 2) as f64)).await.0,
                2 => get(&app, "/api/v2/dashboard").await.0,
                3 => get(&app, if i % 12 == 3 { "/api/v2/transactions?account=CPAYEE" } else { "/api/v2/graph?account=LM1&depth=3" }).await.0,
                4 => act(&app, &format!("/api/v2/alerts/{alert}/triage"), Some(ASHA), json!({"comment": format!("note {i}")})).await.0,
                _ => act(&app, &format!("/api/v2/investigations/{case}/actions"), Some(RAVI), json!({"action": "comment", "text": format!("c{i}")})).await.0,
            }
        }));
    }
    for j in jobs {
        assert_eq!(j.await.unwrap(), StatusCode::OK);
    }
    // every event id was sent twice, at the same moment
    let ids: BTreeSet<usize> = (0..60).filter(|i| i % 6 < 2).map(|i| i / 2).collect();
    let t = ok(&app, "/api/v2/transactions?account=CPAYEE&limit=200").await;
    assert_eq!(t["total"].as_u64().unwrap() as usize, ids.len(), "each event stored once");
    let d = ok(&app, &format!("/api/v2/investigations/{case}")).await;
    assert_eq!(d["investigation"]["comments"].as_array().unwrap().len(), 10, "no comment lost");
    let pack = ok(&app, &format!("/api/v2/alerts/{alert}/evidence")).await;
    assert_eq!(pack["alert"]["actions"].as_array().unwrap().iter().filter(|a| a["action"] == "comment").count(), 10);
    assert_eq!(ok(&app, "/api/v2/audit").await["chain_valid"], true);
    assert_eq!(ok(&app, "/api/v2/dashboard").await["payments"]["live_events"]["value"].as_u64().unwrap() as usize, 2 + ids.len());
}

/// The demo replay is labelled as demonstration data everywhere it shows.
#[tokio::test]
async fn demo_stream_rows_are_labelled_demonstration_data() {
    let (app, _) = app();
    let none = ok(&app, "/api/v2/dashboard").await;
    assert_eq!((none["data"]["kind"].as_str(), none["payments"]["stored"]["value"].as_u64()), (Some("none"), Some(0)), "an empty server shows no numbers of its own");
    assert!(none["detection"]["demo_stream_truth"].is_null());
    let post = |p: &'static str| call(&app, Request::builder().method("POST").uri(p).body(Body::empty()).unwrap());
    assert_eq!(post("/api/v2/demo/reset").await.0, StatusCode::OK);
    let (s, next) = post("/api/v2/demo/next?n=20000").await;
    assert_eq!(s, StatusCode::OK);
    let feed = next["feed"].as_array().unwrap();
    assert!(!feed.is_empty(), "the replay escalates some payments");
    let d = ok(&app, "/api/v2/dashboard").await;
    assert_eq!((d["data"]["kind"].as_str(), d["data"]["demo_payments_replayed"].as_u64(), d["payments"]["scored"]["value"].as_u64()),
               (Some("demo_stream"), Some(20000), Some(20000)));
    assert_eq!(d["detection"]["demo_stream_truth"], next["truth"]);
    assert_eq!(d["payments"]["by_decision"]["value"], next["decisions"]);
    let t = ok(&app, "/api/v2/transactions?limit=200").await;
    let rows = t["transactions"].as_array().unwrap();
    assert_eq!((t["data"].as_str(), rows.len()), (Some("demo_stream"), 60.min(t["total"].as_u64().unwrap() as usize)));
    assert!(rows.iter().all(|r| r["source"] == "demo_stream" && r["demo"] == true && r["id"].as_str().unwrap().starts_with("D-") && r["band"] != "allow"));
    // a demo row's detail and graph come from the same feed row
    let top = &feed[0];
    let row = rows.iter().find(|r| r["risk"] == top["risk"] && r["amount"] == top["amount"]).unwrap();
    let detail = ok(&app, &format!("/api/v2/transactions/{}", row["id"].as_str().unwrap())).await;
    assert_eq!((&detail["transaction"]["reasons"], &detail["transaction"]["case_id"]), (&top["reasons"], &top["case_id"]));
    let g = ok(&app, &format!("/api/v2/graph?account={}&bank={}", row["from_account"].as_str().unwrap(), row["from_bank"].as_str().unwrap())).await;
    assert_eq!(g["data"], "demo_stream");
    assert!(g["edges"].as_array().unwrap().iter().any(|e| e["id"] == row["id"] && e["evidence"]["demo"] == true));
    let seed = g["nodes"].as_array().unwrap().iter().find(|n| n["seed"] == true).unwrap();
    if row["case_id"].is_string() {
        assert!(seed["indicators"].as_array().unwrap().iter().any(|i| i["kind"] == "ring_case"), "a payment the engine linked into a ring case: {seed}");
    }
}

struct Redis(Child, String);

impl Drop for Redis {
    fn drop(&mut self) {
        let _ = self.0.kill();
    }
}

fn redis_server() -> Option<Redis> {
    if !Command::new("sh").arg("-c").arg("command -v redis-server").output().ok()?.status.success() {
        eprintln!("redis-server not installed: skipping");
        return None;
    }
    let port = std::net::TcpListener::bind("127.0.0.1:0").ok()?.local_addr().ok()?.port();
    let child = Command::new("redis-server").args(["--port", &port.to_string(), "--save", "", "--appendonly", "no"]).stdout(Stdio::null()).spawn().ok()?;
    let url = format!("redis://127.0.0.1:{port}/0");
    for _ in 0..100 {
        if redis::Client::open(url.as_str()).ok().and_then(|c| c.get_connection().ok()).is_some() {
            return Some(Redis(child, url));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    None
}

/// Redis mode: a second instance (a cold start) reads the first one's decisions, alerts and investigations.
#[tokio::test]
async fn redis_mode_shares_decisions_alerts_and_investigations_across_instances() {
    let Some(r) = redis_server() else { return };
    let instance = || {
        let (app, shared) = app();
        shared.state.lock().unwrap().live = rusetect_server::live::Live::redis(&r.1, "w1:").unwrap();
        app
    };
    let (a, b) = (instance(), instance());
    let sent = mule_network(&a).await;
    let blk = blocked(&a, "rds").await;
    let alert = blk["alert_id"].as_str().unwrap().to_string();
    // the other instance searches, explains and draws what the first one ingested
    let t = ok(&b, "/api/v2/transactions?account=MULE01&limit=200").await;
    assert_eq!((t["total"].as_u64(), t["window"]["store"].as_str(), t["data"].as_str()), (Some(24), Some("redis"), Some("live")), "{}", t["window"]);
    assert_eq!(t["window"]["live_events_ingested"], 26);
    for s in &sent {
        let row = t["transactions"].as_array().unwrap().iter().find(|x| x["id"] == s["event_id"]).unwrap();
        assert_eq!((&row["band"], &row["risk"]), (&s["action"], &s["risk"]));
    }
    let d = ok(&b, "/api/v2/transactions/rds-2").await;
    assert_eq!((d["transaction"]["band"].as_str(), &d["alert"]["alert_id"], d["explanations"]["available"].as_bool()), (Some("block"), &json!(alert), Some(false)));
    assert!(d["explanations"]["note"].as_str().unwrap().contains("Redis"), "the missing reason codes are explained, not invented");
    let g = ok(&b, "/api/v2/graph?account=MULE01").await;
    assert_eq!((g["counts"]["accounts"].as_u64(), g["counts"]["payments"].as_u64()), (Some(14), Some(24)));
    let dash = ok(&b, "/api/v2/dashboard").await;
    assert_eq!((dash["data"]["kind"].as_str(), dash["payments"]["live_events"]["value"].as_u64()), (Some("live"), Some(26)));
    assert!(dash["alerts"]["total"]["value"].as_u64().unwrap() >= 1);
    // case work on one instance is seen and continued on the other
    let (s, tr) = act(&b, &format!("/api/v2/alerts/{alert}/triage"), Some(ASHA), json!({"assignee": "analyst.asha", "priority": "high"})).await;
    assert_eq!(s, StatusCode::OK, "{tr}");
    let (s, c) = act(&a, "/api/v2/investigations", Some(ASHA), json!({"title": "redis case", "alert_ids": [alert]})).await;
    assert_eq!(s, StatusCode::OK, "{c}");
    let id = c["case_id"].as_str().unwrap().to_string();
    let actions = format!("/api/v2/investigations/{id}/actions");
    assert_eq!(act(&b, &actions, Some(ASHA), json!({"action": "request_close", "text": "done"})).await.1["status"], "pending_closure");
    assert_eq!(act(&a, &actions, Some(ASHA), json!({"action": "approve_close"})).await.0, StatusCode::BAD_REQUEST, "four-eyes holds across instances");
    assert_eq!(act(&a, &actions, Some(RAVI), json!({"action": "approve_close"})).await.1["status"], "closed");
    let seen = ok(&b, &format!("/api/v2/investigations/{id}")).await;
    assert_eq!((seen["investigation"]["status"].as_str(), seen["investigation"]["history"].as_array().unwrap().len()), (Some("closed"), 3));
    assert_eq!((seen["alerts"][0]["workflow"]["assignee"].as_str(), seen["alerts"][0]["status"].as_str()), (Some("analyst.asha"), Some("open")));
    assert_eq!(ok(&b, "/api/v2/investigations?status=closed").await["total"], 1);
    let (s, e) = act(&b, "/api/v2/investigations", Some(RAVI), json!({"title": "again", "alert_ids": [alert]})).await;
    assert_eq!(s, StatusCode::CONFLICT, "the alert's investigation is known to every instance: {e}");
    let other = blocked(&b, "rds2").await;
    let (_, c2) = act(&b, "/api/v2/investigations", Some(RAVI), json!({"title": "other", "alert_ids": [other["alert_id"]]})).await;
    assert_ne!(c2["case_id"], id.as_str(), "ids come from one counter");
    let mut con = redis::Client::open(r.1.as_str()).unwrap().get_connection().unwrap();
    let n: i64 = redis::cmd("HLEN").arg("w1:live:cases").query(&mut con).unwrap();
    assert_eq!(n, 2);
}
