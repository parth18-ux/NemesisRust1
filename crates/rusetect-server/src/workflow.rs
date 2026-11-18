//! Alert triage and investigations: the case work around the review desk (docs/API.md).
//!
//!   POST /api/v2/alerts/{id}/triage           assignee, priority, state, SLA and comments on one alert
//!   GET  /api/v2/investigations               investigations, newest first (status, assignee, priority filters)
//!   POST /api/v2/investigations               open an investigation from one or more alerts
//!   GET  /api/v2/investigations/{id}          one investigation with its alerts and its ledger entries
//!   POST /api/v2/investigations/{id}/actions  assign, priority, status, sla, comment, add_alert, request_close,
//!                                             approve_close, reject_close, reopen
//!   GET  /api/v2/investigations/{id}/narrative  deterministic, sourced narrative draft (rusetect::narrative)
//!
//! * Writes need an analyst or supervisor key (X-Analyst-Key) and a fresh X-Nonce, as a review does. An analyst can take
//!   work themselves; giving it to someone else, and reopening a closed investigation, need a supervisor.
//! * Maker-checker: an investigation is closed by two people. One requests the closure with a reason, a different
//!   analyst or supervisor approves or rejects it (the four-eyes rule a block already has).
//! * Nothing here decides a payment: an alert's `status` changes only through /review, and no route blocks on its own.
//! * Every action is appended to the hash-chained ledger (GET /api/v2/audit); comment and reason texts stay in the
//!   alert or investigation and enter the ledger as hashes.
//! * With Redis the investigations live in `live:cases` and live alerts in `live:alerts`, shared by every instance;
//!   the ledger entries of case work are this instance's, as those of analyst reviews are.
use crate::{blocking, header, int_query, live, ok, parse_obj, require_reader, review_err, ApiError, ApiResult, AppRef, AppState, Shared};
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use rusetect::pyfmt::utc_now_z;
use rusetect::review::hash_obj;
use rusetect::security::{allowed, analyst_for, Analyst, KEYS};
use serde_json::{json, Map, Value};
use std::collections::HashMap;

pub const PRIORITIES: [&str; 4] = ["low", "medium", "high", "critical"];
pub const ALERT_STATES: [&str; 4] = ["new", "in_progress", "escalated", "done"];
/// `closed` is reached only through request_close and approve_close.
pub const CASE_STATUSES: [&str; 5] = ["open", "in_progress", "pending", "pending_closure", "closed"];
pub const CASE_ACTIONS: [&str; 10] = ["assign", "priority", "status", "sla", "comment", "add_alert", "request_close", "approve_close", "reject_close", "reopen"];
/// Hours until `sla_due` when none is given: a desk default per priority, not a regulatory deadline.
pub const SLA_HOURS: [f64; 4] = [168.0, 72.0, 24.0, 4.0];
const SLA_MAX_HOURS: f64 = 2160.0;
/// Investigations kept in this process; beyond it the oldest closed one is dropped from memory (its ledger entries stay).
pub const MAX_CASES: usize = 5000;
const CASE_ALERTS_MAX: usize = 50;
const COMMENTS_MAX: usize = 200;
const HISTORY_MAX: usize = 500;
const TEXT_MAX: usize = 1000;

fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn due_in(hours: f64) -> String {
    (chrono::Utc::now() + chrono::Duration::seconds((hours * 3600.0) as i64)).format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn default_sla(priority: &str) -> String {
    due_in(SLA_HOURS[PRIORITIES.iter().position(|p| *p == priority).unwrap_or(1)])
}

/// Open and past its due time (`now` and `sla_due` are both utc_now_z strings, so they compare as text).
pub(crate) fn breached(c: &Value, now: &str) -> bool {
    c["status"] != "closed" && c["sla_due"].as_str().is_some_and(|d| d < now)
}

/// The analyst or supervisor behind X-Analyst-Key.
fn reviewer(h: &HeaderMap) -> Result<Analyst, ApiError> {
    let who = analyst_for(header(h, "x-analyst-key").as_deref()).ok_or_else(|| ApiError::new(401, "missing or unknown X-Analyst-Key"))?;
    if !allowed(&who.role, "hold") {
        return Err(ApiError::new(403, format!("role {} cannot do case work", who.role)));
    }
    Ok(who)
}

fn use_nonce(s: &mut AppState, nonce: Option<&str>) -> Result<(), ApiError> {
    if s.nonces.use_nonce(nonce) { Ok(()) } else { Err(ApiError::new(409, "X-Nonce missing or already used")) }
}

/// Work goes to a known analyst or supervisor; an analyst can only take it themselves.
fn may_assign(who: &Analyst, to: &str) -> Result<(), ApiError> {
    if !KEYS.keys.values().any(|a| a.analyst == to && allowed(&a.role, "hold")) {
        return Err(ApiError::new(400, "assignee must be a known analyst or supervisor"));
    }
    if who.role != "supervisor" && who.analyst != to {
        return Err(ApiError::new(403, "an analyst can take work themselves; assigning it to someone else needs a supervisor key"));
    }
    Ok(())
}

/// The optional fields every case-work body shares, type-checked: (assignee, priority, sla_hours, text).
fn common(o: &crate::validate::Obj, e: &mut crate::validate::Errors, text: &str) -> (Option<String>, Option<String>, Option<f64>, String) {
    let assignee = o.opt_str(e, "assignee");
    if let Some(a) = &assignee {
        o.check_len(e, "assignee", a, Some(1), Some(128));
    }
    let priority = o.opt_str(e, "priority");
    if let Some(p) = &priority {
        o.one_of(e, "priority", p, &PRIORITIES);
    }
    let sla = o.opt_float(e, "sla_hours");
    if sla.is_some_and(|h| !(h > 0.0 && h <= SLA_MAX_HOURS)) {
        e.push("value_error", &["body", "sla_hours"], "Value error, sla_hours must be above 0 and at most 2160", o.get("sla_hours").unwrap_or(&Value::Null));
    }
    let t = o.str_or(e, text, "");
    o.check_len(e, text, &t, None, Some(TEXT_MAX));
    (assignee, priority, sla, t.trim().to_string())
}

/// POST /api/v2/alerts/{alert_id}/triage: set any of assignee, priority (low / medium / high / critical), state (new /
/// in_progress / escalated / done), sla_hours, and add a comment. The first priority also sets `sla_due` from the desk
/// default unless sla_hours is given. The alert's decision `status` is not touched.
pub async fn v2_triage(State(app): State<AppRef>, Path(alert_id): Path<String>, h: HeaderMap, b: Bytes) -> ApiResult {
    let ((assignee, priority, sla, comment), state) = parse_obj(&b, |o, e| {
        let state = o.opt_str(e, "state");
        if let Some(s) = &state {
            o.one_of(e, "state", s, &ALERT_STATES);
        }
        (common(o, e, "comment"), state)
    })?;
    if assignee.is_none() && priority.is_none() && state.is_none() && sla.is_none() && comment.is_empty() {
        return Err(ApiError::new(422, "nothing to change: send assignee, priority, state, sla_hours or comment"));
    }
    blocking(move || {
        let who = reviewer(&h)?;
        if let Some(a) = &assignee {
            may_assign(&who, a)?;
        }
        let nonce = header(&h, "x-nonce");
        live::act_on_alert(&app, &alert_id, |s| use_nonce(s, nonce.as_deref()), |s| {
            let cur = s.desk.alerts.get(&alert_id).ok_or_else(|| ApiError::new(404, "unknown alert"))?;
            let mut ch = Map::new();
            match (&sla, &priority) {
                (Some(h), _) => ch.insert("sla_due".into(), json!(due_in(*h))),
                (None, Some(p)) if !cur["workflow"]["sla_due"].is_string() => ch.insert("sla_due".into(), json!(default_sla(p))),
                _ => None,
            };
            for (k, v) in [("assignee", assignee), ("priority", priority), ("state", state)] {
                if let Some(v) = v {
                    ch.insert(k.into(), json!(v));
                }
            }
            ok(s.desk.triage(&alert_id, &who.analyst, ch, &comment).map_err(review_err)?)
        })
    }).await
}

/// Append a case event to the ledger and to the investigation's own history.
fn record(s: &mut AppState, c: &mut Value, analyst: &str, action: &str, detail: Value) {
    let e = s.desk.append(json!({"type": "case_event", "case_id": c["case_id"], "analyst": analyst, "action": action, "status": c["status"], "detail": detail}));
    c["updated_at"] = e["at"].clone();
    let h = c["history"].as_array_mut().unwrap();
    h.push(json!({"at": e["at"], "analyst": analyst, "action": action, "detail": detail, "ledger_seq": e["seq"], "ledger_hash": e["hash"]}));
    if h.len() > HISTORY_MAX {
        h.remove(0);
    }
}

/// Put an alert into an investigation, or (`add` false) take it out again. An alert is in one investigation at a time.
fn tag(app: &Shared, alert_id: &str, case_id: &str, add: bool) -> Result<(), ApiError> {
    live::act_on_alert(app, alert_id, |_| Ok(()), |s| {
        let a = s.desk.alerts.get(alert_id).ok_or_else(|| ApiError::new(404, format!("unknown alert {alert_id}")))?;
        match a["case_id"].as_str() {
            Some(cur) if cur != case_id => return Err(ApiError::new(409, format!("alert {alert_id} is already in investigation {cur}"))),
            Some(_) if add => return Err(ApiError::new(409, format!("alert {alert_id} is already in this investigation"))),
            _ => {}
        }
        s.desk.set_field(alert_id, "case_id", if add { json!(case_id) } else { Value::Null });
        Ok(())
    })
}

fn view(mut c: Value) -> Value {
    c["sla_breached"] = json!(breached(&c, &utc_now_z()));
    c
}

/// POST /api/v2/investigations: open an investigation from 1 to 50 alerts. Body: title, alert_ids, and optionally
/// priority (default medium), assignee and sla_hours (default: the desk default for the priority).
pub async fn v2_investigation_open(State(app): State<AppRef>, h: HeaderMap, b: Bytes) -> ApiResult {
    let (title, alert_ids, (assignee, priority, sla, summary)) = parse_obj(&b, |o, e| {
        let title = o.req_str(e, "title");
        if o.get("title").is_some_and(|v| v.is_string()) {
            o.check_len(e, "title", title.trim(), Some(1), Some(200));
        }
        (title.trim().to_string(), o.str_list(e, "alert_ids"), common(o, e, "summary"))
    })?;
    let unique: std::collections::HashSet<&String> = alert_ids.iter().collect();
    if alert_ids.is_empty() || alert_ids.len() > CASE_ALERTS_MAX || unique.len() != alert_ids.len() || alert_ids.iter().any(|a| a.is_empty() || a.len() > 64) {
        return Err(ApiError::new(422, "alert_ids: 1 to 50 different alert ids"));
    }
    blocking(move || {
        let who = reviewer(&h)?;
        if let Some(a) = &assignee {
            may_assign(&who, a)?;
        }
        {
            let mut s = lock(&app.state);
            use_nonce(&mut s, header(&h, "x-nonce").as_deref())?;
            if s.cases.len() >= MAX_CASES {
                let old = s.cases.iter().find(|(_, c)| c["status"] == "closed").map(|(k, _)| k.clone())
                    .ok_or_else(|| ApiError::new(503, "too many open investigations; close some first"))?;
                s.cases.shift_remove(&old);
            }
        }
        let case_id = format!("INV-{:05}", live::next_case_seq(&app)?);
        for (i, id) in alert_ids.iter().enumerate() {
            if let Err(e) = tag(&app, id, &case_id, true) {
                for done in &alert_ids[..i] {
                    let _ = tag(&app, done, &case_id, false);
                }
                return Err(e);
            }
        }
        live::act_on_case(&app, &case_id, true, |s| {
            let priority = priority.unwrap_or_else(|| "medium".into());
            let now = utc_now_z();
            let mut c = json!({"case_id": case_id, "title": title, "summary": summary, "status": "open", "priority": priority, "assignee": assignee,
                               "alert_ids": alert_ids, "created_by": who.analyst, "created_at": now, "updated_at": now,
                               "sla_due": sla.map(due_in).unwrap_or_else(|| default_sla(&priority)), "comments": [], "history": [], "closure": null});
            let detail = json!({"alert_ids": c["alert_ids"], "priority": c["priority"], "assignee": c["assignee"], "sla_due": c["sla_due"]});
            record(s, &mut c, &who.analyst, "opened", detail);
            s.cases.insert(case_id.clone(), c.clone());
            ok(view(c))
        })
    }).await
}

/// POST /api/v2/investigations/{case_id}/actions. Body: action and its field: assign (assignee), priority (priority),
/// status (status: open / in_progress / pending), sla (sla_hours), comment (text), add_alert (alert_id), request_close
/// (text: the reason), approve_close, reject_close (a different analyst or supervisor), reopen (supervisor).
pub async fn v2_investigation_action(State(app): State<AppRef>, Path(case_id): Path<String>, h: HeaderMap, b: Bytes) -> ApiResult {
    let (action, status, alert_id, (assignee, priority, sla, text)) = parse_obj(&b, |o, e| {
        let action = o.req_str(e, "action");
        o.one_of(e, "action", &action, &CASE_ACTIONS);
        let status = o.opt_str(e, "status");
        if let Some(s) = &status {
            o.one_of(e, "status", s, &CASE_STATUSES[..3]);
        }
        let alert_id = o.opt_str(e, "alert_id");
        if let Some(a) = &alert_id {
            o.check_len(e, "alert_id", a, Some(1), Some(64));
        }
        (action, status, alert_id, common(o, e, "text"))
    })?;
    let need = |v: bool, field: &str| if v { Ok(()) } else { Err(ApiError::new(422, format!("{action} needs {field}"))) };
    match action.as_str() {
        "assign" => need(assignee.is_some(), "assignee")?,
        "priority" => need(priority.is_some(), "priority")?,
        "status" => need(status.is_some(), "status")?,
        "sla" => need(sla.is_some(), "sla_hours")?,
        "comment" | "request_close" => need(!text.is_empty(), "text")?,
        "add_alert" => need(alert_id.is_some(), "alert_id")?,
        _ => {}
    }
    blocking(move || {
        let who = reviewer(&h)?;
        if let Some(a) = assignee.as_ref().filter(|_| action == "assign") {
            may_assign(&who, a)?;
        }
        if action == "reopen" && who.role != "supervisor" {
            return Err(ApiError::new(403, "reopening a closed investigation needs a supervisor key"));
        }
        use_nonce(&mut lock(&app.state), header(&h, "x-nonce").as_deref())?;
        let closed = || ApiError::new(409, "the investigation is closed (a supervisor can reopen it; comments stay open)");
        if action == "add_alert" {
            // the alert is tagged before the investigation lists it (act_on_case must not call act_on_alert)
            live::act_on_case(&app, &case_id, false, |s| {
                let c = s.cases.get(&case_id).ok_or_else(|| ApiError::new(404, "unknown investigation"))?;
                if c["status"] == "closed" {
                    return Err(closed());
                }
                if c["alert_ids"].as_array().is_some_and(|a| a.len() >= CASE_ALERTS_MAX) {
                    return Err(ApiError::new(409, "an investigation holds at most 50 alerts"));
                }
                Ok(())
            })?;
            tag(&app, alert_id.as_deref().unwrap_or(""), &case_id, true)?;
        }
        live::act_on_case(&app, &case_id, true, |s| {
            let mut c = s.cases.get(&case_id).cloned().ok_or_else(|| ApiError::new(404, "unknown investigation"))?;
            let st = c["status"].as_str().unwrap_or("").to_string();
            if st == "closed" && !matches!(action.as_str(), "comment" | "reopen") {
                return Err(closed());
            }
            let pending = st == "pending_closure";
            let detail = match action.as_str() {
                "assign" => {
                    c["assignee"] = json!(assignee);
                    json!({"assignee": assignee})
                }
                "priority" => {
                    c["priority"] = json!(priority);
                    json!({"priority": priority})
                }
                "status" => {
                    if pending {
                        return Err(ApiError::new(409, "a closure request is pending: approve or reject it first"));
                    }
                    c["status"] = json!(status);
                    json!({"from": st, "to": status})
                }
                "sla" => {
                    c["sla_due"] = json!(due_in(sla.unwrap_or(0.0)));
                    json!({"sla_due": c["sla_due"]})
                }
                "comment" => {
                    let cm = c["comments"].as_array_mut().unwrap();
                    if cm.len() >= COMMENTS_MAX {
                        return Err(ApiError::new(409, "an investigation holds at most 200 comments"));
                    }
                    cm.push(json!({"at": utc_now_z(), "analyst": who.analyst, "text": text}));
                    json!({"comment_hash": hash_obj(&json!(text))})
                }
                "add_alert" => {
                    c["alert_ids"].as_array_mut().unwrap().push(json!(alert_id));
                    json!({"alert_id": alert_id})
                }
                "request_close" => {
                    if pending {
                        return Err(ApiError::new(409, "a closure request is already pending"));
                    }
                    c["closure"] = json!({"requested_by": who.analyst, "requested_at": utc_now_z(), "reason": text, "status_before": st});
                    c["status"] = json!("pending_closure");
                    json!({"reason_hash": hash_obj(&json!(text))})
                }
                "approve_close" | "reject_close" => {
                    if !pending {
                        return Err(ApiError::new(409, "no closure request is pending"));
                    }
                    if c["closure"]["requested_by"] == who.analyst.as_str() {
                        return Err(ApiError::new(400, "four-eyes: the analyst who requested the closure cannot decide it"));
                    }
                    let approve = action == "approve_close";
                    c["closure"][if approve { "approved_by" } else { "rejected_by" }] = json!(who.analyst);
                    c["closure"]["decided_at"] = json!(utc_now_z());
                    c["status"] = if approve { json!("closed") } else { c["closure"]["status_before"].clone() };
                    json!({"requested_by": c["closure"]["requested_by"]})
                }
                _ => {
                    if st != "closed" {
                        return Err(ApiError::new(409, "only a closed investigation can be reopened"));
                    }
                    c["status"] = json!("open");
                    json!({})
                }
            };
            record(s, &mut c, &who.analyst, &action, detail);
            s.cases.insert(case_id.clone(), c.clone());
            ok(view(c))
        })
    }).await
}

/// GET /api/v2/investigations: newest first; optional status, assignee, priority and limit (default 100, at most 500).
pub async fn v2_investigations(State(app): State<AppRef>, h: HeaderMap, Query(q): Query<HashMap<String, String>>) -> ApiResult {
    require_reader(&h)?;
    for (k, allowed) in [("status", &CASE_STATUSES[..]), ("priority", &PRIORITIES[..])] {
        if q.get(k).is_some_and(|v| !allowed.contains(&v.as_str())) {
            return Err(ApiError::new(422, format!("{k}: must be one of {}", allowed.join(", "))));
        }
    }
    let limit = int_query(&q.get("limit").cloned(), "limit", 100)?.clamp(1, 500) as usize;
    blocking(move || {
        let hits: Vec<Value> = live::all_cases(&app)?.into_iter()
            .filter(|c| ["status", "assignee", "priority"].iter().all(|k| q.get(*k).is_none_or(|v| c[*k] == v.as_str()))).map(view).collect();
        ok(json!({"total": hits.len(), "investigations": hits.into_iter().take(limit).collect::<Vec<_>>(),
                  "sla_default_hours": PRIORITIES.iter().zip(SLA_HOURS).collect::<HashMap<_, _>>()}))
    }).await
}

/// GET /api/v2/investigations/{case_id}: the investigation, its alerts as they are now, and its ledger entries.
pub async fn v2_investigation(State(app): State<AppRef>, h: HeaderMap, Path(case_id): Path<String>) -> ApiResult {
    require_reader(&h)?;
    blocking(move || {
        let c = live::act_on_case(&app, &case_id, false, |s| s.cases.get(&case_id).cloned().ok_or_else(|| ApiError::new(404, "unknown investigation")))?;
        let alerts: Vec<Value> = c["alert_ids"].as_array().into_iter().flatten().filter_map(|a| a.as_str()).map(|id| {
            live::with_alert(&app, id, |s| Ok(s.desk.alerts.get(id).map(|a| json!({"alert_id": id, "status": a["status"], "shown_decision": a["shown_decision"],
                "payment": a["payment"], "event_id": a["event_id"], "workflow": a["workflow"], "evidence": format!("/api/v2/alerts/{id}/evidence")}))))
                .ok().flatten().unwrap_or_else(|| json!({"alert_id": id, "missing": true}))
        }).collect();
        let s = lock(&app.state);
        let audit: Vec<&Value> = s.desk.log.iter().filter(|e| e["case_id"] == case_id.as_str()).collect();
        ok(json!({"investigation": view(c), "alerts": alerts, "audit_entries": audit,
                  "audit_source": "this instance's hash-chained ledger (GET /api/v2/audit verifies the chain)"}))
    }).await
}

/// GET /api/v2/investigations/{case_id}/narrative: a deterministic draft of the case (rusetect::narrative, "template v1")
/// read from the stored investigation, the evidence pack of each alert, its live record and the desk's campaigns. Every
/// statement names its sources; the draft requires analyst review and never blocks or changes a decision.
pub async fn v2_investigation_narrative(State(app): State<AppRef>, h: HeaderMap, Path(case_id): Path<String>) -> ApiResult {
    require_reader(&h)?;
    blocking(move || {
        let c = live::act_on_case(&app, &case_id, false, |s| s.cases.get(&case_id).cloned().ok_or_else(|| ApiError::new(404, "unknown investigation")))?;
        let alerts: Vec<Value> = c["alert_ids"].as_array().into_iter().flatten().filter_map(|a| a.as_str()).filter_map(|id| {
            let pack = live::with_alert(&app, id, |s| Ok(s.desk.evidence(id))).ok().flatten()?;
            let record = pack["alert"]["event_id"].as_str().and_then(|e| live::stored_event(&app, e).ok().flatten()).unwrap_or(Value::Null);
            Some(json!({"pack": pack, "record": record}))
        }).collect();
        let camps = rusetect::campaigns::build_recent_values(lock(&app.state).desk.alerts.values());
        ok(rusetect::narrative::build(&c, &alerts, &camps))
    }).await
}
