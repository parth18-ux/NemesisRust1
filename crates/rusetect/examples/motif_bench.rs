//! Cost and counts of the temporal-motif reason on the score_bench slice (100K payments of history, then n scored);
//! writes results/motif_reasons.json. cargo run --release -p rusetect --example motif_bench [n]
use rusetect::{paths, payment, Engine, Payment};
use serde_json::json;
use std::time::Instant;
fn main() -> anyhow::Result<()> {
    let n: usize = std::env::args().nth(1).map(|v| v.parse()).transpose()?.unwrap_or(20_000);
    let rows = payment::read_demo_rows(&paths::models_dir().join("demo_stream.csv.gz"))?;
    let pays: Vec<Payment> = rows.iter().map(|r| Payment::from_demo_row(r).unwrap()).collect();
    let mut e = Engine::new(&paths::models_dir(), true)?;
    for p in &pays[..100_000] { e.features(p); }
    let (mut flagged, mut with, mut on_allow, mut sum, mut max) = (0usize, 0usize, 0usize, 0.0f64, 0.0f64);
    let mut counts = serde_json::Map::new();
    for p in &pays[100_000..100_000 + n] {
        let r = e.score_payment(p);
        let m = r["reasons"].as_array().unwrap().iter().find(|x| x.get("motifs").is_some());
        if r["band"] == "safe" {
            on_allow += m.is_some() as usize;
            continue;
        }
        flagged += 1;
        // the pass alone, repeated on the state the engine just used (it only reads)
        let t = Instant::now();
        let again = std::hint::black_box(e.motif_reason(p));
        let us = t.elapsed().as_secs_f64() * 1e6;
        assert_eq!(again.as_ref(), m, "the timed pass returns the served reason");
        (sum, max) = (sum + us, max.max(us));
        for k in m.iter().flat_map(|x| x["motifs"].as_array().unwrap()) {
            let c = counts.entry(k.as_str().unwrap()).or_insert(json!(0));
            *c = json!(c.as_i64().unwrap() + 1);
        }
        with += m.is_some() as usize;
    }
    let mean = sum / flagged.max(1) as f64;
    let out = json!({"slice": format!("demo_stream.csv.gz: 100000 payments of history, then {n} scored"), "scored": n, "flagged": flagged,
        "flagged_with_motif": with, "approved_with_motif": on_allow, "motif_counts": counts,
        "motif_pass_mean_us": (mean * 100.0).round() / 100.0, "motif_pass_max_us": (max * 100.0).round() / 100.0,
        "bound_mean_us": 50.0, "within_bound": mean <= 50.0});
    std::fs::write(paths::repo_root().join("results/motif_reasons.json"), serde_json::to_string_pretty(&out)? + "\n")?;
    println!("{out}");
    Ok(())
}
