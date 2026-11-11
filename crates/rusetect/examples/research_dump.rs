//! Research harness: stream an AMLworld style CSV (columns ts, fb, fa, tb, ta, amt_recv, cur_recv, amt_paid, cur_paid,
//! fmt, y) through the served engine and write, for every payment, the exact float32 model input vector, the served L1,
//! L2 XGBoost and L2 LightGBM scores and the label. Every payment is scored by both L2 halves (not only those that pass
//! L1) so that offline analysis can study the gate and the halves separately.
//!
//! cargo run --release -p rusetect --example research_dump -- <in.csv[.gz]> <out.csv[.gz]> [genuine keep rate]
//!
//! The analysis scripts in research/ read the output. State starts empty, exactly as in the console demo.
use rusetect::{paths, payment, Engine, Payment};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write;

/// Extra detection indicators for research (not served yet). All causal: each is computed from earlier payments only,
/// before the current payment updates the state, and none reads a label.
///   s_cnt_1h, s_usd_1h        sender outflow count and USD in the last 60 minutes (bursts)
///   d_cnt_1h, d_fanin_1h      payee inflow count and distinct senders in the last 60 minutes (mule collection)
///   d_usd_24h                 payee inflow USD in the last 24 hours (the engine has the count, not the sum)
///   pair_cnt_24h, pair_usd_24h  same payer to same payee in the last 24 hours (structuring: one sum split in parts)
///   amt_z                     log amount against the sender's own earlier log amounts (z score; needs 3 payments)
///   amt_to_max                amount over the sender's largest earlier payment
///   just_below_round          amount within 10% below a power of ten (9,500 under 10,000; 990 under 1,000)
///   hour_dev                  circular distance in hours from the sender's usual hour (needs 3 payments)
///   payee_age_min             minutes since the payee was first seen in any role (new collection accounts)
///   s_new_payees_24h          payments to never-seen payees by this sender in the last 24 hours
pub const EXTRA: [&str; 13] = ["s_cnt_1h", "s_usd_1h", "d_cnt_1h", "d_fanin_1h", "d_usd_24h", "pair_cnt_24h", "pair_usd_24h",
    "amt_z", "amt_to_max", "just_below_round", "hour_dev", "payee_age_min", "s_new_payees_24h"];

#[derive(Default)]
struct Acc {
    out: VecDeque<(i64, f64, bool)>, // (t, usd, was a new payee) for 24 h
    inn: VecDeque<(i64, f64, u32)>,  // (t, usd, sender) for 24 h
    n: f64, mean: f64, m2: f64, max: f64,
    hs: f64, hc: f64, hn: f64, // circular hour sums
    first: Option<i64>,
}

#[derive(Default)]
struct Extra {
    ids: HashMap<(String, String), u32>,
    acc: Vec<Acc>,
    pairs: HashMap<u64, VecDeque<(i64, f64)>>,
    seen_pair: HashSet<u64>,
}

impl Extra {
    fn id(&mut self, b: &str, a: &str) -> u32 {
        if let Some(i) = self.ids.get(&(b.to_string(), a.to_string())) { return *i; }
        let i = self.acc.len() as u32;
        self.ids.insert((b.to_string(), a.to_string()), i);
        self.acc.push(Acc::default());
        i
    }

