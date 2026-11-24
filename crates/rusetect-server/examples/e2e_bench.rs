//! End-to-end benchmark of the real HTTP server: starts the release `rusetect-server` binary on loopback, drives
//! POST /api/v2/payment and the signed POST /api/v2/events/payment webhook over keep-alive HTTP/1.1 at a few fixed
//! concurrencies, in memory mode and with a local redis-server, and writes results/e2e_bench.json.
//!
//!   cargo build --release -p rusetect-server --bins --examples
//!   target/release/examples/e2e_bench [requests per level, default 5000]
//!   target/release/examples/e2e_bench s1 baseline|after   (S1 protocol, results/e2e_bench_s1_protocol.json)
//!   target/release/examples/e2e_bench read   (ReviewDesk read endpoints, results/read_bench.json; see mod readbench)
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
    if std::env::args().nth(1).as_deref() == Some("read") {
        return readbench::main().await;
    }
    if std::env::args().nth(1).as_deref() == Some("s1") {
        return s1::main(&std::env::args().nth(2).expect("s1 baseline|after")).await;
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
            rows.push(row);
        }
        rows
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

/// ReviewDesk read-endpoint benchmark (docs/decision.md 8ar, protocol by Codex): stage 200 to 50,000 durable alerts
/// through the signed webhook, then offer 60 webhooks/s and 4 analyst reads/s open loop for 15 s warm-up + 60 s.
/// `e2e_bench read` with RUSETECT_BENCH_BIN_MAIN (and optionally RUSETECT_BENCH_BIN_BASELINE) set to server binaries;
/// writes results/read_bench.json and results/read_bench_offers.csv.gz.
mod readbench {
    use super::*;
    use std::io::Write as _;
    use tokio::sync::Mutex;

    const STAGES: [usize; 4] = [200, 2_000, 10_000, 50_000];
    const ORDER: [&str; 8] = ["main", "baseline", "baseline", "main", "main", "baseline", "baseline", "main"];
    const WARM_S: u64 = 15;
    const WINDOW_S: u64 = 60;
    const HOOK_RATE: u64 = 60;
    const HOOK_LIMIT: usize = 32;
    const READ_LIMIT: usize = 4;
    const STAGE_CONC: usize = 32;
    const DRAIN_LIMIT_S: u64 = 120;
    const EVIDENCE_IDS: usize = 75;
    const ANALYST: &str = "bench-analyst-key-0123456789";
    const VIEWER: &str = "bench-viewer-key-0123456789";
    const READS: [&str; 4] = ["GET /api/v2/alerts?limit=20", "GET /api/v2/alerts/summary", "GET /api/v2/live", "GET /api/v2/alerts/{id}/evidence"];
    const WEBHOOK: &str = "POST /api/v2/events/payment";
    const CMDS: [&str; 8] = ["hgetall", "hget", "hkeys", "watch", "exec", "eval", "evalsha", "info"];

