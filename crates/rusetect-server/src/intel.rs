//! Read endpoints behind the website's dashboard, transaction search and entity graph (docs/API.md).
//!
//!   /api/v2/dashboard            counters, alert and investigation status, typologies, activity series, health, drift
//!   /api/v2/transactions         search stored payment decisions (account, band, amount, time, typology, source)
//!   /api/v2/transactions/{id}    one decision: risk, reason codes, model, alert, related accounts, payee history, timeline
//!   /api/v2/graph                accounts and banks around one account, one directed edge per stored payment
//!
//! Nothing here scores or changes anything: every value is read from what the engine, the live store and the review
//! desk already hold, and each response says which. Three kinds of stored payment are read:
//!
//!   live          events ingested by the signed webhook (live.rs): all of them, approved ones included
//!   assess        alerts opened by /api/v2/assess: flagged payments only (an approved one is not stored anywhere)
//!   demo_stream   the Live Console's replay feed: the latest 60 escalated payments of the recorded IBM AMLworld
//!                 (synthetic) stream; demonstration data, labelled as such
//!
//! Bounds: at most SCAN live decisions are read per request (with Redis the feed's 200), pages are at most 200 rows,
//! and a graph at most 300 accounts and 1,000 payments.
use crate::{blocking_read, int_query, live, ok, require_reader, ApiError, ApiResult, AppRef, Shared};
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use rusetect::branding::VERSION;
use rusetect::context_rules::RULESET_VERSION;
use rusetect::engine::decision_rank;
use rusetect::pyfmt::{parse_minutes, py_round, utc_now_z};
use rusetect::review::hash_obj;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap, HashSet};

/// Live decisions read per request, newest first.
const SCAN: usize = 20_000;
const PAGE_MAX: i64 = 200;
const GRAPH_NODES_MAX: i64 = 300;
const GRAPH_EDGES_MAX: usize = 1000;
pub const SOURCES: [&str; 3] = ["live", "assess", "demo_stream"];
/// Said with every graph and every indicator: a link or a flag is a lead for an analyst, not a finding.
pub const NOT_GUILT: &str = "An edge is one stored payment between two accounts. Indicators are model and rule outputs for analyst review; \
                             a link or an indicator is not a finding of wrongdoing.";

fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn band_of(decision: &str) -> &'static str {
    live::ACTIONS[decision_rank(decision)]
}

/// Every stored payment decision in one shape, newest payment first, with the minute of its timestamp.
pub(crate) struct Corpus {
    pub txns: Vec<(i64, Value)>,
    pub alerts: Vec<Value>,
    live_seq: u64,
    live_listed: usize,
    live_capacity: usize,
    redis: bool,
}

/// One stored decision as the read endpoints return it. `pay` holds the payment fields of /api/v2/payment.
fn txn(id: &str, source: &str, pay: &Value, risk: &Value, decision: &str, reasons: &Value, alert_id: &Value) -> (i64, Value) {
    let ts = pay["timestamp"].as_str().unwrap_or("").replace('/', "-");
    let reasons = if reasons.is_array() { reasons.clone() } else { json!([]) };
    let typologies: Vec<Value> = reasons.as_array().unwrap().iter().filter_map(|r| r["motifs"].as_array()).flatten().cloned().collect();
    (parse_minutes(&ts).unwrap_or(i64::MIN),
     json!({"id": id, "source": source, "demo": source == "demo_stream", "timestamp": ts,
            "from_bank": pay["from_bank"], "from_account": pay["from_account"], "to_bank": pay["to_bank"], "to_account": pay["to_account"],
            "amount": pay["amount_paid"], "currency": pay["payment_currency"], "format": pay["payment_format"],
            "risk": risk, "band": band_of(decision), "decision": decision, "typologies": typologies, "reasons": reasons, "alert_id": alert_id}))
}

fn live_txn(r: &Value) -> (i64, Value) {
    let mut t = txn(r["event_id"].as_str().unwrap_or(""), "live", &r["payment"], &r["risk"], r["final_decision"].as_str().unwrap_or("Approve"),
                    &r["reasons"], &r["alert_id"]);
    for k in ["event_id", "seq", "received_at", "payment_decision", "context_band", "pramaan", "ledger", "store"] {
        t.1[k] = r[k].clone();
    }
    t
}