    fn row(&mut self, p: &Payment, usd: f64) -> [f64; 13] {
        let (s, d) = (self.id(&p.from_bank, &p.from_account), self.id(&p.to_bank, &p.to_account));
        let t = p.tmin;
        let nan = f64::NAN;
        let key = (s as u64) << 32 | d as u64;
        let new_pair = !self.seen_pair.contains(&key);
        let lu = usd.max(0.0).ln_1p();
        let hour = ((t.rem_euclid(1440)) as f64) / 60.0;
        let mut o = [nan; 13];
        {
            let a = &mut self.acc[s as usize];
            while a.out.front().map(|x| x.0 <= t - 1440).unwrap_or(false) { a.out.pop_front(); }
            let h1: Vec<&(i64, f64, bool)> = a.out.iter().filter(|x| x.0 > t - 60).collect();
            o[0] = h1.len() as f64; o[1] = h1.iter().map(|x| x.1).sum::<f64>().ln_1p();
            o[12] = a.out.iter().filter(|x| x.2).count() as f64;
            if a.n >= 3.0 {
                let sd = (a.m2 / (a.n - 1.0)).sqrt();
                o[7] = if sd > 0.0 { (lu - a.mean) / sd } else { 0.0 };
                let mh = a.hs.atan2(a.hc).rem_euclid(std::f64::consts::TAU) * 24.0 / std::f64::consts::TAU;
                let dh = (hour - mh).abs();
                o[10] = dh.min(24.0 - dh);
            }
            o[8] = if a.max > 0.0 { usd / a.max } else { nan };
        }
        {
            let b = &mut self.acc[d as usize];
            while b.inn.front().map(|x| x.0 <= t - 1440).unwrap_or(false) { b.inn.pop_front(); }
            let h1: Vec<&(i64, f64, u32)> = b.inn.iter().filter(|x| x.0 > t - 60).collect();
            o[2] = h1.len() as f64;
            o[3] = h1.iter().map(|x| x.2).collect::<HashSet<u32>>().len() as f64;
            o[4] = b.inn.iter().map(|x| x.1).sum::<f64>().ln_1p();
            o[11] = b.first.map(|f| (t - f) as f64).unwrap_or(nan);
        }
        {
            let q = self.pairs.entry(key).or_default();
            while q.front().map(|x| x.0 <= t - 1440).unwrap_or(false) { q.pop_front(); }
            o[5] = q.len() as f64; o[6] = q.iter().map(|x| x.1).sum::<f64>().ln_1p();
            q.push_back((t, usd));
        }
        o[9] = if usd >= 1.0 { let m = usd / 10f64.powf(usd.log10().floor()); (m >= 9.0) as i64 as f64 } else { 0.0 };
        // update state
        let a = &mut self.acc[s as usize];
        a.out.push_back((t, usd, new_pair));
        a.n += 1.0; let dl = lu - a.mean; a.mean += dl / a.n; a.m2 += dl * (lu - a.mean);
        a.max = a.max.max(usd);
        let ang = hour / 24.0 * std::f64::consts::TAU; a.hs += ang.sin(); a.hc += ang.cos(); a.hn += 1.0;
        if a.first.is_none() { a.first = Some(t); }
        let b = &mut self.acc[d as usize];
        b.inn.push_back((t, usd, s));
        if b.first.is_none() { b.first = Some(t); }
        self.seen_pair.insert(key);
        o
    }
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let inp = args.next().map(std::path::PathBuf::from).unwrap_or_else(|| paths::models_dir().join("demo_stream.csv.gz"));
    let out = args.next().unwrap_or_else(|| "research_dump.csv".into());
    // optional share of genuine payments to keep (every positive is kept). State is still updated for every payment;
    // dropped rows skip scoring and formatting, which is most of the time on the Large files. Totals go to <out>.json.
    let neg_rate: f64 = args.next().map(|r| r.parse()).transpose()?.unwrap_or(1.0);
    let mut rng = 0x9E37_79B9_7F4A_7C15u64; // xorshift64: deterministic, no extra dependency
    let (mut kept, mut pos) = (0usize, 0usize);
    // stream rows one at a time so files larger than memory (HI-Medium, 32M payments) work
    let f = std::fs::File::open(&inp)?;
    let reader: Box<dyn std::io::Read> = if inp.extension().map(|e| e == "gz").unwrap_or(false) {
        Box::new(flate2::read::MultiGzDecoder::new(f))
    } else {
        Box::new(f)
    };
    let mut rd = csv::Reader::from_reader(std::io::BufReader::with_capacity(1 << 20, reader));
    let mut eng = Engine::new(&paths::models_dir(), true)?;
    // gzip the output when the name ends in .gz (the full datasets with all columns would not fit on disk otherwise)
    let file = std::fs::File::create(&out)?;
    let sink: Box<dyn Write> = if out.ends_with(".gz") { Box::new(flate2::write::GzEncoder::new(file, flate2::Compression::fast())) } else { Box::new(file) };
    let mut w = std::io::BufWriter::with_capacity(1 << 20, sink);
    let names = eng.cfg.features.clone();
    writeln!(w, "row,y,usd,fmt,l1,l2x,l2l,{},{}", names.join(","), EXTRA.join(","))?;
    let mut extra = Extra::default();
    let t = std::time::Instant::now();
    let mut n = 0usize;
    for (i, r) in rd.deserialize::<payment::DemoRow>().enumerate() {
        let r = &r?;
        n += 1;
        let p = Payment::from_demo_row(r).ok_or_else(|| anyhow::anyhow!("bad row {i}"))?;
        let usd = eng.cfg.usd(p.amount_paid, &p.payment_currency);
        let ex = extra.row(&p, usd);
        let f = eng.features(&p);
        let y1 = r.y == "1";
        pos += y1 as usize;
        if !y1 && neg_rate < 1.0 {
            rng ^= rng << 13; rng ^= rng >> 7; rng ^= rng << 17;
            if (rng >> 11) as f64 / (1u64 << 53) as f64 >= neg_rate { continue; }
        }
        kept += 1;
        let x = eng.vec(&f);
        let m = eng.models.as_ref().unwrap();
        let (s1, sx, sl) = (m.l1.predict(&x), m.l2x.predict(&x), m.l2l.predict_f32(&x));
        let xs: Vec<String> = x.iter().map(|v| if v.is_nan() { String::new() } else { format!("{v:?}") }).collect();
        let es: Vec<String> = ex.iter().map(|v| if v.is_nan() { String::new() } else { format!("{:.6}", v) }).collect();
        writeln!(w, "{i},{},{usd},{},{s1:?},{sx:?},{sl:?},{},{}", if r.y.is_empty() { "0" } else { &r.y }, r.fmt.replace(',', " "), xs.join(","), es.join(","))?;
    }
    w.flush()?;
    drop(w);
    if neg_rate < 1.0 {
        std::fs::write(format!("{out}.json"), format!("{{\"rows\": {n}, \"positives\": {pos}, \"kept\": {kept}, \"neg_rate\": {neg_rate}}}"))?;
    }
    eprintln!("dumped {n} payments ({kept} kept) in {:?} to {out}", t.elapsed());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pay(t: i64, to: &str, usd: f64) -> Payment {
        Payment { from_bank: "1".into(), from_account: "A".into(), to_bank: "2".into(), to_account: to.into(), amount_paid: usd,
                  payment_currency: "US Dollar".into(), receiving_currency: "US Dollar".into(), payment_format: "ACH".into(), tmin: t, eid: None }
    }

    #[test]
    fn indicators_are_causal_and_count_structuring() {
        let mut x = Extra::default();
        let a = x.row(&pay(0, "B", 9500.0), 9500.0);
        assert_eq!((a[0], a[5]), (0.0, 0.0), "first payment sees no history");
        assert_eq!(a[9], 1.0, "9,500 is just below 10,000");
        assert!(a[11].is_nan(), "a payee never seen has no age");
        let b = x.row(&pay(10, "B", 2500.0), 2500.0);
        assert_eq!((b[0], b[5], b[2]), (1.0, 1.0, 1.0), "second part of a split sees the first, not itself");
        assert_eq!(b[11], 10.0);
        let c = x.row(&pay(2000, "B", 2500.0), 2500.0);
        assert_eq!((c[0], c[5]), (0.0, 0.0), "windows forget payments older than 24 h");
    }
}
