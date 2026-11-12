//! Payee name check over stdin lines "entered<TAB>account<TAB>type" (type may be empty); prints "score<TAB>verdict" per
//! line from the served Rust code. Used by research/payee_check_eval.py.
//!   payee_dump T_HI T_LO [v1|v2] < pairs.tsv
use std::io::{BufRead, Write};

fn main() -> anyhow::Result<()> {
    let arg = |i: usize| -> anyhow::Result<f64> { Ok(std::env::args().nth(i).ok_or_else(|| anyhow::anyhow!("usage: payee_dump T_HI T_LO"))?.parse()?) };
    let (t_hi, t_lo) = (arg(1)?, arg(2)?);
    let v2 = std::env::args().nth(3).as_deref() == Some("v2");
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        let mut f = line.split('\t');
        let (e, a, t) = (f.next().unwrap_or(""), f.next().unwrap_or(""), f.next().filter(|t| !t.is_empty()));
        let r = rusetect::payee::check(e, a, t, t_hi, t_lo, v2);
        writeln!(out, "{:.4}\t{}", r.score, r.verdict)?;
    }
    Ok(())
}
