//! Stateless scoring worker: reads payments from one Redis Stream partition, keeps account state in Redis, scores the
//! payment cascade and the context layer per payment, and writes decisions to the "decisions" stream.
use super::redis_state::RedisState;
use rusetect::context_rules::{assess, combine, from_engine_features};
use rusetect::engine::DECISIONS;
use rusetect::payment::Payment;
use rusetect::pyfmt::{dumps, py_round, DEFAULT_STYLE};
use rusetect::{paths, Engine};
use serde_json::{json, Value};

pub struct Scorer {
    pub eng: Engine,
    pub state: RedisState,
}

impl Scorer {
    pub fn new(con: redis::Connection, prefix: &str) -> anyhow::Result<Self> {
        let eng = Engine::new(&paths::models_dir(), true)?;
        let state = RedisState::new(con, eng.cfg.clone(), prefix);
        Ok(Scorer { eng, state })
    }

    pub fn score(&mut self, payments: &[Payment]) -> redis::RedisResult<Vec<Value>> {
        let m = self.eng.models.as_ref().unwrap();
        let cfg = &self.eng.cfg;
        let mut out = Vec::with_capacity(payments.len());
        for p in payments {
            let f = self.state.features(p)?;
            let sf = self.state.last_sender_fan_in;
            let x = self.eng.vec(&f);
            let s1 = m.l1.predict(&x) as f64;
            let mut esc = s1 >= cfg.l1_threshold;
            let mut risk = if esc { (m.l2x.predict(&x) as f64 + m.l2l.predict_f32(&x)) / 2.0 } else { s1 };
            // same format hardening as Engine::score_payment; shortcut: no pair (structuring) aggregation here, its
            // 24 h pair window would need to live in Redis next to the account state
            if let Some(h) = m.hardening.as_ref() {
                risk = h.risk(&x, risk).0;
                esc = true;
            }
            let (vt, ht, bt) = self.eng.thresholds();
            let rank = if !esc { 0 } else if risk >= bt { 3 } else if risk >= ht { 2 } else if risk >= vt { 1 } else { 0 };
            let ctx = assess(None, &from_engine_features(&f, sf), true, None, true, None, None);
            let c = combine(DECISIONS[rank], &ctx);
            out.push(json!({"eid": p.eid, "payment_decision": DECISIONS[rank], "risk": if esc { json!(py_round(risk, 5)) } else { Value::Null },
                            "context_band": ctx.band, "final_decision": c["final_decision"], "raised_by_context": c["raised_by_context"]}));
        }
        Ok(out)
    }
}

/// Consume one partition until a stop marker arrives; returns how many payments were scored.
pub fn run(redis_url: &str, partition: u32, batch: usize, group: &str, prefix: &str) -> anyhow::Result<usize> {
    let client = redis::Client::open(redis_url)?;
    let mut con = client.get_connection()?;
    let stream = format!("{prefix}payments:{partition}");
    let _: redis::RedisResult<()> = redis::cmd("XGROUP").arg("CREATE").arg(&stream).arg(group).arg("0").arg("MKSTREAM").query(&mut con);
    let mut sc = Scorer::new(client.get_connection()?, prefix)?;
    let mut n = 0usize;
    loop {
        let resp: redis::Value = redis::cmd("XREADGROUP").arg("GROUP").arg(group).arg(format!("worker-{partition}")).arg("COUNT").arg(batch)
            .arg("BLOCK").arg(2000).arg("STREAMS").arg(&stream).arg(">").query(&mut con)?;
        let msgs = parse_stream(resp);
        if msgs.is_empty() {
            continue;
        }
        let (mut stop, mut pays, mut ids, mut enq) = (false, vec![], vec![], vec![]);
        for (id, fields) in msgs {
            ids.push(id.clone());
            if fields.iter().any(|(k, _)| k == "stop") {
                stop = true;
                continue;
            }
            let Some(pj) = fields.iter().find(|(k, _)| k == "p").map(|x| x.1.clone()) else { continue };
            pays.push(serde_json::from_str::<Payment>(&pj)?);
            enq.push(fields.iter().find(|(k, _)| k == "enq_ns").and_then(|x| x.1.parse::<u128>().ok()).unwrap_or(0));
        }
        if !pays.is_empty() {
            let res = sc.score(&pays)?;
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            let mut pl = redis::pipe();
            for (mut d, e) in res.into_iter().zip(enq) {
                d["latency_ms"] = json!(py_round(now.saturating_sub(e) as f64 / 1e6, 2));
                d["partition"] = json!(partition);
                pl.cmd("XADD").arg(format!("{prefix}decisions")).arg("*").arg("d").arg(dumps(&d, DEFAULT_STYLE)).ignore();
            }
            pl.cmd("XACK").arg(&stream).arg(group).arg(&ids).ignore();
            pl.query::<()>(&mut con)?;
            n += pays.len();
        } else if !ids.is_empty() {
            redis::cmd("XACK").arg(&stream).arg(group).arg(&ids).query::<()>(&mut con)?;
        }
        if stop {
            redis::cmd("XADD").arg(format!("{prefix}decisions")).arg("*").arg("done").arg(partition).arg("n").arg(n).query::<()>(&mut con)?;
            return Ok(n);
        }
    }
}

/// XREADGROUP reply -> [(id, [(field, value)])]
fn parse_stream(v: redis::Value) -> Vec<(String, Vec<(String, String)>)> {
    use redis::Value as V;
    let s = |v: &V| -> String {
        match v {
            V::BulkString(b) => String::from_utf8_lossy(b).into_owned(),
            V::SimpleString(s) => s.clone(),
            V::Int(i) => i.to_string(),
            _ => String::new(),
        }
    };
    let mut out = vec![];
    if let V::Array(streams) = v {
        for st in streams {
            let V::Array(pair) = st else { continue };
            let Some(V::Array(entries)) = pair.get(1) else { continue };
            for e in entries {
                let V::Array(e) = e else { continue };
                let id = e.first().map(s).unwrap_or_default();
                let mut fields = vec![];
                if let Some(V::Array(kv)) = e.get(1) {
                    for c in kv.chunks(2) {
                        if c.len() == 2 {
                            fields.push((s(&c[0]), s(&c[1])));
                        }
                    }
                }
                out.push((id, fields));
            }
        }
    }
    out
}
