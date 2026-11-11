//! Stateless scoring worker for one partition.
//!   rusetect-worker --redis redis://localhost:6379/0 --partition 0 [--batch 256]
fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let arg = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let url = arg("--redis").or_else(|| std::env::var("REDIS_URL").ok()).unwrap_or_else(|| "redis://localhost:6379/0".into());
    let partition: u32 = arg("--partition").or_else(|| std::env::var("PARTITION").ok()).and_then(|p| p.parse().ok()).unwrap_or(0);
    let batch: usize = arg("--batch").and_then(|p| p.parse().ok()).unwrap_or(256);
    let n = rusetect_server::scale::worker::run(&url, partition, batch, "rusetect", "")?;
    println!("scored {n}");
    Ok(())
}
