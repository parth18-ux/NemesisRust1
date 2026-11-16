# Website implementation map (Stage W0 audit)

Audit of `main` at `aa9121f` (16 Nov 2026), done before any website change. It covers brief sections 1 and 4: what
exists, how it is wired, and what the planned website sections A to I can and cannot show today. Test evidence is in
`docs/BASELINE_TEST_REPORT.md`; the plan is in `docs/IMPLEMENTATION_ROADMAP.md`.

Ground rules used here:

- A capability counts as tested only if a test in this repository (or the Python repo `/home/user/nemesis1`) runs
  it and the run in `docs/BASELINE_TEST_REPORT.md` passed. "Smoke-tested" means one request of the local smoke test
  (same report, section 5) got the expected status, which is weaker evidence.
- Every metric quoted is copied from a file in `results/`, `research/out/` or `service/models/`, and the file is named.
- The product is named **RuseTect** throughout the code (`crates/rusetect/src/branding.rs`: `NAME = "RuseTect"`,
  `VERSION = "2.2.0"`), and a test (`no_legacy_names_anywhere_in_source`, plus the branding checks) ties the static pages
  to that name. The brief calls the product NEMESIS. Renaming the UI is a product decision for the user (see the
  roadmap, stage W2), not something this audit assumes.

## 1. Classification summary