    fn cpu_s(pid: u32) -> f64 {
        let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        let f: Vec<&str> = s.rsplit_once(')').map(|(_, r)| r.split_whitespace().collect()).unwrap_or_default();
        let t = cmd_out("getconf", &["CLK_TCK"]).and_then(|s| s.parse().ok()).unwrap_or(100.0);
        (f.get(11).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0) + f.get(12).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0)) / t
    }

    fn kb(pid: u32, key: &str) -> u64 {
        std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default().lines()
            .find(|l| l.starts_with(key)).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse().ok()).unwrap_or(0)
    }

    fn sample(srv: u32, red: u32, at: f64) -> Value {
        json!({"at_s": r2(at), "server_cpu_s": r2(cpu_s(srv)), "redis_cpu_s": r2(cpu_s(red)),
               "server_rss_mb": r2(kb(srv, "VmRSS:") as f64 / 1024.0), "redis_rss_mb": r2(kb(red, "VmRSS:") as f64 / 1024.0)})
    }

    fn r2(x: f64) -> f64 {
        (x * 100.0).round() / 100.0
    }

    fn req(method: &str, path: &str, key: Option<&str>, body: Option<&str>) -> Vec<u8> {
        let mut h = String::new();
        if let Some(k) = key {
            h += &format!("x-analyst-key: {k}\r\n");
        }
        if method == "POST" {
            h += &format!("x-nonce: n{}\r\n", chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0));
        }
        let b = body.unwrap_or("");
        let len = if method == "POST" { format!("content-type: application/json\r\ncontent-length: {}\r\n", b.len()) } else { String::new() };
        format!("{method} {path} HTTP/1.1\r\nhost: 127.0.0.1\r\n{h}{len}\r\n{b}").into_bytes()
    }

    async fn conn(port: u16) -> BufReader<TcpStream> {
        let s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        s.set_nodelay(true).unwrap();
        BufReader::new(s)
    }

    async fn once(port: u16, r: Vec<u8>) -> (u16, Vec<u8>) {
        let (s, _, b) = exchange(&mut conn(port).await, &r).await.unwrap_or((0, false, vec![]));
        (s, b)
    }

    fn counts(con: &mut redis::Connection) -> (usize, usize, usize) {
        let seq = redis::cmd("GET").arg("bench:live:seq").query::<Option<usize>>(con).unwrap().unwrap_or(0);
        let alerts = redis::cmd("HLEN").arg("bench:live:alerts").query::<usize>(con).unwrap();
        let ledger = redis::cmd("LLEN").arg("bench:live:ledger").query::<usize>(con).unwrap();
        (seq, alerts, ledger)
    }

    fn commandstats(con: &mut redis::Connection) -> std::collections::BTreeMap<String, u64> {
        let info: String = redis::cmd("INFO").arg("commandstats").query(con).unwrap_or_default();
        info.lines().filter_map(|l| {
            let (name, rest) = l.strip_prefix("cmdstat_")?.split_once(':')?;
            let calls = rest.split(',').find_map(|kv| kv.strip_prefix("calls="))?.parse().ok()?;
            CMDS.contains(&name).then(|| (name.to_string(), calls))
        }).collect()
    }

    /// Signed webhooks `from..from+n` at `conc`; returns the number not answered 200.
    async fn send_hooks(port: u16, run: &str, from: usize, n: usize, conc: usize) -> usize {
        let idx = Arc::new(AtomicUsize::new(from));
        let mut tasks = vec![];
        for _ in 0..conc {
            let (idx, run) = (idx.clone(), run.to_string());
            tasks.push(tokio::spawn(async move {
                let (mut c, mut bad) = (conn(port).await, 0);
                loop {
                    let k = idx.fetch_add(1, Ordering::Relaxed);
                    if k >= from + n {
                        return bad;
                    }
                    if !matches!(exchange(&mut c, &request(port, true, k, &run)).await, Ok((200, _, _))) {
                        bad += 1;
                        c = conn(port).await;
                    }
                }
            }));
        }
        let mut bad = 0;
        for t in tasks {
            bad += t.await.unwrap();
        }
        bad
    }

    struct Offer {
        ep: &'static str,
        sched_ms: f64,
        lag_ms: f64,
        req_ms: f64,
        total_ms: f64,
        status: u16,
        bytes: usize,
        measured: bool,
    }

    fn pct(v: &mut [f64], p: f64) -> f64 {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        if v.is_empty() { 0.0 } else { v[((p / 100.0 * v.len() as f64).ceil() as usize).clamp(1, v.len()) - 1] }
    }

    fn series(mut v: Vec<f64>) -> Value {
        json!({"p50": r2(pct(&mut v, 50.0)), "p95": r2(pct(&mut v, 95.0)), "p99": r2(pct(&mut v, 99.0)), "max": r2(pct(&mut v, 100.0))})
    }

    async fn trial(bin: &str, rev: &str, stage: usize, trial: usize, csv: &mut Vec<String>) -> Value {
        let tag = format!("{rev}-s{stage}-t{trial}");
        let (rport, port) = (free_port(), free_port());
        let dir = std::env::temp_dir().join(format!("read-bench-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let redis = Kill(Command::new("redis-server").args(["--bind", "127.0.0.1", "--port", &rport.to_string(), "--save", "",
            "--appendonly", "yes", "--appendfsync", "everysec", "--dir", dir.to_str().unwrap()]).stdout(Stdio::null()).spawn().unwrap());
        let url = format!("redis://127.0.0.1:{rport}/0");
        let mut con = loop {
            if let Ok(c) = redis::Client::open(url.as_str()).unwrap().get_connection() {
                break c;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        let keys = json!({ANALYST: {"analyst": "bench.analyst", "role": "analyst"}, VIEWER: {"analyst": "bench.viewer", "role": "viewer"}});
        let srv = Kill(Command::new(bin).args(["--host", "127.0.0.1", "--port", &port.to_string()])
            .env_remove("RUSETECT_DEMO").env("RUSETECT_ANALYST_KEYS", keys.to_string())
            .env("RUSETECT_SIGNING_KEY", "YmVuY2gtc2lnbmluZy1zZWVkLTAxMjM0NTY3ODlhYmM=") // synthetic 32-byte seed
            .env("RUSETECT_CONSORTIUM_SECRET", "bench-consortium").env("RUSETECT_HASH_SALT", "bench-salt")
            .env("RUSETECT_RATE_LIMIT", "1000000000").env("RUSETECT_WEBHOOK_SECRET", SECRET)
            .env("REDIS_URL", &url).env("RUSETECT_REDIS_PREFIX", "bench:").env_remove("KV_URL")
            .stdout(Stdio::null()).stderr(Stdio::null()).spawn().expect("start server"));
        let mut healthy = false;
        for _ in 0..600 {
            if let Ok(s) = TcpStream::connect(("127.0.0.1", port)).await {
                if roundtrip(&mut BufReader::new(s), &req("GET", "/api/health", None, None)).await.ok() == Some(200) {
                    healthy = true;
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(healthy, "{bin} did not become healthy");
        let (sp, rp) = (srv.0.id(), redis.0.id());

        // ---- staging through the signed webhook
        let t_stage = Instant::now();
        let (mut next, mut stage_errors) = (0usize, 0usize);
        loop {
            let have = counts(&mut con).1;
            if have >= stage {
                break;
            }
            let n = ((stage - have) * 5).clamp(20, 5_000);
            stage_errors += send_hooks(port, &tag, next, n, STAGE_CONC).await;
            next += n;
        }
        let stage_s = t_stage.elapsed().as_secs_f64();
        let (seq0, alerts0, ledger0) = counts(&mut con);
        let mut ids: Vec<String> = redis::cmd("HKEYS").arg("bench:live:alerts").query(&mut con).unwrap();
        ids.sort_by_key(|s| s.trim_start_matches(|c: char| !c.is_ascii_digit()).parse::<u64>().unwrap_or(u64::MAX));
        let evidence: Vec<String> = ids.iter().take(EVIDENCE_IDS).cloned().collect();
        let reserved = [ids[ids.len() - 1].clone(), ids[ids.len() - 2].clone()];
        assert!(evidence.len() == EVIDENCE_IDS && !evidence.contains(&reserved[0]) && !evidence.contains(&reserved[1]), "too few alerts");

        // ---- checks before
        let check = |held: String| async move {
            let anon = once(port, req("GET", "/api/v2/alerts?limit=1", None, None)).await.0;
            let viewer = once(port, req("POST", &format!("/api/v2/alerts/{held}/review"), Some(VIEWER), Some(r#"{"action":"hold"}"#))).await.0;
            let hold = once(port, req("POST", &format!("/api/v2/alerts/{held}/review"), Some(ANALYST), Some(r#"{"action":"hold","note":"bench"}"#))).await.0;
            (anon, viewer, hold)
        };
        let pack_hash = |id: String| async move {
            let (_, b) = once(port, req("GET", &format!("/api/v2/alerts/{id}/evidence"), Some(ANALYST), None)).await;
            serde_json::from_slice::<Value>(&b).ok().and_then(|v| v["pack_hash"].as_str().map(String::from))
        };
        let before = check(reserved[0].clone()).await;
        let hash_before = pack_hash(reserved[0].clone()).await;
        let seq_before_window = counts(&mut con).0;

        // ---- open-loop window
        let pool = |n: usize| async move {
            let (tx, rx) = tokio::sync::mpsc::channel(n);
            for _ in 0..n {
                tx.send(conn(port).await).await.unwrap();
            }
            (tx, Arc::new(Mutex::new(rx)))
        };
        let (htx, hrx) = pool(HOOK_LIMIT).await;
        let (rtx, rrx) = pool(READ_LIMIT).await;
        let total_s = WARM_S + WINDOW_S;
        let t0 = tokio::time::Instant::now() + Duration::from_millis(300);
        let end = t0 + Duration::from_secs(total_s);
        let mut tasks = vec![];
        let mut plan: Vec<(&'static str, Duration, Vec<u8>, bool)> = vec![];
        for k in 0..(total_s * HOOK_RATE) as usize {
            let at = Duration::from_secs_f64(k as f64 / HOOK_RATE as f64);
            plan.push((WEBHOOK, at, vec![], true));
        }
        for (s, id) in evidence.iter().enumerate().take(total_s as usize) {
            for (j, ep) in READS.iter().enumerate() {
                let path = match j {
                    0 => "/api/v2/alerts?limit=20".to_string(),
                    1 => "/api/v2/alerts/summary".to_string(),
                    2 => "/api/v2/live".to_string(),
                    _ => format!("/api/v2/alerts/{id}/evidence"),
                };
                plan.push((ep, Duration::from_secs(s as u64) + Duration::from_millis(250 * j as u64), req("GET", &path, Some(ANALYST), None), false));
            }
        }
        let warm = Duration::from_secs(WARM_S);
        let mut hook_k = next;
        for (ep, at, body, is_hook) in plan {
            let (tx, rx) = if is_hook { (htx.clone(), hrx.clone()) } else { (rtx.clone(), rrx.clone()) };
            let tag = tag.clone();
            let k = hook_k;
            if is_hook {
                hook_k += 1;
            }
            tasks.push(tokio::spawn(async move {
                let sched = t0 + at;
                tokio::time::sleep_until(sched).await;
                let mut c = rx.lock().await.recv().await.unwrap();
                let start = tokio::time::Instant::now();
                let body = if is_hook { request(port, true, k, &tag) } else { body };
                let (status, bytes) = match exchange(&mut c, &body).await {
                    Ok((s, _, b)) => (s, b.len()),
                    Err(_) => {
                        c = conn(port).await;
                        (0, 0)
                    }
                };
                let done = tokio::time::Instant::now();
                tx.send(c).await.unwrap();
                let ms = |d: Duration| d.as_secs_f64() * 1000.0;
                (Offer { ep, sched_ms: ms(at), lag_ms: ms(start - sched), req_ms: ms(done - start), total_ms: ms(done - sched), status, bytes,
                         measured: at >= warm }, done)
            }));
        }
        // probes: every 10 s, and the commandstats at the start and end of the measured window
        let probe = tokio::spawn({
            let url = url.clone();
            async move {
                let mut c = redis::Client::open(url.as_str()).unwrap().get_connection().unwrap();
                let (mut samples, mut cs) = (vec![], vec![]);
                for s in (0..=total_s).step_by(5) {
                    tokio::time::sleep_until(t0 + Duration::from_secs(s)).await;
                    if s % 10 == 0 || s == total_s {
                        samples.push(sample(sp, rp, s as f64));
                    }
                    if s == WARM_S || s == total_s {
                        cs.push(commandstats(&mut c));
                    }
                }
                (samples, cs)
            }
        });
        let mut offers = vec![];
        let mut last_done = end;
        let deadline = end + Duration::from_secs(DRAIN_LIMIT_S);
        let mut missing = 0;
        for t in tasks {
            match tokio::time::timeout_at(deadline, t).await {
                Ok(Ok((o, done))) => {
                    last_done = last_done.max(done);
                    offers.push((o, done));
                }
                _ => missing += 1,
            }
        }
        let (samples, cs) = probe.await.unwrap();
        let open_at_end = offers.iter().filter(|(_, d)| *d > end).count() + missing;
        let drain_s = (last_done - end).as_secs_f64();

        // ---- checks after
        let (seq1, alerts1, ledger1) = counts(&mut con);
        let accepted = offers.iter().filter(|(o, _)| o.ep == WEBHOOK && o.status == 200).count();
        let after = check(reserved[1].clone()).await;
        let hash_after = pack_hash(reserved[0].clone()).await;
        let peak = r2(kb(sp, "VmHWM:") as f64 / 1024.0);
        let checks = json!({"anonymous_401": [before.0 == 401, after.0 == 401], "viewer_write_403": [before.1 == 403, after.1 == 403],
            "analyst_hold_200": [before.2 == 200, after.2 == 200], "reserved_pack_hash_stable": hash_before.is_some() && hash_before == hash_after,
            "seq_advanced_by_accepted_webhooks": seq1 - seq_before_window == accepted});
        let valid = stage_errors == 0 && missing == 0 && checks["anonymous_401"] == json!([true, true]) && checks["viewer_write_403"] == json!([true, true])
            && checks["analyst_hold_200"] == json!([true, true]) && checks["reserved_pack_hash_stable"] == true && checks["seq_advanced_by_accepted_webhooks"] == true;

        // ---- per endpoint summaries (measured window only) and raw rows
        let mut eps = serde_json::Map::new();
        for ep in std::iter::once(WEBHOOK).chain(READS) {
            let rows: Vec<&Offer> = offers.iter().map(|(o, _)| o).filter(|o| o.ep == ep && o.measured).collect();
            let mut st = std::collections::BTreeMap::<String, usize>::new();
            for o in &rows {
                *st.entry(o.status.to_string()).or_default() += 1;
            }
            eps.insert(ep.into(), json!({"offers": rows.len(), "status": st,
                "request_ms": series(rows.iter().map(|o| o.req_ms).collect()), "scheduled_to_completion_ms": series(rows.iter().map(|o| o.total_ms).collect()),
                "start_lag_ms": series(rows.iter().map(|o| o.lag_ms).collect()), "mean_response_bytes": rows.iter().map(|o| o.bytes).sum::<usize>() / rows.len().max(1)}));
        }
        for (o, _) in &offers {
            csv.push(format!("{rev},{stage},{trial},{},{},{:.1},{:.3},{:.3},{:.3},{},{}", o.ep, o.measured as u8, o.sched_ms, o.lag_ms, o.req_ms, o.total_ms, o.status, o.bytes));
        }
        let window_hooks_ok = offers.iter().filter(|(o, _)| o.ep == WEBHOOK && o.measured && o.status == 200).count();
        let delta: Value = cs.get(1).map(|b| json!(CMDS.iter().map(|k| (k.to_string(), json!(b.get(*k).unwrap_or(&0) - cs[0].get(*k).unwrap_or(&0))))
            .collect::<serde_json::Map<String, Value>>())).unwrap_or(Value::Null);
        drop(srv);
        drop(redis);
        let _ = std::fs::remove_dir_all(&dir);
        eprintln!("{tag}: staged {alerts0} alerts in {stage_s:.0} s, valid {valid}, open at end {open_at_end}, evidence p50 {} ms, alerts p50 {} ms",
            eps[READS[3]]["request_ms"]["p50"], eps[READS[0]]["request_ms"]["p50"]);
        json!({"revision": rev, "stage": stage, "trial": trial, "valid": valid, "staging": {"webhooks": next, "errors": stage_errors, "seconds": r2(stage_s)},
               "start": {"seq": seq0, "durable_alerts": alerts0, "ledger": ledger0, "expected_resident": alerts0.min(10_000), "resident": null},
               "end": {"seq": seq1, "durable_alerts": alerts1, "ledger": ledger1, "accepted_with_alert_delta": alerts1 - alerts0},
               "evidence_ids": {"first": evidence.first(), "last": evidence.last()}, "reserved": reserved,
               "window": {"webhooks_accepted_per_s": r2(window_hooks_ok as f64 / WINDOW_S as f64), "open_at_end": open_at_end, "drain_s": r2(drain_s), "missing": missing},
               "endpoints": eps, "checks": checks, "samples": samples, "server_peak_rss_mb": peak,
               "redis_commandstats_window_delta": delta, "harness_info_calls_in_window": 1,
               "pool_counters": "unavailable (not exposed)"})
    }

    fn median(mut v: Vec<f64>) -> f64 {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        if v.is_empty() { f64::NAN } else if v.len() % 2 == 1 { v[v.len() / 2] } else { (v[v.len() / 2 - 1] + v[v.len() / 2]) / 2.0 }
    }

    pub async fn main() {
        let bins: Vec<(&str, String)> = [("main", "RUSETECT_BENCH_BIN_MAIN"), ("baseline", "RUSETECT_BENCH_BIN_BASELINE")].iter()
            .filter_map(|(r, k)| std::env::var(k).ok().map(|b| (*r, b))).collect();
        assert!(bins.iter().any(|(r, _)| *r == "main"), "set RUSETECT_BENCH_BIN_MAIN");
        let only: Option<Vec<usize>> = std::env::var("RUSETECT_BENCH_STAGES").ok().map(|s| s.split(',').map(|x| x.parse().unwrap()).collect());
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../results");
        let (mut trials, mut csv) = (vec![], vec!["revision,stage,trial,endpoint,measured,scheduled_ms,start_lag_ms,request_ms,scheduled_to_completion_ms,status,bytes".to_string()]);
        for stage in STAGES.iter().copied().filter(|s| only.as_ref().is_none_or(|o| o.contains(s))) {
            let mut seen = std::collections::HashMap::<&str, usize>::new();
            for rev in ORDER {
                let Some((_, bin)) = bins.iter().find(|(r, _)| *r == rev) else { continue };
                let n = seen.entry(rev).or_default();
                *n += 1;
                let mut t = trial(bin, rev, stage, *n, &mut csv).await;
                if t["valid"] != true {
                    eprintln!("invalid trial: rerunning once in place");
                    let again = trial(bin, rev, stage, *n, &mut csv).await;
                    t = json!({"invalid_first_attempt": t, "rerun": again});
                }
                trials.push(t);
            }
        }
        let get = |t: &Value| if t.get("rerun").is_some() { t["rerun"].clone() } else { t.clone() };
        let mut summary = vec![];
        for stage in STAGES {
            for (rev, _) in &bins {
                let ts: Vec<Value> = trials.iter().map(get).filter(|t| t["stage"] == stage && t["revision"] == *rev).collect();
                if ts.is_empty() {
                    continue;
                }
                let mut eps = serde_json::Map::new();
                for ep in std::iter::once(WEBHOOK).chain(READS) {
                    let m = |p: &str| r2(median(ts.iter().filter_map(|t| t["endpoints"][ep]["request_ms"][p].as_f64()).collect()));
                    eps.insert(ep.into(), json!({"median_trial_p50_ms": m("p50"), "median_trial_p99_ms": m("p99"),
                        "trial_p50_ms": ts.iter().map(|t| t["endpoints"][ep]["request_ms"]["p50"].clone()).collect::<Vec<_>>(),
                        "trial_p99_ms": ts.iter().map(|t| t["endpoints"][ep]["request_ms"]["p99"].clone()).collect::<Vec<_>>()}));
                }
                summary.push(json!({"stage": stage, "revision": rev, "trials": ts.len(), "valid_trials": ts.iter().filter(|t| t["valid"] == true).count(),
                    "durable_alerts_at_start": ts.iter().map(|t| t["start"]["durable_alerts"].clone()).collect::<Vec<_>>(),
                    "server_peak_rss_mb": ts.iter().map(|t| t["server_peak_rss_mb"].clone()).collect::<Vec<_>>(), "endpoints": eps}));
            }
        }
        let rev_of = |k: &str| std::env::var(k).ok();
        let out = json!({"what": "ReviewDesk read-endpoint benchmark (docs/decision.md 8ar; protocol by Codex)",
            "generated_by": "crates/rusetect-server/examples/e2e_bench.rs (e2e_bench read)",
            "generated_at": chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(), "harness_head": cmd_out("git", &["describe", "--always", "--dirty"]),
            "revisions": {"main": rev_of("RUSETECT_BENCH_REV_MAIN"), "baseline": rev_of("RUSETECT_BENCH_REV_BASELINE")},
            "binaries_sha256": bins.iter().map(|(r, b)| (r.to_string(), json!(cmd_out("sha256sum", &[b]).and_then(|s| s.split_whitespace().next().map(String::from))))).collect::<serde_json::Map<_, _>>(),
            "machine": machine(),
            "scope": "One shared 4-CPU container, loopback, no TLS: load generator, server and redis-server share the CPUs. Not a production capacity figure.",
            "design": {"stages_durable_alerts": STAGES, "order_per_stage": ORDER, "warmup_s": WARM_S, "window_s": WINDOW_S, "webhooks_per_s": HOOK_RATE,
                       "reads_per_s": 4, "webhook_in_flight_limit": HOOK_LIMIT, "read_in_flight_limit": READ_LIMIT, "staging_concurrency": STAGE_CONC,
                       "evidence_ids": "75 oldest durable alerts, one per second", "desk_eviction_threshold": 10_000, "redis": "fresh per trial, AOF everysec",
                       "percentiles": "nearest rank over the measured window; with 60 reads per endpoint p99 is the maximum"},
            "summary": summary, "trials": trials});
        std::fs::write(root.join("read_bench.json"), serde_json::to_string_pretty(&out).unwrap() + "\n").unwrap();
        let mut gz = flate2::write::GzEncoder::new(std::fs::File::create(root.join("read_bench_offers.csv.gz")).unwrap(), flate2::Compression::best());
        gz.write_all((csv.join("\n") + "\n").as_bytes()).unwrap();
        gz.finish().unwrap();
        eprintln!("wrote results/read_bench.json and results/read_bench_offers.csv.gz");
    }
}
