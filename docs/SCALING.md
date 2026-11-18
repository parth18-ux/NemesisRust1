# Scaling RuseTect

How the scoring path goes from one Python process to a bank's payment volume, what is built and tested in this
repository, and what is still design. Every number below comes from a script in `ml/` and a file in `results/`;
all data is synthetic (IBM AMLworld, Feedzai BAF).

## 1. What has to scale

| Piece | State | Cost per payment | How it scales |
|---|---|---|---|
| Feature computation | per account totals, 24 h windows, "has A paid B" index | a few dictionary and array updates | partition by account; state in a keyed store |
| L1 gatekeeper (30 trees) | none | well under a millisecond, batched | stateless, add processes |
| L2 XGBoost + LightGBM (about 20% of payments) | none | under a millisecond, batched | stateless, add processes |
| Context layer (rules) | none beyond the features | microseconds | stateless |
| Ring linking, owner merge | graph of alerted payments | runs on alerts only | hourly batch job |

The models are cheap. **The account state is the scaling problem.**

## 2. Measured on the full HI-Small file (5,078,345 payments, 515,088 accounts)

`ml/scale_bench.py` streams every payment, in time order, through three state layouts in separate processes.

| Layout | Account state | Payments per second (one core) | Features vs prototype |
|---|---|---|---|
| Prototype objects (`service/rusetect_engine.py`) | 2,136 MB | 42,679 | reference |
| Compact, exact pair index | 1,372 MB | 41,459 | identical on all 5,078,345 payments |
| Compact, Bloom pair index (20M capacity, 0.1% false positives) | 1,361 MB | 31,673 | 0.000% of test payments differ; decisions agree on 100.000% |

Account state is resident memory after streaming minus resident memory right after loading the payments. Inside the
compact layout the per account arrays take 39 MB and the exact pair index about
58 MB (34 MB as a Bloom filter); most of the rest is the
302,073 live 24 h windows and the account id map. So the compact layout cuts state by
36%, and the next saving is in the windows (fixed size ring buffers or sketches), not the pair index.

The compact layout (`service/compact_state.py`):
* accounts get integer ids; their totals live in typed arrays (`array` module), not one Python object each;
* "distinct payees / payers so far" are counters: a pair is new exactly when the payee is new to the payer, so the
  counter moves only when the pair index says "first time" (no per account sets);
* 24 h windows exist only for accounts active in the last day and are evicted when empty;
* the pair index is pluggable: an exact set of 64 bit pair ids (features identical to the prototype, checked in
  `tests/test_compact_state.py`), or a fixed size Bloom filter whose only possible error is to call a truly new
  payee "seen before".

## 3. Scale out: Redis state, stateless workers

Built in `service/scale/`, tested in `tests/test_scale.py` against a real `redis-server`:

```
payments ──► producer ──► Redis Streams  payments:0 … payments:N-1   (partition = crc32(payer) mod N)
                                 │
                 ┌───────────────┴───────────────┐
             worker 0   …   worker N-1            (stateless: features, L1, L2, pair window, context rules; batched)
                 │                               │
                 └──────► Redis account state ◄──┘   a:{acct} hash · p:{acct} payee set · wo/wi:{acct} 24 h zsets · pr:{acct}>payee risk zset
                                 │
                           decisions stream ──► analyst review desk, case tool, STR workflow
```

* Every key carries the account in a `{hash tag}`, so Redis Cluster shards the state by account with no code change.
* A worker reads one partition. All of a payer's payments land on one worker, in order, so the payer's own history
  is exact. A payee's receipts from other partitions can lag by milliseconds: the normal trade off of streaming state.
* One read round trip and one write round trip per payment, both pipelined; the models score in batches.
* With a single writer the Redis features equal the in memory engine's (`tests/test_scale.py`, 6,000 payments).

| Workers | Payments | Payments per second | Redis memory |
|---|---|---|---|
| 1 | 200,000 | 934 | 248.2 MB for 185,098 accounts |
| 2 | 200,000 | 1,315 | about 1,406 bytes per account |

Latency at 789 payments per second (two workers, publish to decision, including queueing):
p50 289.4 ms, p95 794.6 ms, p99 970.3 ms.

These are far below the 41,459 per second of the in process state: every payment costs two network
round trips to Redis, and here Redis, the producer and the workers share 2 cores (so the two worker run is
CPU bound, not partition bound). The next steps are known: move the read and update into one server side Lua script
(one round trip), pipeline across a batch where payers do not repeat, and run workers on their own cores or machines.