| # | Capability | Classification | Evidence |
|---|---|---|---|
| 1a | Payment cascade (L1 gatekeeper, L2 XGBoost + LightGBM, decision bands, hardening) | Fully implemented and tested | `tests/api.rs`, `tests/live.rs`, `tests/motifs.rs`, engine unit tests |
| 1b | Context layer (message, session, network rules `ctx-1.4`) on `/v2/assess` | Fully implemented and tested | `tests/api.rs` (`digital_arrest_end_to_end` and others) |
| 1c | Temporal-motif reason codes | Fully implemented and tested | `tests/motifs.rs` (4 tests) |
| 1d | Batch scoring `/v2/payments/batch` | Fully implemented and tested | `tests/api.rs` (body-limit and validation tests), smoke |
| 1e | Signed live ingest webhook `/v2/events/payment` (memory and Redis) | Fully implemented and tested | `tests/live.rs` (14 tests, Redis included), smoke |
| 1f | Drift monitor `/v2/drift` | Fully implemented and tested | `tests/api.rs` drift tests; known limit: demo traffic feeds it |
| 1g | L0 onboarding gate `/v2/onboard` | Fully implemented and tested | `tests/api.rs::onboarding_gate` |
| 1h | L4 "cases" `/v2/cases` (engine case list) | Implemented but untested | no test calls the route; smoke only; returns `[]` after `/v2/assess` because it lists the engine's L4 cases, not desk alerts |
| 1i | Transaction search, transaction detail by id, beneficiary history | Not implemented | no transaction store; the engine keeps only per-account aggregates |
| 2a | Scam-message detector `/v2/message` (TF-IDF logistic regression + tactics, workflows, entities) | Fully implemented and tested | `tests/api.rs`, `scam_text` unit tests |
| 2b | Lure fusion (a risky message raises a later payment) | Fully implemented and tested | `lure_fusion` unit tests, `tests/api.rs` |
| 2c | Alert-fatigue summary `/v2/alerts/summary` | Fully implemented and tested | `tests/api.rs` |
| 2d | Message history, search, related cases by message | Not implemented | messages are not stored except as watchlist entities |
| 3a | Keystroke verifier `/v2/behaviour/enrol`, `/v2/behaviour/check` | Fully implemented and tested | `tests/api.rs`, `behaviour` unit tests; profiles in memory only |
| 3b | Session signals (call, screen share, remote app, pasted payee, SIM swap) inside `/v2/assess` | Fully implemented and tested | `tests/api.rs` (session and network tests) |
| 3c | Identity pre-OTP check `/v2/identity/check` | Fully implemented and tested | `tests/api.rs` |
| 3d | PRAMAAN account-takeover check `/v2/authenticate`, `/v2/challenge/verify` | Fully implemented and tested | `tests/api.rs::authenticate_owner_and_sim_swap` and others |
| 3e | Duress PIN `/v2/duress/register`, `/v2/payments/confirm` | Fully implemented and tested | `tests/api.rs` |
| 3f | Mouse dynamics (`mouse_dynamics.rs`, ExtraTrees per user) | Partially implemented | Rust module and unit tests exist, evaluation in `results/balabit_eval.json`, but no API route serves it |
| 4a | Coordination rule (k flagged payers into one payee) in `/v2/assess` `network` | Fully implemented and tested | `tests/api.rs` |
| 4b | Consortium mule registry (HMAC identifiers, Ed25519-signed, hash-chained ledger, Merkle root) | Fully implemented and tested | `tests/api.rs` (forgery tests), `consortium` unit tests |
| 4c | Sanctions screening `/v2/sanctions/screen` | Fully implemented and tested | `tests/api.rs::sanctions_screen_finds_variants_and_validates` |
| 4d | Payee name check `/v2/payee/check` | Fully implemented and tested | `tests/api.rs` |
| 4e | Linked alerts in the evidence pack | Fully implemented and tested | `tests/api.rs::alert_review_audit_evidence` |
| 4f | Entity graph (accounts, devices, customers, merchants as nodes; typed edges) as an API | Not implemented | the only graph is drawn in the browser (`investigate.html`, `ring()`), from alert payer/payee pairs |
| 5a | Campaign linking `/v2/campaigns` (shared payee, message identifiers, script similarity) | Fully implemented and tested | `tests/api.rs::campaigns_use_only_the_newest_alerts`, STR test uses it |
| 6a | Rust serving layer (axum, 41 API routes, 7 page routes) | Fully implemented and tested | 133 Rust tests passed; 62/62 smoke requests |
| 6b | Auth: demo keys, configured keys, 4 roles, nonce, rate limit, body limit, security headers | Fully implemented and tested | `security` unit tests, `tests/api.rs`; configured-key smoke (section 5b of the test report) |
| 6c | Redis durable store and scale-out workers | Fully implemented and tested | `tests/live.rs`, `tests/scale.rs` (Redis ran locally) |
| 6d | Vercel deployment | Implemented but untested here | `vercel.json` + `api/rusetect.rs`; the live site was not reachable from this sandbox (proxy refused) |
| 7a | Python API port (`/home/user/nemesis1/service/server.py`) | Fully implemented and tested, with one failing test | 57 of 58 pytest tests pass; the failure is caused by git-ignored `graphify-out/` cache files (test report section 4) |
| 7b | Python research and evaluation scripts (`research/*.py`, `nemesis1/ml/*.py`) | Implemented but untested | outputs committed in `research/out/` and `results/`; no unit tests for the scripts |
| 8a | Alert desk: open, release, verify, hold, two-analyst block (maker-checker), customer answer, re-verify | Fully implemented and tested | `tests/api.rs`, `review` unit tests, smoke |
| 8b | Assignment, comments, priority, SLA, case entity that groups alerts | Not implemented | `ReviewDesk` alerts have `status`, `actions`, `block_approvals` only |
| 9a | Evidence pack with content hash and Ed25519 signature, `/v2/verify`, `/v2/keys/public` | Fully implemented and tested | `tests/api.rs` verifies the signature |
| 9b | Hash-chained audit log `/v2/audit` (`chain_valid`) | Fully implemented and tested | `review` unit tests, `tests/api.rs` |
| 10a | STR draft `/v2/alerts/{id}/str` (template, FIU-IND sections A to F) | Fully implemented and tested | `tests/api.rs::digital_arrest_end_to_end` |
| 10b | Benchmark report page `/report`, model card `/v2/model-card`, message-model card | Implemented but untested | pages are static HTML built in the Python repo; no test calls `/v2/model-card` or `/v2/message-model` (smoke only) |
| 10c | Printable/exportable case report (PDF or HTML) | Not implemented | |
| 10d | Console replay `/v2/demo/reset`, `/v2/demo/next` | Demo/mock only (real models on recorded synthetic IBM AMLworld data) | `tests/api.rs`; labels used only to report accuracy |
| 10e | Investigator mule fixture `/v2/fixtures/mule` | Demo/mock only | fictional fixture `fixtures/mule_replay.json` |
| -- | Broken | none found | every route answered as expected locally; see the risks in section 6 |

