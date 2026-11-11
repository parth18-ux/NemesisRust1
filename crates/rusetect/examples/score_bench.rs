//! Scoring cost per payment through Engine::score_payment on the bundled demo stream (after 100K payments of history).
//! cargo run --release -p rusetect --example score_bench [n]
use rusetect::{paths, payment, Engine, Payment};
use std::time::Instant;
fn main() -> anyhow::Result<()> {
    let n: usize = std::env::args().nth(1).map(|v| v.parse()).transpose()?.unwrap_or(20_000);
    let rows = payment::read_demo_rows(&paths::models_dir().join("demo_stream.csv.gz"))?;
    let pays: Vec<Payment> = rows.iter().map(|r| Payment::from_demo_row(r).unwrap()).collect();
    let mut e = Engine::new(&paths::models_dir(), true)?;
    for p in &pays[..100_000] { e.features(p); }
    let (mut ts, mut tf, mut ns, mut nf) = (0.0, 0.0, 0usize, 0usize);
    for p in &pays[100_000..100_000 + n] {
        let t = Instant::now();
        let r = e.score_payment(p);
        let d = t.elapsed().as_secs_f64();
        if r["band"] == "safe" { ts += d; ns += 1 } else { tf += d; nf += 1 }
    }
    println!("{n} payments: {:.0} per second; safe {ns} at {:.1} us, flagged {nf} at {:.1} us",
             n as f64 / (ts + tf), ts / ns.max(1) as f64 * 1e6, tf / nf.max(1) as f64 * 1e6);
    Ok(())
}