pub(crate) fn corpus(app: &Shared) -> Result<Corpus, ApiError> {
    let alerts = live::all_alerts(app)?;
    let (recs, live_seq, live_capacity) = live::recent(app, SCAN)?;
    let (demo, redis) = {
        let s = lock(&app.state);
        (s.demo.feed.clone(), s.live.configured())
    };
    let mut txns: Vec<(i64, Value)> = recs.iter().map(live_txn).collect();
    let listed: HashSet<&str> = recs.iter().filter_map(|r| r["event_id"].as_str()).collect();
    for a in &alerts {
        // a live alert older than the listed window, or an alert opened by /api/v2/assess
        let (id, source) = match a["event_id"].as_str() {
            Some(e) => (e, "live"),
            None => (a["alert_id"].as_str().unwrap_or(""), "assess"),
        };
        if listed.contains(id) {
            continue;
        }
        let asd = &a["assessed"];
        let (risk, reasons) = if asd["payment"].is_object() { (&asd["payment"]["risk"], &asd["payment"]["reasons"]) } else { (&asd["risk"], &Value::Null) };
        let mut t = txn(id, source, &a["payment"], risk, asd["final_decision"].as_str().unwrap_or("Approve"), reasons, &a["alert_id"]);
        t.1["event_id"] = a["event_id"].clone();
        txns.push(t);
    }
    for d in &demo {
        let side = |k: &str| {
            let (b, a) = d[k].as_str().unwrap_or("").split_once('·').unwrap_or_default();
            (json!(b), json!(a))
        };
        let ((fb, fa), (tb, ta)) = (side("from"), side("to"));
        let pay = json!({"timestamp": d["ts"], "from_bank": fb, "from_account": fa, "to_bank": tb, "to_account": ta,
                         "amount_paid": d["amount"], "payment_currency": d["currency"], "payment_format": d["format"]});
        let mut t = txn(&format!("D-{}", &hash_obj(d)[..16]), "demo_stream", &pay, &d["risk"], d["decision"].as_str().unwrap_or("Approve"), &d["reasons"], &Value::Null);
        t.1["case_id"] = d["case_id"].clone();
        t.1["synthetic_label"] = d["truth"].clone();
        txns.push(t);
    }
    let status: HashMap<&str, &Value> = alerts.iter().filter_map(|a| Some((a["alert_id"].as_str()?, &a["status"]))).collect();
    for (_, t) in &mut txns {
        t["alert_status"] = t["alert_id"].as_str().and_then(|a| status.get(a)).map(|s| (*s).clone()).unwrap_or(Value::Null);
    }
    txns.sort_by_key(|t| std::cmp::Reverse(t.0));
    Ok(Corpus { txns, alerts, live_seq, live_listed: recs.len(), live_capacity: live_capacity.min(SCAN), redis })
}

impl Corpus {
    /// What was read, so a page never passes a window off as the whole history.
    fn window(&self) -> Value {
        let mut by: BTreeMap<&str, usize> = SOURCES.iter().map(|s| (*s, 0)).collect();
        for (_, t) in &self.txns {
            *by.entry(t["source"].as_str().unwrap_or("")).or_default() += 1;
        }
        json!({"store": if self.redis { "redis" } else { "memory" }, "live_events_ingested": self.live_seq, "live_events_read": self.live_listed,
               "live_events_readable": self.live_capacity, "by_source": by,
               "note": if self.redis { "Redis mode: the newest 200 live decisions (the feed) and every stored live alert are searched; an older approved \
                                       event is found only by its id. Alerts from /api/v2/assess and the demo feed are this instance's." }
                       else { "Memory mode: decisions are kept in this process and lost on restart." }})
    }

    /// live, demo_stream, mixed or none: what the stored decisions are.
    fn data_kind(&self) -> &'static str {
        let has = |demo: bool| self.txns.iter().any(|t| (t.1["source"] == "demo_stream") == demo);
        match (self.live_seq > 0 || has(false), has(true)) {
            (true, true) => "mixed",
            (true, false) => "live",
            (false, true) => "demo_stream",
            _ => "none",
        }
    }
}

type Q = HashMap<String, String>;

fn q_str(q: &Q, k: &str, max: usize) -> Result<Option<String>, ApiError> {
    match q.get(k).filter(|v| !v.is_empty()) {
        Some(v) if v.chars().count() > max => Err(ApiError::new(422, format!("{k}: at most {max} characters"))),
        v => Ok(v.cloned()),
    }
}

