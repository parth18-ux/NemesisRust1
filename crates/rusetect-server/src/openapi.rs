//! OpenAPI 3.1 description of the API (FastAPI generated this from the pydantic models; here it is written out)
//! and the Swagger UI page served at /docs.
use rusetect::branding::{NAME, VERSION};
use serde_json::{json, Map, Value};

fn s(t: &str) -> Value {
    json!({"type": t})
}

fn schemas() -> Value {
    let payment = json!({
        "timestamp": {"type": "string", "description": "YYYY-MM-DD HH:MM (UTC)", "examples": ["2022-09-09 10:00"]},
        "from_bank": s("string"), "from_account": s("string"), "to_bank": s("string"), "to_account": s("string"),
        "amount_paid": {"type": "number", "exclusiveMinimum": 0},
        "payment_currency": {"type": "string", "default": "US Dollar"},
        "receiving_currency": {"anyOf": [s("string"), s("null")]},
        "payment_format": {"type": "string", "default": "ACH", "description": "ACH, Bitcoin, Cash, Cheque, Credit Card, Reinvestment, Wire"}
    });
    let req = json!(["timestamp", "from_bank", "from_account", "to_bank", "to_account", "amount_paid"]);
    let mut assess = payment.as_object().unwrap().clone();
    assess.insert("message".into(), json!({"anyOf": [{"type": "string", "maxLength": 5000}, s("null")], "description": "Message or call transcript the payer received (optional)"}));
    assess.insert("consent".into(), json!({"type": "boolean", "default": false, "description": "Payer opted in to message context; without it the text is ignored and not stored"}));
    assess.insert("use_intent_model".into(), json!({"type": "boolean", "default": true}));
    assess.insert("session".into(), json!({"anyOf": [{"$ref": "#/components/schemas/SessionIn"}, s("null")]}));
    json!({
        "PaymentIn": {"type": "object", "properties": payment, "required": req},
        "AssessIn": {"type": "object", "properties": assess, "required": req},
        "EventIn": {"type": "object", "properties": {"type": {"type": "string", "description": "password_reset, new_device, new_payee, limit_raise or transfer"},
                    "minute": {"type": "integer"}}, "required": ["type", "minute"]},
        "SessionIn": {"type": "object", "description": "Signals from the bank app at payment time (session and person levels). All optional.", "properties": {
            "session_id": {"anyOf": [s("string"), s("null")]}, "on_call": s("boolean"), "call_minutes": s("number"), "video_call": s("boolean"),
            "screen_share": s("boolean"), "remote_access_app": s("boolean"), "payee_pasted": s("boolean"), "hesitation_s": s("number"),
            "app_hidden_s": s("number"), "typing_anomaly": s("boolean"),
            "device_hash": {"anyOf": [s("string"), s("null")], "description": "salted device hash, never the raw IMEI"},
            "enrolled_device_hashes": {"type": "array", "items": s("string")},
            "sim_swap_hours": {"anyOf": [s("number"), s("null")], "description": "hours since the last SIM swap, from the telecom check"},
            "events": {"type": "array", "items": {"$ref": "#/components/schemas/EventIn"}}}},
        "MessageIn": {"type": "object", "required": ["text"], "properties": {
            "text": {"type": "string", "minLength": 1, "maxLength": 5000, "examples": ["Your SBI account will be blocked today. Update PAN at http://sbi-kyc-update.in"]},
            "channel": {"type": "string", "default": "sms", "description": "sms, whatsapp, email, call_transcript"},
            "timestamp": {"anyOf": [s("string"), s("null")], "description": "YYYY-MM-DD HH:MM (UTC); defaults to now"},
            "bank": {"anyOf": [s("string"), s("null")]}, "account": {"anyOf": [s("string"), s("null")]}}},
        "ReviewIn": {"type": "object", "required": ["action"], "properties": {
            "action": {"type": "string", "description": "release, verify, hold or approve_block (a block needs two different authenticated analysts)"},
            "note": {"type": "string", "maxLength": 500, "default": ""}, "analyst": {"anyOf": [s("string"), s("null")], "description": "ignored: the analyst is identified by the X-Analyst-Key header"}}},
        "CustomerIn": {"type": "object", "required": ["answer"], "properties": {"answer": {"type": "string", "description": "stop or continue"}}},
        "ReverifyIn": {"type": "object", "properties": {"method": {"type": "string", "default": "video_kyc", "description": "video_kyc or branch"}}},
        "DuressRegisterIn": {"type": "object", "required": ["account", "normal_pin", "duress_pin"], "properties": {"account": s("string"), "normal_pin": s("string"),
            "duress_pin": s("string"), "trusted_contacts": {"type": "array", "items": s("string")}}},
        "ConfirmIn": {"type": "object", "required": ["account", "pin"], "properties": {"account": s("string"), "pin": s("string"), "alert_id": {"anyOf": [s("string"), s("null")]}}},
        "DigraphIn": {"type": "object", "required": ["account", "digraphs"], "properties": {"account": s("string"),
            "digraphs": {"type": "object", "description": "key pair -> milliseconds, e.g. {\"th\": 112, \"he\": 98}"}}},
        "ConsortiumCheckIn": {"type": "object", "required": ["identifier"], "properties": {"registry": {"type": "string", "default": "mule_account"},
            "identifier": {"type": "string", "description": "for mule_account: bank:account"}}},
        "ConsortiumPublishIn": {"type": "object", "required": ["identifier"], "properties": {"bank": {"type": "string", "default": "BANK-B"},
            "registry": {"type": "string", "default": "mule_account"}, "identifier": s("string"), "reason": {"type": "string", "default": "confirmed mule"}}},
        "TriageIn": {"type": "object", "description": "At least one field.", "properties": {"assignee": s("string"),
            "priority": {"type": "string", "enum": ["low", "medium", "high", "critical"]}, "state": {"type": "string", "enum": ["new", "in_progress", "escalated", "done"]},
            "sla_hours": {"type": "number", "exclusiveMinimum": 0, "maximum": 2160}, "comment": {"type": "string", "maxLength": 1000}}},
        "InvestigationIn": {"type": "object", "required": ["title", "alert_ids"], "properties": {"title": {"type": "string", "minLength": 1, "maxLength": 200},
            "alert_ids": {"type": "array", "items": s("string"), "minItems": 1, "maxItems": 50}, "summary": {"type": "string", "maxLength": 1000},
            "priority": {"type": "string", "enum": ["low", "medium", "high", "critical"], "default": "medium"}, "assignee": s("string"),
            "sla_hours": {"type": "number", "exclusiveMinimum": 0, "maximum": 2160}}},
        "InvestigationActionIn": {"type": "object", "required": ["action"], "properties": {
            "action": {"type": "string", "enum": ["assign", "priority", "status", "sla", "comment", "add_alert", "request_close", "approve_close", "reject_close", "reopen"]},
            "assignee": s("string"), "priority": {"type": "string", "enum": ["low", "medium", "high", "critical"]},
            "status": {"type": "string", "enum": ["open", "in_progress", "pending"]}, "sla_hours": {"type": "number", "exclusiveMinimum": 0, "maximum": 2160},
            "text": {"type": "string", "maxLength": 1000, "description": "the comment, or the reason of request_close"}, "alert_id": s("string")}},
        "SanctionsIn": {"type": "object", "required": ["name"], "properties": {"name": {"type": "string", "minLength": 1, "maxLength": 300},
            "type": {"type": "string", "enum": ["individual", "entity"]}}},
        "PayeeCheckIn": {"type": "object", "required": ["entered_name", "account_name"], "properties": {
            "entered_name": {"type": "string", "minLength": 1, "maxLength": 300}, "account_name": {"type": "string", "minLength": 1, "maxLength": 300},
            "account_type": {"type": "string", "enum": ["individual", "business"]}}},
        "VerifyIn": {"type": "object", "required": ["signed_fields", "signature"], "properties": {"signed_fields": s("object"), "signature": s("string")}},
        "ApplicationIn": {"type": "object", "required": ["customer_age"], "additionalProperties": true,
            "description": "Account-opening application in the Feedzai BAF schema; omitted fields are treated as missing.",
            "properties": {"customer_age": s("integer")}}
    })
}

