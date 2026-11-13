//! End to end API tests (ports of tests/test_api.py, test_messages.py, test_review.py, test_context.py,
//! test_session_network.py and test_branding.py). Each test builds its own app, so tests are independent.
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use rusetect::security::Signer;
use rusetect::paths;
use serde_json::{json, Value};
use tower::ServiceExt;

struct T {
    app: axum::Router,
}

fn nonce() -> String {
    format!("{:032x}", rand::random::<u128>())
}

impl T {
    fn new() -> Self {
        std::env::set_var("RUSETECT_RATE_LIMIT", "1000000");
        T { app: rusetect_server::build_app(rusetect_server::new_shared()) }
    }

    async fn req(&self, method: &str, path: &str, body: Option<Value>, headers: &[(&str, &str)]) -> (StatusCode, Value, String) {
        let mut b = Request::builder().method(method).uri(path);
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        let body = match body {
            Some(v) => {
                b = b.header("content-type", "application/json");
                Body::from(v.to_string())
            }
            None => Body::empty(),
        };
        let r = self.app.clone().oneshot(b.body(body).unwrap()).await.unwrap();
        let status = r.status();
        let bytes = r.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes).to_string();
        (status, serde_json::from_str(&text).unwrap_or(Value::Null), text)
    }
    async fn get(&self, path: &str) -> Value {
        self.req("GET", path, None, &[]).await.1
    }
    async fn post(&self, path: &str, body: Value) -> Value {
        self.req("POST", path, Some(body), &[]).await.1
    }
    async fn review(&self, aid: &str, key: Option<&str>, action: &str, n: Option<&str>) -> (StatusCode, Value) {
        let nn = n.map(String::from).unwrap_or_else(nonce);
        let mut h = vec![("X-Nonce", nn.as_str())];
        if let Some(k) = key {
            h.push(("X-Analyst-Key", k));
        }
        let (s, v, _) = self.req("POST", &format!("/api/v2/alerts/{aid}/review"), Some(json!({"action": action})), &h).await;
        (s, v)
    }
}

const S09: &str = "The finance office has revised the vendor details. Please remit the balance to the replacement beneficiary ahead of the afternoon cutoff. Call 9876543210 or pay vendor.new@ybl.";
const SCAM: &str = "Sir, CBI has registered an FIR against your number. To avoid digital arrest transfer Rs 2,50,000 to the RBI verification account rbi.verify@paytm. Do not tell anyone.";
const LEGIT_OTP: &str = "482913 is your OTP for transaction of Rs 2,300 at MYNTRA on HDFC Bank card XX1234. Do not share this OTP with anyone.";
const ARREST: &str = "This is CBI cyber cell. You are under digital arrest for money laundering. Stay on the video call and transfer the amount to the safe account immediately. Do not tell anyone.";

fn pay() -> Value {
    json!({"timestamp": "2022-09-09 10:00", "from_bank": "070", "from_account": "CV1", "to_bank": "011", "to_account": "CM1", "amount_paid": 48000, "payment_format": "ACH"})
}

// ---------------------------------------------------------------- test_api.py
#[tokio::test]
async fn health_and_pages() {
    let t = T::new();
    assert_eq!(t.get("/api/health").await["engine_loaded"], true);
    for p in ["/", "/console", "/report", "/docs"] {
        assert_eq!(t.req("GET", p, None, &[]).await.0, StatusCode::OK, "{p}");
    }
}

#[tokio::test]
async fn single_payment() {
    let t = T::new();
    let r = t.post("/api/v2/payment", json!({"timestamp": "2022-09-09 10:00", "from_bank": "070", "from_account": "A1", "to_bank": "011",
                                            "to_account": "B2", "amount_paid": 120.0, "payment_format": "Cheque"})).await;
    assert!(["Approve", "Step-up verification", "Hold for analyst", "Block + STR draft"].contains(&r["decision"].as_str().unwrap()));
}

#[tokio::test]
async fn onboarding_gate() {
    let t = T::new();
    let samples: Value = serde_json::from_str(&std::fs::read_to_string(paths::models_dir().join("sample_applications.json")).unwrap()).unwrap();
    assert_eq!(t.post("/api/v2/onboard", samples["mule"].clone()).await["flag"], true);
    assert_eq!(t.post("/api/v2/onboard", samples["genuine"].clone()).await["flag"], false);
}

#[tokio::test]
async fn demo_replay() {
    let t = T::new();
    assert_eq!(t.post("/api/v2/demo/reset", json!({})).await["history_ingested"], 100_000);
    let mut n = Value::Null;
    for _ in 0..10 {
        n = t.post("/api/v2/demo/next?n=2000", json!({})).await;
    }
    assert_eq!(n["position"], 20_000);
    assert!(n["exits"]["L1"].as_i64() > n["exits"]["L2"].as_i64());
}