fn q_one_of(q: &Q, k: &str, allowed: &[&str]) -> Result<Option<String>, ApiError> {
    match q_str(q, k, 32)? {
        Some(v) if !allowed.contains(&v.as_str()) => Err(ApiError::new(422, format!("{k}: must be one of {}", allowed.join(", ")))),
        v => Ok(v),
    }
}

fn q_amount(q: &Q, k: &str) -> Result<Option<f64>, ApiError> {
    match q_str(q, k, 32)? {
        None => Ok(None),
        Some(v) => v.trim().parse::<f64>().ok().filter(|x| x.is_finite() && *x >= 0.0).map(Some)
            .ok_or_else(|| ApiError::new(422, format!("{k}: must be a number of at least 0"))),
    }
}

/// `YYYY-MM-DD HH:MM` (UTC, `T` accepted for the space) or a date, which means its first minute, or its last for `end`.
fn q_time(q: &Q, k: &str, end: bool) -> Result<Option<i64>, ApiError> {
    let Some(v) = q_str(q, k, 32)? else { return Ok(None) };
    let v = v.replace('T', " ");
    parse_minutes(&v).or_else(|| parse_minutes(&format!("{v} 00:00")).map(|t| if end { t + 1439 } else { t })).map(Some)
        .ok_or_else(|| ApiError::new(422, format!("{k}: expected YYYY-MM-DD or YYYY-MM-DD HH:MM (UTC)")))
}

#[derive(Default)]
struct Filter {
    account: Option<String>,
    bank: Option<String>,
    band: Option<String>,
    source: Option<String>,
    typology: Option<String>,
    min: Option<f64>,
    max: Option<f64>,
    from: Option<i64>,
    to: Option<i64>,
}

impl Filter {
    fn parse(q: &Q) -> Result<Self, ApiError> {
        let f = Filter { account: q_str(q, "account", 128)?, bank: q_str(q, "bank", 128)?, band: q_one_of(q, "band", &live::ACTIONS)?,
                         source: q_one_of(q, "source", &SOURCES)?, typology: q_str(q, "typology", 64)?, min: q_amount(q, "min_amount")?,
                         max: q_amount(q, "max_amount")?, from: q_time(q, "from", false)?, to: q_time(q, "to", true)? };
        if matches!((f.min, f.max), (Some(a), Some(b)) if a > b) {
            return Err(ApiError::new(422, "min_amount is above max_amount"));
        }
        if matches!((f.from, f.to), (Some(a), Some(b)) if a > b) {
            return Err(ApiError::new(422, "from is after to"));
        }
        Ok(f)
    }

    fn in_time(&self, t: i64) -> bool {
        self.from.is_none_or(|a| t >= a) && self.to.is_none_or(|b| t <= b && t != i64::MIN)
    }

    fn keep(&self, t: i64, v: &Value) -> bool {
        let side = |s: &str| {
            self.account.as_deref().is_none_or(|a| v[format!("{s}_account")] == a) && self.bank.as_deref().is_none_or(|b| v[format!("{s}_bank")] == b)
        };
        let amount = v["amount"].as_f64();
        (side("from") || side("to"))
            && self.band.as_deref().is_none_or(|b| v["band"] == b)
            && self.source.as_deref().is_none_or(|s| v["source"] == s)
            && self.typology.as_deref().is_none_or(|m| v["typologies"].as_array().is_some_and(|a| a.iter().any(|x| x == m)))
            && self.min.is_none_or(|m| amount.is_some_and(|a| a >= m))
            && self.max.is_none_or(|m| amount.is_some_and(|a| a <= m))
            && self.in_time(t)
    }
}

/// GET /api/v2/transactions: stored payment decisions, newest payment first. Filters: account, bank (either side),
/// band (allow / verify / hold / block), source, typology (a motif name), min_amount, max_amount, from, to; limit
/// (default 50, at most 200) and offset.
pub async fn v2_transactions(State(app): State<AppRef>, h: HeaderMap, Query(q): Query<Q>) -> ApiResult {
    require_reader(&h)?;
    let f = Filter::parse(&q)?;
    let limit = int_query(&q.get("limit").cloned(), "limit", 50)?.clamp(1, PAGE_MAX) as usize;
    let offset = int_query(&q.get("offset").cloned(), "offset", 0)?.clamp(0, SCAN as i64 * 2) as usize;
    blocking_read(move || {
        let c = corpus(&app)?;
        let hits: Vec<&Value> = c.txns.iter().filter(|(t, v)| f.keep(*t, v)).map(|x| &x.1).collect();
        ok(json!({"total": hits.len(), "offset": offset, "limit": limit, "data": c.data_kind(),
                  "transactions": hits.iter().skip(offset).take(limit).collect::<Vec<_>>(), "window": c.window()}))
    }).await
}

