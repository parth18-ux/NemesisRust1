# Performance report: F1 and F2

The signed Redis webhook after the step 1 changes (F1) and the read endpoints behind the workspace (F2), measured under
the protocol registered in `docs/SCALING.md` section 3d before any measurement (with three amendments, written there
before the figures they affect). **Every number below is copied from `results/perf_f1.json`**, written by
`target/release/examples/e2e_bench f1` (`crates/rusetect-server/examples/e2e_bench.rs`) and by the profile step.

**What this is, and is not.** One shared container with 4 logical CPUs (Intel(R) Xeon(R) Processor @ 2.10GHz). The load generator, the
server and `redis-server` run on the same cores, over loopback: no network hop, no TLS, no cold start. Other sessions
use the same machine. These figures show how the code behaves and where it stops scaling; **they are not a production
capacity figure** and not a measurement of the Vercel deployment.

## Runs

| Run | Code (as recorded) | Results written |
|---|---|---|
| `f1_before` | `2b56a8e-dirty`: binaries built before the F5 fix was written (the tree was `-dirty` with that edit while it ran) | 2026-11-18T11:14:13Z |
| `f1_after` | `f9b36a2`: with the F5 fix (bounded heavy reads) and the pair-window change for retries | 2026-11-18T16:11:37Z |

The commit ids in the file were recorded before this branch was rebased onto `main` (after pull request #39 merged,
with no change to any file): `2b56a8e` is now `15ca39f` and `f9b36a2` is now `484eb37`. The protocol was committed as
`364b4a5` (then `22b7fd8`) before any measurement.

Design (both runs): concurrency 1, 8, 32, 64, 128; 3 repetitions on
fresh processes; webhook 20,000 events per level after 1,000
warm-up; reads 200 requests per endpoint and level after 20
warm-up, against the store the webhook levels left. Figures are medians of the repetitions; CPU is CPU seconds over wall
seconds (1.0 is one of the 4 CPUs).

## F1: the Redis webhook (`f1_after`)

| Concurrency | Requests per second (median) | By repetition | p50 ms | p95 ms | p99 ms | max ms | Error rate | CPU server | CPU Redis | CPU load generator | Server RSS MB | Redis memory MB | Check |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 1 | 982.65 | 814.98 / 994.59 / 982.65 | 0.93 | 1.56 | 2.07 | 17.97 | 0 | 0.61 | 0.24 | 0.06 | 171.11 | 32.43 | green |
| 8 | 3,708.14 | 3,598.95 / 3,777.82 / 3,708.14 | 2.01 | 3.48 | 4.64 | 20.60 | 0 | 1.98 | 0.67 | 0.13 | 207.09 | 63.30 | green |
| 32 | 4,297.53 | 4,015.30 / 4,297.53 / 4,575.88 | 7.30 | 9.63 | 10.99 | 15.51 | 0 | 2.14 | 0.70 | 0.14 | 234.17 | 92.47 | green |
| 64 | 3,811.72 | 3,394.24 / 3,811.72 / 3,882.07 | 15.42 | 27.35 | 35.88 | 46.51 | 0 | 2.08 | 0.67 | 0.13 | 256.56 | 123.57 | green |
| 128 | 2,861.29 | 2,541.35 / 2,861.29 / 3,146.43 | 40.49 | 68.83 | 73.30 | 81.92 | 0 | 1.94 | 0.53 | 0.11 | 273.15 | 150.57 | green |

Every level of every repetition passed the correctness check (no errors, `live:seq` advanced by exactly the events
sent). The best median is 4,297.53 requests per second at concurrency 32, p99
10.99 ms. The run on the code before the fix (`f1_before`, same harness) is in the file; its webhook
levels give the same picture.

## F2: the read endpoints (`f1_after`)