/// (method, path, summary, request schema, extra parameters)
const ROUTES: &[(&str, &str, &str, Option<&str>, &[(&str, &str, &str)])] = &[
    ("get", "/api/health", "Engine status", None, &[]),
    ("post", "/api/v2/payment", "Score one payment and update account state. Send payments in time order. A recent risky message to the payer, or a payee named in one, can raise the decision (never lower it).", Some("PaymentIn"), &[]),
    ("post", "/api/v2/assess", "Payment cascade + context layer (ruleset in the response). The final decision is the stricter of the two; context alone never goes beyond an analyst hold. Message text is redacted before it is returned or stored.", Some("AssessIn"), &[]),
    ("post", "/api/v2/events/payment", "Signed ingest webhook for a payment event from an outside system: the /api/v2/assess body plus event_id (and an optional PRAMAAN auth object). Headers X-RuseTect-Timestamp (epoch seconds, within 5 minutes) and X-RuseTect-Signature: v1=hex(HMAC-SHA256(RUSETECT_WEBHOOK_SECRET, timestamp + \".\" + raw body)). Scores, decides, opens an alert on verify/hold/block, writes a ledger entry and commits the result in the same request. A repeated event_id returns the stored result (header Idempotent-Replayed: true). See docs/live.md.", Some("AssessIn"), &[]),
    ("get", "/api/v2/live", "Latest live ingested decisions, newest first, and whether the store is durable (Redis) or in memory.", None, &[]),
    ("get", "/api/v2/drift", "Drift of the last scored payments against the validation profile: PSI of the score and five inputs (warn 0.1, alert 0.25), band alert-rate ratios (warn 2x, alert 3x), status ok / warn / alert, PSI ranking, traffic (live_and_api or demo_stream), last status change and reference sha256. Per instance.", None, &[]),
    ("get", "/api/v2/alerts", "Alerts awaiting or after human review, newest first, each with demo (true for /assess alerts on a demo-mode instance). With any of q, band, state, priority, assigned_to, demo, sort, order, limit or offset the answer is a page: {alerts, total, all, offset, limit, sort, order, assignees, demo}. Without them, a plain array as before.", None,
     &[("status", "query", "string"), ("q", "query", "string"), ("band", "query", "string"), ("state", "query", "string"), ("priority", "query", "string"), ("assigned_to", "query", "string"),
       ("demo", "query", "boolean"), ("sort", "query", "string"), ("order", "query", "string"), ("limit", "query", "integer"), ("offset", "query", "integer")]),
    ("get", "/api/v2/whoami", "The analyst id and role behind the presented X-Analyst-Key (401 without a known key). The key is never returned.", None, &[("x-analyst-key", "header", "string")]),
    ("post", "/api/v2/alerts/{alert_id}/review", "Analyst action. Needs X-Analyst-Key and a fresh X-Nonce. Two different analysts must approve a block; the block then publishes the payee to the consortium mule registry (hashed).", Some("ReviewIn"),
     &[("alert_id", "path", "string"), ("x-analyst-key", "header", "string"), ("x-nonce", "header", "string")]),
    ("get", "/api/v2/alerts/{alert_id}/evidence", "Signed evidence pack for one alert", None, &[("alert_id", "path", "string")]),
    ("get", "/api/v2/alerts/{alert_id}/str", "STR draft built from the evidence pack (template, not a language model). For analyst review; never filed automatically.", None, &[("alert_id", "path", "string")]),
    ("post", "/api/v2/alerts/{alert_id}/triage", "Case work on an alert: assignee, priority (low, medium, high, critical), state (new, in_progress, escalated, done), sla_hours and a comment. Needs an analyst or supervisor X-Analyst-Key and a fresh X-Nonce. Never changes the alert's decision status.", Some("TriageIn"),
     &[("alert_id", "path", "string"), ("x-analyst-key", "header", "string"), ("x-nonce", "header", "string")]),
    ("get", "/api/v2/investigations", "Investigations opened from alerts, newest first, each with sla_breached.", None, &[("status", "query", "string"), ("assignee", "query", "string"), ("priority", "query", "string"), ("limit", "query", "integer")]),
    ("post", "/api/v2/investigations", "Open an investigation from 1 to 50 alerts. Needs an analyst or supervisor X-Analyst-Key and a fresh X-Nonce.", Some("InvestigationIn"), &[("x-analyst-key", "header", "string"), ("x-nonce", "header", "string")]),
    ("get", "/api/v2/investigations/{case_id}", "One investigation with its alerts as they are now and its ledger entries.", None, &[("case_id", "path", "string")]),
    ("post", "/api/v2/investigations/{case_id}/actions", "Act on an investigation: assign, priority, status, sla, comment, add_alert, request_close, approve_close, reject_close or reopen. Closing takes two people (the requester cannot approve); reopening needs a supervisor. Every action is appended to the ledger.", Some("InvestigationActionIn"),
     &[("case_id", "path", "string"), ("x-analyst-key", "header", "string"), ("x-nonce", "header", "string")]),
    ("get", "/api/v2/investigations/{case_id}/narrative", "Deterministic narrative draft (template v1, no language model): statements typed observed_fact, model_output, rule_finding, interpretation or analyst_conclusion, each with its sources (alert, transaction, ledger seq, evidence pack hash, comment), plus suggested checks and open questions. requires_human_review: true; it decides nothing.", None, &[("case_id", "path", "string")]),
    ("get", "/api/v2/dashboard", "Dashboard numbers, each with its source: payments scored, stored and flagged by band, alerts and investigations by status, flagged payees, top typologies and reason codes, an activity series, health and drift. data.kind says live, demo_stream, mixed or none.", None, &[("bucket", "query", "string")]),
    ("get", "/api/v2/transactions", "Search stored payment decisions (live events, /assess alerts, the demo feed), newest payment first.", None,
     &[("account", "query", "string"), ("bank", "query", "string"), ("band", "query", "string"), ("source", "query", "string"), ("typology", "query", "string"),
       ("min_amount", "query", "number"), ("max_amount", "query", "number"), ("from", "query", "string"), ("to", "query", "string"), ("limit", "query", "integer"), ("offset", "query", "integer")]),
    ("get", "/api/v2/transactions/{txn_id}", "One stored decision: risk and thresholds, SHAP and motif reason codes, model, alert, related accounts, beneficiary history and ledger timeline.", None, &[("txn_id", "path", "string")]),
    ("get", "/api/v2/graph", "Entity graph around one account from stored payments: account and bank nodes, one directed edge per payment with amount, timestamp and evidence ids. Indicators are leads for review, not findings. At most 300 accounts and 1,000 payments.", None,
     &[("account", "query", "string"), ("bank", "query", "string"), ("depth", "query", "integer"), ("from", "query", "string"), ("to", "query", "string"), ("limit", "query", "integer")]),
    ("get", "/api/v2/campaigns", "Alerts linked into scam campaigns by shared payee, payee identifiers in messages, or near identical scripts.", None, &[]),
    ("post", "/api/v2/alerts/{alert_id}/customer", "The customer's answer on the pause screen.", Some("CustomerIn"), &[("alert_id", "path", "string")]),
    ("post", "/api/v2/alerts/{alert_id}/reverify", "Customer recovery: a re-verified customer goes back to an analyst for release.", Some("ReverifyIn"), &[("alert_id", "path", "string")]),
    ("post", "/api/v2/authenticate", "Account-takeover check for a login or payment (PRAMAAN): allow, challenge (sms_otp, passkey, bank_callback or cool_off) or block, with reasons in English and Hindi and tree-SHAP signals.", None, &[]),
    ("post", "/api/v2/challenge/verify", "Report whether the requested challenge passed: allow (a strong challenge also trusts the device) or block.", None, &[]),
    ("post", "/api/v2/identity/check", "Run before the OTP is sent: unenrolled device plus a recent SIM swap means no OTP and re-verification.", Some("SessionIn"), &[]),
    ("post", "/api/v2/duress/register", "Register a normal and a duress PIN", Some("DuressRegisterIn"), &[]),
    ("post", "/api/v2/payments/confirm", "Confirm a payment with a PIN. The duress PIN shows success to the customer but holds the money and alerts trusted contacts and the fraud desk.", Some("ConfirmIn"), &[]),
    ("post", "/api/v2/behaviour/enrol", "Enrol one typing session", Some("DigraphIn"), &[]),
    ("post", "/api/v2/behaviour/check", "Typing rhythm check (verifier trained and tested on real keystrokes, see results/aalto_eval.json); use the result as SessionIn.typing_anomaly.", Some("DigraphIn"), &[]),
    ("get", "/api/v2/consortium", "Signed, hash chained cross bank ledger (mule accounts, fraud devices, SIM swap takeovers); identifiers are HMAC tokens.", None, &[]),
    ("post", "/api/v2/consortium/check", "Is an identifier on a consortium registry?", Some("ConsortiumCheckIn"), &[]),
    ("post", "/api/v2/consortium/publish", "Demo: a member bank publishes a flag (needs an analyst key).", Some("ConsortiumPublishIn"), &[("x-analyst-key", "header", "string")]),
    ("get", "/api/v2/keys/public", "Ed25519 public key for verdict and evidence signatures", None, &[]),
    ("post", "/api/v2/verify", "Verify a signature over signed fields", Some("VerifyIn"), &[]),
    ("get", "/api/v2/demo/analysts", "Demo keys (only when RUSETECT_ANALYST_KEYS is not set).", None, &[]),
    ("get", "/api/v2/fixtures/mule", "The fictional mule replay fixture (fixtures/mule_replay.json) used by the investigator demo.", None, &[]),
    ("get", "/api/v2/audit", "Hash-chained audit log", None, &[]),
    ("post", "/api/v2/message", "L-1 lure detector. Scores the text, explains which scam workflow and tactics it matches, and applies the alert-fatigue policy (push only high-risk, deduplicated, capped). Entities in risky messages join a payee watchlist.", Some("MessageIn"), &[]),
    ("get", "/api/v2/messages", "Message history: the last 200 scored messages of this instance, newest first, redacted (identifiers replaced by [PHONE], [UPI] and the like; entities kept as counts), with the message model's description and thresholds. In memory only.", None,
     &[("q", "query", "string"), ("band", "query", "string"), ("limit", "query", "integer")]),
    ("post", "/api/v2/sanctions/screen", "Watchlist name screening. Returns the five closest list entries (name, list_id, program, score in [0, 1]) and hit = best score >= threshold (fitted on validation; matcher v2 and its threshold from research/out/sanctions_eval_v2.json when adopted, else research/out/sanctions_eval.json). The bundled list is fictitious test data; load the real list in deployment.", Some("SanctionsIn"), &[]),
    ("post", "/api/v2/payee/check", "Payee name check (confirmation of payee), against authorised push payment scams. Compares the name the payer entered with the account holder name (from the bank's records; passed in here) using the sanctions matcher v2, and returns verdict (match, close_match with suggested_name, or no_match), score in [0, 1] and a short reason. A name on the wrong account type (account_type given) is at most close_match. Thresholds fitted on validation in research/out/payee_check_eval.json.", Some("PayeeCheckIn"), &[]),
    ("get", "/api/v2/alerts/summary", "How many messages were scored, pushed, or suppressed (and why); how many payments a lure escalated.", None, &[]),
    ("get", "/api/v2/message-model", "Message model evaluation", None, &[]),
    ("post", "/api/v2/payments/batch", "Score up to 20,000 payments (sorted by timestamp)", None, &[]),
    ("get", "/api/v2/cases", "Current ring cases", None, &[("limit", "query", "integer")]),
    ("post", "/api/v2/onboard", "L0 onboarding gate for a new account application (Feedzai BAF schema)", Some("ApplicationIn"), &[]),
    ("get", "/api/v2/model-card", "All benchmark numbers", None, &[]),
    ("get", "/api/v2/stats", "Engine counters", None, &[]),
    ("post", "/api/v2/demo/reset", "Clear state, then ingest earlier payments as account history (features only, no scoring).", None, &[("warm", "query", "boolean")]),
    ("post", "/api/v2/demo/next", "Score the next n demo payments. Labels are only used to report accuracy, never by the model.", None, &[("n", "query", "integer")]),
];

