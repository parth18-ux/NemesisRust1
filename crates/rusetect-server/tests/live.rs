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
const PREVIOUS: &str = "old-webhook-secret-0123456789";

fn app() -> (axum::Router, rusetect_server::AppRef) {
    std::env::set_var("RUSETECT_RATE_LIMIT", "1000000");
    std::env::set_var("RUSETECT_WEBHOOK_SECRET", SECRET);
    std::env::set_var("RUSETECT_WEBHOOK_SECRET_PREVIOUS", PREVIOUS);
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
        let e = rusetect_server::live::verify_signature(&[b"k"], Some(&ts), Some("v1=00"), b"{}", 1_700_000_000).unwrap_err();
        assert_eq!(e.status, 401);
    }
    let e = rusetect_server::live::verify_signature(&[b"k"], Some("0"), Some("v1=00"), b"{}", i64::MIN).unwrap_err();
    assert_eq!(e.status, 401);
}

#[tokio::test]
async fn memory_fallback_reports_not_durable() {
    let (app, _) = app();
    let h = get(&app, "/api/health").await;
    assert_eq!((h["durable"].as_bool(), h["store"]["kind"].as_str()), (Some(false), Some("memory")));
    assert_eq!(get(&app, "/api/v2/live").await["durable"], false);
    assert_eq!(h["ready"], true, "memory mode is ready once the engine is loaded");
}