| Endpoint | Concurrency | Completed repetitions | Requests per second (median) | By repetition | p50 ms | p95 ms | p99 ms | Error rate | CPU server | CPU Redis | Server peak RSS MB |
|---|---|---|---|---|---|---|---|---|---|---|---|
| `GET /api/v2/dashboard` | 1 | 3 of 3 | 1.65 | 1.55 / 1.67 / 1.65 | 601.50 | 712.70 | 757.65 | 0 | 0.97 | 0.02 | 1,997.87 |
| `GET /api/v2/dashboard` | 8 | 3 of 3 | 2.66 | 2.50 / 2.66 / 2.66 | 2,995.22 | 3,324.59 | 3,455.75 | 0 | 1.65 | 0.03 | 2,070.78 |
| `GET /api/v2/dashboard` | 32 | 3 of 3 | 2.66 | 2.61 / 2.67 / 2.66 | 11,968.44 | 12,589.95 | 12,800.51 | 0 | 1.63 | 0.03 | 2,071.25 |
| `GET /api/v2/dashboard` | 64 | 3 of 3 | 2.53 | 2.53 / 2.70 / 2.51 | 24,814.82 | 26,308.86 | 26,671.04 | 0 | 1.64 | 0.03 | 2,071.25 |
| `GET /api/v2/dashboard` | 128 | 3 of 3 | 2.60 | 2.60 / 2.72 / 2.40 | 40,574.04 | 49,738.76 | 50,353.02 | 0 | 1.65 | 0.04 | 2,071.25 |
| `GET /api/v2/transactions` | 1 | 3 of 3 | 1.86 | 1.87 / 1.86 / 1.69 | 539.92 | 610.14 | 665.95 | 0 | 0.97 | 0.03 | 2,071.25 |
| `GET /api/v2/transactions` | 8 | 3 of 3 | 2.90 | 2.86 / 2.99 / 2.90 | 2,736.31 | 2,977.85 | 3,195.12 | 0 | 1.62 | 0.04 | 2,071.25 |
| `GET /api/v2/transactions` | 32 | 3 of 3 | 2.89 | 2.79 / 2.89 / 2.92 | 10,913.11 | 11,418.79 | 11,635.89 | 0 | 1.60 | 0.04 | 2,071.25 |
| `GET /api/v2/transactions` | 64 | 3 of 3 | 3 | 2.87 / 3 / 3.06 | 21,234.20 | 21,716.48 | 21,851.58 | 0 | 1.61 | 0.04 | 2,071.25 |
| `GET /api/v2/transactions` | 128 | 3 of 3 | 2.99 | 2.79 / 2.99 / 3 | 34,316.70 | 42,981.78 | 43,383.84 | 0 | 1.60 | 0.04 | 2,071.25 |
| `GET /api/v2/graph` | 1 | 3 of 3 | 1.79 | 1.75 / 1.85 / 1.79 | 557.01 | 655.80 | 733.69 | 0 | 0.97 | 0.03 | 2,071.25 |
| `GET /api/v2/graph` | 8 | 3 of 3 | 2.82 | 2.69 / 2.94 / 2.82 | 2,846.69 | 3,118.79 | 3,491.87 | 0 | 1.60 | 0.04 | 2,071.25 |
| `GET /api/v2/graph` | 32 | 3 of 3 | 2.81 | 2.69 / 2.90 / 2.81 | 11,312 | 11,907.38 | 12,033.80 | 0 | 1.59 | 0.04 | 2,071.25 |
| `GET /api/v2/graph` | 64 | 3 of 3 | 2.73 | 2.73 / 2.73 / 2.84 | 22,626.80 | 24,286.66 | 24,449.76 | 0 | 1.60 | 0.04 | 2,071.25 |
| `GET /api/v2/graph` | 128 | 3 of 3 | 2.84 | 2.84 / 2.80 / 2.88 | 36,186.19 | 45,839.04 | 46,103.83 | 0 | 1.60 | 0.04 | 2,071.25 |
| `GET /api/v2/alerts` | 1 | 3 of 3 | 2.03 | 2.03 / 2.03 / 2.05 | 483.48 | 599.21 | 655.24 | 0 | 0.97 | 0.03 | 2,071.25 |
| `GET /api/v2/alerts` | 8 | 3 of 3 | 3.26 | 3.34 / 3.26 / 3.13 | 2,430.75 | 2,810.59 | 3,087.78 | 0 | 1.53 | 0.04 | 2,071.25 |
| `GET /api/v2/alerts` | 32 | 3 of 3 | 3.17 | 3.17 / 3.28 / 3.06 | 9,883.54 | 10,934.53 | 11,085.73 | 0 | 1.52 | 0.04 | 2,071.25 |
| `GET /api/v2/alerts` | 64 | 3 of 3 | 3.28 | 3.28 / 3.46 / 3.23 | 19,406.34 | 19,768.57 | 19,851.40 | 0 | 1.54 | 0.04 | 2,071.25 |
| `GET /api/v2/alerts` | 128 | 3 of 3 | 3.31 | 3.31 / 3.31 / 3.21 | 30,799.64 | 38,697.96 | 38,976.65 | 0 | 1.54 | 0.04 | 2,071.25 |