pub fn spec() -> Value {
    let mut paths: Map<String, Value> = Map::new();
    for (method, path, summary, body, params) in ROUTES {
        let mut op = json!({"summary": summary.split(". ").next().unwrap_or(summary), "description": summary,
                            "responses": {"200": {"description": "Successful Response", "content": {"application/json": {"schema": {}}}},
                                          "422": {"description": "Validation Error"}}});
        if let Some(b) = body {
            op["requestBody"] = json!({"required": true, "content": {"application/json": {"schema": {"$ref": format!("#/components/schemas/{b}")}}}});
        } else if *path == "/api/v2/payments/batch" {
            op["requestBody"] = json!({"required": true, "content": {"application/json": {"schema": {"type": "array", "items": {"$ref": "#/components/schemas/PaymentIn"}}}}});
        }
        if !params.is_empty() {
            op["parameters"] = Value::Array(params.iter().map(|(n, at, t)| json!({"name": n, "in": at, "required": *at == "path", "schema": {"type": t}})).collect());
        }
        let entry = paths.entry(path.to_string()).or_insert_with(|| json!({}));
        entry[*method] = op;
    }
    json!({"openapi": "3.1.0", "info": {"title": format!("{NAME} API"), "version": VERSION, "description": crate::description()},
           "paths": paths, "components": {"schemas": schemas()}})
}

