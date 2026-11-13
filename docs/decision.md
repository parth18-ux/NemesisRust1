# Decisions and reasoning

Why RuseTect is built the way it is, and why each change in the November 2026 review session was made. Numbers come
from the files named next to them.

## Architecture

**Rust for serving, Python for training.**
Serving touches every payment, so it needs predictable latency, one small binary and no garbage-collector pauses: Rust
gives all three and runs unchanged on a laptop, in Docker and on Vercel. Training happens offline and needs the mature
gradient-boosting libraries (XGBoost, LightGBM, scikit-learn), whose cores are already compiled C++. Rewriting training
in Rust would cost days and risk the parity we verify (Rust matches Python to within 3e-8). The rule: anything that runs
per payment is Rust; anything that fits a model is Python, and the Rust engine loads the exported model files.

**Gradient-boosted trees, not a neural network.**
Payment data is tabular, the positives are rare (0.1% to 2%), and analysts need reasons. Trees are the strongest
published baseline on this kind of data, train in minutes on a CPU, and give exact per-feature explanations
(TreeSHAP). The research review found no evidence that a graph neural network beats trees with graph features on
these datasets (`research/out/other_*.json`: neighbour aggregates help trees on every graph tested).

**Causal account-history features.**
Every feature is computed from earlier payments only, before the current one updates the state. This is what lets the
same code run live and in research, and it rules out leakage from the future.

**Libraries.**

| Library | Used for | Why this one |
|---|---|---|
| axum + tokio | HTTP server | the standard async Rust web stack; small, fast, well maintained |
| serde / serde_json | JSON in and out, model files | the Rust standard for serialization |
| own XGBoost and LightGBM readers (`crates/rusetect/src/models`) | inference | no C library to link, exact control of float order for parity, TreeSHAP included |
| flate2, csv | streaming large datasets | stream gzip and CSV without loading files into memory |
| redis | scale-out path | shared account state across workers |
| vercel_runtime | serverless demo | the official Rust runtime for Vercel |
| XGBoost, LightGBM (Python) | training | best accuracy on tabular data; native missing-value handling |
| scikit-learn | isotonic calibration, metrics | reference implementations of PR-AUC, ROC-AUC, isotonic regression |
| pandas, numpy | research data handling | chunked CSV reading for files larger than memory |
| Playwright (Node) | PDF of the research page | prints the exact generated HTML page |

## Evaluation rules

- **Chronological splits.** Fraud changes over time; a random split lets the model see the future. Every dataset is
  split by time (60% train, 20% validation, 20% test).
- **Thresholds and calibration on validation only.** The test part is read once, after the decision.
- **Adoption rules written in the script.** Each candidate's rule is in its script's docstring and is decided on
  validation before the script reads test. For the later studies (pooled second opinion, payer window, real against
  synthetic, the L3 studies, the indicator test) the script was committed before its results, so git shows the rule came
  first; for the earlier ones (full HI-Small evaluation, hardened risk, structuring, unified training) script and results
  arrived in the same commit, so that cannot be shown. One study broke the rule: `unified_train.py` decided on test.
  Candidates rejected by their own rules (format-free retrain, new indicators twice, payer window, third L2 model, L3
  combiner) are the rule working, not a failure.
