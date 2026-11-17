//! Investigation narrative draft (the "AI-assisted investigation" of docs/IMPLEMENTATION_ROADMAP.md), built from fixed
//! templates, not a language model.
//!
//! Input: an investigation, the evidence pack of each of its alerts (with the live record of a webhook event when there
//! is one) and the campaigns the desk links. Output: statements, each typed as one of [`TYPES`] and carrying the ids it
//! was read from (alert, transaction, ledger seq, evidence pack hash, comment), plus suggested checks and open questions
//! chosen by which signals fired. The same input always gives the same draft (`digest`).
//!
//! * Facts and model outputs are copied from stored fields; rule findings name the rule and its ruleset.
//! * Interpretations come from a fixed table of conservative phrasings ("consistent with ..."), never "is fraud".
//! * Analyst conclusions are only what analysts wrote: case comments, closure reasons, review decisions and notes.
//! * A statement with no source is dropped, so none reaches the page. The draft decides nothing and changes nothing.
//! * Optional LLM rephrasing is documented as a hook only: it is not enabled and no code here calls any external API.
use crate::review::hash_obj;
use crate::str_report::{plain, py_str};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

pub const GENERATED_BY: &str = "template v1";
pub const NOTICE: &str = "Draft — requires analyst review. AI-assisted interpretations are not conclusions.";
pub const TYPES: [&str; 5] = ["observed_fact", "model_output", "rule_finding", "interpretation", "analyst_conclusion"];
pub const SECTIONS: [(&str, &str); 5] = [("summary", "Summary"), ("timeline", "Timeline"), ("signals", "Signals explained"),
                                         ("related_evidence", "Related evidence"), ("analyst_conclusions", "Analyst conclusions")];
/// Review decisions an analyst takes on an alert (rusetect::security::allowed).
const DECISIONS: [&str; 4] = ["release", "verify", "hold", "approve_block"];

fn s_alert(id: &str) -> Value {
    json!({"kind": "alert", "id": id})
}
fn s_txn(id: &str) -> Value {
    json!({"kind": "transaction", "id": id})
}
fn s_ledger(seq: &Value) -> Option<Value> {
    seq.as_u64().map(|n| json!({"kind": "ledger", "seq": n}))
}

fn money(v: &Value) -> String {
    v.as_f64().map(|f| format!("{f:.2}")).unwrap_or_else(|| py_str(v))
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// One alert of the investigation as the draft reads it.
struct Al<'a> {
    id: String,
    /// the transaction id the read endpoints use: the event id of a live event, else the alert id
    txn: String,
    a: &'a Value,
    x: &'a Value,
    rec: &'a Value,
    pack_hash: String,
    audit: Vec<&'a Value>,
    linked: Vec<String>,
}

impl Al<'_> {
    fn pay(&self) -> &Value {
        &self.a["payment"]
    }
    fn side(&self, s: &str) -> String {
        format!("{}/{}", py_str(&self.pay()[format!("{s}_bank")]), py_str(&self.pay()[format!("{s}_account")]))
    }
    fn pack(&self) -> Value {
        json!({"kind": "evidence_pack", "alert_id": self.id, "pack_hash": self.pack_hash})
    }
    fn both(&self) -> [Value; 2] {
        [s_txn(&self.txn), s_alert(&self.id)]
    }
    fn risk(&self) -> Option<f64> {
        self.x["payment"]["risk"].as_f64().or_else(|| self.x["risk"].as_f64()).or_else(|| self.rec["risk"].as_f64())
    }
    fn model_decision(&self) -> String {
        let d = &self.x["payment_decision"];
        py_str(if d.is_string() { d } else { &self.rec["payment_decision"] })
    }
    /// SHAP and motif reasons: the stored assessment's, else the live record's.
    fn reasons(&self) -> Vec<&Value> {
        let r = self.x["payment"]["reasons"].as_array().filter(|r| !r.is_empty()).or_else(|| self.rec["reasons"].as_array());
        r.into_iter().flatten().collect()
    }
    fn context_reasons(&self) -> Vec<String> {
        self.x["context"]["reasons"].as_array().into_iter().flatten().filter_map(|r| r.as_str().map(String::from)).collect()
    }
}

#[derive(Default)]
struct Draft {
    stmts: Vec<Value>,
    checks: Vec<Value>,
    questions: Vec<Value>,
}

