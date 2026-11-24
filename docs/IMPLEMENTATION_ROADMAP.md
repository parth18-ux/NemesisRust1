# Implementation roadmap

One roadmap for the whole system, written after the Stage W0 audit (`docs/WEBSITE_IMPLEMENTATION_MAP.md`,
`docs/BASELINE_TEST_REPORT.md`). It delivers brief sections 3 (website), 5 (unified workflow), 6 (testing),
7 (performance) and 8 (security), and folds in the open research and engineering work.

Order, as the user set it: the website first (stages W1 to W8, in the order backend endpoints, design system and
shell, dashboard and alerts, network graph, case workspace, assistant and reports, scam and behavioural modules,
end-to-end tests). After that come the remaining items in the brief's priority order: function and integration
(known limits, porting), detection, data and retraining, integrity and security, performance, and the final reports.

## Rules for every stage

- **Small and testable.** Each stage is one pull request with its own tests, and it ends with the full baseline
  green: `cargo test --release --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `python3 tools/pilot_sender.py --self-test`, the Python suite in `/home/user/nemesis1`, plus the stage's new tests.
- **Nothing already built breaks.** Existing routes, request schemas and response keys stay as they are; new data goes
  in new routes or new keys. `/console`, `/investigate`, `/report` and `/` keep working until the new app replaces
  them, and the old URLs then redirect. The 133 Rust tests are the regression net.
- **One fraud model.** New views read the existing engines (cascade, context layer, message model, verifier, PRAMAAN,
  sanctions, payee check, campaigns, consortium). No second scoring model and no score computed in the browser.
- **Honest data.** Every number on a page comes from an API response or a committed file in `results/` or
  `research/out/`. Demo-replay data, the fictional mule fixture and synthetic training sets are labelled as such.
  Offline metrics are shown with their dataset and split, never as production accuracy.
- **Human in charge.** No automated blocking; a block still needs two analysts; drafts stay drafts.
- **Model tier.** Fable for design, hard bugs and review; Opus for implementation. Each stage names both where both
  are needed: Fable designs or reviews, Opus builds.
- **Commits.** Author Parth Sharma `<parth18-ux@users.noreply.github.com>`, no AI co-author lines; few commits;
  push when the stage is done; production deploys only with the user's approval.

## Frontend approach

Extend the existing static console; no build step and no framework.

- **Why**: the four pages are plain HTML with inline CSS and JS, embedded in the Rust binary and served by one Vercel
  function. They load fast, have no supply-chain surface beyond Google Fonts, and the tests already check them. Nothing
  in the brief needs a bundler: the views are tables, forms, a few charts and one graph.
- **Structure**: one app page `service/static/app.html` with hash routing (`#/dashboard`, `#/transactions`,
  `#/alerts`, `#/alerts/A00012`, `#/network`, `#/cases/...`, `#/scams`, `#/behaviour`, `#/reports`), a shared
  stylesheet `service/static/assets/app.css` (the design tokens, once), and native ES modules in
  `service/static/assets/js/` (`api.js` fetch wrapper with key and nonce handling, one module per view). Served by a
  new route `GET /assets/{*path}` that reads from the embedded `ASSETS` list (paths whitelisted, correct
  `content-type`, long cache with a version query string).
- **Fonts**: keep Geist, Instrument Serif and Geist Mono with the same weights and fallbacks (all three are SIL Open
  Font License). Optionally self-host the WOFF2 files under `/assets/fonts/` so the app works without Google's CDN and
  a strict CSP can drop `fonts.googleapis.com`; the rendered font stays the same.
- **Libraries** (vendored as files under `/assets/vendor/`, version pinned, licence file kept next to them, no CDN at
  run time):
  - Graph: **Cytoscape.js** (MIT): pan, zoom, directed typed edges, layouts, selection, filtering and good performance
    for a few thousand elements. Alternative if it proves too heavy: hand-written SVG with **d3-force** (ISC, a
    BSD-style licence).
  - Charts: hand-written SVG for the handful of bars and lines the dashboard needs (no dependency). If time-series
    density demands it, **uPlot** (MIT).
  - Nothing else. No paid service.
- **Design system**: same fonts; tokens renamed to roles (`--nav-bg`, `--surface`, `--surface-2`, `--ink*`,
  `--accent`, `--risk-high`, `--risk-med`, `--risk-low`, `--info`). Values move to the brief's direction: charcoal or
  navy navigation, white and light-grey surfaces, restrained teal/cyan accent, red/amber/green only for risk. A dark
  theme via `prefers-color-scheme`. Dense tables with tabular numbers in Geist Mono.

