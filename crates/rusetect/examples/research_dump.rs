//! Research harness: stream an AMLworld style CSV (columns ts, fb, fa, tb, ta, amt_recv, cur_recv, amt_paid, cur_paid,
//! fmt, y) through the served engine and write, for every payment, the exact float32 model input vector, the served L1,
//! L2 XGBoost and L2 LightGBM scores and the label. Every payment is scored by both L2 halves (not only those that pass
//! L1) so that offline analysis can study the gate and the halves separately.
//!
//! cargo run --release -p rusetect --example research_dump -- <in.csv[.gz]> <out.csv>
//!
//! The analysis scripts in research/ read the output. State starts empty, exactly as in the console demo.
use rusetect::{paths, payment, Engine, Payment};
use std::io::Write;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let inp = args.next().map(std::path::PathBuf::from).unwrap_or_else(|| paths::models_dir().join("demo_stream.csv.gz"));
    let out = args.next().unwrap_or_else(|| "research_dump.csv".into());
    let rows = payment::read_demo_rows(&inp)?;
    let mut eng = Engine::new(&paths::models_dir(), true)?;
    let mut w = std::io::BufWriter::new(std::fs::File::create(&out)?);
    let names = eng.cfg.features.clone();
    writeln!(w, "row,y,usd,fmt,l1,l2x,l2l,{}", names.join(","))?;
    let t = std::time::Instant::now();
    for (i, r) in rows.iter().enumerate() {
        let p = Payment::from_demo_row(r).ok_or_else(|| anyhow::anyhow!("bad row {i}"))?;
        let usd = eng.cfg.usd(p.amount_paid, &p.payment_currency);
        let f = eng.features(&p);
        let x = eng.vec(&f);
        let m = eng.models.as_ref().unwrap();
        let (s1, sx, sl) = (m.l1.predict(&x), m.l2x.predict(&x), m.l2l.predict_f32(&x));
        let xs: Vec<String> = x.iter().map(|v| if v.is_nan() { String::new() } else { format!("{v:?}") }).collect();
        writeln!(w, "{i},{},{usd},{},{s1:?},{sx:?},{sl:?},{}", if r.y.is_empty() { "0" } else { &r.y }, r.fmt.replace(',', " "), xs.join(","))?;
    }
    eprintln!("dumped {} payments in {:?} to {out}", rows.len(), t.elapsed());
    Ok(())
}