No errors at any level. Each request takes about half a second or more even alone (median at concurrency 1:
`/api/v2/dashboard` 601.5 ms, `/api/v2/transactions` 539.92 ms, `/api/v2/graph` 557.01 ms, `/api/v2/alerts` 483.48 ms), and throughput stays between 1.65 and
3.31 requests per second whatever the concurrency: more callers only queue. Redis is nearly idle (CPU about
0.04); the time is the server's.

### Before the fix (`f1_before`)

| Endpoint | Concurrency | Completed repetitions | Requests per second (median) | By repetition | p50 ms | p95 ms | p99 ms | Error rate | CPU server | CPU Redis | Server peak RSS MB |
|---|---|---|---|---|---|---|---|---|---|---|---|
| `GET /api/v2/dashboard` | 1 | 3 of 3 | 1.63 | 1.70 / 1.63 / 1.57 | 609.45 | 714.74 | 759.42 | 0 | 0.97 | 0.03 | 1,939.57 |
| `GET /api/v2/dashboard` | 8 | 3 of 3 | 2.57 | 2.67 / 2.54 / 2.57 | 3,103.17 | 3,666.23 | 3,937.64 | 0 | 1.66 | 0.04 | 2,059.84 |
| `GET /api/v2/dashboard` | 32 | 3 of 3 | 2.60 | 2.65 / 2.51 / 2.60 | 11,988.55 | 14,391.22 | 15,777.36 | 0 | 1.68 | 0.04 | 4,076.23 |
| `GET /api/v2/dashboard` | 64 | 0 of 3 | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a (server resident memory passed 6000 MB after 22.7 s with 1 responses; server killed by the harness) |
| `GET /api/v2/dashboard` | 128 | 0 of 3 | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a (the server was killed at an earlier level of this repetition) |
| `GET /api/v2/transactions` | 1 | 0 of 3 | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a (the server was killed at an earlier level of this repetition) |
| `GET /api/v2/transactions` | 8 | 0 of 3 | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a (the server was killed at an earlier level of this repetition) |
| `GET /api/v2/transactions` | 32 | 0 of 3 | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a (the server was killed at an earlier level of this repetition) |
| `GET /api/v2/transactions` | 64 | 0 of 3 | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a (the server was killed at an earlier level of this repetition) |
| `GET /api/v2/transactions` | 128 | 0 of 3 | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a (the server was killed at an earlier level of this repetition) |
| `GET /api/v2/graph` | 1 | 0 of 3 | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a (the server was killed at an earlier level of this repetition) |
| `GET /api/v2/graph` | 8 | 0 of 3 | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a (the server was killed at an earlier level of this repetition) |
| `GET /api/v2/graph` | 32 | 0 of 3 | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a (the server was killed at an earlier level of this repetition) |
| `GET /api/v2/graph` | 64 | 0 of 3 | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a (the server was killed at an earlier level of this repetition) |
| `GET /api/v2/graph` | 128 | 0 of 3 | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a (the server was killed at an earlier level of this repetition) |
| `GET /api/v2/alerts` | 1 | 0 of 3 | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a (the server was killed at an earlier level of this repetition) |
| `GET /api/v2/alerts` | 8 | 0 of 3 | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a (the server was killed at an earlier level of this repetition) |
| `GET /api/v2/alerts` | 32 | 0 of 3 | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a (the server was killed at an earlier level of this repetition) |
| `GET /api/v2/alerts` | 64 | 0 of 3 | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a (the server was killed at an earlier level of this repetition) |
| `GET /api/v2/alerts` | 128 | 0 of 3 | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a (the server was killed at an earlier level of this repetition) |