## AI-assisted investigation without a paid LLM

There is no paid LLM API. The assistant is a deterministic, evidence-templated narrative generator, extending the
pattern `str_report.rs` already uses ("every sentence is traceable to a field in the evidence pack").

- New Rust module `crates/rusetect/src/narrative.rs` and route `GET /api/v2/alerts/{id}/narrative` (reader). Input:
  the evidence pack (and later the investigation). Output: sections (summary, what happened, model signals, rule
  findings, linked entities, suggested checks, open questions), each a list of statements:
  `{text, kind, sources[], template_id}` where `kind` is one of `observed_fact`, `model_output`, `rule_finding`,
  `interpretation`, `analyst_conclusion`, and `sources` are JSON pointers into the evidence pack plus its `pack_hash`.
- Facts and model outputs are copied from fields; rule findings name the rule and ruleset version; interpretations come
  from a fixed, reviewed template table (for example "funds left within N minutes of arriving" when the motif reason
  says so) and are labelled as interpretation; suggested checks come from a typology-to-checklist table.
  `analyst_conclusion` statements appear only when an analyst wrote them (review notes or comments).
- Same input gives the same output; the response carries `status: "DRAFT - requires analyst review"`, the template
  table version and the pack hash. A test fails if any statement lacks a source.
- **Optional LLM (user-configured, off by default)**: if the operator sets e.g. `RUSETECT_LLM_URL` to a model they run
  or pay for, it may only rephrase the deterministic draft. Its output is validated (every sentence must keep its
  `sources`; new numbers or names are rejected), labelled `ai_generated_interpretation`, shown side by side with the
  template draft, and still requires human review. Nothing in the product depends on it.
- The assistant never takes an action: it cannot review, block, publish or file.

## Stage list

| Stage | What | Tier |
|---|---|---|
| W0 | Audit, baseline tests, this roadmap (done) | Opus, Fable review |
| W0.1 | Make the baseline fully green: branding test skips git-ignored paths (Python repo) | Opus |
| W1a | Read model for the UI: transaction store, transaction search and detail, account view, model version in results | Fable design, Opus build |
| W1b | Overview endpoint for the dashboard | Opus |
| W1c | Alert queue on the server: search, sort, paging, priority, typology, assignment, comments | Fable design, Opus build |
| W1d | Graph endpoint with evidence per edge | Fable design, Opus build |
| W1e | Investigations (case) entity, timeline, attachments-by-reference, SLA | Fable design, Opus build |
| W1f | Narrative endpoint and pack verification endpoint | Fable design, Opus build |
| W1g | Message log and behaviour summary endpoints | Opus |
| W2 | Design system and app shell, current fonts kept | Fable design, Opus build |
| W3 | Executive dashboard and alert management views | Opus |
| W4 | Network intelligence view | Opus, Fable review |
| W5 | Case workspace view | Opus |
| W6 | Investigation assistant and reports views | Opus, Fable review |
| W7 | Scam and behavioural views, thin areas shown honestly | Opus |
| W8 | Website end-to-end tests and screenshots | Opus |
| K1 | Known limit: orphan alert after a commit failure | Fable (hard bug), Opus |
| K2 | Known limit: 24 h pair-window double count on a re-scored retry | Fable, Opus |
| K3 | Done: demo replay has its own drift window (`docs/DRIFT.md`) | Opus |
| K4 | Known limit: two engines in memory with Redis | Fable design, Opus |
| P1 | Port the fixes to the Python repo `/home/user/nemesis1` | Opus |
| D1 | Evasion: `timing_jitter` and `new_payee` | Fable design, Opus |
| D2 | AMLSim non-transfer: diagnosis and a pre-registered fix attempt | Fable |
| R1 | Full-data phase: HI-Small full evaluation and retrain | Opus, Fable review |
| R2 | Full-data phase: SAML-D and HI-Medium | Opus, Fable review |
| R3 | Remaining datasets: graph, card, login, behavioural | Opus, Fable review |
| R4 | Unified multi-domain training with the new indicators | Fable design, Opus |
| S1 | Security review and hardening (brief section 8) | Fable review, Opus fixes |
| F1 | Re-measure the Redis webhook after PR #37; profile the drop above c=32 | Opus, Fable for the profile |
| F2 | Full API-path benchmark (brief section 7) | Opus |
| Z1 | Final documents: FINAL_TEST_REPORT, PERFORMANCE_REPORT, SECURITY_REVIEW, PILOT_READINESS | Opus, Fable review |

