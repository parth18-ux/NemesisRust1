//! Dump engine scores for the first N demo rows as JSON lines (used to compare against the Python engine).
use rusetect::{paths, payment, Engine, Payment};
fn main() -> anyhow::Result<()> {
    let n: usize = std::env::args().nth(1).map(|s| s.parse().unwrap()).unwrap_or(5000);
    let warm: usize = std::env::args().nth(2).map(|s| s.parse().unwrap()).unwrap_or(0);
    let rows = payment::read_demo_rows(&paths::models_dir().join("demo_stream.csv.gz"))?;
    let mut eng = Engine::new(&paths::models_dir(), true)?;
    let t = std::time::Instant::now();
    for r in &rows[..warm] { eng.features(&Payment::from_demo_row(r).unwrap()); }
    for r in &rows[warm..warm + n] {
        let p = Payment::from_demo_row(r).unwrap();
        let mut o = eng.score_payment(&p);
        o.remove("latency_ms");
        println!("{}", rusetect::pyfmt::dumps(&serde_json::Value::Object(o), rusetect::pyfmt::RESPONSE_STYLE));
    }
    eprintln!("scored {n} in {:?}; cases {}", t.elapsed(), eng.cases(3).len());
    Ok(())
}