Run it yourself:

```bash
# local, no Docker
redis-server --save "" --appendonly no &
python service/scale/worker.py --partition 0 &
python service/scale/worker.py --partition 1 &
python service/scale/replay.py --csv path/to/HI-Small_Trans.csv --partitions 2 --limit 200000

# Docker (Redis + API + two workers)
docker compose -f deploy/docker-compose.yml up --build
```

`deploy/docker-compose.yml` was written for a machine with Docker; this workspace has no Docker daemon, so the
compose file itself is untested here (the same processes are tested natively above).

## 3b. End-to-end HTTP benchmark of the Rust server

`crates/rusetect-server/examples/e2e_bench.rs` starts the release `rusetect-server` binary on loopback and drives it
over keep-alive HTTP/1.1: `POST /api/v2/payment` and the HMAC-signed `POST /api/v2/events/payment` webhook, at
concurrency 1, 8, 32, 5,000 requests per level after a warm-up, first with the in-memory store and then
with a local `redis-server` (webhook only: `/v2/payment` keeps its state in process memory either way). It writes
`results/e2e_bench.json`, which the table below is copied from.

**One container, not Vercel.** The load generator, the server and Redis share 4 logical CPUs (Intel(R) Xeon(R) Processor @ 2.30GHz); there is
no network hop, TLS or cold start. Scoring holds the server's one state mutex, so it is serial; more concurrency mostly
adds queueing. Expect tens of percent run to run on a shared container. It replaces the old Python figure
(`results/load_test.json`, uvicorn, 220 requests per second), which measured a server that is no longer served.

| Store | Endpoint | Concurrency | Requests per second | p50 ms | p95 ms | p99 ms | Errors |
|---|---|---|---|---|---|---|---|
| memory | `POST /api/v2/payment` | 1 | 1,979.86 | 0.28 | 1.51 | 1.91 | 0 |
| memory | `POST /api/v2/payment` | 8 | 2,588.63 | 2.95 | 6.07 | 8.27 | 0 |
| memory | `POST /api/v2/payment` | 32 | 3,474.8 | 8.24 | 19.78 | 28.67 | 0 |
| memory | `POST /api/v2/events/payment` | 1 | 1,210.66 | 0.68 | 1.64 | 2.52 | 0 |
| memory | `POST /api/v2/events/payment` | 8 | 1,865.72 | 4.24 | 7.23 | 9.65 | 0 |
| memory | `POST /api/v2/events/payment` | 32 | 1,961.23 | 16.55 | 23.6 | 37.0 | 0 |
| redis | `POST /api/v2/events/payment` | 1 | 1,382.09 | 0.67 | 1.06 | 1.3 | 0 |
| redis | `POST /api/v2/events/payment` | 8 | 1,333.57 | 5.71 | 7.76 | 9.18 | 0 |
| redis | `POST /api/v2/events/payment` | 32 | 1,146.63 | 27.06 | 35.41 | 43.07 | 0 |

Rerun it:

```bash
cargo build --release -p rusetect-server --bins --examples
target/release/examples/e2e_bench            # optional argument: requests per level (default 5000); needs redis-server
```

## 3c. S1: scaling the Redis webhook on one node (pre-registered)

The Redis-mode webhook ran fully serial in section 3b: `events_payment` held the one state mutex for the whole
ingest, six synchronous Redis round trips included. S1 takes Redis I/O and scoring out of that mutex (a small
connection pool, per-payer stripes, a ledger-order lock) and cuts the round trips from six to four. (Since S1 the
stripes cover the payee too, so payee features are not read stale under concurrency; this was not re-measured.)

**Protocol, registered before any measurement of the changed code** (`results/e2e_bench_s1_protocol.json`):

* Harness: `e2e_bench s1 baseline|after`, closed loop, concurrency 1, 8, 32, 64, 128; 20,000 requests per level after
  a 1,000 request warm-up; 3 repetitions on fresh processes, median reported next to all three; `RUSETECT_LIVE_POOL`
  recorded. Same workload as 3b (400 payers, 300 payees, unique signed events).
* Correctness gate, every level of every repetition (a level that fails it voids the run): no errors; `live:ledger`
  grew by exactly N; `live:seq` advanced by exactly N; 100 sampled events resent come back `Idempotent-Replayed:
  true` with identical bodies and no ledger growth.