## Website stages

### W0.1 Baseline fully green

- `tests/test_branding.py::test_no_legacy_names_anywhere_in_source` in `/home/user/nemesis1` scans git-ignored
  `graphify-out/` cache files. Make it walk `git ls-files` (or skip `.gitignore`d paths). Check the Rust twin
  `no_legacy_names_anywhere_in_source` in `tests/api.rs` does the same.
- Test: pytest 58 of 58 with `graphify-out/` present.

### W1 Backend endpoints the UI needs

All new routes are additive, read existing engine state, follow the existing auth rule (`security::gate`: any role
reads; writes need analyst or supervisor where they change an alert or case) and get tests in `tests/api.rs` (memory)
and, where state is shared, `tests/live.rs` (Redis). Each response carries `scope` (`"instance"` or `"shared"`) and
`demo` (true when the data came from the replay or fixtures), so the UI can label it.

**W1a Transaction read model.**
- Add a bounded store of scored payments (ring buffer, default 50,000, env-configurable; in Redis mode a capped list
  per prefix): `tx_id`, the payment fields, decision, band, risk, reasons, layer exit, source (`api`, `assess`,
  `webhook`, `replay`), alert id if any. Written by the existing scoring paths; scoring itself is unchanged.
- `GET /api/v2/transactions?q=&account=&payee=&decision=&min_risk=&from=&to=&format=&sort=&limit=&cursor=`.
- `GET /api/v2/transactions/{tx_id}`: the stored result plus the engine's feature values and reasons.
- `GET /api/v2/accounts/{bank}:{account}`: timeline from the store, beneficiaries with counts, totals and first/last
  seen, engine aggregates (fan-in, fan-out, 24 h windows) already kept by `Engine`.
- Add `model: {api_version, cascade_sha256, hardening_sha256, ruleset}` to `/health` and as a new key in payment
  results (hashes computed once at load).
- Tests: search filters, paging, eviction at capacity, Redis parity, no change to existing response keys.

**W1b Overview.** `GET /api/v2/overview`: counts by decision and band, alerts by status and severity, open and closed
investigations, campaigns, watchlist entities, drift status, health and model block, and time buckets (per minute or
hour) from the transaction store. Confirmed-fraud counts only from analyst `blocked` outcomes and, for the replay, the
labelled `truth` block marked `demo: true`. Test: counts equal the sums from the underlying routes.

**W1c Alert queue.**
- `GET /api/v2/alerts` gains optional `q`, `sort`, `order`, `limit`, `cursor`, `assigned_to`, `priority`,
  `typology` (no parameters gives today's response exactly).
- Derived, labelled fields added to each alert: `priority` (rule over `risk_score` band and final decision, rule id
  shown), `typology` (from the context workflow, message workflow and motif reasons), `detection_sources`
  (cascade, context, message, session, network, consortium).
- `POST /api/v2/alerts/{id}/assign` (`analyst` or `supervisor`, nonce) and `POST /api/v2/alerts/{id}/comment`
  (any analyst role, nonce, 2,000 chars), both appended to the hash-chained audit log.
- Tests: role checks, nonce replay, audit chain still valid, last-write-wins limit documented for Redis.

**W1d Graph.** `GET /api/v2/graph?alert=|account=|campaign=&depth=1..3&from=&to=&limit=`: nodes (`account`,
`beneficiary`, `device` when a session gave a `device_hash`, `campaign`, `consortium_flag`) and directed edges
(`payment` with amount, time and `tx_id`; `shared_payee`, `shared_identifier`, `similar_script` from campaigns;
`consortium` with ledger `seq`; `coordination`). Every edge carries `evidence: [{kind, ref}]` pointing at a stored
transaction, alert, campaign link or ledger entry. Node and edge caps with a `truncated` flag. The response states that
a link is not an allegation. Tests: every edge has evidence that resolves; caps; time filter.

**W1e Investigations.** A case entity kept apart from today's `/v2/cases` (the engine's L4 list, unchanged):
`POST /api/v2/investigations` (from one or more alert ids), `GET /api/v2/investigations?status=&assigned_to=`,
`GET /api/v2/investigations/{id}` (alerts, transactions, timeline merged from audit entries, comments, attachments,
SLA due and breached flags), `POST .../{id}/status`, `/assign`, `/comment`, `/attach` (a reference with a SHA-256 of
an uploaded or external document; no file storage in the first cut). Closing needs a supervisor; every change goes on
the audit chain. Maker-checker stays only where the backend supports it: block approval. Tests: lifecycle, roles,
SLA computation, audit chain.