#[tokio::test]
async fn previous_secret_is_accepted_during_rotation() {
    let (app, _) = app();
    let e = event("evt-rot-1", "ROT1");
    let ts = now();
    let (s, r, _) = send(&app, &e, ts, Some(sign(ts, &e.to_string(), PREVIOUS))).await;
    assert_eq!(s, StatusCode::OK, "{r}");
    let (s, _, _) = send(&app, &event("evt-rot-2", "ROT2"), ts, Some(sign(ts, &event("evt-rot-2", "ROT2").to_string(), "a-third-secret-not-configured"))).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
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

/// Redis down: the webhook fails closed (503 + Retry-After, the event is not committed or marked seen, /api/health says
/// not ready), and once Redis is back the same event is scored and committed, not lost.
#[tokio::test]
async fn redis_outage_fails_closed_and_recovers() {
    let Some(mut r) = redis_server() else { return };
    let (app, shared) = app();
    shared.state.lock().unwrap().live = rusetect_server::live::Live::redis(&r.1, "o:").unwrap();
    let (s, _, _) = send(&app, &event("evt-o1", "OV1"), now(), None).await;
    assert_eq!(s, StatusCode::OK);
    let _ = r.0.kill();
    let _ = r.0.wait();
    let e = event("evt-o2", "OV2");
    let (raw, ts) = (e.to_string(), now());
    let req = Request::builder().method("POST").uri("/api/v2/events/payment").header("content-type", "application/json")
        .header("x-rusetect-timestamp", ts.to_string()).header("x-rusetect-signature", sign(ts, &raw, SECRET)).body(Body::from(raw)).unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(resp.headers().get("retry-after").and_then(|v| v.to_str().ok()), Some(rusetect_server::RETRY_AFTER_S));
    let h = get(&app, "/api/health").await;
    assert_eq!((h["ready"].as_bool(), h["durable"].as_bool()), (Some(false), Some(false)), "{h}");
    // Redis returns on the same port: the retry of the refused event is processed (not a replay), and health recovers
    let port = r.1.trim_end_matches("/0").rsplit(':').next().unwrap().to_string();
    r.0 = Command::new("redis-server").args(["--port", &port, "--save", "", "--appendonly", "no"]).stdout(Stdio::null()).spawn().unwrap();
    for _ in 0..100 {
        if redis::Client::open(r.1.as_str()).ok().and_then(|c| c.get_connection().ok()).is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let (s, body, replayed) = send(&app, &e, now(), None).await;
    assert_eq!((s, replayed), (StatusCode::OK, false), "{body}");
    assert_eq!(body["durable"], true);
    assert_eq!(get(&app, "/api/health").await["ready"], true);
}

/// An event_id another request is still processing: 409 with Retry-After of the claim's lifetime, and nothing written.
#[tokio::test]
async fn a_claimed_event_is_a_409_with_retry_after() {
    let Some(r) = redis_server() else { return };
    let (app, shared) = app();
    shared.state.lock().unwrap().live = rusetect_server::live::Live::redis(&r.1, "ra:").unwrap();
    let mut con = redis::Client::open(r.1.as_str()).unwrap().get_connection().unwrap();
    redis::cmd("SET").arg("ra:live:ev:evt-ra").arg("pending:77").query::<()>(&mut con).unwrap();
    let e = event("evt-ra", "RA1");
    let (raw, ts) = (e.to_string(), now());
    let req = Request::builder().method("POST").uri("/api/v2/events/payment").header("content-type", "application/json")
        .header("x-rusetect-timestamp", ts.to_string()).header("x-rusetect-signature", sign(ts, &raw, SECRET)).body(Body::from(raw)).unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let ra = rusetect_server::live::CLAIM_TTL_S.to_string();
    assert_eq!(resp.headers().get("retry-after").and_then(|v| v.to_str().ok()), Some(ra.as_str()));
    assert_eq!(llen(&mut con, "ra:live:ledger"), 0);
    let claim: Option<String> = redis::cmd("GET").arg("ra:live:ev:evt-ra").query(&mut con).unwrap();
    assert_eq!(claim.as_deref(), Some("pending:77"), "another request's claim is left alone");
}

fn llen(con: &mut redis::Connection, k: &str) -> i64 {
    redis::cmd("LLEN").arg(k).query(con).unwrap()
}

fn get_int(con: &mut redis::Connection, k: &str) -> i64 {
    redis::cmd("GET").arg(k).query::<Option<i64>>(con).unwrap().unwrap_or(0)
}

/// 32 concurrent sends of one event_id: one is scored and committed, the others wait (409) or replay it unchanged; a
/// replay does not take a sequence number.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_duplicates_commit_once() {
    let Some(r) = redis_server() else { return };
    let (app, shared) = app();
    shared.state.lock().unwrap().live = rusetect_server::live::Live::redis(&r.1, "cd:").unwrap();
    let e = event("evt-dup", "CD1");
    let tasks: Vec<_> = (0..32).map(|_| {
        let (app, e) = (app.clone(), e.clone());
        tokio::spawn(async move { send(&app, &e, now(), None).await })
    }).collect();
    let mut res = vec![];
    for t in tasks {
        res.push(t.await.unwrap());
    }
    let first: Vec<&Value> = res.iter().filter(|(s, _, rp)| *s == StatusCode::OK && !rp).map(|x| &x.1).collect();
    assert_eq!(first.len(), 1, "exactly one non-replayed 200");
    for (s, b, rp) in &res {
        assert!(*s == StatusCode::CONFLICT || (*s == StatusCode::OK && (!rp || b == first[0])), "{s} {b}");
    }
    let mut con = redis::Client::open(r.1.as_str()).unwrap().get_connection().unwrap();
    assert_eq!(llen(&mut con, "cd:live:ledger"), 1);
    let out_n: i64 = redis::cmd("HGET").arg("cd:a:{070|CD1}").arg("out_n").query(&mut con).unwrap();
    assert_eq!(out_n, 1, "account state written once");
    assert_eq!(get_int(&mut con, "cd:live:seq"), 1);
    let (s, again, replayed) = send(&app, &e, now(), None).await;
    assert_eq!((s, replayed, &again), (StatusCode::OK, true, first[0]));
    assert_eq!(get_int(&mut con, "cd:live:seq"), 1, "a replay does not advance live:seq");
}

/// 200 events from 16 concurrent senders, a quarter from one payer, some raised by PRAMAAN: sequence numbers are unique
/// and gap-free, every event has one ledger entry, the shared payer's count is exact, and the chained entries reach
/// live:ledger in chain order.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_events_keep_ledger_and_state_consistent() {
    let Some(r) = redis_server() else { return };
    let (app, shared) = app();
    shared.state.lock().unwrap().live = rusetect_server::live::Live::redis(&r.1, "cc:").unwrap();
    let tasks: Vec<_> = (0..16).map(|t| {
        let app = app.clone();
        tokio::spawn(async move {
            let mut out = vec![];
            for i in (t..200).step_by(16) {
                let mut e = event(&format!("cc-{i}"), &if i % 4 == 0 { "CCSAME".to_string() } else { format!("CC{i}") });
                if i % 10 == 1 {
                    let mut a = auth(1.767e9, 60000.0);
                    a["user_id"] = json!(format!("cc-user-{i}"));
                    e["auth"] = a;
                }
                out.push(send(&app, &e, now(), None).await);
            }
            out
        })
    }).collect();
    let mut res = vec![];
    for t in tasks {
        res.extend(t.await.unwrap());
    }
    assert!(res.iter().all(|(s, _, rp)| *s == StatusCode::OK && !rp), "every event scored once");
    let mut seqs: Vec<u64> = res.iter().map(|x| x.1["seq"].as_u64().unwrap()).collect();
    seqs.sort();
    assert_eq!(seqs, (1..=200).collect::<Vec<u64>>(), "unique and gap-free");
    let mut con = redis::Client::open(r.1.as_str()).unwrap().get_connection().unwrap();
    assert_eq!(llen(&mut con, "cc:live:ledger"), 200);
    let out_n: i64 = redis::cmd("HGET").arg("cc:a:{070|CCSAME}").arg("out_n").query(&mut con).unwrap();
    assert_eq!(out_n, 50, "the shared payer's count is exact");
    let ledger: Vec<String> = redis::cmd("LRANGE").arg("cc:live:ledger").arg(0).arg(-1).query(&mut con).unwrap();
    let st = shared.state.lock().unwrap();
    let chained: Vec<Value> = ledger.iter().map(|x| serde_json::from_str::<Value>(x).unwrap()).filter(|e| !e["seq"].is_null()).collect();
    assert!(chained.len() >= 20, "the PRAMAAN-raised events are chained: {}", chained.len());
    let mut last = 0;
    for e in &chained {
        let n = e["seq"].as_u64().unwrap();
        assert!(n > last, "chained entries in increasing seq");
        let prev = if n == 1 { "0".repeat(64) } else { st.desk.log[n as usize - 2]["hash"].as_str().unwrap().to_string() };
        assert_eq!(e["prev"].as_str(), Some(prev.as_str()), "prev is the previous hash");
        assert_eq!(e, &st.desk.log[n as usize - 1]);
        last = n;
    }
    assert!(st.desk.verify_chain(), "the desk chain verifies");
}