fn dedup(sources: Vec<Value>) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for s in sources {
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

impl Draft {
    /// A statement; one without a source is dropped.
    fn say(&mut self, section: &str, kind: &str, template: &str, text: String, sources: Vec<Value>) {
        let sources = dedup(sources);
        if sources.is_empty() {
            return;
        }
        let id = format!("S{}", self.stmts.len() + 1);
        self.stmts.push(json!({"id": id, "section": section, "type": kind, "template_id": template, "text": text, "sources": sources}));
    }
    fn check(&mut self, template: &str, text: String, sources: Vec<Value>) {
        let sources = dedup(sources);
        if !sources.is_empty() && !self.checks.iter().any(|c| c["template_id"] == template) {
            let id = format!("C{}", self.checks.len() + 1);
            self.checks.push(json!({"id": id, "template_id": template, "text": text, "sources": sources}));
        }
    }
    fn ask(&mut self, template: &str, text: String, sources: Vec<Value>) {
        let sources = dedup(sources);
        if !sources.is_empty() {
            let id = format!("Q{}", self.questions.len() + 1);
            self.questions.push(json!({"id": id, "template_id": template, "text": text, "sources": sources}));
        }
    }
}

/// `case`: the investigation as stored. `alerts`: one `{"pack": evidence pack, "record": live record or null}` per alert
/// the desk still holds, in the investigation's order. `campaigns`: rusetect::campaigns::build_recent.
pub fn build(case: &Value, alerts: &[Value], campaigns: &[Value]) -> Value {
    let case_id = py_str(&case["case_id"]);
    let als: Vec<Al> = alerts.iter().filter_map(|e| {
        let a = &e["pack"]["alert"];
        let id = a["alert_id"].as_str()?.to_string();
        Some(Al {
            txn: a["event_id"].as_str().map(String::from).unwrap_or_else(|| id.clone()),
            x: &a["assessed"],
            rec: &e["record"],
            pack_hash: py_str(&e["pack"]["pack_hash"]),
            audit: e["pack"]["audit_entries"].as_array().into_iter().flatten().collect(),
            linked: e["pack"]["linked_alerts"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(String::from)).collect(),
            id,
            a,
        })
    }).collect();
    let mine: BTreeSet<&str> = als.iter().map(|a| a.id.as_str()).collect();
    let history: Vec<&Value> = case["history"].as_array().into_iter().flatten().collect();
    let opened: Vec<Value> = history.iter().filter(|h| h["action"] == "opened").filter_map(|h| s_ledger(&h["ledger_seq"])).collect();
    let all_txn = |f: &dyn Fn(&Al) -> bool| -> Vec<Value> { als.iter().filter(|a| f(a)).flat_map(|a| a.both()).collect() };
    let mut d = Draft::default();

    // ---- summary: what the investigation holds
    d.say("summary", "observed_fact", "fact.case",
          format!("Investigation {case_id} \"{}\" was opened by {} at {}; it is {} with priority {} and holds {}.",
                  py_str(&case["title"]), py_str(&case["created_by"]), py_str(&case["created_at"]), py_str(&case["status"]).replace('_', " "),
                  py_str(&case["priority"]), plural(case["alert_ids"].as_array().map_or(0, |a| a.len()), "alert", "alerts")),
          opened.iter().cloned().chain(als.iter().map(|a| s_alert(&a.id))).collect());
    if !als.is_empty() {
        let mut totals: BTreeMap<String, f64> = BTreeMap::new();
        for a in &als {
            *totals.entry(py_str(&a.pay()["payment_currency"])).or_default() += a.pay()["amount_paid"].as_f64().unwrap_or(0.0);
        }
        let ts: BTreeSet<String> = als.iter().map(|a| py_str(&a.pay()["timestamp"])).collect();
        let payers: BTreeSet<String> = als.iter().map(|a| a.side("from")).collect();
        let payees: BTreeSet<String> = als.iter().map(|a| a.side("to")).collect();
        d.say("summary", "observed_fact", "fact.payments",
              format!("Its {} total {} between {} and {} (payment time), from {} to {}.", plural(als.len(), "payment", "payments"),
                      totals.iter().map(|(c, v)| format!("{v:.2} {c}")).collect::<Vec<_>>().join(" and "),
                      ts.first().cloned().unwrap_or_default(), ts.last().cloned().unwrap_or_default(),
                      plural(payers.len(), "payer account", "payer accounts"), plural(payees.len(), "payee account", "payee accounts")),
              all_txn(&|_| true));
        let mut shown: BTreeMap<String, usize> = BTreeMap::new();
        for a in &als {
            *shown.entry(py_str(&a.a["shown_decision"])).or_default() += 1;
        }
        d.say("summary", "observed_fact", "fact.decisions",
              format!("Decisions shown on the alerts: {}. Alert status now: {}.", shown.iter().map(|(k, n)| format!("{k} ×{n}")).collect::<Vec<_>>().join(", "),
                      als.iter().map(|a| py_str(&a.a["status"])).collect::<BTreeSet<_>>().into_iter().collect::<Vec<_>>().join(", ")),
              als.iter().map(|a| s_alert(&a.id)).collect());
    }
    // accounts that both receive and send within the investigation, and payees with several payers
    let mut flows: BTreeMap<String, (Vec<usize>, Vec<usize>)> = BTreeMap::new();
    for (i, a) in als.iter().enumerate() {
        flows.entry(a.side("to")).or_default().0.push(i);
        flows.entry(a.side("from")).or_default().1.push(i);
    }
    let sum = |idx: &[usize]| idx.iter().map(|&i| als[i].pay()["amount_paid"].as_f64().unwrap_or(0.0)).sum::<f64>();
    let srcs = |idx: &[usize]| idx.iter().flat_map(|&i| als[i].both()).collect::<Vec<_>>();
    let mut hubs: Vec<String> = Vec::new();
    let mut fan_in: Vec<(String, Vec<Value>)> = Vec::new();
    for (acct, (inn, out)) in &flows {
        let payers: BTreeSet<String> = inn.iter().map(|&i| als[i].side("from")).collect();
        if !inn.is_empty() && !out.is_empty() {
            hubs.push(acct.clone());
            let payees: BTreeSet<String> = out.iter().map(|&i| als[i].side("to")).collect();
            d.say("summary", "observed_fact", "fact.hub",
                  format!("Account {acct} received {} (total {:.2}) from {} and sent {} (total {:.2}) to {} within this investigation.",
                          plural(inn.len(), "payment", "payments"), sum(inn), plural(payers.len(), "payer", "payers"),
                          plural(out.len(), "payment", "payments"), sum(out), plural(payees.len(), "payee", "payees")),
                  srcs(inn).into_iter().chain(srcs(out)).collect());
        } else if payers.len() >= 3 {
            d.say("summary", "observed_fact", "fact.many_payers",
                  format!("Account {acct} received {} (total {:.2}) from {} different payers within this investigation.",
                          plural(inn.len(), "payment", "payments"), sum(inn), payers.len()), srcs(inn));
        }
        if payers.len() >= 4 {
            fan_in.push((acct.clone(), srcs(inn)));
        }
    }

    // ---- timeline: payments by payment time, then ledger entries by sequence
    let mut by_time: Vec<&Al> = als.iter().collect();
    by_time.sort_by_key(|a| (py_str(&a.pay()["timestamp"]), a.id.clone()));
    for a in by_time {
        let p = a.pay();
        d.say("timeline", "observed_fact", "fact.payment",
              format!("{} (payment time): {} {} from {} to {} by {}; alert {} shows \"{}\".", py_str(&p["timestamp"]), money(&p["amount_paid"]),
                      py_str(&p["payment_currency"]), a.side("from"), a.side("to"), py_str(&p["payment_format"]), a.id, py_str(&a.a["shown_decision"])),
              a.both().to_vec());
    }
    let mut ledger: BTreeMap<u64, (Value, Option<&Al>)> = BTreeMap::new();
    for a in &als {
        for e in &a.audit {
            if let Some(n) = e["seq"].as_u64() {
                ledger.insert(n, ((*e).clone(), Some(a)));
            }
        }
    }
    for h in &history {
        if let Some(n) = h["ledger_seq"].as_u64() {
            ledger.insert(n, (json!({"seq": n, "at": h["at"], "type": "case_event", "analyst": h["analyst"], "action": h["action"]}), None));
        }
    }
    for (n, (e, a)) in &ledger {
        let at = py_str(&e["at"]);
        let text = match (e["type"].as_str().unwrap_or(""), a) {
            ("alert_opened", Some(a)) => format!("{at}: alert {} opened with decision \"{}\" (context band {}, ruleset {}).", a.id, py_str(&e["decision"]),
                                                 py_str(&e["context_band"]), py_str(&e["ruleset"])),
            ("live_decision", Some(a)) => format!("{at}: live event {} decided \"{}\" (alert {}).", py_str(&e["event_id"]), py_str(&e["final_decision"]), a.id),
            ("analyst_action", Some(a)) => format!("{at}: {} recorded \"{}\" on alert {} (status {}).", py_str(&e["analyst"]), py_str(&e["action"]), a.id,
                                                   py_str(&e["status"])),
            ("alert_triage", Some(a)) => format!("{at}: {} triaged alert {}{}.", py_str(&e["analyst"]), a.id,
                                                 if e["comment_hash"].is_string() { " with a comment" } else { "" }),
            ("case_event", _) => format!("{at}: {} recorded case action \"{}\" on {case_id}.", py_str(&e["analyst"]), py_str(&e["action"]).replace('_', " ")),
            (t, a) => format!("{at}: ledger entry \"{}\"{}.", t.replace('_', " "), a.map(|a| format!(" for alert {}", a.id)).unwrap_or_default()),
        };
        let mut s = vec![json!({"kind": "ledger", "seq": n})];
        if let Some(a) = a {
            s.push(s_alert(&a.id));
        }
        d.say("timeline", "observed_fact", "fact.ledger", text, s);
    }

    // ---- signals: model outputs, then rule findings, then interpretations of what fired
    let risks: Vec<(f64, usize)> = als.iter().enumerate().filter_map(|(i, a)| a.risk().map(|r| (r, i))).collect();
    if !risks.is_empty() {
        let mut dec: BTreeMap<String, usize> = BTreeMap::new();
        for &(_, i) in &risks {
            *dec.entry(als[i].model_decision()).or_default() += 1;
        }
        let (lo, hi) = risks.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), (r, _)| (lo.min(*r), hi.max(*r)));
        d.say("signals", "model_output", "model.risk",
              format!("Payment model (L1/L2 cascade): risk {lo:.5} to {hi:.5} across {}; its own decision: {}.", plural(risks.len(), "payment", "payments"),
                      dec.iter().map(|(k, n)| format!("{k} ×{n}")).collect::<Vec<_>>().join(", ")),
              risks.iter().flat_map(|&(_, i)| [s_txn(&als[i].txn), als[i].pack()]).collect());
    }
    let mut shap: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut motif: BTreeMap<String, (BTreeSet<String>, Vec<usize>)> = BTreeMap::new();
    let mut rules: BTreeMap<String, (BTreeSet<String>, Vec<usize>)> = BTreeMap::new();
    for (i, a) in als.iter().enumerate() {
        for r in a.reasons() {
            match r["motifs"].as_array() {
                Some(m) => {
                    let e = motif.entry(py_str(&r["display"])).or_default();
                    e.0.extend(m.iter().filter_map(|x| x.as_str().map(String::from)));
                    e.1.push(i);
                }
                None => shap.entry(format!("{} = {}", py_str(&r["feature"]), py_str(&r["display"]))).or_default().push(i),
            }
        }
        for r in a.context_reasons() {
            let e = rules.entry(r).or_default();
            e.0.insert(py_str(&a.x["context"]["ruleset"]));
            e.1.push(i);
        }
    }
    for (k, idx) in &shap {
        d.say("signals", "model_output", "model.shap",
              format!("Model reason (tree-SHAP, L2 XGBoost): {k}, on {}.", plural(idx.len(), "payment", "payments")), srcs(idx));
    }
    for (k, (names, idx)) in &motif {
        d.say("signals", "model_output", "model.motif",
              format!("Temporal motif feature of the payment model: {k} ({}), on {}.", names.iter().cloned().collect::<Vec<_>>().join(", ").replace('_', " "),
                      plural(idx.len(), "payment", "payments")), srcs(idx));
    }
    for a in &als {
        let m = &a.x["message_model"];
        if m["band"].is_string() {
            d.say("signals", "model_output", "model.message",
                  format!("Message model on the message sent with alert {}: band \"{}\"{}.", a.id, py_str(&m["band"]),
                          m["workflow"].as_str().map(|w| format!(", workflow \"{w}\"")).unwrap_or_default()), vec![s_alert(&a.id), a.pack()]);
        }
        let pr = &a.rec["pramaan"];
        if pr["decision"].is_string() {
            d.say("signals", "model_output", "model.pramaan",
                  format!("PRAMAAN account-takeover model on event {}: \"{}\" (risk score {}){}.", a.txn, py_str(&pr["decision"]), py_str(&pr["risk_score"]),
                          if pr["raised"] == true { "; it raised the decision" } else { "" }), a.both().to_vec());
        }
    }
    for (r, (rs, idx)) in &rules {
        d.say("signals", "rule_finding", "rule.context",
              format!("Rule \"{r}\" (context ruleset {}) fired on {}{}.", rs.iter().cloned().collect::<Vec<_>>().join(", "), plural(idx.len(), "payment", "payments"),
                      plain(r).map(|p| format!(": {p}")).unwrap_or_default()), srcs(idx));
    }
    let raised: Vec<usize> = (0..als.len()).filter(|&i| als[i].x["raised_by_context"] == true).collect();
    if !raised.is_empty() {
        let bands: BTreeSet<String> = raised.iter().map(|&i| py_str(&als[i].x["context_band"])).collect();
        d.say("signals", "rule_finding", "rule.raised",
              format!("The context layer (band {}) raised the decision above the payment model's own on {}.", bands.into_iter().collect::<Vec<_>>().join(", "),
                      plural(raised.len(), "payment", "payments")),
              raised.iter().flat_map(|&i| [s_alert(&als[i].id), als[i].pack()]).collect());
    }
    for a in &als {
        let banks = &a.x["network"]["flagged_by_consortium"];
        if banks.as_array().is_some_and(|b| !b.is_empty()) {
            d.say("signals", "rule_finding", "rule.consortium",
                  format!("The payee {} of alert {} is on the consortium mule registry, flagged by {}.", a.side("to"), a.id, banks), vec![s_alert(&a.id), a.pack()]);
        }
    }
    let mut camp_out: BTreeSet<String> = BTreeSet::new();
    for c in campaigns {
        let ids: Vec<String> = c["alerts"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(String::from)).collect();
        let inside: Vec<&String> = ids.iter().filter(|i| mine.contains(i.as_str())).collect();
        if inside.is_empty() {
            continue;
        }
        camp_out.extend(ids.iter().filter(|i| !mine.contains(i.as_str())).cloned());
        let by: Vec<String> = c["linked_by"].as_array().into_iter().flatten().map(py_str).collect();
        d.say("signals", "rule_finding", "rule.campaign",
              format!("Campaign rule: {} groups {} ({}), {} of them in this investigation; {}, total {}.", py_str(&c["campaign_id"]), plural(ids.len(), "alert", "alerts"),
                      by.join("; "), inside.len(), plural(c["victims"].as_array().map_or(0, |v| v.len()), "payer", "payers"), money(&c["total_amount"])),
              inside.iter().map(|i| s_alert(i)).collect());
    }
    // interpretations: fixed phrasings, each sourced to the signals it reads
    let fired = |pred: &dyn Fn(&Al) -> bool| all_txn(&|a| pred(a));
    let has_rule = |a: &Al, r: &str| a.context_reasons().iter().any(|x| x == r);
    let has_motif = |a: &Al, m: &str| a.reasons().iter().any(|r| r["motifs"].as_array().is_some_and(|x| x.iter().any(|v| v == m)));
    let pass = fired(&|a| has_rule(a, "account graph: rapid_pass_through") || has_motif(a, "pass_through"));
    d.say("signals", "interpretation", "interp.pass_through",
          format!("The flow{} is consistent with a pass-through (money-mule) pattern: money left within minutes of arriving. This is an interpretation of the signals, not a finding.",
                  if hubs.is_empty() { String::new() } else { format!(" through {}", hubs.join(", ")) }), pass.clone());
    d.say("signals", "interpretation", "interp.mule_signature",
          "The payer's pattern (collecting from several accounts, then forwarding almost all of it within two minutes) is consistent with an account used to move other people's money. It does not show who controls the account.".into(),
          fired(&|a| has_rule(a, "account graph: sender_mule_signature")));
    let mut fan = fired(&|a| has_rule(a, "account graph: fan_in_4plus") || has_motif(a, "fan_in"));
    let fan_accts: Vec<String> = fan_in.iter().map(|f| f.0.clone()).collect();
    fan.extend(fan_in.into_iter().flat_map(|f| f.1));
    d.say("signals", "interpretation", "interp.fan_in",
          format!("Payments from many payers into {} are consistent with collection (fan-in); they may also have an ordinary explanation such as a merchant or a payroll account.",
                  if fan_accts.is_empty() { "one payee".to_string() } else { fan_accts.join(", ") }), fan.clone());
    let lure = fired(&|a| a.context_reasons().iter().any(|r| r.starts_with("message cue:")));
    d.say("signals", "interpretation", "interp.lure",
          "The message cues are consistent with a social-engineering lure sent before the payment; whether the customer acted on it is not recorded.".into(), lure.clone());
    let ato = fired(&|a| a.rec["pramaan"]["decision"].as_str().is_some_and(|x| x != "allow") || a.context_reasons().iter().any(|r| r.starts_with("session:")));
    d.say("signals", "interpretation", "interp.takeover",
          "The session and account-takeover signals are consistent with someone other than the customer controlling the session; they are not proof of it.".into(), ato.clone());
    d.say("signals", "interpretation", "interp.cycle",
          "A cycle motif is consistent with money returning to where it started, a pattern seen in layering; it can also come from ordinary transfers between related accounts.".into(),
          fired(&|a| has_motif(a, "cycle")));
    let model_only_rules = fired(&|a| a.model_decision() == "Approve" && a.x["raised_by_context"] == true);
    d.say("signals", "interpretation", "interp.rules_not_model",
          "The payment model alone approved these payments; the alerts rest on rule and motif signals. This is consistent with a pattern that shows across payments rather than in any single one.".into(),
          model_only_rules.clone());

    // ---- related evidence
    for a in &als {
        let seqs: Vec<Value> = a.audit.iter().filter_map(|e| s_ledger(&e["seq"])).collect();
        d.say("related_evidence", "observed_fact", "fact.pack",
              format!("Evidence pack of alert {}: pack hash {}…, {} ({}).", a.id, &a.pack_hash[..a.pack_hash.len().min(16)], plural(seqs.len(), "ledger entry", "ledger entries"),
                      seqs.iter().map(|s| format!("seq {}", s["seq"])).collect::<Vec<_>>().join(", ")),
              std::iter::once(a.pack()).chain(seqs.clone()).collect());
    }
    let outside: BTreeSet<String> = als.iter().flat_map(|a| a.linked.iter().cloned()).filter(|l| !mine.contains(l.as_str())).collect();
    if !outside.is_empty() {
        d.say("related_evidence", "observed_fact", "fact.outside",
              format!("{} share a payer or payee account with this investigation's alerts but are not in it: {}.", plural(outside.len(), "alert", "alerts"),
                      outside.iter().cloned().collect::<Vec<_>>().join(", ")),
              outside.iter().map(|i| s_alert(i)).collect());
    }

    // ---- analyst conclusions: only what analysts wrote or decided
    for (i, c) in case["comments"].as_array().into_iter().flatten().enumerate() {
        let h = hash_obj(&c["text"]);
        let seq = history.iter().find(|x| x["action"] == "comment" && x["detail"]["comment_hash"] == h.as_str()).and_then(|x| s_ledger(&x["ledger_seq"]));
        d.say("analyst_conclusions", "analyst_conclusion", "analyst.comment",
              format!("{} ({}): \"{}\"", py_str(&c["analyst"]), py_str(&c["at"]), py_str(&c["text"])),
              std::iter::once(json!({"kind": "comment", "case_id": case_id, "index": i})).chain(seq).collect());
    }
    let cl = &case["closure"];
    if cl["reason"].is_string() {
        let seq = history.iter().rev().find(|x| x["action"] == "request_close").and_then(|x| s_ledger(&x["ledger_seq"]));
        d.say("analyst_conclusions", "analyst_conclusion", "analyst.closure",
              format!("{} requested closure: \"{}\"{}.", py_str(&cl["requested_by"]), py_str(&cl["reason"]),
                      if cl["approved_by"].is_string() { format!("; approved by {}", py_str(&cl["approved_by"])) }
                      else if cl["rejected_by"].is_string() { format!("; rejected by {}", py_str(&cl["rejected_by"])) } else { String::new() }),
              seq.into_iter().collect());
    }
    for a in &als {
        for act in a.a["actions"].as_array().into_iter().flatten() {
            let (who, what, note) = (py_str(&act["analyst"]), py_str(&act["action"]), act["note"].as_str().unwrap_or(""));
            if who == "system" || (!DECISIONS.contains(&what.as_str()) && note.is_empty()) {
                continue;
            }
            d.say("analyst_conclusions", "analyst_conclusion", "analyst.alert",
                  format!("{who} on alert {}: {}{}.", a.id, what.replace('_', " "), if note.is_empty() { String::new() } else { format!(" (\"{note}\")") }),
                  vec![s_alert(&a.id), a.pack()]);
        }
    }

    // ---- suggested checks: chosen by which signals fired
    let payees = all_txn(&|_| true);
    if !pass.is_empty() || !hubs.is_empty() {
        d.check("check.kyc", format!("Request the KYC file and account-opening documents of {}; compare the stated occupation and expected turnover with the observed flow.",
                                     if hubs.is_empty() { "the pass-through account".into() } else { hubs.join(", ") }), pass.iter().cloned().chain(all_txn(&|a| hubs.contains(&a.side("to")) || hubs.contains(&a.side("from")))).collect());
        d.check("check.next_hop", "Trace the onward payments of the receiving accounts (the next hop) at their banks; the stored payments end at this bank's view.".into(), pass.clone());
    }
    if !fan.is_empty() {
        d.check("check.payers", "Contact a sample of the payers to confirm whether they authorised the payments and why they paid this account.".into(), fan.clone());
    }
    if !lure.is_empty() {
        d.check("check.message", "Read the redacted message in the evidence pack and confirm with the customer how the payment instruction reached them.".into(), lure.clone());
    }
    if !ato.is_empty() {
        d.check("check.device", "Check the device, SIM-change and login history with the customer before releasing any held payment.".into(), ato.clone());
    }
    if !camp_out.is_empty() {
        d.check("check.campaign", format!("Review the campaign's other alerts ({}) and consider adding them to this investigation.", camp_out.iter().cloned().collect::<Vec<_>>().join(", ")),
                camp_out.iter().map(|i| s_alert(i)).collect());
    }
    d.check("check.sanctions", "Screen the payee names against the sanctions list (POST /api/v2/sanctions/screen) and run a payee name check (POST /api/v2/payee/check): stored payments carry account ids, not names, so neither was run for this investigation.".into(),
            payees.clone());
    d.check("check.verify", "Verify each evidence pack's signature and the ledger chain (Reports) before relying on this draft or filing an STR.".into(),
            als.iter().map(|a| a.pack()).collect());

    // ---- open questions
    let has_conclusion = d.stmts.iter().any(|s| s["type"] == "analyst_conclusion");
    if !has_conclusion {
        d.ask("question.no_conclusion", "No analyst conclusion is recorded yet: what does the assigned analyst conclude, and on which evidence?".into(),
              opened.iter().cloned().chain(als.iter().map(|a| s_alert(&a.id))).collect());
    }
    let open: Vec<&Al> = als.iter().filter(|a| matches!(a.a["status"].as_str(), Some("open") | Some("block_pending_second_approval"))).collect();
    if !open.is_empty() {
        d.ask("question.open_alerts", format!("{} still await a review decision ({}): release, verify, hold or block?", plural(open.len(), "alert", "alerts"),
                                               open.iter().map(|a| a.id.as_str()).collect::<Vec<_>>().join(", ")),
              open.iter().map(|a| s_alert(&a.id)).collect());
    }
    if !outside.is_empty() {
        d.ask("question.outside", "Should the alerts that share an account with this investigation be added to it?".into(), outside.iter().map(|i| s_alert(i)).collect());
    }
    if !model_only_rules.is_empty() {
        d.ask("question.rules_only", "Is the rule and motif evidence enough on its own, given that the payment model approved these payments?".into(), model_only_rules);
    }
    let missing: Vec<&str> = case["alert_ids"].as_array().into_iter().flatten().filter_map(|v| v.as_str()).filter(|i| !mine.contains(i)).collect();
    if !missing.is_empty() {
        d.ask("question.missing", format!("The desk no longer holds {} ({}): is a copy of their evidence kept elsewhere?", plural(missing.len(), "alert", "alerts"), missing.join(", ")),
              opened.clone());
    }

    let digest = hash_obj(&json!({"statements": d.stmts, "suggested_checks": d.checks, "open_questions": d.questions}));
    json!({
        "case_id": case_id,
        "status": "DRAFT - requires analyst review",
        "requires_human_review": true,
        "generated_by": GENERATED_BY,
        "notice": NOTICE,
        "decides_nothing": "This draft never blocks, releases or changes a decision; it only reads stored data.",
        "types": {"observed_fact": "copied from a stored payment, alert or ledger entry", "model_output": "a score or reason the models returned",
                  "rule_finding": "a rule that fired, with its ruleset", "interpretation": "a fixed, conservative template reading of the signals above; not a conclusion",
                  "analyst_conclusion": "written or decided by an analyst"},
        "sections": SECTIONS.iter().map(|(k, t)| json!({"id": k, "title": t})).collect::<Vec<_>>(),
        "statements": d.stmts,
        "suggested_checks": d.checks,
        "open_questions": d.questions,
        "inputs": {"alerts": als.iter().map(|a| json!({"alert_id": a.id, "transaction": a.txn, "pack_hash": a.pack_hash})).collect::<Vec<_>>(),
                   "case_ledger_seqs": history.iter().filter_map(|h| h["ledger_seq"].as_u64()).collect::<Vec<_>>()},
        "digest": digest,
        "llm_rephrase": {"enabled": false, "note": "Optional hook, not enabled: an operator-run model could only rephrase this draft, keeping every statement's sources. No code calls an external API."},
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack(id: &str, from: &str, to: &str, ts: &str, reasons: Value, motifs: Value) -> Value {
        json!({"pack": {"alert": {"alert_id": id, "status": "open", "shown_decision": "Step-up verification",
            "payment": {"timestamp": ts, "from_bank": "01", "from_account": from, "to_bank": "02", "to_account": to, "amount_paid": 100.0,
                        "payment_currency": "Rupee", "payment_format": "ACH"},
            "assessed": {"payment": {"risk": 0.001, "decision": "Approve", "reasons": motifs}, "payment_decision": "Approve", "context_band": "MEDIUM",
                         "raised_by_context": true, "context": {"reasons": reasons, "ruleset": "ctx-1.4"}}, "actions": []},
            "audit_entries": [{"seq": 1, "at": "t", "type": "alert_opened", "decision": "Step-up verification"}], "linked_alerts": [], "pack_hash": "ab".repeat(32)},
               "record": null})
    }

    #[test]
    fn every_statement_is_typed_and_sourced_and_the_draft_is_deterministic() {
        let case = json!({"case_id": "INV-00001", "title": "t", "status": "open", "priority": "high", "created_by": "a", "created_at": "t",
                          "alert_ids": ["A1", "A2", "GONE"], "comments": [{"at": "t", "analyst": "a", "text": "looks like a mule"}],
                          "history": [{"action": "opened", "ledger_seq": 3, "at": "t", "analyst": "a"},
                                      {"action": "comment", "ledger_seq": 4, "at": "t", "analyst": "a", "detail": {"comment_hash": hash_obj(&json!("looks like a mule"))}}],
                          "closure": null});
        let mot = json!([{"feature": "Temporal motif", "display": "pass-through chain (24 h)", "motifs": ["pass_through"]}]);
        let alerts = [pack("A1", "V1", "M", "2022-09-09 10:00", json!(["message cue: authority"]), json!([])),
                      pack("A2", "M", "C", "2022-09-09 10:01", json!(["account graph: rapid_pass_through"]), mot)];
        let n = build(&case, &alerts, &[]);
        assert_eq!(n, build(&case, &alerts, &[]), "same input, same draft");
        assert_eq!((n["requires_human_review"].as_bool(), n["generated_by"].as_str()), (Some(true), Some(GENERATED_BY)));
        let st = n["statements"].as_array().unwrap();
        for s in st.iter().chain(n["suggested_checks"].as_array().unwrap()).chain(n["open_questions"].as_array().unwrap()) {
            assert!(!s["sources"].as_array().unwrap().is_empty(), "{s}");
        }
        for s in st {
            assert!(TYPES.contains(&s["type"].as_str().unwrap()), "{s}");
            assert!(!s["text"].as_str().unwrap().to_lowercase().contains("is fraud"), "{s}");
        }
        let kinds = |t: &str| st.iter().filter(|s| s["type"] == t).map(|s| s["template_id"].as_str().unwrap().to_string()).collect::<Vec<_>>();
        assert!(kinds("interpretation").contains(&"interp.pass_through".to_string()) && kinds("interpretation").contains(&"interp.lure".to_string()));
        assert_eq!(kinds("analyst_conclusion"), ["analyst.comment"], "only the analyst's comment is a conclusion");
        assert!(kinds("rule_finding").contains(&"rule.context".to_string()) && kinds("model_output").contains(&"model.motif".to_string()));
        let c = st.iter().find(|s| s["type"] == "analyst_conclusion").unwrap();
        assert!(c["sources"].as_array().unwrap().contains(&json!({"kind": "ledger", "seq": 4})), "the comment links its ledger entry");
        assert!(n["open_questions"].as_array().unwrap().iter().any(|q| q["template_id"] == "question.missing"));
        assert!(n["open_questions"].as_array().unwrap().iter().all(|q| q["template_id"] != "question.no_conclusion"));
    }
}