* Pass: Redis webhook at concurrency 32, median of 3, at least 3,000 requests per second with p99 under 50 ms and the
  gate green. Stretch, reported either way: 5,000 requests per second at any level up to 128 with p99 under 50 ms.
  No regression: memory-mode `/api/v2/payment` and memory-mode webhook within 10% of the baseline.
* The baseline is the unchanged code, same machine, same session. Output: `results/e2e_bench_s1.json`.

**Result** (`results/e2e_bench_s1.json`; baseline at `0ae8027-dirty`, whose binaries were built from the unchanged
server code before the edits that make it `-dirty`; after at `4be9aeb`; `RUSETECT_LIVE_POOL` unset in both, so
the after run used the default of 2 x 4 logical CPUs). Medians of 3 repetitions; "after rps by repetition" lists all three.
The correctness gate is checked on the Redis webhook rows only (it counts `live:ledger` and `live:seq`); memory rows show
`n/a`.

| Store | Endpoint | Concurrency | Baseline rps | Baseline p99 ms | After rps | p50 ms | p95 ms | p99 ms | max ms | After rps by repetition | Gate |
|---|---|---|---|---|---|---|---|---|---|---|---|
| redis | `POST /api/v2/events/payment` | 1 | 1,166.26 | 1.49 | 1,356.15 | 0.67 | 1.12 | 1.33 | 6.25 | 1,695.81 / 1,347.29 / 1,356.15 | green |
| redis | `POST /api/v2/events/payment` | 8 | 1,320.85 | 8.05 | 4,648.73 | 1.65 | 2.64 | 3.3 | 7.8 | 4,648.73 / 5,035.58 / 4,549.65 | green |
| redis | `POST /api/v2/events/payment` | 32 | 1,293.44 | 32.04 | 5,282.43 | 5.94 | 7.81 | 8.86 | 13.52 | 5,002.02 / 5,955.65 / 5,282.43 | green |
| redis | `POST /api/v2/events/payment` | 64 | 1,312.12 | 81.35 | 4,823.22 | 12.23 | 24.43 | 30.56 | 40.29 | 4,413.48 / 5,575.19 / 4,823.22 | green |
| redis | `POST /api/v2/events/payment` | 128 | 1,084.9 | 185.32 | 3,231.58 | 35.39 | 64.18 | 70.92 | 80.74 | 3,231.58 / 4,485.62 / 3,179.32 | green |
| memory | `POST /api/v2/payment` | 1 | 2,419.11 | 1.73 | 2,303.58 | 0.28 | 1.56 | 1.82 | 8.67 | 2,361.48 / 2,303.58 / 2,055.72 | n/a |
| memory | `POST /api/v2/payment` | 8 | 2,823.69 | 7.47 | 2,725.64 | 2.73 | 5.79 | 7.71 | 15.1 | 2,790.48 / 2,676.13 / 2,725.64 | n/a |
| memory | `POST /api/v2/payment` | 32 | 3,295.03 | 31.09 | 3,116.95 | 10.16 | 22.84 | 31.28 | 65.65 | 3,217.28 / 3,109.5 / 3,116.95 | n/a |
| memory | `POST /api/v2/payment` | 64 | 3,712.43 | 56.59 | 3,269.37 | 18.56 | 45.06 | 58.66 | 123.67 | 3,378.47 / 3,122.24 / 3,269.37 | n/a |
| memory | `POST /api/v2/payment` | 128 | 3,911.95 | 103.12 | 3,579.52 | 33.45 | 79.14 | 108.31 | 214.6 | 3,838.98 / 3,579.52 / 3,499.51 | n/a |
| memory | `POST /api/v2/events/payment` | 1 | 1,212.19 | 2.39 | 1,225.31 | 0.63 | 2.2 | 2.41 | 14.95 | 1,225.31 / 1,225.27 / 1,230.71 | n/a |
| memory | `POST /api/v2/events/payment` | 8 | 1,876.69 | 9.85 | 1,851.48 | 3.97 | 7.43 | 9.57 | 20.63 | 1,851.48 / 1,747.73 / 1,950.44 | n/a |
| memory | `POST /api/v2/events/payment` | 32 | 1,848.51 | 43.48 | 1,871.63 | 16.1 | 29.07 | 35.45 | 48.98 | 1,859.29 / 1,886.48 / 1,871.63 | n/a |
| memory | `POST /api/v2/events/payment` | 64 | 1,857.5 | 85.27 | 1,905.0 | 32.08 | 57.44 | 69.67 | 92.19 | 1,905.0 / 1,932.16 / 1,776.23 | n/a |
| memory | `POST /api/v2/events/payment` | 128 | 1,351.85 | 282.67 | 1,304.57 | 72.85 | 267.5 | 378.88 | 560.27 | 1,277.79 / 1,337.3 / 1,304.57 | n/a |

