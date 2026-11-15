# Fresh holdout evaluation: pre-registration DRAFT

**Status: DRAFT, awaiting user approval; not run.** No data for it has been generated or read. Once approved, this
file is committed unchanged (only the status line and the approval commit change) before any data is generated, and
the result is recorded in `docs/decision.md` whether it passes or fails.

## Why

Every payment result so far is on data that has been read before. The HI-Small test part has been read by many
studies, some designed after earlier results, so the served model still needs one confirmation on data no study has
read (`docs/MODEL_CARD.md`, Known limitations). The plan's phase C exit evidence is an untouched holdout report.

## Data (the user picks one before approval)

- **Option A, untouched data from a used generator.** A new run of the public IBM AMLworld generator (the generator
  behind HI-Small) with the HI-Small settings and a new random seed. Same schema, so the served model runs unchanged.
  Answers: does the served score hold on fresh draws, without the reuse of the HI-Small test part?
- **Option B, a generator not yet used.** IBM AMLSim (Apache-2.0), which no result in `results/` or `research/out/`
  uses. Different schema: the column mapping (payment format and currency where missing) is written into this file
  before generation and not changed afterwards. Answers: does it transfer to a new generator? The served cascade's
  zero-shot transfer has been weak on other generators (`research/out/hardened_transfer.json`), so a fail is a
  plausible, useful outcome.

The seed is the first eight hex digits of the commit that approves this file, so it cannot be chosen after looking.

## Frozen before generation

- Code, models, band thresholds and features: the approval commit. No refit, recalibration or threshold change.
- Split: the same chronological day split as `research/served_eval.py`; earlier days are history only, metrics come
  from the test days only.
- One run. Any bug fix after the run is reported with both the original and the corrected result.

## Measures

- PR-AUC and ROC-AUC of the served score, with 95% intervals from the whole-day block bootstrap of
  `research/served_eval.py`.
- Precision and recall of the verify, hold and block bands at the served thresholds, and alerts per 1,000 payments.
- Two baselines on the same rows: (B0) the served account-graph context rules alone (`context_rules.rs`,
  `graph_points`), and (B1) ranking by amount.

## Rule

- **H1 adds value.** The served PR-AUC beats the better of B0 and B1, and the lower end of the paired day-bootstrap
  95% interval of the difference is above zero.
- **H2 bands mean something.** The lower end of the 95% interval of hold-or-block precision is above the test
  part's laundering rate.

Pass: both. Anything else is a fail, recorded with its numbers in `results/fresh_holdout.json` and
`docs/decision.md`. A fail does not lead to tuning on this holdout; any fix is checked on a later, separate holdout.

## Not covered

Real bank data (the shadow pilot, `docs/PILOT.md`), message context (no messages in either generator), and an
adversary who has studied the model.