fn key_of(v: &Value, side: &str) -> String {
    format!("{}:{}", v[format!("{side}_bank")].as_str().unwrap_or(""), v[format!("{side}_account")].as_str().unwrap_or(""))
}

fn brief(v: &Value) -> Value {
    json!({"id": v["id"], "source": v["source"], "timestamp": v["timestamp"], "from": key_of(v, "from"), "to": key_of(v, "to"),
           "amount": v["amount"], "currency": v["currency"], "band": v["band"], "risk": v["risk"], "alert_id": v["alert_id"]})
}

fn sum(xs: impl Iterator<Item = f64>) -> f64 {
    py_round(xs.sum(), 2)
}

/// GET /api/v2/transactions/{id}: one stored decision with what explains it and what surrounds it. `id` is the
/// event_id of a live event, the alert id of an /api/v2/assess alert, or a demo row's `D-` id.
pub async fn v2_transaction(State(app): State<AppRef>, h: HeaderMap, Path(id): Path<String>) -> ApiResult {
    require_reader(&h)?;
    if id.is_empty() || id.chars().count() > 128 {
        return Err(ApiError::new(422, "transaction id: 1 to 128 characters"));
    }
    blocking_read(move || {
        let c = corpus(&app)?;
        let t = match c.txns.iter().find(|x| x.1["id"] == id.as_str()) {
            Some(x) => x.1.clone(),
            // a live event older than the listed window still has its own record
            None => live::stored_event(&app, &id)?.map(|r| live_txn(&r).1).ok_or_else(|| ApiError::new(404, "unknown transaction"))?,
        };
        let (payer, payee) = (key_of(&t, "from"), key_of(&t, "to"));
        // accounts one stored payment away from the payer or the payee
        let mut related: indexmap::IndexMap<(String, &str), (usize, f64, usize)> = indexmap::IndexMap::new();
        let mut history: Vec<&Value> = Vec::new();
        for (_, v) in &c.txns {
            let (f, to) = (key_of(v, "from"), key_of(v, "to"));
            if to == payee {
                history.push(v);
            }
            let rel = if f == payer && to != payee { (to, "paid by the payer") } else if to == payer && f != payee { (f, "paid the payer") }
                else if to == payee && f != payer { (f, "paid the payee") } else if f == payee && to != payer { (to, "paid by the payee") } else { continue };
            let e = related.entry(rel).or_default();
            *e = (e.0 + 1, e.1 + v["amount"].as_f64().unwrap_or(0.0), e.2 + (v["band"] != "allow") as usize);
        }
        let alert = t["alert_id"].as_str().and_then(|a| c.alerts.iter().find(|x| x["alert_id"] == a));
        let s = lock(&app.state);
        let timeline: Vec<&Value> = s.desk.log.iter().filter(|e| {
            (e["event_id"].is_string() && e["event_id"] == t["event_id"]) || (e["alert_id"].is_string() && e["alert_id"] == t["alert_id"])
        }).take(200).collect();
        let cfg = s.engine.as_ref().map(|e| &e.cfg.raw);
        let reasons = t["reasons"].as_array().cloned().unwrap_or_default();
        let (motifs, model): (Vec<Value>, Vec<Value>) = reasons.into_iter().partition(|r| r.get("motifs").is_some());
        let none = model.is_empty() && motifs.is_empty();
        let payers: HashSet<String> = history.iter().map(|v| key_of(v, "from")).collect();
        ok(json!({
            "transaction": t,
            "risk": {"score": t["risk"], "band": t["band"], "decision": t["decision"],
                     "thresholds": cfg.map(|c| json!({"verify": c["verify_threshold"], "hold": c["hold_threshold"], "block": c["block_threshold"]})),
                     "source": "the cascade's served risk for this payment; thresholds from service/models/cascade_config.json (fitted on validation)"},
            "explanations": {"model_reasons": model, "motif_reasons": motifs, "available": !none,
                "note": if !none { "model_reasons are tree-SHAP contributions of the L2 XGBoost model; motif_reasons name the temporal motifs this payment completed" }
                        else if t["band"] == "allow" { "an approved payment carries no reason codes" }
                        else if t["store"] == "redis" { "Redis mode scores on the bank-scale path, which returns no reason codes" }
                        else { "no reason codes were stored for this decision" }},
            "model": {"service_version": VERSION, "ruleset": RULESET_VERSION, "trained_on": cfg.map(|c| c["trained_on"].clone()),
                      "hardened": s.engine.as_ref().and_then(|e| e.models.as_ref()).map(|m| m.hardening.is_some()),
                      "note": "the model files carry no version of their own: the service version and the context ruleset identify what scored this payment"},
            "alert": alert.map(|a| json!({"alert_id": a["alert_id"], "status": a["status"], "shown_decision": a["shown_decision"], "workflow": a["workflow"],
                                          "case_id": a["case_id"], "actions": a["actions"]})),
            "related_accounts": related.iter().take(50).map(|((acct, rel), (n, amt, flagged))|
                json!({"account": acct, "relation": rel, "payments": n, "total_amount": py_round(*amt, 2), "flagged_payments": flagged})).collect::<Vec<_>>(),
            "beneficiary_history": {"payee": payee, "payments": history.len(), "distinct_payers": payers.len(),
                "total_amount": sum(history.iter().filter_map(|v| v["amount"].as_f64())),
                "flagged_payments": history.iter().filter(|v| v["band"] != "allow").count(),
                "first_seen": history.last().map(|v| &v["timestamp"]), "last_seen": history.first().map(|v| &v["timestamp"]),
                "recent": history.iter().take(50).map(|v| brief(v)).collect::<Vec<_>>()},
            "timeline": timeline, "timeline_source": "ledger entries of this instance naming the event or its alert (GET /api/v2/audit)",
            "window": c.window(),
        }))
    }).await
}

