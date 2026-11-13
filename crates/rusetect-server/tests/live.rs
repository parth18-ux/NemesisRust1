//! Signed ingest webhook (/api/v2/events/payment): signature, freshness, idempotency, the inline action (alert and
//! ledger entry) and the durable store. The Redis test is skipped when redis-server is not installed.
use axum::body::Body;
use axum::http::{Request, StatusCode};
use hmac::{Hmac, Mac};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::process::{Child, Command, Stdio};
use tower::ServiceExt;

const SECRET: &str = "test-webhook-secret-0123456789";

fn app() -> (axum::Router, rusetect_server::AppRef) {
    std::env::set_var("RUSETECT_RATE_LIMIT", "1000000");
    std::env::set_var("RUSETECT_WEBHOOK_SECRET", SECRET);
    let shared = rusetect_server::new_shared();
    (rusetect_server::build_app(shared.clone()), shared)
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn sign(ts: i64, body: &str, secret: &str) -> String {
    let mut m = Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    m.update(format!("{ts}.{body}").as_bytes());
    format!("v1={}", hex::encode(m.finalize().into_bytes()))
}

/// POST a signed event; returns (status, body, replayed header).
async fn send(app: &axum::Router, body: &Value, ts: i64, sig: Option<String>) -> (StatusCode, Value, bool) {
    let raw = body.to_string();
    let sig = sig.unwrap_or_else(|| sign(ts, &raw, SECRET));
    let req = Request::builder().method("POST").uri("/api/v2/events/payment").header("content-type", "application/json")
        .header("x-rusetect-timestamp", ts.to_string()).header("x-rusetect-signature", sig).body(Body::from(raw)).unwrap();
    let r = app.clone().oneshot(req).await.unwrap();
    let (st, replayed) = (r.status(), r.headers().get("idempotent-replayed").is_some());
    let b = r.into_body().collect().await.unwrap().to_bytes();
    (st, serde_json::from_slice(&b).unwrap_or(Value::Null), replayed)
}

async fn get(app: &axum::Router, path: &str) -> Value {
    let r = app.clone().oneshot(Request::builder().uri(path).body(Body::empty()).unwrap()).await.unwrap();
    serde_json::from_slice(&r.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

async fn status_of(app: &axum::Router, req: Request<Body>) -> (StatusCode, Value) {
    let r = app.clone().oneshot(req).await.unwrap();
    let st = r.status();
    (st, serde_json::from_slice(&r.into_body().collect().await.unwrap().to_bytes()).unwrap_or(Value::Null))
}

fn review(aid: &str, key: &str, action: &str, nonce: &str) -> Request<Body> {
    Request::builder().method("POST").uri(format!("/api/v2/alerts/{aid}/review")).header("content-type", "application/json")
        .header("x-analyst-key", key).header("x-nonce", nonce).body(Body::from(json!({"action": action}).to_string())).unwrap()
}

async fn post(app: &axum::Router, path: &str, body: Value) -> Value {
    let req = Request::builder().method("POST").uri(path).header("content-type", "application/json").body(Body::from(body.to_string())).unwrap();
    serde_json::from_slice(&app.clone().oneshot(req).await.unwrap().into_body().collect().await.unwrap().to_bytes()).unwrap()
}

fn event(id: &str, acct: &str) -> Value {
    json!({"event_id": id, "timestamp": "2022-09-09 10:00", "from_bank": "070", "from_account": acct, "to_bank": "011", "to_account": "LM1",
           "amount_paid": 9000, "payment_format": "ACH"})
}

fn auth(ts: f64, amount: f64) -> Value {
    json!({"user_id": "live-user", "ts": ts, "device": {"device_id": "attacker-phone", "platform": "android", "sim_change_hours": 1.0},
           "network": {"ip": "49.229.31.106", "city": "Mumbai", "lat": 19.07, "lon": 72.87},
           "behaviour": {"time_to_submit_s": 20, "input_events": 30}, "transaction": {"amount": amount, "payee_id": "mule-9"}})
}

/// A first event from a never-seen device is challenged; after the challenge fails, the next event is blocked.
async fn blocked_event(app: &axum::Router, tag: &str) -> Value {
    let mut e1 = event(&format!("{tag}-1"), &format!("{tag}A"));
    e1["auth"] = auth(1.767e9, 60000.0);
    let (s, r1, _) = send(app, &e1, now(), None).await;
    assert_eq!(s, StatusCode::OK, "{r1}");
    assert_eq!(r1["pramaan"]["decision"], "challenge", "{r1}");
    assert_ne!(r1["action"], "allow", "a challenge asks for verification at least");
    let v = post(app, "/api/v2/challenge/verify", json!({"decision_id": r1["pramaan"]["decision_id"], "passed": false})).await;
    assert_eq!(v["decision"], "block", "{v}");
    let mut e2 = event(&format!("{tag}-2"), &format!("{tag}A"));
    e2["timestamp"] = json!("2022-09-09 10:05");
    e2["auth"] = auth(1.767e9 + 60.0, 100.0);
    let (s, r2, _) = send(app, &e2, now(), None).await;
    assert_eq!(s, StatusCode::OK, "{r2}");
    r2
}

#[tokio::test]
async fn valid_signature_is_accepted_and_a_replay_returns_the_stored_result() {
    let (app, shared) = app();
    let e = event("evt-1", "LV1");
    let (s, r, replayed) = send(&app, &e, now(), None).await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert!(!replayed);
    assert!(["allow", "verify", "hold", "block"].contains(&r["action"].as_str().unwrap()), "{r}");
    assert_eq!((r["event_id"].as_str(), r["source"].as_str(), r["durable"].as_bool()), (Some("evt-1"), Some("live"), Some(false)));
    assert!(r["ledger"]["hash"].is_string());
    // the same event again (fresh timestamp and signature, as a sender's retry would be): the stored result, no re-scoring
    let (s, again, replayed) = send(&app, &e, now(), None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(replayed);
    assert_eq!(again, r);
    {
        let st = shared.state.lock().unwrap();
        assert_eq!(st.desk.log.iter().filter(|x| x["type"] == "live_decision").count(), 1, "one ledger entry");
        assert_eq!(st.engine.as_ref().unwrap().n_seen, 1, "account state written once");
    }
    let live = get(&app, "/api/v2/live").await;
    assert_eq!(live["events"].as_array().unwrap().len(), 1);
    assert_eq!(live["events"][0]["event_id"], "evt-1");
}

#[tokio::test]
async fn bad_signature_and_stale_timestamp_are_refused() {
    let (app, shared) = app();
    let e = event("evt-bad", "LV2");
    let raw = e.to_string();
    let (s, _, _) = send(&app, &e, now(), Some(sign(now(), &raw, "another-secret-0123456789"))).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "wrong secret");
    let (s, _, _) = send(&app, &e, now(), Some(sign(now(), &raw.replace("9000", "9001"), SECRET))).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "body changed after signing");
    let (s, _, _) = send(&app, &e, now(), Some("v1=zz".into())).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "malformed");
    let stale = now() - 301;
    let (s, r, _) = send(&app, &e, stale, None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "correctly signed but older than 5 minutes");
    assert!(r["detail"].as_str().unwrap().contains("5 minutes"));
    let (s, _, _) = send(&app, &e, now() + 301, None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "too far in the future");
    let mut no_id = e.clone();
    no_id.as_object_mut().unwrap().remove("event_id");
    assert_eq!(send(&app, &no_id, now(), None).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    let mut bad = e.clone();
    bad["payment_format"] = json!("Telepathy");
    assert_eq!(send(&app, &bad, now(), None).await.0, StatusCode::UNPROCESSABLE_ENTITY, "existing input caps apply");
    assert_eq!(shared.state.lock().unwrap().engine.as_ref().unwrap().n_seen, 0, "nothing was scored");
}

#[tokio::test]
async fn block_decision_opens_an_alert_and_a_ledger_entry() {
    let (app, shared) = app();
    let r = blocked_event(&app, "blk").await;
    assert_eq!((r["action"].as_str(), r["pramaan"]["decision"].as_str()), (Some("block"), Some("block")), "{r}");
    assert_eq!(r["final_decision"], "Block + STR draft");
    let aid = r["alert_id"].as_str().expect("alert opened").to_string();
    let st = shared.state.lock().unwrap();
    let a = &st.desk.alerts[&aid];
    assert_eq!((a["status"].as_str(), a["event_id"].as_str()), (Some("open"), Some("blk-2")));
    let entry = st.desk.log.iter().find(|x| x["type"] == "live_decision" && x["event_id"] == "blk-2").expect("ledger entry");
    assert_eq!((entry["action"].as_str(), entry["alert_id"].as_str()), (Some("block"), Some(aid.as_str())));
    assert_eq!(entry["hash"], r["ledger"]["hash"]);
    // the ledger position is the entry's place in the log, not the event's store-wide sequence (kept as event_seq)
    let pos = st.desk.log.iter().position(|x| x["hash"] == entry["hash"]).unwrap() + 1;
    assert_eq!(r["ledger"]["seq"], json!(pos));
    assert_eq!(entry["event_seq"], r["seq"]);
    assert_ne!(r["ledger"]["seq"], r["seq"], "alert and earlier entries put the ledger ahead of the event sequence");
    assert!(st.desk.verify_chain());
}

#[tokio::test]
async fn demo_reset_is_refused_once_live_events_exist() {
    let (app, shared) = app();
    let r = blocked_event(&app, "rst").await;
    let req = || Request::builder().method("POST").uri("/api/v2/demo/reset?warm=false").body(Body::empty()).unwrap();
    let (s, body) = status_of(&app, req()).await;
    assert_eq!(s, StatusCode::CONFLICT, "{body}");
    assert!(body["detail"].as_str().unwrap().contains("live events"));
    let st = shared.state.lock().unwrap();
    assert!(st.desk.alerts.contains_key(r["alert_id"].as_str().unwrap()), "the live alert is kept");
}

#[test]
fn extreme_timestamps_are_refused_without_overflow() {
    for ts in [i64::MIN.to_string(), i64::MAX.to_string()] {
        let e = rusetect_server::live::verify_signature(b"k", Some(&ts), Some("v1=00"), b"{}", 1_700_000_000).unwrap_err();
        assert_eq!(e.status, 401);
    }
    let e = rusetect_server::live::verify_signature(b"k", Some("0"), Some("v1=00"), b"{}", i64::MIN).unwrap_err();
    assert_eq!(e.status, 401);
}

#[tokio::test]
async fn memory_fallback_reports_not_durable() {
    let (app, _) = app();
    let h = get(&app, "/api/health").await;
    assert_eq!((h["durable"].as_bool(), h["store"]["kind"].as_str()), (Some(false), Some("memory")));
    assert_eq!(get(&app, "/api/v2/live").await["durable"], false);
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

#[tokio::test]
async fn redis_store_commits_durably_and_dedupes_across_instances() {
    let Some(r) = redis_server() else { return };
    let instance = || {
        let (app, shared) = app();
        shared.state.lock().unwrap().live = rusetect_server::live::Live::redis(&r.1, "t:").unwrap();
        app
    };
    let (a, b) = (instance(), instance());
    assert_eq!(get(&a, "/api/health").await["durable"], true);
    let e = event("evt-r1", "RV1");
    let (s, first, _) = send(&a, &e, now(), None).await;
    assert_eq!(s, StatusCode::OK, "{first}");
    assert_eq!((first["durable"].as_bool(), first["store"].as_str()), (Some(true), Some("redis")));
    // a second function instance (a cold start) sees the stored result: no second scoring, no second write
    let (s, again, replayed) = send(&b, &e, now(), None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(replayed);
    assert_eq!(again, first);
    let mut con = redis::Client::open(r.1.as_str()).unwrap().get_connection().unwrap();
    let out_n: i64 = redis::cmd("HGET").arg("t:a:{070|RV1}").arg("out_n").query(&mut con).unwrap();
    assert_eq!(out_n, 1, "account state written once");
    let n: i64 = redis::cmd("LLEN").arg("t:live:ledger").query(&mut con).unwrap();
    assert_eq!(n, 1);
    // the block path: alert and ledger entry in Redis; the feed is shared by every instance
    let blk = blocked_event(&a, "rblk").await;
    assert_eq!(blk["action"], "block", "{blk}");
    let aid = blk["alert_id"].as_str().unwrap();
    let alert: String = redis::cmd("HGET").arg("t:live:alerts").arg(aid).query(&mut con).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&alert).unwrap()["event_id"], "rblk-2");
    let ledger: Vec<String> = redis::cmd("LRANGE").arg("t:live:ledger").arg(0).arg(-1).query(&mut con).unwrap();
    assert!(ledger.iter().any(|x| x.contains("\"rblk-2\"") && x.contains("\"block\"")));
    let feed = get(&b, "/api/v2/live").await;
    assert_eq!(feed["events"].as_array().unwrap().len(), 3);
    assert_eq!(feed["events"][0]["event_id"], "rblk-2");
    // the ledger list is capped
    let n_ledger: i64 = redis::cmd("LLEN").arg("t:live:ledger").query(&mut con).unwrap();
    assert!(n_ledger as usize <= rusetect_server::live::LEDGER_MAX);
    // another instance lists the durable alert and reviews it; the action is written back to Redis
    let listed = get(&b, "/api/v2/alerts").await;
    assert!(listed.as_array().unwrap().iter().any(|x| x["alert_id"] == aid), "{listed}");
    let (s, acted) = status_of(&b, review(aid, "demo-analyst-1", "approve_block", "nonce-redis-1")).await;
    assert_eq!((s, acted["status"].as_str()), (StatusCode::OK, Some("block_pending_second_approval")), "{acted}");
    let stored: String = redis::cmd("HGET").arg("t:live:alerts").arg(aid).query(&mut con).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&stored).unwrap()["status"], "block_pending_second_approval");
    // the first instance sees that approval: the same analyst cannot approve twice, a second one completes the block
    assert_eq!(status_of(&a, review(aid, "demo-analyst-1", "approve_block", "nonce-redis-2")).await.0, StatusCode::BAD_REQUEST);
    let (s, done) = status_of(&a, review(aid, "demo-analyst-2", "approve_block", "nonce-redis-3")).await;
    assert_eq!((s, done["status"].as_str()), (StatusCode::OK, Some("blocked")), "{done}");
}