/// Redis killed under 16 senders: every response is 200 or 503, every 200 has its stored ev: record (Redis persists
/// every write here), and once Redis is back each 503 event is processed on retry, not replayed (unless its commit
/// had been applied before the reply was lost).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_outage_fails_closed() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let Some(probe) = redis_server() else { return };
    drop(probe);
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port().to_string();
    let dir = std::env::temp_dir().join(format!("rusetect-outage-{port}"));
    std::fs::create_dir_all(&dir).unwrap();
    let url = format!("redis://127.0.0.1:{port}/0");
    let start = || {
        let c = Command::new("redis-server").args(["--port", &port, "--save", "", "--appendonly", "yes", "--appendfsync", "always"])
            .arg("--dir").arg(&dir).stdout(Stdio::null()).spawn().unwrap();
        for _ in 0..100 {
            if redis::Client::open(url.as_str()).ok().and_then(|c| c.get_connection().ok()).is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        Redis(c, url.clone())
    };
    let mut r = start();
    let (app, shared) = app();
    shared.state.lock().unwrap().live = rusetect_server::live::Live::redis(&url, "co:").unwrap();
    let done = std::sync::Arc::new(AtomicUsize::new(0));
    let tasks: Vec<_> = (0..16).map(|t| {
        let (app, done) = (app.clone(), done.clone());
        tokio::spawn(async move {
            let mut out = vec![];
            for k in 0..40 {
                let e = event(&format!("co-{t}-{k}"), &format!("CO{t}"));
                let (s, b, _) = send(&app, &e, now(), None).await;
                done.fetch_add(1, Ordering::Relaxed);
                out.push((e, s, b));
            }
            out
        })
    }).collect();
    while done.load(Ordering::Relaxed) < 48 {
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
    let _ = r.0.kill();
    let _ = r.0.wait();
    let mut res = vec![];
    for t in tasks {
        res.extend(t.await.unwrap());
    }
    assert!(res.iter().all(|x| x.1 == StatusCode::OK || x.1 == StatusCode::SERVICE_UNAVAILABLE), "200 or 503 only");
    assert!(res.iter().any(|x| x.1 == StatusCode::SERVICE_UNAVAILABLE), "the outage was hit");
    r = start();
    let mut con = redis::Client::open(url.as_str()).unwrap().get_connection().unwrap();
    for (e, s, b) in &res {
        let id = e["event_id"].as_str().unwrap();
        let stored: Option<String> = redis::cmd("GET").arg(format!("co:live:ev:{id}")).query(&mut con).unwrap();
        if *s == StatusCode::OK {
            assert_eq!(stored.as_deref().map(|v| serde_json::from_str::<Value>(v).unwrap()).as_ref(), Some(b), "a 200 has its stored record");
            continue;
        }
        let applied = stored.as_deref().is_some_and(|v| !v.starts_with("pending"));
        // an unfinished claim from before the outage expires after CLAIM_TTL_S (60 s): until then the retry is a 409
        let mut tries = 0;
        let (s, body, replayed) = loop {
            let x = send(&app, e, now(), None).await;
            if x.0 != StatusCode::CONFLICT || tries == 70 {
                break x;
            }
            tries += 1;
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        };
        assert_eq!(s, StatusCode::OK, "{body}");
        assert!(!replayed || applied, "a 503 event is processed on retry, not replayed: {id}");
    }
    drop(r);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A second 9000 ACH payment on one pair inside 24 h: the pair window raises it above allow, so it opens an alert.
fn pair_events(acct: &str) -> (Value, Value) {
    let mut b = event(&format!("{acct}-2"), acct);
    b["timestamp"] = json!("2022-09-09 10:05");
    (event(&format!("{acct}-1"), acct), b)
}

/// One process's view of one event: (live alerts for it, live:ledger entries for it, desk alerts for it, desk
/// live_decision entries for it not voided). Checks the desk chain and that record, alert, evidence pack and ledger agree.
fn one_decision(shared: &rusetect_server::AppRef, con: &mut redis::Connection, px: &str, rec: &Value) -> (usize, usize, usize, usize) {
    let id = rec["event_id"].as_str().unwrap();
    let alerts: Vec<(String, String)> = redis::cmd("HGETALL").arg(format!("{px}live:alerts")).query(con).unwrap();
    let alerts: Vec<Value> = alerts.iter().map(|x| serde_json::from_str::<Value>(&x.1).unwrap()).filter(|a| a["event_id"] == id).collect();
    let ledger: Vec<String> = redis::cmd("LRANGE").arg(format!("{px}live:ledger")).arg(0).arg(-1).query(con).unwrap();
    let ledger: Vec<Value> = ledger.iter().map(|x| serde_json::from_str::<Value>(x).unwrap()).filter(|e| e["event_id"] == id).collect();
    let stored: String = redis::cmd("GET").arg(format!("{px}live:ev:{id}")).query(con).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&stored).unwrap(), *rec, "ev: record is the answer");
    for e in &ledger {
        assert_eq!((&e["final_decision"], &e["alert_id"], &e["hash"]), (&rec["final_decision"], &rec["alert_id"], &rec["ledger"]["hash"]));
    }
    let st = shared.state.lock().unwrap();
    assert!(st.desk.verify_chain());
    let voided: Vec<&Value> = st.desk.log.iter().filter(|e| e["type"] == "live_decision_void").flat_map(|e| e["voids"].as_array().unwrap()).collect();
    let desk_alerts = st.desk.alerts.values().filter(|a| a["event_id"] == id).count();
    let entries: Vec<&Value> = st.desk.log.iter().filter(|e| e["type"] == "live_decision" && e["event_id"] == id && !voided.contains(&&e["hash"])).collect();
    for e in &entries {
        assert_eq!((&e["final_decision"], &e["hash"]), (&rec["final_decision"], &rec["ledger"]["hash"]));
    }
    if let Some(aid) = rec["alert_id"].as_str() {
        assert_eq!(alerts.iter().map(|a| a["alert_id"].as_str().unwrap()).collect::<Vec<_>>(), [aid]);
        let pack = st.desk.evidence(aid).expect("evidence pack");
        assert_eq!(pack["alert"]["event_id"], id);
        assert_eq!(pack["alert"]["assessed"]["final_decision"], rec["final_decision"]);
        let decided: Vec<&Value> = pack["audit_entries"].as_array().unwrap().iter().filter(|e| e["type"] == "live_decision").collect();
        assert_eq!(decided.len(), 1, "{pack}");
        assert_eq!(decided[0]["hash"], rec["ledger"]["hash"]);
    }
    (alerts.len(), ledger.len(), desk_alerts, entries.len())
}

