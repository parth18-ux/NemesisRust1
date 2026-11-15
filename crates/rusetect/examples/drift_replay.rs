//! Replay an AMLworld style CSV (ts, fb, fa, tb, ta, amt_recv, cur_recv, amt_paid, cur_paid, fmt, y) through the served
//! engine and the drift monitor (rusetect::drift), as the API does for each scored payment. Labels are never read.
//!
//! cargo run --release -p rusetect --example drift_replay -- <in.csv[.gz]> <out.json> <observe from row> [state from row] [stop row]
//!     [window sizes, comma separated; default the served one]
//!
//! Rows before `state from` are skipped. Every row from it is scored (building account state and the 24 h pair window);
//! rows from `observe from` also go to the monitor. Reading stops at `stop row` (exclusive), so later rows are never
//! parsed. Each window size gets its own monitor; its status is taken every CHECK observed payments once its window is
//! full (windows longer than CHECK overlap), and at the end.
//! Used by research/drift_replay.py.
use rusetect::drift::{DriftMonitor, WINDOW};
const CHECK: usize = 10_000;
use rusetect::{paths, payment, Engine, Payment};
use serde_json::{json, Value};

fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let arg = |i: usize, d: usize| a.get(i).map(|v| v.parse::<usize>()).transpose().map(|v| v.unwrap_or(d));
    let (inp, out) = (std::path::PathBuf::from(&a[0]), &a[1]);
    let (observe, state, stop) = (arg(2, 0)?, arg(3, 0)?, arg(4, usize::MAX)?);
    let sizes: Vec<usize> = match a.get(5) { Some(v) => v.split(',').map(|x| x.parse()).collect::<Result<_, _>>()?, None => vec![WINDOW] };
    let f = std::fs::File::open(&inp)?;
    let reader: Box<dyn std::io::Read> = if inp.extension().is_some_and(|e| e == "gz") { Box::new(flate2::read::MultiGzDecoder::new(f)) } else { Box::new(f) };
    let mut rd = csv::Reader::from_reader(std::io::BufReader::with_capacity(1 << 20, reader));
    let mut eng = Engine::new(&paths::models_dir(), true)?;
    let reference: Value = serde_json::from_slice(&std::fs::read(paths::models_dir().join("drift_reference.json"))?)?;
    let mut mons = sizes.iter().map(|n| DriftMonitor::from_reference(&reference, *n)).collect::<Option<Vec<_>>>().ok_or_else(|| anyhow::anyhow!("drift reference"))?;
    let mut checks: Vec<Vec<Value>> = vec![vec![]; sizes.len()];
    let mut seen = 0usize;
    let t = std::time::Instant::now();
    for (i, r) in rd.deserialize::<payment::DemoRow>().enumerate() {
        if i >= stop {
            break;
        }
        if i < state {
            continue;
        }
        let p = Payment::from_demo_row(&r?).ok_or_else(|| anyhow::anyhow!("bad row {i}"))?;
        let res = eng.score_payment(&p);
        if i < observe {
            continue;
        }
        seen += 1;
        for (m, c) in mons.iter_mut().zip(checks.iter_mut()) {
            m.observe_result(&res);
            if seen.is_multiple_of(CHECK) && seen >= m.capacity() {
                let r = m.report();
                c.push(json!({"last_row": i, "status": r["status"], "score_psi": r["score_psi"], "top_psi": r["top_psi"], "reasons": r["reasons"]}));
            }
        }
    }
    let by_size: serde_json::Map<String, Value> = sizes.iter().zip(mons.iter().zip(&checks)).map(|(n, (m, c))| {
        let count = |s: &str| c.iter().filter(|w| w["status"] == s).count();
        (n.to_string(), json!({"checks": c.len(), "check_status": {"ok": count("ok"), "warn": count("warn"), "alert": count("alert")},
                               "per_check": c, "final": m.report()}))
    }).collect();
    let res = json!({"observed": seen, "check_every": CHECK, "windows": by_size});
    std::fs::write(out, serde_json::to_string_pretty(&res)?)?;
    eprintln!("observed {seen} payments in {:?}", t.elapsed());
    Ok(())
}