/// Swagger UI from jsDelivr, pinned to one release with subresource integrity, so a changed file on the CDN is refused
/// by the browser instead of running on this origin (where the app keeps the analyst key). To upgrade, change the
/// version and both hashes together (`openssl dgst -sha384 -binary FILE | base64` over the npm package's files).
pub const DOCS_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<link type="text/css" rel="stylesheet" href="https://cdn.jsdelivr.net/npm/swagger-ui-dist@5.32.0/swagger-ui.css"
 integrity="sha384-3nuX7df3EaAoiqLBeyS1Ola0Gpg9ryJKVtarubwfnA1cOH8AWHUdbPSIvEqPZ9VH" crossorigin="anonymous">
<link rel="shortcut icon" href="/favicon.svg">
<title>RuseTect API - Swagger UI</title>
</head>
<body>
<div id="swagger-ui"></div>
<script src="https://cdn.jsdelivr.net/npm/swagger-ui-dist@5.32.0/swagger-ui-bundle.js"
 integrity="sha384-7xcoc6ZKDFF7Ek627QTC3Bg/K+5Y36NJ8MWAE43D2m6+3Sh9XO3tdsfHhrS8gNIQ" crossorigin="anonymous"></script>
<script src="/assets/js/docs.js"></script>
</body>
</html>
"#;
