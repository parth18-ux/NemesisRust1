# Shadow-mode pilot protocol

How a bank can measure RuseTect on its own data without letting it decide anything. Every model change in this
repository follows the same rules (see the [model card](MODEL_CARD.md)), so a pilot is the same protocol on new data.

## Principles

- **Shadow only.** RuseTect scores a copy of the payment stream. Its output goes to a log the pilot team reads; it never
  reaches a payment, a customer or a case system.
- **Data stays in the bank.** One binary (`rusetect-server`, or the Redis workers in `deploy/`) runs inside the bank's
  network. Nothing is sent out. Account identifiers can be replaced by salted hashes before the stream reaches it.
- **Fail closed.** The server refuses to start without analyst keys and production secrets
  (`RUSETECT_ANALYST_KEYS`, `RUSETECT_SIGNING_KEY`, `RUSETECT_CONSORTIUM_SECRET`, `RUSETECT_HASH_SALT`).
- **Rules before results.** What counts as success is written down before the first test score is read.

## What the bank provides

| Item | Minimum | Why |
|---|---|---|
| Payment stream | Timestamp, payer and payee (bank, account or hash), amount, currency, payment format or rail | The 31 account-history features need nothing else |
| Labels | Confirmed fraud, mule or SAR outcomes for the same period, with the date each was known | Measures recall and precision; the date keeps the evaluation honest about label delay |
| History | At least 3 months before the pilot window | Account history features need warm-up |
| Alert budget | Reviews per day the team can absorb | Sets the band thresholds, instead of a number chosen here |
| Cost ratio | Rough cost of a missed fraud versus a needless review | Picks the operating point |

## Steps

1. **Replay (weeks 1 to 2).** Stream historical payments through the engine in time order. No labels are read yet.
2. **Freeze the rule.** Write the success rule into the pilot script, for example: block-band precision at least X at the
   agreed alert budget, and no worse than the current system on recall.
3. **Calibrate on the first half.** Fit the isotonic maps and band thresholds on the earlier labelled months only
   (`research/hardened_eval.py` is the template).
4. **Measure on the second half.** Report PR-AUC and band precision and recall with bootstrap intervals, alerts per day,
   and recall by typology if the labels carry one. Include the format-switch and split-payment attacks
   (`research/structuring_defence.py`).
5. **Live shadow (weeks 3 to 8).** Score the live stream in parallel with the current system; compare alerts weekly as
   labels arrive.
6. **Decide.** Adopt, retrain on the bank's data under the same rule, or stop. The decision and its evidence are kept
   with the signed audit log.

## Compare rankings on a historical replay

Run the existing [pilot sender](PILOT_ONBOARDING.md) with `--log replay.jsonl` against the intended scoring configuration.
Send the warm-up and holdout payments in chronological order. Record the build, model version and store mode with the
result so the comparison can be repeated. Use a fresh evaluation store for each build.

The evaluator joins the replay to two exports by `event_id`.

| Export | Columns |
|---|---|
| Incumbent | `event_id,score`. Export the score available at decision time, with higher scores ranked first. Supply a score for every holdout payment. |
| Outcomes | `event_id,case_id,confirmed_at`. Use a shared case ID for payments attached to the same confirmed case. The timestamp is when the payment was confirmed as linked to that case. |

```bash
python3 -I tools/pilot_evaluate.py replay.jsonl incumbent.csv outcomes.csv \
    --start 2026-08-01 --end 2026-09-01 --as-of 2026-10-01T00:00:00Z \
    --reviews-per-day 50 > pilot-comparison.json
```

Freeze the UTC holdout dates, label snapshot and daily budget before reading results. For each full day, both methods
rank the same payments and receive up to the same number of reviews. RuseTect ranks by action severity first, then
descending risk, with missing risk after scored payments in the same action band. Both methods break ties by payment
time and then event ID. The report counts confirmed cases once even when several of their payments are reviewed. Yield
and recall are measured against outcomes confirmed by the snapshot. Unconfirmed payments do not count as hits.

This is a retrospective ranking comparison. It uses the full day's scores to choose that day's reviews. Measure
time to alert, queueing, and threshold behaviour separately in the live shadow run.

## Compare chronological review queues

Use this comparison to estimate how a fixed daily analyst capacity affects review timing. Each method sees a payment
at its timestamp, admits it if its frozen threshold is met and capacity remains, and reviews a group at most once
during the holdout. UTC midnight resets capacity. Equal timestamps are ordered by event ID.

Prepare a payments CSV with `event_id,timestamp,review_group,served,cascade,amount_usd,velocity`. Export the
served and cascade scores available at payment time, use a USD amount known at that time, and define a trailing
velocity feature and review group before looking at outcomes. Use the same event universe for each method. The
review group should come from payment-time data, such as an account identifier available to the analyst.
The separate outcomes CSV has the same format as in the replay comparison above.

Choose each method's threshold using a preceding validation interval and record the interval, feature definitions,
daily capacity, and model revisions before scoring the holdout. Save the exclusive validation end and five thresholds
in the policy JSON. To generate a policy without outcome labels, run:

```bash
python3 tools/pilot_freeze_policy.py validation-payments.csv \
    --start 2026-07-01 --end 2026-08-01 --reviews-per-day 50 --random-seed 41 > policy.json
```

The command ranks each group's peak validation score and chooses the threshold at the planned review volume
(daily capacity times the calendar days in validation, limited to the number of groups). It then replays the
validation queue to report how many reviews the chronological rule actually admits. Ties may put extra groups
at the threshold, and historical payments may cluster, so the daily cap still applies and actual utilization may
fall short of capacity. Freeze the generated file before evaluating the holdout and use the same daily budget.

A hand-authored policy can also use this format. The following values are examples.

```json
{
  "validation_end_utc_exclusive": "2026-08-01",
  "random_seed": 41,
  "thresholds": {"served": 0.5, "cascade": 0.5, "amount_usd": 5000, "velocity": 10, "random": 0.99}
}
```

```bash
python3 tools/pilot_queue.py payments.csv outcomes.csv policy.json \
    --start 2026-08-01 --end 2026-09-01 --as-of 2026-10-01T00:00:00Z \
    --reviews-per-day 50 > chronological-comparison.json
```

The report includes reviews per payment day, confirmed-case recall, and median minutes from a case's first linked
holdout payment to the first reviewed payment linked to that case. Cases count once. The outcome snapshot determines
which links count, and queue selection uses payment-time inputs alone. The seeded baseline scores each event
deterministically and uses the same admission rules. A reported delay covers found cases with a link confirmed by the snapshot.

## What the pilot will not show

- Performance at another bank, or on another rail, without measuring there.
- Behaviour against an adversary who has studied this model. The two attacks tested here are a floor, not a ceiling.