#[tokio::test]
async fn validation_errors_look_like_fastapi() {
    let t = T::new();
    let (s, v, _) = t.req("POST", "/api/v2/payment", Some(json!({"timestamp": "2022-09-09 10:00", "from_bank": 7, "amount_paid": -1})), &[]).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let types: Vec<&str> = v["detail"].as_array().unwrap().iter().map(|e| e["type"].as_str().unwrap()).collect();
    assert!(types.contains(&"string_type") && types.contains(&"missing") && types.contains(&"greater_than"));
    assert_eq!(t.req("GET", "/api/nope", None, &[]).await.0, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------- test_messages.py
async fn msg(t: &T, text: &str, account: &str, ts: &str) -> Value {
    t.post("/api/v2/message", json!({"text": text, "timestamp": ts, "bank": "070", "account": account})).await
}

#[tokio::test]
async fn scam_and_legit_messages() {
    let t = T::new();
    let s = msg(&t, SCAM, "M1", "2022-09-09 09:00").await;
    assert_eq!(s["band"], "high");
    assert_eq!(s["workflow"]["id"], "digital_arrest");
    assert_eq!(s["alert"]["notify"], true);
    assert!(s["entities"]["upi_ids"].as_array().unwrap().contains(&json!("rbi.verify@paytm")));
    for text in [LEGIT_OTP, "Bhai 500 bhej de PhonePe pe, kal lauta dunga.", "Your Myntra order is out for delivery today."] {
        let r = msg(&t, text, "M1", "2022-09-09 09:00").await;
        assert_eq!(r["band"], "safe", "{text}");
        assert_eq!(r["alert"]["notify"], false);
    }
}

#[tokio::test]
async fn alert_fatigue_dedup() {
    let t = T::new();
    let first = msg(&t, SCAM, "F1", "2022-09-09 08:00").await;
    let again = msg(&t, SCAM, "F1", "2022-09-09 09:00").await;
    assert_eq!(first["alert"]["notify"], true);
    assert_eq!(again["alert"]["notify"], false);
    assert!(again["alert"]["reason"].as_str().unwrap().contains("Already warned"));
}

#[tokio::test]
async fn lure_escalates_following_payment() {
    let t = T::new();
    msg(&t, SCAM, "P1", "2022-09-09 09:00").await;
    let p = json!({"timestamp": "2022-09-09 10:00", "from_bank": "070", "from_account": "P1", "to_bank": "011", "to_account": "NEWPAYEE", "amount_paid": 50.0, "payment_format": "Cheque"});
    let r = t.post("/api/v2/payment", p.clone()).await;
    assert_ne!(r["decision"], "Approve");
    assert_eq!(r["reasons"][0]["feature"], "Payer received a scam-like message");
    let mut other = p.clone();
    other["from_account"] = json!("Q7");
    other["to_bank"] = json!("paytm");
    other["to_account"] = json!("rbi.verify@paytm");
    other["timestamp"] = json!("2022-09-09 10:05");
    let r2 = t.post("/api/v2/payment", other).await;
    assert!(["Hold for analyst", "Block + STR draft"].contains(&r2["decision"].as_str().unwrap()));
    let s = t.get("/api/v2/alerts/summary").await;
    assert!(s["payments_escalated"].as_i64().unwrap() >= 2 && s["messages"].as_i64().unwrap() >= 1);
}

// ---------------------------------------------------------------- test_review.py
#[tokio::test]
async fn alert_review_audit_evidence() {
    let t = T::new();
    t.post("/api/v2/demo/reset?warm=false", json!({})).await;
    let r = t.post("/api/v2/assess", json!({"timestamp": "2022-09-09 10:00", "from_bank": "070", "from_account": "RV1", "to_bank": "011",
                                           "to_account": "RM1", "amount_paid": 9300, "payment_format": "ACH", "message": S09, "consent": true})).await;
    let aid = r["alert_id"].as_str().unwrap().to_string();
    assert_ne!(r["shown_decision"], "Block + STR draft");
    let (s, _, _) = t.req("POST", &format!("/api/v2/alerts/{aid}/review"), Some(json!({"action": "approve_block"})), &[]).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(t.review(&aid, Some("demo-viewer"), "approve_block", None).await.0, StatusCode::FORBIDDEN);
    assert_eq!(t.review(&aid, Some("demo-analyst-1"), "approve_block", Some("fixed-nonce-123")).await.1["status"], "block_pending_second_approval");
    assert_eq!(t.review(&aid, Some("demo-analyst-2"), "approve_block", Some("fixed-nonce-123")).await.0, StatusCode::CONFLICT);
    assert_eq!(t.review(&aid, Some("demo-analyst-1"), "approve_block", None).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(t.review(&aid, Some("demo-analyst-2"), "approve_block", None).await.1["status"], "blocked");
    let au = t.get("/api/v2/audit").await;
    assert!(au["chain_valid"] == true && au["entries"].as_array().unwrap().len() >= 3);
    let ev = t.get(&format!("/api/v2/alerts/{aid}/evidence")).await;
    assert!(ev["pack_hash"].is_string() && ev["alert"]["payment"].get("message").is_none());
    let public = t.get("/api/v2/keys/public").await["public_key_b64"].as_str().unwrap().to_string();
    assert!(Signer::verify(&json!({"pack_hash": ev["pack_hash"], "alert_id": aid}), ev["signature"]["signature"].as_str().unwrap(), &public));
    let con = t.get("/api/v2/consortium").await;
    assert_eq!(con["entries"].as_array().unwrap().last().unwrap()["registry"], "mule_account");
    let s = ev.to_string();
    assert!(!s.contains("9876543210") && !s.contains("vendor.new@ybl"));
}

#[tokio::test]
async fn block_and_release_are_final() {
    let t = T::new();
    t.post("/api/v2/demo/reset?warm=false", json!({})).await;
    let mut ids = vec![];
    for acct in ["RF1", "RF2"] {
        let r = t.post("/api/v2/assess", json!({"timestamp": "2022-09-09 10:00", "from_bank": "070", "from_account": acct, "to_bank": "011",
                                               "to_account": "RM1", "amount_paid": 9300, "payment_format": "ACH", "message": S09, "consent": true})).await;
        ids.push(r["alert_id"].as_str().unwrap().to_string());
    }
    // a two-analyst block cannot be released afterwards
    t.review(&ids[0], Some("demo-analyst-1"), "approve_block", None).await;
    t.review(&ids[0], Some("demo-analyst-2"), "approve_block", None).await;
    assert_eq!(t.review(&ids[0], Some("demo-analyst-1"), "release", None).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(t.get(&format!("/api/v2/alerts/{}/evidence", ids[0])).await["alert"]["status"], "blocked");
    // an approval before a release cannot combine with one after it
    t.review(&ids[1], Some("demo-analyst-1"), "approve_block", None).await;
    t.review(&ids[1], Some("demo-analyst-1"), "release", None).await;
    assert_eq!(t.review(&ids[1], Some("demo-analyst-2"), "approve_block", None).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(t.get(&format!("/api/v2/alerts/{}/evidence", ids[1])).await["alert"]["status"], "released");
}

#[tokio::test]
async fn block_stands_when_consortium_ledger_is_full() {
    let shared = rusetect_server::new_shared();
    let t = T { app: rusetect_server::build_app(shared.clone()) };
    t.post("/api/v2/demo/reset?warm=false", json!({})).await;
    shared.state.lock().unwrap().consortium.ledger = vec![json!({}); rusetect::consortium::MAX_LEDGER];
    let r = t.post("/api/v2/assess", json!({"timestamp": "2022-09-09 10:00", "from_bank": "070", "from_account": "LF1", "to_bank": "011",
                                           "to_account": "RM1", "amount_paid": 9300, "payment_format": "ACH", "message": S09, "consent": true})).await;
    let aid = r["alert_id"].as_str().unwrap().to_string();
    t.review(&aid, Some("demo-analyst-1"), "approve_block", None).await;
    let (s, a) = t.review(&aid, Some("demo-analyst-2"), "approve_block", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!((a["status"].as_str(), a["published"].as_bool()), (Some("blocked"), Some(false)));
    assert!(a["publish_error"].as_str().unwrap().contains("full"));
    assert_eq!(a["actions"].as_array().unwrap().last().unwrap()["action"], "consortium_publish_failed");
}

#[tokio::test]
async fn customer_answer_cannot_overwrite_an_analyst_hold() {
    let t = T::new();
    t.post("/api/v2/demo/reset?warm=false", json!({})).await;
    let r = t.post("/api/v2/assess", json!({"timestamp": "2022-09-09 10:00", "from_bank": "070", "from_account": "RV1", "to_bank": "011",
                                           "to_account": "RM1", "amount_paid": 9300, "payment_format": "ACH", "message": S09, "consent": true})).await;
    let aid = r["alert_id"].as_str().unwrap().to_string();
    assert_eq!(t.review(&aid, Some("demo-analyst-1"), "hold", None).await.1["status"], "held");
    let entries = t.get("/api/v2/audit").await["entries"].as_array().unwrap().len();
    let (s, _, _) = t.req("POST", &format!("/api/v2/alerts/{aid}/customer"), Some(json!({"answer": "continue"})), &[]).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(t.get(&format!("/api/v2/alerts/{aid}/evidence")).await["alert"]["status"], "held");
    assert_eq!(t.get("/api/v2/audit").await["entries"].as_array().unwrap().len(), entries, "a refused answer leaves no audit entry");
}

#[tokio::test]
async fn reverify_and_duress_cannot_undo_analyst_decisions() {
    let t = T::new();
    t.post("/api/v2/demo/reset?warm=false", json!({})).await;
    let mut ids = vec![];
    for acct in ["RV2", "RV3", "RV4"] {
        let r = t.post("/api/v2/assess", json!({"timestamp": "2022-09-09 10:00", "from_bank": "070", "from_account": acct, "to_bank": "011",
                                               "to_account": "RM1", "amount_paid": 9300, "payment_format": "ACH", "message": S09, "consent": true})).await;
        ids.push(r["alert_id"].as_str().unwrap().to_string());
    }
    // held -> reverify refused
    t.review(&ids[0], Some("demo-analyst-1"), "hold", None).await;
    let (s, _, _) = t.req("POST", &format!("/api/v2/alerts/{}/reverify", ids[0]), Some(json!({})), &[]).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(t.get(&format!("/api/v2/alerts/{}/evidence", ids[0])).await["alert"]["status"], "held");
    // blocked and released -> duress recorded, status unchanged
    t.review(&ids[1], Some("demo-analyst-1"), "approve_block", None).await;
    t.review(&ids[1], Some("demo-analyst-2"), "approve_block", None).await;
    t.review(&ids[2], Some("demo-analyst-1"), "release", None).await;
    t.post("/api/v2/duress/register", json!({"account": "D2", "normal_pin": "2468", "duress_pin": "8642", "trusted_contacts": ["son"]})).await;
    for (aid, want) in [(&ids[1], "blocked"), (&ids[2], "released")] {
        let r = t.post("/api/v2/payments/confirm", json!({"account": "D2", "pin": "8642", "alert_id": aid})).await;
        assert_eq!((r["funds"].as_str(), r["alert_status_unchanged"].as_bool()), (Some("held"), Some(true)));
        assert_eq!(t.get(&format!("/api/v2/alerts/{aid}/evidence")).await["alert"]["status"], want);
    }
}

// ---------------------------------------------------------------- test_context.py
#[tokio::test]
async fn assess_endpoint_veto_and_consent() {
    let t = T::new();
    t.post("/api/v2/demo/reset?warm=false", json!({})).await;
    let body = json!({"timestamp": "2022-09-09 10:00", "from_bank": "070", "from_account": "VX1", "to_bank": "011", "to_account": "MX1",
        "amount_paid": 9300, "payment_format": "ACH",
        "message": "The finance office has revised the vendor details. Please remit the balance to the replacement beneficiary ahead of the afternoon cutoff. Call 9876543210."});
    let mut b1 = body.clone();
    b1["consent"] = json!(true);
    let r = t.post("/api/v2/assess", b1).await;
    assert_eq!(r["context"]["context"], 40);
    assert!(!r["context"]["redacted_text"].as_str().unwrap_or("").contains("9876543210"));
    assert!(r["final_decision"] != "Block + STR draft" || r["payment_decision"] == "Block + STR draft");
    let mut b2 = body.clone();
    b2["from_account"] = json!("VX2");
    b2["consent"] = json!(false);
    let r2 = t.post("/api/v2/assess", b2).await;
    assert_eq!(r2["context"]["context"], 0);
    assert!(r2["context"]["redacted_text"].is_null());
}

// ---------------------------------------------------------------- test_session_network.py
#[tokio::test]
async fn digital_arrest_end_to_end() {
    let t = T::new();
    t.post("/api/v2/demo/reset?warm=false", json!({})).await;
    let mut b = pay();
    b["message"] = json!(ARREST);
    b["consent"] = json!(true);
    b["session"] = json!({"on_call": true, "video_call": true, "call_minutes": 50, "screen_share": true, "payee_pasted": true});
    let r = t.post("/api/v2/assess", b).await;
    assert!(["Hold for analyst", "Block + STR draft"].contains(&r["final_decision"].as_str().unwrap()) || ["HIGH", "CRITICAL"].contains(&r["context"]["band"].as_str().unwrap()));
    assert!(r["risk_score"]["score"].as_i64().unwrap() >= 600);
    assert_eq!(r["intervention"]["show"], "pause");
    assert_eq!(r["intervention"]["question_type"], "arrest");
    let public = t.get("/api/v2/keys/public").await["public_key_b64"].as_str().unwrap().to_string();
    assert!(Signer::verify(&r["signature"]["signed_fields"], r["signature"]["signature"].as_str().unwrap(), &public));
    let aid = r["alert_id"].as_str().unwrap();
    assert_eq!(t.post(&format!("/api/v2/alerts/{aid}/customer"), json!({"answer": "stop"})).await["status"], "stopped_by_customer");
    let s = t.get(&format!("/api/v2/alerts/{aid}/str")).await;
    assert!(s["status"].as_str().unwrap().starts_with("DRAFT"));
    assert!(s["sections"]["D_grounds_of_suspicion"].as_array().unwrap().iter().any(|g| g.as_str().unwrap().contains("call")));
}

#[tokio::test]
async fn duress_and_identity_endpoints() {
    let t = T::new();
    let (s, _, _) = t.req("POST", "/api/v2/duress/register", Some(json!({"account": "D1", "normal_pin": "2468", "duress_pin": "8642", "trusted_contacts": ["son"]})), &[]).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(t.post("/api/v2/payments/confirm", json!({"account": "D1", "pin": "8642"})).await["funds"], "held");
    let r = t.post("/api/v2/identity/check", json!({"device_hash": "x", "enrolled_device_hashes": ["y"], "sim_swap_hours": 2})).await;
    assert_eq!(r["decision"], "deny_otp_reverify");
}

#[tokio::test]
async fn cross_bank_flag_reaches_the_score() {
    let t = T::new();
    let (s, _, _) = t.req("POST", "/api/v2/consortium/publish", Some(json!({"bank": "BANK-B", "identifier": "011:CM7"})), &[]).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _, _) = t.req("POST", "/api/v2/consortium/publish", Some(json!({"bank": "BANK-B", "identifier": "011:CM7"})), &[("X-Analyst-Key", "demo-analyst-1")]).await;
    assert_eq!(s, StatusCode::OK);
    let mut b = pay();
    b["to_account"] = json!("CM7");
    let r = t.post("/api/v2/assess", b).await;
    assert_eq!(r["network"]["flagged_by_consortium"], json!(["BANK-B"]));
    assert!(r["context"]["reasons"].as_array().unwrap().contains(&json!("account graph: coordinated_accounts")));
}

#[tokio::test]
async fn typing_check_endpoint_uses_the_verifier() {
    let t = T::new();
    let keys = ["th", "he", "in", "er", "an", "re", "on", "at"];
    for i in 0..3 {
        let d: serde_json::Map<String, Value> = keys.iter().enumerate().map(|(j, k)| (k.to_string(), json!(100 + 10 * j + i))).collect();
        t.post("/api/v2/behaviour/enrol", json!({"account": "TYPE1", "digraphs": d})).await;
    }
    let d: serde_json::Map<String, Value> = keys.iter().enumerate().map(|(j, k)| (k.to_string(), json!(2.1 * (100.0 + 10.0 * j as f64)))).collect();
    let r = t.post("/api/v2/behaviour/check", json!({"account": "TYPE1", "digraphs": d})).await;
    assert_eq!(r["method"], "verifier");
    assert_eq!(r["anomaly"], true);
}

// ---------------------------------------------------------------- test_branding.py
#[tokio::test]
async fn pages_use_the_name_and_api_identity() {
    let t = T::new();
    for (path, title) in [("/", "<title>RuseTect</title>"), ("/console", "<title>RuseTect Live Console</title>"), ("/report", "<title>RuseTect Benchmark</title>")] {
        let html = t.req("GET", path, None, &[]).await.2;
        assert!(html.contains(title) && html.contains("href=\"/favicon.svg\""), "{path}");
    }
    let h = t.get("/api/health").await;
    assert!(h["name"] == "RuseTect" && h["id"] == "rusetect" && h["engine_loaded"] == true);
    assert_eq!(t.get("/openapi.json").await["info"]["title"], "RuseTect API");
    assert_eq!(t.req("GET", "/favicon.svg", None, &[]).await.0, StatusCode::OK);
}

#[test]
fn config_has_every_threshold_the_engine_reads() {
    let cfg: Value = serde_json::from_str(&std::fs::read_to_string(paths::models_dir().join("cascade_config.json")).unwrap()).unwrap();
    let g = |k: &str| cfg[k].as_f64().unwrap_or_else(|| panic!("{k}"));
    assert!(g("l1_threshold") < g("verify_threshold") && g("verify_threshold") < g("hold_threshold") && g("hold_threshold") < g("block_threshold"));
}

#[test]
fn engine_rejects_config_without_thresholds() {
    let mut cfg: Value = serde_json::from_str(&std::fs::read_to_string(paths::models_dir().join("cascade_config.json")).unwrap()).unwrap();
    cfg.as_object_mut().unwrap().remove("verify_threshold");
    let err = rusetect::engine::CascadeConfig::from_json(cfg).unwrap_err().to_string();
    assert!(err.contains("verify_threshold"));
}

#[test]
fn engine_loads_all_live_models() {
    let e = rusetect::Engine::new(&paths::models_dir(), true).unwrap();
    assert!(e.models_loaded());
}

#[test]
fn report_embeds_committed_benchmark_exactly() {
    let html = std::fs::read_to_string(paths::static_dir().join("report.html")).unwrap();
    let start = html.find("const DATA = ").unwrap() + "const DATA = ".len();
    let end = start + html[start..].find(";\n").unwrap();
    let data: Value = serde_json::from_str(&html[start..end]).unwrap();
    let bench: Value = serde_json::from_str(&std::fs::read_to_string(paths::models_dir().join("benchmark_results.json")).unwrap()).unwrap();
    assert_eq!(data["aml"], bench["aml_models"]);
    assert_eq!(data["baf"], bench["baf_models"]);
    assert_eq!(data["cascade"], bench["cascade"]);
    assert_eq!(data["l0"], bench["l0_final"]);
}

#[test]
fn landing_page_benchmark_numbers_match_committed_results() {
    let html = std::fs::read_to_string(paths::static_dir().join("index.html")).unwrap();
    let bench: Value = serde_json::from_str(&std::fs::read_to_string(paths::models_dir().join("benchmark_results.json")).unwrap()).unwrap();
    let re = fancy_regex::Regex::new(r#"\["([^"]+)", ([0-9.]+)"#).unwrap();
    let shown: std::collections::HashMap<String, f64> = re.captures_iter(&html).filter_map(|c| c.ok())
        .map(|c| (c[1].to_string(), c[2].parse().unwrap())).collect();
    for (label, key) in [("XGBoost + graph features", "XGBoost+graph"), ("Random Forest + graph", "RandomForest+graph"), ("CatBoost + graph", "CatBoost+graph"),
                         ("LightGBM + graph", "LightGBM+graph"), ("Decision Tree + graph", "DecisionTree+graph"),
                         ("Logistic Regression + graph", "LogisticRegression+graph"), ("MLP", "MLP"), ("XGBoost, raw fields only", "XGBoost")] {
        let v = shown.get(label).unwrap_or_else(|| panic!("{label}"));
        assert!((v - bench["aml_models"][key]["pr_auc"].as_f64().unwrap()).abs() < 0.0005, "{label}");
    }
}

#[test]
fn no_legacy_names_anywhere_in_source() {
    use base64::Engine as _;
    let pat = String::from_utf8(base64::engine::general_purpose::STANDARD.decode("cmFrc2hhbXxjaXRhZGVsfGlpdFtcc18tXSpkZWxoaXx0cmFja1xzKjA/MlxifGFtYXpvbg==").unwrap()).unwrap();
    let re = fancy_regex::Regex::new(&format!("(?i){pat}")).unwrap();
    let mut hits = vec![];
    fn walk(d: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(d).unwrap().flatten() {
            let p = e.path();
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            if p.is_dir() {
                if ![".git", "target", "work", "data", "graphify-out"].contains(&name.as_str()) { // graphify-out: local, git-ignored graph cache
                    walk(&p, out);
                }
            } else {
                out.push(p);
            }
        }
    }
    let mut files = vec![];
    let root = paths::repo_root();
    walk(&root, &mut files);
    for p in files {
        let ext = p.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default();
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        if !["rs", "html", "md", "json", "ps1", "txt", "svg", "toml", "yml", "yaml", "sh"].contains(&ext.as_str()) && !["Dockerfile", ".gitignore"].contains(&name.as_str()) {
            continue;
        }
        if p.file_name().map(|n| n == "api.rs").unwrap_or(false) {
            continue;
        }
        // docs/research/ is the 9 Nov 2026 research audit, which discusses the project's history by its old names on
        // purpose; it was added after this guard and fails the original Python test in the same way.
        if p.strip_prefix(&root).map(|r| r.starts_with("docs/research")).unwrap_or(false) {
            continue;
        }
        let text = String::from_utf8_lossy(&std::fs::read(&p).unwrap()).to_string();
        for (n, line) in text.lines().enumerate() {
            if re.is_match(line).unwrap_or(false) {
                hits.push(format!("{}:{}", p.strip_prefix(&root).unwrap().display(), n + 1));
            }
        }
    }
    assert!(hits.is_empty(), "{hits:?}");
}

/// A payment without a format, or with a format or currency the models never saw, is rejected instead of being scored
/// with a silent default (research/out/slice_results.json section F measures what the old defaults did).
#[tokio::test]
async fn payment_format_and_currency_are_validated() {
    let t = T::new();
    let base = json!({"timestamp": "2022-09-09 10:00", "from_bank": "070", "from_account": "V1", "to_bank": "011", "to_account": "V2", "amount_paid": 100.0});
    let (s, v, _) = t.req("POST", "/api/v2/payment", Some(base.clone()), &[]).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(v["detail"][0]["type"], "missing");
    assert_eq!(v["detail"][0]["loc"], json!(["body", "payment_format"]));
    let mut p = base.clone();
    p["payment_format"] = json!("UPI");
    let (s, v, _) = t.req("POST", "/api/v2/payment", Some(p), &[]).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(v["detail"][0]["type"], "literal_error");
    let mut p = base.clone();
    p["payment_format"] = json!("Wire");
    p["payment_currency"] = json!("INR");
    let (s, v, _) = t.req("POST", "/api/v2/payment", Some(p.clone()), &[]).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(v["detail"][0]["loc"], json!(["body", "payment_currency"]));
    p["payment_currency"] = json!("Rupee");
    let (s, _, _) = t.req("POST", "/api/v2/payment", Some(p), &[]).await;
    assert_eq!(s, StatusCode::OK);
}

/// The validation lists must match the served model vocabulary.
#[test]
fn validation_lists_match_cascade_config() {
    let cfg: Value = serde_json::from_str(&std::fs::read_to_string(paths::models_dir().join("cascade_config.json")).unwrap()).unwrap();
    let keys = |k: &str| { let mut v: Vec<String> = cfg[k].as_object().unwrap().keys().cloned().collect(); v.sort(); v };
    let mut f: Vec<String> = rusetect_server::inputs::PAYMENT_FORMATS.iter().map(|s| s.to_string()).collect(); f.sort();
    let mut c: Vec<String> = rusetect_server::inputs::CURRENCIES.iter().map(|s| s.to_string()).collect(); c.sort();
    assert_eq!(f, keys("fmt_codes"));
    assert_eq!(c, keys("cur_codes"));
}

/// Demo keys only on request; with real analyst keys the server must not fall back to demo secrets.
#[test]
fn production_mode_requires_secrets() {
    use rusetect_server::missing_production_secrets as miss;
    let env = |pairs: &'static [(&'static str, &'static str)]| move |k: &str| pairs.iter().find(|(a, _)| *a == k).map(|(_, v)| v.to_string());
    assert_eq!(miss(env(&[])).len(), 1, "no keys and no explicit demo flag: refuse to start");
    assert_eq!(miss(env(&[("RUSETECT_DEMO", "true")])).len(), 1, "only the exact flag opens demo mode");
    assert!(miss(env(&[("RUSETECT_DEMO", "1")])).is_empty(), "explicit demo mode needs nothing");
    const K: &str = r#"{"0123456789abcdef": {"analyst": "asha", "role": "analyst"}}"#;
    assert_eq!(miss(env(&[("RUSETECT_ANALYST_KEYS", "{}")])).len(), 1, "keys that cannot work stop the start-up");
    assert_eq!(miss(env(&[("RUSETECT_ANALYST_KEYS", "")])).len(), 1);
    assert_eq!(miss(env(&[("RUSETECT_ANALYST_KEYS", K)])).len(), 3);
    assert_eq!(miss(env(&[("RUSETECT_ANALYST_KEYS", K), ("RUSETECT_SIGNING_KEY", "x"), ("RUSETECT_CONSORTIUM_SECRET", " ")])),
               vec!["RUSETECT_CONSORTIUM_SECRET", "RUSETECT_HASH_SALT"]);
    assert!(miss(env(&[("RUSETECT_ANALYST_KEYS", K), ("RUSETECT_SIGNING_KEY", "a"), ("RUSETECT_CONSORTIUM_SECRET", "b"), ("RUSETECT_HASH_SALT", "c")])).is_empty());
}

/// Format hardening and the structuring defence (research/hardened_eval.py, research/structuring_defence.py).
#[test]
fn hardened_risk_ignores_format_and_pairs_add_up() {
    use rusetect::engine::interp;
    use rusetect::{Engine, Payment};
    let m = (vec![0.0, 1.0, 2.0], vec![0.0, 0.5, 0.5]);
    assert_eq!((interp(&m, -1.0), interp(&m, 0.5), interp(&m, 3.0)), (0.0, 0.25, 0.5), "np.interp semantics");
    let pay = |fmt: &str, t: i64, amt: f64| Payment { from_bank: "070".into(), from_account: "S1".into(), to_bank: "011".into(), to_account: "D1".into(),
        amount_paid: amt, payment_currency: "US Dollar".into(), receiving_currency: "US Dollar".into(), payment_format: fmt.into(), tmin: t, eid: None };
    let score = |fmt: &str| Engine::new(&paths::models_dir(), true).unwrap().score_payment(&pay(fmt, 0, 9000.0));
    let (a, c) = (score("ACH"), score("Cheque"));
    assert!(a.contains_key("cascade_risk"), "hardening.json is loaded");
    assert_eq!(a["format_free_risk"], c["format_free_risk"], "the second opinion does not see the format");
    assert!(c["risk"].as_f64().unwrap() >= c["format_free_risk"].as_f64().unwrap());
    let mut e = Engine::new(&paths::models_dir(), true).unwrap();
    let parts: Vec<_> = (0..4).map(|i| e.score_payment(&pay("ACH", i * 30, 2250.0))).collect();
    assert!(!parts[0].contains_key("pair_risk") && parts[3].contains_key("pair_risk"));
    let r = |v: &serde_json::Map<String, Value>, k: &str| v[k].as_f64().unwrap();
    assert!(r(&parts[3], "pair_risk") >= r(&parts[3], "cascade_risk"), "noisy-OR never lowers the cascade risk");
}

#[test]
fn pair_window_expires_and_sweeps_idle_keys() {
    use rusetect::engine::PairWindow;
    let mut w = PairWindow::new();
    assert_eq!(w.push(1u32, 0, 0.5), (1, 0.5));
    assert_eq!(w.push(1, 30, 0.5), (2, 0.75), "noisy-OR of two halves");
    assert_eq!(w.push(1, 30 + 1440, 0.2).0, 1, "parts older than 24 h drop out");
    for k in 2..65_534u32 {
        w.push(k, 0, 0.1); // one-off pairs; 3 + 65,532 pushes so far
    }
    assert!(w.keys() >= 65_000);
    w.push(0, 5000, 0.1); // the 65,536th push, a day later, sweeps idle keys
    assert!(w.keys() <= 2, "idle pairs are swept, kept {}", w.keys());
}

#[tokio::test]
async fn infinite_and_unicode_inputs_are_refused_cleanly() {
    let t = T::new();
    let mut b = pay();
    b["amount_paid"] = json!("inf");
    let (s, _, _) = t.req("POST", "/api/v2/payment", Some(b), &[]).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "inf would poison the account's running sums");
    let mut b = pay();
    b["timestamp"] = json!("２０２２-09-09 10:00");
    let (s, _, _) = t.req("POST", "/api/v2/payment", Some(b), &[]).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "full-width digits: a 422, not a dropped connection");
    let (s, _, _) = t.req("POST", "/api/v2/behaviour/enrol", Some(json!({"account": "T1", "digraphs": {"ab": "nan"}})), &[]).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
}

#[test]
fn late_payments_leave_the_window_on_time() {
    use rusetect::{Engine, Payment};
    let mut e = Engine::new(&paths::models_dir(), false).unwrap();
    let pay = |t: i64, to: &str| Payment { from_bank: "1".into(), from_account: "S".into(), to_bank: "2".into(), to_account: to.into(), amount_paid: 10.0,
        payment_currency: "US Dollar".into(), receiving_currency: "US Dollar".into(), payment_format: "ACH".into(), tmin: t, eid: None };
    e.features(&pay(10_000, "A"));
    e.features(&pay(100, "B")); // arrives late, a week earlier
    let f = e.features(&pay(10_100, "C"));
    assert_eq!(f.get("s_cnt_24h"), 2.0, "the late payment is older than 24 h and must not count (as in the Redis zsets)");
}

#[test]
fn duress_registration_cannot_be_overwritten_and_locks_out() {
    let mut v = rusetect::intervention::DuressVault::new();
    v.register("X", "1234", "4321", &[]).unwrap();
    assert!(v.register("X", "9999", "8888", &["attacker".into()]).is_err());
    for _ in 0..rusetect::intervention::MAX_FAILED_PINS {
        v.confirm("X", "0000");
    }
    assert_eq!(v.confirm("X", "1234")["result"], "locked");
}

/// PRAMAAN account-takeover authentication: an everyday payment from the owner's own device passes; a SIM swap on a
/// new device is never sent an SMS code; a failed challenge blocks the next attempt.
#[tokio::test]
async fn authenticate_owner_and_sim_swap() {
    let t = T::new();
    let base = |ts: f64, dev: &str, sim: Value, amount: f64, payee: &str| json!({"user_id": "asha", "ts": ts,
        "device": {"device_id": dev, "platform": "android", "sim_change_hours": sim},
        "network": {"ip": "49.229.31.106", "city": "Mumbai", "lat": 19.07, "lon": 72.87},
        "behaviour": {"time_to_submit_s": 20, "input_events": 30}, "transaction": {"amount": amount, "payee_id": payee}});
    let day = 86400.0;
    // build a little history on her own phone: the first payment is device binding, then she becomes known
    let first = t.post("/api/v2/authenticate", base(1.767e9, "phone-a", Value::Null, 600.0, "grocer")).await;
    assert_eq!(first["decision"], "challenge", "first payment from a never-seen device is stepped up");
    let v = t.post("/api/v2/challenge/verify", json!({"decision_id": first["decision_id"], "passed": true})).await;
    assert_eq!(v["decision"], "allow");
    for i in 1..12 {
        t.post("/api/v2/authenticate", base(1.767e9 + i as f64 * day, "phone-a", Value::Null, 600.0, "grocer")).await;
    }
    let everyday = t.post("/api/v2/authenticate", base(1.767e9 + 12.0 * day, "phone-a", Value::Null, 640.0, "grocer")).await;
    assert_eq!(everyday["decision"], "allow", "{everyday}");
    let swap = t.post("/api/v2/authenticate", base(1.767e9 + 12.2 * day, "attacker-phone", json!(1.0), 60000.0, "mule-9")).await;
    assert_ne!(swap["decision"], "allow", "{swap}");
    assert_ne!(swap["challenge_type"], "sms_otp", "after a SIM swap the SMS code would reach the attacker");
    assert!(!swap["reasons"].as_array().unwrap().is_empty() && !swap["message_hi"].as_str().unwrap().is_empty());
    if swap["decision"] == "challenge" {
        t.post("/api/v2/challenge/verify", json!({"decision_id": swap["decision_id"], "passed": false})).await;
        let again = t.post("/api/v2/authenticate", base(1.767e9 + 12.21 * day, "attacker-phone", json!(1.2), 100.0, "mule-9")).await;
        assert_eq!((again["decision"].as_str(), again["rule"].as_str()), (Some("block"), Some("recent_failed_challenge")));
    }
    let (s, _, _) = t.req("POST", "/api/v2/authenticate", Some(json!({"ts": 1})), &[]).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, _, _) = t.req("POST", "/api/v2/challenge/verify", Some(json!({"decision_id": "nope", "passed": true})), &[]).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

/// RuseTect's consortium mule registry feeds PRAMAAN's payee-risk signal.
#[tokio::test]
async fn consortium_flag_raises_payee_risk_in_authentication() {
    let t = T::new();
    let pay = |payee: &str| json!({"user_id": "ravi", "ts": 1.767e9, "device": {"device_id": "phone-r"},
        "transaction": {"amount": 2500, "payee_id": payee}});
    let clean = t.post("/api/v2/authenticate", pay("011:CLEAN1")).await;
    assert_eq!(clean["features"]["payee_risk"], Value::Null);
    let (s, _, _) = t.req("POST", "/api/v2/consortium/publish", Some(json!({"bank": "BANK-B", "identifier": "011:MULE7"})), &[("X-Analyst-Key", "demo-analyst-1")]).await;
    assert_eq!(s, StatusCode::OK);
    let r = t.post("/api/v2/authenticate", pay("011:MULE7")).await;
    assert_eq!(r["features"]["payee_risk"], json!(0.9));
    assert_eq!(r["payee_flagged_by_consortium"], json!(["BANK-B"]));
    assert!(r["risk_score"].as_f64().unwrap() >= clean["risk_score"].as_f64().unwrap(), "a flagged payee never lowers the risk");
}

#[tokio::test]
async fn future_timestamps_are_refused() {
    let t = T::new();
    let mut p = pay();
    p["timestamp"] = json!("2999-01-01 10:00");
    assert_eq!(t.req("POST", "/api/v2/payment", Some(p), &[]).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(t.req("POST", "/api/v2/payment", Some(pay()), &[]).await.0, StatusCode::OK);
}

#[test]
fn campaigns_use_only_the_newest_alerts() {
    use rusetect::campaigns::{build_recent, MAX_ALERTS};
    let alerts: indexmap::IndexMap<String, Value> = (0..=MAX_ALERTS).map(|i| (format!("A{i}"), json!({"alert_id": format!("A{i}"), "status": "open",
        "payment": {"to_bank": "011", "to_account": "P1", "from_account": format!("V{i}"), "amount_paid": 1, "timestamp": "2022-09-01 10:00"}}))).collect();
    let cs = build_recent(&alerts);
    let ids = cs[0]["alerts"].as_array().unwrap();
    assert_eq!(ids.len(), MAX_ALERTS);
    assert!(!ids.contains(&json!("A0")) && ids.contains(&json!(format!("A{MAX_ALERTS}"))));
}

#[tokio::test]
async fn payee_check_verdicts_and_validation() {
    let t = T::new();
    let ev: Value = serde_json::from_str(&std::fs::read_to_string(paths::repo_root().join("research/out/payee_check_eval.json")).unwrap()).unwrap();
    assert_eq!(ev["adopted"], true, "the endpoint is served only when the pre-registered evaluation adopted it");
    let r = t.post("/api/v2/payee/check", json!({"entered_name": "Mr John Smith", "account_name": "John Smith", "account_type": "individual"})).await;
    assert_eq!(r, json!({"verdict": "match", "score": 1.0, "reason": "exact match"}));
    let r = t.post("/api/v2/payee/check", json!({"entered_name": "J Smith", "account_name": "John Smith"})).await;
    assert_eq!((r["verdict"].as_str(), r["suggested_name"].as_str()), (Some("close_match"), Some("John Smith")));
    let r = t.post("/api/v2/payee/check", json!({"entered_name": "John Smith", "account_name": "John Smith Trading Ltd", "account_type": "business"})).await;
    assert_eq!(r["verdict"], "close_match");
    assert!(r["reason"].as_str().unwrap().starts_with("the account is a business account"));
    let r = t.post("/api/v2/payee/check", json!({"entered_name": "Aldric Venthorpe", "account_name": "Mary Jones"})).await;
    assert_eq!((r["verdict"].as_str(), r.get("suggested_name")), (Some("no_match"), None));
    for bad in [json!({}), json!({"entered_name": "A"}), json!({"entered_name": "", "account_name": "B"}), json!({"entered_name": "x".repeat(301), "account_name": "B"}),
                json!({"entered_name": "Dr.", "account_name": "B"}), json!({"entered_name": "A", "account_name": "B", "account_type": "trust"})] {
        assert_eq!(t.req("POST", "/api/v2/payee/check", Some(bad.clone()), &[]).await.0, StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
    }
}

#[tokio::test]
async fn sanctions_screen_finds_variants_and_validates() {
    let t = T::new();
    let r = t.post("/api/v2/sanctions/screen", json!({"name": "VENTHORPE, Mr Aldric"})).await;
    assert_eq!((r["hit"].as_bool(), r["matches"][0]["list_id"].as_str(), r["matches"][0]["score"].as_f64()), (Some(true), Some("TEST-0001"), Some(1.0)));
    assert_eq!(r["matches"][0]["program"], "FICTITIOUS-TEST");
    let thr: Value = serde_json::from_str(&std::fs::read_to_string(paths::repo_root().join("research/out/sanctions_eval_v2.json")).unwrap()).unwrap();
    assert_eq!(thr["adopted"], true);
    assert_eq!(r["threshold"], thr["v2"]["threshold"], "the served threshold is the fitted v2 one");
    let r = t.post("/api/v2/sanctions/screen", json!({"name": "Rafael Montecervo"})).await;
    assert_eq!((r["hit"].as_bool(), r["matches"][0]["list_id"].as_str()), (Some(true), Some("TEST-0009")), "v2 matches a dropped middle name");
    let r = t.post("/api/v2/sanctions/screen", json!({"name": "Maria Garcia", "type": "individual"})).await;
    assert_eq!(r["hit"], false);
    let r = t.post("/api/v2/sanctions/screen", json!({"name": "Velmora Shiping Co", "type": "entity"})).await;
    assert!(r["matches"].as_array().unwrap().iter().all(|m| m["list_id"].as_str().unwrap() >= "TEST-0029"), "type filters the list");
    for bad in [json!({}), json!({"name": ""}), json!({"name": "x".repeat(301)}), json!({"name": "Dr."}), json!({"name": "A B", "type": "vessel"})] {
        assert_eq!(t.req("POST", "/api/v2/sanctions/screen", Some(bad.clone()), &[]).await.0, StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
    }
}

#[tokio::test]
async fn oversized_strings_are_refused_before_they_reach_state() {
    let t = T::new();
    let before = t.get("/api/v2/stats").await["accounts_tracked"].clone();
    let mut b = pay();
    b["from_account"] = json!("A".repeat(129));
    let (s, v, _) = t.req("POST", "/api/v2/assess", Some(b), &[]).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(v["detail"][0]["type"], "string_too_long");
    assert_eq!(t.get("/api/v2/stats").await["accounts_tracked"], before);
    let auth = json!({"user_id": "u1", "ts": 1.767e9, "device": {"device_id": "d".repeat(600)}});
    assert_eq!(t.req("POST", "/api/v2/authenticate", Some(auth), &[]).await.0, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn gets_are_rate_limited_too() {
    let shared = rusetect_server::new_shared();
    *shared.rate.lock().unwrap() = rusetect::security::RateLimiter::new(3, 10.0);
    let t = T { app: rusetect_server::build_app(shared) };
    for _ in 0..3 {
        assert_eq!(t.req("GET", "/api/v2/consortium", None, &[]).await.0, StatusCode::OK);
    }
    assert_eq!(t.req("GET", "/api/v2/consortium", None, &[]).await.0, StatusCode::TOO_MANY_REQUESTS);
}
