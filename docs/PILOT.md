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

## What the pilot will not show

- Performance at another bank, or on another rail, without measuring there.
- Behaviour against an adversary who has studied this model. The two attacks tested here are a floor, not a ceiling.
