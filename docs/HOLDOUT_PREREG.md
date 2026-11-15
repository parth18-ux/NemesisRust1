# Fresh holdout evaluation on IBM AMLSim: pre-registration

**Status: approved by the user 2026-11-15 (option B).** Committed, with the scoring script
`research/holdout_amlsim.py`, before any AMLSim data was generated or read. After this commit only the result is
added (`research/out/holdout_amlsim.json`, `docs/decision.md`), whether it passes or fails.

## Why

Every payment result so far is on data that has been read before. The HI-Small test part has been read by many
studies, some designed after earlier results, so the served model still needs one confirmation on data no study has
read (`docs/MODEL_CARD.md`, Known limitations). The plan's phase C exit evidence is an untouched holdout report.

The user chose option B, a generator not yet used: IBM AMLSim, which no result in `results/` or `research/out/` uses.
Question: does the served score transfer, unchanged, to a new generator? The served cascade's zero-shot transfer has
been weak on other generators (`research/out/hardened_transfer.json`), so a fail is a plausible, useful outcome.
(Option A, a new seed of the AMLworld generator, was not chosen.)

## Data: how it is produced

- **Generator:** IBM AMLSim, https://github.com/IBM/AMLSim, master at commit
  `7338a4bcb1af9bcfea2201ad7daccfe2a4d569ca` (2022-02-14). Licence: Apache-2.0 (the repository's `LICENSE` file,
  checked). The generated data is ours; nothing from AMLSim is committed here except the numbers in the result file.
- **MASON 20**, the simulator's one dependency not on Maven Central, is compiled from source: GitHub `eclab/mason`, tag
  `v20` (commit `b799a4a3bf0a9f562a3532edeb2aac1bd106c42e`, Academic Free License 3.0); only `sim.engine` and what it
  needs. The other jars come from Maven Central at the versions in AMLSim's `pom.xml` (json 20180813, webgraph 3.6.1)
  and its README (commons-math3 3.6.1). AMLSim's Java sources are compiled with `javac` (OpenJDK 21).
- **Graph generator:** `scripts/transaction_graph_generator.py` under networkx 1.11 (AMLSim requires 1.x), run with
  `python3 -I` from a separate directory. Python 3.13 removed three things networkx 1.11 imports (`fractions.gcd`,
  `collections.Mapping` and friends, `lib2to3` used only by GML I/O); a wrapper restores them as aliases or stubs
  before import. Toolchain only: no AMLSim logic is changed.
- **Configuration:** AMLSim's top-level `conf.json` with `input.directory` = `paramFiles/10K` (the repository's 10K
  parameter set: 12,043 accounts, 100 laundering alert patterns of fan-in, fan-out and cycle, 3,000 normal models,
  720 daily steps, one bank), `simulation_name` = `holdout` and `random_seed` (below); nothing else changed. This
  equals `paramFiles/10K/conf.json` plus the `normal_models` input key the current generator requires. Chosen for
  size: it fits this container.
- **Seed:** the first eight hex digits of this commit's SHA, read as an integer, modulo 2^31 (the simulator reads a
  32-bit signed integer). It is fixed by this commit, so it cannot be chosen after looking. One run.
- **Data used:** the simulator's `tx_log.csv` (every transfer and cash payment with its `isSAR` label), all rows.

## Mapping to the served features

Each AMLSim row becomes one row of the AMLworld CSV that `research_dump` (the served Rust engine) reads; the served
feature code then computes every feature as it does in production. The mapping is fixed here:

| AMLworld column | from AMLSim |
|---|---|
| timestamp | `base_date` 2017-01-01 + `step` days, 00:00 (rows in file order within a step) |
| from bank, to bank | one constant bank (`AMLSIM`); the 10K configuration has one bank |
| from account, to account | `nameOrig`, `nameDest` |
| amount paid, amount received | `amount` |
| currency paid, received | US Dollar (AMLSim amounts carry no currency) |
| payment format | `TRANSFER` → ACH; `CASH-IN`, `CASH-OUT` → Cash (AMLgentex precedent, `research/amlgentex_transfer.py`) |
| label | `isSAR` |

Features that cannot be built from AMLSim, and how they are handled (no imputation, no refit; the served model sees
what the served code computes):

- `hour`: AMLSim has daily steps and no time of day, so it is always 0.
- `dow`: comes from the synthetic date; carries no generator signal.
- `cur_code`, `cross_cur`: one currency, so constant.
- `cross_bank`, `new_bank_pair`: one bank, so constant.
- `fmt_code`: only ACH and Cash occur, by the mapping above.
- Time-gap and 24-hour window features (`s_gap_out`, `d_gap_in`, `s_mins_since_in`, `s_age_out`, `s_*_24h`,
  `d_*_24h`) and the 24-hour pair window of the served score: computed at day resolution; payments on the same day
  are simultaneous. This is part of the generator gap being measured.

## Frozen before generation

- Code, models, maps, band thresholds and features: this commit. No refit, recalibration or threshold change on AMLSim.
- Score: the served score exactly as `research/evasion_validation.py` builds it (engine `finish_risk`): cascade risk
  (L1 gate, mean of the L2 halves), raised to the 24 h noisy-OR of the same payer-payee pair, then
  max(iso_served, iso_free) from `service/models/hardening.json`. The cascade is the risk before the pair window and
  the maps, with its own thresholds (`service/models/cascade_config.json`).
- Split: chronological. Steps 0 to 575 build engine state only; metrics come from the test window, steps 576 to 719
  (the last 20% of steps). Nothing is fitted, so there is no validation part.
- One run, scored once. Any bug fix after the run is reported with both the original and the corrected result.

## Measures (test window)

- PR-AUC and ROC-AUC of the served score and of the cascade.
- Verify, hold, block and hold-or-block bands at the served thresholds (cascade: its own thresholds): flags, alerts per
  1,000 payments, precision, recall.
- Baseline B1: ranking by amount. (The draft's B0, the context rules alone, is dropped: `graph_points` is not in the
  research dump, and it is not part of the served score.)
- 95% intervals from a whole-day (step) block bootstrap, 500 resamples, `default_rng(41)`.

## Rule (fixed)

- **H1 adds value.** The lower end of the paired day-bootstrap 95% interval of served PR-AUC minus the better of B1's
  PR-AUC and the laundering rate (a random ranking) is above zero.
- **H2 bands mean something.** The served hold-or-block band raises at least one alert on test, and the lower end of
  the day-bootstrap 95% interval of its precision is above the test laundering rate.

Verdict: both pass → **transfers**; exactly one → **partial transfer**; neither → **does not transfer**. The cascade's
numbers, ROC-AUC and the block band are reported but do not change the verdict. A fail does not lead to tuning on this
holdout; any fix is checked on a later, separate holdout.

## Not covered

Real bank data (the shadow pilot, `docs/PILOT.md`), message context (AMLSim has no messages), several banks or
currencies (one of each in this configuration), and an adversary who has studied the model.
