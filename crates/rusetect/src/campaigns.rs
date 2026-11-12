//! Scam campaigns: link alerts that are the same operation hitting different people (port of service/campaigns.py).
//!
//! Two alerts join a campaign when they share a payee account, a payee identifier hashed out of a message, or a near
//! identical script (Jaccard similarity of 5 character shingles of the REDACTED messages >= 0.5). Union find over
//! those links; only groups of two or more alerts are reported. Each campaign lists why its alerts are linked.
use crate::pyfmt::{py_float_value, py_round, py_split, py_sum};
use crate::str_report::py_str;
use indexmap::IndexMap;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashSet};

pub const SIM: f64 = 0.5;

pub fn shingles(text: &str, k: usize) -> HashSet<String> {
    let t: Vec<char> = py_split(&text.to_lowercase()).join(" ").chars().collect();
    if t.len() < k {
        return HashSet::new();
    }
    (0..=t.len() - k).map(|i| t[i..i + k].iter().collect()).collect()
}

pub fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    a.intersection(b).count() as f64 / a.union(b).count() as f64
}

/// Campaigns are built from at most this many of the newest alerts.
// shortcut: build() is O(n^2) and runs under the state lock, so only the newest 2,000 alerts are linked; index by payee/entity/shingle when older links matter.
pub const MAX_ALERTS: usize = 2_000;

/// Campaigns over the newest MAX_ALERTS alerts of the review desk (kept in arrival order).
pub fn build_recent(alerts: &IndexMap<String, Value>) -> Vec<Value> {
    let all: Vec<&Value> = alerts.values().skip(alerts.len().saturating_sub(MAX_ALERTS)).collect();
    build(&all)
}

/// alerts: ReviewDesk alerts (payment + assessed.context with redacted_text and entity_hashes).
pub fn build(alerts: &[&Value]) -> Vec<Value> {
    let n = alerts.len();
    let mut parent: Vec<usize> = (0..n).collect();
    let mut why: BTreeMap<(usize, usize), BTreeSet<&'static str>> = BTreeMap::new();
    fn find(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    let mut union = |parent: &mut Vec<usize>, i: usize, j: usize, reason: &'static str| {
        let (a, b) = (find(parent, i), find(parent, j));
        if a != b {
            parent[a] = b;
        }
        why.entry((i.min(j), i.max(j))).or_default().insert(reason);
    };
    let payee: Vec<String> = alerts.iter().map(|a| format!("{}:{}", py_str(&a["payment"]["to_bank"]), py_str(&a["payment"]["to_account"]))).collect();
    let ctx: Vec<&Value> = alerts.iter().map(|a| a["assessed"].get("context").unwrap_or(&Value::Null)).collect();
    let ents: Vec<HashSet<String>> = ctx.iter().map(|c| c.get("entity_hashes").and_then(|e| e.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default()).collect();
    let red = |c: &Value| c.get("redacted_text").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let sh: Vec<HashSet<String>> = ctx.iter().map(|c| shingles(&red(c), 5)).collect();
    let mut by_payee: IndexMap<&str, Vec<usize>> = IndexMap::new();
    for (i, p) in payee.iter().enumerate() {
        by_payee.entry(p.as_str()).or_default().push(i);
    }
    for idx in by_payee.values() {
        for &j in &idx[1..] {
            union(&mut parent, idx[0], j, "same payee account");
        }
    }
    for i in 0..n {
        for j in i + 1..n {
            if !ents[i].is_disjoint(&ents[j]) {
                union(&mut parent, i, j, "same payee identifier in messages");
            }
            if !sh[i].is_empty() && !sh[j].is_empty() && jaccard(&sh[i], &sh[j]) >= SIM {
                union(&mut parent, i, j, "near identical script");
            }
        }
    }
    let mut groups: IndexMap<usize, Vec<usize>> = IndexMap::new();
    for i in 0..n {
        let r = find(&mut parent, i);
        groups.entry(r).or_default().push(i);
    }
    let mut out: Vec<(usize, Value)> = Vec::new();
    for (root, idx) in &groups {
        if idx.len() < 2 {
            continue;
        }
        let mut reasons: BTreeSet<&str> = BTreeSet::new();
        for ((i, _), rs) in &why {
            if find(&mut parent, *i) == *root {
                reasons.extend(rs.iter());
            }
        }
        let mut ids: Vec<String> = idx.iter().map(|&i| py_str(&alerts[i]["alert_id"])).collect();
        let alert_ids = ids.clone();
        ids.sort();
        let cid = format!("CMP-{}", hex::encode(Sha256::digest(ids.join("|").as_bytes()))[..8].to_uppercase());
        let scripts: Vec<String> = idx.iter().map(|&i| red(ctx[i])).filter(|s| !s.is_empty()).collect();
        let victims: BTreeSet<String> = idx.iter().map(|&i| py_str(&alerts[i]["payment"]["from_account"])).collect();
        let payees: BTreeSet<&str> = idx.iter().map(|&i| payee[i].as_str()).collect();
        let total = py_sum(idx.iter().map(|&i| py_float_value(&alerts[i]["payment"]["amount_paid"]).unwrap_or(0.0)));
        let ts: Vec<String> = idx.iter().map(|&i| py_str(&alerts[i]["payment"]["timestamp"])).collect();
        let statuses: BTreeSet<String> = idx.iter().map(|&i| py_str(&alerts[i]["status"])).collect();
        out.push((idx.len(), json!({"campaign_id": cid, "alerts": alert_ids, "size": idx.len(), "victims": victims, "payees": payees,
            "linked_by": reasons, "total_amount": py_round(total, 2), "first_seen": ts.iter().min(), "last_seen": ts.iter().max(),
            "sample_script": scripts.first(), "statuses": statuses})));
    }
    out.sort_by_key(|x| std::cmp::Reverse(x.0));
    out.into_iter().map(|x| x.1).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn campaigns_link_by_payee_identifier_and_script() {
        let al = |i: usize, payee: &str, text: &str, ents: Vec<&str>| json!({"alert_id": format!("A{i}"), "status": "open",
            "payment": {"to_bank": "011", "to_account": payee, "from_account": format!("V{i}"), "amount_paid": 1000, "timestamp": format!("2022-09-0{i} 10:00")},
            "assessed": {"context": {"redacted_text": text, "entity_hashes": ents}}});
        let s = "Your KYC will expire today. Pay the verification fee to <UPI> now or your account will be blocked.";
        let a = [al(1, "P1", s, vec!["h1"]), al(2, "P2", &s.replace("today", "tonight"), vec![]), al(3, "P3", "hello", vec!["h1"]), al(4, "P9", "unrelated", vec![])];
        let cs = build(&a.iter().collect::<Vec<_>>());
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0]["size"], 3);
        let lb: Vec<&str> = cs[0]["linked_by"].as_array().unwrap().iter().map(|x| x.as_str().unwrap()).collect();
        assert!(lb.contains(&"near identical script") && lb.contains(&"same payee identifier in messages"));
    }
}
