# Execution flow

How a request moves through RuseTect, which function calls which, and what changed in the November 2026 review
session. Paths are relative to the repository root.

## Entry points

| Binary | File | Starts with | Used for |
|---|---|---|---|
| `rusetect-server` | `crates/rusetect-server/src/main.rs` | `main()` | HTTP API and pages, local or Docker |
| Vercel function | `api/rusetect.rs` | `main()` | the same API as one serverless function (`vercel.json` routes every path to it) |
| `rusetect-worker` | `crates/rusetect-server/src/bin/worker.rs` | `main()` → `scale::worker::run` | scoring one Redis stream partition |
| `rusetect-replay` | `crates/rusetect-server/src/bin/replay.rs` | `main()` → `scale::producer::publish` | streaming a payments CSV into Redis |
| `research_dump` | `crates/rusetect/examples/research_dump.rs` | `main()` | research: streams any dataset through the engine features and models, writes CSV |
| `score_bench`, `profile`, `parity_dump`, `text_dump` | `crates/rusetect/examples/` | `main()` | speed and Python parity checks |
| Research scripts | `research/*.py` | `main()` | evaluations; each writes `research/out/<name>.json` |

## Server start-up (order)

1. `rusetect_server::check_config()` (`crates/rusetect-server/src/lib.rs`): refuses to start unless
   `RUSETECT_ANALYST_KEYS` (plus `RUSETECT_SIGNING_KEY`, `RUSETECT_CONSORTIUM_SECRET`, `RUSETECT_HASH_SALT`) or
   `RUSETECT_DEMO=1` is set. Calls `missing_production_secrets()`.
2. `rusetect_server::ensure_root()`: if the repository files are not next to the binary, unpacks the embedded copy
   (`ASSETS`, built by the `assets!` macro) to a temp folder and sets `RUSETECT_ROOT`.
3. `new_shared()` → `AppState::load()`:
   - `Engine::new(models_dir, true)` (`crates/rusetect/src/engine.rs`)
     - `CascadeConfig::load()` reads `service/models/cascade_config.json` (features, thresholds, codes)
     - `Models::load()` reads the L1 XGBoost, L2 XGBoost and LightGBM, and the L0 onboarding model
     - `Hardening::load()` reads `service/models/hardening.json` and `hardening_xgb.json` (format-free second opinion)
   - `MessageModel::load()` (scam text), typing verifier, consortium, signer, review desk
   - `RateLimiter::new(RUSETECT_RATE_LIMIT or 240, 10 s)`
4. `build_app()` builds the axum router: pages (`/`, `/console`, `/investigate`, `/report`, `/research`, `/docs`)
   and `/api/...` routes, wrapped in the `rate_limit` middleware and CORS.
5. `axum::serve` (server) or `vercel_runtime::run` (Vercel).

## A payment request: `POST /api/v2/payment`

```
rate_limit (middleware)                       per-client token bucket; X-Forwarded-For trusted only on Vercel or RUSETECT_TRUST_PROXY=1
 └─ v2_payment
     ├─ parse_obj(PaymentIn::parse)           inputs.rs: required fields; format and currencies must be known values (422 otherwise)
     ├─ payment_of → Payment                  timestamp parsed to minutes
     └─ with_state → score_with_context       one lock, requests applied in order
         ├─ capacity check                    MAX_ACCOUNTS (RUSETECT_MAX_ACCOUNTS, default 2,000,000): 503 if a new account would exceed it
         ├─ Engine::score_payment
         │   ├─ features(p)                   31 causal account-history features; updates account state
         │   ├─ vec(f)                        feature vector in model order
         │   ├─ L1 XGBoost gate               below l1_threshold: exit with risk = L1 score
         │   ├─ else L2 = (XGBoost + LightGBM) / 2
         │   └─ finish_risk                   (new this session)
         │       ├─ pair window               noisy-OR of this payer→payee pair's cascade risks over 24 h
         │       ├─ Hardening::risk           max(iso_served(cascade or pair risk), iso_free(format-free XGBoost))
         │       ├─ band_for_risk             verify / hold / block on the hardened thresholds (thresholds())
         │       ├─ reasons                   only for flagged payments: SHAP of the model that set the risk
         │       │                            (cascade_reasons or free_reasons) plus a "repeated payments" chip
         │       └─ link                      case graph when the band is high
         └─ lures.apply_to_payment            a recent risky message can raise (never lower) the decision
```

`POST /api/v2/assess` adds the context layer: `context_rules::assess` on the message and payment context, then
`combine` takes the stricter decision. `POST /api/v2/payments/batch` loops `score_with_context`.

## Redis scale path

`rusetect-replay` → `producer::publish` (partition by payer hash) → Redis streams →
`rusetect-worker` → `worker::run` → `Scorer::score` (features from `redis_state`, L1, L2, `Hardening::risk`,
`Engine::thresholds`, then `context_rules::assess` and `combine`). The pair aggregation is not in this path yet.

## Research pipeline

```
raw dataset ──converter (research/card_convert.py, samld_convert.py)──▶ AMLworld-layout CSV
          ──research_dump (Rust engine, optional genuine-row sampling)──▶ research/out or scratch *.csv.gz (+ .json totals)
          ──research/*.py evaluation──▶ research/out/*.json
          ──research/build_report.py──▶ service/static/research.html, docs/MODEL_CARD.md
          ──research/make_pdf.js──▶ docs/research/*.pdf
```

Adoption scripts write served model files only when their pre-registered rule passes:
`hardened_eval.py` and `pooled_hardening.py` write `service/models/hardening.json` and `hardening_xgb.json`.

## What changed in this session (Claude)

| Area | File | Change |
|---|---|---|
| Config | `crates/rusetect-server/src/lib.rs` | `missing_production_secrets` and `check_config`: fail closed without keys or `RUSETECT_DEMO=1` |
| Capacity | `crates/rusetect-server/src/lib.rs` | `MAX_ACCOUNTS` check in `score_with_context`; `Engine::knows` |
| Health | `crates/rusetect-server/src/lib.rs` | `hardened` flag; hardening files embedded in `ASSETS` |
| Scoring | `crates/rusetect/src/engine.rs` | `Hardening`, `interp`, `thresholds`, `finish_risk` (pair window and hardening), `cascade_reasons`, `free_reasons`; SHAP only for flagged payments |
| Speed | `crates/rusetect/src/models/xgb.rs` | TreeSHAP sums into one buffer per thread |
| Workers | `crates/rusetect-server/src/scale/worker.rs` | hardened risk and thresholds |
| Research harness | `crates/rusetect/examples/research_dump.rs` | streaming, gzip output, 13 extra indicators, genuine-row sampling with `.json` totals |
| Benchmarks | `crates/rusetect/examples/score_bench.rs` | end-to-end scoring speed |
| Validation | `crates/rusetect-server/src/inputs.rs`, `validate.rs` | required `payment_format`; format and currency lists (earlier in the session) |
| Tests | `crates/rusetect-server/tests/api.rs` | fail-closed config, hardened risk ignores format, pair risk adds up, validation lists |
| Research | `research/*.py` | full HI-Small, transfer, unified training, other datasets, hardening, pooled hardening, structuring, evasion suite |
| Docs | `docs/MODEL_CARD.md`, `docs/PILOT.md`, `docs/decision.md`, `docs/Flow.md`, README | generated or new |
| Deploy | `deploy/docker-compose.yml`, `run_local.ps1`, Vercel env `RUSETECT_DEMO=1` | explicit demo mode |
