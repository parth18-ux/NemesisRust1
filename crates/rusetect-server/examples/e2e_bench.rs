//! End-to-end benchmark of the real HTTP server: starts the release `rusetect-server` binary on loopback, drives
//! POST /api/v2/payment and the signed POST /api/v2/events/payment webhook over keep-alive HTTP/1.1 at a few fixed
//! concurrencies, in memory mode and with a local redis-server, and writes results/e2e_bench.json.
//!
//!   cargo build --release -p rusetect-server --bins --examples
//!   target/release/examples/e2e_bench [requests per level, default 5000]
//!   target/release/examples/e2e_bench s1 baseline|after   (S1 protocol, results/e2e_bench_s1_protocol.json)
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
    let rport = free_port();
    let redis = Kill(Command::new("redis-server").args(["--port", &rport.to_string(), "--save", "", "--appendonly", "no"])
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
    let exe = std::env::current_exe().unwrap();
    let bin = exe.parent().unwrap().parent().unwrap().join("rusetect-server");
    assert!(bin.exists(), "{} not found: run `cargo build --release -p rusetect-server --bins --examples` first", bin.display());
    let port = free_port();
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