At concurrency 64 the dashboard grew the server past 6,000 MB in every repetition and the harness killed it
("server resident memory passed 6000 MB after 22.7 s with 1 responses; server killed by the harness"); in the stopped first run, the same level reached the container's memory limit and the kernel killed the
server. With the fix, the server's peak resident memory over every read level of `f1_after` is 2,071.25 MB.

## Where the time goes

### Reads: every request copies every stored alert

`/api/v2/dashboard`, `/api/v2/transactions`, `/api/v2/graph` and `/api/v2/alerts` all start from `live::all_alerts`:
an `HGETALL` of the whole `live:alerts` hash, a JSON parse of each alert, and a copy of the in-memory desk's alerts,
before any filter or page is applied. So the cost of one request grows with the number of stored alerts, not with the
page asked for, and each request in flight holds a full copy. Unbounded, concurrent requests multiplied that copy until
the process was killed (security finding F5). **Fixed** (cheap and safe: nothing stored or returned changes): at most
one such read per CPU runs at once (`HEAVY_READS` in `crates/rusetect-server/src/lib.rs`), the rest wait without
holding memory. **Not fixed:** the per-request cost itself. Paging in Redis (a sorted index of alerts, `HSCAN`/`HMGET` of
one page) or a cached summary for the dashboard would remove it; that is a design change to the store, left for a
separate step.

### Webhook: the drop above concurrency 32 is the store, not the concurrency

In F1 every level runs on the same server after the levels before it, so the store grows as concurrency rises. Run on
a **fresh server each time**, concurrency 128 is no slower than 32: 4,004.76, 4,025.78, 4,071.98 requests per
second at 32 and 3,882.97, 4,153.09, 3,941.43 at 128 (`profile.fresh_levels`). Held at **concurrency 32 on one
server while the store grows** by 20,000 events a level (`profile.growth`), throughput goes
3,945.75 → 4,172.8 → 4,083.38 → 3,695.01 → 2,882.24 → 2,171.43 requests per second, with
157 → 2,516 → 5,433 → 7,201 → 9,320 → 10,909 alerts stored before each level. The steep fall comes
with the levels that cross 10,000 stored alerts; the smaller dip one level earlier (a single run, not repeated) was not
investigated.

The profile names the cause. On the last growth level the hottest function of the server is
`IndexMap<String, serde_json::Value>::get_index_of` (with `memcmp` close behind), called from
`ReviewDesk::open_alert_as` ← `live::decide` ← `live::ingest_redis` (`profile.growth`). The in-memory review desk keeps
at most `MAX_ALERTS` (10,000) alerts; once it is full, every new alert scans all of them for a released or blocked one
to evict (`self.alerts.iter().find(...)`, reading each alert's `status`) and then `shift_remove`s from an `IndexMap`,
which moves every later entry. Both are linear in the desk size and run **under the state mutex**, so every alert-raising
event serialises on them. On a fresh server the same profile shows the model (`XgbBooster::margin`, then
`LiteBooster::raw`) on top instead (16.09%, 5.84% of server CPU at concurrency 32), and
11.18 server context switches per request at 32 against
11.15 at 128: no sign of lock convoying from concurrency alone.

CPU agrees: the server uses about 2 of the 4 CPUs and Redis under 1 at every level, and server CPU falls slightly at
concurrency 128 while throughput falls, which is what a serial section does.

**Not fixed in this step.** The fix is small (evict the oldest entry when no final alert is near the front, or keep an
index of final alerts, and use `swap_remove` or a `VecDeque` of ids) but it changes which alert the desk evicts, so it
does not meet the fix rule ("no change to what is stored or decided") and would need its own test and a re-run. Until
then, a long-running instance that holds 10,000 open alerts slows down; reviewing (releasing or blocking) alerts or a
restart restores the speed.

## What this means

- On this box the webhook peaks at 4,297.53 events per second (concurrency 32, median of 3).
  Below the full-desk threshold it holds about the same rate at concurrency 128.
- The read endpoints are the weak spot: under a second each when the store holds tens of thousands of alerts, and
  about 3 per second in total. They are fine for a few analysts, not for a busy desk with a large store.
- Two named next steps, neither built here: paged alert reads in Redis, and an O(1) desk eviction.