**W1f Narrative and verification.** `GET /api/v2/alerts/{id}/narrative` and
`GET /api/v2/investigations/{id}/narrative` (see "AI-assisted investigation" above).
`POST /api/v2/evidence/verify` recomputes an evidence pack's `pack_hash` and checks its signature against the public
key, returning which check passed. `GET /api/v2/investigations/{id}/report` returns a printable HTML report
(case details, transactions, relationships, model outputs, signals, provenance, notes, timestamps, audit) with the
verification result embedded only if it succeeded. Tests: determinism, every statement sourced, tamper detection.

**W1g Messages and behaviour summaries.** A bounded log of scored messages (redacted text, band, workflow, tactics,
entities, linked alerts) with `GET /api/v2/messages?q=&band=&limit=`; `GET /api/v2/behaviour/summary` (typing checks,
PRAMAAN decisions by outcome, identity checks, duress events, each with counts and `scope`). Mouse dynamics is added as
`POST /api/v2/behaviour/mouse/check` only if the Fable design finds a sound population model for serving; otherwise the
UI shows it as "evaluated offline, not served" with `results/balabit_eval.json`.

### W2 Design system and app shell

- `service/static/app.html`, `assets/app.css`, `assets/js/api.js`, the `/assets/{*path}` route, left navigation
  (charcoal/navy) with sections A to I, top bar with instance health, store kind, signing key id and auth mode,
  role-aware key entry (demo keys offered only in demo mode).
- Fonts unchanged: Geist (body), Instrument Serif (headings), Geist Mono (numbers and ids). Token values change to
  the brief's palette; token names become role names. Dark theme.
- Naming decision for the user: keep "RuseTect" (what the code, pages and branding tests say) or rename the UI to
  "NEMESIS". If renamed, change `branding.rs` and both repos' branding tests in the same stage.
- Old pages keep working; the landing page links to the app.
- Tests: Rust test that every asset is embedded and served with the right type; CSP header added here
  (`default-src 'self'`, fonts allowed only if not self-hosted).

### W3 Executive dashboard and alert management

- Dashboard (A): KPI cards, activity chart, decision and band distribution, alert severity, investigation status,
  campaigns, drift and model health, recent suspicious activity; all from `/v2/overview`; detection performance from
  `/v2/model-card` and committed results with dataset labels; demo data badged.
- Alerts (F): server-side table with search, sort, filter, paging; detail drawer with the `assessed` result, reasons,
  risk score, intervention, evidence link; actions (assign, comment, hold, verify, release, approve block, create
  investigation) with the role and nonce rules.

### W4 Network intelligence

- Cytoscape.js view over `/v2/graph`: typed nodes and directed edges, amounts and times, search, zoom and pan,
  filters by edge type and time range, entity side panel, expand-one-hop, highlight of alerted and consortium-flagged
  entities, and an evidence list for every edge. A fixed caption: a connection is not evidence of guilt.
- Performance check with the cap (for example 2,000 elements) and the replay data.

### W5 Case workspace

- Investigation list (open, pending, closed, critical, SLA at risk) and a one-screen case view: header, alerts,
  transactions, timeline, graph excerpt, comments, attachments (references with hashes), audit history, actions.
  Block approval shows the two-analyst state.

### W6 Investigation assistant and reports

- Assistant panel on the alert and case views showing the narrative draft grouped by statement kind, each statement
  with its source link, a "requires analyst review" banner, and a place for the analyst's conclusion (saved as a
  comment). Optional LLM rephrasing shown only when configured.
- Reports (I): printable case report, STR draft, evidence pack download, verification panel that calls
  `/v2/evidence/verify` and `/v2/verify` and shows the result as returned.

### W7 Scam and behavioural modules

- Scam intelligence (C): message submission (consent noted), result with band, workflow, tactics, highlighted key
  phrases, entities, model risk vs final risk, message log, related campaigns and alerts; the model card with the note
  that the Indian rows are synthetic.
- Behavioural (D): typing enrol and check, session-signal explainer from `/v2/assess`, identity check, PRAMAAN
  decisions with reasons, duress outcome, summary counts; mouse dynamics as served or "not served" per W1g. Each signal
  is marked consent-based; nothing is shown that the backend did not return.

### W8 Website end-to-end tests and screenshots

