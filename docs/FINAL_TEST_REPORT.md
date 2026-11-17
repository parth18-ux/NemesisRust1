# Final test report: website (stage W8)

Recorded on 17 Nov 2026 on branch `ccr-a05856fa-mavodd`, after stage W8 (server-side alert search, review actions in the
alert drawer, per-alert demo flag, `/api/v2/whoami`, tablet layout checks, the whole-site audit and the restyle to the
original olive-on-warm-white palette). Every number below is copied from the command's own output in this session. This
is the website part; the performance, security and pilot parts follow in later stages (roadmap Z1).

Machine: 4 logical CPUs, Linux 6.18.44, `cargo 1.97.0`, Node 22.23.4, Playwright 1.63.0 with its Chromium build 1243.

## 1. Commands and results

| Step | Command | Result | Time |
| --- | --- | --- | --- |
| Rust tests | `cargo test --release --workspace` | 146 passed, 0 failed, 0 ignored; Redis tests ran (no "redis-server not installed") | 310 s wall, including the release build |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` | clean, no warning | 5.2 s (incremental) |
| Browser suite, desktop | `BASE=http://localhost:8011 VIEWPORT=desktop node tests/e2e/smoke.js` | 54 passed, 0 failed (1440 x 900) | 29.3 s |
| Browser suite, tablet | `... VIEWPORT=tablet node tests/e2e/smoke.js` | 54 passed, 0 failed (834 x 1112) | 31.7 s |
| Browser suite, phone | `... VIEWPORT=phone node tests/e2e/smoke.js` | 54 passed, 0 failed (390 x 844) | 30.8 s |
| Screenshots | `SHOTS=1 BASE=http://localhost:8011 node tests/e2e/smoke.js` | 54 passed, 0 failed; 45 files in `docs/screenshots/` (15 views x desktop, tablet, phone) | 55.8 s |
| Secrets in frontend code | the grep in the CI job `website` (demo keys, private keys, `RUSETECT_*` secrets, `sk-`, `AKIA`, `ghp_` tokens) over `service/static` without `vendor/` | no match | |

Each browser run used a fresh server: `RUSETECT_DEMO=1 RUSETECT_RATE_LIMIT=100000 ./target/release/rusetect-server
--port 8011` (the default limit of 240 requests per 10 s per client is below what the report and screenshot steps send).
The Rust test count by binary: 62 (rusetect unit), 4, 45 (`tests/api.rs`), 1 (`assets.rs`), 8 (`intel.rs`), 1
(`intel_auth.rs`), 14 (`live.rs`), 4 (`motifs.rs`), 7 (`scale.rs`). New in W8: `api.rs::alert_queue_searches_sorts_and_pages_on_the_server`,
and new assertions in `intel.rs` (webhook alerts are never demo) and `intel_auth.rs` (`/v2/whoami` and the paged alert
list with configured keys).

CI: `.github/workflows/ci.yml` job `website` (after `rust`) builds the server, checks the frontend for secrets, installs
the Playwright pinned in `tests/e2e/package-lock.json` with its official installer (`npx playwright install --with-deps
chromium`) and runs the suite at the three widths, each on a fresh demo server. The workflow runs on pushes and pull
requests to `main`; it did not run on this branch push.

## 2. What the browser suite tests, per section

One check name per row of `smoke.js`; timings are from the desktop run (tablet and phone are within 0.3 s, except the
navigation check, 3.3 s at tablet and phone because it opens the menu for every link).