Against the pre-registered rule (`pass` in the file):

* Correctness gate: green at every level of every repetition (no errors, exact ledger growth, exact
  `live:seq` advance, 100 identical replays per level with no ledger growth). The baseline was green too.
* Primary: **met**. Concurrency 32 median 5,282.43 requests per second, p99 8.86 ms.
* Stretch (5,000 requests per second with p99 under 50 ms): **met** at concurrency 32.
* No regression: **not met**. Below 90% of baseline: `POST /api/v2/payment` at concurrency 64: 3,269.37 against 3,712.43.
  The change does not touch `/api/v2/payment` (no code on its path changed), so this is most likely run-to-run
  variation on the shared container, but the rule was fixed in advance and is applied as written. The rerun that might
  clear it was not done.
* **Overall: fail**, on the no-regression clause alone.

Past concurrency 32, throughput falls and p99 rises (best median 5,282.43 at concurrency
32). Load generator, server, Redis and the blocking pool all compete for the same 4 logical CPUs,
and Redis runs its commands on one thread. Where the time goes was not profiled, so no bottleneck is named.

**10,000 requests per second on one node is not credible on this box.** It would take nearly twice the best median
above. Every event already costs four Redis round trips and a scoring pass, and Redis, the server and the load
generator share four CPUs. More concurrency made it slower, not faster. Getting there needs more cores or separate
machines for Redis and the load generator, or fewer round trips per event (group commit, change D, deliberately not
built). It also needs a measurement made under a rule written first.

High-volume senders must raise `RUSETECT_RATE_LIMIT` (requests per 10 s per client): at its default the per-client
limit sits far below these rates. The webhook is deliberately not exempt from the limiter (`docs/RUNBOOK.md` section 8).

## 3d. F1/F2: webhook and read endpoints after step 1 (pre-registered)

Step 1 changed the Redis webhook's commit path (a retry after a partial failure leaves one outcome; a success never hides
a missing feed, ledger or alert record), so the S1 figures above describe older code. F1 re-measures the Redis webhook;
F2 measures the read endpoints behind the workspace for the first time. Both are run by `e2e_bench f1`
(`crates/rusetect-server/examples/e2e_bench.rs`) and written to `results/perf_f1.json`; `docs/PERFORMANCE_REPORT.md`
quotes only that file.

**Protocol, written and committed before any F1 or F2 measurement:**

* Build: release binaries of the commit under test (`cargo build --release -p rusetect-server --bins --examples`). The
  git head is recorded; the binaries must be built from it.
* Processes: one `rusetect-server` (demo mode, webhook secret set, rate limit raised so it never refuses) and one
  `redis-server` (no persistence), both on fixed non-default loopback ports in 8200 to 8299, and the load generator, all
  on the same shared 4-CPU container. Fresh processes for every repetition.
* Load: closed loop, keep-alive HTTP/1.1, concurrency 1, 8, 32, 64 and 128.
  * F1, `POST /api/v2/events/payment` (Redis store): 1,000 warm-up then 20,000 signed, unique events per level. Same
    workload as S1 (400 payers, 300 payees).
  * F2, after the F1 levels on the same server (so the store holds 105,000 events and their alerts): `GET
    /api/v2/dashboard`; `GET /api/v2/transactions?limit=50&offset=k` with `k` cycling over 0, 50, ... 950; `GET
    /api/v2/graph?account=A&depth=2` for an account taken from the newest stored payment; `GET
    /api/v2/alerts?limit=50&offset=k` with `k` cycling the same way. 200 warm-up then 3,000 requests per endpoint
    and level (cut by amendment 1 below).
* Repetitions: 3. The table reports the median of the three for every figure, with the three throughputs next to it.
* Per level: throughput (successful responses per wall second); latency p50, p95, p99 and max of successful requests;
  error rate (non-200 responses and connection errors over requests); CPU of the server, of `redis-server` and of the
  load generator (CPU seconds over wall seconds, from `/proc/<pid>/stat`, so 1.0 is one core of the 4); server resident
  memory after the level and its peak (`VmRSS`, `VmHWM`), and Redis `used_memory`.