- Browser tests with **Playwright** (Apache-2.0) against a local `rusetect-server` in demo mode: every navigation link,
  button, form, filter, search, chart render and API call; normal and suspicious payments; malformed input; missing
  fields; duplicate events (webhook replay); transaction splitting (structuring); rapid beneficiary dispersal (fan-out
  fixture); a mule network (`fixtures/mule_fanin.json`); service failures (engine missing, Redis down) and concurrent
  requests. Kept in `tests/e2e/` with a small `package.json` used only for tests (the site itself stays build-free).
- Screenshots of every view at desktop and tablet width, committed under `docs/screenshots/`.
- CI job added after the Rust job.

## Remaining work, after the website

### Function and integration: known limits (from `docs/live.md`)

- **K1 Orphan alert after a commit failure.** Open the alert and the desk-chain entry only after the commit succeeds,
  or roll them back on failure; test with the existing Redis outage harness.
- **K2 Pair-window double count.** A retry that is scored afresh adds its risk to the 24 h pair window again; dedupe
  the pair window by `event_id` as account totals already are. Test: replay after a lost reply leaves window totals
  unchanged.
- **K3 Drift fed by demo traffic.** Feed the drift monitor only from live and API traffic, or keep separate windows
  with a `source` label; test that a replay does not move live drift.
- **K4 Two engines in memory with Redis.** Share one engine (models are read-only) between the desk path and the
  stateless Redis path; measure memory before and after.
- **P1 Port to the Python repo.** Bring the fixes from PRs #29 to #37 and K1 to K4 that have Python twins into
  `/home/user/nemesis1`, with its tests; keep the parity checks green.

### Detection

- **D1 Evasion.** `research/out/evasion_validation.json` marks `timing_jitter` (PR-AUC 0.4515 against clean 0.6316)
  and `new_payee` (0.5451) as known weaknesses. Pre-register a fix (for example time-bucket-robust window features and
  payee-age-independent fan features), evaluate on validation only, adopt only if it meets the registered rule.
- **D2 AMLSim non-transfer.** `research/out/holdout_amlsim.json` verdict "does not transfer" (served PR-AUC 0.0024).
  Diagnose which features shift, test a domain-robust feature subset on a new pre-registered seed, and record the
  result whatever it is. The website keeps showing the current verdict until then.

### Data and retraining (the PDF plan items still open)

- **R1 HI-Small full data**: full evaluation and retrain on all 5.08M rows, chronological splits, thresholds on
  validation only.
- **R2 SAML-D and HI-Medium**: convert, evaluate transfer, then retrain if the pre-registered rule says so.
- **R3 Remaining datasets**: graph, card, login and behavioural sets from `docs/datasets.md`, each with a
  licence check and a result file in `research/out/`.
- **R4 Unified multi-domain training** with the new indicators; served only if it beats the current models on each
  domain's validation split; served model version bumped and shown on the website.

### Integrity and security

- **S1 Security review** (brief section 8): authentication, roles on every new route, input validation, secrets,
  sensitive data in responses and logs, audit coverage of every write, `cargo audit`, rate limits per route, CSP,
  tenant isolation (single tenant today; state it), and a production signing key so evidence verifies across
  instances. Fixes land as small PRs with tests.

### Performance

- **F1 Redis webhook.** The committed S1 benchmark (`results/e2e_bench_s1.json`, git head `4be9aeb`, before PR #37)
  peaks at 5,282.43 requests/s at concurrency 32 and falls to 4,823.22 at 64 and 3,231.58 at 128 (p99 8.86, 30.56 and
  70.92 ms); its own gate is `pass: false` (memory `/v2/payment` at c=64 regressed). Re-run the pre-registered
  protocol on the current code, then profile above c=32 (lock contention on the state mutex for PRAMAAN and the desk
  chain, pool size, Redis round trips, CPU shared by client, server and Redis on 4 CPUs).
- **F2 Full API path**: throughput, p50, p95, p99, error rate, CPU and memory for the website's read routes (overview,
  transactions, graph) and the scoring routes, local only, labelled as single-node measurements.

### Final documents

- **Z1** `docs/FINAL_TEST_REPORT.md`, `docs/PERFORMANCE_REPORT.md`, `docs/SECURITY_REVIEW.md`,
  `docs/PILOT_READINESS.md`, each built from command output and files in `results/`, plus the updated
  `docs/WEBSITE_IMPLEMENTATION_MAP.md` with the new classifications.

## How each stage finishes

1. Baseline commands green, plus the stage's new tests (counts recorded in the PR).
2. `graphify . --update` so the knowledge graph stays current.
3. Fable review of the diff for design and correctness where the stage table says so.
4. Push to the working branch; no merge or production deploy without the user's approval.