Counts over the 41 rows: 29 fully implemented and tested (7a with one failing Python test caused by local files),
4 implemented but untested (6d untested from this sandbox), 1 partially implemented, 2 demo/mock only, 0 broken,
5 not implemented.

## 2. Components (brief section 4)

Paths are relative to the repository root. `S` = `crates/rusetect-server/src`, `R` = `crates/rusetect/src`.
All API routes are under `/api` (route table: `S/lib.rs` lines 297 to 346). Auth column: in demo mode
(`RUSETECT_DEMO=1`, which the public site uses) every route is open. With configured keys (`RUSETECT_ANALYST_KEYS`)
the rule is `R/security.rs::gate`: public routes need no key; any role may GET; a POST needs `channel`, `analyst` or
`supervisor`; review and consortium publish need `analyst` or `supervisor`; `/v2/demo/*` needs `supervisor`.
Alerts, evidence, STR, cases and audit also need a key of any role (`require_reader`).

### 2.1 Payment-risk engine

- **Source**: `R/engine.rs` (cascade, account state, reasons, motifs), `R/models/{lgbm,xgb}.rs`, `R/payment.rs`,
  `R/context_rules.rs` (ruleset `ctx-1.4`), `R/risk_score.rs`, `R/drift.rs`, `S/inputs.rs`, `S/lib.rs`
  (`v2_payment`, `assess`, `v2_batch`, `v2_drift`, `v2_stats`, `v2_cases`, `v2_onboard`), `S/live.rs`, `S/scale/*`.
- **Models** (`service/models/`): `l1_gatekeeper.json`, `l2_xgb.json`, `l2_lgbm.json`, `hardening_xgb.json`
  (+ `hardening.json`), `cascade_config.json`, `l0_onboarding_lgbm.json` + `l0_config.json`, `drift_reference.json`.
- **Model version**: no explicit version string on the payment models. Provenance fields only:
  `cascade_config.json.trained_on` = "IBM AMLworld HI-Small + LI-Small train weeks (first 60% of each, by time), live
  path features"; `hardening.json.note` = "written by research/pooled_hardening.py; risk = max(iso_served(cascade),
  iso_free(pooled 29-feature XGBoost))"; context ruleset `ctx-1.4`; API version `2.2.0`. Missing: a model hash or
  version in each response.
- **Endpoints and auth**:

| Route | Method | Auth (configured keys) | Input | Output (top-level keys, from the smoke run) |
|---|---|---|---|---|
| `/v2/payment` | POST | channel+ | `timestamp` ("YYYY-MM-DD HH:MM"), `from_bank`, `from_account`, `to_bank`, `to_account`, `amount_paid`, `payment_format`, optional `payment_currency` (default "US Dollar"), `receiving_currency` | `l1_score, features, layer_exit, risk, decision, band, reasons, case_id, cascade_risk, format_free_risk, latency_ms, lure_context` |
| `/v2/assess` | POST | channel+ | the payment fields (flat) + `message`, `consent`, `use_intent_model`, `session{on_call, video_call, call_minutes, screen_share, remote_access_app, payee_pasted, typing_anomaly, device_hash, sim_swap_hours, events[] ...}` | `payment, context, payment_decision, context_band, final_decision, raised_by_context, network, message_model, risk_score, intervention, alert_id, shown_decision, time_to_decision_ms, signature` |
| `/v2/payments/batch` | POST | channel+ | JSON list of payments (up to 20,000; 2 MiB body limit) | per-payment results |
| `/v2/events/payment` | POST | public route, HMAC header `X-RuseTect-Signature: v1=<hex>` + `X-RuseTect-Timestamp` | payment fields + `event_id` | decision, alert, ledger entry; replays return the stored result |
| `/v2/live` | GET | any | none | `durable, store, events` |
| `/v2/drift` | GET | any | none | `status, warming_up, reasons, window, score_psi, score, features, bands, top_psi, thresholds, reference, scope` |
| `/v2/stats` | GET | any | none | `payments_seen, accounts_tracked, decisions, open_cases` |
| `/v2/cases` | GET | reader | `?limit` | engine L4 case list |
| `/v2/onboard` | POST | channel+ | Feedzai BAF application fields | flag, score, reasons |

