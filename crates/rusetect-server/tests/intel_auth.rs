//! Role checks on the website endpoints with configured analyst keys (not the public demo): its own test binary, since
//! the keys are read once per process.
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

const KEYS: &str = r#"{"viewer-key-0123456789": {"analyst": "ops.view", "role": "viewer"}, "channel-key-0123456789": {"analyst": "core.banking", "role": "channel"},
    "analyst-key-0123456789": {"analyst": "asha", "role": "analyst"}, "second-key-0123456789": {"analyst": "ravi", "role": "analyst"},
    "supervisor-key-0123456789": {"analyst": "meera", "role": "supervisor"}}"#;

async fn call(app: &axum::Router, method: &str, path: &str, key: Option<&str>, body: Option<Value>) -> (StatusCode, Value) {
    let mut b = Request::builder().method(method).uri(path).header("content-type", "application/json").header("x-nonce", format!("{:032x}", rand::random::<u128>()));
    if let Some(k) = key {
        b = b.header("x-analyst-key", k);
    }
    let r = app.clone().oneshot(b.body(body.map(|v| Body::from(v.to_string())).unwrap_or_else(Body::empty)).unwrap()).await.unwrap();
    let st = r.status();
    (st, serde_json::from_slice(&r.into_body().collect().await.unwrap().to_bytes()).unwrap_or(Value::Null))
}

#[tokio::test]
async fn configured_keys_gate_reads_and_case_work_by_role() {
    std::env::set_var("RUSETECT_ANALYST_KEYS", KEYS);
    std::env::set_var("RUSETECT_RATE_LIMIT", "1000000");
    let app = rusetect_server::build_app(rusetect_server::new_shared());
    // the mule fixture through the channel key: the first forward the engine flags opens an alert
    let pay = |from: &str, to: &str, min: usize, amount: f64| json!({"timestamp": format!("2022-09-09 {:02}:{:02}", 10 + min / 60, min % 60),
        "from_bank": if from == "MULE" { "0200" } else { "0100" }, "from_account": from, "to_bank": if to == "MULE" { "0200" } else { "0300" }, "to_account": to,
        "amount_paid": amount, "payment_currency": "Rupee", "payment_format": "ACH"});
    assert_eq!(call(&app, "POST", "/api/v2/assess", Some("viewer-key-0123456789"), Some(pay("V0", "MULE", 0, 9500.0))).await.0, StatusCode::FORBIDDEN);
    let mut fwd = Value::Null;
    for (i, amt) in [9500.0, 12000.0, 18000.0, 14500.0, 9900.0, 16500.0, 11200.0, 17800.0, 10400.0, 15000.0, 13300.0, 19500.0].into_iter().enumerate() {
        assert_eq!(call(&app, "POST", "/api/v2/assess", Some("channel-key-0123456789"), Some(pay(&format!("V{i}"), "MULE", i * 7, amt))).await.0, StatusCode::OK);
        fwd = call(&app, "POST", "/api/v2/assess", Some("channel-key-0123456789"), Some(pay("MULE", "COLL", i * 7 + 1, (amt * 98.0f64).round() / 100.0))).await.1;
        if fwd["alert_id"].is_string() {
            break;
        }
    }
    let alert = fwd["alert_id"].as_str().unwrap_or_else(|| panic!("a forward is flagged: {fwd}")).to_string();

    // reads name accounts: no key and an unknown key are refused, every role reads
    let reads = ["/api/v2/dashboard".to_string(), "/api/v2/transactions".into(), format!("/api/v2/transactions/{alert}"), "/api/v2/graph?account=MULE".into(),
                 "/api/v2/investigations".into()];
    for path in &reads {
        assert_eq!(call(&app, "GET", path, None, None).await.0, StatusCode::UNAUTHORIZED, "{path}");
        assert_eq!(call(&app, "GET", path, Some("wrong-key-0123456789"), None).await.0, StatusCode::UNAUTHORIZED, "{path}");
        for key in ["viewer-key-0123456789", "channel-key-0123456789", "analyst-key-0123456789", "supervisor-key-0123456789"] {
            assert_eq!(call(&app, "GET", path, Some(key), None).await.0, StatusCode::OK, "{path} {key}");
        }
    }
    // the stored alert is what /assess answered: an assess alert is a transaction of source `assess`
    let (_, t) = call(&app, "GET", &format!("/api/v2/transactions/{alert}"), Some("viewer-key-0123456789"), None).await;
    assert_eq!((t["transaction"]["source"].as_str(), &t["transaction"]["risk"], &t["transaction"]["reasons"]), (Some("assess"), &fwd["payment"]["risk"], &fwd["payment"]["reasons"]));

    // case work: analysts and supervisors only
    let triage = format!("/api/v2/alerts/{alert}/triage");
    let open = json!({"title": "forward", "alert_ids": [alert]});
    for (key, want) in [(None, StatusCode::UNAUTHORIZED), (Some("wrong-key-0123456789"), StatusCode::UNAUTHORIZED),
                        (Some("viewer-key-0123456789"), StatusCode::FORBIDDEN), (Some("channel-key-0123456789"), StatusCode::FORBIDDEN)] {
        assert_eq!(call(&app, "POST", &triage, key, Some(json!({"priority": "high"}))).await.0, want, "{key:?}");
        assert_eq!(call(&app, "POST", "/api/v2/investigations", key, Some(open.clone())).await.0, want, "{key:?}");
        assert_eq!(call(&app, "POST", "/api/v2/investigations/INV-00001/actions", key, Some(json!({"action": "comment", "text": "x"}))).await.0, want, "{key:?}");
    }
    assert_eq!(call(&app, "POST", &triage, Some("analyst-key-0123456789"), Some(json!({"assignee": "ravi"}))).await.0, StatusCode::FORBIDDEN, "not hers to give");
    assert_eq!(call(&app, "POST", &triage, Some("supervisor-key-0123456789"), Some(json!({"assignee": "core.banking"}))).await.0, StatusCode::BAD_REQUEST, "a channel is not an analyst");
    assert_eq!(call(&app, "POST", &triage, Some("analyst-key-0123456789"), Some(json!({"assignee": "asha", "priority": "high"}))).await.1["workflow"]["assignee"], "asha");
    let (s, c) = call(&app, "POST", "/api/v2/investigations", Some("analyst-key-0123456789"), Some(open)).await;
    assert_eq!(s, StatusCode::OK, "{c}");
    let actions = format!("/api/v2/investigations/{}/actions", c["case_id"].as_str().unwrap());
    let step = |key: &'static str, action: &'static str| { let (app, actions) = (app.clone(), actions.clone());
        async move { call(&app, "POST", &actions, Some(key), Some(json!({"action": action, "text": "reviewed"}))).await.0 } };
    assert_eq!(step("analyst-key-0123456789", "request_close").await, StatusCode::OK);
    assert_eq!(step("analyst-key-0123456789", "approve_close").await, StatusCode::BAD_REQUEST, "maker and checker differ");
    assert_eq!(step("second-key-0123456789", "approve_close").await, StatusCode::OK);
    assert_eq!(step("second-key-0123456789", "reopen").await, StatusCode::FORBIDDEN);
    assert_eq!(step("supervisor-key-0123456789", "reopen").await, StatusCode::OK);
}