/// GET /api/v2/graph?account=..: the accounts within `depth` stored payments of one account (1 to 3, default 1), one
/// directed edge per payment (amount, timestamp and the id of the stored decision as evidence), and the banks the
/// accounts are held at. Optional bank, from, to; limit is the most accounts returned (default 100, at most 300).
pub async fn v2_graph(State(app): State<AppRef>, h: HeaderMap, Query(q): Query<Q>) -> ApiResult {
    require_reader(&h)?;
    let account = q_str(&q, "account", 128)?.ok_or_else(|| ApiError::new(422, "account: required"))?;
    let f = Filter { bank: q_str(&q, "bank", 128)?, from: q_time(&q, "from", false)?, to: q_time(&q, "to", true)?, ..Default::default() };
    let depth = int_query(&q.get("depth").cloned(), "depth", 1)?;
    if !(1..=3).contains(&depth) {
        return Err(ApiError::new(422, "depth: 1 to 3"));
    }
    let limit = int_query(&q.get("limit").cloned(), "limit", 100)?.clamp(1, GRAPH_NODES_MAX) as usize;
    blocking_read(move || {
        let c = corpus(&app)?;
        let pays: Vec<(&Value, String, String)> = c.txns.iter().filter(|(t, _)| f.in_time(*t)).map(|(_, v)| (v, key_of(v, "from"), key_of(v, "to"))).collect();
        let mut near: HashMap<&str, Vec<&str>> = HashMap::new();
        for (_, a, b) in &pays {
            near.entry(a).or_default().push(b);
            near.entry(b).or_default().push(a);
        }
        let is_seed = |k: &str| k.split_once(':').is_some_and(|(b, a)| a == account && f.bank.as_deref().is_none_or(|x| x == b));
        // breadth first from the account, nearest first, until `limit` accounts
        let mut level: indexmap::IndexMap<&str, i64> = indexmap::IndexMap::new();
        let mut seeds: Vec<&str> = near.keys().copied().filter(|k| is_seed(k)).collect();
        seeds.sort();
        if seeds.is_empty() {
            return Err(ApiError::new(404, "no stored payment names this account"));
        }
        level.extend(seeds.into_iter().take(limit).map(|k| (k, 0)));
        let (mut i, mut truncated) = (0, false);
        while i < level.len() {
            let (k, d) = level.get_index(i).map(|(k, d)| (*k, *d)).unwrap();
            i += 1;
            if d == depth {
                continue;
            }
            for n in &near[k] {
                if level.contains_key(n) {
                    continue;
                }
                if level.len() >= limit {
                    truncated = true;
                    break;
                }
                level.insert(n, d + 1);
            }
        }
        let inside: Vec<&(&Value, String, String)> = pays.iter().filter(|(_, a, b)| level.contains_key(a.as_str()) && level.contains_key(b.as_str())).collect();
        truncated |= inside.len() > GRAPH_EDGES_MAX;
        // ring cases the engine linked on this instance: bank_account -> case id
        let s = lock(&app.state);
        let mut ring: HashMap<String, String> = HashMap::new();
        if let Some(e) = s.engine.as_ref() {
            for (root, edges) in &e.case_alerts {
                let id = rusetect::engine::case_id(root);
                for x in edges.iter().flat_map(|x| [&x.from, &x.to]) {
                    ring.entry(x.clone()).or_insert_with(|| id.clone());
                }
            }
        }
        drop(s);
        let mut nodes: indexmap::IndexMap<String, Value> = level.iter().map(|(k, d)| {
            let (bank, acct) = k.split_once(':').unwrap_or_default();
            let mut ind: Vec<Value> = Vec::new();
            if let Some(case) = ring.get(&format!("{bank}_{acct}")) {
                ind.push(json!({"kind": "ring_case", "name": case, "source": "engine L4 ring linker (GET /api/v2/cases)"}));
            }
            (format!("account:{k}"), json!({"id": format!("account:{k}"), "type": "account", "bank": bank, "account": acct, "depth": d, "seed": *d == 0,
                                           "sent": 0, "received": 0, "flagged_payments": 0, "alerts": [], "indicators": ind}))
        }).collect();
        let mut edges: Vec<Value> = Vec::new();
        let mut motifs: BTreeMap<(String, String), Vec<Value>> = BTreeMap::new();
        for (v, a, b) in inside.iter().take(GRAPH_EDGES_MAX) {
            for (k, dir) in [(a, "sent"), (b, "received")] {
                let n = &mut nodes[&format!("account:{k}")];
                n[dir] = json!(n[dir].as_i64().unwrap_or(0) + 1);
                if v["band"] != "allow" {
                    n["flagged_payments"] = json!(n["flagged_payments"].as_i64().unwrap_or(0) + 1);
                }
                let al = n["alerts"].as_array_mut().unwrap();
                if v["alert_id"].is_string() && al.len() < 10 && !al.contains(&v["alert_id"]) {
                    al.push(v["alert_id"].clone());
                }
                for m in v["typologies"].as_array().into_iter().flatten().filter_map(|m| m.as_str()) {
                    motifs.entry((format!("account:{k}"), m.to_string())).or_default().push(v["id"].clone());
                }
            }
            edges.push(json!({"id": v["id"], "type": "payment", "from": format!("account:{a}"), "to": format!("account:{b}"), "amount": v["amount"],
                              "currency": v["currency"], "format": v["format"], "timestamp": v["timestamp"], "band": v["band"], "risk": v["risk"],
                              "typologies": v["typologies"],
                              "evidence": {"source": v["source"], "transaction": v["id"], "event_id": v["event_id"], "alert_id": v["alert_id"],
                                           "ledger": v["ledger"], "demo": v["demo"]}}));
        }
        for ((node, name), ev) in motifs {
            let n = ev.len();
            nodes[&node]["indicators"].as_array_mut().unwrap().push(json!({"kind": "motif", "name": name, "payments": n,
                "evidence": ev.into_iter().take(5).collect::<Vec<_>>(), "source": "temporal-motif reason codes of flagged payments"}));
        }
        let banks: std::collections::BTreeSet<String> = nodes.values().filter_map(|n| n["bank"].as_str().map(String::from)).collect();
        let held: Vec<Value> = nodes.values().map(|n| json!({"id": format!("held:{}", n["id"].as_str().unwrap_or("")), "type": "held_at", "from": n["id"],
            "to": format!("bank:{}", n["bank"].as_str().unwrap_or("")), "evidence": {"source": "the bank code in the stored payment"}})).collect();
        let n_accounts = nodes.len();
        let mut all: Vec<Value> = nodes.into_values().collect();
        all.extend(banks.iter().map(|b| json!({"id": format!("bank:{b}"), "type": "bank", "bank": b})));
        let n_payments = edges.len();
        edges.extend(held);
        ok(json!({"seed": {"account": account, "bank": f.bank}, "depth": depth, "nodes": all, "edges": edges,
                  "counts": {"accounts": n_accounts, "banks": banks.len(), "payments": n_payments, "payments_between_these_accounts": inside.len()},
                  "truncated": truncated, "limits": {"accounts": limit, "payments": GRAPH_EDGES_MAX, "depth_max": 3},
                  "data": c.data_kind(), "window": c.window(), "disclaimer": NOT_GUILT,
                  "not_available": {"devices": "stored payments carry no device id (PRAMAAN keeps devices per user id, not per account)",
                                    "customers_and_merchants": "the payment schema names accounts and banks only"}}))
    }).await
}