- **Integration status**: served in Rust; used by `/console` (payment form, replay, live feed, drift) and
  `/investigate` (assess). With Redis, the webhook scores through the stateless scale path, which skips the message,
  session, lure, per-feature and motif reasons (`docs/live.md`).
- **Detection quality, as measured** (for the website's honesty labels): served PR-AUC 0.72 on HI-Small validation
  (`research/out/served_eval.json`, `all.pr_auc.served`); on a fresh IBM AMLSim holdout PR-AUC 0.0024 and verdict
  "does not transfer" (`research/out/holdout_amlsim.json`); known evasion weaknesses `timing_jitter` (PR-AUC drop
  0.1801) and `new_payee` (0.0865) (`research/out/evasion_validation.json`).
- **Missing for the website**: a transaction store with search and filters, transaction detail by id, beneficiary
  history, a per-account timeline, model version and hash in each result, a transaction-level "related accounts" query.

### 2.2 Scam-message detection

- **Source**: `R/scam_text.rs`, `R/pyre.rs` (regex), `R/lure_fusion.rs`, `S/lib.rs` (`v2_message`,
  `v2_message_model`, `v2_alert_summary`).
- **Model**: `service/models/message_model.json`: "logistic regression on hashed TF-IDF (word 1-2 grams + char 3-5
  grams), Indian rows weighted x4", trained on "7365 messages: UCI SMS Spam Collection + curated Indian scam/legit
  corpus (synthetic)" (`meta`). No version string. Card: `results/message_model_eval.json` (served at
  `/v2/message-model`).
- **Endpoints**: `POST /v2/message` (channel+): `text` (1 to 5,000 chars), optional `channel` (default sms),
  `timestamp`, `bank`, `account` -> `risk, band, action, workflow, other_workflows, tactics, key_phrases,
  hard_signals, model_risk, entities, alert, channel`. `GET /v2/message-model`, `GET /v2/alerts/summary`.
- **Integration**: console "message" panel; `/v2/assess` takes `message` with `consent`; text is redacted before it is
  stored or returned.
- **Missing**: stored message history and search, a link from a message to the alerts it raised (only campaigns link
  by script similarity), real-world Indian evaluation data (the Indian rows are synthetic).

### 2.3 Behavioural intelligence

- **Source**: `R/behaviour.rs` (typing verifier), `R/session_signals.rs`, `R/pramaan.rs`, `R/intervention.rs`
  (duress vault and interventions), `R/mouse_dynamics.rs` + `R/extra_trees.rs` (not served).
