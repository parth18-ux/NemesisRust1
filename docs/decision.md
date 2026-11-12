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

**9. What was not done, and why.**
- No GNN: no evidence it beats trees with graph features here, and it would complicate serving and explanations.
- New indicators not served: they did not pass their pre-registered rule (`research/out/unified_train.json`).
- No claim of real-world accuracy: every payment model is trained on synthetic data; the shadow pilot
  (`docs/PILOT.md`) is the way to measure it on a bank's data.
