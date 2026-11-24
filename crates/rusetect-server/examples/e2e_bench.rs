//! End-to-end benchmark of the real HTTP server: starts the release `rusetect-server` binary on loopback, drives
//! POST /api/v2/payment and the signed POST /api/v2/events/payment webhook over keep-alive HTTP/1.1 at a few fixed
//! concurrencies, in memory mode and with a local redis-server, and writes results/e2e_bench.json.
//!
//!   cargo build --release -p rusetect-server --bins --examples
//!   target/release/examples/e2e_bench [requests per level, default 5000]
//!   target/release/examples/e2e_bench s1 baseline|after   (S1 protocol, results/e2e_bench_s1_protocol.json)
//!   target/release/examples/e2e_bench scale <variant> <rep>  (8ap protocol, docs/decision.md; results/scale_<variant>.json)
//!
//! One machine: client, server and redis-server share its CPUs. This is not a measurement of the Vercel deployment.
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

const SECRET: &str = "e2e-bench-webhook-secret-0123456789";
const LEVELS: [usize; 3] = [1, 8, 32];
const WARMUP: usize = 200;
const FORMATS: [&str; 5] = ["ACH", "Cheque", "Credit Card", "Wire", "Cash"];

struct Kill(Child);
impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn cmd_out(c: &str, args: &[&str]) -> Option<String> {
    let o = Command::new(c).args(args).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// Payment `i`: 400 payers, 300 payees, one minute apart, so state stays bounded and timestamps rise.
fn payment(i: usize) -> Value {
    let t = chrono::DateTime::from_timestamp(1_661_990_400 + 60 * i as i64, 0).unwrap();
    json!({"timestamp": t.format("%Y-%m-%d %H:%M").to_string(), "from_bank": "070", "from_account": format!("B{:04}", i % 400),
           "to_bank": "011", "to_account": format!("P{:04}", (i * 7) % 300), "amount_paid": 500 + (i * 37) % 20_000,
           "payment_format": FORMATS[i % 5]})
}

fn request(port: u16, webhook: bool, i: usize, run: &str) -> Vec<u8> {
    let mut body = payment(i);
    let mut extra = String::new();
    let path = if webhook {
        body["event_id"] = json!(format!("{run}-{i}"));
        let ts = chrono::Utc::now().timestamp();
        let mut m = Hmac::<Sha256>::new_from_slice(SECRET.as_bytes()).unwrap();
        m.update(format!("{ts}.{body}").as_bytes());
        extra = format!("x-rusetect-timestamp: {ts}\r\nx-rusetect-signature: v1={}\r\n", hex::encode(m.finalize().into_bytes()));
        "/api/v2/events/payment"
    } else {
        "/api/v2/payment"
    };
    let b = body.to_string();
    format!("POST {path} HTTP/1.1\r\nhost: 127.0.0.1:{port}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n{extra}\r\n{b}", b.len()).into_bytes()
}

/// Send one request on a kept connection; returns the status code.
async fn roundtrip(r: &mut BufReader<TcpStream>, req: &[u8]) -> std::io::Result<u16> {
    Ok(exchange(r, req).await?.0)
}

/// Send one request on a kept connection; returns (status, Idempotent-Replayed: true, body).
async fn exchange(r: &mut BufReader<TcpStream>, req: &[u8]) -> std::io::Result<(u16, bool, Vec<u8>)> {
    r.get_mut().write_all(req).await?;
    let (mut line, mut status, mut len, mut replayed) = (String::new(), 0u16, 0usize, false);
    loop {
        line.clear();
        if r.read_line(&mut line).await? == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        let l = line.trim_end();
        if l.is_empty() {
            break;
        }
        if let Some(s) = l.strip_prefix("HTTP/1.1 ") {
            status = s.get(..3).and_then(|c| c.parse().ok()).unwrap_or(0);
        } else if let Some((k, v)) = l.split_once(':') {
            if k.eq_ignore_ascii_case("content-length") {
                len = v.trim().parse().unwrap_or(0);
            } else if k.eq_ignore_ascii_case("idempotent-replayed") {
                replayed = v.trim() == "true";
            }
        }
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body).await?;
    Ok((status, replayed, body))
}

/// `n` requests from `conc` keep-alive connections; request indexes start at `*next`.
async fn level(port: u16, webhook: bool, conc: usize, n: usize, next: &mut usize, run: &str) -> Value {
    level_sampled(port, webhook, conc, n, next, run, usize::MAX).await.0
}

/// As `level`, also returning (request index, first response body) for every `every`-th request.
async fn level_sampled(port: u16, webhook: bool, conc: usize, n: usize, next: &mut usize, run: &str, every: usize) -> (Value, Vec<(usize, Vec<u8>)>) {
    let start = *next;
    *next += n;
    let idx = Arc::new(AtomicUsize::new(0));
    let t0 = Instant::now();
    let mut tasks = vec![];
    for _ in 0..conc {
        let (idx, run) = (idx.clone(), run.to_string());
        tasks.push(tokio::spawn(async move {
            let mut r = BufReader::new(TcpStream::connect(("127.0.0.1", port)).await.unwrap());
            r.get_mut().set_nodelay(true).unwrap();
            let (mut lat, mut errors, mut kept) = (vec![], 0usize, vec![]);
            loop {
                let k = idx.fetch_add(1, Ordering::Relaxed);
                if k >= n {
                    break;
                }
                let req = request(port, webhook, start + k, &run);
                let t = Instant::now();
                match exchange(&mut r, &req).await {
                    Ok((200, _, body)) => {
                        lat.push(t.elapsed().as_secs_f64() * 1000.0);
                        if k % every == 0 {
                            kept.push((start + k, body));
                        }
                    }
                    Ok(_) => errors += 1,
                    Err(_) => {
                        errors += 1;
                        r = BufReader::new(TcpStream::connect(("127.0.0.1", port)).await.unwrap());
                    }
                }
            }
            (lat, errors, kept)
        }));
    }
    let (mut lat, mut errors, mut kept) = (vec![], 0, vec![]);
    for t in tasks {
        let (l, e, s) = t.await.unwrap();
        lat.extend(l);
        errors += e;
        kept.extend(s);
    }
    let wall = t0.elapsed().as_secs_f64();
    if n > 0 && errors > 0 {
        eprintln!("  {errors} non-200 responses");
    }
    lat.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let pct = |p: f64| if lat.is_empty() { 0.0 } else { lat[((p / 100.0 * lat.len() as f64).ceil() as usize).clamp(1, lat.len()) - 1] };
    let r2 = |x: f64| (x * 100.0).round() / 100.0;
    (json!({"concurrency": conc, "requests": n, "ok": lat.len(), "errors": errors, "wall_s": r2(wall),
           "throughput_rps": r2(lat.len() as f64 / wall),
           "latency_ms": {"p50": r2(pct(50.0)), "p95": r2(pct(95.0)), "p99": r2(pct(99.0)), "max": r2(pct(100.0)),
                          "mean": r2(lat.iter().sum::<f64>() / lat.len().max(1) as f64)}}), kept)
}

fn machine() -> Value {
    let cpu = std::fs::read_to_string("/proc/cpuinfo").ok()
        .and_then(|s| s.lines().find(|l| l.starts_with("model name")).and_then(|l| l.split_once(':')).map(|(_, v)| v.trim().to_string()));
    json!({"cpu": cpu, "logical_cpus": std::thread::available_parallelism().map(|n| n.get()).ok(),
           "os": cmd_out("uname", &["-sr"]), "rustc": cmd_out("rustc", &["-V"]), "redis_server": cmd_out("redis-server", &["--version"])})
}

/// Start a redis-server on a free port; returns it and its URL once it accepts connections.
async fn redis_server() -> (Kill, String) {
    redis_server_on(free_port()).await
}

/// As `redis_server`, on a given port.
async fn redis_server_on(rport: u16) -> (Kill, String) {
    let redis = Kill(Command::new("redis-server").args(["--bind", "127.0.0.1", "--port", &rport.to_string(), "--save", "", "--appendonly", "no"])
        .stdout(Stdio::null()).spawn().expect("redis-server must be installed"));
    for _ in 0..100 {
        if TcpStream::connect(("127.0.0.1", rport)).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    (redis, format!("redis://127.0.0.1:{rport}/0"))
}

/// Start the server binary (next to this example's directory) and wait for /api/health.
async fn server(redis_url: Option<&str>) -> (Kill, u16) {
    server_on(free_port(), redis_url).await
}

/// As `server`, on a given port.
async fn server_on(port: u16, redis_url: Option<&str>) -> (Kill, u16) {
    let exe = std::env::current_exe().unwrap();
    let bin = exe.parent().unwrap().parent().unwrap().join("rusetect-server");
    assert!(bin.exists(), "{} not found: run `cargo build --release -p rusetect-server --bins --examples` first", bin.display());
    let mut c = Command::new(bin);
    c.args(["--host", "127.0.0.1", "--port", &port.to_string()]).env("RUSETECT_DEMO", "1").env("RUSETECT_RATE_LIMIT", "1000000000")
        .env("RUSETECT_WEBHOOK_SECRET", SECRET).env_remove("REDIS_URL").env_remove("KV_URL").stdout(Stdio::null()).stderr(Stdio::null());
    if let Some(u) = redis_url {
        c.env("REDIS_URL", u).env("RUSETECT_REDIS_PREFIX", "bench:");
    }
    let child = Kill(c.spawn().expect("start rusetect-server"));
    let health = format!("GET /api/health HTTP/1.1\r\nhost: 127.0.0.1:{port}\r\n\r\n").into_bytes();
    for _ in 0..600 {
        if let Ok(s) = TcpStream::connect(("127.0.0.1", port)).await {
            if roundtrip(&mut BufReader::new(s), &health).await.ok() == Some(200) {
                return (child, port);
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("rusetect-server did not become healthy");
}

/// Run every endpoint and concurrency level against one fresh server.
async fn mode(name: &str, redis_url: Option<&str>, endpoints: &[(&str, bool)], n: usize) -> Value {
    let (_srv, port) = server(redis_url).await;
    let mut out = serde_json::Map::new();
    let mut next = 0;
    for &(ep, webhook) in endpoints {
        level(port, webhook, 4, WARMUP, &mut next, &format!("{name}-warm")).await;
        let mut rows = vec![];
        for c in LEVELS {
            eprintln!("{name} {ep} concurrency {c}");
            rows.push(level(port, webhook, c, n, &mut next, &format!("{name}-c{c}")).await);
        }
        out.insert(ep.to_string(), json!(rows));
    }
    Value::Object(out)
}

#[tokio::main]
async fn main() {
    if std::env::args().nth(1).as_deref() == Some("f1") {
        return f1::main(std::env::args().nth(2).as_deref()).await;
    }
    if std::env::args().nth(1).as_deref() == Some("s1") {
        return s1::main(&std::env::args().nth(2).expect("s1 baseline|after")).await;
    }
    if std::env::args().nth(1).as_deref() == Some("scale") {
        let rep = std::env::args().nth(3).and_then(|r| r.parse().ok()).expect("scale <variant> <rep>");
        return s1::scale(&std::env::args().nth(2).expect("scale <variant> <rep>"), rep).await;
    }
    let n: usize = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(5000);
    let mut results = serde_json::Map::new();
    let both = [("POST /api/v2/payment", false), ("POST /api/v2/events/payment", true)];
    results.insert("memory".into(), mode("memory", None, &both, n).await);

    // redis mode: /v2/payment does not touch the store, so only the webhook is run
    let (redis, url) = redis_server().await;
    results.insert("redis".into(), mode("redis", Some(&url), &both[1..], n).await);
    drop(redis);

    let out = json!({
        "what": "End-to-end HTTP benchmark of the Rust rusetect-server (release build) on loopback, keep-alive HTTP/1.1",
        "scope": "One container: load generator, server and redis-server share the same CPUs. Not the Vercel deployment.",
        "generated_by": "crates/rusetect-server/examples/e2e_bench.rs",
        "generated_at": chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        "git_head": cmd_out("git", &["describe", "--always", "--dirty"]),
        "requests_per_level": n, "warmup_requests": WARMUP, "concurrency_levels": LEVELS,
        "workload": "400 payers, 300 payees, timestamps one minute apart; webhook events HMAC signed, unique event_id",
        "notes": ["scoring holds the server's one state mutex, so scoring is serial; HTTP and JSON work around it runs in parallel",
                  "redis mode runs only the webhook: /v2/payment keeps state in process memory in both modes",
                  "memory-mode webhook runs the full /v2/assess path; redis-mode webhook runs the bank-scale path with account state in Redis"],
        "machine": machine(),
        "results": results,
    });
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../results/e2e_bench.json");
    std::fs::write(&path, serde_json::to_string_pretty(&out).unwrap() + "\n").unwrap();
    eprintln!("wrote {}", path.display());
}

/// S1 protocol (results/e2e_bench_s1_protocol.json): 5 levels, 20,000 requests each after a 1,000 request warm-up, 3
/// repetitions on fresh processes, a correctness gate after every Redis level, medians next to every repetition.
mod s1 {
    use super::*;

    const LEVELS: [usize; 5] = [1, 8, 32, 64, 128];
    const N: usize = 20_000;
    const WARM: usize = 1_000;
    const REPS: usize = 3;
    const SAMPLES: usize = 100;
    const WEBHOOK: &str = "POST /api/v2/events/payment";
    const PAYMENT: &str = "POST /api/v2/payment";

    fn int(con: &mut redis::Connection, c: &str, k: &str) -> i64 {
        redis::cmd(c).arg(k).query::<Option<i64>>(con).unwrap().unwrap_or(0)
    }

    /// One repetition of the Redis-mode webhook on a fresh redis-server and server, gate checked after each level.
    async fn redis_rep(rep: usize) -> Vec<Value> {
        redis_rep_until(rep, false).await
    }

    /// As `redis_rep`; with `stop`, a level with p99 above STOP_P99_MS or more than 1% errors ends the repetition (8ap).
    async fn redis_rep_until(rep: usize, stop: bool) -> Vec<Value> {
        let (_redis, url) = redis_server().await;
        let (_srv, port) = server(Some(&url)).await;
        let mut con = redis::Client::open(url.as_str()).unwrap().get_connection().unwrap();
        let (ledger, seq) = ("bench:live:ledger", "bench:live:seq");
        let (mut next, mut rows) = (0, vec![]);
        for c in LEVELS {
            let run = format!("redis-r{rep}-c{c}");
            eprintln!("redis rep {rep} webhook concurrency {c}");
            level(port, true, c, WARM, &mut next, &format!("{run}-warm")).await;
            // the ledger list is capped at LEDGER_MAX: start each level from an empty list so growth is exact
            redis::cmd("DEL").arg(ledger).query::<()>(&mut con).unwrap();
            let s0 = int(&mut con, "GET", seq);
            let (mut row, kept) = level_sampled(port, true, c, N, &mut next, &run, N / SAMPLES).await;
            let (grew, advanced) = (int(&mut con, "LLEN", ledger), int(&mut con, "GET", seq) - s0);
            let mut r = BufReader::new(TcpStream::connect(("127.0.0.1", port)).await.unwrap());
            let mut bad = 0;
            for (i, body) in &kept {
                match exchange(&mut r, &request(port, true, *i, &run)).await {
                    Ok((200, true, b)) if &b == body => {}
                    _ => bad += 1,
                }
            }
            let replay_growth = int(&mut con, "LLEN", ledger) - grew;
            let green = row["errors"] == 0 && grew == N as i64 && advanced == N as i64 && kept.len() == SAMPLES && bad == 0 && replay_growth == 0;
            row["gate"] = json!({"green": green, "ledger_growth": grew, "seq_advance": advanced, "replays_sent": kept.len(),
                                 "replays_not_identical": bad, "ledger_growth_on_replay": replay_growth});
            eprintln!("  {} rps, p99 {} ms, gate {}", row["throughput_rps"], row["latency_ms"]["p99"], if green { "green" } else { "RED" });
            let over = row["latency_ms"]["p99"].as_f64().unwrap_or(f64::MAX) > STOP_P99_MS || row["errors"].as_u64().unwrap_or(0) * 100 > N as u64;
            rows.push(row);
            if stop && over {
                eprintln!("  stop rule: the remaining levels of this repetition are not run");
                break;
            }
        }
        rows
    }

    /// 8ap: a level whose p99 is above this (ms), or with more than 1% errors, ends its repetition
    const STOP_P99_MS: f64 = 1000.0;
    /// 8ap primary metric: the best level median throughput whose median p99 is under this (ms)
    const P99_BOUND_MS: f64 = 25.0;
    const SCALE_REPS: usize = 3;

    /// `e2e_bench scale <variant> <rep>` (docs/decision.md 8ap): one Redis webhook repetition on fresh processes, against
    /// the rusetect-server next to this binary's directory, added to results/scale_<variant>.json; once `main` and
    /// `candidate` both hold every repetition, results/scale_compare.json applies the pre-registered rule.
    pub async fn scale(variant: &str, rep: usize) {
        use sha2::Digest;
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../results");
        let path = root.join(format!("scale_{variant}.json"));
        let mut out: Value = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_else(|| json!({
            "what": format!("8ap: the signed Redis webhook on one node, variant {variant} (protocol: docs/decision.md 8ap)"),
            "generated_by": "crates/rusetect-server/examples/e2e_bench.rs (e2e_bench scale <variant> <rep>)",
            "scope": "One shared 4-CPU container: load generator, server and redis-server share the same cores, over loopback. Not the Vercel deployment and not a production capacity figure.",
            "design": {"levels": LEVELS, "requests_per_level": N, "warmup_per_level": WARM, "replay_samples": SAMPLES, "stop_p99_ms": STOP_P99_MS,
                       "p99_bound_ms": P99_BOUND_MS, "workload": "400 payers, 300 payees, unique HMAC-signed event ids (as S1 and F1)"},
            "repetitions": {}}));
        let exe = std::env::current_exe().unwrap();
        let bin = exe.parent().unwrap().parent().unwrap().join("rusetect-server");
        let rows = redis_rep_until(rep, true).await;
        out["repetitions"][rep.to_string()] = json!({"generated_at": chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            "server_revision": std::env::var("RUSETECT_BENCH_SERVER_REVISION").ok(), "harness_git_head": cmd_out("git", &["describe", "--always", "--dirty"]),
            "server_sha256": std::fs::read(&bin).ok().map(|b| hex::encode(sha2::Sha256::digest(b))),
            "harness_sha256": std::fs::read(&exe).ok().map(|b| hex::encode(sha2::Sha256::digest(b))),
            "rusetect_live_pool": std::env::var("RUSETECT_LIVE_POOL").ok(), "machine": machine(), "levels": rows});
        out["summary"] = scale_summary(&out["repetitions"]);
        std::fs::write(&path, serde_json::to_string_pretty(&out).unwrap() + "\n").unwrap();
        eprintln!("wrote {}", path.display());
        let read = |v: &str| std::fs::read_to_string(root.join(format!("scale_{v}.json"))).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok());
        if let (Some(b), Some(c)) = (read("main"), read("candidate")) {
            let full = |v: &Value| v["repetitions"].as_object().is_some_and(|r| r.len() == SCALE_REPS);
            if full(&b) && full(&c) {
                let (pb, pc) = (&b["summary"]["primary"], &c["summary"]["primary"]);
                let (rb, rc) = (pb["throughput_rps"].as_f64(), pc["throughput_rps"].as_f64());
                let gates = c["summary"]["gate_green"] == true;
                let change = rb.zip(rc).map(|(b, c)| (c / b - 1.0) * 100.0);
                let verdict = match change {
                    _ if !gates => "fail: a candidate correctness gate is red",
                    Some(x) if x >= 10.0 => "improvement",
                    Some(x) if x <= -10.0 => "regression",
                    Some(_) => "no material change",
                    None => "not decidable: no level meets the p99 bound in one variant",
                };
                let cmp = json!({"what": "8ap: main against candidate, rule of docs/decision.md 8ap", "main": pb, "candidate": pc,
                                 "primary_change_pct": change.map(|x| (x * 100.0).round() / 100.0), "candidate_gates_green": gates, "verdict": verdict});
                std::fs::write(root.join("scale_compare.json"), serde_json::to_string_pretty(&cmp).unwrap() + "\n").unwrap();
                eprintln!("8ap verdict: {verdict} ({change:?} %)");
            }
        }
    }

    /// Per level: median throughput and latency over the repetitions that ran it (a level is complete only when every
    /// repetition ran it); the primary metric is the best complete, gate-green level median under P99_BOUND_MS.
    fn scale_summary(reps: &Value) -> Value {
        let reps: Vec<&Value> = reps.as_object().map(|m| m.values().collect()).unwrap_or_default();
        let levels: Vec<Value> = LEVELS.iter().map(|&c| {
            let rows: Vec<&Value> = reps.iter().filter_map(|r| r["levels"].as_array()?.iter().find(|l| l["concurrency"] == c)).collect();
            let m = |f: &dyn Fn(&Value) -> f64| if rows.is_empty() { Value::Null } else { json!(median(&rows.iter().map(|r| f(r)).collect::<Vec<_>>())) };
            let lat = |k: &'static str| move |r: &Value| r["latency_ms"][k].as_f64().unwrap();
            json!({"concurrency": c, "repetitions_run": rows.len(), "complete": !rows.is_empty() && rows.len() == reps.len(),
                   "gate_green": rows.iter().all(|r| r["gate"]["green"] == true),
                   "throughput_rps": m(&|r| r["throughput_rps"].as_f64().unwrap()), "p50": m(&lat("p50")), "p95": m(&lat("p95")),
                   "p99": m(&lat("p99")), "max": m(&lat("max")), "throughput_by_repetition": rows.iter().map(|r| r["throughput_rps"].clone()).collect::<Vec<_>>()})
        }).collect();
        let primary = levels.iter().filter(|l| l["complete"] == true && l["gate_green"] == true && l["p99"].as_f64().is_some_and(|p| p < P99_BOUND_MS))
            .max_by(|a, b| a["throughput_rps"].as_f64().unwrap().total_cmp(&b["throughput_rps"].as_f64().unwrap()));
        json!({"repetitions": reps.len(), "gate_green": levels.iter().all(|l| l["gate_green"] == true),
               "primary": primary.map(|l| json!({"concurrency": l["concurrency"], "throughput_rps": l["throughput_rps"], "p99": l["p99"]})).unwrap_or(Value::Null),
               "levels": levels})
    }

    /// One repetition of both memory-mode endpoints on a fresh server.
    async fn memory_rep(rep: usize) -> Vec<(&'static str, Vec<Value>)> {
        let (_srv, port) = server(None).await;
        let mut next = 0;
        let mut out = vec![];
        for (ep, webhook) in [(PAYMENT, false), (WEBHOOK, true)] {
            let mut rows = vec![];
            for c in LEVELS {
                eprintln!("memory rep {rep} {ep} concurrency {c}");
                level(port, webhook, c, WARM, &mut next, &format!("memory-r{rep}-c{c}-warm")).await;
                rows.push(level(port, webhook, c, N, &mut next, &format!("memory-r{rep}-c{c}")).await);
            }
            out.push((ep, rows));
        }
        out
    }

    fn median(v: &[f64]) -> f64 {
        let mut v = v.to_vec();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    }

    /// [rep][level] rows -> per level {concurrency, median, repetitions}.
    fn summarise(reps: &[Vec<Value>]) -> Value {
        let levels: Vec<Value> = LEVELS.iter().enumerate().map(|(i, c)| {
            let rows: Vec<&Value> = reps.iter().map(|r| &r[i]).collect();
            let m = |f: &dyn Fn(&Value) -> f64| median(&rows.iter().map(|r| f(r)).collect::<Vec<_>>());
            let lat = |k: &'static str| move |r: &Value| r["latency_ms"][k].as_f64().unwrap();
            let gate = rows.iter().all(|r| r.get("gate").is_none_or(|g| g["green"] == true));
            json!({"concurrency": c, "median": {"throughput_rps": m(&|r| r["throughput_rps"].as_f64().unwrap()),
                   "p50": m(&lat("p50")), "p95": m(&lat("p95")), "p99": m(&lat("p99")), "max": m(&lat("max")),
                   "errors": rows.iter().map(|r| r["errors"].as_u64().unwrap()).sum::<u64>()},
                   "gate_green": gate, "repetitions": rows})
        }).collect();
        json!(levels)
    }

    async fn phase() -> Value {
        let mut redis = vec![];
        let (mut mp, mut mw) = (vec![], vec![]);
        for rep in 0..REPS {
            redis.push(redis_rep(rep).await);
            let m = memory_rep(rep).await;
            mp.push(m[0].1.clone());
            mw.push(m[1].1.clone());
        }
        json!({"generated_at": chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
               "git_head": cmd_out("git", &["describe", "--always", "--dirty"]),
               "rusetect_live_pool": std::env::var("RUSETECT_LIVE_POOL").ok(), "machine": machine(),
               "results": {"redis": {WEBHOOK: summarise(&redis)}, "memory": {PAYMENT: summarise(&mp), WEBHOOK: summarise(&mw)}}})
    }

    fn at<'a>(p: &'a Value, mode: &str, ep: &str, c: usize) -> &'a Value {
        p["results"][mode][ep].as_array().unwrap().iter().find(|l| l["concurrency"] == c).unwrap()
    }

    /// The pre-registered rule, applied to the `after` phase against the baseline.
    fn verdict(base: &Value, after: &Value) -> Value {
        let gate = LEVELS.iter().all(|&c| at(after, "redis", WEBHOOK, c)["gate_green"] == true);
        let m32 = &at(after, "redis", WEBHOOK, 32)["median"];
        let (r32, l32) = (m32["throughput_rps"].as_f64().unwrap(), m32["p99"].as_f64().unwrap());
        let stretch: Vec<usize> = LEVELS.iter().copied().filter(|&c| {
            let l = at(after, "redis", WEBHOOK, c);
            l["median"]["throughput_rps"].as_f64().unwrap() >= 5000.0 && l["median"]["p99"].as_f64().unwrap() < 50.0
        }).collect();
        let mut regressions = vec![];
        for ep in [PAYMENT, WEBHOOK] {
            for c in LEVELS {
                let (b, a) = (at(base, "memory", ep, c)["median"]["throughput_rps"].as_f64().unwrap(), at(after, "memory", ep, c)["median"]["throughput_rps"].as_f64().unwrap());
                if a < 0.9 * b {
                    regressions.push(json!({"endpoint": ep, "concurrency": c, "baseline_rps": b, "after_rps": a}));
                }
            }
        }
        let primary = gate && r32 >= 3000.0 && l32 < 50.0;
        json!({"gate_green": gate, "primary": {"met": primary, "c32_median_rps": r32, "c32_median_p99_ms": l32},
               "stretch": {"met": gate && !stretch.is_empty(), "levels": stretch},
               "no_regression": {"met": regressions.is_empty(), "regressions": regressions},
               "pass": primary && regressions.is_empty()})
    }

    pub async fn main(which: &str) {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../results");
        let protocol: Value = serde_json::from_str(&std::fs::read_to_string(root.join("e2e_bench_s1_protocol.json")).unwrap()).unwrap();
        let path = root.join("e2e_bench_s1.json");
        let mut out = match which {
            "baseline" => json!({"what": "S1: the signed Redis webhook on one node, pre-registered protocol (results/e2e_bench_s1_protocol.json)",
                                 "generated_by": "crates/rusetect-server/examples/e2e_bench.rs (e2e_bench s1 baseline|after)",
                                 "scope": "One container: load generator, server and redis-server share the same CPUs. Not the Vercel deployment."}),
            "after" => serde_json::from_str(&std::fs::read_to_string(&path).expect("run `e2e_bench s1 baseline` first")).unwrap(),
            _ => panic!("s1 baseline|after"),
        };
        out["protocol"] = protocol;
        let p = phase().await;
        if which == "baseline" {
            out["baseline"] = p;
        } else {
            out["pass"] = verdict(&out["baseline"], &p);
            out["repetitions"] = p;
        }
        std::fs::write(&path, serde_json::to_string_pretty(&out).unwrap() + "\n").unwrap();
        eprintln!("wrote {}", path.display());
    }
}

/// F1/F2 protocol (docs/SCALING.md section 3d): the Redis webhook and the read endpoints at concurrency 1 to 128, 3
/// repetitions on fresh processes, with CPU of the server, Redis and this load generator, and server memory.
/// `e2e_bench f1` writes results/perf_f1.json (key `runs.<label>`, label from the second argument, default `f1`);
/// `e2e_bench f1 hold <secs>` only starts Redis and a loaded server on the F1 ports and waits (for a profiler).
mod f1 {
    use super::*;

    const LEVELS: [usize; 5] = [1, 8, 32, 64, 128];
    const N_WEBHOOK: usize = 20_000;
    const WARM_WEBHOOK: usize = 1_000;
    const N_READ: usize = 200;
    const WARM_READ: usize = 20;
    const REPS: usize = 3;
    const SERVER_PORT: u16 = 8210;
    const REDIS_PORT: u16 = 8250;
    /// the harness kills the server above this resident size (the container has 16 GB, shared with other sessions)
    const RSS_CAP_MB: u64 = 6_000;
    const WEBHOOK: &str = "POST /api/v2/events/payment";
    const READS: [&str; 4] = ["GET /api/v2/dashboard", "GET /api/v2/transactions", "GET /api/v2/graph", "GET /api/v2/alerts"];

    type Make = Arc<dyn Fn(usize) -> Vec<u8> + Send + Sync>;

    fn ticks() -> f64 {
        cmd_out("getconf", &["CLK_TCK"]).and_then(|s| s.parse().ok()).unwrap_or(100.0)
    }

    /// CPU seconds (user + system) used so far by a process.
    fn cpu_s(pid: &str) -> f64 {
        let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        let f: Vec<&str> = s.rsplit_once(')').map(|(_, r)| r.split_whitespace().collect()).unwrap_or_default();
        (f.get(11).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0) + f.get(12).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0)) / ticks()
    }

    fn status_kb(pid: &str, key: &str) -> u64 {
        std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default().lines()
            .find(|l| l.starts_with(key)).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse().ok()).unwrap_or(0)
    }

    fn redis_used(con: &mut redis::Connection) -> u64 {
        let info: String = redis::cmd("INFO").arg("memory").query(con).unwrap_or_default();
        info.lines().find_map(|l| l.strip_prefix("used_memory:")).and_then(|v| v.trim().parse().ok()).unwrap_or(0)
    }

    fn get(port: u16, path: &str) -> Vec<u8> {
        format!("GET {path} HTTP/1.1\r\nhost: 127.0.0.1:{port}\r\n\r\n").into_bytes()
    }

    /// `n` requests made by `mk(i)` from `conc` keep-alive connections, with the CPU and memory figures of the protocol.
    async fn run(port: u16, conc: usize, n: usize, mk: Make, pids: (&str, &str), con: &mut redis::Connection) -> Value {
        let (srv, red) = pids;
        let idx = Arc::new(AtomicUsize::new(0));
        let (c0, r0, l0) = (cpu_s(srv), cpu_s(red), cpu_s("self"));
        let t0 = Instant::now();
        // memory guard: past RSS_CAP_MB the server is killed and the level marked aborted, so the shared container's
        // memory limit is never reached (the kernel would pick a victim, possibly another session's process)
        let killed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let watch = {
            let (srv, killed, idx) = (srv.to_string(), killed.clone(), idx.clone());
            tokio::spawn(async move {
                loop {
                    let rss = status_kb(&srv, "VmRSS:") / 1024;
                    if rss > RSS_CAP_MB {
                        let _ = Command::new("kill").args(["-9", &srv]).status();
                        killed.store(true, Ordering::Relaxed);
                        idx.store(usize::MAX / 2, Ordering::Relaxed);
                        return rss;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            })
        };
        let mut tasks = vec![];
        for _ in 0..conc {
            let (idx, mk, killed) = (idx.clone(), mk.clone(), killed.clone());
            tasks.push(tokio::spawn(async move {
                let mut r = BufReader::new(TcpStream::connect(("127.0.0.1", port)).await.unwrap());
                r.get_mut().set_nodelay(true).unwrap();
                let (mut lat, mut errors) = (vec![], 0usize);
                loop {
                    let k = idx.fetch_add(1, Ordering::Relaxed);
                    if k >= n {
                        break;
                    }
                    let req = mk(k);
                    let t = Instant::now();
                    match exchange(&mut r, &req).await {
                        Ok((200, _, _)) => lat.push(t.elapsed().as_secs_f64() * 1000.0),
                        Ok(_) => errors += 1,
                        Err(_) => {
                            errors += 1;
                            if killed.load(Ordering::Relaxed) {
                                break;
                            }
                            r = BufReader::new(TcpStream::connect(("127.0.0.1", port)).await.unwrap());
                        }
                    }
                }
                (lat, errors)
            }));
        }
        let (mut lat, mut errors) = (vec![], 0);
        for t in tasks {
            let (l, e) = t.await.unwrap();
            lat.extend(l);
            errors += e;
        }
        let wall = t0.elapsed().as_secs_f64();
        watch.abort();
        if killed.load(Ordering::Relaxed) {
            eprintln!("  server killed: resident memory above {RSS_CAP_MB} MB");
            return json!({"concurrency": conc, "requests": n, "aborted": format!("server resident memory passed {RSS_CAP_MB} MB after {wall:.1} s with {} responses; server killed by the harness", lat.len())});
        }
        lat.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let pct = |p: f64| if lat.is_empty() { 0.0 } else { lat[((p / 100.0 * lat.len() as f64).ceil() as usize).clamp(1, lat.len()) - 1] };
        let r2 = |x: f64| (x * 100.0).round() / 100.0;
        let r3 = |x: f64| (x * 1000.0).round() / 1000.0;
        json!({"concurrency": conc, "requests": n, "ok": lat.len(), "errors": errors, "error_rate": r3(errors as f64 / n.max(1) as f64),
               "wall_s": r2(wall), "throughput_rps": r2(lat.len() as f64 / wall),
               "latency_ms": {"p50": r2(pct(50.0)), "p95": r2(pct(95.0)), "p99": r2(pct(99.0)), "max": r2(pct(100.0))},
               "cpu_cores": {"server": r2((cpu_s(srv) - c0) / wall), "redis": r2((cpu_s(red) - r0) / wall), "load_generator": r2((cpu_s("self") - l0) / wall)},
               "server_rss_mb": r2(status_kb(srv, "VmRSS:") as f64 / 1024.0), "server_peak_rss_mb": r2(status_kb(srv, "VmHWM:") as f64 / 1024.0),
               "redis_used_memory_mb": r2(redis_used(con) as f64 / 1048576.0)})
    }

    fn webhook(port: u16, run: String, offset: usize) -> Make {
        Arc::new(move |k| request(port, true, offset + k, &run))
    }

    fn read(port: u16, ep: &str, account: String) -> Make {
        let ep = ep.to_string();
        Arc::new(move |k| {
            let off = (k % 20) * 50;
            get(port, &match ep.as_str() {
                "GET /api/v2/dashboard" => "/api/v2/dashboard".to_string(),
                "GET /api/v2/transactions" => format!("/api/v2/transactions?limit=50&offset={off}"),
                "GET /api/v2/graph" => format!("/api/v2/graph?account={account}&depth=2"),
                _ => format!("/api/v2/alerts?limit=50&offset={off}"),
            })
        })
    }

    /// Start Redis and the server on the F1 ports. Returns them, a Redis connection and the two pids.
    async fn start() -> (Kill, Kill, redis::Connection, String, String) {
        for p in [REDIS_PORT, SERVER_PORT] {
            // a leftover process on a fixed port would answer in place of the fresh one
            drop(std::net::TcpListener::bind(("127.0.0.1", p)).unwrap_or_else(|_| panic!("port {p} is in use")));
        }
        let (redis, url) = redis_server_on(REDIS_PORT).await;
        let (srv, _) = server_on(SERVER_PORT, Some(&url)).await;
        let con = redis::Client::open(url.as_str()).unwrap().get_connection().unwrap();
        let (sp, rp) = (srv.0.id().to_string(), redis.0.id().to_string());
        (redis, srv, con, sp, rp)
    }

    /// One fixed-concurrency sequence on fresh processes. Keep the same payment generator and
    /// timing as F1, while recording the durable store size at each measurement boundary.
    async fn growth() {
        const CONCURRENCY: usize = 32;
        const STAGES: usize = 7;
        let (_redis, _server, mut con, sp, rp) = start().await;
        let warmup = run(SERVER_PORT, 4, WARM_WEBHOOK, webhook(SERVER_PORT, "growth".into(), 0), (&sp, &rp), &mut con).await;
        let mut stages = Vec::with_capacity(STAGES);
        for stage in 0..STAGES {
            let offset = WARM_WEBHOOK + stage * N_WEBHOOK;
            let (events_before, alerts_before) = growth_counts(&mut con);
            let mut row = run(SERVER_PORT, CONCURRENCY, N_WEBHOOK, webhook(SERVER_PORT, "growth".into(), offset), (&sp, &rp), &mut con).await;
            let (events_after, alerts_after) = growth_counts(&mut con);
            row["stage"] = json!(stage + 1);
            row["events_before"] = json!(events_before);
            row["events_after"] = json!(events_after);
            row["durable_alerts_before"] = json!(alerts_before);
            row["durable_alerts_after"] = json!(alerts_after);
            row["check"] = json!(row["errors"] == 0 && events_before == offset && events_after == offset + N_WEBHOOK);
            eprintln!("growth stage {}: {} to {} durable alerts, {} rps, p99 {} ms, check {}",
                stage + 1, alerts_before, alerts_after, row["throughput_rps"], row["latency_ms"]["p99"], row["check"]);
            stages.push(row);
        }
        let green = warmup["errors"] == 0 && stages.iter().all(|row| row["check"] == true);
        println!("{}", json!({
            "machine": machine(),
            "variant": std::env::var("RUSETECT_BENCH_VARIANT").ok(),
            "server_revision": std::env::var("RUSETECT_BENCH_SERVER_REVISION").ok(),
            "warmup": warmup,
            "stages": stages,
            "check": green,
        }));
    }

    fn growth_counts(con: &mut redis::Connection) -> (usize, usize) {
        let seq = redis::cmd("GET").arg("bench:live:seq").query::<Option<usize>>(con).unwrap().unwrap_or(0);
        let alerts = redis::cmd("HLEN").arg("bench:live:alerts").query::<usize>(con).unwrap();
        (seq, alerts)
    }

    async fn rep(rep: usize) -> (Vec<Value>, Vec<(&'static str, Vec<Value>)>) {
        let (_redis, _srv, mut con, sp, rp) = start().await;
        let port = SERVER_PORT;
        let mut next = 0;
        let mut hooks = vec![];
        for c in LEVELS {
            let tag = format!("f1-r{rep}-c{c}");
            eprintln!("f1 rep {rep} webhook concurrency {c}");
            run(port, 4.min(c), WARM_WEBHOOK, webhook(port, format!("{tag}-warm"), next), (&sp, &rp), &mut con).await;
            next += WARM_WEBHOOK;
            let s0 = redis::cmd("GET").arg("bench:live:seq").query::<Option<i64>>(&mut con).unwrap().unwrap_or(0);
            let mut row = run(port, c, N_WEBHOOK, webhook(port, tag, next), (&sp, &rp), &mut con).await;
            next += N_WEBHOOK;
            let adv = redis::cmd("GET").arg("bench:live:seq").query::<Option<i64>>(&mut con).unwrap().unwrap_or(0) - s0;
            row["check"] = json!({"green": row["errors"] == 0 && adv == N_WEBHOOK as i64, "seq_advance": adv});
            eprintln!("  {} rps p99 {} ms cpu {}", row["throughput_rps"], row["latency_ms"]["p99"], row["cpu_cores"]);
            hooks.push(row);
        }
        // an account from the newest stored payment, for the graph
        let mut r = BufReader::new(TcpStream::connect(("127.0.0.1", port)).await.unwrap());
        let (_, _, b) = exchange(&mut r, &get(port, "/api/v2/transactions?limit=1")).await.unwrap();
        let v: Value = serde_json::from_slice(&b).unwrap();
        let account = v["transactions"][0]["from_account"].as_str().unwrap().to_string();
        let mut reads = vec![];
        let mut dead = false;
        for ep in READS {
            let mut rows = vec![];
            for c in LEVELS {
                if dead {
                    rows.push(json!({"concurrency": c, "skipped": "the server was killed at an earlier level of this repetition"}));
                    continue;
                }
                eprintln!("f1 rep {rep} {ep} concurrency {c}");
                run(port, 4.min(c), WARM_READ, read(port, ep, account.clone()), (&sp, &rp), &mut con).await;
                let row = run(port, c, N_READ, read(port, ep, account.clone()), (&sp, &rp), &mut con).await;
                dead = row.get("aborted").is_some();
                eprintln!("  {} rps p99 {} ms errors {}", row["throughput_rps"], row["latency_ms"]["p99"], row["errors"]);
                rows.push(row);
            }
            reads.push((ep, rows));
        }
        (hooks, reads)
    }

    fn median(v: &[f64]) -> f64 {
        let mut v = v.to_vec();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    }

    /// [rep][level] rows -> per level {concurrency, median of every figure, repetitions}.
    fn summarise(reps: &[Vec<Value>]) -> Value {
        json!(LEVELS.iter().enumerate().map(|(i, c)| {
            let all: Vec<&Value> = reps.iter().map(|r| &r[i]).collect();
            let rows: Vec<&Value> = all.iter().copied().filter(|r| r.get("aborted").is_none() && r.get("skipped").is_none()).collect();
            if rows.is_empty() {
                return json!({"concurrency": c, "median": null, "completed_repetitions": 0, "repetitions": all});
            }
            let m = |f: &dyn Fn(&Value) -> f64| (median(&rows.iter().map(|r| f(r)).collect::<Vec<_>>()) * 100.0).round() / 100.0;
            let num = |p: &'static str| move |r: &Value| r.pointer(p).and_then(Value::as_f64).unwrap_or(0.0);
            json!({"concurrency": c, "median": {
                "throughput_rps": m(&num("/throughput_rps")), "p50": m(&num("/latency_ms/p50")), "p95": m(&num("/latency_ms/p95")),
                "p99": m(&num("/latency_ms/p99")), "max": m(&num("/latency_ms/max")), "error_rate": m(&num("/error_rate")),
                "cpu_server": m(&num("/cpu_cores/server")), "cpu_redis": m(&num("/cpu_cores/redis")), "cpu_load_generator": m(&num("/cpu_cores/load_generator")),
                "server_rss_mb": m(&num("/server_rss_mb")), "server_peak_rss_mb": m(&num("/server_peak_rss_mb")), "redis_used_memory_mb": m(&num("/redis_used_memory_mb"))},
                "throughput_by_repetition": rows.iter().map(|r| r["throughput_rps"].clone()).collect::<Vec<_>>(),
                "check_green": rows.iter().all(|r| r.get("check").is_none_or(|g| g["green"] == true)),
                "completed_repetitions": rows.len(), "repetitions": all})
        }).collect::<Vec<_>>())
    }

    pub async fn main(arg: Option<&str>) {
        if arg == Some("growth") {
            growth().await;
            return;
        }
        if arg == Some("hold") {
            let secs: u64 = std::env::args().nth(3).and_then(|a| a.parse().ok()).unwrap_or(600);
            let (_redis, _srv, _con, sp, rp) = start().await;
            eprintln!("server pid {sp} on {SERVER_PORT}, redis pid {rp} on {REDIS_PORT}; holding {secs} s");
            tokio::time::sleep(Duration::from_secs(secs)).await;
            return;
        }
        if arg == Some("load") {
            // one webhook level against a server already started by `hold`, for the profiler: f1 load <concurrency> <n>
            let c: usize = std::env::args().nth(3).and_then(|a| a.parse().ok()).unwrap_or(32);
            let n: usize = std::env::args().nth(4).and_then(|a| a.parse().ok()).unwrap_or(40_000);
            let mut con = redis::Client::open(format!("redis://127.0.0.1:{REDIS_PORT}/0").as_str()).unwrap().get_connection().unwrap();
            let sp = cmd_out("pgrep", &["-f", &format!("rusetect-server --host 127.0.0.1 --port {SERVER_PORT}")]).unwrap_or_default();
            let rp = cmd_out("pgrep", &["-f", &format!("redis-server \\*:{REDIS_PORT}")]).unwrap_or_default();
            let tag = format!("load-{}-c{c}", chrono::Utc::now().timestamp_millis());
            let row = run(SERVER_PORT, c, n, webhook(SERVER_PORT, tag, 0), (sp.lines().next().unwrap_or(""), rp.lines().next().unwrap_or("")), &mut con).await;
            println!("{}", serde_json::to_string(&row).unwrap());
            return;
        }
        let label = arg.unwrap_or("f1").to_string();
        let (mut hooks, mut reads): (Vec<Vec<Value>>, Vec<Vec<Vec<Value>>>) = (vec![], vec![vec![]; READS.len()]);
        for r in 0..REPS {
            let (h, rd) = rep(r).await;
            hooks.push(h);
            for (i, (_, rows)) in rd.into_iter().enumerate() {
                reads[i].push(rows);
            }
        }
        let mut res = serde_json::Map::new();
        res.insert(WEBHOOK.into(), summarise(&hooks));
        for (i, ep) in READS.iter().enumerate() {
            res.insert(ep.to_string(), summarise(&reads[i]));
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../results/perf_f1.json");
        let mut out: Value = std::fs::read_to_string(&root).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_else(|| json!({
            "what": "F1/F2: the signed Redis webhook and the workspace read endpoints at concurrency 1 to 128 (protocol: docs/SCALING.md section 3d)",
            "generated_by": "crates/rusetect-server/examples/e2e_bench.rs (e2e_bench f1 [label])",
            "scope": "One shared 4-CPU container: load generator, server and redis-server compete for the same cores. Loopback, no TLS, no cold start. Not the Vercel deployment and not a production capacity figure.",
            "runs": {}}));
        out["runs"][&label] = json!({"generated_at": chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            "git_head": cmd_out("git", &["describe", "--always", "--dirty"]), "machine": machine(),
            "rusetect_live_pool": std::env::var("RUSETECT_LIVE_POOL").ok(),
            "design": {"levels": LEVELS, "repetitions": REPS, "webhook_requests_per_level": N_WEBHOOK, "webhook_warmup": WARM_WEBHOOK,
                       "read_requests_per_level": N_READ, "read_warmup": WARM_READ, "server_port": SERVER_PORT, "redis_port": REDIS_PORT,
                       "read_queries": {"GET /api/v2/transactions": "limit=50, offset cycling 0..950 by 50", "GET /api/v2/alerts": "limit=50, offset cycling 0..950 by 50",
                                        "GET /api/v2/graph": "depth=2, account of the newest stored payment", "GET /api/v2/dashboard": "no query"},
                       "cpu": "CPU seconds over wall seconds from /proc/<pid>/stat; 1.0 is one of the 4 logical CPUs"},
            "results": res});
        std::fs::write(&root, serde_json::to_string_pretty(&out).unwrap() + "\n").unwrap();
        eprintln!("wrote {}", root.display());
    }
}