fn top(counts: HashMap<String, usize>, n: usize) -> Vec<Value> {
    let mut v: Vec<(String, usize)> = counts.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v.into_iter().take(n).map(|(name, count)| json!({"name": name, "count": count})).collect()
}

/// GET /api/v2/dashboard: the numbers of the executive dashboard, each `{value, source}`. `data.kind` says whether the
/// stored decisions are live, the demo stream, both or none. `bucket` is hour (default) or day.
pub async fn v2_dashboard(State(app): State<AppRef>, h: HeaderMap, Query(q): Query<Q>) -> ApiResult {
    require_reader(&h)?;
    let bucket = if q_one_of(&q, "bucket", &["hour", "day"])?.as_deref() == Some("day") { 1440 } else { 60 };
    blocking_read(move || {
        let c = corpus(&app)?;
        let cases = live::all_cases(&app)?;
        let m = |value: Value, source: &str| json!({"value": value, "source": source});
        let count_by = |items: &mut dyn Iterator<Item = &Value>| {
            let mut by: BTreeMap<String, usize> = BTreeMap::new();
            for v in items {
                *by.entry(v.as_str().unwrap_or("unset").to_string()).or_default() += 1;
            }
            by
        };
        // stored decisions: bands, typologies, reason codes and the activity series
        let (mut typ, mut why): (HashMap<String, usize>, HashMap<String, usize>) = Default::default();
        let mut series: BTreeMap<i64, Map<String, Value>> = BTreeMap::new();
        for (t, v) in &c.txns {
            if v["band"] != "allow" {
                for x in v["typologies"].as_array().into_iter().flatten().filter_map(|x| x.as_str()) {
                    *typ.entry(x.into()).or_default() += 1;
                }
                for r in v["reasons"].as_array().into_iter().flatten().filter(|r| r.get("motifs").is_none()) {
                    *why.entry(r["feature"].as_str().unwrap_or("").into()).or_default() += 1;
                }
            }
            if *t != i64::MIN {
                let b = series.entry(t - t.rem_euclid(bucket)).or_default();
                for k in ["total", v["band"].as_str().unwrap_or("allow")] {
                    b.insert(k.into(), json!(b.get(k).and_then(|x| x.as_i64()).unwrap_or(0) + 1));
                }
            }
        }
        let series: Vec<Value> = series.into_iter().rev().take(72).rev().map(|(t, mut b)| {
            b.insert("t".into(), json!(chrono::DateTime::from_timestamp(t * 60, 0).map(|d| d.format("%Y-%m-%d %H:%M").to_string())));
            for k in live::ACTIONS {
                b.entry(k).or_insert(json!(0));
            }
            Value::Object(b)
        }).collect();
        let bands = count_by(&mut c.txns.iter().map(|t| &t.1["band"]));
        let flagged = c.txns.iter().filter(|t| t.1["band"] != "allow").count();
        let closed = |a: &&Value| matches!(a["status"].as_str(), Some("released" | "blocked"));
        let payees: HashSet<String> = c.alerts.iter().map(|a| key_of(&a["payment"], "to")).collect();
        let now = utc_now_z();
        let stored = "stored decisions read for this request (see window)";
        let desk = "review desk alerts (with Redis: plus live:alerts)";
        let s = lock(&app.state);
        let eng = s.engine.as_ref();
        let demo_n = s.demo.ptr.saturating_sub(s.demo.warm);
        let campaigns = rusetect::campaigns::build_recent(&s.desk.alerts).len();
        ok(json!({
            "generated_at": now,
            "data": {"kind": c.data_kind(), "live_events": c.live_seq, "demo_payments_replayed": demo_n,
                     "note": "live: events ingested by the signed webhook. demo_stream: the Live Console's replay of recorded IBM AMLworld (synthetic) \
                              payments, demonstration data. Counters marked `engine` cover both."},
            "payments": {
                "scored": m(json!(eng.map(|e| e.decisions.values().sum::<i64>())), "engine counter: payments this instance scored (API, demo replay, and the webhook in memory mode)"),
                "seen": m(json!(eng.map(|e| e.n_seen)), "engine counter: scored payments plus history loaded into account state without scoring (demo warm-up)"),
                "by_decision": m(json!(eng.map(|e| &e.decisions)), "engine counter, the model's own decision before context, lures or PRAMAAN"),
                "live_events": m(json!(c.live_seq), "live store sequence (all instances with Redis)"),
                "demo_replayed": m(json!(demo_n), "demo replay position on this instance"),
                "stored": m(json!(c.txns.len()), stored), "flagged": m(json!(flagged), stored), "by_band": m(json!(bands), stored)},
            "alerts": {
                "total": m(json!(c.alerts.len()), desk),
                "open": m(json!(c.alerts.iter().filter(|a| !closed(a)).count()), "alerts not yet released or blocked"),
                "closed": m(json!(c.alerts.iter().filter(closed).count()), "alerts released or blocked by analysts"),
                "by_status": m(json!(count_by(&mut c.alerts.iter().map(|a| &a["status"]))), desk),
                "by_priority": m(json!(count_by(&mut c.alerts.iter().map(|a| &a["workflow"]["priority"]))), "alert triage (POST /api/v2/alerts/{id}/triage)"),
                "unassigned": m(json!(c.alerts.iter().filter(|a| !closed(a) && !a["workflow"]["assignee"].is_string()).count()), "open alerts with no assignee")},
            "investigations": {
                "total": m(json!(cases.len()), "investigations (GET /api/v2/investigations)"),
                "by_status": m(json!(count_by(&mut cases.iter().map(|x| &x["status"]))), "investigations"),
                "by_priority": m(json!(count_by(&mut cases.iter().map(|x| &x["priority"]))), "investigations"),
                "sla_breached": m(json!(cases.iter().filter(|x| crate::workflow::breached(x, &now)).count()), "open investigations past sla_due")},
            "entities": {
                "flagged_payees": m(json!(payees.len()), "distinct payee accounts named in alerts; an indicator for review, not a finding"),
                "ring_cases": m(json!(eng.map(|e| e.case_alerts.len())), "engine L4 ring linker (GET /api/v2/cases), this instance"),
                "accounts_tracked": m(json!(eng.map(|e| e.n_accounts())), "engine account state, this instance"),
                "campaigns": m(json!(campaigns), "alerts linked into campaigns (GET /api/v2/campaigns), this instance")},
            "typologies": m(json!(top(typ, 10)), "temporal-motif reason codes of flagged stored decisions"),
            "reason_codes": m(json!(top(why, 10)), "tree-SHAP reason codes of flagged stored decisions"),
            "messages": m(s.lures.summary(), "lure detector counters (GET /api/v2/alerts/summary)"),
            "activity": {"bucket": if bucket == 60 { "hour" } else { "day" }, "series": series,
                         "source": "stored decisions by payment timestamp (UTC), the latest 72 buckets; demo rows are escalated payments only"},
            "detection": {"demo_stream_truth": if demo_n > 0 { json!(s.demo.truth) } else { Value::Null },
                          "source": "synthetic labels of the replayed demo stream (block band against the label); benchmark results: GET /api/v2/model-card",
                          "confirmed_fraud": Value::Null,
                          "confirmed_fraud_note": "no confirmed-fraud outcome is stored; alerts blocked by two analysts are in alerts.by_status"},
            "health": {
                "engine_loaded": eng.is_some(), "hardened": eng.and_then(|e| e.models.as_ref()).map(|m| m.hardening.is_some()).unwrap_or(false),
                "message_model": s.messages.is_some(), "pramaan": s.auth.is_some(), "sanctions": s.sanctions.is_some(), "payee_check": s.payee.is_some(),
                "store": s.live.store_json(), "durable": s.live.durable(), "ready": eng.is_some() && (!s.live.configured() || s.live.durable()),
                "version": VERSION, "ruleset": RULESET_VERSION, "automated_blocking": false,
                "source": "this instance (GET /api/health); a block needs two analysts"},
            "drift": {"status": s.drift.as_ref().map(|d| d.status()).unwrap_or("unavailable"), "source": "GET /api/v2/drift (this instance's window)"},
            "window": c.window(),
        }))
    }).await
}