* F1 correctness check at every level (as in S1): no errors, `live:seq` advanced by exactly the events sent. A level
  that fails it is reported as failed, not dropped.
* Profile: separately from the timed runs (perf sampling slows the server), one webhook run at concurrency 32 and one
  at 128, each with `perf record` (CPU-clock sampling, no call graphs) on the server for a fixed window, plus the CPU
  split above and the server's context switches. The report names the bottleneck these measurements show for the drop
  in throughput above concurrency 32; if they do not show one, it says so.
* Fix rule: a fix is made only if the profile names a cause that is cheap and safe to change (no change to what is
  stored or decided, all tests green). If one is made, F1 is run again in full (3 repetitions) on the fixed code and
  both runs are kept in `results/perf_f1.json` and reported side by side. No target is set in advance: F1 and F2
  are measurements, not a pass or fail test.
* **Amendment 1, made before any read result was recorded.** The first run was stopped during its first read level:
  with the store as F1 leaves it, one `GET /api/v2/dashboard` took close to a second, so 3,000 requests per endpoint
  and level (60,000 per repetition) would have run for many hours. The read load is cut to 20 warm-up and 200
  requests per endpoint and level; everything else stands. The webhook figures of that stopped run were not kept;
  the full protocol, webhook included, was run again from the start.
* **Amendment 2, made before any figure of the second run was kept.** In the second run the dashboard level at
  concurrency 128 grew the server past the container's memory limit and the kernel killed it. The container is shared
  with other sessions, so the harness now kills the server itself once it holds more than 6,000 MB resident, marks
  that level aborted and skips the rest of that repetition's read levels; medians are taken over completed
  repetitions only, and the count is reported. The code as it was is measured under this rule as run `f1_before`.
  The memory growth is a defect (any reader can exhaust the server's memory), so it falls under the fix rule: if it is
  fixed, the full protocol runs again on the fixed code as `f1_after`, and both are reported.
* **Amendment 3.** The first `f1_after` run completed, but its results file was cut short when the disk filled while
  the profile was being added to it, so no figure of it is used. `f1_after` was run again in full on a later commit,
  which also carries the pair-window change for retries (`docs/live.md`); the profile is written to the file after it.
* Scope, stated in the report: a shared 4-CPU container where the load generator, the server and Redis compete for the
  same cores; no network hop, TLS or cold start; not the Vercel deployment and **not a production capacity figure**.

**Result:** `docs/PERFORMANCE_REPORT.md`, from `results/perf_f1.json`. The webhook's fall above concurrency 32 is the
store growing across levels (the review desk's eviction scan once it holds 10,000 alerts), not concurrency; the read
endpoints copy every stored alert per request.

## 4. From here to a bank

1. **Throughput.** UPI runs at roughly 20 billion payments a month, about 7,000 to 8,000 a second on average with
   peaks several times higher (check NPCI's current figures). One bank sees its own share. Workers are stateless, so
   capacity is the number of worker processes; Redis Cluster (or Flink keyed state, Aerospike, or DynamoDB) holds
   the state, sharded by account.
2. **State size.** Keep running totals and 24 h windows only. Replace exact payee sets with a Bloom or cuckoo filter
   per partition once the exact index outgrows memory (section 2 measures the trade off).
3. **Placement.** Inside the bank (RBI data localisation, DPDP Act), on the payment switch before the debit, within
   the switch's timeout budget. Decisions feed the existing case tool; the four eyes block rule and the hash chained
   audit log carry over; STRs go to FIU-IND as today.
4. **Cold start.** Replay recent history into the state store before going live; without history, recall collapses.
5. **Rollout.** Retrain on the bank's labelled fraud, run in shadow mode (score, act on nothing) for weeks, set
   thresholds to the analyst team's capacity, then champion and challenger against the current system with drift
   monitoring.

## 5. Honest limits

* All results are on synthetic data.
* The live payment model holds 0.716 PR-AUC on HI-Small but 0.336 on LI-Small, a lower laundering intensity it never
  trained on (`results/full_eval_*.json`). A bank must retrain on its own data.
* The ctx-1.3 mule signature, tuned on HI-Small validation weeks, does not transfer to LI-Small (no true positives).
* Scale out numbers come from a 2 core container where Redis, the producer and the workers share the cores; they
  show the design works and give a floor, not a production capacity figure.