- **The HI-Small test part is spent.** It has now been read by more than eight studies, and later studies were designed
  knowing earlier test results. Its numbers describe the served model, but they are no longer an untouched estimate: the
  served stack needs one confirmation on data no study has read (a new generator run or a bank's shadow pilot).
- **Isotonic maps fitted on the whole validation part** (hardened, pooled, third L2) are judged on that same part, which
  flatters their validation numbers; the test numbers are unaffected. The L3 combiner and indicator studies fit on the
  first half of validation and judge on the second.
- **PR-AUC first.** With 0.1% positives ROC-AUC looks good for weak models; PR-AUC shows precision where it matters.
  ROC-AUC is used across datasets because prevalence differs 10 to 100 times between them.
- **No MAE, RMSE or R².** Those measure errors when predicting a number. Fraud is a yes/no outcome, so the metrics are
  ranking (PR-AUC, ROC-AUC), band precision and recall, and calibration (expected calibration error).

## Session decisions

**1. Require `payment_format`; reject unknown formats and currencies.**
The API used to fill a missing format with ACH, the format most laundering uses in the training data, which pushed
genuine payments into higher bands (`research/out/slice_results.json`, section F). Rejecting is safer than guessing.

**2. Fail closed on configuration.**
Demo keys used to switch on whenever no keys were configured, so a mis-deployed server would accept published keys. Now
the server refuses to start unless it is given real keys and secrets, or `RUSETECT_DEMO=1` explicitly.

**3. Cap in-memory account state.**
Random account ids could grow memory without limit. A cap (default 2,000,000 accounts) refuses only payments that would
add accounts; known accounts keep scoring.

**4. Hardened risk: a format-free second opinion.**
Laundering re-sent in another format dropped the cascade's PR-AUC from 0.71 to under 0.01, because 92% of training
laundering is ACH. Retraining without the format feature lost accuracy and was rejected by its rule. Taking the larger
of the two isotonic-mapped scores keeps the cascade's accuracy on honest traffic and the format-free model's robustness under
attack. It passed its rule (`research/out/hardened_eval.json`). Calibration (isotonic) is needed first, because the two
models' raw scores are on different scales; isotonic was chosen over Platt because it does not assume a shape.

**5. Pair aggregation before calibration.**
Splitting one payment into four defeats per-payment scoring. A 24 hour noisy-OR of the cascade risks of each payer to
payee pair restores part of the recall without fitted parameters. Aggregating after calibration barely helped (the
calibration map sends small risks to near zero), so the served version aggregates before calibration. That choice was
made after seeing the first result on the same window, and the report labels it exploratory
(`research/out/structuring_defence.json`).

**6. Compute explanations only for flagged payments.**
TreeSHAP was about 90% of scoring time and ran for payments that were approved anyway. Reasons are now computed only when
a payment leaves the safe band, from the model that actually set the risk.

**7. Stream and sample inside Rust.**
The IBM Large files have 180 million payments. Sampling genuine rows inside the Rust harness (state still updated for
every payment) made the dump about 8 times faster and kept the output small; totals are written next to each file so
every metric is reweighted to the true prevalence.

**8. Pooled second opinion (multi-domain training): adopted, with a regression to disclose.**
The format-free model was trained on HI-Small only. A model trained on the training parts of every payment dataset
(bank AML, cards, wallets, and IEEE-CIS with real fraud labels) replaced it after passing its three rules (G1 to G3),
decided on validation parts only (`research/out/pooled_hardening.json`). This is multi-domain training, not evidence of
generalisation: the candidate saw the training parts of the very datasets on which its gain was measured, while the old
model saw only HI-Small. The mean gain also hides losses on the two largest bank-AML sets, HI-Large and LI-Large
(per-dataset numbers in the same file and in the report); the rule had no "no bank-AML set may get worse" check, and it
should have. It stays served because the served risk is the larger of the cascade and this model, so the cascade still
scores those sets, and it gains on six of the eight bank-AML sets; that candidate, a model trained on bank-AML data
only, was then tested with a per-dataset guard and not adopted (`research/out/aml_hardening.json`): it fixed the ROC-AUC
loss on HI-Large and LI-Large, but lost on AMLgentex (rule B1) and its mean bank-AML gain stayed under half a point (B2).
The served engine was rebuilt and replayed against the Python results; block decisions match exactly.

**8a. Payer window: not adopted.**
Widening the 24 hour window from a payer-payee pair to all of a payer's payments catches parts sent to new mule accounts,
but even with its own validation-fitted thresholds it raised clean block alerts about a hundredfold and failed rules A1
and A3 (`research/out/account_aggregation.json`). The pair window stays. The Python window was also rewritten to a
running log sum (linear time); it matches the old version to 1e-12.

**8b. Real against synthetic card data: real only.**
On IEEE-CIS (real card fraud) the arm trained on real data won on validation, and on test the synthetic-only arm was
close to random on the real data (`research/out/real_vs_synthetic.json`). Synthetic card sets are kept as tests, not as
a substitute for real training data. The unseen-payer subset is too small to support any claim.

**8c. One pair window for the API and the workers, with idle keys swept.**
The API kept every payer-payee pair it had seen forever, so memory grew without limit over a long uptime, and the Redis
workers skipped the window, so the same payment could get a different decision on the scaled path. The API now uses
`PairWindow` (pairs idle for a day are swept every 65,536 payments); the workers keep the same window in Redis
(`RedisState::pair_window`), so it survives restarts and rebalancing, and a redelivered payment is counted once.
`tests/scale.rs` checks the Redis window equals `PairWindow` and that worker decisions equal the API's.

**8d. More layers for the block band: tested, not adopted.**
Three pre-registered attempts to raise block-band recall (`research/out/l3_eval.json`, `research/out/l3_stack_eval.json`,
`research/out/new_features_eval.json`):
- a third L2 model trained on HI-Small, HI-Medium and HI-Large (about 40 times more laundering): validation block
  recall moved by under a point (rule: two points). It ranks better overall on test (PR-AUC), and on test its bands also
  catch more, but at more alerts (the thresholds sit on isotonic plateaus), so the study did not compare equal workloads;
  the later two do.
- a learned monotone L3 combiner over the layer scores: the same flat block recall, and it relearned the payment-format
  shortcut (worst format-attack PR-AUC near zero), because validation holds no attacked payments.
- the 13 account-behaviour indicators in that third model: 2.1 points over the same model without them, but 1.3 over
  the served cascade (rule: two), at exactly the served alert count.
Four different candidates land within about a point of the same block recall at the same alert count, which points to
a limit of the features and labels at this budget rather than of the layering. The served max of two calibrated scores stays: it is
the only combination that keeps the format attack closed.

**8e. Security and robustness pass after an independent review.**
- One access gate for every API route (`security::gate`): with real keys configured, everything except health, the
  public key, signature checks and the model card needs a known key, and wiping state (demo reset and replay) needs a
  supervisor. Before, only the review and evidence routes checked keys, so anyone could reset all state, change alert
  status, read campaigns (tipping off) or poison the watchlist. Demo mode stays open, as the public demo needs.
- Keys are validated at start-up (JSON, 16+ characters, known role), so a bad value stops the server instead of
  failing every request later.
- Duress PINs: a registration is never overwritten (it could hijack a customer's PIN and contacts), five wrong PINs
  lock the account, and the deliberately slow PIN hashing runs outside the shared lock so it cannot stall the server.
- Consortium publishing needs an analyst or supervisor key and signs only as this bank outside demo mode.
- Inputs: infinite amounts and non-finite typing values are refused (inf turned an account's running sums into NaN
  for good); timestamps accept ASCII digits only (other scripts' digits panicked the request); USD amounts are capped.
- Windows keep time order, so a late payment leaves the 24 h window on time, as in the Redis state.
- The engine passes the unrounded risk to the pair window and the calibration maps, as the workers and research do.
- Workers: malformed messages go to a dead-letter stream instead of stopping the worker, unacknowledged messages
  are replayed on restart (at-least-once), and the pair window applies with or without the hardening file.
- Memory: typing profiles, session events, device lists and case edges are capped; batches are all or nothing.
- Response headers (nosniff, no framing, no referrer); Redis is no longer published to the host in docker-compose,
  and the container runs as an unprivileged user.

**8f. PRAMAAN account-takeover authentication: ported, adopted as a new layer.**
PRAMAAN (MIT, Team CaffeineEaters, co-authored by Parth Sharma) decides allow / challenge / block for a login or payment
from 27 causal behaviour, device and transaction features with a monotone XGBoost, and picks the step-up the likely attack
cannot pass (a passkey, never an SMS code, after a SIM swap; a bank call-back for remote-access apps; a cool-off for a
coached customer on their own device). It is ported to Rust (`crates/rusetect/src/pramaan.rs`, routes
`/api/v2/authenticate` and `/api/v2/challenge/verify`) and checked against PRAMAAN's Python engine on 36,398
simulated requests: the same decision, challenge, rule, reasons and features on every one (`examples/pramaan_parity.rs`).
Against RuseTect's identity rule on the same traffic and the same assumed challenge efficacies
(`research/out/pramaan_eval.json`): attack requests stopped 96.4% against
77.0% with no touch or typing data (the conservative case), at the cost of more
genuine customers challenged (3.8% against 1.7%).
It is added next to the identity rule, not in place of it. Simulated traffic and assumed efficacies, from PRAMAAN's own
simulator: the comparison ranks designs, it is not a real-world fraud rate. Not ported: PRAMAAN's touch BiLSTM and typing
1D-CNN (the deviations are inputs; its simpler touch model, window statistics + LDA, nearly matches the BiLSTM in its own
report and is the one to port next) and the PayPal checkout gate (PayPal-specific, and it needs sandbox keys).

**8g. RuseTect payee intelligence feeds PRAMAAN.** When an authentication request carries no payee risk and its payee
is on the consortium mule registry, the engine receives 0.9 (PRAMAAN's own value for a held mule account) and the response
names the flagging banks. Monotone model: a flag can only raise the risk (tested). The touch model port is blocked: the
HMOG dataset is not reachable from this environment (HTTP 403), and the model needs it to fit its LDA projection.

**8h. MIDAS-R streaming burst score (Stream-AD/MIDAS, Apache-2.0): not adopted.** MIDAS-R edge, source and destination
scores were added as three extra features to a third L2 model and compared with the same model without them, at the
served alert counts on the second half of HI-Small validation, under rule M1-M4 pre-registered in `research/midas_eval.py`.
M1 and M2 failed: block-band recall was 0.6412 (precision 0.5153) with MIDAS against 0.6374 (0.5123) for the current model
and 0.6489 (0.5215) for the control without MIDAS. M3 and M4 passed (PR-AUC 0.6493 against 0.6496 for the current model and
0.6601 for the control; worst PR-AUC under the format attack 0.6138 against 0.6171). The MIDAS-R score alone reached
ROC-AUC 0.4789 and PR-AUC 0.001 on validation, no better than chance, so it is not ported (`research/out/midas_eval.json`).
A first run was invalid (all scores zero: the timestamp unit was read as nanoseconds while pandas returned microseconds,
collapsing the stream into one tick); it was fixed and rerun without changing the rule.

**8i. Ponytail pass.** Unused Rust removed (`pyfmt::dumps_indent`, unused `paths` helpers, `LiteBooster::num_trees`) and the
duplicate `py_str` in `campaigns.rs` now uses `str_report::py_str`: 77 fewer lines of Rust (9484 to 9407). Tests (73),
PRAMAAN parity and the attack-stream replay are unchanged.

**8j. Review fixes: campaign cap, worker input check, future timestamps.** Campaign building is quadratic and runs under
the state lock, so `/v2/str` and `/v2/campaigns` now link only the newest 2,000 alerts (`campaigns::MAX_ALERTS`).
Redis workers now send payments with an unknown payment format or currency to the dead-letter stream, using the API's own
lists, since the model reads those as missing and approves almost everything. The API refuses (422) a payment timestamped
more than one day ahead of the wall clock, which would otherwise make every window evict relative to a far-future time.

**8k. Per-route roles and analyst holds.** Outside demo mode the key check now also checks the role for each route
(`security::gate`): a viewer key reads only, a channel key also calls the scoring and customer routes, and only an analyst
or supervisor key reaches the review and consortium publish routes; before, any known key could call every POST. The
customer's pause-screen answer now changes only an alert no analyst has acted on (open, cooling_off, stopped_by_customer);
on a held, blocked, released or verification alert it is refused with 409 and leaves the status and audit log unchanged.

**8l. Sanctions name screening.** New `POST /api/v2/sanctions/screen` (`crates/rusetect/src/sanctions.rs`, a channel
route): names are normalised (accents, punctuation and honorifics removed), tokens are aligned in any order by
Jaro-Winkler and a Soundex key breaks ties; the response lists the five closest entries with scores and `hit`. List
source: the OFAC SDN download (treasury.gov) was blocked by this network, so `service/models/sanctions_sdn.csv` holds
40 **fictitious test entities** (program FICTITIOUS-TEST), none a real sanctioned party; the real list must be
loaded in deployment, and the threshold refitted on it. The threshold rule was committed before the first run
(`research/sanctions_eval.py`): on validation, the highest precision with recall >= 0.95, giving 0.927083
(validation precision 1.0, recall 0.9623). Test, once: precision 1.0, recall 0.9231, F1 0.96
(52 true variants, 112 hard negatives), below the 0.95 recall target; transliterations (recall 0.625)
and dropped middle names (0.75) are missed most. The validation split is small (53 true variants), so the
threshold is coarse; refit on the real list. Results: `research/out/sanctions_eval.json`, which the API reads for the threshold.

**8m. Customer paths cannot undo analyst decisions.** The pause-screen answer, re-verification and duress PIN now share
one check (`customer_may_act` in `crates/rusetect-server/src/lib.rs`): re-verification is accepted only from open,
cooling_off, stopped_by_customer or customer_verification and otherwise returns 409 with the status unchanged. A duress PIN
still escalates any alert to duress_hold except a blocked or released one, where the event is recorded but the status stays
and the confirm response carries `alert_status_unchanged: true`.

**8n. Sanctions matcher v2 (transliteration folding, dropped middle names): adopted.** The v1 test split (8l) had been
read once already, when its misses were analysed, so it could not judge a matcher designed after reading it. The rule
was therefore committed before any v2 code (`research/sanctions_eval_v2.py`, ADOPTION RULE v2) together with a FRESH
test set: 40 new fictitious entities, new romanisation rewrites and 150 common names from new pools, read once. v2
(`score_v2` in `crates/rusetect/src/sanctions.rs`; v1 stays callable) folds each token (suffix off -> ov, digraph and
letter rules, doubled letters collapsed) and scores a token pair max(JW raw, 0.96 x JW folded); a query of two or more
tokens shorter than the list name may also align one-to-one with every pair >= 0.90, scoring the mean minus 0.06 per
unmatched list token. Scores come from the Rust scorer itself (no Python mirror). Threshold, fitted on the v1 validation
split only: 0.94 (validation precision 1.0, recall 1.0). Fresh test: precision 1.0, recall 0.9847, F1 0.9923
(262 true variants, 412 hard negatives), against v1 at its own threshold: precision 1.0, recall 0.8626; v2 recall on
fresh transliterations 1.0 (v1 0.55) and dropped middle names 0.9545 (v1 0.2273). Old test: precision 1.0, recall
0.9808 (v1 1.0 and 0.9231). All four checks hold, so the API serves v2 with threshold 0.94; it reads
`research/out/sanctions_eval_v2.json` and falls back to v1 when `adopted` is false. Limits: variants are synthetic,
the fresh rewrites overlap the folding table, and the list is fictitious, so refit on the real list.

**8o. Redis workers count each payment once.** Delivery stays at-least-once, so `RedisState::features` now claims
`seen:{eid}` with SET NX (26 h) and a redelivered payment is rescored from the current state, so a decision is still
emitted, without writing its account counters, amounts or 24 h windows again. Payments without an eid cannot be
recognised and keep the old behaviour. The claim and the account-state writes now run as one Lua script (EVAL), so a
crash leaves both or neither; the script spans payer, payee and global keys in different hash slots, so it needs a single
Redis node rather than Redis Cluster.

**8p. Payee name check (confirmation of payee).** New `POST /api/v2/payee/check` (a channel route), aimed at
authorised push payment scams: `{"entered_name", "account_name", "account_type"?}` -> `{"verdict", "score",
"suggested_name"?, "reason"}`, where verdict is match, close_match (with the account name to show the payer) or
no_match, and reason is a short token-level explanation ("surname matches, first name given as an initial", "the
account is a business account but an individual's name was entered; ..."). No second matcher: `crates/rusetect/src/payee.rs`
scores with the served sanctions matcher v2 (`score_v2`) and only adds the verdict bands, the type check and the reason.
The account name comes from the bank's own records in deployment; here it is passed in the request. The rule was
committed before any payee code (`research/payee_check_eval.py`): synthetic labelled pairs (300 people,
100 businesses; exact, noise, initials, nicknames, typos, transliterations, dropped middle names and
suffixes against same surname, same first name, same trade, unrelated, and wrong account type), split 60/20/20 by
identity, thresholds fitted on validation only: match >= 0.9352, close_match >= 0.7967 (validation match
precision 0.9975, same-payee coverage 0.9732). Test, once (546 same-payee
and 320 different pairs): match precision 1.0 (0
different pairs labelled match), same payee reaching match or close_match 0.9799 (match alone
0.7015), so both checks of the adoption rule hold and the endpoint is served; the API reads the thresholds
from `research/out/payee_check_eval.json` and answers 503 if `adopted` is false. The cost: 0.2094
of different pairs get close_match, so the payer is shown someone else's name (owner-name businesses 0/20/0
and same-trade businesses 0/20/0 as match/close/no_match); initials reach no_match in
10 of 120 test pairs and hard nicknames (Bob/Robert) are not
recognised. `/api/v2/assess` does not call it yet: its inputs are validated and signed as one record, so adding the two
names there is a separate change. Limits: names are synthetic, from fixed pools; no open-source Pay.UK CoP matching
rules exist to compare with (they are published only as rulebooks).

**8q. Review fixes 2: bounded name matching, final review states, challenge eviction.** (1) The v2 subset rule
searched every one-to-one token alignment, factorial in the token count, on caller-supplied names and under the
service lock (13 against 14 identical tokens is about 14! steps). It now runs only when the list name has at most 8
tokens; longer names use the symmetric score alone. No evaluated name is that long: re-running
`research/sanctions_eval_v2.py` and `research/payee_check_eval.py` leaves every file in `research/out` unchanged.
(2) A blocked or released alert is final: further analyst actions are refused ("alert is final"), so a two-analyst
block already published to the consortium cannot be released by one analyst. Any release, verify or hold clears the
block approvals, so an approval given before a release cannot combine with one given after it. (3) PRAMAAN pending
challenges were stored only while fewer than 100,000 were pending, so once full every new challenge failed to verify;
now the oldest pending challenge is dropped to make room.

**8r. Payee check v2 (nicknames, surname-anchored close match): not adopted.** v1 (8p) showed the payer someone else's
name on 0.2094 of different-person test pairs and missed hard nicknames. v2, behind a switch in
`crates/rusetect/src/payee.rs` (`check(.., v2)`), for individual accounts only: a hand-written table of
56 common English diminutives maps to canonical given names (Bob/Rob/Robbie to Robert, Bill/Will to William, Ted to
Edward, ...; written for this project, no external list) on the account's given names, and an entered token is mapped
only onto one of them; close_match also needs an entered token matching the account surname at the match threshold.
Business names are scored as before. The rule was committed before any v2 code (`research/payee_check_eval_v2.py`): a
fresh set from the v1 generator with a new seed (20261114) and name pools disjoint from v1's, v2 thresholds fitted on
its validation split only (match >= 0.94, close_match >= 0.7333; v1 keeps 0.9352 /
0.7967). Fresh test, v1 against v2: match precision 0.9948 against
0.995 (2 and 2 different pairs labelled
match), same payee reaching match or close_match 0.978 against 0.9853,
different pairs labelled close_match 0.2375 against 0.3438.
Nicknames improve (match/close/no_match 3/18/4 against 23/2/0) and the
surname anchor removes same-first-name close matches (1/16/43 against
1/0/59), but the lower fitted close threshold lets same-surname pairs through
(1/15/44 against 1/46/13) and a business name typed for a
person's account carries the surname (0/5/55 against
0/21/39). Checks failed: a_fresh_test_v2_match_precision_eq_1, c_fresh_test_v2_different_close_le_half_v1, so v1 stays served (the API reads
`research/out/payee_check_eval_v2.json` and switches only if `adopted` is true). On the old test split: v1 / v2 close
match on different pairs 0.2094 / 0.3625, same payee covered
0.9799 / 0.9799. Stated before the run: same-trade and owner-name business
pairs are all close_match and unchanged by design, so the halving check was hard to reach; the nickname pairs and
the table share an author, so nickname recall is not an independent measure. The served v1 itself labelled 2
different-person pairs as "match" on the fresh test (match precision 0.9948, `research/out/payee_check_eval_v2.json`),
so its earlier test precision of 1.0 overstated it.

**8s. Security review 2: string caps, bounded ledger and desk, GET rate limit, proxy header.** (1) Every string field
the API validates is refused above 5,000 characters (`validate::MAX_STR`, the message cap) with the
`string_too_long` error, list items included, before it is copied into engine state or a stored alert; bank and
account ids are capped at 128. `/v2/authenticate` reads raw JSON, so any string over 512 characters inside its
device, network, behaviour or transaction objects is a 422. (2) The demo consortium ledger stops at
`consortium::MAX_LEDGER` = 10,000 entries (a publish beyond it is an error), since every GET `/v2/consortium`
re-verifies all signatures and every check scans the ledger. The review desk keeps at most `review::MAX_ALERTS` =
10,000 alerts: beyond it the oldest final (released or blocked) alert is dropped first, and the oldest alert only when
none is final, because refusing new alerts would hide fraud. The hash-chained audit log is not trimmed (dropping its
head breaks `verify_chain`), so it still grows with traffic; it is bounded only by the rate limit and restarts.
(3) The rate limit now covers every `/api` method with the same per-client budget (240 per 10 s by default), not only
POST. (4) With `RUSETECT_TRUST_PROXY=1` the client key is the rightmost X-Forwarded-For entry, the one the trusted proxy
appended; the leftmost is whatever the caller sent. Vercel overwrites the header with one address, so it is unchanged
there.

**8t. Ledger-full blocks succeed; bounded watchlist, coordination and duress maps.** (1) When the consortium ledger is
full, a two-analyst block still stands and the review call returns 200: the alert gets `"published": false`, a
`publish_error` with the reason, and a `consortium_publish_failed` system event, instead of the publish error turning
the review into a 400. (2) The lure payee watchlist keeps at most `lure_fusion::MAX_WATCH` = 100,000 entries and the
coordination tracker at most `coordination::MAX_TRACKED` = 100,000 payees and 100,000 flagged senders, each dropping
the oldest first. The duress vault refuses new registrations (409) beyond `intervention::MAX_DURESS_USERS` = 100,000
rather than evicting a registered customer, and keeps the newest `MAX_NOTIFICATIONS` = 100,000 notifications. All are
in-memory shortcuts; a real deployment moves them to a store. The per-account lure event and push maps are not capped.

**8u. Capped lure account maps; explicit PRAMAAN decision.** (1) The per-account lure event and push maps now keep at
most `lure_fusion::MAX_ACCOUNTS` = 100,000 accounts each, dropping the oldest-added account first (the coordination
tracker's pattern), so a flood of distinct account ids cannot grow memory without bound (tested). (2)
`research/out/pramaan_eval.json` now carries a `decision` field written by `research/pramaan/compare.py` (`--decide`
re-derives it from the saved arms): adopted, under the rule of 8f, PRAMAAN stops a larger share of attack requests than
the identity rule with and without behaviour data; the genuine-challenge cost is reported beside it, not gated. The rule
was written down after the run, not pre-registered, and the field says so. The research page reads this field.

**8v. Ponytail pass 2: shared helpers, no behaviour change.** Duplicated code merged into one place each: the
payment-only features (amount, codes, hour, cross-bank and so on) are built once in `CascadeConfig::payment_features`
and reused by the engine, the compact state and the Redis state; the cascade and format-free SHAP reasons share
`Engine::shap_reasons`; audit-log events are written as one `json!` object (same key order, so the same hashes); the
sanctions and payee routes share `req_name`; the pause-screen and re-verification routes take the alert once for the
404 check. Every validation, cap and error stays. Checked by the full test suite, clippy, and a before/after dump of
`examples/parity_dump` on the demo stream, which is byte-identical. PRAMAAN code is unchanged.

**8w. Live ingest: a signed webhook that decides and commits in the same request.** RuseTect runs on Vercel's free
tier, where nothing runs between requests, so the webhook itself is the trigger: `POST /api/v2/events/payment` checks an
HMAC-SHA256 signature over the raw body (timestamp and signature headers, 5 minute tolerance, constant-time compare),
claims the `event_id`, scores the payment with the served engine, applies PRAMAAN when the event carries an `auth`
object, opens a review alert for verify/hold/block, appends a hash-chained ledger entry and commits before replying. A
repeated `event_id` returns the stored result and is not scored again. The durable store is Redis over `rediss://`
(Upstash's free tier through the Vercel Marketplace), reusing the streaming workers' Redis account state
(`scale::redis_state`), so every function instance sees the same history; the REST API was not needed because Upstash
accepts TLS connections from a function. Without a Redis URL the server keeps everything in memory and says so
(`/api/health` `"durable": false`, console badge "not durable: in-memory"). The console polls `GET /api/v2/live` every
1.5 s instead of holding a server-sent-events stream, which a serverless function would cut at its time limit. How to
send events: `docs/live.md`.

**9. What was not done, and why.**
- No GNN: no evidence it beats trees with graph features here, and it would complicate serving and explanations.
- New indicators not served: they did not pass their pre-registered rule (`research/out/unified_train.json`).
- No claim of real-world accuracy: every payment model is trained on synthetic data; the shadow pilot
  (`docs/PILOT.md`) is the way to measure it on a bank's data.
