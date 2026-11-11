//! Stream an IBM AMLworld style CSV (Timestamp, From Bank, Account, To Bank, Account, Amount Received, Receiving
//! Currency, Amount Paid, Payment Currency, Payment Format[, Is Laundering]) through the partitioned workers, in time order.
//!   rusetect-replay --csv payments.csv.gz --partitions 2 [--limit 100000] [--rate 2000]
use rusetect::payment::Payment;
use rusetect::pyfmt::{parse_minutes_slash, py_float_str};
use rusetect_server::scale::producer::publish;

fn rows(path: &str, limit: usize) -> anyhow::Result<Vec<Payment>> {
    let f = std::fs::File::open(path)?;
    let r: Box<dyn std::io::Read> = if path.ends_with(".gz") { Box::new(flate2::read::MultiGzDecoder::new(f)) } else { Box::new(f) };
    let mut rd = csv::Reader::from_reader(std::io::BufReader::new(r));
    let mut out = vec![];
    for (i, rec) in rd.records().enumerate() {
        if limit > 0 && i >= limit {
            break;
        }
        let r = rec?;
        let g = |k: usize| r.get(k).unwrap_or("").to_string();
        out.push(Payment { eid: Some(i as i64), from_bank: g(1), from_account: g(2), to_bank: g(3), to_account: g(4),
            amount_paid: py_float_str(&g(7)).unwrap_or(0.0), payment_currency: g(8), receiving_currency: g(6), payment_format: g(9),
            tmin: parse_minutes_slash(&g(0)).ok_or_else(|| anyhow::anyhow!("bad timestamp in row {i}"))? });
    }
    out.sort_by_key(|p| p.tmin); // stable: same minute keeps file order
    Ok(out)
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let arg = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let csv = arg("--csv").ok_or_else(|| anyhow::anyhow!("--csv is required"))?;
    let parts: u32 = arg("--partitions").and_then(|v| v.parse().ok()).unwrap_or(2);
    let limit: usize = arg("--limit").and_then(|v| v.parse().ok()).unwrap_or(0);
    let rate: f64 = arg("--rate").and_then(|v| v.parse().ok()).unwrap_or(0.0);
    let url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379/0".into());
    let mut con = redis::Client::open(url)?.get_connection()?;
    let pays = rows(&csv, limit)?;
    let t0 = std::time::Instant::now();
    if rate > 0.0 {
        for (i, chunk) in pays.chunks(50).enumerate() {
            publish(&mut con, chunk, parts, "", 2000, false)?;
            let due = (i * 50 + chunk.len()) as f64 / rate;
            let el = t0.elapsed().as_secs_f64();
            if due > el {
                std::thread::sleep(std::time::Duration::from_secs_f64(due - el));
            }
        }
        for k in 0..parts {
            redis::cmd("XADD").arg(format!("payments:{k}")).arg("*").arg("stop").arg(1).query::<()>(&mut con)?;
        }
    } else {
        publish(&mut con, &pays, parts, "", 2000, true)?;
    }
    println!("published {} payments to {parts} partitions in {:.1}s", pays.len(), t0.elapsed().as_secs_f64());
    Ok(())
}