fn redis_instance(r: &Redis, px: &str) -> (axum::Router, rusetect_server::AppRef, std::sync::Arc<rusetect_server::live::Durable>) {
    let (app, shared) = app();
    let live = rusetect_server::live::Live::redis(&r.1, px).unwrap();
    let du = live.redis.clone().unwrap();
    shared.state.lock().unwrap().live = live;
    (app, shared, du)
}

/// The commit fails after decide opened the alert and chained the entries: the attempt is taken out of the desk, the
/// claim released, and the retry (scored again from the features the payment was first scored with, its own pair-window
/// member replaced) gives exactly what a clean single send gives: one alert, one ledger entry, one decision.
#[tokio::test]
async fn a_commit_failure_then_a_retry_leaves_one_alert_one_entry_and_the_clean_decision() {
    let Some(r) = redis_server() else { return };
    let mut con = redis::Client::open(r.1.as_str()).unwrap().get_connection().unwrap();
    let (a, b) = pair_events("CF1");
    // clean single send, in its own namespace
    let (clean_app, _, _) = redis_instance(&r, "c:");
    assert_eq!(send(&clean_app, &a, now(), None).await.0, StatusCode::OK);
    let (s, clean, _) = send(&clean_app, &b, now(), None).await;
    assert_eq!(s, StatusCode::OK, "{clean}");
    assert_ne!(clean["action"], "allow", "the pair window raises the second payment: {clean}");
    // the same two payments, the second one's commit failing once
    let (app, shared, du) = redis_instance(&r, "k:");
    assert_eq!(send(&app, &a, now(), None).await.0, StatusCode::OK);
    let chain = shared.state.lock().unwrap().desk.log.len();
    du.fail_next_commit(1);
    assert_eq!(send(&app, &b, now(), None).await.0, StatusCode::SERVICE_UNAVAILABLE);
    {
        let st = shared.state.lock().unwrap();
        assert_eq!(st.desk.log.len(), chain, "the attempt's chain entries are gone");
        assert!(!st.desk.alerts.values().any(|x| x["event_id"] == "CF1-2"), "no orphan alert");
    }
    assert_eq!(redis::cmd("GET").arg("k:live:ev:CF1-2").query::<Option<String>>(&mut con).unwrap(), None, "claim released");
    let (s, rec, replayed) = send(&app, &b, now(), None).await;
    assert_eq!((s, replayed), (StatusCode::OK, false), "{rec}");
    assert_eq!(one_decision(&shared, &mut con, "k:", &rec), (1, 1, 1, 1));
    for k in ["action", "final_decision", "payment_decision", "risk", "context_band"] {
        assert_eq!(rec[k], clean[k], "{k}: the retry decides as a clean single send");
    }
    // the pair window holds each payment once, with the same risks as the clean send
    let window = |px: &str, con: &mut redis::Connection| {
        let keys: Vec<String> = redis::cmd("KEYS").arg(format!("{px}pr:*")).query(con).unwrap();
        assert_eq!(keys.len(), 1);
        redis::cmd("ZRANGE").arg(&keys[0]).arg(0).arg(-1).query::<Vec<String>>(con).unwrap()
    };
    let w = window("k:", &mut con);
    assert_eq!(w.len(), 2, "{w:?}");
    assert_eq!(w, window("c:", &mut con));
    // and a further retry is a replay
    let (s, again, replayed) = send(&app, &b, now(), None).await;
    assert_eq!((s, replayed, &again), (StatusCode::OK, true, &rec));
    assert_eq!(one_decision(&shared, &mut con, "k:", &rec), (1, 1, 1, 1));
}

