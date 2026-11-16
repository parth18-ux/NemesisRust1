# Baseline test report (Stage W0)

Recorded on 16 Nov 2026 against `main` at `aa9121f`, before any website change. Every number below was read from the
command's own output in this session; nothing is carried over from earlier reports.

Machine: 4 logical CPUs, Linux 6.18.44, `cargo 1.97.0`, `rustc 1.97.0`, Python 3.13.18, `redis-server` installed
(so the Redis tests ran rather than skipped). Timings are wall-clock seconds measured with `date` around each command.

## Summary

| # | Command | Result | Count | Time |
|---|---|---|---|---|
| 1 | `cargo test --release --workspace` | **PASS** (exit 0) | 133 passed, 0 failed, 0 ignored | 112 s (release build already cached: "Finished in 13.63s") |
| 2 | `cargo clippy --workspace --all-targets -- -D warnings` | **PASS** (exit 0) | 0 warnings, 0 errors | 23 s (forced re-check of all three crates) |
| 3 | `python3 tools/pilot_sender.py --self-test` | **PASS** (exit 0) | "pilot_sender self-test passed" | 0.6 s |
| 4 | `python3 -m pytest tests/ -q` in `/home/user/nemesis1` | **FAIL** (exit 1) | 57 passed, 1 failed | 54 s |
| 5a | Local smoke test, demo mode, every route | **PASS** | 62 of 62 requests as expected | about 1 s (first `/v2/assess` 266 ms, model warm-up) |
| 5b | Local smoke test, configured analyst keys (auth rules) | **PASS** | 9 of 9 checks as expected | under 1 s |
| -- | Python tests in this repository | none exist | 0 | -- |
| -- | Frontend tests (JS, browser, end to end) | none exist | 0 | -- |
| -- | Live site `https://nemesis-rust1.vercel.app` | **not run** | -- | the sandbox proxy refused the connection |

## 1. `cargo test --release --workspace`

```
rusetect           unittests src/lib.rs   61 passed; 0 failed   0.56s
rusetect_server    unittests src/lib.rs    4 passed; 0 failed   0.54s
rusetect_replay    src/bin/replay.rs       0 tests
rusetect_server    src/main.rs             0 tests
rusetect_worker    src/bin/worker.rs       0 tests
tests/api.rs                              43 passed; 0 failed   7.26s
tests/live.rs                             14 passed; 0 failed  66.68s
tests/motifs.rs                            4 passed; 0 failed   3.95s
tests/scale.rs                             7 passed; 0 failed   3.64s
api/rusetect.rs (Vercel bin)               0 tests
Doc-tests rusetect, rusetect_server        0 tests
EXIT 0 SECONDS 112
```

Total 133 passed. `tests/live.rs` dominates the time (the Redis outage and concurrency tests); one of them,
`concurrent_outage_fails_closed`, ran for over 60 s by design. No "redis-server not installed: skipping" line was
printed, so the Redis tests in `tests/live.rs` and `tests/scale.rs` really ran.

## 2. `cargo clippy --workspace --all-targets -- -D warnings`

The first run replayed a cached result in 0.35 s, so the three crate roots were touched and clippy was run again:

```
Checking rusetect v2.2.0
Checking rusetect-server v2.2.0
Checking rusetect-vercel v2.2.0
Finished `dev` profile [unoptimized + debuginfo] target(s) in 23.21s
EXIT 0
```

No warning or error lines.

## 3. `python3 tools/pilot_sender.py --self-test`

```
pilot_sender self-test passed
EXIT 0 MS 605
```

## 4. Python tests in `/home/user/nemesis1`

`python3 -m pytest tests/ -q -p no:cacheprovider` (8 test files; `test_scale.py` ran because `redis` and
`redis-server` are installed):

```
...........F..............................................               [100%]
FAILED tests/test_branding.py::test_no_legacy_names_anywhere_in_source
1 failed, 57 passed in 53.28s
```