| Section | What is exercised | Checks | Desktop time |
| --- | --- | --- | --- |
| Shell (W2) | dashboard renders, all nine section links route (through the menu button at tablet and phone), `aria-current`, old pages `/`, `/console`, `/investigate`, `/report`, `/docs` answer | 2 | 2.2 s |
| A. Dashboard | KPI cards from `/v2/dashboard`, confirmed fraud shown as not available, hour and day buckets redraw the chart, alert links open the drawer, theme toggle | in the shell row + audit | |
| B. Transactions | account, typology, band, amount, source, time filters, paging, empty result, clear; detail with risk, reason codes, related accounts, history, timeline, links; in-app links; SHAP rows on a replayed payment | 5 | 2.5 s |
| C. Scam | scam and benign samples against the model's own phrases and thresholds; empty, oversized, half-linked and malformed input; redacted history, band filter and text search | 4 + audit | 0.7 s |
| D. Behavioural | typed keystroke enrolment and check, synthetic owner and impostor, PRAMAAN presets (owner, SIM swap, coached) with challenge, identity, duress PIN; unavailable signals labelled | 3 + audit | 7.8 s |
| E. Network | empty state and demo load, search, edge click to its transaction, amount and direction filters, expand, depth, deep link, zoom in and out, fit, time range, aggregated parallel edges, View in network from the drawer | 7 + audit | 1.7 s |
| F. Alerts | server-side search, band, status, state, priority and assignee filters, sort by every column both ways for risk, per-row Demo labels and the banner count, select all, pager, deep link `#/alerts/{id}` written on open and opening the drawer on load, Escape, 401 and 403 messages, triage with nonce, review decisions (viewer disabled with reason; analyst A approves, A's second approval disabled, B approves, the alert is final; hold and verify; ledger entries; nonce replay refused 409), investigation from a selection | 12 | 3.5 s |
| G. Investigations | open from the drawer, every workspace panel, escaped comment, assignment rules, four-eyes close and supervisor reopen, evidence verify (real and forced false), list tabs (open, pending, closed, critical, SLA, all), SLA and progress | 6 + audit | 1.6 s |
| H. Assistant | typed and sourced draft with banner, sources jump in place and to the transaction, viewer refused, analyst accepts an edited draft into an audited comment, `#/assistant` case picker | 4 + audit | 1.0 s |
| I. Reports | ten sections, verification equal to the server's, forced verify and audit failures shown as Not verified, JSON and STR downloads, print view and PDF; no console error | 5 | 2.0 s |
| Audit (W8) | every route in light and dark theme: no console error, no CSP violation, no sideways scroll, every control labelled, every button and link named, text contrast at least WCAG AA; CSP header on `/app`; keyboard (skip link first, visible focus on 40 Tab stops, drawer focus trap and return, dialog Escape); malformed input on every form; every remaining control once | 6 | 6.1 s |

Malformed input covered: transaction minimum above maximum, markup typed as an account in the network search (shown as
text, 404 explained), triage SLA hours `abc`, `-5`, `99999` and an empty triage, an investigation with a blank title,
a malformed review body (422) and an unknown action, bad `sort`, `limit` and `demo` on `/v2/alerts` (422), a blank case
comment, a negative PRAMAAN amount, a short duress PIN, a typing check with nothing typed, and the scam form's cases
above. Three of these showed a misleading or raw message before W8 and were fixed: SLA hours (now checked in the page),
validation errors (the page shows the server's messages, never its JSON) and negative PRAMAAN inputs.

## 3. Accessibility method

axe-core was not used: its licence is MPL-2.0, which this project does not vendor. The suite carries its own checks
instead (`audit()` in `smoke.js`): accessible names for controls, buttons, links and images; contrast of every visible
text run against its first opaque background with the WCAG formula (4.5:1, or 3:1 for large text; disabled controls
exempt); keyboard order and focus visibility as listed above. It found and W8 fixed: muted text at 4.22:1 on the page
background (the token was darkened) and two sideways scrolls at phone width (long hashes in the case timeline and the
report's narrative digest now wrap). These checks are not a full WCAG audit: screen-reader output, zoom to 400 % and
reduced-motion behaviour were not tested.

## 4. Not tested here

- The suite runs in demo mode only; configured analyst keys are covered by `tests/intel_auth.rs` (API) but not in a browser.
- Redis mode in the browser (the API is covered by `tests/live.rs` and `tests/scale.rs`).
- Browsers other than Chromium.
- The four legacy pages (`/`, `/console`, `/investigate`, `/report`) are checked for a 200 answer only; they use inline
  scripts and send no CSP.
