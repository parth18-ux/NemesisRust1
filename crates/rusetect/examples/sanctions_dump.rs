//! Score names read from stdin (one per line) against a watchlist CSV; prints, per name, a tab-separated line of the
//! served scores against every list entry in file order. Used by research/sanctions_eval.py.
//!   sanctions_dump service/models/sanctions_sdn.csv [--v2] < names.txt
use rusetect::sanctions::List;
use std::io::{BufRead, Write};

fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).ok_or_else(|| anyhow::anyhow!("usage: sanctions_dump <list.csv>"))?;
    let mut list = List::load(path.as_ref())?;
    list.v2 = std::env::args().nth(2).as_deref() == Some("--v2");
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    for line in std::io::stdin().lock().lines() {
        let s: Vec<String> = list.scores(&line?).iter().map(|(s, _)| format!("{s:.6}")).collect();
        writeln!(out, "{}", s.join("\t"))?;
    }
    Ok(())
}