Cause, read from the assertion: the test walks every file under the repository root, including the git-ignored
`graphify-out/cache/ast/*.json`, and one cached graph node names a file in `docs/research/` (a folder the test
excludes on purpose). The source files themselves are clean. The failure is environmental (a local tool's cache), not
a product bug, but it is a real failure of the command as run and is recorded as such. Fix options are in the roadmap
(stage W0.1): skip git-ignored paths in the test.

This repository has no Python test files (`find . -name 'test_*.py'` outside `target/` finds none).

## 5. Local server smoke test

Server: `target/release/rusetect-server --host 127.0.0.1 --port 8765`, built by run 1, ready 2 s after launch.
Script: a standard-library Python client kept outside the repository (one request per route, plus negative cases).

### 5a. Demo mode (`RUSETECT_DEMO=1`, `RUSETECT_WEBHOOK_SECRET` set, rate limit raised for the test)

All 62 requests returned the expected status (expected/actual shown once where they differ from 200):

| Route | Status | Notes |
|---|---|---|
| `GET /`, `/console`, `/investigate`, `/report`, `/favicon.svg`, `/openapi.json`, `/docs` | 200 each | |
| `GET /api/health` | 200 | `engine_loaded: true`, `hardened: true`, `ruleset: ctx-1.4`, signing `ephemeral: true`, store `memory`, `ready: true` |
| `POST /api/v2/demo/reset?warm=false` | 200 | 139 ms |
| `POST /api/v2/payment` | 200; malformed body 422 | |
| `POST /api/v2/assess` (digital-arrest message, risky session) x2 | 200 | opened alerts A00001, A00002 |
| `POST /api/v2/verify` (signature from assess) | 200 | |
| `GET /api/v2/alerts`, `?status=open`, `/alerts/summary` | 200 | |
| `GET /api/v2/alerts/A00001/evidence`, `/str` | 200; unknown alert 404 | |
| `POST /api/v2/alerts/A00001/review` | 200 (analyst 1 approve_block); 400 (same analyst again); 200 (supervisor second approval); 409 (replayed nonce); 403 (viewer) | maker-checker and nonce work |
| `POST /api/v2/alerts/A00002/customer`, `/reverify` | 200, 200 | |
| `GET /api/v2/campaigns`, `/cases`, `/audit` | 200 | `/cases` returned `[]` (L4 cases, not desk alerts) |
| `POST /api/v2/identity/check`, `/authenticate`, `/challenge/verify` | 200 | |
| `POST /api/v2/duress/register`, `/payments/confirm` | 200 | |
| `POST /api/v2/behaviour/enrol` x3, `/behaviour/check` | 200 | |
| `GET /api/v2/consortium`; `POST /consortium/check`, `/consortium/publish` (analyst key) | 200 | |
| `GET /api/v2/keys/public`, `/demo/analysts`, `/fixtures/mule`, `/message-model`, `/model-card`, `/stats`, `/drift`, `/live` | 200 | |
| `POST /api/v2/message` | 200; empty text 422 | |
| `POST /api/v2/sanctions/screen`, `/payee/check`, `/payments/batch`, `/onboard` | 200 | |
| `POST /api/v2/events/payment` | 200; replay of the same event 200 (stored result); bad signature 401 | |
| `POST /api/v2/demo/next?n=200` | 200 | 14 ms |
| `POST /api/v2/demo/reset` after a live event | 409 | reset refused so live events are kept |
| `GET /api/nope` | 404 | |

Latency of the in-process requests was 0.3 to 28 ms except the first `/v2/assess` (266 ms, model warm-up) and the
demo reset (139 ms). These are single requests on loopback, not a benchmark.

### 5b. Configured keys (`RUSETECT_ANALYST_KEYS` with viewer, channel, analyst and supervisor keys, plus the three production secrets)

| Check | Expected | Actual |
|---|---|---|
| `GET /api/health`, no key | 200 | 200 |
| `GET /api/v2/alerts`, no key | 401 | 401 |
| `GET /api/v2/alerts`, viewer | 200 | 200 |
| `POST /api/v2/payment`, viewer | 403 | 403 |
| `POST /api/v2/payment`, channel | 200 | 200 |
| `POST /api/v2/demo/reset`, analyst | 403 | 403 |
| `GET /api/v2/demo/analysts` | 200 with `[]` | 200 |
| a published demo key | 401 | 401 |
| `GET /console`, no key | 200 (pages are public) | 200 |

## 6. Not covered by any test today

- No frontend test of any kind: no check that page buttons, forms, filters or charts call the right routes.
- No end-to-end browser test; no screenshot baseline.
- No Rust test calls `/v2/cases`, `/v2/model-card` or `/v2/message-model` (smoke only).
- `mouse_dynamics.rs` has unit tests but no route, so it is untested as a service.
- The deployed Vercel site was not reachable from this sandbox.
- Performance: not re-measured in this stage. The latest committed benchmark is `results/e2e_bench_s1.json`
  (git head `4be9aeb`, before the PR #37 review fixes); its own gate records `"pass": false` because
  `POST /api/v2/payment` at concurrency 64 regressed (3712.43 to 3269.37 requests/s).
