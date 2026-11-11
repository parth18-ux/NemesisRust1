use rusetect::{paths, payment, Engine, Payment};
use std::time::Instant;
fn main() -> anyhow::Result<()> {
    let rows = payment::read_demo_rows(&paths::models_dir().join("demo_stream.csv.gz"))?;
    let mut eng = Engine::new(&paths::models_dir(), true)?;
    let pays: Vec<Payment> = rows.iter().map(|r| Payment::from_demo_row(r).unwrap()).collect();
    for p in &pays[..100_000] { eng.features(p); }
    let mut xs = vec![];
    let t = Instant::now();
    for p in &pays[100_000..120_000] { let f = eng.features(p); xs.push(eng.vec(&f)); }
    println!("features 20k: {:?}", t.elapsed());
    let m = eng.models.as_ref().unwrap();
    let t = Instant::now(); let mut esc = vec![];
    for x in &xs { if (m.l1.predict(x) as f64) >= eng.cfg.l1_threshold { esc.push(x.clone()); } }
    println!("L1 20k: {:?} ({} escalated)", t.elapsed(), esc.len());
    let t = Instant::now(); let mut s = 0.0; for x in &esc { s += m.l2x.predict(x) as f64; } println!("L2 xgb: {:?} {s:.1}", t.elapsed());
    let t = Instant::now(); let mut s = 0.0; for x in &esc { s += m.l2l.predict_f32(x); } println!("L2 lgbm: {:?} {s:.1}", t.elapsed());
    let t = Instant::now(); let mut s = 0.0f32; for x in &esc { s += m.l2x.contributions(x)[0]; } println!("TreeSHAP: {:?} {s:.1}", t.elapsed());
    Ok(())
}