- **Models**: `typing_verifier.json` (LightGBM, "Aalto 136M Keystrokes, 2,867 random participants", threshold rule "5%
  false alarms on genuine 1-sentence attempts of validation participants"); `auth_xgb.json` + `auth_config.json`
  (PRAMAAN, `version: auth-sim-s7`, trained on "ml.auth.sim (synthetic) + HMOG touch").
- **Endpoints**: `POST /v2/behaviour/enrol` and `/v2/behaviour/check` (`account`, `digraphs{pair: ms}`);
  `POST /v2/identity/check` (`device_hash`, `enrolled_device_hashes`, `sim_swap_hours`); `POST /v2/authenticate`
  (`user_id`, `ts`, optional `device`, `network`, `behaviour`, `transaction`) -> allow/challenge/block with reasons in
  English and Hindi and tree-SHAP signals; `POST /v2/challenge/verify` (`decision_id`, `passed`);
  `POST /v2/duress/register`, `POST /v2/payments/confirm`. Session signals ride in `/v2/assess` `session`.
- **Integration**: `/investigate` uses identity, duress and session; the console shows none of the behavioural routes.
- **Missing**: a mouse-dynamics route; persistence of typing profiles and PRAMAAN history (memory only, per instance);
  any aggregate view (counts of challenges, typing anomalies) for a dashboard; consent records.

### 2.4 Entity and account linking

- **Source**: `R/coordination.rs`, `R/consortium.rs`, `R/sanctions.rs`, `R/payee.rs`, `R/review.rs` (linked alerts in
  the evidence pack), `service/static/investigate.html` (`ring()`, client-side component graph).
- **Endpoints**: `GET /v2/consortium` (`members, entries, valid, merkle_root`), `POST /v2/consortium/check`
  (`registry`, `identifier`), `POST /v2/consortium/publish` (analyst+, nonce not needed), `POST /v2/sanctions/screen`
  (`name`) -> `matches, hit, threshold`, `POST /v2/payee/check` (`entered_name`, `account_name`, `account_type`) ->
  `verdict, score, reason, suggested_name`. Coordination appears as `network` in `/v2/assess`.
- **Model version**: sanctions matcher v1 or v2 chosen at start-up from `research/out/sanctions_eval_v2.json`
  (`adopted`), payee check v1 or v2 from `payee_check_eval_v2.json`; list `service/models/sanctions_sdn.csv`.
- **Missing**: a server-side graph endpoint (nodes, typed edges, amounts, timestamps, evidence per edge), device and
  customer entities, time-range queries. The browser graph is a frontend calculation over the alert list, which the
  brief forbids as the source of truth for the network view.

### 2.5 Campaign detection

- **Source**: `R/campaigns.rs` (union-find over shared payee, payee identifiers hashed out of messages, and 5-char
  shingle Jaccard >= 0.5 on redacted scripts; newest alerts only).
- **Endpoint**: `GET /v2/campaigns` (any role; open in demo) -> list of `campaign_id, alerts, size, victims, payees,
  linked_by, total_amount, first_seen, last_seen, sample_script, statuses`.
- **Integration**: `/investigate` campaign panel; the STR draft cites the campaign.
- **Missing**: persistence (campaigns are rebuilt from in-memory alerts), campaign detail by id, emerging-pattern
  trend over time.

### 2.6 Rust serving layer

- **Source**: `S/lib.rs` (router, middleware, handlers), `S/live.rs`, `S/validate.rs`, `S/inputs.rs`,
  `S/openapi.rs`, `S/main.rs`, `api/rusetect.rs` (Vercel), `S/scale/*` + `bin/worker.rs`, `bin/replay.rs`.
- **Pages**: `/`, `/console`, `/investigate`, `/report`, `/favicon.svg`, `/openapi.json`, `/docs`.
- **State**: one `Mutex<AppState>` per process; memory store by default, Redis (`REDIS_URL` or `KV_URL`) for the live
  path. Models and pages are embedded in the binary (`ASSETS`) and unpacked to a temp folder on a cold start.
- **Security**: fail-closed start-up (`check_config`), constant-time key lookup, `X-Nonce` replay guard on reviews,
  per-client rate limit (default 240 per 10 s), 2 MiB body limit, `nosniff`, `X-Frame-Options: DENY`,
  `no-referrer`, CORS from `CORS_ORIGINS` (default `*`).
- **Missing**: no Content-Security-Policy; `/openapi.json` (`S/openapi.rs`, 151 lines) is hand written and may lag the
  routes; no versioned UI-facing aggregate endpoints.

### 2.7 Python model pipelines

- **This repo**: `research/*.py` (34 scripts: evaluations, hardening, drift reference, sanctions and payee evals,
  unified training), outputs in `research/out/`; `research/build_report.py` and `research/make_pdf.js` build the
  research page and PDF; `tools/pilot_sender.py` (self-test passes), `tools/pilot_rehearsal.py`.
- **Sister repo** `/home/user/nemesis1`: the original FastAPI service (`service/server.py` and 18 other modules, the Python
  originals of every Rust module), `ml/*.py` (47 training and benchmark scripts that produced `service/models/*`),
  8 pytest files. `/report` and `index.html` cite `ml/*.py` paths that exist only there (`ml/` here holds
  `ml/exported/` only).
- **Duplication**: every serving module exists twice (Python and Rust) with parity tests; `research/build_report.py`
  exists in both repos; the static pages are copied between repos.
- **Missing**: tests for the research scripts; one command that rebuilds the served models from data in this repo.

### 2.8 Analyst workflow

- **Source**: `R/review.rs` (`ReviewDesk`), `R/security.rs` (roles, nonce), `R/intervention.rs`, `S/lib.rs`
  (`v2_alerts`, `v2_review`, `v2_customer`, `v2_reverify`, `v2_audit`), `S/live.rs` (Redis-backed alerts).
- **States**: `open`, `held`, `customer_verification`, `block_pending_second_approval`, `blocked`, `released`, plus
  customer and system states such as `stopped_by_customer`, `customer_reverified`.
- **Endpoints**: `GET /v2/alerts?status=` (reader) -> list of `alert_id, status, shown_decision, payment, assessed,
  actions, block_approvals`; `POST /v2/alerts/{id}/review` (analyst+, headers `X-Analyst-Key`, `X-Nonce` of 8 to 128
  chars; body `action` in `approve_block|hold|release|verify`, `note` up to 500 chars); `POST
  /v2/alerts/{id}/customer` (`answer`); `POST /v2/alerts/{id}/reverify`; `GET /v2/audit` -> `entries, chain_valid`.
- **Dual approval**: genuine. A block needs two different analysts (smoke: same analyst twice -> 400); the model never
  blocks on its own; an approved block publishes the payee to the consortium ledger.
- **Missing**: assignment, separate comments, priority, SLA timers, a case object that groups alerts, attachments,
  pagination and search on alerts, concurrent-edit locking across instances (last write wins, `docs/live.md`).

### 2.9 Evidence generation

- **Source**: `R/review.rs::evidence` (pack with `pack_hash` = SHA-256 of canonical JSON), `R/security.rs::Signer`
  (Ed25519), `R/consortium.rs` (Merkle root).
- **Endpoints**: `GET /v2/alerts/{id}/evidence` (reader) -> `alert, linked_alerts, audit_entries, disclaimer,
  pack_hash, signature`; `POST /v2/verify` (`signed_fields`, `signature`) -> `valid`; `GET /v2/keys/public`.
- **Caveat**: without `RUSETECT_SIGNING_KEY` the key is per process (`health.signing.ephemeral = true`, as in demo
  mode). On Vercel each instance has its own key, so a signature made on one instance can fail on another. The UI must
  show "verified" only from a `/v2/verify` call that returned `valid: true`.
- **Missing**: a server-side pack verification route that recomputes `pack_hash`; a downloadable signed bundle.

### 2.10 Reporting

- **Source**: `R/str_report.rs` (template, "not a language model"), `service/static/report.html` (static benchmark
  report), `S/lib.rs::v2_model_card` (reads `service/models/benchmark_results.json`).
- **Endpoints**: `GET /v2/alerts/{id}/str` (reader) -> `status` ("DRAFT ..."), `summary`, `sections`
  (`A_reporting_entity`, `B_transactions`, `C_linked_accounts_and_alerts`, `D_grounds_of_suspicion`,
  `E_action_taken`, `F_attachments`), `disclaimer`; `GET /v2/model-card` -> `dataset, protocol, aml_models,
  baf_models, l0_final, cascade, online_parity`.
- **Missing**: a case report that joins alert, evidence, STR, audit and analyst notes into one printable document;
  labelled narrative sections (fact, model, rule, interpretation, analyst conclusion).

## 3. Website sections A to I

| Section | Backend data that exists | Gaps that block it |
|---|---|---|
| A. Executive dashboard | `/v2/stats` (payments seen, accounts, decisions, open cases), `/v2/alerts` (status counts derivable), `/v2/alerts/summary` (message volumes), `/v2/campaigns` (count), `/v2/drift` (status), `/health` (engine, store, signing, ready), `/v2/model-card` (offline detection performance) | no time-bucketed activity series; no confirmed-fraud source except demo-replay labels (`demo/next` `truth`); counts are per instance on Vercel; no single summary endpoint, so the page would aggregate in the browser |
| B. Transaction intelligence | per-payment risk, band, reasons, features, layer exit (`/v2/payment`, `/v2/assess`), live feed `/v2/live`, replay feed | no stored transactions, no search or filter, no detail by id, no beneficiary history or account timeline, no model version in responses |
| C. Scam intelligence | `/v2/message` (band, workflow, tactics, key phrases, entities), `/v2/message-model` (card), lure watchlist summary, campaigns | no message history or search; related cases only through campaigns; Indian training rows are synthetic (must be labelled) |
| D. Behavioural | typing check, session signals, identity check, PRAMAAN, duress | mouse dynamics not served; no aggregates; profiles not persisted; device and session data must be marked consent-based |
| E. Network graph | alert payer and payee pairs, evidence `linked_alerts`, campaigns `linked_by`, coordination counts, consortium flags | no graph endpoint, no typed edges with evidence, no device or customer nodes, no time-range query; the current graph is computed in the browser |
| F. Alert management | `/v2/alerts` with status filter, review actions with roles, nonce and maker-checker, `assessed` detail with reasons and risk score | no assignment, priority, typology field, search, sort or pagination on the server; no "create case" |
| G. Case management | alert actions history, audit chain, evidence pack, customer path | no case entity, comments, attachments, SLA, investigator assignment or progress; dual approval exists only for blocks |
| H. AI-assisted investigation | evidence pack, STR template (every sentence traceable), reasons, campaigns | no narrative endpoint that labels fact, model output, rule, interpretation and analyst conclusion; no paid LLM may be used |
| I. Reports and evidence | evidence pack + signature + `/v2/verify`, audit `chain_valid`, STR draft, consortium Merkle root, benchmark report | no combined printable case report; no server-side pack re-hash check; ephemeral demo signing key |

## 4. Frontend as it is

- **Architecture**: four self-contained static HTML pages in `service/static/` (`index.html` 565 lines,
  `console.html` 480, `investigate.html` 498, `report.html` 440), each with its own inline `<style>` and inline
  `<script>`; no build step, no framework, no third-party JS. Pages are served by the Rust binary (`page()` in
  `S/lib.rs`) and embedded in it. `vercel.json` rewrites every path to the one Rust function.
- **Fonts** (loaded from Google Fonts on all four pages, same URL):
  `https://fonts.googleapis.com/css2?family=Instrument+Serif:ital@0;1&family=Geist:wght@400;500;600&family=Geist+Mono:wght@400;500&display=swap`
  - Body and UI: **Geist** (400, 500, 600), token `--sans`.
  - Headings (h1, h2): **Instrument Serif** (regular and italic), token `--serif` (`--display`).
  - Numbers, labels, pills, IDs: **Geist Mono** (400, 500), token `--mono`.
  - The user wants to keep these fonts. Keep the three families, the same weights and the same fallbacks.
- **CSS tokens, exactly as written** (`console.html` lines 14 to 21; `investigate.html` and `report.html` are
  identical; `index.html` has the subset shown after):

```css
:root{
  --paper:#ffffff; --wash:#f5f4ef; --olive:#3c4a14; --olive-ink:#f3f5ea; --sage:#8e9b74; --sage-deep:#6f7c57;
  --bg:#f5f4ef; --panel:#ffffff; --ink:#111111; --ink-2:#5f5f5c; --ink-3:#8d8d88; --rule:#e7e6e1;
  --accent:#3c4a14; --accent-soft:#e8ecda; --bad:#b3261e; --bad-soft:#f6e1de; --warn:#8a5a00; --warn-soft:#f3e9d4; --ok:#4d6b1f; --ok-soft:#e5edd5;
  --f-boost:#3c4a14; --f-classic:#6f7c57; --f-deep:#8a6d3b; --f-graph:#a8641c; --f-unsup:#b5b4ad;
  --serif:"Instrument Serif","Iowan Old Style","Palatino Linotype",Georgia,serif;
  --sans:"Geist",ui-sans-serif,system-ui,-apple-system,"Segoe UI",sans-serif;
  --display:var(--serif); --body:var(--sans); --mono:"Geist Mono",ui-monospace,SFMono-Regular,Menlo,monospace;
```

`index.html` (lines 21 to 26):

```css
:root{
  --paper:#ffffff; --ink:#111111; --ink-2:#5f5f5c; --ink-3:#8d8d88; --rule:#e7e6e1; --wash:#f5f4ef;
  --olive:#3c4a14; --olive-ink:#f3f5ea; --sage:#8e9b74; --sage-deep:#6f7c57; --alert:#b3261e;
  --serif:"Instrument Serif","Iowan Old Style","Palatino Linotype",Georgia,serif;
  --sans:"Geist",ui-sans-serif,system-ui,-apple-system,"Segoe UI",sans-serif;
  --mono:"Geist Mono",ui-monospace,SFMono-Regular,Menlo,monospace;
```

  Shared rules worth keeping: `h1{font:400 clamp(36px,4.8vw,56px)/1 var(--serif);letter-spacing:-.02em}`, `h2` serif
  400, buttons pill-shaped (`border-radius:999px`), primary button olive, panels `border-radius:16px`, focus ring
  `2px solid var(--olive)`, `font-variant-numeric:tabular-nums` on numbers, reduced-motion rule. There is no dark mode.
  The palette is olive and sage on warm white; the brief asks for navy or charcoal navigation with cyan, teal and blue
  accents, which is a token change, not a font change.
- **Duplication**: the token block, site nav and base rules are copied into every page (no shared stylesheet).

## 5. Hardcoded demo values, duplicated code, unused modules, integrations

- **Hardcoded or demo values**: four published demo keys (`R/security.rs::DEMO_KEYS`, served by
  `/v2/demo/analysts` in demo mode); `THIS_BANK = "BANK-A"` and `Consortium::demo()` member banks; default STR
  reporting entity (`str_report::DEFAULT_ENTITY`); `DEMO_WARM = 100_000` rows of `demo_stream.csv.gz`; the fictional
  `fixtures/mule_replay.json`; `investigate.html` shows amounts with a rupee sign (`₹`) while the replay and the default
  `payment_currency` are US Dollar; `index.html` and `report.html` carry benchmark numbers as static text (generated in
  the Python repo, not read from the API at view time).
- **Duplicated code**: Python and Rust implementations of every service module (intentional parity); page CSS and nav
  copied four times; `research/build_report.py` in both repos.
- **Unused modules**: `R/compact_state.rs` (no reference from any crate); `R/mouse_dynamics.rs` (compiled and
  unit-tested, never routed); `R/extra_trees.rs` only feeds mouse dynamics. `crates/rusetect/examples/*` are research
  dump tools.
- **Two "case" concepts**: `/v2/cases` lists the engine's L4 cases from `/v2/payment` and the replay; review-desk
  alerts from `/v2/assess` are a separate list. A website "case" must not silently merge them.
- **Integrations not reachable here**: the deployed site (`nemesis-rust1.vercel.app`) was refused by the sandbox
  proxy, so this audit could not confirm what production serves.
- **Broken integrations**: none found in the local run.

## 6. Risks and gaps that matter most for the website

1. **No server-side source for three core views.** Transaction search (B), the network graph (E) and case management
   (G) have no backend; building them in the browser would invent data. New read endpoints over existing state come first.
2. **State is per instance and in memory.** On Vercel, alerts, cases, profiles and dashboard counts differ between
   instances and vanish on cold starts unless Redis is set; the dashboard must say which instance and store it shows.
3. **Detection does not transfer and has known evasions.** AMLSim holdout verdict "does not transfer"; `timing_jitter`
   and `new_payee` cut PR-AUC (section 2.1). The UI must show offline metrics with their dataset, not as production accuracy.
4. **Signatures can fail across instances.** The demo signing key is ephemeral per process; a "verified" badge must
   come from `/v2/verify`.
5. **Naming and test coupling.** The code, pages and tests say RuseTect; the brief says NEMESIS. Renaming touches the
   branding tests and both repos. Also, the only failing test today (Python branding scan) fails because it walks the
   git-ignored `graphify-out/` folder.