/// The commit is applied but its reply is lost: the request finds its own result and answers with it; the alert and the
/// ledger entry it opened stand, once; a retry is a replay.
#[tokio::test]
async fn a_lost_commit_reply_gives_one_decision() {
    let Some(r) = redis_server() else { return };
    let mut con = redis::Client::open(r.1.as_str()).unwrap().get_connection().unwrap();
    let (app, shared, du) = redis_instance(&r, "lr:");
    let mut e = event("LR-1", "LR1");
    e["amount_paid"] = json!(950000);
    e["timestamp"] = json!("2022-09-09 03:00");
    du.fail_next_commit(2);
    let (s, rec, replayed) = send(&app, &e, now(), None).await;
    assert_eq!((s, replayed), (StatusCode::OK, false), "{rec}");
    assert!(rec["alert_id"].is_string(), "{rec}");
    assert_eq!(one_decision(&shared, &mut con, "lr:", &rec), (1, 1, 1, 1));
    let (s, again, replayed) = send(&app, &e, now(), None).await;
    assert_eq!((s, replayed, &again), (StatusCode::OK, true, &rec));
    assert_eq!(one_decision(&shared, &mut con, "lr:", &rec), (1, 1, 1, 1));
}

/// The commit fails and the store cannot say whether it landed: the desk drops its copy of the alert and keeps the chain
/// entries (live:ledger may hold them). When the claim has expired and the retry finds the event uncommitted, those
/// entries are voided before the retry decides: one alert, one live decision, one ledger entry, the chain still valid.
#[tokio::test]
async fn an_unknown_commit_outcome_is_settled_by_the_retry() {
    let Some(r) = redis_server() else { return };
    let mut con = redis::Client::open(r.1.as_str()).unwrap().get_connection().unwrap();
    let (app, shared, du) = redis_instance(&r, "uk:");
    let (a, b) = pair_events("UK1");
    assert_eq!(send(&app, &a, now(), None).await.0, StatusCode::OK);
    du.fail_next_commit(3);
    assert_eq!(send(&app, &b, now(), None).await.0, StatusCode::SERVICE_UNAVAILABLE);
    assert!(!shared.state.lock().unwrap().desk.alerts.values().any(|x| x["event_id"] == "UK1-2"));
    let claim: Option<String> = redis::cmd("GET").arg("uk:live:ev:UK1-2").query(&mut con).unwrap();
    assert!(claim.unwrap().starts_with("pending:"), "outcome unknown: the claim is left to expire");
    redis::cmd("DEL").arg("uk:live:ev:UK1-2").query::<()>(&mut con).unwrap(); // CLAIM_TTL_S passes
    let (s, rec, replayed) = send(&app, &b, now(), None).await;
    assert_eq!((s, replayed), (StatusCode::OK, false), "{rec}");
    assert_eq!(one_decision(&shared, &mut con, "uk:", &rec), (1, 1, 1, 1));
    let st = shared.state.lock().unwrap();
    let void = st.desk.log.iter().find(|e| e["type"] == "live_decision_void").expect("the earlier attempt is voided");
    assert_eq!((void["event_id"].as_str(), void["voids"].as_array().unwrap().len()), (Some("UK1-2"), 2), "{void}");
}