#[tokio::test]
async fn redis_failures_release_the_claim_or_keep_an_applied_commit() {
    let Some(r) = redis_server() else { return };
    let (app, shared) = app();
    shared.state.lock().unwrap().live = rusetect_server::live::Live::redis(&r.1, "f:").unwrap();
    let mut con = redis::Client::open(r.1.as_str()).unwrap().get_connection().unwrap();
    // scoring fails (an account key of the wrong type): the claim is released, so the sender's retry is processed
    redis::cmd("SET").arg("f:a:{070|FV1}").arg("x").query::<()>(&mut con).unwrap();
    let (s, _, _) = send(&app, &event("evt-f1", "FV1"), now(), None).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
    let claim: Option<String> = redis::cmd("GET").arg("f:live:ev:evt-f1").query(&mut con).unwrap();
    assert_eq!(claim, None, "claim released");
    redis::cmd("DEL").arg("f:a:{070|FV1}").query::<()>(&mut con).unwrap();
    let (s, ok, replayed) = send(&app, &event("evt-f1", "FV1"), now(), None).await;
    assert_eq!((s, replayed), (StatusCode::OK, false), "{ok}");
    // the commit transaction reports an error after storing the result (feed of the wrong type): the stored result
    // stands, so the claim is not released and a retry is a replay, not a second commit
    redis::cmd("SET").arg("f:live:feed").arg("x").query::<()>(&mut con).unwrap();
    let (s, first, _) = send(&app, &event("evt-f2", "FV2"), now(), None).await;
    assert_eq!(s, StatusCode::OK, "{first}");
    let (s, again, replayed) = send(&app, &event("evt-f2", "FV2"), now(), None).await;
    assert_eq!((s, replayed), (StatusCode::OK, true));
    assert_eq!(again, first);
    let n: i64 = redis::cmd("LLEN").arg("f:live:ledger").query(&mut con).unwrap();
    assert_eq!(n, 2, "one ledger entry per event");
    // a dropped connection is reopened once, without a PING per request
    redis::cmd("DEL").arg("f:live:feed").query::<()>(&mut con).unwrap();
    redis::cmd("CLIENT").arg("KILL").arg("TYPE").arg("normal").arg("SKIPME").arg("yes").query::<i64>(&mut con).unwrap();
    let (s, _, _) = send(&app, &event("evt-f3", "FV3"), now(), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(get(&app, "/api/health").await["store"]["reachable"], true);
}
