//! STR draft from an alert's evidence pack (port of service/str_report.py).
//!
//! Built from a template, not a language model: every sentence is traceable to a field in the evidence pack,
//! nothing leaves the bank's servers, and the same inputs always give the same draft. Sections follow the parts of a
//! Suspicious Transaction Report filed with FIU-IND; an analyst must review, edit and sign before anything is filed.
use serde_json::{json, Value};
use std::collections::BTreeSet;

pub fn plain(r: &str) -> Option<&'static str> {
    Some(match r {
        "message cue: authority" => "the message impersonated an authority (bank, police, government or a company office)",
        "message cue: redirection" => "the message redirected payment to a new or 'safe' account",
        "message cue: pressure" => "the message used time pressure or threats such as 'digital arrest'",
        "message cue: credential" => "the message asked for an OTP, PIN or password",
        "message cue: remote_access" => "the message asked for screen sharing or a remote access app",
        "message cue: refund" => "the message offered a refund or cashback that required a payment action",
        "message cue: investment" => "the message promised guaranteed or very high returns",
        "message cue: identity" => "the message threatened KYC, PAN or Aadhaar problems",
        "message cue: secrecy" => "the message told the customer to keep the payment secret",
        "payment check: new_beneficiary" => "first payment from this customer to this payee",
        "payment check: amount_deviation" => "amount far above the customer's usual payments",
        "payment check: payee_named_in_message" => "the payee identifier appeared in the scam message",
        "account graph: fan_in_4plus" => "the payee received money from four or more different accounts in 24 hours",
        "account graph: rapid_pass_through" => "money was forwarded within minutes of arriving",
        "account graph: coordinated_accounts" => "the payee was paid by several accounts already flagged for review",
        "account graph: sender_mule_signature" => "the payer collected from several accounts and forwarded almost all of it within two minutes",
        "session: remote_access_app" => "a remote access app was running during the payment",
        "session: screen_share" => "the screen was being shared during the payment",
        "session: on_call_while_paying" => "the customer was on a call while paying",
        "session: long_or_video_call" => "the call was a long or video call, typical of 'digital arrest' coercion",
        "session: payee_pasted" => "the payee details were pasted in rather than typed or chosen",
        "session: long_hesitation" => "the customer hesitated for a long time before confirming",
        "session: app_hidden_mid_payment" => "the banking app was sent to the background during the payment",
        "session: typing_rhythm_unlike_customer" => "typing rhythm did not match the customer's enrolled pattern",
        "session: sim_swap_takeover" => "an unenrolled device was used shortly after a SIM swap",
        "session: recent_sim_swap" => "the SIM was swapped recently",
        "session: unenrolled_device" => "the payment came from a device not enrolled for this customer",
        "session: attack_chain_full" => "password reset, new device, new payee and transfer happened in sequence within 24 hours",
        "session: attack_chain_short" => "new device, new payee and transfer happened within an hour",
        _ => return None,
    })
}

/// Python `str(v)` / f-string formatting of a JSON value
pub fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "None".into(),
        Value::Bool(b) => if *b { "True".into() } else { "False".into() },
        Value::Number(n) if n.is_f64() => crate::pyfmt::py_repr(n.as_f64().unwrap()),
        other => other.to_string(),
    }
}

pub fn draft(pack: &Value, campaign: Option<&Value>, reporting_entity: &str) -> Value {
    let a = &pack["alert"];
    let p = &a["payment"];
    let x = &a["assessed"];
    let c = x.get("context").cloned().unwrap_or(json!({}));
    let pay = x.get("payment").cloned().unwrap_or(json!({}));
    let mut grounds: Vec<String> = c["reasons"].as_array().into_iter().flatten().filter_map(|r| r.as_str())
        .filter(|r| !r.starts_with("negated")).map(|r| plain(r).map(String::from).unwrap_or_else(|| r.to_string())).collect();
    for r in pay["reasons"].as_array().into_iter().flatten() {
        grounds.push(format!("payment model: {} = {}", py_str(&r["feature"]), py_str(&r["display"])));
    }
    let actions: Vec<String> = a["actions"].as_array().into_iter().flatten().map(|e| {
        let note = e["note"].as_str().unwrap_or("");
        format!("{}: {}", py_str(&e["analyst"]), py_str(&e["action"])) + &if note.is_empty() { String::new() } else { format!(" ({note})") }
    }).collect();
    let me = a["alert_id"].as_str().unwrap_or("");
    let mut linked: BTreeSet<String> = pack["linked_alerts"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(String::from)).collect();
    if let Some(cmp) = campaign {
        let camp: BTreeSet<String> = cmp["alerts"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(String::from)).filter(|x| x != me).collect();
        linked.extend(camp);
    }
    let victims = campaign.and_then(|c| c["victims"].as_array()).map(|v| v.len()).unwrap_or(0);
    let sections = json!({
        "A_reporting_entity": reporting_entity,
        "B_transactions": [{"date_time": p["timestamp"], "from": format!("{}/{}", py_str(&p["from_bank"]), py_str(&p["from_account"])),
                            "to": format!("{}/{}", py_str(&p["to_bank"]), py_str(&p["to_account"])), "amount": p["amount_paid"],
                            "currency": p["payment_currency"], "mode": p["payment_format"]}],
        "C_linked_accounts_and_alerts": {"alerts": linked, "campaign": campaign.map(|c| c["campaign_id"].clone()).unwrap_or(Value::Null),
                                         "victims_in_campaign": if victims > 0 { json!(victims) } else { Value::Null }},
        "D_grounds_of_suspicion": if grounds.is_empty() { vec!["no rule fired; raised by the payment model score alone".to_string()] } else { grounds.clone() },
        "E_action_taken": if actions.is_empty() { vec!["no analyst action yet".to_string()] } else { actions },
        "F_attachments": {"evidence_pack_sha256": pack.get("pack_hash").cloned().unwrap_or(Value::Null), "ruleset": c.get("ruleset").cloned().unwrap_or(Value::Null),
                          "message_text": if c.get("redacted_text").map(|t| !t.is_null() && t != "").unwrap_or(false) { "redacted copy in the evidence pack" } else { "none or not consented" }},
    });
    let mut summary = format!("On {} account {} paid {} {} to {}. RuseTect raised '{}' (context band {}). Grounds: {}.",
        py_str(&p["timestamp"]), py_str(&p["from_account"]), py_str(&p["amount_paid"]), py_str(&p["payment_currency"]), py_str(&p["to_account"]),
        py_str(&a["shown_decision"]), py_str(c.get("band").unwrap_or(&Value::Null)), grounds.iter().take(6).cloned().collect::<Vec<_>>().join("; "));
    if let Some(cmp) = campaign {
        summary += &format!(" The payment is part of campaign {} with {} linked alerts.", py_str(&cmp["campaign_id"]), py_str(&cmp["size"]));
    }
    json!({"status": "DRAFT, for analyst review and signature", "summary": summary, "sections": sections,
           "disclaimer": "Generated from rules and the evidence pack; risk points are not fraud probabilities. Not filed."})
}

pub const DEFAULT_ENTITY: &str = "[Bank name, FIU-IND registration]";