/// Memory mode decides and commits in one locked call, so there is no partial failure; a retry is a replay and record,
/// alert, evidence pack and ledger agree on the one decision.
#[tokio::test]
async fn memory_mode_retry_keeps_one_decision() {
    let (app, shared) = app();
    let (a, b) = pair_events("MM1");
    assert_eq!(send(&app, &a, now(), None).await.0, StatusCode::OK);
    let (s, rec, _) = send(&app, &b, now(), None).await;
    assert_eq!(s, StatusCode::OK);
    let (s, again, replayed) = send(&app, &b, now(), None).await;
    assert_eq!((s, replayed, &again), (StatusCode::OK, true, &rec));
    let st = shared.state.lock().unwrap();
    let entries: Vec<&Value> = st.desk.log.iter().filter(|e| e["type"] == "live_decision" && e["event_id"] == "MM1-2").collect();
    assert_eq!(entries.len(), 1);
    assert_eq!((&entries[0]["final_decision"], &entries[0]["hash"]), (&rec["final_decision"], &rec["ledger"]["hash"]));
    assert_eq!(st.desk.alerts.values().filter(|x| x["event_id"] == "MM1-2").count(), rec["alert_id"].is_string() as usize);
    if let Some(aid) = rec["alert_id"].as_str() {
        let pack = st.desk.evidence(aid).unwrap();
        assert_eq!(pack["alert"]["event_id"], "MM1-2");
        assert_eq!(pack["audit_entries"].as_array().unwrap().iter().filter(|e| e["type"] == "live_decision").count(), 1);
    }
    assert_eq!(st.engine.as_ref().unwrap().n_seen, 2, "account state written once per payment");
}
